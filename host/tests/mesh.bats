#!/usr/bin/env bats
# `vercelify mesh`: agent.toml edits, the firewall's mesh rules, and the
# commands against a mock agent. The real mesh is tested by
# host/tests/contract/mesh.sh (network namespaces, root).

setup() {
  T=$(mktemp -d)
  python3 "$BATS_TEST_DIRNAME/mock_agent.py" "$T/port" "$T/requests.log" &
  MOCK_PID=$!
  for _ in $(seq 50); do [ -s "$T/port" ] && break; sleep 0.1; done
  mkdir -p "$T/etc"
  echo test-agent-token >"$T/etc/agent.token"
  printf 'role=main\nversion=test\n' >"$T/etc/install.env"
  export NO_COLOR=1 VERCELIFY_ETC="$T/etc" VERCELIFY_LOG="$T/log" VERCELIFY_TEST=1
  export AGENT_LOCAL_URL="http://127.0.0.1:$(cat "$T/port")"
  CLI="$BATS_TEST_DIRNAME/../vercelify"
}

teardown() { kill "$MOCK_PID" 2>/dev/null; rm -rf "$T"; }

load_libs() {
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/common.sh"
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/mesh.sh"
}

@test "toml_set replaces, adds before tables, leaves comments" {
  load_libs
  f="$T/agent.toml"
  printf '# mesh_coordinator = false\nport = 7420\n\n[backup]\nrepo = "x"\n' >"$f"
  toml_set mesh_coordinator true "$f"
  grep -qx 'mesh_coordinator = true' "$f"
  grep -qx '# mesh_coordinator = false' "$f"
  # Added before the first table, so it stays a top-level key.
  [ "$(grep -n 'mesh_coordinator = true' "$f" | cut -d: -f1)" -lt "$(grep -n '^\[backup\]' "$f" | cut -d: -f1)" ]
  toml_set mesh_coordinator false "$f"
  [ "$(grep -c '^mesh_coordinator' "$f")" -eq 1 ]
  grep -qx 'mesh_coordinator = false' "$f"
  toml_set port 7421 "$f"
  grep -qx 'port = 7421' "$f"
}

@test "toml_list_add adds once, to existing, empty or missing lists" {
  load_libs
  f="$T/agent.toml"
  printf 'listen = ["192.168.1.20", "127.0.0.1"]\n' >"$f"
  toml_list_add listen mesh "$f"
  grep -qx 'listen = \["192.168.1.20", "127.0.0.1", "mesh"\]' "$f"
  toml_list_add listen mesh "$f"
  [ "$(grep -o '"mesh"' "$f" | wc -l)" -eq 1 ]
  printf 'listen = []\n' >"$f"
  toml_list_add listen mesh "$f"
  grep -qx 'listen = \["mesh"\]' "$f"
  printf 'port = 1\n' >"$f"
  toml_list_add listen mesh "$f"
  grep -qx 'listen = \["mesh"\]' "$f"
}

@test "firewall: mesh rules for servers and devices, both families" {
  cat >"$T/acl.json" <<'JSON'
{"interface":"vfy0","version":3,"servers":["10.77.0.1","fd76:6679:6d65::1"],
 "devices":[{"name":"mac","ipv4":"10.77.0.4","ipv6":"fd76:6679:6d65::4","ports":["7420/tcp","5600-5610/udp","junk","22/sctp"]}]}
JSON
  run env MESH_ACL="$T/acl.json" FIREWALL_CONF=/nonexistent bash "$BATS_TEST_DIRNAME/../firewall.sh" rules 4
  [ "$status" -eq 0 ]
  [ "${lines[0]}" = "-s 10.77.0.1 -j RETURN" ]
  [[ "$output" == *"-s 10.77.0.4 -p icmp -j RETURN"* ]]
  [[ "$output" == *"-s 10.77.0.4 -p tcp --dport 7420 -j RETURN"* ]]
  [[ "$output" == *"-s 10.77.0.4 -p udp --dport 5600:5610 -j RETURN"* ]]
  [[ "$output" != *"junk"* && "$output" != *"sctp"* && "$output" != *"fd76"* ]]
  run env MESH_ACL="$T/acl.json" FIREWALL_CONF=/nonexistent bash "$BATS_TEST_DIRNAME/../firewall.sh" rules 6
  [ "${lines[0]}" = "-s fd76:6679:6d65::1 -j RETURN" ]
  [[ "$output" == *"-s fd76:6679:6d65::4 -p ipv6-icmp -j RETURN"* ]]
  [[ "$output" != *"10.77"* ]]
  run env MESH_ACL="$T/acl.json" FIREWALL_CONF=/nonexistent bash "$BATS_TEST_DIRNAME/../firewall.sh" out-rules 4
  [ "$status" -eq 0 ]
  [ -z "$output" ]
  run env MESH_ACL="$T/missing.json" FIREWALL_CONF=/nonexistent bash "$BATS_TEST_DIRNAME/../firewall.sh" mesh
  [ "$status" -ne 0 ]
  [[ "$output" == *"is vercelify-mesh running?"* ]]
}

@test "firewall: containers reach other servers only on the ports the map lists" {
  cat >"$T/acl.json" <<'JSON'
{"interface":"vfy0","version":4,"servers":["10.77.0.1"],"devices":[],
 "container":[{"name":"main","ipv4":"10.77.0.1","ipv6":"fd76:6679:6d65::1","ports":["80/tcp","15432/tcp","bad"]},
              {"name":"pi","ipv4":"10.77.0.5","ipv6":"fd76:6679:6d65::5","ports":["80/tcp"]}]}
JSON
  run env MESH_ACL="$T/acl.json" FIREWALL_CONF=/nonexistent bash "$BATS_TEST_DIRNAME/../firewall.sh" out-rules 4
  [ "$status" -eq 0 ]
  [ "${#lines[@]}" -eq 3 ]
  [ "${lines[0]}" = "-d 10.77.0.1 -p tcp --dport 80 -j ACCEPT" ]
  [ "${lines[1]}" = "-d 10.77.0.1 -p tcp --dport 15432 -j ACCEPT" ]
  [ "${lines[2]}" = "-d 10.77.0.5 -p tcp --dport 80 -j ACCEPT" ]
  run env MESH_ACL="$T/acl.json" FIREWALL_CONF=/nonexistent bash "$BATS_TEST_DIRNAME/../firewall.sh" out-rules 6
  [ "${lines[2]}" = "-d fd76:6679:6d65::5 -p tcp --dport 80 -j ACCEPT" ]
}

@test "mesh invite, nodes and revoke through the agent" {
  run "$CLI" mesh invite
  [ "$status" -eq 0 ]
  [[ "$output" == *"sudo vercelify mesh join vfyj1.server-owner-60"* ]]
  [[ "$output" == *"valid until 2026-10-25T13:00:00Z, once"* ]]
  run "$CLI" mesh invite --device --role viewer --ttl 15
  [[ "$output" == *"vfyj1.device-viewer-15"* ]]
  run "$CLI" mesh invite --role root
  [ "$status" -ne 0 ]
  [[ "$output" == *"unknown role"* ]]
  run "$CLI" mesh invite --ttl soon
  [ "$status" -ne 0 ]
  run "$CLI" mesh nodes
  [ "$status" -eq 0 ]
  [[ "${lines[0]}" == "coordinator  main"*"10.77.0.1"*"192.168.1.20:51820"* ]]
  [[ "${lines[1]}" == "revoked      pi5"*"never"* ]]
  [[ "${lines[2]}" == "server       cabin"*"on the relay (51 KiB)" ]]
  [[ "${lines[0]}" != *"relay"* ]]
  run "$CLI" mesh revoke pi5
  [ "$status" -eq 0 ]
  [[ "$output" == *"pi5 is off the mesh"* ]]
  grep -q '^DELETE /v1/mesh/nodes/pi5 auth=Bearer test-agent-token' "$T/requests.log"
  run "$CLI" mesh revoke ghost
  [ "$status" -ne 0 ]
  [[ "$output" == *"mesh node ghost not found"* ]]
  run "$CLI" mesh join not-a-token
  [ "$status" -ne 0 ]
  [[ "$output" == *"usage: vercelify mesh join vfyj1."* ]]
  run "$CLI" mesh join vfyj1.x --bogus
  [ "$status" -ne 0 ]
  [[ "$output" == *"[--platform]"* ]]
}

@test "a platform main server's invite makes servers its workers" {
  printf 'engine = "platform"\n' >"$T/etc/agent.toml"
  run "$CLI" mesh invite
  [ "$status" -eq 0 ]
  [[ "$output" == *"sudo vercelify mesh join vfyj1.server-owner-60 --platform"* ]]
  run "$CLI" mesh invite --device
  [[ "$output" != *"--platform"* ]]
}

@test "standby: choose a worker, see its copies, turn it off" {
  run "$CLI" standby
  [ "$status" -eq 0 ]
  [[ "$output" == *"no standby"* ]]
  run "$CLI" standby pi5
  [ "$status" -eq 0 ]
  [[ "$output" == *"standby: pi5"* && "$output" == *"first copy is on its way"* ]]
  grep -q '^PUT /v1/platform/settings/standby auth=Bearer test-agent-token' "$T/requests.log"
  run "$CLI" standby
  [[ "$output" == *"last copy 2026-10-25T11:55:00Z (200 KiB)"* ]]
  run "$CLI" standby ghost
  [ "$status" -ne 0 ]
  [[ "$output" == *"server ghost not found"* ]]
  run "$CLI" standby off
  [[ "$output" == *"no standby"* ]]
}

@test "promote and demote refuse what they shouldn't do" {
  # The main server isn't a standby.
  run "$CLI" promote
  [ "$status" -ne 0 ]
  [[ "$output" == *"promote runs on the standby"* ]]
  run "$CLI" promote --now
  [[ "$output" == *"usage: vercelify promote [--force]"* ]]
  # A worker without a copy.
  printf 'role=worker\n' >"$T/etc/install.env"
  run env STANDBY_DIR="$T/standby" "$CLI" promote
  [ "$status" -ne 0 ]
  [[ "$output" == *"no copy of the main server here"* ]]
  # demote only after a standby took over (or --force).
  printf 'role=main\n' >"$T/etc/install.env"
  run env MESH_DIR="$T/mesh" "$CLI" demote
  [ "$status" -ne 0 ]
  [[ "$output" == *"no standby replaced this server"* ]]
}

@test "github-app and webhook (platform engine)" {
  # the engine turned off
  printf 'engine = "none"\n' >"$T/etc/agent.toml"
  run "$CLI" github-app
  [ "$status" -ne 0 ]
  [[ "$output" == *"part of the platform engine"* ]]
  rm "$T/etc/agent.toml"
  run "$CLI" github-app --org acme
  [ "$status" -eq 0 ]
  [[ "$output" == *"https://mesh.example.com/v1/github-app/start?state=s-acme"* ]]
  grep -q '^POST /v1/platform/github-app auth=Bearer test-agent-token' "$T/requests.log"
  run "$CLI" github-app --bogus
  [[ "$output" == *"usage: vercelify github-app"* ]]
  run "$CLI" webhook github
  [ "$status" -eq 0 ]
  [[ "$output" == *"Payload URL: https://mesh.example.com/v1/webhooks/github/git_1"* ]]
  [[ "$output" == *"SSSSSSSSSS"* && "$output" == *"push, pull_request"* ]]
  run "$CLI" webhook nope
  [ "$status" -ne 0 ]
  [[ "$output" == *"git source nope not found"* ]]
}

@test "domain with a tunnel on a coordinator: devices outside the LAN join through mesh.DOMAIN" {
  python3 "$BATS_TEST_DIRNAME/mock_cloudflare.py" "$T/cf-port" "$T/cf.json" &
  CF_PID=$!
  for _ in $(seq 50); do [ -s "$T/cf-port" ] && break; sleep 0.1; done
  mkdir -p "$T/bin" "$T/systemd"
  for b in systemctl cloudflared; do printf '#!/bin/sh\necho "%s $*" >>"%s/calls"\n' "$b" "$T" >"$T/bin/$b"; done
  chmod +x "$T/bin/"*
  printf 'mesh_coordinator = true\nlisten = ["127.0.0.1", "mesh"]\n' >"$T/etc/agent.toml"
  PATH="$T/bin:$PATH" SYSTEMD_DIR="$T/systemd" CF_API_BASE="http://127.0.0.1:$(cat "$T/cf-port")/client/v4" \
    run "$CLI" domain example.com --cloudflare-token test-cf-token
  kill "$CF_PID" 2>/dev/null
  [ "$status" -eq 0 ]
  grep -q '^mesh_public_url = "https://mesh.example.com"' "$T/etc/agent.toml"
  [ "$(jq -r '.configs[] | .ingress[0].hostname' "$T/cf.json")" = mesh.example.com ]
  grep -q 'PUT /v1/platform/settings/domain .*"wildcard":"example.com"' "$T/requests.log"
  grep -q '^domain_mode=cloudflare' "$T/etc/install.env"
  grep -q 'systemctl restart vercelify-agent' "$T/calls"
}

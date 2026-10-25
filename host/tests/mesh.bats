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
  printf 'listen = ["tailscale", "127.0.0.1"]\n' >"$f"
  toml_list_add listen mesh "$f"
  grep -qx 'listen = \["tailscale", "127.0.0.1", "mesh"\]' "$f"
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
  run env MESH_ACL="$T/missing.json" FIREWALL_CONF=/nonexistent bash "$BATS_TEST_DIRNAME/../firewall.sh" mesh
  [ "$status" -ne 0 ]
  [[ "$output" == *"is vercelify-mesh running?"* ]]
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
}

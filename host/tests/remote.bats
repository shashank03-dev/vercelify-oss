#!/usr/bin/env bats
bats_require_minimum_version 1.5.0
# `vercelify remote`: the router's port forwards (stub upnpc/natpmpc) and
# what ends up in agent.toml.

setup() {
  T=$(mktemp -d)
  mkdir -p "$T/bin" "$T/etc" "$T/systemd"
  printf 'listen = ["127.0.0.1", "mesh"]\nmesh_coordinator = true\n' >"$T/etc/agent.toml"
  cat >"$T/bin/ip" <<'STUB'
#!/usr/bin/env bash
echo "1.1.1.1 via 192.168.1.1 dev eth0 src 192.168.1.20 uid 0"
STUB
  cat >"$T/bin/upnpc" <<'STUB'
#!/usr/bin/env bash
echo "upnpc $*" >>"$CALLS"
[[ -n "${NO_UPNP:-}" ]] && exit 1
[[ "$*" == *" -s"* || "$1" == -s ]] && echo "ExternalIPAddress = ${PUBLIC_IP:-203.0.113.7}"
exit 0
STUB
  cat >"$T/bin/natpmpc" <<'STUB'
#!/usr/bin/env bash
echo "natpmpc $*" >>"$CALLS"
echo "Public IP address : ${PUBLIC_IP:-203.0.113.7}"
echo "Mapped public port $2 protocol $4 to local port $3 lifetime 3600"
STUB
  printf '#!/usr/bin/env bash\necho "systemctl $*" >>"$CALLS"\n' >"$T/bin/systemctl"
  chmod +x "$T/bin/"*
  export CALLS="$T/calls" PATH="$T/bin:$PATH" NO_COLOR=1 VERCELIFY_TEST=1
  export VERCELIFY_ETC="$T/etc" VERCELIFY_LOG="$T/log" SYSTEMD_DIR="$T/systemd"
  : >"$CALLS"
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/common.sh"
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/mesh.sh"
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/remote.sh"
  apt_install() { :; }
  ROLE=main
}

teardown() { rm -rf "$T"; }

@test "public addresses only: private, CGNAT and link-local are refused" {
  remote_public_ip 203.0.113.7
  remote_public_ip 8.8.8.8
  for ip in 10.1.2.3 192.168.1.1 172.20.0.1 100.64.0.1 100.127.255.1 169.254.1.1 127.0.0.1 '' bogus; do
    run ! remote_public_ip "$ip"
  done
  remote_public_ip 100.128.0.1
  remote_public_ip 172.32.0.1
}

@test "UPnP forwards both ports to this machine and the agent learns the public address" {
  run remote_on
  [ "$status" -eq 0 ]
  grep -q 'upnpc -e vercelify -a 192.168.1.20 51820 51820 UDP 3600' "$CALLS"
  grep -q 'upnpc -e vercelify -a 192.168.1.20 7443 7443 TCP 3600' "$CALLS"
  grep -qx 'mesh_public_endpoints = \["203.0.113.7:51820"\]' "$T/etc/agent.toml"
  grep -qx 'mesh_public_url = "http://203.0.113.7:7443"' "$T/etc/agent.toml"
  grep -q 'systemctl restart vercelify-agent' "$CALLS"
  grep -q 'OnUnitActiveSec=20min' "$T/systemd/vercelify-remote.timer"
  grep -q '^method=upnp' "$T/etc/remote.env"
  # renewing with nothing changed doesn't restart the agent
  : >"$CALLS"
  run remote_cli refresh
  [ "$status" -eq 0 ]
  run ! grep -q 'restart vercelify-agent' "$CALLS"
  # a new public address is followed
  PUBLIC_IP=198.51.100.4 run remote_cli refresh
  grep -qx 'mesh_public_endpoints = \["198.51.100.4:51820"\]' "$T/etc/agent.toml"
  grep -q 'systemctl restart vercelify-agent' "$CALLS"
}

@test "NAT-PMP when the router has no UPnP; configured ports are used" {
  printf 'mesh_port = 41641\nmesh_control_listen = "0.0.0.0:8443"\n' >>"$T/etc/agent.toml"
  NO_UPNP=1 run remote_apply
  [ "$status" -eq 0 ]
  grep -q 'natpmpc -a 41641 41641 udp 3600' "$CALLS"
  grep -q 'natpmpc -a 8443 8443 tcp 3600' "$CALLS"
  grep -qx 'mesh_public_url = "http://203.0.113.7:8443"' "$T/etc/agent.toml"
  grep -q '^method=natpmp' "$T/etc/remote.env"
}

@test "CGNAT: nothing is kept open and the Cloudflare way is suggested" {
  PUBLIC_IP=100.72.1.9 run remote_apply
  [ "$status" -ne 0 ]
  [[ "$output" == *"CGNAT"*"cloudflare-token"* ]]
  grep -q 'upnpc -d 51820 UDP' "$CALLS"
  run ! grep -q mesh_public "$T/etc/agent.toml"
}

@test "a Cloudflare tunnel stays the control address" {
  printf 'mesh_public_url = "https://mesh.example.com"\n' >>"$T/etc/agent.toml"
  run remote_apply
  [ "$status" -eq 0 ]
  grep -qx 'mesh_public_url = "https://mesh.example.com"' "$T/etc/agent.toml"
  grep -qx 'mesh_public_endpoints = \["203.0.113.7:51820"\]' "$T/etc/agent.toml"
}

@test "off removes the forwards, the timer and only what on wrote" {
  remote_on >/dev/null 2>&1
  printf 'mesh_name = "box"\n' >>"$T/etc/agent.toml"
  : >"$CALLS"
  run remote_off
  [ "$status" -eq 0 ]
  grep -q 'upnpc -d 7443 TCP' "$CALLS"
  grep -q 'systemctl disable --now vercelify-remote.timer' "$CALLS"
  [ ! -e "$T/systemd/vercelify-remote.timer" ] && [ ! -e "$T/etc/remote.env" ]
  run ! grep -q mesh_public "$T/etc/agent.toml"
  grep -q 'mesh_name = "box"' "$T/etc/agent.toml"
}

@test "only on the main server" {
  ROLE=worker
  run remote_on
  [ "$status" -ne 0 ]
  [[ "$output" == *"for the main server"* ]]
}

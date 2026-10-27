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
[[ -n "${NO_NATPMP:-}" ]] && exit 1
echo "Public IP address : ${PUBLIC_IP:-203.0.113.7}"
echo "Mapped public port $2 protocol $4 to local port $3 lifetime 3600"
STUB
  # cloudflared's metrics endpoint, and the public drop.
  cat >"$T/bin/curl" <<'STUB'
#!/usr/bin/env bash
echo "curl $*" >>"$CALLS"
if [[ "$*" == *quicktunnel* ]]; then
  [[ -n "${NO_TUNNEL:-}" ]] && exit 7
  echo "{\"hostname\":\"${TUNNEL_HOST:-calm-river-1.trycloudflare.com}\"}"
fi
exit 0
STUB
  # Restarting the agent signs a new note, as the agent does.
  cat >"$T/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
echo "systemctl $*" >>"$CALLS"
if [[ "$*" == "restart vercelify-agent" && -n "${NOTE:-}" ]]; then
  echo "vfy0123456789abcdef0123456789abcdef01234567 $NOTE" >"$MESH_DIR/beacon"
fi
exit 0
STUB
  chmod +x "$T/bin/"*
  mkdir -p "$T/mesh"
  export CALLS="$T/calls" PATH="$T/bin:$PATH" NO_COLOR=1 VERCELIFY_TEST=1
  export VERCELIFY_ETC="$T/etc" VERCELIFY_LOG="$T/log" SYSTEMD_DIR="$T/systemd"
  export MESH_DIR="$T/mesh" QUICK_TUNNEL_WAIT=2 BEACON_WAIT=1 MESH_BEACON_URL=https://drop.test
  : >"$CALLS"
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/common.sh"
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/mesh.sh"
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/remote.sh"
  apt_install() { :; }
  install_cloudflared() { :; }
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

@test "the ports, under the CLI's set -euo pipefail, with nothing configured" {
  run bash -euo pipefail -c '. "$1/lib/common.sh"; . "$1/lib/remote.sh"; remote_ports' _ "$BATS_TEST_DIRNAME/.."
  [ "$status" -eq 0 ]
  [ "$output" = $'udp 51820\ntcp 7443' ]
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

@test "CGNAT: nothing is kept open on the router; a quick tunnel instead" {
  PUBLIC_IP=100.72.1.9 run remote_apply
  [ "$status" -eq 0 ]
  [[ "$output" == *"CGNAT"* ]]
  grep -q 'upnpc -d 51820 UDP' "$CALLS"
  run ! grep -q mesh_public_endpoints "$T/etc/agent.toml"
  grep -qx 'mesh_public_url = "https://calm-river-1.trycloudflare.com"' "$T/etc/agent.toml"
  grep -q -- '--url http://127.0.0.1:7443' "$T/systemd/vercelify-quicktunnel.service"
  grep -q 'DynamicUser=yes' "$T/systemd/vercelify-quicktunnel.service"
  grep -q '^method=tunnel' "$T/etc/remote.env"
}

@test "no UPnP or NAT-PMP: a quick tunnel, its signed address posted; it follows a new one" {
  export NO_UPNP=1 NO_NATPMP=1 NOTE=vfyb1.first.sig
  run remote_apply
  [ "$status" -eq 0 ]
  [[ "$output" == *"https://calm-river-1.trycloudflare.com"* ]]
  grep -q 'systemctl restart vercelify-quicktunnel' "$CALLS"
  grep -q 'systemctl restart vercelify-agent' "$CALLS"
  grep -q 'curl -fsS -m 15 -o /dev/null -d vfyb1.first.sig https://drop.test/vfy0123456789abcdef0123456789abcdef01234567' "$CALLS"
  # Each renewal posts it again (the drop keeps messages for hours)…
  : >"$CALLS"
  run remote_apply
  run ! grep -q 'restart vercelify-agent' "$CALLS"
  run ! grep -q 'restart vercelify-quicktunnel' "$CALLS"
  # (a unit from an older version, or another port, restarts the tunnel)
  sed -i 's/7443/7999/' "$T/systemd/vercelify-quicktunnel.service"
  : >"$CALLS"
  run remote_apply
  grep -q 'systemctl restart vercelify-quicktunnel' "$CALLS"
  grep -q -- '--url http://127.0.0.1:7443' "$T/systemd/vercelify-quicktunnel.service"
  grep -q 'd vfyb1.first.sig https://drop.test/' "$CALLS"
  # …and a new tunnel address is followed and posted.
  : >"$CALLS"
  TUNNEL_HOST=bold-sea-2.trycloudflare.com NOTE=vfyb1.second.sig run remote_apply
  grep -qx 'mesh_public_url = "https://bold-sea-2.trycloudflare.com"' "$T/etc/agent.toml"
  grep -q 'd vfyb1.second.sig https://drop.test/' "$CALLS"
}

@test "the router opens the ports again: direct, and the tunnel goes" {
  NO_UPNP=1 NO_NATPMP=1 remote_apply >/dev/null 2>&1
  : >"$CALLS"
  run remote_apply
  [ "$status" -eq 0 ]
  grep -q 'systemctl disable --now vercelify-quicktunnel' "$CALLS"
  [ ! -e "$T/systemd/vercelify-quicktunnel.service" ]
  grep -qx 'mesh_public_url = "http://203.0.113.7:7443"' "$T/etc/agent.toml"
  grep -qx 'mesh_public_endpoints = \["203.0.113.7:51820"\]' "$T/etc/agent.toml"
  # and back: the stale forward is dropped for the tunnel
  NO_UPNP=1 NO_NATPMP=1 run remote_apply
  run ! grep -q mesh_public_endpoints "$T/etc/agent.toml"
  grep -qx 'mesh_public_url = "https://calm-river-1.trycloudflare.com"' "$T/etc/agent.toml"
}

@test "the tunnel can't start: says what to do, writes nothing" {
  NO_UPNP=1 NO_NATPMP=1 NO_TUNNEL=1 run remote_apply
  [ "$status" -ne 0 ]
  [[ "$output" == *"quick tunnel didn't start"*"UPnP"* ]]
  run ! grep -q mesh_public "$T/etc/agent.toml"
}

@test "with a Cloudflare domain and a closed router: the domain, no quick tunnel" {
  printf 'mesh_public_url = "https://mesh.example.com"\n' >>"$T/etc/agent.toml"
  NO_UPNP=1 NO_NATPMP=1 run remote_apply
  [ "$status" -eq 0 ]
  run ! grep -q quicktunnel "$CALLS"
  grep -qx 'mesh_public_url = "https://mesh.example.com"' "$T/etc/agent.toml"
  grep -q '^method=cloudflare' "$T/etc/remote.env"
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

@test "off from a tunnel: the tunnel and its address go" {
  NO_UPNP=1 NO_NATPMP=1 remote_on >/dev/null 2>&1
  run remote_off
  [ "$status" -eq 0 ]
  [ ! -e "$T/systemd/vercelify-quicktunnel.service" ]
  run ! grep -q mesh_public_url "$T/etc/agent.toml"
}

@test "only on the main server" {
  ROLE=worker
  run remote_on
  [ "$status" -ne 0 ]
  [[ "$output" == *"for the main server"* ]]
}

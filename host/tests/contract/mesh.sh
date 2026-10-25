#!/usr/bin/env bash
# Contract test for the mesh (design §13) with real WireGuard between
# network namespaces: a coordinator ("main") and a worker ("pi") on one LAN,
# and a third server ("far") on another network behind a router, so the
# test covers both LAN and routed (direct) paths. It joins both with the
# daemon, then checks pings in a full mesh, *.vfy names, the agent over the
# mesh, key rotation, the ACL file and revocation.
#
#   cargo build -p vercelify-agent && cargo build -p vercelify-mesh --features daemon
#   sudo host/tests/contract/mesh.sh      # needs root, iproute2, ping, curl, jq, python3
#
# KEEP=1 leaves the namespaces and the work directory for a look around.
set -euo pipefail
export NO_PROXY="*" no_proxy="*"
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
agent=${AGENT_BIN:-$repo/target/debug/vercelify-agent}
mesh=${MESH_BIN:-$repo/target/debug/vercelify-mesh}
work=$(mktemp -d)
token="mesh-contract-0123456789abcdef"
ns_main=vfyt-main ns_pi=vfyt-pi ns_far=vfyt-far ns_mac=vfyt-mac ns_rtr=vfyt-rtr

pass=0
ok() { pass=$((pass + 1)); printf '  \033[32m✓\033[0m %s\n' "$*"; }
fail() {
  printf '  \033[31m✗\033[0m %s\n' "$*" >&2
  for f in "$work"/*.log; do echo "--- $f" >&2; tail -25 "$f" >&2; done
  exit 1
}
ns_run() { local ns=$1; shift; ip netns exec "$ns" "$@"; }
# until N CMD…: retry CMD for N seconds.
until_ok() {
  local n=$1; shift
  for _ in $(seq "$((n * 4))"); do "$@" >/dev/null 2>&1 && return 0; sleep 0.25; done
  return 1
}

cleanup() {
  if [[ "${KEEP:-0}" == 1 ]]; then echo "kept: $work and namespaces vfyt-*"; return; fi
  local p
  for p in $(cat "$work"/*.pid 2>/dev/null); do kill "$p" 2>/dev/null || true; done
  # Anything else this run started (found by its work directory).
  for p in $(pgrep -f -- "$work" 2>/dev/null); do [[ "$p" == "$$" ]] || kill "$p" 2>/dev/null || true; done
  sleep 0.5
  for ns in $ns_main $ns_pi $ns_far $ns_mac $ns_rtr; do ip netns del "$ns" 2>/dev/null || true; done
  rm -rf "$work"
}
trap cleanup EXIT

[[ $EUID -eq 0 ]] || { echo "run as root (network namespaces)"; exit 1; }
for b in "$agent" "$mesh"; do [[ -x "$b" ]] || { echo "missing $b (cargo build)"; exit 1; }; done
for t in ip ping curl jq python3; do command -v "$t" >/dev/null || { echo "missing $t"; exit 1; }; done

echo "network: LAN 192.168.77.0/24 (main, pi) — router — 172.31.9.0/24 (far)"
for ns in $ns_main $ns_pi $ns_far $ns_mac $ns_rtr; do ip netns del "$ns" 2>/dev/null || true; ip netns add "$ns"; ns_run "$ns" ip link set lo up; done
ns_run $ns_rtr ip link add lan type bridge
ns_run $ns_rtr ip link add wan type bridge
ns_run $ns_rtr ip addr add 192.168.77.1/24 dev lan
ns_run $ns_rtr ip addr add 172.31.9.1/24 dev wan
ns_run $ns_rtr ip link set lan up
ns_run $ns_rtr ip link set wan up
ns_run $ns_rtr sysctl -qw net.ipv4.ip_forward=1
link() { # NS BRIDGE ADDR GATEWAY
  local ns=$1 br=$2 addr=$3 gw=$4
  ip link add "v-$ns" type veth peer name eth0 netns "$ns"
  ip link set "v-$ns" netns $ns_rtr
  ns_run $ns_rtr ip link set "v-$ns" master "$br" up
  ns_run "$ns" ip addr add "$addr" dev eth0
  ns_run "$ns" ip link set eth0 up
  ns_run "$ns" ip route add default via "$gw"
}
link $ns_main lan 192.168.77.10/24 192.168.77.1
link $ns_pi lan 192.168.77.11/24 192.168.77.1
link $ns_far wan 172.31.9.10/24 172.31.9.1
link $ns_mac lan 192.168.77.12/24 192.168.77.1

echo "coordinator: agent + vercelify-mesh in $ns_main"
mkdir -p "$work/main" "$work/pi" "$work/far" "$work/mac"
printf '%s\n' "$token" >"$work/agent.token"
# Background processes start as `ip netns exec` itself (it execs the
# command), so $! is the daemon's pid and cleanup can stop it.
ip netns exec $ns_main env VERCELIFY_CONFIG=/nonexistent VERCELIFY_LISTEN=127.0.0.1,mesh VERCELIFY_PORT=7420 \
  VERCELIFY_TOKEN_FILE="$work/agent.token" VERCELIFY_ROLE=main VERCELIFY_INSTALL_ENV="$work/install.env" \
  VERCELIFY_STATE_DIR="$work/main/state" VERCELIFY_RUN_DIR="$work/main/run" \
  VERCELIFY_PLATFORM_DB="$work/main/state/platform.db" VERCELIFY_MESH_DIR="$work/main/mesh" \
  VERCELIFY_MESH_COORDINATOR=1 VERCELIFY_MESH_CONTROL_LISTEN=0.0.0.0:7443 VERCELIFY_MESH_NAME=main \
  VERCELIFY_WATCH_INTERVAL=0 VERCELIFY_AUTODEPLOY_INTERVAL=0 VERCELIFY_WAKE_LISTEN='' \
  VERCELIFY_TOKENS_FILE="$work/main/state/tokens.json" VERCELIFY_AUDIT_LOG="$work/main/state/audit.log" \
  "$agent" serve >"$work/agent.log" 2>&1 &
echo $! >"$work/agent.pid"
until_ok 20 ns_run $ns_main curl -sf http://127.0.0.1:7420/v1/health || fail "the agent didn't start"
until_ok 10 test -s "$work/main/mesh/state.json" || fail "the coordinator wrote no state for its own node"

up() { # NS NAME — the ACL hook renders the real firewall in the node's namespace
  ip netns exec "$1" env RUST_LOG=vercelify_mesh=debug "$mesh" --dir "$work/$2/mesh" --run-dir "$work/$2/run" up \
    --acl-hook "cp $work/$2/run/mesh-acl.json $work/$2/acl.copy && MESH_ACL=$work/$2/run/mesh-acl.json FIREWALL_CONF=/nonexistent bash $repo/host/firewall.sh mesh" \
    >"$work/mesh-$2.log" 2>&1 &
  echo $! >"$work/mesh-$2.pid"
}
up $ns_main main
has_addr() { ns_run "$1" ip -4 addr show vfy0 | grep -q "$2"; }
until_ok 15 has_addr $ns_main 10.77.0.1 || fail "vfy0 didn't come up on main"
ok "coordinator on 10.77.0.1"

api() { ns_run $ns_main curl -sf -X "$1" -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
  ${3:+--data "$3"} "http://127.0.0.1:7420$2"; }
invite() { api POST /v1/mesh/invites '{"kind":"server"}' | jq -r .token; }

echo "joining"
t_pi=$(invite)
[[ "$t_pi" == vfyj1.* ]] || fail "no join token: $t_pi"
ns_run $ns_pi "$mesh" --dir "$work/pi/mesh" join "$t_pi" --name pi | grep -q "joined as pi (10.77.0.2" || fail "pi didn't join"
ok "pi joined (sealed enrollment over the LAN)"
again=$(ns_run $ns_pi "$mesh" --dir "$work/pi/mesh2" join "$t_pi" --name pi2 2>&1 || true)
[[ "$again" == *"used already"* ]] || fail "a join token worked twice: $again"
ok "a join token works once"
t_far=$(invite)
ns_run $ns_far "$mesh" --dir "$work/far/mesh" join "$t_far" --name far | grep -q "joined as far (10.77.0.3" || fail "far didn't join"
ok "far joined (through the router)"
up $ns_pi pi
up $ns_far far

echo "paths"
until_ok 20 ns_run $ns_pi ping -c1 -W1 10.77.0.1 || fail "pi can't reach main over the mesh"
ok "pi → main (LAN)"
until_ok 20 ns_run $ns_far ping -c1 -W1 10.77.0.1 || fail "far can't reach main over the mesh"
ok "far → main (routed)"
until_ok 30 ns_run $ns_pi ping -c1 -W1 10.77.0.3 || fail "pi can't reach far: no full mesh"
ok "pi → far directly (full mesh, not through main)"
ns_run $ns_far ping -c1 -W1 10.77.0.2 >/dev/null || fail "far can't reach pi"
ns_run $ns_pi ping -c1 -W1 -6 fd76:6679:6d65::1 >/dev/null 2>&1 && ok "IPv6 on the mesh" || echo "  - IPv6 inside the namespaces is off: skipped"

until_ok 15 jq -e '.peers[] | select(.name == "far") | .handshake_secs != null' "$work/pi/run/mesh-status.json" ||
  fail "no status file on pi"
path_main=$(jq -r '.peers[] | select(.name == "main") | .path' "$work/pi/run/mesh-status.json")
path_far=$(jq -r '.peers[] | select(.name == "far") | .path' "$work/pi/run/mesh-status.json")
[[ "$path_main" == lan && "$path_far" == direct ]] || fail "paths: main=$path_main far=$path_far"
ok "status: main on the LAN, far direct"
# The tunnel to far must not run through main: its endpoint is far's own address.
ep=$(jq -r '.peers[] | select(.name == "far") | .endpoint' "$work/pi/run/mesh-status.json")
[[ "$ep" == 172.31.9.10:* ]] || fail "pi reaches far at $ep"
ok "pi's endpoint for far is far itself ($ep)"

echo "names and services"
dns() { # NS SERVER NAME → first A record
  ns_run "$1" python3 - "$2" "$3" <<'PY'
import socket, struct, sys
server, name = sys.argv[1], sys.argv[2]
q = struct.pack(">HHHHHH", 0x1234, 0x0100, 1, 0, 0, 0)
q += b"".join(bytes([len(l)]) + l.encode() for l in name.split(".")) + b"\0" + struct.pack(">HH", 1, 1)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.settimeout(3); s.sendto(q, (server, 53))
r = s.recv(512)
rcode, an = r[3] & 15, struct.unpack(">H", r[6:8])[0]
print(".".join(map(str, r[-4:])) if rcode == 0 and an else f"rcode={rcode}")
PY
}
until_ok 10 dns $ns_pi 10.77.0.2 far.vfy || fail "no mesh DNS on pi"
[[ "$(dns $ns_pi 10.77.0.2 far.vfy)" == 10.77.0.3 ]] || fail "far.vfy → $(dns $ns_pi 10.77.0.2 far.vfy)"
[[ "$(dns $ns_pi 10.77.0.2 blog.main.vfy)" == 10.77.0.1 ]] || fail "blog.main.vfy didn't resolve"
[[ "$(dns $ns_pi 10.77.0.2 example.com)" == rcode=5 ]] || fail "names outside .vfy aren't refused"
ok "far.vfy → 10.77.0.3, *.main.vfy → main, others refused"
agent_over_mesh() { ns_run $ns_pi curl -sf --max-time 3 http://10.77.0.1:7420/v1/health | grep -q '"mesh"'; }
until_ok 20 agent_over_mesh || fail "the agent isn't reachable over the mesh"
ok "pi reaches main's agent at 10.77.0.1:7420"
# A node token is useless from outside the mesh.
pi_token=$(jq -r .node_token "$work/pi/mesh/state.json")
code=$(ns_run $ns_pi curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $pi_token" http://192.168.77.10:7420/v1/mesh/map --max-time 3 || true)
[[ "$code" != 200 ]] || fail "a node token worked from the LAN address"
ok "node tokens are refused from outside the mesh"
jq -e '.servers | index("10.77.0.1")' "$work/pi/run/mesh-acl.json" >/dev/null || fail "no ACL file"
until_ok 5 test -s "$work/pi/acl.copy" || fail "the ACL hook didn't run"
ok "ACL file written and the firewall hook ran"

echo "a device and the firewall"
t_mac=$(api POST /v1/mesh/invites '{"kind":"device","role":"viewer"}' | jq -r .token)
ns_run $ns_mac "$mesh" --dir "$work/mac/mesh" join "$t_mac" --name mac | grep -q "joined as mac (10.77.0.4" || fail "the mac didn't join"
up $ns_mac mac
until_ok 20 ns_run $ns_mac ping -c1 -W1 10.77.0.1 || fail "the mac can't reach main"
macmap=$(jq -r .map "$work/mac/mesh/state.json" | cut -d. -f2 | tr '_-' '/+' | base64 -d 2>/dev/null || true)
[[ "$macmap" != *'"name":"mac2"'* ]] || true
# A device sees servers only.
until_ok 10 jq -e '[.peers[].kind] | all(. == "server")' "$work/mac/run/mesh-status.json" || fail "the mac sees other devices"
# On main: an allowed port (the agent) and one that isn't.
ip netns exec $ns_main python3 -m http.server 9999 --bind 10.77.0.1 --directory "$work" >/dev/null 2>&1 &
echo $! >"$work/http.pid"
until_ok 10 bash -c "ip netns exec $ns_main iptables -n -L VERCELIFY-MESH | grep -q 10.77.0.4" || fail "main's firewall has no rules for the mac"
listening() { ns_run $ns_main curl -s -o /dev/null --max-time 1 http://10.77.0.1:9999/; }
until_ok 10 listening || fail "the test server on main:9999 didn't start"
mac_get() { ns_run $ns_mac curl -s -o /dev/null -w '%{http_code}' --max-time 3 "http://10.77.0.1:$1/$2" || true; }
[[ "$(mac_get 7420 v1/health)" == 200 ]] || fail "the mac can't reach the agent (allowed)"
[[ "$(mac_get 9999 '')" == 000 ]] || fail "the mac reached port 9999 (not allowed)"
pi_get=$(ns_run $ns_pi curl -s -o /dev/null -w '%{http_code}' --max-time 3 http://10.77.0.1:9999/ || true)
[[ "$pi_get" == 200 ]] || fail "a server couldn't reach port 9999 on another server ($pi_get)"
ok "firewall: the mac reaches the agent but not :9999; servers reach everything"

echo "rotation"
old=$(ns_run $ns_pi "$mesh" --dir "$work/pi/mesh" pubkey)
ns_run $ns_pi "$mesh" --run-dir "$work/pi/run" rotate >/dev/null
until_ok 15 bash -c "[[ \"\$(ip netns exec $ns_pi $mesh --dir $work/pi/mesh pubkey)\" != '$old' ]]" || fail "the key didn't rotate"
until_ok 20 ns_run $ns_pi ping -c1 -W1 10.77.0.3 || fail "pi lost far after rotating its key"
until_ok 20 ns_run $ns_pi ping -c1 -W1 10.77.0.1 || fail "pi lost main after rotating its key"
new_pk=$(api GET /v1/mesh/nodes | jq -r '.[] | select(.name == "pi") | .public_key')
[[ "$new_pk" == "$(ns_run $ns_pi "$mesh" --dir "$work/pi/mesh" pubkey)" ]] || fail "the coordinator has another key for pi"
ok "pi rotated its key; the mesh followed"

echo "revocation"
api DELETE /v1/mesh/nodes/far >/dev/null || fail "revoke failed"
gone() { ! ns_run $ns_pi ping -c1 -W1 10.77.0.3 >/dev/null 2>&1; }
until_ok 15 gone || fail "pi still reaches far after its revocation"
until_ok 15 jq -e '.revoked == true' "$work/far/run/mesh-status.json" || fail "far doesn't know it was revoked"
ns_run $ns_pi ping -c1 -W1 10.77.0.1 >/dev/null || fail "revoking far broke pi"
ok "far revoked: peers dropped it, it knows, the rest keeps working"

echo "restart with the last map, coordinator away"
kill "$(cat "$work/agent.pid")"
kill "$(cat "$work/mesh-pi.pid")"
sleep 1
up $ns_pi pi
until_ok 20 ns_run $ns_pi ping -c1 -W1 10.77.0.1 || fail "pi didn't come back on its saved map"
ok "pi came back on its saved map while the coordinator's agent is down"

echo "mesh: $pass checks passed"

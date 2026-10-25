#!/usr/bin/env bash
# Contract test for NAT traversal on the mesh (design §13.6–13.7) with real
# WireGuard and real Linux NAT between network namespaces. An "internet"
# (203.0.113.0/24) holds the coordinator ("main") and three home routers:
#
#   a  192.168.1.10 ── rtr1 (NAT, keeps ports)  ┐
#   b  192.168.2.10 ── rtr2 (NAT, keeps ports)  ├── internet ── main 203.0.113.10
#   c  192.168.3.10 ── rtr3 (symmetric NAT)     ┘
#
# Both kinds of NAT let in only replies to what went out. Checks: every
# node joins over the internet; a and b punch a direct path through both
# NATs (disco, call_me through the relay, peer-assisted STUN); c, behind a
# symmetric NAT, reaches a through the relay; main sees who is on the relay.
#
#   cargo build -p vercelify-agent && cargo build -p vercelify-mesh --features daemon
#   sudo host/tests/contract/mesh-nat.sh   # needs root, iproute2, iptables, ping, curl, jq
#
# KEEP=1 leaves the namespaces and the work directory for a look around.
set -euo pipefail
export NO_PROXY="*" no_proxy="*"
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
agent=${AGENT_BIN:-$repo/target/debug/vercelify-agent}
mesh=${MESH_BIN:-$repo/target/debug/vercelify-mesh}
work=$(mktemp -d)
token="mesh-nat-contract-0123456789abcdef"
all="vfyn-inet vfyn-main vfyn-rtr1 vfyn-rtr2 vfyn-rtr3 vfyn-a vfyn-b vfyn-c"

pass=0
ok() { pass=$((pass + 1)); printf '  \033[32m✓\033[0m %s\n' "$*"; }
fail() {
  printf '  \033[31m✗\033[0m %s\n' "$*" >&2
  for f in "$work"/*.log; do echo "--- $f" >&2; tail -25 "$f" >&2; done
  for n in a b c main; do [[ -f "$work/$n/run/mesh-status.json" ]] && { echo "--- $n status" >&2; cat "$work/$n/run/mesh-status.json" >&2; }; done
  exit 1
}
ns_run() { local ns=$1; shift; ip netns exec "$ns" "$@"; }
until_ok() {
  local n=$1; shift
  for _ in $(seq "$((n * 4))"); do "$@" >/dev/null 2>&1 && return 0; sleep 0.25; done
  return 1
}

cleanup() {
  if [[ "${KEEP:-0}" == 1 ]]; then echo "kept: $work and namespaces vfyn-*"; return; fi
  local p
  for p in $(cat "$work"/*.pid 2>/dev/null); do kill "$p" 2>/dev/null || true; done
  for p in $(pgrep -f -- "$work" 2>/dev/null); do [[ "$p" == "$$" ]] || kill "$p" 2>/dev/null || true; done
  sleep 0.5
  for ns in $all; do ip netns del "$ns" 2>/dev/null || true; done
  rm -rf "$work"
}
trap cleanup EXIT

[[ $EUID -eq 0 ]] || { echo "run as root (network namespaces)"; exit 1; }
for b in "$agent" "$mesh"; do [[ -x "$b" ]] || { echo "missing $b (cargo build)"; exit 1; }; done
for t in ip iptables ping curl jq; do command -v "$t" >/dev/null || { echo "missing $t"; exit 1; }; done

echo "network: three home routers and a coordinator on 203.0.113.0/24"
for ns in $all; do ip netns del "$ns" 2>/dev/null || true; ip netns add "$ns"; ns_run "$ns" ip link set lo up; done
ns_run vfyn-inet ip link add wan type bridge
ns_run vfyn-inet ip link set wan up
wan() { # NS ADDR — a leg on the internet
  ip link add "w-$1" type veth peer name wan0 netns "$1"
  ip link set "w-$1" netns vfyn-inet
  ns_run vfyn-inet ip link set "w-$1" master wan up
  ns_run "$1" ip addr add "$2/24" dev wan0
  ns_run "$1" ip link set wan0 up
}
wan vfyn-main 203.0.113.10
home() { # ROUTER WAN-ADDR HOST NET MODE
  local rtr=$1 waddr=$2 host=$3 net=$4 mode=$5
  wan "$rtr" "$waddr"
  ip link add "l-$host" type veth peer name eth0 netns "$host"
  ip link set "l-$host" netns "$rtr"
  ns_run "$rtr" ip addr add "$net.1/24" dev "l-$host"
  ns_run "$rtr" ip link set "l-$host" up
  ns_run "$rtr" sysctl -qw net.ipv4.ip_forward=1
  ns_run "$host" ip addr add "$net.10/24" dev eth0
  ns_run "$host" ip link set eth0 up
  ns_run "$host" ip route add default via "$net.1"
  # Only replies come in; the lan goes out masqueraded. Like any home
  # router, it drops what comes unasked to itself (which also keeps such
  # packets out of its NAT table, where they would take the ports the lan
  # needs).
  ns_run "$rtr" iptables -A INPUT -i wan0 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  ns_run "$rtr" iptables -A INPUT -i wan0 -j DROP
  ns_run "$rtr" iptables -P FORWARD DROP
  ns_run "$rtr" iptables -A FORWARD -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  ns_run "$rtr" iptables -A FORWARD -i "l-$host" -o wan0 -j ACCEPT
  if [[ "$mode" == symmetric ]]; then
    # A new public port for every destination.
    ns_run "$rtr" iptables -t nat -A POSTROUTING -o wan0 -j MASQUERADE --random-fully
  else
    ns_run "$rtr" iptables -t nat -A POSTROUTING -o wan0 -j MASQUERADE
  fi
}
home vfyn-rtr1 203.0.113.21 vfyn-a 192.168.1 cone
home vfyn-rtr2 203.0.113.22 vfyn-b 192.168.2 cone
home vfyn-rtr3 203.0.113.23 vfyn-c 192.168.3 symmetric
# The NATs are real: a host behind one can't be reached from outside.
if ns_run vfyn-main ping -c1 -W1 192.168.1.10 >/dev/null 2>&1; then fail "the lan behind rtr1 is reachable"; fi

echo "coordinator in vfyn-main"
mkdir -p "$work/main" "$work/a" "$work/b" "$work/c"
printf '%s\n' "$token" >"$work/agent.token"
ip netns exec vfyn-main env VERCELIFY_CONFIG=/nonexistent VERCELIFY_LISTEN=127.0.0.1,mesh VERCELIFY_PORT=7420 \
  VERCELIFY_TOKEN_FILE="$work/agent.token" VERCELIFY_ROLE=main VERCELIFY_INSTALL_ENV="$work/install.env" \
  VERCELIFY_STATE_DIR="$work/main/state" VERCELIFY_RUN_DIR="$work/main/run" \
  VERCELIFY_PLATFORM_DB="$work/main/state/platform.db" VERCELIFY_MESH_DIR="$work/main/mesh" \
  VERCELIFY_MESH_COORDINATOR=1 VERCELIFY_MESH_CONTROL_LISTEN=0.0.0.0:7443 VERCELIFY_MESH_NAME=main \
  VERCELIFY_WATCH_INTERVAL=0 VERCELIFY_AUTODEPLOY_INTERVAL=0 VERCELIFY_WAKE_LISTEN='' \
  VERCELIFY_TOKENS_FILE="$work/main/state/tokens.json" VERCELIFY_AUDIT_LOG="$work/main/state/audit.log" \
  "$agent" serve >"$work/agent.log" 2>&1 &
echo $! >"$work/agent.pid"
until_ok 20 ns_run vfyn-main curl -sf http://127.0.0.1:7420/v1/health || fail "the agent didn't start"
until_ok 10 test -s "$work/main/mesh/state.json" || fail "the coordinator wrote no state"

up() { # NS NAME — no public STUN servers here: peers tell each other
  ip netns exec "$1" env RUST_LOG=vercelify_mesh=debug VERCELIFY_MESH_STUN= "$mesh" \
    --dir "$work/$2/mesh" --run-dir "$work/$2/run" up >"$work/mesh-$2.log" 2>&1 &
  echo $! >"$work/mesh-$2.pid"
}
up vfyn-main main
until_ok 15 bash -c "ip netns exec vfyn-main ip -4 addr show vfy0 | grep -q 10.77.0.1" || fail "vfy0 didn't come up on main"
ok "coordinator on 10.77.0.1 (203.0.113.10)"

api() { ns_run vfyn-main curl -sf -X "$1" -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
  ${3:+--data "$3"} "http://127.0.0.1:7420$2"; }

echo "joining from behind the NATs"
for n in a b c; do
  t=$(api POST /v1/mesh/invites '{"kind":"server"}' | jq -r .token)
  ns_run "vfyn-$n" "$mesh" --dir "$work/$n/mesh" join "$t" --name "$n" | grep -q "joined as $n" || fail "$n didn't join"
  up "vfyn-$n" "$n"
done
ok "a, b and c joined over the internet (sealed enrollment through NAT)"

peer() { # NODE PEER FIELD — from NODE's status file
  jq -r --arg n "$2" ".peers[] | select(.name == \$n) | .$3" "$work/$1/run/mesh-status.json" 2>/dev/null
}
path_is() { [[ "$(peer "$1" "$2" path)" == "$3" ]]; }

echo "paths"
for n in a b c; do
  until_ok 30 ns_run "vfyn-$n" ping -c1 -W1 10.77.0.1 || fail "$n can't reach main"
done
ok "everyone reaches main (public) directly"
until_ok 30 path_is c main direct || fail "c → main isn't direct: $(peer c main path)"
ok "c → main direct through its symmetric NAT (main answers where c's packets come from)"

until_ok 60 ns_run vfyn-a ping -c1 -W1 10.77.0.3 || fail "a can't reach b"
until_ok 60 path_is a b direct || fail "a → b isn't direct: $(peer a b path)"
ep=$(peer a b endpoint)
[[ "$ep" == 203.0.113.22:* ]] || fail "a reaches b at $ep, not through b's router"
until_ok 30 path_is b a direct || fail "b → a isn't direct: $(peer b a path)"
ok "a ↔ b direct through both NATs (hole punched to $ep, rtt $(peer a b rtt_ms) ms)"

until_ok 60 ns_run vfyn-a ping -c1 -W1 10.77.0.4 || fail "a can't reach c"
ns_run vfyn-c ping -c1 -W1 10.77.0.2 >/dev/null || fail "c can't reach a"
until_ok 30 path_is a c relay || fail "a → c isn't on the relay: $(peer a c path)"
ok "a ↔ c through the relay (c's NAT is symmetric)"

relayed() { api GET /v1/mesh/nodes | jq -e '.[] | select(.name == "c") | .relay_connected and .relayed_bytes > 0'; }
until_ok 10 relayed || fail "main doesn't show c on the relay"
ok "main shows c on the relay, with the bytes it relayed"

echo "revocation on the relay"
api DELETE /v1/mesh/nodes/c >/dev/null || fail "revoke failed"
off_relay() { api GET /v1/mesh/nodes | jq -e '.[] | select(.name == "c") | .relay_connected == false'; }
until_ok 15 off_relay || fail "c is still on the relay after its revocation"
gone() { ! ns_run vfyn-a ping -c1 -W1 10.77.0.4 >/dev/null 2>&1; }
until_ok 15 gone || fail "a still reaches c"
ns_run vfyn-a ping -c1 -W1 10.77.0.3 >/dev/null || fail "revoking c broke a ↔ b"
ok "c revoked: the relay dropped it and refuses its key; a ↔ b unaffected"

echo "mesh-nat: $pass checks passed"

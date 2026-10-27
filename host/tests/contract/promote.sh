#!/usr/bin/env bash
# Contract test for the warm standby (design §8.4): the main server and a
# worker ("pi") with Docker each, plus a third mesh node ("far"). The worker
# becomes the standby and gets sealed copies; the main server dies; `promote`
# on the standby takes over: the coordinator's address and keys at a new map
# epoch (far follows it without doing anything), the database with every app,
# the apps that ran on the standby keep serving, the ones that ran on the
# old main server come back on the new one. When the old main server
# returns, it sees it was replaced and steps down.
#
#   cargo build -p vercelify-agent && cargo build -p vercelify-mesh --features daemon
#   sudo host/tests/contract/promote.sh    # root, Docker, iproute2, nsenter, curl, jq
#
# KEEP=1 leaves everything up for a look around.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
TEST_NAME=promote
TEST_APPS="web edge"
# shellcheck source-path=SCRIPTDIR source=lib-twoservers.sh
. "$here/lib-twoservers.sh"
ns_far=vfyn-far
fwd() { iptables -w "$1" FORWARD -i vfyn0 -o vfyn1 -j ACCEPT && iptables -w "$1" FORWARD -i vfyn1 -o vfyn0 -j ACCEPT; }
teardown() {
  fwd -D 2>/dev/null || true
  ip link del vfyn1 2>/dev/null || true
  ip netns del $ns_far 2>/dev/null || true
  cleanup
}
trap teardown EXIT

echo "a third node on the mesh"
ip netns del $ns_far 2>/dev/null || true
ip netns add $ns_far
ip netns exec $ns_far ip link set lo up
ip link add vfyn1 type veth peer name eth0 netns $ns_far
ip addr add 192.168.89.1/24 dev vfyn1
ip link set vfyn1 up
ip netns exec $ns_far ip addr add 192.168.89.2/24 dev eth0
ip netns exec $ns_far ip link set eth0 up
ip netns exec $ns_far ip route add default via 192.168.89.1
sysctl -qw net.ipv4.ip_forward=1
# Docker's FORWARD policy is DROP: let the two test networks talk.
fwd -I
in_w ip route add 192.168.89.0/24 via 192.168.88.1 2>/dev/null || true
mkdir -p "$work/far"
join=$(api POST /v1/mesh/invites '{"kind":"server"}' | jq -r .token)
ip netns exec $ns_far "$mesh" --dir "$work/far/mesh" join "$join" --name far --port 51832 | grep -q "joined as far" ||
  fail "far didn't join"
ip netns exec $ns_far "$mesh" --dir "$work/far/mesh" --run-dir "$work/far/run" up >"$work/mesh-far.log" 2>&1 &
echo $! >"$work/mesh-far.pid"
until_ok 30 ip netns exec $ns_far ping -c1 -W1 10.77.0.1 || fail "far can't reach main"
until_ok 30 ip netns exec $ns_far ping -c1 -W1 10.77.0.2 || fail "far can't reach pi"
ok "far is on the mesh"

echo "two apps: one on each server"
# The new main server has no internet: the old one's image is there already
# (as it would be after a pull while it was a worker).
docker save "$APP_IMAGE" | wdocker load -q >/dev/null
deploy() { # NAME NODE_ID HOST
  api POST /v1/platform/apps "$(jq -nc --arg a "$1" --arg i "$APP_IMAGE" --arg n "$2" \
    '{name: $a, kind: "image", source: {image: $i}, node_id: $n, runtime: {port: 80, health_timeout_secs: 90, drain_secs: 5}}')" >/dev/null ||
    fail "creating $1"
  api POST "/v1/platform/apps/$1/domains" "$(jq -nc --arg h "$3" '{host: $h, https: false}')" >/dev/null
  local dep end=$((SECONDS + 180)) s
  dep=$(api POST "/v1/platform/apps/$1/deploy" '{}' | jq -r .id)
  while ((SECONDS < end)); do
    s=$(api GET "/v1/platform/deployments/$dep" | jq -r .status)
    [[ "$s" == healthy ]] && return 0
    [[ "$s" == failed ]] && fail "$1: $(api GET "/v1/platform/deployments/$dep" | jq -r .error)"
    sleep 1
  done
  fail "$1 didn't deploy"
}
main_id=$(api GET /v1/platform/nodes | jq -r '.[] | select(.role == "main") | .id')
deploy web "$main_id" web.test
deploy edge "$wid" edge.test
# Not grep -q: it quits at the match, curl dies of SIGPIPE mid-page and pipefail fails the check.
answers() { curl -sf -H "Host: $2" "http://$1/" | grep -i caddy >/dev/null; }
until_ok 30 answers "127.0.0.1:$PORT_HTTP" web.test || fail "web doesn't answer"
until_ok 30 answers "127.0.0.1:$PORT_HTTP" edge.test || fail "edge doesn't answer"
ok "web runs on the main server, edge on pi"

echo "the standby"
api PUT /v1/platform/settings/standby '{"node": "pi"}' | jq -e '.node == "pi"' >/dev/null || fail "choosing the standby"
copied() { api GET /v1/platform/settings/standby | jq -e '.last_sent_at != null and .bytes > 0'; }
until_ok 60 copied || fail "no copy reached the standby: $(api GET /v1/platform/settings/standby)"
[[ -s "$work/w/state/standby/snapshot.sealed" ]] || fail "the standby keeps no copy"
jq -e '.from != null and .epoch == 1' "$work/w/state/standby/snapshot.json" >/dev/null || fail "snapshot meta"
! grep -q "master" "$work/w/state/standby/snapshot.sealed" || fail "the copy isn't sealed"
ok "pi holds a sealed copy of the main server"
res=$(api_raw PUT /v1/platform/settings/standby "$(jq -nc --arg n "$main_id" '{node: $n}')")
[[ $(jq -r .error <<<"$res") == *"the standby is a worker"* ]] || fail "the main server became its own standby: $res"
ok "the main server can't be its own standby"
out=$(in_w env "${w_env[@]}" "$agent" promote 2>&1 || true)
[[ "$out" == *"still answers"* ]] || fail "promote went ahead with the main server up: $out"
ok "promote refuses while the main server answers"

echo "the main server dies"
for p in agent-main mesh-main agent-worker mesh-worker; do kill "$(cat "$work/$p.pid")"; done
sleep 2
for _ in $(seq 20); do ip link show vfy0 >/dev/null 2>&1 || break; sleep 0.5; done
ip link del vfy0 2>/dev/null || true
ok "main server gone (agent, mesh)"

echo "promote"
out=$(in_w env "${w_env[@]}" "$agent" promote 2>&1) || fail "promote failed: $out"
[[ "$out" == *"is now the main server"* && "$out" == *"epoch 2"* ]] || fail "promote said: $out"
ok "promote: $out"
promoted_env=("${w_env[@]}" VERCELIFY_ROLE=main VERCELIFY_MESH_COORDINATOR=1 "VERCELIFY_LISTEN=127.0.0.1,mesh")
nsenter --net="/var/run/netns/$ns" env "${promoted_env[@]}" "$agent" serve >"$work/agent-promoted.log" 2>&1 &
echo $! >"$work/agent-promoted.pid"
until_ok 30 in_w curl -sf "http://127.0.0.1:$PORT_AGENT/v1/health" || fail "the promoted agent didn't start"
until_ok 15 test -s "$work/w/mesh/state.json" || fail "no coordinator state on the new main server"
nsenter --net="/var/run/netns/$ns" "$mesh" --dir "$work/w/mesh" --run-dir "$work/w/run" up >"$work/mesh-promoted.log" 2>&1 &
echo $! >"$work/mesh-promoted.pid"
until_ok 30 sh -c "nsenter --net=/var/run/netns/$ns ip -4 addr show vfy0 | grep -q 10.77.0.1" || fail "the new main server isn't 10.77.0.1"
ok "pi is the coordinator now, at 10.77.0.1"
papi() { in_w curl -sf -X "$1" -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
  ${3:+--data "$3"} "http://127.0.0.1:$PORT_AGENT$2"; }
[[ $(papi GET /v1/platform/apps | jq -r '[.[].name] | sort | join(",")') == "edge,web" ]] || fail "apps: $(papi GET /v1/platform/apps)"
ok "every app is in the new main server's database"

echo "the mesh follows"
far_sees() {
  jq -e '.peers[] | select(.addresses[0] == "10.77.0.1" and .name == "pi" and .handshake_secs != null and .handshake_secs < 60)' \
    "$work/far/run/mesh-status.json"
}
until_ok 90 far_sees || fail "far doesn't follow the new coordinator: $(jq -c '[.peers[] | {name, addresses, endpoint, handshake_secs}], .map_version, .epoch' "$work/far/run/mesh-status.json")"
until_ok 30 ip netns exec $ns_far curl -sf "http://10.77.0.1:$PORT_AGENT/v1/health" || fail "far can't reach the new main server's agent"
jq -e '.epoch == 2' "$work/far/run/mesh-status.json" >/dev/null || fail "far didn't take the new epoch"
ok "far reaches the new coordinator at 10.77.0.1 and took epoch 2"

echo "the apps"
until_ok 30 in_w sh -c "curl -sf -H 'Host: edge.test' http://127.0.0.1:80/ | grep -qi caddy" || fail "edge stopped answering"
ok "edge (it ran on pi) keeps answering"
web_here() { in_w sh -c "curl -sf -H 'Host: web.test' http://127.0.0.1:80/ | grep -qi caddy"; }
until_ok 180 web_here || fail "web (it ran on the old main server) didn't come back: $(papi GET /v1/platform/apps/web/deployments | jq -c '[.[] | {number, trigger, status, error}]')"
ok "web (it ran on the old main server) is recovered on the new one"

echo "the old main server returns"
env "${main_env[@]}" "$agent" serve >"$work/agent-old.log" 2>&1 &
old=$!
echo $old >"$work/agent-old.pid"
until_ok 60 test -s "$work/main/mesh/superseded.json" || fail "the old main server didn't step down"
code=0
timeout 20 tail --pid="$old" -f /dev/null || fail "the old agent didn't exit"
wait "$old" || code=$?
[[ "$code" == 3 ]] || fail "the old agent exited with $code"
grep -q "steps down" "$work/agent-old.log" || fail "no step-down message"
ok "it saw pi at epoch 2 with its coordinator key, and stepped down"
env "${main_env[@]}" "$agent" serve >"$work/agent-old2.log" 2>&1 &
echo $! >"$work/agent-old2.pid"
until_ok 30 curl -sf "http://127.0.0.1:$PORT_AGENT/v1/health" || fail "the old agent doesn't start again"
msg=$(curl -s -H "Authorization: Bearer $token" "http://127.0.0.1:$PORT_AGENT/v1/platform/apps" | jq -r .error)
[[ "$msg" == *"replaced as the main server by pi"* ]] || fail "the old main server still runs the platform: $msg"
ok "restarted, it runs neither the platform nor the coordinator: $msg"

echo "$pass checks passed"

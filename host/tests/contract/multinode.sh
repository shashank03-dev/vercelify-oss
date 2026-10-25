#!/usr/bin/env bash
# Contract test for multi-node (design §8.2, §8.3) with real Docker on two
# servers: the main server in this namespace (this machine's Docker), a
# worker in a network namespace with a Docker daemon of its own, joined by
# vercelify-mesh. It checks that the worker registers by heartbeat, that an
# app placed on it is pulled once by the main server and served to the
# worker from the mesh registry, runs there, and answers through the main
# server's proxy; then logs, restart and a terminal through the main
# server, replicas on both servers behind one route, and revocation.
#
#   cargo build -p vercelify-agent && cargo build -p vercelify-mesh --features daemon
#   sudo host/tests/contract/multinode.sh   # root, Docker, iproute2, nsenter, curl, jq
#
# Needs the images caddy:2.8-alpine, traefik:v3.7 and registry:2 here
# (pulled when missing). KEEP=1 leaves everything up for a look around.
set -euo pipefail
export NO_PROXY="*" no_proxy="*"
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
agent=${AGENT_BIN:-$repo/target/debug/vercelify-agent}
mesh=${MESH_BIN:-$repo/target/debug/vercelify-mesh}
work=$(mktemp -d /tmp/vfy-multinode.XXXXXX)
token="multinode-contract-0123456789abcdef"
ns=vfyn-worker
APP_IMAGE=${APP_IMAGE:-caddy:2.8-alpine}
PORT_HTTP=18190 PORT_HTTPS=18543 PORT_METRICS=18192 PORT_AGENT=17420

pass=0
ok() { pass=$((pass + 1)); printf '  \033[32m✓\033[0m %s\n' "$*"; }
fail() {
  printf '  \033[31m✗\033[0m %s\n' "$*" >&2
  for f in "$work"/*.log; do echo "--- $f" >&2; tail -30 "$f" >&2; done
  exit 1
}
until_ok() {
  local end=$((SECONDS + $1)); shift
  while ((SECONDS < end)); do "$@" >/dev/null 2>&1 && return 0; sleep 0.5; done
  return 1
}
# `ip netns exec` remounts /sys, which hides the cgroups Docker needs.
# Background processes call nsenter themselves, so $! is the daemon.
in_w() { nsenter --net="/var/run/netns/$ns" "$@"; }
wdocker() { docker -H "unix://$work/w/docker.sock" "$@"; }

cleanup() {
  if [[ "${KEEP:-0}" == 1 ]]; then echo "kept: $work, namespace $ns"; return; fi
  local p pids
  pids=$(cat "$work"/*.pid 2>/dev/null || true)
  for p in $pids; do kill "$p" 2>/dev/null || true; done
  # The agents stop gracefully (and could recreate the proxy meanwhile).
  for _ in $(seq 40); do
    local alive=0
    for p in $pids; do kill -0 "$p" 2>/dev/null && alive=1; done
    ((alive)) || break
    sleep 0.25
  done
  for p in $pids; do kill -9 "$p" 2>/dev/null || true; done
  if [[ -S "$work/w/docker.sock" ]]; then
    for c in $(wdocker ps -aq 2>/dev/null); do wdocker rm -f "$c" >/dev/null 2>&1 || true; done
  fi
  for c in $(docker ps -aq --filter label=vercelify.contract="$work" 2>/dev/null); do docker rm -f "$c" >/dev/null 2>&1 || true; done
  docker rm -f vercelify-proxy vfy-registry >/dev/null 2>&1 || true
  for c in $(docker ps -aq --filter label=vercelify.app-name=web); do docker rm -f "$c" >/dev/null 2>&1 || true; done
  [[ -f "$work/w/docker.pid" ]] && kill "$(cat "$work/w/docker.pid")" 2>/dev/null || true
  sleep 1
  ip link del vfyn0 2>/dev/null || true
  ip link del vfy0 2>/dev/null || true
  ip netns del "$ns" 2>/dev/null || true
  # What the worker's Docker left mounted (a container it didn't get to stop).
  local m
  for m in $(awk -v w="$work" 'index($2, w) == 1 { print $2 }' /proc/mounts | sort -r); do umount -l "$m" 2>/dev/null || true; done
  rm -rf "$work"
}
trap cleanup EXIT

[[ $EUID -eq 0 ]] || { echo "run as root (network namespaces)"; exit 1; }
for b in "$agent" "$mesh"; do [[ -x "$b" ]] || { echo "missing $b (cargo build)"; exit 1; }; done
for t in ip nsenter curl jq docker; do command -v "$t" >/dev/null || { echo "missing $t"; exit 1; }; done
docker info >/dev/null 2>&1 || { echo "Docker isn't running"; exit 1; }
if ip link show vfy0 >/dev/null 2>&1; then echo "vfy0 exists here already (a real mesh?): refusing"; exit 1; fi
for img in "$APP_IMAGE" traefik:v3.7 registry:2; do
  docker image inspect "$img" >/dev/null 2>&1 || docker pull -q "$img" >/dev/null || { echo "missing image $img"; exit 1; }
done

echo "network: main (this namespace, 192.168.88.1) — worker ($ns, 192.168.88.2)"
ip netns del "$ns" 2>/dev/null || true
ip netns add "$ns"
in_w ip link set lo up
ip link add vfyn0 type veth peer name eth0 netns "$ns"
ip addr add 192.168.88.1/24 dev vfyn0
ip link set vfyn0 up
in_w ip addr add 192.168.88.2/24 dev eth0
in_w ip link set eth0 up
in_w ip route add default via 192.168.88.1

echo "worker's Docker"
mkdir -p "$work/main" "$work/w"
nsenter --net="/var/run/netns/$ns" dockerd --data-root "$work/w/data" --exec-root "$work/w/exec" --pidfile "$work/w/docker.pid" \
  --host "unix://$work/w/docker.sock" >"$work/dockerd-worker.log" 2>&1 &
until_ok 60 wdocker info || fail "the worker's dockerd didn't start"
# The worker has no internet: its proxy's image comes from here (an app's
# image comes through the mesh registry: that is what is tested).
docker save traefik:v3.7 | wdocker load -q >/dev/null
printf '#!/bin/sh\nDOCKER_HOST=unix://%s exec docker "$@"\n' "$work/w/docker.sock" >"$work/w/docker.sh"
chmod +x "$work/w/docker.sh"
ok "two Docker daemons"

printf '%s\n' "$token" >"$work/agent.token"
common=(VERCELIFY_CONFIG=/nonexistent VERCELIFY_TOKEN_FILE="$work/agent.token" VERCELIFY_ENGINE=platform
  VERCELIFY_AUTODEPLOY_INTERVAL=0 VERCELIFY_WAKE_LISTEN='' VERCELIFY_BUILD_DRIVER=docker
  VERCELIFY_WATCH_INTERVAL=30 VERCELIFY_SYSTEMD_DIR="$work/none")
main_env=("${common[@]}" "VERCELIFY_LISTEN=127.0.0.1,mesh" "VERCELIFY_PORT=$PORT_AGENT" VERCELIFY_ROLE=main
  VERCELIFY_INSTALL_ENV="$work/main/install.env" VERCELIFY_STATE_DIR="$work/main/state" VERCELIFY_RUN_DIR="$work/main/run"
  VERCELIFY_PLATFORM_DB="$work/main/state/platform.db" VERCELIFY_MASTER_KEY_FILE="$work/main/state/master.key"
  VERCELIFY_MESH_DIR="$work/main/mesh" VERCELIFY_MESH_COORDINATOR=1 VERCELIFY_MESH_CONTROL_LISTEN=192.168.88.1:17443
  VERCELIFY_MESH_NAME=main VERCELIFY_MESH_PORT=51830 VERCELIFY_TOKENS_FILE="$work/main/state/tokens.json" VERCELIFY_AUDIT_LOG="$work/main/state/audit.log"
  "VERCELIFY_PROXY_HTTP_PORT=$PORT_HTTP" "VERCELIFY_PROXY_HTTPS_PORT=$PORT_HTTPS" "VERCELIFY_PROXY_METRICS_PORT=$PORT_METRICS"
  VERCELIFY_HDD_VOLUME_ROOT="$work/main/hdd")
env "${main_env[@]}" "$agent" serve >"$work/agent-main.log" 2>&1 &
echo $! >"$work/agent-main.pid"
until_ok 30 curl -sf "http://127.0.0.1:$PORT_AGENT/v1/health" || fail "the main agent didn't start"
until_ok 10 test -s "$work/main/mesh/state.json" || fail "the coordinator wrote no state"
"$mesh" --dir "$work/main/mesh" --run-dir "$work/main/run" up >"$work/mesh-main.log" 2>&1 &
echo $! >"$work/mesh-main.pid"
until_ok 20 sh -c "ip -4 addr show vfy0 | grep -q 10.77.0.1" || fail "vfy0 didn't come up on main"
ok "main server: platform engine, coordinator on 10.77.0.1"

api() { curl -sf -X "$1" -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
  ${3:+--data "$3"} "http://127.0.0.1:$PORT_AGENT$2"; }
api_raw() { curl -s -X "$1" -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
  ${3:+--data "$3"} "http://127.0.0.1:$PORT_AGENT$2"; }

echo "the worker joins"
join=$(api POST /v1/mesh/invites '{"kind":"server"}' | jq -r .token)
in_w "$mesh" --dir "$work/w/mesh" join "$join" --name pi --port 51831 | grep -q "joined as pi" || fail "the worker didn't join"
nsenter --net="/var/run/netns/$ns" "$mesh" --dir "$work/w/mesh" --run-dir "$work/w/run" up >"$work/mesh-worker.log" 2>&1 &
echo $! >"$work/mesh-worker.pid"
until_ok 30 in_w ping -c1 -W1 10.77.0.1 || fail "the worker can't reach main over the mesh"
w_env=("${common[@]}" VERCELIFY_LISTEN=mesh "VERCELIFY_PORT=$PORT_AGENT" VERCELIFY_ROLE=worker
  VERCELIFY_INSTALL_ENV="$work/w/install.env" VERCELIFY_STATE_DIR="$work/w/state" VERCELIFY_RUN_DIR="$work/w/run2"
  VERCELIFY_MESH_DIR="$work/w/mesh" VERCELIFY_TOKENS_FILE="$work/w/state/tokens.json" VERCELIFY_AUDIT_LOG="$work/w/state/audit.log"
  VERCELIFY_DOCKER_BIN="$work/w/docker.sh" VERCELIFY_PROXY_HTTP_PORT=80 VERCELIFY_PROXY_HTTPS_PORT=443
  VERCELIFY_PROXY_METRICS_PORT=8082 VERCELIFY_HDD_VOLUME_ROOT="$work/w/hdd")
nsenter --net="/var/run/netns/$ns" env "${w_env[@]}" "$agent" serve >"$work/agent-worker.log" 2>&1 &
echo $! >"$work/agent-worker.pid"
worker_online() { api GET /v1/platform/nodes | jq -e '.[] | select(.name == "pi" and .role == "worker" and .online and .status.docker_ok)'; }
until_ok 60 worker_online || fail "the worker didn't register (heartbeat)"
wid=$(api GET /v1/platform/nodes | jq -r '.[] | select(.name == "pi") | .id')
[[ $(api GET /v1/platform/nodes | jq -r '.[] | select(.name == "pi") | .mesh_ip') == 10.77.0.2 ]] || fail "wrong mesh address"
ok "worker registered by heartbeat: online, Docker up, at 10.77.0.2"
until_ok 60 wdocker inspect vercelify-proxy || fail "the worker's proxy didn't start"
ok "the worker runs its own proxy"
# Its runtime API answers the main server only.
code=$(in_w curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer nope" "http://10.77.0.2:$PORT_AGENT/v1/runtime/containers")
[[ "$code" == 401 ]] || fail "the runtime API took a wrong token ($code)"
ok "the worker's runtime API refuses other tokens"

echo "an app placed on the worker"
api POST /v1/platform/apps "$(jq -nc --arg i "$APP_IMAGE" --arg n "$wid" \
  '{name: "web", kind: "image", source: {image: $i}, node_id: $n, runtime: {port: 80, health_timeout_secs: 90, drain_secs: 5}}')" >/dev/null ||
  fail "creating the app: $(api_raw POST /v1/platform/apps '{}')"
api POST /v1/platform/apps/web/domains '{"host": "web.test", "https": false}' >/dev/null || fail "adding the domain"
dep=$(api POST /v1/platform/apps/web/deploy '{}' | jq -r .id)
status() { api GET "/v1/platform/deployments/$dep" | jq -r .status; }
wait_healthy() {
  local end=$((SECONDS + 240)) s
  while ((SECONDS < end)); do
    s=$(status)
    [[ "$s" == healthy ]] && return 0
    [[ "$s" == failed || "$s" == cancelled ]] && fail "deployment $s: $(api GET "/v1/platform/deployments/$dep" | jq -r .error)
$(api GET "/v1/platform/deployments/$dep/logs" | tail -20)"
    sleep 1
  done
  fail "deployment stuck at $(status)"
}
wait_healthy
inst=$(api GET /v1/platform/apps/web/instances)
[[ $(jq -r '.[0].node' <<<"$inst") == pi && $(jq length <<<"$inst") == 1 ]] || fail "instances: $inst"
container=$(jq -r '.[0].container' <<<"$inst")
wdocker inspect "$container" >/dev/null || fail "$container isn't on the worker"
docker inspect "$container" >/dev/null 2>&1 && fail "$container runs on the main server too"
ok "deployed on the worker ($container)"
wdocker inspect -f '{{.Config.Image}}' "$container" | grep -q '^127.0.0.1:5055/vfy/web:' || fail "the worker didn't pull from the mesh registry"
docker inspect vfy-registry >/dev/null || fail "no registry on the main server"
ok "image pulled once here, served to the worker by the mesh registry"
answer() { curl -sf -H "Host: web.test" "http://127.0.0.1:$PORT_HTTP/" | grep -qi caddy; }
until_ok 30 answer || fail "web.test doesn't answer through the main server's proxy: $(curl -s -H 'Host: web.test' "http://127.0.0.1:$PORT_HTTP/" | head -5)"
ok "web.test answers through the main server's proxy → the worker's proxy → the container"
grep -q "10.77.0.2:80" "$work/main/state/proxy/dynamic/base-web.yaml" || fail "the main route doesn't forward to the worker"
ok "the main server's route forwards over the mesh"

echo "running it from the main server"
logs=$(curl -sf -H "Authorization: Bearer $token" "http://127.0.0.1:$PORT_AGENT/v1/platform/apps/web/logs?tail=20")
[[ -n "$logs" ]] && jq -e '.text' <<<"$(head -1 <<<"$logs")" >/dev/null || fail "no logs from the worker: $logs"
ok "logs come from the worker"
before=$(wdocker inspect -f '{{.State.StartedAt}}' "$container")
api POST /v1/platform/apps/web/restart '{}' | jq -e '.restarted == 1' >/dev/null || fail "restart"
[[ $(wdocker inspect -f '{{.State.StartedAt}}' "$container") != "$before" ]] || fail "it didn't restart on the worker"
ok "restart reaches the worker"
term=$(api POST /v1/platform/apps/web/terminal '{"cols": 80, "rows": 24}' | jq -r .id)
[[ "$term" == term_r* ]] || fail "no relayed terminal: $term"
api POST "/v1/platform/terminals/$term/input" "$(jq -nc --arg d "$(printf 'echo mesh-$((6*7))\nexit\n' | base64 -w0)" '{data: $d}')" >/dev/null ||
  fail "terminal input"
out=$(curl -s -m 10 -H "Authorization: Bearer $token" "http://127.0.0.1:$PORT_AGENT/v1/platform/terminals/$term/output" || true)
decoded=$(jq -r 'select(.data) | .data' <<<"$out" | base64 -d 2>/dev/null || true)
[[ "$decoded" == *mesh-42* ]] || fail "terminal output: $out"
ok "a terminal in the worker's container, relayed by the main server"

echo "replicas on both servers"
api PATCH /v1/platform/apps/web '{"node_id": null, "runtime": {"port": 80, "replicas": 2, "health_timeout_secs": 90, "drain_secs": 5}}' >/dev/null ||
  fail "patch: $(api_raw PATCH /v1/platform/apps/web '{}')"
dep=$(api POST /v1/platform/apps/web/deploy '{}' | jq -r .id)
wait_healthy
inst=$(api GET /v1/platform/apps/web/instances)
main_name=$(api GET /v1/platform/nodes | jq -r '.[] | select(.role == "main") | .name')
[[ $(jq length <<<"$inst") == 2 && $(jq -r '[.[].node] | sort | join(",")' <<<"$inst") == "$(printf '%s\n' "$main_name" pi | sort | paste -sd,)" ]] ||
  fail "replicas: $inst"
upstreams=$(grep -c 'url:' "$work/main/state/proxy/dynamic/base-web.yaml")
[[ "$upstreams" == 2 ]] || fail "the main route has $upstreams upstreams: $(cat "$work/main/state/proxy/dynamic/base-web.yaml")"
until_ok 30 sh -c "! docker -H unix://$work/w/docker.sock inspect $container" || fail "the previous instance wasn't retired"
codes=""
for _ in 1 2 3 4 5 6 7 8; do codes+="$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: web.test' "http://127.0.0.1:$PORT_HTTP/") "; sleep 0.3; done
[[ "$codes" =~ ^(200\ )+$ ]] || fail "requests with replicas: $codes"
ok "two replicas (main + worker) behind one route; the old instance retired"

echo "a replica dies on the worker"
replica=$(jq -r '.[] | select(.node == "pi") | .container' <<<"$inst")
wdocker stop "$replica" >/dev/null
# Within 3 minutes of going live that is a failed deploy: the engine sees it
# on the worker and rolls back.
rolled() {
  api GET /v1/platform/apps/web/deployments |
    jq -e '.[] | select(.trigger == "auto-rollback" and .status == "healthy")'
}
until_ok 120 rolled || fail "no automatic rollback: $(api GET /v1/platform/apps/web/deployments | jq -c '[.[] | {number, trigger, status}]')"
until_ok 30 answer || fail "the app doesn't answer after the rollback"
ok "a replica dying on the worker right after a deploy rolls the app back"

echo "revocation"
api DELETE /v1/mesh/nodes/pi >/dev/null
until_ok 10 sh -c "! curl -sf -H 'Authorization: Bearer $token' http://127.0.0.1:$PORT_AGENT/v1/platform/apps/web/instances | jq -e '.[] | select(.node == \"pi\" and .state == \"running\")'" ||
  fail "a revoked worker is still driven"
ok "a revoked worker isn't called any more"

echo "$pass checks passed"

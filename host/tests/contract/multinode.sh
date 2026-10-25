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
here=$(cd "$(dirname "$0")" && pwd)
TEST_NAME=multinode
# shellcheck source-path=SCRIPTDIR source=lib-twoservers.sh
. "$here/lib-twoservers.sh"

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

#!/usr/bin/env bash
# Shared by the two-server contract tests (multinode.sh, promote.sh): the
# main server in this namespace (this machine's Docker), a worker ("pi") in a
# network namespace with a Docker daemon of its own, both on the mesh, the
# worker registered with the main server. Sourced; sets $work, $ns, $wid,
# api/api_raw/in_w/wdocker/until_ok/ok/fail, main_env and w_env.
# shellcheck disable=SC2034 # the variables are for the scripts that source this
export NO_PROXY="*" no_proxy="*"
repo=$(cd "${here:?set by the script that sources this}/../../.." && pwd)
agent=${AGENT_BIN:-$repo/target/debug/vercelify-agent}
mesh=${MESH_BIN:-$repo/target/debug/vercelify-mesh}
work=$(mktemp -d "/tmp/vfy-${TEST_NAME:-twoservers}.XXXXXX")
export TEST_NAME
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
  local app
  for app in ${TEST_APPS:-web}; do
    for c in $(docker ps -aq --filter "label=vercelify.app-name=$app"); do docker rm -f "$c" >/dev/null 2>&1 || true; done
  done
  # Project networks the test's apps made (unused ones only).
  docker network prune -f --filter label=vercelify.managed=platform >/dev/null 2>&1 || true
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
  VERCELIFY_WATCH_INTERVAL=30 VERCELIFY_SYSTEMD_DIR="$work/none" VERCELIFY_SYSTEMCTL_BIN=true)
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
  VERCELIFY_PLATFORM_DB="$work/w/state/platform.db" VERCELIFY_MASTER_KEY_FILE="$work/w/state/master.key"
  VERCELIFY_MESH_CONTROL_LISTEN=192.168.88.2:17443 VERCELIFY_MESH_NAME=pi VERCELIFY_MESH_PORT=51831
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

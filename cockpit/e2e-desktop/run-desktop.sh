#!/usr/bin/env bash
# Desktop e2e: real Tauri binary + real agent (Vercelify's engine) under Xvfb.
#
#   ./e2e-desktop/run-desktop.sh
#
# Needs: webkit2gtk-driver (WebKitWebDriver), xvfb, tauri-driver
# (`cargo install tauri-driver --locked`), ffmpeg and mpv.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(cd .. && pwd)
: "${AGENT_PORT:=7433}"
: "${STATE:=$(mktemp -d)}"
AGENT_URL=http://127.0.0.1:$AGENT_PORT
AGENT_TOKEN=desktop-e2e-token-$(date +%s)
export AGENT_URL AGENT_TOKEN
# Fresh profile: settings (config), WebKit localStorage (data) and cache.
export XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME
XDG_CONFIG_HOME=$(mktemp -d)
XDG_DATA_HOME=$(mktemp -d)
XDG_CACHE_HOME=$(mktemp -d)
mkdir -p "$ROOT/e2e-logs"

npx vite build >/dev/null
(cd "$ROOT" && cargo build -q -p vercelify-agent && cargo build -q -p vercelify-cockpit --features tauri/custom-protocol)
export COCKPIT_BIN=$ROOT/target/debug/vercelify-cockpit

pids=()
cleanup() { kill "${pids[@]}" 2>/dev/null || true; }
trap cleanup EXIT

# Media uses loopback; the independent mesh contracts exercise real private
# routing. Supply a complete state fixture so receiver_url is available (a
# comment alone did not stub the mesh and streaming silently could not start).
mkdir -p "$STATE/mesh"
cat >"$STATE/mesh/state.json" <<EOF
{"node_id":"desktop-test","name":"desktop-test","kind":"server","ipv4":"127.0.0.1","ipv6":"::1","ipv4_prefix_len":8,"certificate":"test","node_token":"test","coordinator_key":"test","coordinator":{"public_key":"test","ipv4":"127.0.0.1","endpoints":[]},"api_url":"http://127.0.0.1:$AGENT_PORT"}
EOF
VERCELIFY_CONFIG=/nonexistent VERCELIFY_ENGINE=platform VERCELIFY_LISTEN=127.0.0.1 VERCELIFY_PORT=$AGENT_PORT \
VERCELIFY_TOKEN=$AGENT_TOKEN VERCELIFY_STATE_DIR=$STATE VERCELIFY_RUN_DIR=$STATE/run \
VERCELIFY_MESH_DIR=$STATE/mesh VERCELIFY_PLATFORM_DB=$STATE/platform.db VERCELIFY_MASTER_KEY_FILE=$STATE/master.key \
VERCELIFY_TOKENS_FILE=$STATE/tokens.json VERCELIFY_AUDIT_LOG=$STATE/audit.log VERCELIFY_INSTALL_ENV=$STATE/install.env \
VERCELIFY_PROXY_HTTP_PORT=18580 VERCELIFY_PROXY_HTTPS_PORT=18581 VERCELIFY_PROXY_METRICS_PORT=18582 \
VERCELIFY_WAKE_LISTEN='' VERCELIFY_AUTODEPLOY_INTERVAL=0 \
VERCELIFY_REQUIRE_STORAGE_MOUNT=false \
VERCELIFY_MPV_EXTRA_ARGS="--vo=null --msg-level=all=warn,cplayer=info" \
  "$ROOT/target/debug/vercelify-agent" >"$ROOT/e2e-logs/desktop-agent.log" 2>&1 &
pids+=($!)

Xvfb :99 -screen 0 1600x1000x24 >/dev/null 2>&1 &
pids+=($!)
export DISPLAY=:99 WEBKIT_DISABLE_COMPOSITING_MODE=1

tauri-driver --port 4444 --native-port 4445 >"$ROOT/e2e-logs/tauri-driver.log" 2>&1 &
pids+=($!)
for _ in $(seq 40); do curl -fsS -o /dev/null http://127.0.0.1:4444/status 2>/dev/null && break; sleep 0.25; done

node --test --test-reporter=spec --test-concurrency=1 e2e-desktop/desktop.test.mjs

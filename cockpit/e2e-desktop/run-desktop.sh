#!/usr/bin/env bash
# Desktop e2e: real Tauri binary + real agent (+ Coolify) under Xvfb.
#
#   COOLIFY_TOKEN=... ./e2e-desktop/run-desktop.sh
#
# Needs: webkit2gtk-driver (WebKitWebDriver), xvfb, tauri-driver
# (`cargo install tauri-driver --locked`), ffmpeg and mpv.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(cd .. && pwd)
: "${COOLIFY_URL:=http://127.0.0.1:8000}"
: "${COOLIFY_TOKEN:?set COOLIFY_TOKEN}"
: "${AGENT_PORT:=7433}"
export AGENT_URL=http://127.0.0.1:$AGENT_PORT AGENT_TOKEN=desktop-e2e-token-$(date +%s) COOLIFY_URL COOLIFY_TOKEN
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

VERCELIFY_LISTEN=127.0.0.1 VERCELIFY_PORT=$AGENT_PORT VERCELIFY_TOKEN=$AGENT_TOKEN \
VERCELIFY_REQUIRE_STORAGE_MOUNT=false VERCELIFY_TAILSCALE_BIN=$ROOT/agent/tests/stubs/tailscale \
VERCELIFY_MPV_EXTRA_ARGS="--vo=null --msg-level=all=warn,cplayer=info" \
  "$ROOT/target/debug/vercelify-agent" --config /nonexistent >"$ROOT/e2e-logs/desktop-agent.log" 2>&1 &
pids+=($!)

Xvfb :99 -screen 0 1600x1000x24 >/dev/null 2>&1 &
pids+=($!)
export DISPLAY=:99 WEBKIT_DISABLE_COMPOSITING_MODE=1

tauri-driver --port 4444 --native-port 4445 >"$ROOT/e2e-logs/tauri-driver.log" 2>&1 &
pids+=($!)
for _ in $(seq 40); do curl -fsS -o /dev/null http://127.0.0.1:4444/status 2>/dev/null && break; sleep 0.25; done

node --test --test-reporter=spec --test-concurrency=1 e2e-desktop/desktop.test.mjs

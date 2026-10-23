#!/usr/bin/env bash
# Live end-to-end test: real agent + real Coolify + the built cockpit UI.
#
#   COOLIFY_TOKEN=... ./e2e/run-live.sh            (Coolify on http://127.0.0.1:8000)
#
# Starts the agent (debug build) against this machine's Docker, serves the
# production UI with `vite preview` (proxying /__agent and /__coolify), and
# runs Playwright. Screenshots land in ../test-results.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(cd .. && pwd)
: "${COOLIFY_URL:=http://127.0.0.1:8000}"
: "${COOLIFY_TOKEN:?set COOLIFY_TOKEN to a Coolify API token}"
: "${AGENT_PORT:=7431}"
: "${E2E_HDD:=$(mktemp -d)}"
export E2E_AGENT_TOKEN=${E2E_AGENT_TOKEN:-e2e-agent-token-$(date +%s)}
export E2E_HDD COOLIFY_URL COOLIFY_TOKEN

(cd "$ROOT" && cargo build -q -p vercelify-agent)
mkdir -p "$E2E_HDD/volumes" "$ROOT/e2e-logs"

VERCELIFY_LISTEN=127.0.0.1 VERCELIFY_PORT=$AGENT_PORT VERCELIFY_TOKEN=$E2E_AGENT_TOKEN \
VERCELIFY_STORAGE_MOUNT=$E2E_HDD VERCELIFY_HDD_VOLUME_ROOT=$E2E_HDD/volumes VERCELIFY_REQUIRE_STORAGE_MOUNT=false \
VERCELIFY_TAILSCALE_BIN=${VERCELIFY_TAILSCALE_BIN:-$ROOT/agent/tests/stubs/tailscale} \
VERCELIFY_MPV_EXTRA_ARGS="${VERCELIFY_MPV_EXTRA_ARGS:---vo=null}" \
  "$ROOT/target/debug/vercelify-agent" --config /nonexistent >"$ROOT/e2e-logs/agent.log" 2>&1 &
AGENT_PID=$!
PREVIEW_PID=''
trap 'kill $AGENT_PID ${PREVIEW_PID:-} 2>/dev/null || true' EXIT

npx vite build >/dev/null
VERCELIFY_AGENT_URL=http://127.0.0.1:$AGENT_PORT VERCELIFY_COOLIFY_URL=$COOLIFY_URL npx vite preview >"$ROOT/e2e-logs/preview.log" 2>&1 &
PREVIEW_PID=$!
for _ in $(seq 60); do curl -fsS -o /dev/null http://localhost:1421 2>/dev/null && break; sleep 0.5; done

npx playwright test "$@"

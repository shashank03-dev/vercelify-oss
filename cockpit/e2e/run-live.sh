#!/usr/bin/env bash
# Live end-to-end test: a real agent running Vercelify's engine on this
# machine's Docker, and the built cockpit UI.
#
#   ./e2e/run-live.sh
#
# Starts the agent (debug build) with the platform engine and its own proxy on
# spare ports, serves the production UI with `vite preview` (proxying
# /__agent), and runs Playwright. Screenshots land in ../test-results.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(cd .. && pwd)
# shellcheck source=../../host/tests/contract/lib-guard.sh
. "$ROOT/host/tests/contract/lib-guard.sh"
: "${AGENT_PORT:=7431}"
: "${E2E_PROXY_HTTP:=18480}"
: "${E2E_HDD:=$(mktemp -d)}"
: "${E2E_STATE:=$(mktemp -d)}"
: "${E2E_IMAGE:=traefik/whoami:latest}"
export E2E_AGENT_TOKEN=${E2E_AGENT_TOKEN:-e2e-agent-token-$(date +%s)}
export E2E_HDD E2E_IMAGE E2E_PROXY_HTTP E2E_AGENT_URL=http://127.0.0.1:$AGENT_PORT

(cd "$ROOT" && cargo build -q -p vercelify-agent)
# What an earlier run left: every run starts with a fresh engine database.
docker ps -aq --filter name=vfy-e2e- --filter name=vfy-db-e2e- | xargs -r docker rm -f >/dev/null
docker volume ls -q --filter name=vfy-db-e2e- | xargs -r docker volume rm >/dev/null
# A real "HDD": the cockpit offers moves to it only when it is a mounted disk.
HDD_IMG=''
if [ "$(id -u)" = 0 ] && ! mountpoint -q "$E2E_HDD"; then
  HDD_IMG=$(mktemp --suffix=.img)
  truncate -s 512M "$HDD_IMG" && mkfs.ext4 -q "$HDD_IMG" && mount -o loop "$HDD_IMG" "$E2E_HDD"
fi
mkdir -p "$E2E_HDD/volumes" "$ROOT/e2e-logs"
# Database backups go to a restic repository, as `vercelify backup setup` makes.
if command -v restic >/dev/null; then
  printf 'RESTIC_PASSWORD=e2e-backup-password\nBACKUP_REPOS=%s\n' "$E2E_STATE/restic" >"$E2E_STATE/backup.env"
  RESTIC_PASSWORD=e2e-backup-password restic -q init --repo "$E2E_STATE/restic" >/dev/null
  export E2E_RESTIC=1
fi

VERCELIFY_CONFIG=/nonexistent VERCELIFY_ENGINE=platform VERCELIFY_LISTEN=127.0.0.1 VERCELIFY_PORT=$AGENT_PORT \
VERCELIFY_TOKEN=$E2E_AGENT_TOKEN VERCELIFY_STATE_DIR=$E2E_STATE VERCELIFY_RUN_DIR=$E2E_STATE/run \
VERCELIFY_PLATFORM_DB=$E2E_STATE/platform.db VERCELIFY_MASTER_KEY_FILE=$E2E_STATE/master.key \
VERCELIFY_TOKENS_FILE=$E2E_STATE/tokens.json VERCELIFY_AUDIT_LOG=$E2E_STATE/audit.log VERCELIFY_INSTALL_ENV=$E2E_STATE/install.env \
VERCELIFY_PROXY_HTTP_PORT=$E2E_PROXY_HTTP VERCELIFY_PROXY_HTTPS_PORT=$((E2E_PROXY_HTTP + 1)) VERCELIFY_PROXY_METRICS_PORT=$((E2E_PROXY_HTTP + 2)) \
VERCELIFY_BUILD_DRIVER=docker VERCELIFY_WAKE_LISTEN='' VERCELIFY_AUTODEPLOY_INTERVAL=0 VERCELIFY_BACKUP_ENV=$E2E_STATE/backup.env \
VERCELIFY_STORAGE_MOUNT=$E2E_HDD VERCELIFY_HDD_VOLUME_ROOT=$E2E_HDD/volumes VERCELIFY_REQUIRE_STORAGE_MOUNT=false \
VERCELIFY_MPV_BIN=${VERCELIFY_MPV_BIN:-$(command -v mpv || echo "$ROOT/agent/tests/stubs/mpv")} \
VERCELIFY_MPV_EXTRA_ARGS="${VERCELIFY_MPV_EXTRA_ARGS:---vo=null}" \
  "$ROOT/target/debug/vercelify-agent" >"$ROOT/e2e-logs/agent.log" 2>&1 &
AGENT_PID=$!
PREVIEW_PID=''
cleanup() {
  kill "$AGENT_PID" ${PREVIEW_PID:+"$PREVIEW_PID"} 2>/dev/null || true
  if [ -n "$HDD_IMG" ]; then
    sleep 1
    umount "$E2E_HDD" 2>/dev/null && rm -f "$HDD_IMG"
  fi
}
trap cleanup EXIT
for _ in $(seq 60); do curl -fsS -o /dev/null "$E2E_AGENT_URL/v1/health" 2>/dev/null && break; sleep 0.5; done

npx vite build >/dev/null
VERCELIFY_AGENT_URL=$E2E_AGENT_URL npx vite preview >"$ROOT/e2e-logs/preview.log" 2>&1 &
PREVIEW_PID=$!
for _ in $(seq 60); do curl -fsS -o /dev/null http://localhost:1421 2>/dev/null && break; sleep 0.5; done

npx playwright test "$@"

#!/usr/bin/env bash
# Start/stop only the disposable target for offhost-builds.py.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=lib-guard.sh
. "$here/lib-guard.sh"
work=/tmp/vfy-offhost-contract
# Docker-in-Docker may put /tmp on tmpfs; image handoff staging belongs on disk.
state=/var/lib/vfy-offhost-contract-state
if [[ ${1:-} == stop ]]; then
  if [[ -f $work/pid ]]; then
    pid=$(cat "$work/pid")
    if [[ $pid =~ ^[0-9]+$ && -r /proc/$pid/comm && $(cat "/proc/$pid/comm") == vercelify-agent ]]; then
      kill "$pid" 2>/dev/null || true
    fi
  fi
  docker ps -aq --filter label=vercelify.app-name=offhost-contract | xargs -r docker rm -f >/dev/null
  docker rm -f vercelify-proxy >/dev/null 2>&1 || true
  rm -rf "$work" "$state"
  exit
fi
: "${AGENT_BIN:?candidate agent binary required}"
mkdir -p "$state" "$work/run"
if [[ -e $work/state && ! -L $work/state ]]; then
  echo 'Stop the old disposable target before restarting with persistent staging.' >&2
  exit 1
fi
[[ -L $work/state ]] || ln -s "$state" "$work/state"
cat > "$work/config.toml" <<CONF
build_concurrency = 0
docker_bin = "$work/docker"
git_bin = "$work/forbidden-git"
CONF
cat > "$work/docker" <<WRAP
#!/bin/sh
case "\$1 \$2" in 'buildx build') echo forbidden >> '$work/target-builds'; exit 99 ;; esac
exec /usr/local/bin/docker "\$@"
WRAP
printf '#!/bin/sh\necho forbidden >> %s/target-builds\nexit 99\n' "$work" > "$work/forbidden-git"
chmod 755 "$work/docker" "$work/forbidden-git"
printf 'header = "Authorization: Bearer offhost-contract-only"\n' > "$work/curl.conf"
chmod 600 "$work/curl.conf"
VERCELIFY_CONFIG="$work/config.toml" VERCELIFY_ENGINE=platform VERCELIFY_ROLE=main \
VERCELIFY_LISTEN=0.0.0.0 VERCELIFY_PORT=17425 VERCELIFY_TOKEN=offhost-contract-only \
VERCELIFY_STATE_DIR="$work/state" VERCELIFY_RUN_DIR="$work/run" \
VERCELIFY_PLATFORM_DB="$work/state/platform.db" VERCELIFY_MASTER_KEY_FILE="$work/state/master.key" \
VERCELIFY_PROXY_HTTP_PORT=18080 VERCELIFY_PROXY_HTTPS_PORT=18443 VERCELIFY_PROXY_METRICS_PORT=18084 \
VERCELIFY_WATCH_INTERVAL=0 VERCELIFY_AUTODEPLOY_INTERVAL=0 VERCELIFY_WAKE_LISTEN='' \
VERCELIFY_TOKENS_FILE="$work/state/tokens.json" VERCELIFY_AUDIT_LOG="$work/state/audit.log" \
"$AGENT_BIN" serve > "$work/agent.txt" 2>&1 &
echo $! > "$work/pid"
for _ in $(seq 30); do
  curl -fsS http://127.0.0.1:17425/v1/health >/dev/null 2>&1 && exit 0
  sleep 1
done
cat "$work/agent.txt" >&2
exit 1

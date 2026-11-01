#!/usr/bin/env bash
set -euo pipefail
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
export VERCELIFY_ETC="$T/etc" VERCELIFY_STATE="$T/state" STORAGE_MOUNT="$T/storage" DOCKER_VOLUMES_DIR="$T/volumes"
export BACKUP_STAGE="$T/state/backup-stage" PLATFORM_DB="$T/state/platform.db" BACKUP_ENV_FILE="$T/etc/backup.env" AGENT_CONFIG="$T/etc/agent.toml" MESH_DIR="$T/state/mesh"
mkdir -p "$VERCELIFY_ETC" "$VERCELIFY_STATE" "$STORAGE_MOUNT/volumes" "$DOCKER_VOLUMES_DIR/db/_data"
printf "RESTIC_PASSWORD='synthetic'\nBACKUP_REPOS='%s/repo'\n" "$T" >"$BACKUP_ENV_FILE"
export C_BOLD='' C_RESET=''
export ROLE=main
export RESTIC=/usr/bin/true
run_cmd() { printf '%s\n' "$*" >>"$T/calls"; "$@"; }
info() { :; }; ok() { :; }; step() { :; }; warn() { printf 'warning: %s\n' "$*"; }; die() { echo "ERROR: $*"; exit 1; }; confirm() { return 0; }
restic() { return 0; }
systemctl() { return 0; }
docker() { case "$1" in ps) printf 'vercelify-proxy\nvfy-app\nunrelated-production-db\n';; stop) return 1;; *) return 0;; esac; }
source /src/host/lib/backup.sh
out="$T/restore"
mkdir -p "$out$BACKUP_STAGE" "$out$DOCKER_VOLUMES_DIR/db/_data"
printf 'snapshot database\n' >"$out$BACKUP_STAGE/platform.db"
printf 'synthetic key\n' >"$out$VERCELIFY_STATE/master.key"
printf 'snapshot content\n' >"$out$DOCKER_VOLUMES_DIR/db/_data/keep"
printf 'data created after snapshot\n' >"$DOCKER_VOLUMES_DIR/db/_data/stale"
printf 'current\n' >"$PLATFORM_DB"
printf 'new-installer-token\n' >"$VERCELIFY_STATE/tokens.json"
mkdir -p "$out$VERCELIFY_ETC"
printf 'old-device-token\n' >"$out$VERCELIFY_STATE/tokens.json"
backup_restore latest --instance --target "$out"
printf 'restore_returned_success=true\n'
printf 'stale_file_survived=%s\n' "$([[ -f "$DOCKER_VOLUMES_DIR/db/_data/stale" ]] && echo true || echo false)"
printf 'state_overwritten_despite_failed_stop=%s\n' "$([[ $(cat "$PLATFORM_DB") == 'snapshot database' ]] && echo true || echo false)"
printf 'old_device_tokens_restored=%s\n' "$([[ $(cat "$VERCELIFY_STATE/tokens.json") == 'old-device-token' ]] && echo true || echo false)"
cat "$T/calls"
have() { return 0; }
write_file() { mkdir -p "$(dirname "$1")"; cat >"$1"; chmod "$2" "$1"; }
export SYSTEMD_DIR="$T/systemd"
printf "RESTIC_PASSWORD='synthetic'\nBACKUP_REPOS='%s/repo'\nAWS_ACCESS_KEY_ID='synthetic-id'\nAWS_SECRET_ACCESS_KEY='synthetic-key'\nBACKUP_KEEP_DAILY=30\n" "$T" >"$BACKUP_ENV_FILE"
backup_setup >/dev/null 2>&1
printf 's3_credentials_preserved_on_setup=%s\n' "$(grep -q '^AWS_SECRET_ACCESS_KEY=' "$BACKUP_ENV_FILE" && echo true || echo false)"
printf 'custom_retention_preserved_on_setup=%s\n' "$(grep -q '^BACKUP_KEEP_DAILY=30$' "$BACKUP_ENV_FILE" && echo true || echo false)"

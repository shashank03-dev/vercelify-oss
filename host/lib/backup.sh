#!/usr/bin/env bash
# Backups with restic: encrypted, deduplicated, to the storage disk, another
# server in your pool (over the mesh) and/or any S3-compatible bucket.
#
# What is saved: the engine's state (a consistent copy of platform.db and the
# master key that unseals every stored secret), /etc/vercelify, consistent
# dumps of every database container, and the data of app volumes on both
# tiers. The live platform.db is never read: SQLite may be mid-write.

: "${VERCELIFY_STATE:=/var/lib/vercelify}"
: "${BACKUP_ENV_FILE:=$VERCELIFY_ETC/backup.env}"
: "${BACKUP_STAGE:=$VERCELIFY_STATE/backup-stage}"
: "${PLATFORM_DB:=$VERCELIFY_STATE/platform.db}"
: "${SQLITE3:=sqlite3}"
: "${DOCKER_VOLUMES_DIR:=/var/lib/docker/volumes}"
: "${RESTIC:=restic}"
BACKUP_LOG="$VERCELIFY_STATE/backup.log"
BACKUP_STATUS="$VERCELIFY_STATE/backup.json"

# POSIX single-quoted literals: the shell and the agent decode the same bytes,
# including quotes, backslashes and newlines, without expanding credentials.
backup_env_assignment() {
  local value=$2
  value=${value//\'/\'\\\'\'}
  printf "%s='%s'\n" "$1" "$value"
}

backup_load_env() {
  [[ -r "$BACKUP_ENV_FILE" ]] || die "backups are not set up — run: sudo vercelify backup setup"
  set -a
  # shellcheck disable=SC1090
  . "$BACKUP_ENV_FILE"
  set +a
  [[ -n "${RESTIC_PASSWORD:-}" && -n "${BACKUP_REPOS:-}" ]] || die "$BACKUP_ENV_FILE is incomplete"
}

default_backup_repo() {
  if mountpoint -q "$STORAGE_MOUNT" 2>/dev/null; then
    echo "$STORAGE_MOUNT/backups/restic"
  else
    echo "/var/backups/vercelify-restic"
  fi
}

# What a new `backup setup` keeps from the existing backup.env unless it is
# given again: credentials of off-site repositories and how long backups are
# kept (a reset would prune history the user chose to keep).
BACKUP_KEPT_VARS=(AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN AWS_DEFAULT_REGION
  B2_ACCOUNT_ID B2_ACCOUNT_KEY AZURE_ACCOUNT_NAME AZURE_ACCOUNT_KEY
  GOOGLE_PROJECT_ID GOOGLE_APPLICATION_CREDENTIALS
  BACKUP_KEEP_DAILY BACKUP_KEEP_WEEKLY BACKUP_KEEP_MONTHLY)

# vercelify backup setup [--to REPO]... [--s3-key ID --s3-secret SECRET]
backup_setup() {
  local repos=() key='' secret='' pw r v
  while (( $# )); do
    case "$1" in
      --to) repos+=("${2:?--to needs a repository, e.g. sftp:root@pi:/mnt/storage/backups/$(hostname -s)}"); shift ;;
      --s3-key) key=${2:?}; shift ;;
      --s3-secret) secret=${2:?}; shift ;;
      *) die "unknown option $1" ;;
    esac
    shift
  done
  have "$RESTIC" || apt_install restic || die "could not install restic"
  local -A kept=()
  if [[ -r "$BACKUP_ENV_FILE" ]]; then
    # shellcheck disable=SC1090
    pw=$(. "$BACKUP_ENV_FILE"; printf '%s' "${RESTIC_PASSWORD:-}")
    if (( ${#repos[@]} == 0 )); then
      # shellcheck disable=SC1090
      read -r -a repos <<<"$(. "$BACKUP_ENV_FILE"; printf '%s' "${BACKUP_REPOS:-}")"
    fi
    for v in "${BACKUP_KEPT_VARS[@]}"; do
      # shellcheck disable=SC1090
      if (unset "$v"; . "$BACKUP_ENV_FILE"; [[ -n "${!v+set}" ]]); then
        # shellcheck disable=SC1090
        kept[$v]=$(. "$BACKUP_ENV_FILE"; printf '%s' "${!v}")
      fi
    done
  fi
  [[ -n "$key" ]] && kept[AWS_ACCESS_KEY_ID]=$key
  [[ -n "$secret" ]] && kept[AWS_SECRET_ACCESS_KEY]=$secret
  for v in BACKUP_KEEP_DAILY:7 BACKUP_KEEP_WEEKLY:4 BACKUP_KEEP_MONTHLY:3; do
    [[ "${kept[${v%%:*}]:-}" =~ ^[0-9]{1,4}$ ]] || kept[${v%%:*}]=${v#*:}
  done
  # Restoring onto a new machine: reuse the password of the existing backups.
  pw=${VERCELIFY_BACKUP_PASSWORD:-${pw:-$(random_hex 24)}}
  (( ${#repos[@]} )) || repos=("$(default_backup_repo)")
  for r in "${repos[@]}"; do
    [[ "$r" =~ ^[A-Za-z0-9:@/._~+=-]+$ ]] || die "invalid repository '$r'"
  done
  # The new settings are checked against the repositories first, and only
  # then replace the working file: a failed setup leaves it as it was.
  local new="$BACKUP_ENV_FILE.new"
  rm -f "$new"
  {
    echo "# Vercelify backups (sudo vercelify backup). Keep RESTIC_PASSWORD somewhere safe:"
    echo "# without it, nobody (you included) can restore these backups."
    backup_env_assignment RESTIC_PASSWORD "$pw"
    echo "BACKUP_REPOS=\"${repos[*]}\""
    for v in "${BACKUP_KEPT_VARS[@]}"; do
      [[ -n "${kept[$v]+set}" ]] || continue
      if [[ $v == BACKUP_KEEP_* ]]; then echo "$v=${kept[$v]}"; else backup_env_assignment "$v" "${kept[$v]}"; fi
    done
  } | write_file "$new" 0600 || true
  setup_failed() { rm -f "$new"; die "$@"; }
  if [[ -e "$new" ]]; then BACKUP_ENV_FILE=$new backup_load_env; else backup_load_env; fi
  for r in "${repos[@]}"; do
    [[ "$r" == /* ]] && run_cmd mkdir -p "$r"
    if RESTIC_REPOSITORY=$r "$RESTIC" cat config >/dev/null 2>&1; then
      ok "repository ready: $r"
    elif [[ -n "${VERCELIFY_BACKUP_PASSWORD:-}" ]] && [[ "$r" != /* || -e "$r/config" ]]; then
      setup_failed "cannot open $r with that password"
    elif [[ "$r" == /* && -e "$r/config" ]]; then
      # Backups from an earlier install whose password went with its config:
      # never re-create over them, say how to get them back.
      setup_failed "$r already holds backups locked with another password (from an earlier install?). Either put back the old $BACKUP_ENV_FILE, or give its password: sudo VERCELIFY_BACKUP_PASSWORD='…' vercelify backup setup — or start fresh, keeping the old ones aside: sudo mv $r $r.old && sudo vercelify backup setup"
    else
      run_cmd env RESTIC_REPOSITORY="$r" "$RESTIC" init >/dev/null || setup_failed "could not create the repository $r"
      ok "created repository: $r"
    fi
  done
  if [[ -e "$new" ]]; then
    run_cmd mv -f "$new" "$BACKUP_ENV_FILE"
  fi
  write_file "$SYSTEMD_DIR/vercelify-backup.service" 0644 <<EOF || true
[Unit]
Description=Vercelify backup
After=docker.service network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/vercelify backup run
# Gentle on an old laptop or a Pi: background CPU and disk priority.
Nice=15
IOSchedulingClass=idle
EOF
  write_file "$SYSTEMD_DIR/vercelify-backup.timer" 0644 <<'EOF' || true
[Unit]
Description=Nightly Vercelify backup

[Timer]
OnCalendar=*-*-* 03:17
RandomizedDelaySec=30min
Persistent=true

[Install]
WantedBy=timers.target
EOF
  run_cmd systemctl daemon-reload
  run_cmd systemctl enable --now vercelify-backup.timer >/dev/null 2>&1 || true
  ok "nightly backups on (around 03:17) — run one now with: sudo vercelify backup run"
  printf '\n  %sBackup password%s (store it in your password manager):\n    %s\n\n' "$C_BOLD" "$C_RESET" "$pw" >&2
}

write_backup_status() { # OK(true|false) ERROR SNAPSHOT
  jq -nc --argjson ok "$1" --arg e "$2" --arg s "${3:-}" --arg t "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{ok: $ok, finished_at: $t, error: (if $e == "" then null else $e end), snapshot: (if $s == "" then null else $s end)}' |
    write_file "$BACKUP_STATUS" 0644 || true
}

# Consistent dumps of every database container into the staging directory.
# A dump that fails is reported, but never stops the rest from being saved
# (the engine's state matters more than any one app's database). Prints the
# names that failed.
dump_databases() {
  local stage=$1 id name image n=0 failed=()
  mkdir -p "$stage/databases"
  # The engine's own database, fresh: sqlite3's online backup. Without
  # sqlite3, the copy the agent keeps (platform.db.bak, every 5 min) is saved.
  if [[ -f "$PLATFORM_DB" ]] && have "$SQLITE3"; then
    if "$SQLITE3" "$PLATFORM_DB" ".backup '$stage/platform.db'" 2>/dev/null; then
      n=$((n + 1))
    else
      warn "a fresh copy of the engine's database failed; its 5-minute copy is saved"
      rm -f "$stage/platform.db"
    fi
  fi
  while read -r id name image; do
    [[ -n "$id" ]] || continue
    local rc=0
    case "$image" in
      *postgres*|*postgis*)
        docker exec "$id" sh -c 'pg_dumpall -U "${POSTGRES_USER:-postgres}"' >"$stage/databases/$name.sql" || rc=$? ;;
      *mariadb*)
        docker exec "$id" sh -c 'exec mariadb-dump --all-databases --single-transaction -uroot -p"${MARIADB_ROOT_PASSWORD:-$MYSQL_ROOT_PASSWORD}"' >"$stage/databases/$name.sql" || rc=$? ;;
      *mysql*)
        docker exec "$id" sh -c 'exec mysqldump --all-databases --single-transaction -uroot -p"$MYSQL_ROOT_PASSWORD"' >"$stage/databases/$name.sql" || rc=$? ;;
      *mongo*)
        docker exec "$id" sh -c 'exec mongodump --archive ${MONGO_INITDB_ROOT_USERNAME:+-u "$MONGO_INITDB_ROOT_USERNAME" -p "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin}' >"$stage/databases/$name.archive" || rc=$? ;;
      *redis*|*valkey*|*keydb*|*dragonfly*)
        # Flush to disk; the files are saved with the volumes below.
        docker exec "$id" sh -c 'REDISCLI_AUTH="${VFY_PASSWORD:-$REDIS_PASSWORD}" "$(command -v valkey-cli || command -v keydb-cli || echo redis-cli)" SAVE' >/dev/null 2>&1 || true
        continue ;;
      *) continue ;;
    esac
    if (( rc )); then
      err "dumping $name failed (its files are still saved with the volumes)"
      failed+=("$name")
    else
      n=$((n + 1))
    fi
  done < <(docker ps --format '{{.ID}} {{.Names}} {{.Image}}')
  info "dumped $n database(s)" >&2
  printf '%s\n' "${failed[@]}"
}

backup_paths() {
  local p
  for p in "$VERCELIFY_ETC" "$VERCELIFY_STATE" "$(master_key_file)" \
           "$STORAGE_MOUNT/volumes" "$DOCKER_VOLUMES_DIR" "$BACKUP_STAGE"; do
    [[ -e "$p" ]] && printf '%s\n' "$p"
  done
  return 0
}

backup_run() {
  backup_load_env
  mkdir -p "$VERCELIFY_STATE"
  exec > >(tee -a "$BACKUP_LOG") 2>&1
  step "Backup $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  local snap='' r save_failed='' out dump_failed paths=()
  rm -rf "$BACKUP_STAGE"; mkdir -p "$BACKUP_STAGE"; chmod 0700 "$BACKUP_STAGE"
  dump_failed=$(dump_databases "$BACKUP_STAGE" | paste -sd, -)
  mapfile -t paths < <(backup_paths)
  for r in $BACKUP_REPOS; do
    info "saving to $r"
    if out=$(RESTIC_REPOSITORY=$r "$RESTIC" backup --tag vercelify --host "$(hostname -s)" \
        --exclude-caches --exclude "$DOCKER_VOLUMES_DIR/ollama-models" --exclude "$DOCKER_VOLUMES_DIR/*/_data/lost+found" \
        --exclude "$VERCELIFY_STATE/backup.log" --exclude "$PLATFORM_DB" --exclude "$PLATFORM_DB-wal" \
        --exclude "$PLATFORM_DB-shm" --exclude "${ROLLBACK_DIR:-$VERCELIFY_STATE/rollback}" "${paths[@]}" 2>&1); then
      snap=$(sed -n 's/^snapshot \([0-9a-f]*\) saved.*/\1/p' <<<"$out" | tail -1)
      ok "snapshot ${snap:-saved} in $r"
      RESTIC_REPOSITORY=$r "$RESTIC" forget --tag vercelify --host "$(hostname -s)" --prune \
        --keep-daily "${BACKUP_KEEP_DAILY:-7}" --keep-weekly "${BACKUP_KEEP_WEEKLY:-4}" \
        --keep-monthly "${BACKUP_KEEP_MONTHLY:-3}" >/dev/null 2>&1 || warn "pruning old snapshots in $r failed"
    else
      err "$r: $(tail -3 <<<"$out")"
      save_failed+="${save_failed:+, }$r"
    fi
  done
  rm -rf "$BACKUP_STAGE"
  if [[ -n "$save_failed" ]]; then
    write_backup_status false "could not save to $save_failed" "$snap"
    die "backup failed for: $save_failed"
  fi
  if [[ -n "$dump_failed" ]]; then
    write_backup_status false "saved, but these databases could not be dumped: $dump_failed" "$snap"
    die "backup saved, but dumping failed for: $dump_failed"
  fi
  write_backup_status true "" "$snap"
  ok "backup complete"
}

backup_list() {
  backup_load_env
  local r=${1:-${BACKUP_REPOS%% *}}
  RESTIC_REPOSITORY=$r "$RESTIC" snapshots --tag vercelify
}

# Docker volumes that belong to Vercelify (apps, previews, compose projects,
# databases); anything else on the Docker host is someone else's data.
owned_volume() { [[ $1 == vfy-* || $1 == vfyv2-* ]]; }

# Running containers that write what an instance restore replaces: ours, and
# any other one using a volume being restored (ALL=1: every container but the
# proxy and the registry).
restore_writers() { # ALL VOLUME...
  local all=$1 v; shift
  {
    if (( all )); then
      docker ps --format '{{.Names}}' | grep -vx -e vercelify-proxy -e vfy-registry
    else
      docker ps --filter label=vercelify.managed=platform --format '{{.Names}}'
      for v in "$@"; do docker ps --filter "volume=$v" --format '{{.Names}}'; done
    fi
  } | sed '/^$/d' | sort -u
}

# Replace LIVE with exactly SNAPSHOT's tree: copied beside it first, then
# switched; the previous tree stays at LIVE.before-restore-STAMP.
restore_tree() { # SNAPSHOT LIVE STAMP
  local src=$1 live=$2 stage="$2.vfy-restore"
  run_cmd rm -rf "$stage"
  run_cmd mkdir -p "$(dirname "$live")"
  run_cmd rsync -aHAX --numeric-ids --delete "$src/" "$stage/" || return 1
  if [[ -e "$live" ]]; then run_cmd mv "$live" "$live.before-restore-$3" || return 1; fi
  run_cmd mv "$stage" "$live"
}

# vercelify backup restore [SNAPSHOT] [--target DIR | --instance [--all-volumes]] [--from REPO]
backup_restore() {
  backup_load_env
  local snap=latest target='' instance=0 all=0 repo=${BACKUP_REPOS%% *}
  while (( $# )); do
    case "$1" in
      --target) target=${2:?}; shift ;;
      --instance) instance=1 ;;
      --all-volumes) all=1 ;;
      --from) repo=${2:?}; shift ;;
      -*) die "unknown option $1" ;;
      *) snap=$1 ;;
    esac
    shift
  done
  [[ "$snap" =~ ^[0-9a-f]{8,64}$|^latest$ ]] || die "invalid snapshot '$snap'"
  target=${target:-$VERCELIFY_STATE/restore-$(date +%Y%m%d-%H%M%S)}
  mkdir -p "$target"; chmod 0700 "$target"
  step "Restoring snapshot $snap from $repo"
  run_cmd env RESTIC_REPOSITORY="$repo" "$RESTIC" restore "$snap" --target "$target" || die "restore failed"
  ok "files restored under $target"
  (( instance )) || {
    info "databases are in $target$BACKUP_STAGE/databases; to put this server back as it was: sudo vercelify backup restore $snap --instance"
    return 0
  }
  [[ $ROLE == main ]] || die "--instance restores a main server; on a worker copy what you need from $target"
  local t=$target db=''
  if [[ -f "$t$BACKUP_STAGE/platform.db" ]]; then
    db="$t$BACKUP_STAGE/platform.db"
  elif [[ -f "$t$PLATFORM_DB.bak" ]]; then
    db="$t$PLATFORM_DB.bak"
  fi
  [[ -n "$db" ]] || die "no engine state in this snapshot; the files are in $t"
  [[ -f "$t$(master_key_file)" ]] || die "the snapshot has no master key: its secrets could not be unsealed. Files are in $t"
  # The manifest: which volumes come back. Docker volumes that aren't ours
  # are left alone unless --all-volumes says so.
  local vols=() skipped=() hdd=() d name
  for d in "$t$DOCKER_VOLUMES_DIR"/*/; do
    [[ -d "$d/_data" ]] || continue
    name=$(basename "$d")
    if (( all )) || owned_volume "$name"; then vols+=("$name"); else skipped+=("$name"); fi
  done
  for d in "$t$STORAGE_MOUNT/volumes"/*/; do
    [[ -d "$d" ]] && hdd+=("$(basename "$d")")
  done
  info "volumes to restore: ${vols[*]:-none}${hdd[*]:+; storage disk: ${hdd[*]}}"
  (( ${#skipped[@]} )) && warn "not Vercelify's, left as they are: ${skipped[*]} (--all-volumes restores them too)"
  confirm "Replace this server's apps, settings, secrets and app data with the backup?" n || die "cancelled (files stay in $t)"
  # The engine stops first: it would put back what it last knew.
  run_cmd systemctl stop vercelify-agent || die "the agent did not stop; nothing was changed (files are in $t)"
  # Every writer of what is replaced stops, and is seen stopped, before any
  # live file changes: a database must not see its files change under it.
  local writers=() left=()
  mapfile -t writers < <(restore_writers "$all" "${vols[@]}")
  if (( ${#writers[@]} )); then
    run_cmd docker stop "${writers[@]}" >/dev/null || warn "some containers did not stop"
    mapfile -t left < <(restore_writers "$all" "${vols[@]}")
  fi
  if (( ${#left[@]} )); then
    run_cmd docker start "${writers[@]}" >/dev/null || true
    run_cmd systemctl start vercelify-agent || true
    die "still running: ${left[*]} — nothing was changed and everything was started again (files are in $t)"
  fi
  local stamp tokens
  stamp=$(date +%Y%m%d-%H%M%S)
  tokens=$(agent_setting tokens_file "$VERCELIFY_STATE/tokens.json")
  run_cmd install -m 0600 "$t$(master_key_file)" "$(master_key_file)"
  run_cmd rm -f "$PLATFORM_DB-wal" "$PLATFORM_DB-shm"
  run_cmd install -m 0600 "$db" "$PLATFORM_DB"
  # Who may use the API: paired devices' tokens (as of the backup — revoked
  # since then means revoke again) and the server's own token. The machine's
  # own settings (agent.toml, install.env, tunnels) stay as they are.
  [[ -f "$t$tokens" ]] && run_cmd install -m 0600 "$t$tokens" "$tokens"
  [[ -f "$t$VERCELIFY_ETC/agent.token" ]] && run_cmd install -m 0600 "$t$VERCELIFY_ETC/agent.token" "$VERCELIFY_ETC/agent.token"
  # The network's identity too (the coordinator's keys, this node's): workers
  # and devices keep their certificates and find the server again.
  local mesh=${MESH_DIR:-$VERCELIFY_STATE/mesh} restored_mesh=0
  if [[ -d "$t$mesh" ]]; then
    run_cmd systemctl stop vercelify-mesh 2>/dev/null || true
    run_cmd mkdir -p "$mesh"
    run_cmd rsync -a --delete "$t$mesh/" "$mesh/"
    restored_mesh=1
  fi
  # Each volume exactly as it was in the snapshot (no newer files mixed in).
  local failed=()
  for name in "${vols[@]}"; do
    run_cmd docker volume create "$name" >/dev/null || true
    restore_tree "$t$DOCKER_VOLUMES_DIR/$name/_data" "$DOCKER_VOLUMES_DIR/$name/_data" "$stamp" || failed+=("$name")
  done
  for name in "${hdd[@]}"; do
    restore_tree "$t$STORAGE_MOUNT/volumes/$name" "$STORAGE_MOUNT/volumes/$name" "$stamp" || failed+=("$name")
  done
  (( ${#writers[@]} )) && { run_cmd docker start "${writers[@]}" >/dev/null || true; }
  # The engine reconciles from the restored state: apps and databases start
  # (or redeploy) as they were when the backup was taken.
  run_cmd systemctl start vercelify-agent
  (( restored_mesh )) && { run_cmd systemctl restart vercelify-mesh 2>/dev/null || true; }
  (( ${#failed[@]} )) && die "these volumes could not be restored: ${failed[*]} (their previous data is unchanged or kept as *.before-restore-$stamp; files are in $t)"
  ok "instance restored — the engine is bringing apps and databases back as they were. Database dumps are in $t$BACKUP_STAGE/databases"
  (( ${#vols[@]} + ${#hdd[@]} )) && info "the data each volume had before is kept beside it as *.before-restore-$stamp; remove it once the apps check out"
  [[ -f "$t$tokens" ]] && info "device access is as it was at the backup: tokens revoked after it work again until revoked again (sudo vercelify tokens)"
  return 0
}

# A string setting of agent.toml, else DEFAULT.
agent_setting() { # KEY DEFAULT
  local f
  f=$(sed -n -E "s/^[[:space:]]*$1[[:space:]]*=[[:space:]]*\"([^\"]+)\".*/\\1/p" "${AGENT_CONFIG:-$VERCELIFY_ETC/agent.toml}" 2>/dev/null | head -1)
  printf '%s\n' "${f:-$2}"
}

# The engine's master key (agent.toml `master_key_file`, else beside platform.db).
master_key_file() { agent_setting master_key_file "$VERCELIFY_STATE/master.key"; }

backup_cli() {
  case "${1:-status}" in
    setup) shift; backup_setup "$@" ;;
    run) backup_run ;;
    list|snapshots) shift; backup_list "$@" ;;
    restore) shift; backup_restore "$@" ;;
    status)
      if [[ ! -r "$BACKUP_ENV_FILE" ]]; then info "backups are not set up — run: sudo vercelify backup setup"; return 0; fi
      # shellcheck disable=SC1090
      info "repositories: $(. "$BACKUP_ENV_FILE"; printf '%s' "$BACKUP_REPOS")"
      if [[ -r "$BACKUP_STATUS" ]]; then
        jq -r '"  last backup: \(.finished_at) — \(if .ok then "OK" else "FAILED: \(.error)" end)"' "$BACKUP_STATUS"
      else
        info "no backup has run yet"
      fi
      systemctl list-timers vercelify-backup.timer --no-pager 2>/dev/null | sed -n 2p || true
      ;;
    *) die "usage: vercelify backup [status|setup [--to REPO]…|run|list|restore [SNAPSHOT] [--target DIR|--instance [--all-volumes]] [--from REPO]]" ;;
  esac
}

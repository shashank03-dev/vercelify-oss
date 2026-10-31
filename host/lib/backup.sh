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

# vercelify backup setup [--to REPO]... [--s3-key ID --s3-secret SECRET]
backup_setup() {
  local repos=() key='' secret='' pw r
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
  local had_env=0
  [[ -e "$BACKUP_ENV_FILE" ]] && had_env=1
  if [[ -r "$BACKUP_ENV_FILE" ]]; then
    # shellcheck disable=SC1090
    pw=$(. "$BACKUP_ENV_FILE"; printf '%s' "${RESTIC_PASSWORD:-}")
    if (( ${#repos[@]} == 0 )); then
      # shellcheck disable=SC1090
      read -r -a repos <<<"$(. "$BACKUP_ENV_FILE"; printf '%s' "${BACKUP_REPOS:-}")"
    fi
  fi
  # Restoring onto a new machine: reuse the password of the existing backups.
  pw=${VERCELIFY_BACKUP_PASSWORD:-${pw:-$(random_hex 24)}}
  (( ${#repos[@]} )) || repos=("$(default_backup_repo)")
  for r in "${repos[@]}"; do
    [[ "$r" =~ ^[A-Za-z0-9:@/._~+=-]+$ ]] || die "invalid repository '$r'"
  done
  {
    echo "# Vercelify backups (sudo vercelify backup). Keep RESTIC_PASSWORD somewhere safe:"
    echo "# without it, nobody (you included) can restore these backups."
    backup_env_assignment RESTIC_PASSWORD "$pw"
    echo "BACKUP_REPOS=\"${repos[*]}\""
    echo "BACKUP_KEEP_DAILY=7"
    echo "BACKUP_KEEP_WEEKLY=4"
    echo "BACKUP_KEEP_MONTHLY=3"
    [[ -n "$key" ]] && backup_env_assignment AWS_ACCESS_KEY_ID "$key"
    [[ -n "$secret" ]] && backup_env_assignment AWS_SECRET_ACCESS_KEY "$secret"
  } | write_file "$BACKUP_ENV_FILE" 0600 || true
  backup_load_env
  for r in "${repos[@]}"; do
    [[ "$r" == /* ]] && run_cmd mkdir -p "$r"
    if RESTIC_REPOSITORY=$r "$RESTIC" cat config >/dev/null 2>&1; then
      ok "repository ready: $r"
    elif [[ -n "${VERCELIFY_BACKUP_PASSWORD:-}" ]] && [[ "$r" != /* || -e "$r/config" ]]; then
      die "cannot open $r with that password"
    elif [[ "$r" == /* && -e "$r/config" ]]; then
      # Backups from an earlier install whose password went with its config:
      # never re-create over them, say how to get them back.
      (( had_env )) || rm -f "$BACKUP_ENV_FILE"
      die "$r already holds backups locked with another password (from an earlier install?). Either put back the old $BACKUP_ENV_FILE, or give its password: sudo VERCELIFY_BACKUP_PASSWORD='…' vercelify backup setup — or start fresh, keeping the old ones aside: sudo mv $r $r.old && sudo vercelify backup setup"
    else
      run_cmd env RESTIC_REPOSITORY="$r" "$RESTIC" init >/dev/null || die "could not create the repository $r"
      ok "created repository: $r"
    fi
  done
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

# vercelify backup restore [SNAPSHOT] [--target DIR | --instance] [--from REPO]
backup_restore() {
  backup_load_env
  local snap=latest target='' instance=0 repo=${BACKUP_REPOS%% *}
  while (( $# )); do
    case "$1" in
      --target) target=${2:?}; shift ;;
      --instance) instance=1 ;;
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
  confirm "Replace this server's apps, settings, secrets and app data with the backup?" n || die "cancelled (files stay in $t)"
  # The engine stops first: it would put back what it last knew.
  run_cmd systemctl stop vercelify-agent
  # Volume files are rewritten under no running container: a database must
  # not see its files change under it.
  local running=()
  mapfile -t running < <(docker ps --format '{{.Names}}' | grep -vx -e vercelify-proxy -e vfy-registry || true)
  (( ${#running[@]} )) && { run_cmd docker stop "${running[@]}" >/dev/null || warn "some containers did not stop"; }
  run_cmd install -m 0600 "$t$(master_key_file)" "$(master_key_file)"
  run_cmd rm -f "$PLATFORM_DB-wal" "$PLATFORM_DB-shm"
  run_cmd install -m 0600 "$db" "$PLATFORM_DB"
  # The network's identity too (the coordinator's keys, this node's): workers
  # and devices keep their certificates and find the server again.
  local mesh=${MESH_DIR:-$VERCELIFY_STATE/mesh} restored_mesh=0
  if [[ -d "$t$mesh" ]]; then
    run_cmd systemctl stop vercelify-mesh 2>/dev/null || true
    run_cmd mkdir -p "$mesh"
    run_cmd rsync -a --delete "$t$mesh/" "$mesh/"
    restored_mesh=1
  fi
  [[ -d "$t$STORAGE_MOUNT/volumes" ]] && run_cmd rsync -aHAX --numeric-ids "$t$STORAGE_MOUNT/volumes/" "$STORAGE_MOUNT/volumes/"
  [[ -d "$t$DOCKER_VOLUMES_DIR" ]] && run_cmd rsync -aHAX --numeric-ids "$t$DOCKER_VOLUMES_DIR/" "$DOCKER_VOLUMES_DIR/"
  (( ${#running[@]} )) && { run_cmd docker start "${running[@]}" >/dev/null || true; }
  # The engine reconciles from the restored state: apps and databases start
  # (or redeploy) as they were when the backup was taken.
  run_cmd systemctl start vercelify-agent
  (( restored_mesh )) && { run_cmd systemctl restart vercelify-mesh 2>/dev/null || true; }
  ok "instance restored — the engine is bringing apps and databases back as they were. Database dumps are in $t$BACKUP_STAGE/databases"
}

# The engine's master key (agent.toml `master_key_file`, else beside platform.db).
master_key_file() {
  local f
  f=$(sed -n -E 's/^[[:space:]]*master_key_file[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "${AGENT_CONFIG:-$VERCELIFY_ETC/agent.toml}" 2>/dev/null | head -1)
  printf '%s\n' "${f:-$VERCELIFY_STATE/master.key}"
}

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
    *) die "usage: vercelify backup [status|setup [--to REPO]…|run|list|restore [SNAPSHOT] [--target DIR|--instance] [--from REPO]]" ;;
  esac
}

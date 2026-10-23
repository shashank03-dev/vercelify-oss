#!/usr/bin/env bash
# Backups with restic: encrypted, deduplicated, to the storage disk, another
# server in your pool (over Tailscale) and/or any S3-compatible bucket.
#
# What is saved: Coolify's database and .env (its APP_KEY decrypts every
# stored secret), SSH keys, /etc/vercelify, agent state, consistent dumps of
# every database container, and the data of app volumes on both tiers.

: "${VERCELIFY_STATE:=/var/lib/vercelify}"
: "${BACKUP_ENV_FILE:=$VERCELIFY_ETC/backup.env}"
: "${BACKUP_STAGE:=$VERCELIFY_STATE/backup-stage}"
: "${COOLIFY_DATA:=/data/coolify}"
: "${DOCKER_VOLUMES_DIR:=/var/lib/docker/volumes}"
: "${RESTIC:=restic}"
BACKUP_LOG="$VERCELIFY_STATE/backup.log"
BACKUP_STATUS="$VERCELIFY_STATE/backup.json"

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
    echo "RESTIC_PASSWORD=$pw"
    echo "BACKUP_REPOS=\"${repos[*]}\""
    echo "BACKUP_KEEP_DAILY=7"
    echo "BACKUP_KEEP_WEEKLY=4"
    echo "BACKUP_KEEP_MONTHLY=3"
    [[ -n "$key" ]] && echo "AWS_ACCESS_KEY_ID=$key"
    [[ -n "$secret" ]] && echo "AWS_SECRET_ACCESS_KEY=$secret"
  } | write_file "$BACKUP_ENV_FILE" 0600 || true
  backup_load_env
  for r in "${repos[@]}"; do
    [[ "$r" == /* ]] && run_cmd mkdir -p "$r"
    if RESTIC_REPOSITORY=$r "$RESTIC" cat config >/dev/null 2>&1; then
      ok "repository ready: $r"
    elif [[ -n "${VERCELIFY_BACKUP_PASSWORD:-}" ]] && [[ "$r" != /* || -e "$r/config" ]]; then
      die "cannot open $r with that password"
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
# (Coolify's secrets matter more than any one app's database). Prints the
# names that failed.
dump_databases() {
  local stage=$1 id name image n=0 failed=()
  mkdir -p "$stage/databases"
  if docker inspect "$COOLIFY_DB_CONTAINER" >/dev/null 2>&1; then
    if docker exec "$COOLIFY_DB_CONTAINER" pg_dump -U coolify -Fc coolify >"$stage/databases/coolify.dump"; then
      n=$((n + 1))
    else
      err "dumping Coolify's database failed"
      failed+=(coolify)
    fi
  fi
  while read -r id name image; do
    [[ -n "$id" && "$name" != "$COOLIFY_DB_CONTAINER" ]] || continue
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
        docker exec "$id" sh -c 'redis-cli ${REDIS_PASSWORD:+-a "$REDIS_PASSWORD"} SAVE' >/dev/null 2>&1 || true
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
  for p in "$COOLIFY_DATA/source/.env" "$COOLIFY_DATA/ssh" "$VERCELIFY_ETC" "$VERCELIFY_STATE" \
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
        --exclude "$VERCELIFY_STATE/backup.log" "${paths[@]}" 2>&1); then
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
  [[ $ROLE == main ]] || die "--instance restores a main server (Coolify); on a worker copy what you need from $target"
  confirm "Replace this server's Coolify database, keys and app data with the backup?" n || die "cancelled (files stay in $target)"
  local t=$target
  run_cmd docker stop coolify >/dev/null 2>&1 || true
  if [[ -f "$t$COOLIFY_DATA/source/.env" ]]; then
    # APP_KEY must match the database or every stored secret is unreadable.
    run_cmd cp -a "$t$COOLIFY_DATA/source/.env" "$COOLIFY_DATA/source/.env"
  fi
  [[ -d "$t$COOLIFY_DATA/ssh" ]] && run_cmd rsync -a "$t$COOLIFY_DATA/ssh/" "$COOLIFY_DATA/ssh/"
  if [[ -f "$t$BACKUP_STAGE/databases/coolify.dump" ]]; then
    docker exec -i "$COOLIFY_DB_CONTAINER" pg_restore -U coolify -d coolify --clean --if-exists --no-owner \
      <"$t$BACKUP_STAGE/databases/coolify.dump" || warn "pg_restore reported problems (often harmless notices)"
  fi
  # Volume files are rewritten under no running container: a database must
  # not see its files change under it. Coolify's own database was just
  # restored from its dump; its volume's file copy (taken while it ran) is
  # left out, or it would undo that.
  local running=() keep=() v
  mapfile -t running < <(docker ps --format '{{.Names}}' | grep -vx -e "$COOLIFY_DB_CONTAINER" -e coolify || true)
  mapfile -t keep < <(docker inspect -f '{{range .Mounts}}{{if eq .Type "volume"}}{{.Name}}{{"\n"}}{{end}}{{end}}' "$COOLIFY_DB_CONTAINER" 2>/dev/null | sed '/^$/d')
  (( ${#running[@]} )) && { run_cmd docker stop "${running[@]}" >/dev/null || warn "some containers did not stop"; }
  [[ -d "$t$STORAGE_MOUNT/volumes" ]] && run_cmd rsync -aHAX --numeric-ids "$t$STORAGE_MOUNT/volumes/" "$STORAGE_MOUNT/volumes/"
  if [[ -d "$t$DOCKER_VOLUMES_DIR" ]]; then
    local excludes=()
    for v in "${keep[@]}"; do excludes+=(--exclude "/$v/"); done
    run_cmd rsync -aHAX --numeric-ids "${excludes[@]}" "$t$DOCKER_VOLUMES_DIR/" "$DOCKER_VOLUMES_DIR/"
  fi
  (( ${#running[@]} )) && { run_cmd docker start "${running[@]}" >/dev/null || warn "some containers did not start again — start them in Coolify"; }
  run_cmd docker start coolify >/dev/null
  coolify_wait_healthy 600 || warn "Coolify is slow to come back — check: docker logs coolify"
  ok "instance restored. Start (or redeploy) your apps and databases in Coolify; database dumps are in $t$BACKUP_STAGE/databases"
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

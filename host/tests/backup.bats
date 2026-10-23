#!/usr/bin/env bats
# `vercelify backup` with stub restic, docker and systemctl.

setup() {
  T=$(mktemp -d)
  mkdir -p "$T/bin" "$T/etc" "$T/state" "$T/systemd" "$T/coolify/source" "$T/coolify/ssh" "$T/volumes" "$T/storage/volumes/app"
  echo 'APP_KEY=base64:secret' >"$T/coolify/source/.env"
  printf 'role=main\nversion=test\n' >"$T/etc/install.env"
  cat >"$T/bin/restic" <<'STUB'
#!/usr/bin/env bash
echo "restic $* repo=$RESTIC_REPOSITORY pw=${RESTIC_PASSWORD:+set}" >>"$CALLS"
case "$1" in
  cat) [[ -e "$RESTIC_REPOSITORY/config" ]] ;;
  init) mkdir -p "$RESTIC_REPOSITORY" && touch "$RESTIC_REPOSITORY/config" ;;
  backup) [[ "$RESTIC_REPOSITORY" == *broken* ]] && { echo "Fatal: unable to open repository" >&2; exit 1; }
          echo "snapshot 1a2b3c4d saved" ;;
  restore) mkdir -p "$4" ;;
  *) exit 0 ;;
esac
STUB
  cat >"$T/bin/docker" <<'STUB'
#!/usr/bin/env bash
echo "docker $*" >>"$CALLS"
case "$1" in
  inspect) [[ "$*" == *Mounts* ]] && printf 'coolify-db\n\n'; exit 0 ;;
  ps) if [[ "$*" == *'{{.Names}}' ]]; then printf 'coolify\ncoolify-db\npg-main\nweb\n'
      else printf 'a1 pg-main postgres:16\nb2 web node:22\nc3 cache redis:7\nd4 maria mariadb:11\n'; fi ;;
  exec) if [[ "$*" == *psql* ]]; then echo 1; else echo "-- dump of $2"; fi ;;
esac
STUB
  printf '#!/usr/bin/env bash\necho "systemctl $*" >>"$CALLS"\n' >"$T/bin/systemctl"
  printf '#!/usr/bin/env bash\necho "rsync $*" >>"$CALLS"\n' >"$T/bin/rsync"
  chmod +x "$T/bin/"*
  export CALLS="$T/calls" PATH="$T/bin:$PATH" NO_COLOR=1 VERCELIFY_TEST=1
  export VERCELIFY_ETC="$T/etc" VERCELIFY_STATE="$T/state" VERCELIFY_LOG="$T/setup.log" SYSTEMD_DIR="$T/systemd"
  export COOLIFY_DATA="$T/coolify" DOCKER_VOLUMES_DIR="$T/volumes" STORAGE_MOUNT="$T/storage"
  : >"$CALLS"
  CLI="$BATS_TEST_DIRNAME/../vercelify"
}

teardown() { rm -rf "$T"; }

@test "setup creates encrypted repositories, a private env file and a nightly timer" {
  run "$CLI" backup setup --to "$T/repo-a" --to "$T/repo-b"
  [ "$status" -eq 0 ]
  [[ "$output" == *"Backup password"* ]]
  [ "$(stat -c %a "$T/etc/backup.env")" = 600 ]
  grep -q "BACKUP_REPOS=\"$T/repo-a $T/repo-b\"" "$T/etc/backup.env"
  grep -q "restic init repo=$T/repo-a pw=set" "$CALLS"
  grep -q 'OnCalendar=' "$T/systemd/vercelify-backup.timer"
  grep -q 'IOSchedulingClass=idle' "$T/systemd/vercelify-backup.service"
  grep -q 'systemctl enable --now vercelify-backup.timer' "$CALLS"
  # the password never appears on a command line
  pw=$(sed -n 's/^RESTIC_PASSWORD=//p' "$T/etc/backup.env")
  [ "${#pw}" -ge 40 ]
  ! grep -q "$pw" "$CALLS"
  # re-running keeps the password and repositories
  "$CLI" backup setup >/dev/null 2>&1
  grep -q "RESTIC_PASSWORD=$pw" "$T/etc/backup.env"
  grep -q "repo-b" "$T/etc/backup.env"
}

@test "run dumps every database, saves Coolify's secrets and records success" {
  "$CLI" backup setup --to "$T/repo" >/dev/null 2>&1
  : >"$CALLS"
  run "$CLI" backup run
  [ "$status" -eq 0 ]
  grep -q 'docker exec coolify-db pg_dump -U coolify -Fc coolify' "$CALLS"
  grep -q 'docker exec a1 sh -c pg_dumpall' "$CALLS"
  grep -q 'docker exec d4 sh -c exec mariadb-dump' "$CALLS"
  grep -q 'docker exec c3 sh -c redis-cli' "$CALLS"
  ! grep -q 'docker exec b2' "$CALLS"   # not a database
  line=$(grep '^restic backup' "$CALLS")
  [[ "$line" == *"$T/coolify/source/.env"* && "$line" == *"$T/coolify/ssh"* && "$line" == *"$T/etc"* ]]
  [[ "$line" == *"$T/storage/volumes"* && "$line" == *"--exclude $T/volumes/ollama-models"* ]]
  grep -q '^restic forget --tag vercelify .*--prune --keep-daily 7' "$CALLS"
  [ "$(jq -r .ok "$T/state/backup.json")" = true ]
  [ "$(jq -r .snapshot "$T/state/backup.json")" = 1a2b3c4d ]
  [ ! -e "$T/state/backup-stage" ]   # dumps don't linger on disk
}

@test "a failing repository is reported (doctor and alerts read backup.json)" {
  "$CLI" backup setup --to "$T/repo" >/dev/null 2>&1
  sed -i "s#^BACKUP_REPOS=.*#BACKUP_REPOS=\"$T/repo $T/broken\"#" "$T/etc/backup.env"
  run "$CLI" backup run
  [ "$status" -ne 0 ]
  [ "$(jq -r .ok "$T/state/backup.json")" = false ]
  [[ "$(jq -r .error "$T/state/backup.json")" == *"$T/broken"* ]]
}

@test "restore to a directory, never over the live server without --instance" {
  "$CLI" backup setup --to "$T/repo" >/dev/null 2>&1
  run "$CLI" backup restore 1a2b3c4d --target "$T/out"
  [ "$status" -eq 0 ]
  grep -q "restic restore 1a2b3c4d --target $T/out" "$CALLS"
  [[ "$output" == *"--instance"* ]]
  ! grep -q 'docker stop coolify' "$CALLS"
  run "$CLI" backup restore '../x'
  [ "$status" -ne 0 ]
}

@test "--instance: no database sees its files change under it" {
  "$CLI" backup setup --to "$T/repo" >/dev/null 2>&1
  : >"$CALLS"
  printf '#!/usr/bin/env bash\necho OK\n' >"$T/bin/curl"; chmod +x "$T/bin/curl"
  mkdir -p "$T/out$T/volumes/coolify-db" "$T/out$T/volumes/pg-data" "$T/out$T/storage/volumes"
  ASSUME_YES=1 run "$CLI" backup restore latest --instance --target "$T/out"
  [ "$status" -eq 0 ]
  calls=$(grep -E '^(docker (stop|start)|rsync)' "$CALLS")
  # the apps' containers stop before their volumes are rewritten, and start after
  [[ "$calls" == *"docker stop pg-main web"*"rsync"*"$T/volumes/"*"docker start pg-main web"*"docker start coolify"* ]]
  # Coolify's database comes from its dump, not from a file copy taken while it ran
  grep -E "^rsync .*--exclude /coolify-db/ .*$T/volumes/" "$CALLS"
  ! grep -E '^docker stop .*coolify-db' "$CALLS"
}

@test "not set up: clear message" {
  run "$CLI" backup run
  [ "$status" -ne 0 ]
  [[ "$output" == *"sudo vercelify backup setup"* ]]
}

@test "a new machine reuses the old password to reach existing backups" {
  mkdir -p "$T/old" && touch "$T/old/config"
  VERCELIFY_BACKUP_PASSWORD=kept-secret run "$CLI" backup setup --to "$T/old"
  [ "$status" -eq 0 ]
  grep -q '^RESTIC_PASSWORD=kept-secret$' "$T/etc/backup.env"
  ! grep -q 'restic init' "$CALLS"
}

@test "a database that can't be dumped doesn't stop Coolify's secrets from being saved" {
  "$CLI" backup setup --to "$T/repo" >/dev/null 2>&1
  cat >"$T/bin/docker" <<'STUB'
#!/usr/bin/env bash
echo "docker $*" >>"$CALLS"
case "$1" in
  inspect) exit 0 ;;
  ps) printf 'a1 pg-main postgres:16\n' ;;
  exec) [ "$2" = a1 ] && exit 1; echo dump ;;
esac
STUB
  run "$CLI" backup run
  [ "$status" -ne 0 ]
  grep -q '^restic backup' "$CALLS"          # still saved
  [ "$(jq -r .ok "$T/state/backup.json")" = false ]
  [[ "$(jq -r .error "$T/state/backup.json")" == *"pg-main"* ]]
  [ "$(jq -r .snapshot "$T/state/backup.json")" = 1a2b3c4d ]
}

#!/usr/bin/env bash
# Coolify v4: install, wait for health, and apply Vercelify defaults.
# Database tweaks are applied through `docker exec coolify-db psql` because
# the Coolify API has no endpoint for them; every statement checks that the
# column exists first, so a future schema change degrades to a warning.

: "${COOLIFY_URL_LOCAL:=http://127.0.0.1:8000}"
: "${COOLIFY_DB_CONTAINER:=coolify-db}"
: "${COOLIFY_CONTAINER:=coolify}"
# The Coolify release this Vercelify version was tested with (CI runs the
# contract test against it). Fresh installs and `vercelify coolify upgrade`
# use it; VERCELIFY_COOLIFY_VERSION=latest (or another version) overrides.
COOLIFY_TESTED_VERSION=4.4.6

# The version to install: the tested one unless overridden; empty = latest.
coolify_target_version() {
  local v=${VERCELIFY_COOLIFY_VERSION:-$COOLIFY_TESTED_VERSION}
  [[ "$v" == latest ]] && v=''
  [[ -z "$v" || "$v" =~ ^v?[0-9]+(\.[0-9]+)*(-[0-9A-Za-z.]+)?$ ]] || die "invalid Coolify version '$v'"
  printf '%s' "${v#v}"
}

# The running version, from the coolify container's image tag.
coolify_current_version() {
  local img
  img=$(docker inspect -f '{{.Config.Image}}' "$COOLIFY_CONTAINER" 2>/dev/null) || return 1
  printf '%s' "${img##*:}"
}

coolify_installed() { [[ -f /data/coolify/source/.env ]] && docker inspect "$COOLIFY_CONTAINER" >/dev/null 2>&1; }

coolify_psql() {
  docker exec "$COOLIFY_DB_CONTAINER" psql -U coolify -d coolify -v ON_ERROR_STOP=1 -At -c "$1"
}

coolify_has_column() { # TABLE COLUMN
  [[ "$(coolify_psql "select count(*) from information_schema.columns where table_name='$1' and column_name='$2'" 2>/dev/null)" == 1 ]]
}

coolify_wait_healthy() {
  local timeout=${1:-300} start=$SECONDS
  info "waiting for Coolify to become healthy (first boot takes a few minutes)…"
  until [[ "$(curl -fsS -m 5 "$COOLIFY_URL_LOCAL/api/health" 2>/dev/null)" == OK ]]; do
    (( SECONDS - start > timeout )) && return 1
    sleep 5
  done
  # The seeders create the localhost server row shortly after the API is up.
  start=$SECONDS
  until [[ "$(coolify_psql 'select count(*) from server_settings where server_id=0' 2>/dev/null)" == 1 ]]; do
    (( SECONDS - start > 120 )) && return 1
    sleep 3
  done
}

# install_coolify [ADMIN_EMAIL ADMIN_PASSWORD]
install_coolify() {
  step "Coolify (PaaS engine)"
  if coolify_installed; then
    ok "Coolify already installed ($(coolify_current_version || echo v4))"
  else
    local version tmp
    version=$(coolify_target_version)
    [[ "$VERCELIFY_DRY_RUN" == 1 ]] && { info "[dry-run] would install Coolify ${version:-latest}"; return 0; }
    tmp=$(mktemp)
    fetch https://cdn.coollabs.io/coolify/install.sh "$tmp" || die "could not download the Coolify installer"
    info "running the official Coolify installer for ${version:-the latest} release (pulls ~1.5 GB of images)…"
    # The installer names the SSH user after $USER; it is empty under some
    # sudo/su/systemd setups, which silently breaks every deployment.
    local -a env=(-u DRY_RUN USER=root HOME=/root)
    [[ -n "${1:-}" ]] && env+=(ROOT_USERNAME=admin "ROOT_USER_EMAIL=$1" "ROOT_USER_PASSWORD=$2")
    if ! env "${env[@]}" bash "$tmp" ${version:+"$version"} >>"$VERCELIFY_LOG" 2>&1; then
      rm -f "$tmp"
      # The installer's own health wait is short on slow disks; give it longer.
      coolify_wait_healthy 600 || die "Coolify installation failed — see $VERCELIFY_LOG and 'docker logs coolify'"
    fi
    rm -f "$tmp"
  fi
  coolify_wait_healthy 600 || die "Coolify did not become healthy — see 'docker logs coolify'"
  ok "Coolify is up on port 8000"
  if [[ "$VERCELIFY_DRY_RUN" != 1 && -n "${1:-}" ]] && ! coolify_wait_root_user 60; then
    coolify_create_root_user "$1" "$2" && ok "admin account created ($1)"
  fi
  if [[ "$VERCELIFY_DRY_RUN" != 1 ]] && ! coolify_has_root_user; then
    warn "Coolify has NO admin account yet: open the dashboard now and register — the first person to register becomes admin"
  fi
}

# configure_coolify WILDCARD_URL — WILDCARD_URL like http://example.com
configure_coolify() {
  local wildcard=$1
  step "Coolify defaults"
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] && { info "[dry-run] would set wildcard=$wildcard"; return 0; }

  # Guard against the empty-$USER install bug on existing installs too.
  if coolify_has_column servers user; then
    coolify_psql "update servers set \"user\"='root' where id=0 and coalesce(\"user\",'')=''" >/dev/null && true
  fi

  if [[ -z "$wildcard" ]]; then
    warn "wildcard domain not set yet (Tailscale not connected) — later run: sudo vercelify domain"
  elif coolify_has_column server_settings wildcard_domain; then
    coolify_psql "update server_settings set wildcard_domain='${wildcard//\'/\'\'}' where server_id=0" >/dev/null
    ok "wildcard domain → $wildcard (new apps get <name>.${wildcard#*://})"
  else
    warn "could not set the wildcard domain automatically — set it in Coolify → Servers → localhost"
  fi

  # Previews as pr-42-app.example.com: one DNS label deep, so the wildcard
  # record and Cloudflare's free certificate both cover them.
  if coolify_has_column applications preview_url_template; then
    coolify_psql "alter table applications alter column preview_url_template set default 'pr-{{pr_id}}-{{domain}}'" >/dev/null
    local base="<your domain>"
    [[ -n "$wildcard" ]] && base=${wildcard#*://}
    ok "preview URLs → pr-<number>-<app>.$base"
  fi

  # 8 GB RAM: one build at a time keeps the running apps responsive.
  if coolify_has_column server_settings concurrent_builds; then
    coolify_psql "update server_settings set concurrent_builds=1 where server_id=0 and concurrent_builds>1" >/dev/null
  fi

  if coolify_has_column instance_settings is_api_enabled; then
    coolify_psql "update instance_settings set is_api_enabled=true" >/dev/null
  fi

  # Vercelify reads and writes a few of Coolify's tables. A Coolify release
  # that changes them must not arrive unattended: upgrade Coolify on purpose
  # (sudo vercelify coolify upgrade), which checks the schema afterwards.
  if coolify_has_column instance_settings is_auto_update_enabled; then
    coolify_psql "update instance_settings set is_auto_update_enabled=false" >/dev/null
    ok "Coolify auto-update off (upgrade with: sudo vercelify coolify upgrade)"
  fi

  # Instant rollback needs the previous images; keep a few per app.
  if coolify_has_column application_settings docker_images_to_keep; then
    coolify_psql "alter table application_settings alter column docker_images_to_keep set default 5" >/dev/null
    coolify_psql "update application_settings set docker_images_to_keep=5 where docker_images_to_keep < 5" >/dev/null
    ok "the last 5 images of every app are kept for instant rollback"
  fi
  coolify_schema_check || true
}

# Tables and columns Vercelify relies on. Keep in sync with
# agent/src/coolify.rs (REQUIRED_COLUMNS).
COOLIFY_REQUIRED_COLUMNS="servers.user servers.ip servers.private_key_id private_keys.uuid
server_settings.wildcard_domain server_settings.server_id
local_persistent_volumes.host_path local_persistent_volumes.name local_persistent_volumes.resource_type local_persistent_volumes.resource_id
applications.uuid applications.fqdn applications.destination_id applications.watch_paths applications.base_directory
standalone_dockers.server_id"

# Prints the missing ones; fails when any is missing.
coolify_schema_check() {
  local missing=() c
  for c in $COOLIFY_REQUIRED_COLUMNS; do
    coolify_has_column "${c%%.*}" "${c#*.}" || missing+=("$c")
  done
  if (( ${#missing[@]} )); then
    warn "this Coolify version lacks columns Vercelify uses: ${missing[*]} — storage moves/workers may not work; check for a Vercelify update"
    return 1
  fi
  ok "Coolify schema compatible"
}

# Upgrade Coolify on purpose (auto-update is off), then re-check. Without a
# version: the one this Vercelify release was tested with ("latest" = newest).
coolify_upgrade() {
  local version tmp current
  version=$(VERCELIFY_COOLIFY_VERSION=${1:-${VERCELIFY_COOLIFY_VERSION:-}} coolify_target_version)
  current=$(coolify_current_version || true)
  if [[ -n "$version" && "$current" == "$version" ]]; then
    ok "Coolify is already at $version"
    return 0
  fi
  tmp=$(mktemp)
  fetch https://cdn.coollabs.io/coolify/upgrade.sh "$tmp" || die "could not download Coolify's upgrade script"
  info "upgrading Coolify${version:+ to $version} (apps keep running)…"
  env -u DRY_RUN bash "$tmp" ${version:+"$version"} >>"$VERCELIFY_LOG" 2>&1 || { rm -f "$tmp"; die "Coolify upgrade failed — see $VERCELIFY_LOG"; }
  rm -f "$tmp"
  coolify_wait_healthy 900 || die "Coolify did not come back healthy — see 'docker logs coolify'"
  configure_coolify "$(coolify_psql 'select wildcard_domain from server_settings where server_id=0' 2>/dev/null || true)"
}

# Coolify reports healthy before its seeders create the admin account from
# .env (a few seconds to a minute on a fast machine). Wait for it.
coolify_wait_root_user() {
  local timeout=${1:-120} start=$SECONDS
  until coolify_has_root_user; do
    (( SECONDS - start > timeout )) && return 1
    sleep 3
  done
}

# Coolify creates the admin account from .env only if the e-mail's domain
# passes a DNS check, which some networks fail even for real domains (and the
# installer's admin@example.com fallback). Then create it the way Coolify's
# RootUserSeeder does: user 0, owner of team 0, registration closed.
# The password reaches PHP through the environment, never the command line.
coolify_create_root_user() { # EMAIL PASSWORD
  coolify_has_root_user && return 0
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] && return 0
  VFY_ROOT_EMAIL=$1 VFY_ROOT_PASSWORD=$2 docker exec -e VFY_ROOT_EMAIL -e VFY_ROOT_PASSWORD "$COOLIFY_CONTAINER" php artisan tinker --execute='
    $u = (new \App\Models\User)->forceFill([
      "id" => 0, "name" => "admin", "email" => getenv("VFY_ROOT_EMAIL"),
      "password" => \Illuminate\Support\Facades\Hash::make(getenv("VFY_ROOT_PASSWORD")),
    ]);
    $u->save();
    $team = \App\Models\Team::find(0);
    if ($team && ! $u->teams()->where("team_id", 0)->exists()) { $u->teams()->attach($team, ["role" => "owner"]); }
    \App\Models\InstanceSettings::updateOrCreate(["id" => 0], ["is_registration_enabled" => false]);
  ' >>"$VERCELIFY_LOG" 2>&1 || true
  coolify_has_root_user
}

coolify_has_root_user() {
  [[ "$(coolify_psql 'select count(*) from users where id=0' 2>/dev/null)" == 1 ]]
}

# Coolify validates the admin e-mail with a DNS lookup of its domain
# (`email:rfc,dns`); an address like admin@laptop.local is silently rejected
# and no admin account is created. Prints a usable address.
coolify_safe_email() {
  local email=$1 domain=${1#*@}
  if [[ "$email" == *@*.* ]] && getent ahosts "$domain" >/dev/null 2>&1; then
    echo "$email"
  else
    warn "Coolify only accepts login e-mails whose domain exists in DNS — using admin@example.com (change it later in Coolify → Profile)"
    echo "admin@example.com"
  fi
}

# Create (once) an API token for the cockpit and store it.
coolify_api_token() {
  local file="$VERCELIFY_ETC/coolify.token" token
  if [[ -s "$file" ]]; then
    if curl -fsS -m 5 -H "Authorization: Bearer $(<"$file")" "$COOLIFY_URL_LOCAL/api/v1/version" >/dev/null 2>&1; then
      ok "Coolify API token present"
      return 0
    fi
    warn "stored Coolify API token no longer works — creating a new one"
  fi
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] && return 0
  coolify_wait_root_user 60 || true
  # Tokens belong to a user and a team; user 0 / team 0 is the root account.
  token=$(docker exec "$COOLIFY_CONTAINER" php artisan tinker --execute="
    \$u = \App\Models\User::find(0);
    if (! \$u) { fwrite(STDERR, 'no root user'); exit(1); }
    session(['currentTeam' => \$u->teams()->first() ?? \App\Models\Team::find(0)]);
    echo 'TOKEN=' . \$u->createToken('vercelify-cockpit', ['read', 'read:sensitive', 'write', 'deploy'])->plainTextToken;
  " 2>/dev/null | sed -n 's/^TOKEN=//p' | tail -1 || true)
  if [[ -z "$token" ]]; then
    warn "could not create a Coolify API token (no admin account?) — register in the dashboard, then: sudo vercelify pair --refresh"
    return 0
  fi
  printf '%s\n' "$token" | write_file "$file" 0600 || true
  ok "created Coolify API token for the cockpit"
}

# Coolify's SSH public key for the localhost server (used to authorise
# worker nodes). Coolify stores keys as /data/coolify/ssh/keys/ssh_key@<uuid>.
coolify_public_key() {
  local uuid key
  uuid=$(coolify_psql "select pk.uuid from private_keys pk join servers s on s.private_key_id = pk.id where s.id = 0" 2>/dev/null)
  for key in "/data/coolify/ssh/keys/ssh_key@$uuid" /data/coolify/ssh/keys/id.*@host.docker.internal; do
    [[ -n "$uuid" || "$key" != */ssh_key@ ]] || continue
    [[ -r "$key" ]] && ssh-keygen -y -f "$key" 2>/dev/null && return 0
  done
  # The installer also appends the key to root's authorized_keys.
  grep -m1 -E ' coolify$' /root/.ssh/authorized_keys 2>/dev/null
}

# UUID of Coolify's localhost private key (for registering workers via API).
coolify_private_key_uuid() {
  coolify_psql "select pk.uuid from private_keys pk join servers s on s.private_key_id = pk.id where s.id = 0" 2>/dev/null
}

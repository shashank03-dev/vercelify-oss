#!/usr/bin/env bash
# A real Coolify for the migration's contract test (host/tests/contract/
# migrate.sh): started from Coolify's own compose files for a version, the
# way its installer does (no SSH, no proxy start), with its admin account
# and an API token. Sourced; needs root, Docker, curl, jq, openssl and
# ssh-keygen.
#
#   coolify_start VERSION      fresh /data/coolify, up and healthy; sets CTOKEN
#   coolify_stop               down, its volumes and /data/coolify removed
#   cpsql SQL                  a query against Coolify's database
#   capi METHOD PATH [JSON]    Coolify's API with CTOKEN
#   coolify_proxy_start        Coolify's own Traefik (coolify-proxy) on :80/:443

: "${COOLIFY_URL:=http://127.0.0.1:8000}"

cpsql() { docker exec coolify-db psql -U coolify -d coolify -v ON_ERROR_STOP=1 -At -c "$1"; }

capi() {
  curl -fsS -m 60 -H "Authorization: Bearer $CTOKEN" -H 'Content-Type: application/json' -X "$1" ${3:+-d "$3"} \
    "$COOLIFY_URL/api/v1$2"
}

# Docker Hub limits anonymous pulls; Google's mirror of it doesn't.
pull_as() { # IMAGE (a Docker Hub name)
  docker image inspect "$1" >/dev/null 2>&1 && return 0
  local m=$1
  [[ "$m" == */* ]] || m="library/$m"
  docker pull -q "mirror.gcr.io/$m" >/dev/null && docker tag "mirror.gcr.io/$m" "$1" || docker pull -q "$1" >/dev/null
}

coolify_stop() {
  (cd /data/coolify/source 2>/dev/null && docker compose --env-file .env -f docker-compose.yml -f docker-compose.prod.yml down -v >/dev/null 2>&1) || true
  docker compose -f /data/coolify/proxy/docker-compose.yml down >/dev/null 2>&1 || true
  docker rm -f coolify coolify-db coolify-redis coolify-realtime coolify-proxy coolify-sentinel >/dev/null 2>&1 || true
  rm -rf /data/coolify
}

coolify_start() { # VERSION
  local version=$1 f env_file
  coolify_stop
  mkdir -p /data/coolify/{source,ssh/keys,ssh/mux,applications,databases,backups,services,proxy/dynamic,images}
  for f in docker-compose.yml docker-compose.prod.yml .env.production; do
    curl -fsSL "https://raw.githubusercontent.com/coollabsio/coolify/v$version/$f" -o "/data/coolify/source/$f"
  done
  env_file=/data/coolify/source/.env
  cp /data/coolify/source/.env.production "$env_file"
  set_env() { sed -i "s|^$1=.*|$1=$2|" "$env_file"; }
  set_env APP_ID "$(openssl rand -hex 16)"
  set_env APP_KEY "base64:$(openssl rand -base64 32)"
  set_env DB_PASSWORD "$(openssl rand -hex 24)"
  set_env REDIS_PASSWORD "$(openssl rand -hex 24)"
  set_env PUSHER_APP_ID "$(openssl rand -hex 32)"
  set_env PUSHER_APP_KEY "$(openssl rand -hex 32)"
  set_env PUSHER_APP_SECRET "$(openssl rand -hex 32)"
  set_env ROOT_USERNAME admin
  set_env ROOT_USER_EMAIL admin@example.com
  set_env ROOT_USER_PASSWORD "Contract-$(openssl rand -hex 8)"
  set_env REGISTRY_URL ghcr.io
  ssh-keygen -q -t ed25519 -N '' -C root@coolify -f /data/coolify/ssh/keys/id.root@host.docker.internal
  chown -R 9999:root /data/coolify/source /data/coolify/ssh /data/coolify/images
  chmod -R 700 /data/coolify/source /data/coolify/ssh /data/coolify/images
  docker network inspect coolify >/dev/null 2>&1 || docker network create --attachable coolify >/dev/null
  for f in postgres:15-alpine redis:7-alpine busybox:1.37; do pull_as "$f"; done
  (cd /data/coolify/source && LATEST_IMAGE=$version docker compose --env-file .env -f docker-compose.yml -f docker-compose.prod.yml up -d --quiet-pull) >/dev/null
  # Kernels without IPv6 (CI sandboxes) can't open Coolify's [::] listeners.
  if [[ ! -e /proc/net/if_inet6 ]]; then
    docker exec -u root coolify sh -c 'for f in /etc/nginx/site-opts.d/*.conf* /etc/nginx/sites-available/*; do sed -i "/listen \[::\]/d" "$f"; done' 2>/dev/null || true
  fi
  local end=$((SECONDS + 600))
  until [[ "$(curl -fsS -m 5 "$COOLIFY_URL/api/health" 2>/dev/null)" == OK ]]; do
    ((SECONDS < end)) || { docker logs --tail 50 coolify; return 1; }
    sleep 5
  done
  until [[ "$(cpsql 'select count(*) from server_settings where server_id=0' 2>/dev/null)" == 1 ]]; do
    ((SECONDS < end)) || return 1
    sleep 3
  done
  cpsql "update instance_settings set is_api_enabled=true, is_auto_update_enabled=false" >/dev/null
  until [[ "$(cpsql 'select count(*) from users where id=0' 2>/dev/null)" == 1 ]]; do
    ((SECONDS < end)) || return 1
    sleep 3
  done
  CTOKEN=$(docker exec coolify php artisan tinker --execute="
    \$u = \App\Models\User::find(0);
    session(['currentTeam' => \$u->teams()->first() ?? \App\Models\Team::find(0)]);
    echo 'TOKEN=' . \$u->createToken('contract', ['read', 'read:sensitive', 'write', 'deploy'])->plainTextToken;
  " 2>/dev/null | sed -n 's/^TOKEN=//p' | tail -1)
  [[ -n "$CTOKEN" ]]
}

# Coolify writes its proxy's compose file over SSH when it first starts it
# (skipped here): have it generate the configuration, then start it.
coolify_proxy_start() {
  docker exec coolify php artisan tinker --execute 'generateDefaultProxyConfiguration(App\Models\Server::find(0));' >/dev/null 2>&1 || true
  cpsql "select proxy::jsonb->>'last_saved_proxy_configuration' from servers where id=0" >/data/coolify/proxy/docker-compose.yml
  local img
  img=$(grep -m1 -oE "traefik:[^'\" ]+" /data/coolify/proxy/docker-compose.yml || true)
  [[ -z "$img" ]] || pull_as "$img"
  docker compose -f /data/coolify/proxy/docker-compose.yml up -d --quiet-pull >/dev/null
}

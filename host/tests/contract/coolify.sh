#!/usr/bin/env bash
# Contract test against a real Coolify: everything Vercelify reads or writes
# in Coolify (database columns, API routes, the proxy configuration) must exist
# in the version the installer pins (COOLIFY_TESTED_VERSION).
#
#   sudo host/tests/contract/coolify.sh            # needs Docker; uses /data/coolify
#   COOLIFY_VERSION=4.5.0 sudo host/tests/contract/coolify.sh   # try a newer one
#
# Starts Coolify's own compose files for that version (no SSH, no proxy
# start), then checks: the schema (coolify_schema_check), the API routes the
# cockpit, `vfy` and the agent call, the agent's SQL against the real
# database, and the proxy edit that turns on request counters for sleeping
# apps. KEEP=1 leaves it running.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
export VERCELIFY_SRC=$repo NO_COLOR=1 VERCELIFY_LOG=/tmp/vercelify-contract.log
# shellcheck source=../../lib/common.sh
. "$repo/host/lib/common.sh"
# shellcheck source=../../lib/coolify.sh
. "$repo/host/lib/coolify.sh"
version=${COOLIFY_VERSION:-$COOLIFY_TESTED_VERSION}
work=$(mktemp -d)
export VERCELIFY_ETC="$work/etc"
mkdir -p "$VERCELIFY_ETC"

cleanup() {
  [[ "${KEEP:-0}" == 1 ]] && { echo "left running (Coolify $version on :8000)"; return; }
  [[ -n "${agent_pid:-}" ]] && kill "$agent_pid" 2>/dev/null || true
  (cd /data/coolify/source && docker compose --env-file .env -f docker-compose.yml -f docker-compose.prod.yml down -v >/dev/null 2>&1) || true
  docker compose -f /data/coolify/proxy/docker-compose.yml down >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

echo "==> Coolify $version"
mkdir -p /data/coolify/{source,ssh/keys,ssh/mux,applications,databases,backups,services,proxy/dynamic,images}
for f in docker-compose.yml docker-compose.prod.yml .env.production; do
  curl -fsSL "https://raw.githubusercontent.com/coollabsio/coolify/v$version/$f" -o "/data/coolify/source/$f"
done
env_file=/data/coolify/source/.env
if [[ ! -f "$env_file" ]]; then
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
  # COOLIFY_CONTRACT_EMAIL=admin@nowhere.invalid exercises the installer's
  # fallback for an e-mail Coolify's DNS check rejects.
  set_env ROOT_USER_EMAIL "${COOLIFY_CONTRACT_EMAIL:-admin@example.com}"
  set_env ROOT_USER_PASSWORD "Contract-$(openssl rand -hex 8)"
  # ghcr.io: no Docker Hub pull limits in CI.
  set_env REGISTRY_URL "${COOLIFY_REGISTRY:-ghcr.io}"
fi
[[ -f /data/coolify/ssh/keys/id.root@host.docker.internal ]] ||
  ssh-keygen -q -t ed25519 -N '' -C root@coolify -f /data/coolify/ssh/keys/id.root@host.docker.internal
# As the official installer: Coolify runs as uid 9999 and reads its SSH key.
chown -R 9999:root /data/coolify/source /data/coolify/ssh /data/coolify/images
chmod -R 700 /data/coolify/source /data/coolify/ssh /data/coolify/images
docker network inspect coolify >/dev/null 2>&1 || docker network create --attachable coolify >/dev/null
(cd /data/coolify/source && LATEST_IMAGE=$version docker compose --env-file .env -f docker-compose.yml -f docker-compose.prod.yml up -d --quiet-pull) >/dev/null
# Harness-only: kernels without IPv6 (some CI sandboxes) can't open Coolify's
# [::] listeners. Real hosts have IPv6; the installer warns otherwise.
if [[ ! -e /proc/net/if_inet6 ]]; then
  docker exec -u root coolify sh -c 'for f in /etc/nginx/site-opts.d/*.conf* /etc/nginx/sites-available/*; do sed -i "/listen \[::\]/d" "$f"; done' 2>/dev/null || true
fi

fail=0
pass() { echo "  ✓ $1"; }
miss() { echo "  ✗ $1"; fail=1; }
check() { if (eval "$2") >/dev/null 2>&1; then pass "$1"; else miss "$1"; fi; }

coolify_wait_healthy 600 || { docker logs --tail 80 coolify; echo "Coolify did not become healthy"; exit 1; }
[[ "$(coolify_current_version)" == "$version" ]] && pass "running $version" || miss "running $version (got $(coolify_current_version))"

echo "==> database schema"
if coolify_schema_check 2>&1; then pass "every column Vercelify uses exists"; else miss "schema"; fi
for c in instance_settings.is_api_enabled instance_settings.is_auto_update_enabled application_settings.docker_images_to_keep \
         applications.preview_url_template applications.status applications.deleted_at server_settings.concurrent_builds \
         application_previews.fqdn application_previews.pull_request_id servers.proxy personal_access_tokens.abilities \
         personal_access_tokens.team_id users.id; do
  coolify_has_column "${c%%.*}" "${c#*.}" && pass "$c" || miss "$c"
done

echo "==> installer steps against it"
configure_coolify http://127.0.0.1.sslip.io >/dev/null 2>&1 && pass "configure_coolify" || miss "configure_coolify"
check "auto-update turned off" '[[ $(coolify_psql "select is_auto_update_enabled from instance_settings") == f ]]'
check "API enabled" '[[ $(coolify_psql "select is_api_enabled from instance_settings") == t ]]'
check "wildcard domain set" '[[ $(coolify_psql "select wildcard_domain from server_settings where server_id=0") == http://127.0.0.1.sslip.io ]]'
# As install_coolify: Coolify's seeder, else the installer's fallback (when
# Coolify's DNS check rejects the e-mail, as on GitHub's runners).
coolify_wait_root_user 60 ||
  coolify_create_root_user "$(sed -n 's/^ROOT_USER_EMAIL=//p' "$env_file")" "$(sed -n 's/^ROOT_USER_PASSWORD=//p' "$env_file")" || true
check "admin account exists (owner of the root team)" \
  '[[ $(coolify_psql "select count(*) from team_user where user_id=0 and team_id=0 and role='"'"'owner'"'"'") == 1 ]]'
check "registration is closed" '[[ $(coolify_psql "select is_registration_enabled from instance_settings") == f ]]'
admin_password_works() {
  VFY_PW=$(sed -n 's/^ROOT_USER_PASSWORD=//p' "$env_file") docker exec -e VFY_PW coolify php artisan tinker \
    --execute='echo \Illuminate\Support\Facades\Hash::check(getenv("VFY_PW"), \App\Models\User::find(0)->password) ? "yes" : "no";' |
    grep -q yes
}
check "the admin can log in with the password from .env" admin_password_works
coolify_api_token >/dev/null 2>&1 || true
check "API token created" '[[ -s "$VERCELIFY_ETC/coolify.token" ]]'
token=$(cat "$VERCELIFY_ETC/coolify.token" 2>/dev/null || true)
api() { curl -fsS -m 20 -H "Authorization: Bearer $token" "$COOLIFY_URL_LOCAL/api/v1$1"; }
check "token reads /version" 'api /version'
check "token lists applications" 'api /applications | jq -e "type == \"array\""'
check "token lists servers" 'api /servers | jq -e "type == \"array\""'
check "token has no root ability" '[[ $(coolify_psql "select abilities from personal_access_tokens order by id desc limit 1") != *root* ]]'

echo "==> API routes the cockpit, vfy and the agent use"
# (route:list instantiates every controller and fails on some; read the router.)
routes=$(docker exec coolify php artisan tinker --execute 'foreach (app("router")->getRoutes() as $r) { if (str_starts_with($r->uri(), "api/v1/")) echo implode("|", $r->methods())." ".substr($r->uri(), 7)." ".$r->getActionName()."\n"; }' 2>/dev/null)
# A route that only answers "use POST" doesn't count.
has_route() { awk -v m="$1" -v u="$2" '$2 == u && $3 !~ /post_required/ && index("|" $1 "|", "|" m "|") { found = 1 } END { exit !found }' <<<"$routes"; }
while read -r method uri; do
  [[ -z "$method" ]] && continue
  has_route "$method" "$uri" && pass "$method /$uri" || miss "$method /$uri"
done <<'ROUTES'
GET version
GET applications
GET applications/{uuid}
PATCH applications/{uuid}
POST applications/public
GET applications/{uuid}/envs
PATCH applications/{uuid}/envs/bulk
GET applications/{uuid}/logs
POST applications/{uuid}/start
POST applications/{uuid}/stop
POST applications/{uuid}/restart
POST applications/{uuid}/rollback
GET applications/{uuid}/previews
DELETE applications/{uuid}/previews/{pull_request_id}
GET deployments/applications/{uuid}
GET deployments/{uuid}
POST deploy
GET servers
POST servers
GET projects
GET projects/{uuid}
POST projects
GET databases
POST databases/{uuid}/start
POST databases/postgresql
ROUTES

echo "==> the agent's SQL against the real database"
agent=${AGENT_BIN:-$repo/target/debug/vercelify-agent}
if [[ ! -x "$agent" ]]; then (cd "$repo" && cargo build -q -p vercelify-agent); fi
mkdir -p "$work/state" "$work/run"
echo contract-token-0123456789 >"$work/agent.token"
VERCELIFY_LISTEN=127.0.0.1 VERCELIFY_PORT=17420 VERCELIFY_TOKEN_FILE="$work/agent.token" VERCELIFY_ROLE=main \
  VERCELIFY_STATE_DIR="$work/state" VERCELIFY_RUN_DIR="$work/run" VERCELIFY_COOLIFY_TOKEN_FILE="$VERCELIFY_ETC/coolify.token" \
  VERCELIFY_WATCH_INTERVAL=0 VERCELIFY_SLEEP_SECONDS_PER_MINUTE=2 VERCELIFY_TOKENS_FILE="$work/state/tokens.json" VERCELIFY_AUDIT_LOG="$work/state/audit.log" \
  "$agent" serve >"$work/agent.log" 2>&1 &
agent_pid=$!
for _ in $(seq 30); do curl -fsS http://127.0.0.1:17420/v1/health >/dev/null 2>&1 && break; sleep 1; done
ag() { curl -fsS -m 120 -H 'Authorization: Bearer contract-token-0123456789' "http://127.0.0.1:17420/v1$1" "${@:2}"; }
check "agent lists Coolify volumes (SQL)" 'ag /coolify/volumes | jq -e "type == \"array\""'
check "agent finds the uptime targets (SQL)" 'ag /uptime | jq -e "type == \"array\""'
check "agent doctor sees Coolify healthy and the schema complete" \
  'ag "/doctor?refresh=true" | jq -e "[.checks[] | select(.id == \"coolify\" or .id == \"coolify-schema\") | .status] | all(. == \"ok\")"'
check "agent reads Coolify's worker key (SQL)" 'ag /coolify/worker-key | jq -e .public_key'

echo "==> proxy: request counters for sleeping apps"
# Coolify writes its proxy configuration when it first starts the proxy, over
# SSH, which this test skips: have it generate the configuration (saved to its
# database before the SSH step fails), without the metrics flags should an
# earlier KEEP=1 run have added them, then start the proxy from it.
docker exec coolify php artisan tinker --execute 'generateDefaultProxyConfiguration(App\Models\Server::find(0));' >/dev/null 2>&1 || true
coolify_psql "update servers set proxy = jsonb_set(proxy::jsonb, '{last_saved_proxy_configuration}', to_jsonb(regexp_replace(proxy::jsonb->>'last_saved_proxy_configuration', E' *- .--metrics\\.prometheus[^\\n]*\\n', '', 'g')))::json where id=0 and proxy::jsonb ? 'last_saved_proxy_configuration'" >/dev/null
coolify_psql "select proxy::jsonb->>'last_saved_proxy_configuration' from servers where id=0" >/data/coolify/proxy/docker-compose.yml
check "Coolify saved a Traefik proxy configuration" 'grep -q "providers.file.directory" /data/coolify/proxy/docker-compose.yml'
docker compose -f /data/coolify/proxy/docker-compose.yml up -d --quiet-pull >/dev/null 2>&1 || true
check "agent turns on the proxy's request counters (and restarts it)" "ag /sleep/proxy -X POST | jq -e '.metrics == true and .restarted == true'"
check "saved where Coolify reads it (its database)" \
  'coolify_psql "select proxy::jsonb->>'"'"'last_saved_proxy_configuration'"'"' from servers where id=0" | grep -q -- "--metrics.prometheus=true"'
check "and on disk" 'grep -q -- "--metrics.prometheus.addrouterslabels=true" /data/coolify/proxy/docker-compose.yml'
check "Coolify keeps the flags when it regenerates its proxy configuration" \
  "docker exec coolify php artisan tinker --execute '\$s = App\\Models\\Server::find(0); echo implode(\",\", extractCustomProxyCommands(\$s, \$s->proxy->get(\"last_saved_proxy_configuration\")));' | grep -q -- '--metrics.prometheus=true'"
check "Traefik serves its counters" 'for _ in $(seq 30); do curl -fsS http://127.0.0.1:8080/metrics | grep -q "^traefik_" && exit 0; sleep 2; done; exit 1'
check "turning it on again changes nothing" "ag /sleep/proxy -X POST | jq -e '.restarted == false'"

echo "==> an app sleeps when idle and wakes on the next request"
server_uuid=$(api /servers | jq -r '.[0].uuid')
project_uuid=$(api /projects | jq -r '[.[] | select(.name == "contract")][0].uuid // empty')
[[ -n "$project_uuid" ]] || project_uuid=$(curl -fsS -m 20 -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
  -d '{"name":"contract"}' "$COOLIFY_URL_LOCAL/api/v1/projects" | jq -r .uuid)
app_uuid=$(curl -fsS -m 30 -H "Authorization: Bearer $token" -H 'Content-Type: application/json' -d "$(jq -nc \
  --arg p "$project_uuid" --arg s "$server_uuid" \
  '{project_uuid: $p, server_uuid: $s, environment_name: "production", docker_registry_image_name: "traefik/whoami",
    docker_registry_image_tag: "latest", ports_exposes: "80", domains: "http://sleepy.contract.test", instant_deploy: false}')" \
  "$COOLIFY_URL_LOCAL/api/v1/applications/dockerimage" | jq -r .uuid) || app_uuid=''
# A KEEP=1 rerun: the app (and its domain) exist already.
[[ -n "$app_uuid" ]] || app_uuid=$(api /applications | jq -r '[.[] | select(.fqdn == "http://sleepy.contract.test")][0].uuid // empty')
check "an app created through Coolify's API" '[[ -n "$app_uuid" && "$app_uuid" != null ]]'
# Run it the way Coolify does (labels), without Coolify's SSH deploy.
docker rm -f "$app_uuid-contract" >/dev/null 2>&1 || true
docker run -d --name "$app_uuid-contract" --network coolify \
  -l coolify.managed=true -l "coolify.applicationUuid=$app_uuid" -l coolify.pullRequestId=0 -l traefik.enable=true \
  -l "traefik.http.routers.http-0-$app_uuid.entryPoints=http" \
  -l "traefik.http.routers.http-0-$app_uuid.rule=Host(\`sleepy.contract.test\`) && PathPrefix(\`/\`)" \
  -l "traefik.http.services.http-0-$app_uuid.loadbalancer.server.port=80" traefik/whoami >/dev/null
visit() { curl -s -o "$work/page" -w '%{http_code}' -H 'Host: sleepy.contract.test' http://127.0.0.1:80/; }
check "the app answers through the proxy" 'for _ in $(seq 20); do [[ $(visit) == 200 ]] && exit 0; sleep 1; done; exit 1'
agj() { curl -fsS -m 60 -H 'Authorization: Bearer contract-token-0123456789' -H 'Content-Type: application/json' -X "$1" -d "${3:-{\}}" "http://127.0.0.1:17420/v1$2"; }
check "sleep turned on for it" "agj PUT /sleep/$app_uuid '{\"enabled\":true,\"idle_minutes\":5}' | jq -e '.settings[0].enabled and .proxy_metrics'"
check "a wake route was written for the proxy" 'grep -q "sleepy.contract.test" /data/coolify/proxy/dynamic/vercelify-sleep.yaml'
sleep 12   # 5 "minutes" at 2 s each, plus the minimum time awake
agj POST /alerts/check >/dev/null
check "idle: its container was stopped" '[[ $(docker inspect -f "{{.State.Running}}" "$app_uuid-contract") == false ]]'
check "the agent reports it asleep" "ag /sleep | jq -e '.targets[0].state == \"asleep\"'"
# shellcheck disable=SC2034 # read by the check below
code=$(visit)
check "a visit gets the waking-up page" '[[ $code == 503 ]] && grep -q "Waking up sleepy.contract.test" "$work/page"'
check "and starts the app; it answers again" 'for _ in $(seq 30); do [[ $(visit) == 200 ]] && exit 0; sleep 1; done; exit 1'
check "sleep and wake are in the activity list" "ag /jobs | jq -e '[.[].kind] | index(\"sleep\") and index(\"wake\")'"
check "turning sleep off removes the wake route" "agj PUT /sleep/$app_uuid '{\"enabled\":false}' >/dev/null && [[ ! -e /data/coolify/proxy/dynamic/vercelify-sleep.yaml ]]"
docker rm -f "$app_uuid-contract" >/dev/null 2>&1 || true
check "the app's Domains tab can change its domains (PATCH domains)" \
  "curl -fsS -m 30 -X PATCH -H 'Authorization: Bearer $token' -H 'Content-Type: application/json' \
     -d '{\"domains\":\"https://www.contract.test,https://contract.test\"}' $COOLIFY_URL_LOCAL/api/v1/applications/$app_uuid >/dev/null &&
   [[ \$(api /applications/$app_uuid | jq -r .fqdn) == 'https://www.contract.test,https://contract.test' ]]"

echo
if (( fail )); then echo "Coolify $version: contract FAILED"; tail -20 "$work/agent.log"; exit 1; fi
echo "Coolify $version: contract OK"

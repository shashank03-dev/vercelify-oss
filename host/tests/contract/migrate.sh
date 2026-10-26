#!/usr/bin/env bash
# Contract test for `vercelify migrate` (design §18) against a real Coolify
# (the version the migration is tested with): a project with a PostgreSQL
# database, an image app with variables, a volume, a scheduled task and a
# password, a git app with the image Coolify built for it, and a one-click
# service — run the way Coolify runs them, data in each. Then: the plan
# changes nothing; apply takes everything over (Coolify stopped, data
# copied, no builds); every app answers through our proxy with its data;
# rollback brings Coolify back as it was; a second migration and finish
# leave no Coolify.
#
#   cargo build -p vercelify-agent
#   sudo host/tests/contract/migrate.sh    # root, Docker, curl, jq, openssl, ssh-keygen
#
# KEEP=1 leaves everything up. It replaces /data/coolify.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
# shellcheck source=lib-coolify.sh
. "$here/lib-coolify.sh"
version=${COOLIFY_VERSION:-4.4.6}
agent=${AGENT_BIN:-$repo/target/debug/vercelify-agent}
[[ -x "$agent" ]] || (cd "$repo" && cargo build -q -p vercelify-agent)
work=$(mktemp -d)
pass=0
ok() { pass=$((pass + 1)); echo "  ✓ $*"; }
fail() {
  echo "  ✗ $*" >&2
  [[ -f "$work/agent.log" ]] && tail -30 "$work/agent.log" >&2
  exit 1
}
until_ok() { # SECONDS CMD...
  local end=$((SECONDS + $1))
  shift
  until "$@" >/dev/null 2>&1; do
    ((SECONDS < end)) || return 1
    sleep 2
  done
}

export VERCELIFY_CONFIG=/nonexistent VERCELIFY_STATE_DIR="$work/state" VERCELIFY_RUN_DIR="$work/run" \
  VERCELIFY_PLATFORM_DB="$work/state/platform.db" VERCELIFY_MASTER_KEY_FILE="$work/master.key" \
  VERCELIFY_TOKEN_FILE="$work/agent.token" VERCELIFY_TOKENS_FILE="$work/state/tokens.json" \
  VERCELIFY_AUDIT_LOG="$work/audit.log" VERCELIFY_LISTEN=127.0.0.1 VERCELIFY_PORT=17431 VERCELIFY_ROLE=main
token=migrate-contract-0123456789
echo "$token" >"$work/agent.token"
mkdir -p "$work/state" "$work/run"

stop_agent() {
  [[ -s "$work/agent.pid" ]] || return 0
  kill "$(cat "$work/agent.pid")" 2>/dev/null || true
  timeout 20 tail --pid="$(cat "$work/agent.pid")" -f /dev/null 2>/dev/null || true
  rm -f "$work/agent.pid"
}
start_platform() {
  VERCELIFY_ENGINE=platform "$agent" serve >>"$work/agent.log" 2>&1 &
  echo $! >"$work/agent.pid"
  until_ok 30 curl -sf http://127.0.0.1:17431/v1/health || fail "the agent didn't start"
}
cleanup() {
  [[ "${KEEP:-0}" == 1 ]] && { echo "left running (work: $work)"; return; }
  stop_agent
  docker ps -aq --filter label=vercelify.managed | xargs -r docker rm -f >/dev/null 2>&1 || true
  docker rm -f vercelify-proxy >/dev/null 2>&1 || true
  [[ -n "${svc:-}" ]] && docker compose -p "$svc" -f "/data/coolify/services/$svc/docker-compose.yml" down -v >/dev/null 2>&1 || true
  docker ps -aq --filter label=coolify.managed=true | xargs -r docker rm -f >/dev/null 2>&1 || true
  coolify_stop
  docker volume ls -q --filter label=vercelify.from=coolify | xargs -r docker volume rm >/dev/null 2>&1 || true
  for v in "postgres-data-${db:-}" "${web:-}-uploads" "${svc:-}_it-tools-data"; do docker volume rm "$v" >/dev/null 2>&1 || true; done
  rm -rf "$work"
}
trap cleanup EXIT
ag() { curl -sf -H "Authorization: Bearer $token" "http://127.0.0.1:17431/v1$1"; }
via() { curl -s -o "$work/page" -w '%{http_code}' -H "Host: $1" "${@:2}" http://127.0.0.1/; }
answers() { [[ "$(via "$@")" == 200 ]]; }

echo "==> Coolify $version, with a shop"
coolify_start "$version" || fail "Coolify didn't start"
ok "Coolify $version runs"
server=$(capi GET /servers | jq -r '.[0].uuid')
project=$(capi POST /projects '{"name":"shop","description":"the shop"}' | jq -r .uuid)
db_json=$(capi POST /databases/postgresql "$(jq -nc --arg s "$server" --arg p "$project" \
  '{server_uuid: $s, project_uuid: $p, environment_name: "production", name: "shopdb", instant_deploy: false}')")
db=$(jq -r .uuid <<<"$db_json")
db_url=$(jq -r .internal_db_url <<<"$db_json")
web=$(capi POST /applications/dockerimage "$(jq -nc --arg s "$server" --arg p "$project" \
  '{server_uuid: $s, project_uuid: $p, environment_name: "production", name: "web", docker_registry_image_name: "traefik/whoami",
    docker_registry_image_tag: "latest", ports_exposes: "80", domains: "http://web.shop.test", instant_deploy: false}')" | jq -r .uuid)
capi POST "/applications/$web/envs" "$(jq -nc --arg v "$db_url" '{key: "DATABASE_URL", value: $v}')" >/dev/null
capi POST "/applications/$web/envs" '{"key": "GREETING", "value": "hello from coolify"}' >/dev/null
site=$(capi POST /applications/public "$(jq -nc --arg s "$server" --arg p "$project" \
  '{server_uuid: $s, project_uuid: $p, environment_name: "production", name: "site", git_repository: "https://github.com/traefik/whoami",
    git_branch: "master", build_pack: "dockerfile", ports_exposes: "80", domains: "http://site.shop.test", instant_deploy: false}')" | jq -r .uuid)
sha=0123456789abcdef0123456789abcdef01234567
cpsql "update applications set git_commit_sha='$sha' where uuid='$site'" >/dev/null
svc=$(capi POST /services "$(jq -nc --arg s "$server" --arg p "$project" \
  '{type: "it-tools", server_uuid: $s, project_uuid: $p, environment_name: "production", instant_deploy: false}')" | jq -r .uuid)
docker exec coolify php artisan tinker --execute '
  $a = App\Models\Application::where("uuid", "'"$web"'")->first();
  App\Models\LocalPersistentVolume::create(["name" => "'"$web"'-uploads", "mount_path" => "/uploads", "resource_id" => $a->id, "resource_type" => $a->getMorphClass()]);
  App\Models\ScheduledTask::create(["name" => "cleanup", "command" => "echo cleaned", "frequency" => "daily", "application_id" => $a->id, "team_id" => 0, "enabled" => true]);
  $a->is_http_basic_auth_enabled = true; $a->http_basic_auth_username = "shop"; $a->http_basic_auth_password = "open-sesame"; $a->save();
  App\Models\Service::where("uuid", "'"$svc"'")->first()->parse();
  echo "ok";' 2>/dev/null | grep -q ok || fail "Coolify's tinker"
ok "a project, a database, two apps and a service in Coolify"

echo "==> running them the way Coolify does"
for i in postgres:16-alpine traefik/whoami corentinth/it-tools:latest busybox:1.37; do pull_as "$i"; done
pw=$(sed -E 's|^[a-z]+://[^:]+:([^@]+)@.*|\1|' <<<"$db_url")
docker run -d --name "$db" --network coolify --restart unless-stopped -l coolify.managed=true -l coolify.type=database \
  -v "postgres-data-$db:/var/lib/postgresql/data" -e POSTGRES_USER=postgres -e "POSTGRES_PASSWORD=$pw" -e POSTGRES_DB=postgres \
  postgres:16-alpine >/dev/null
until_ok 60 docker exec "$db" pg_isready -U postgres -h 127.0.0.1 || fail "Coolify's database didn't start"
until_ok 30 docker exec "$db" psql -U postgres -c "create table orders (id int); insert into orders values (42)" || fail "writing the shop's data"
labels() { printf '%s\n' -l coolify.managed=true -l "coolify.applicationUuid=$1" -l coolify.pullRequestId=0; }
mapfile -t web_labels < <(labels "$web")
docker run -d --name "$web-111111111111" --network coolify --restart unless-stopped "${web_labels[@]}" \
  -l traefik.enable=true -l "traefik.http.routers.http-0-$web.entryPoints=http" \
  -l "traefik.http.routers.http-0-$web.rule=Host(\`web.shop.test\`) && PathPrefix(\`/\`)" \
  -l "traefik.http.services.http-0-$web.loadbalancer.server.port=80" \
  -v "$web-uploads:/uploads" traefik/whoami >/dev/null
docker run --rm -v "$web-uploads:/u" busybox:1.37 sh -c 'echo invoice-7 > /u/a.txt'
docker tag traefik/whoami "$site:$sha"
mapfile -t site_labels < <(labels "$site")
docker run -d --name "$site-222222222222" --network coolify --restart unless-stopped "${site_labels[@]}" \
  -l traefik.enable=true -l "traefik.http.routers.http-0-$site.entryPoints=http" \
  -l "traefik.http.routers.http-0-$site.rule=Host(\`site.shop.test\`) && PathPrefix(\`/\`)" \
  -l "traefik.http.services.http-0-$site.loadbalancer.server.port=80" "$site:$sha" >/dev/null
mkdir -p "/data/coolify/services/$svc"
cpsql "select docker_compose from services where uuid='$svc'" >"/data/coolify/services/$svc/docker-compose.yml"
: >"/data/coolify/services/$svc/.env"
docker network inspect "$svc" >/dev/null 2>&1 || docker network create --attachable "$svc" >/dev/null
docker compose --project-directory "/data/coolify/services/$svc" -f "/data/coolify/services/$svc/docker-compose.yml" \
  --project-name "$svc" up -d >/dev/null 2>&1 || fail "starting the service as Coolify does"
docker run --rm -v "${svc}_it-tools-data:/d" busybox:1.37 sh -c 'echo tools-data > /d/kept.txt'
tools_host=$(cpsql "select fqdn from service_applications where service_id=(select id from services where uuid='$svc')" | sed 's|^https\?://||')
coolify_proxy_start
docker network connect "$svc" coolify-proxy >/dev/null 2>&1 || true
until_ok 60 answers web.shop.test || fail "web doesn't answer through Coolify ($(via web.shop.test))"
until_ok 30 answers site.shop.test || fail "site doesn't answer through Coolify"
until_ok 90 answers "$tools_host" || fail "the service doesn't answer through Coolify ($(via "$tools_host"))"
ok "Coolify serves web, site and it-tools ($tools_host)"

echo "==> the plan"
plan=$("$agent" migrate plan 2>/dev/null) || fail "plan: $plan"
for want in "+ database shopdb" "+ app web" "+ app site" "+ service it-tools"; do
  [[ "$plan" == *"$want"* ]] || fail "the plan lacks '$want': $plan"
done
[[ "$plan" == *"it stops while their data is copied"* ]] || fail "the plan doesn't mention the volume: $plan"
[[ "$(docker inspect -f '{{.State.Running}}' coolify)" == true ]] || fail "the plan stopped Coolify"
[[ ! -s "$work/state/migrate/state.json" ]] || fail "the plan wrote a state"
ok "the plan lists what is taken over and changes nothing"

migrate_and_check() {
  "$agent" migrate apply >"$work/apply.log" 2>&1 || fail "apply: $(cat "$work/apply.log")"
  for c in coolify coolify-db coolify-proxy "$db" "$web-111111111111" "it-tools-$svc"; do
    [[ "$(docker inspect -f '{{.State.Running}} {{.HostConfig.RestartPolicy.Name}}' "$c")" == "false no" ]] ||
      fail "$c wasn't stopped for good: $(docker inspect -f '{{.State.Running}} {{.HostConfig.RestartPolicy.Name}}' "$c")"
  done
  start_platform
  "$agent" migrate check --wait 300 >"$work/check.log" 2>&1 || fail "check: $(cat "$work/check.log")"
}

echo "==> apply"
migrate_and_check
ok "taken over: Coolify stopped (kept), the first deployments healthy"
[[ "$(docker image inspect -f '{{.Id}}' vfy/site:coolify)" == "$(docker image inspect -f '{{.Id}}' "$site:$sha")" ]] ||
  fail "site doesn't run Coolify's image"
! grep -q "building" <(ag /platform/apps/site/deployments | jq -r '.[0].status') || fail "site was built"
ok "site runs the image Coolify built (no build)"
until_ok 60 answers web.shop.test -u shop:open-sesame || fail "web doesn't answer through our proxy: $(via web.shop.test -u shop:open-sesame)"
[[ "$(via web.shop.test)" == 401 ]] || fail "web lost its password"
ag "/platform/apps/web/env?reveal=true" | jq -e '.[] | select(.key == "GREETING" and .value == "hello from coolify")' >/dev/null ||
  fail "web's variables"
ok "web answers through our proxy, password kept, variables kept"
dburl=$(ag "/platform/apps/web/env?reveal=true" | jq -r '.[] | select(.key == "DATABASE_URL") | .value')
[[ "$dburl" == *'${{db.shopdb.host}}'* ]] || fail "DATABASE_URL doesn't refer to shopdb: $dburl"
net=$(docker inspect vfy-db-shopdb -f '{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}' | tr ' ' '\n' | grep vfy-net)
[[ "$(docker run --rm --network "$net" postgres:16-alpine psql "${db_url/$db/shopdb}" -tAc 'select id from orders')" == 42 ]] ||
  fail "the shop's data isn't in shopdb"
ok "shopdb has the shop's data; web's DATABASE_URL points at it"
[[ "$(cat "$(docker volume inspect -f '{{.Mountpoint}}' vfy-web-uploads)/a.txt")" == invoice-7 ]] || fail "web's uploads"
ok "web's volume was copied"
until_ok 60 answers site.shop.test || fail "site: $(via site.shop.test)"
until_ok 90 answers "$tools_host" || fail "it-tools: $(via "$tools_host")"
tools=$(ag /platform/apps | jq -r '.[] | select(.kind == "compose") | .name')
[[ "$(cat "$(docker volume inspect -f '{{.Mountpoint}}' "vfy-${tools}_it-tools-data")/kept.txt")" == tools-data ]] ||
  fail "the service's volume"
ok "site and the it-tools service answer; the service's data was copied"
ag /platform/apps/web/tasks | jq -e '.[0].name == "cleanup" and .[0].schedule == "0 0 * * *"' >/dev/null || fail "web's task"
ok "web's scheduled task (daily → 0 0 * * *)"
jq -e '.phase == "switched"' "$work/state/migrate/state.json" >/dev/null || fail "phase: $(jq -r .phase "$work/state/migrate/state.json")"

echo "==> rollback"
stop_agent
"$agent" migrate rollback >"$work/rollback.log" 2>&1 || fail "rollback: $(cat "$work/rollback.log")"
[[ -z "$(docker ps -aq --filter label=vercelify.app)" && -z "$(docker ps -aq --filter name=vercelify-proxy)" ]] ||
  fail "our containers are left: $(docker ps -a --format '{{.Names}}' --filter label=vercelify.managed)"
! docker volume inspect vfy-web-uploads >/dev/null 2>&1 || fail "our copies are left"
[[ "$(docker inspect -f '{{.HostConfig.RestartPolicy.Name}}' coolify)" != no ]] || fail "Coolify's restart policy"
until_ok 120 answers web.shop.test || fail "web doesn't answer through Coolify again: $(via web.shop.test)"
until_ok 60 answers "$tools_host" || fail "it-tools isn't back"
[[ "$(docker exec "$db" psql -U postgres -tAc 'select id from orders')" == 42 ]] || fail "Coolify's database"
until_ok 120 curl -fsS -m 5 "$COOLIFY_URL/api/health" || fail "Coolify itself isn't back"
ok "Coolify runs again as it was; ours is gone"

echo "==> again, then finish"
migrate_and_check
ok "migrated a second time"
"$agent" migrate finish >"$work/finish.log" 2>&1 || fail "finish: $(cat "$work/finish.log")"
! docker inspect coolify >/dev/null 2>&1 || fail "Coolify's container is left"
[[ -z "$(docker images -q 'ghcr.io/coollabsio/coolify')" ]] || fail "Coolify's image is left"
until_ok 60 answers web.shop.test -u shop:open-sesame || fail "web after finish"
until_ok 60 answers "$tools_host" || fail "it-tools after finish"
ok "Coolify removed; the apps keep answering"

echo "$pass checks passed"

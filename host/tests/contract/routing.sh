#!/usr/bin/env bash
# Contract test for project rules (vercel.json) on Vercelify's engine: the
# agent reads each app's base routes from the engine, writes its rules into
# the proxy's dynamic folder, and the proxy (the engine's own Traefik) must
# then behave like Vercel.
#
#   sudo host/tests/contract/routing.sh        # needs Docker, curl and jq
#   TRAEFIK_IMAGE=traefik:v3.6 sudo host/tests/contract/routing.sh
#
# Runs the agent with the engine and its proxy, deploys two apps as image
# apps — a static site with real 404s (routing_app.mjs) and whoami over https
# behind basic auth — plus a whoami on this machine as an "external" site,
# then drives everything through the agent's API. Ports 18080/18443/18084
# (proxy), 18083 (external site), 17421 (agent) by default; ROUTING_*_PORT
# changes them.
set -euo pipefail
# Everything here talks to this machine.
export NO_PROXY="*" no_proxy="*"
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=lib-guard.sh
. "$here/lib-guard.sh"
repo=$(cd "$here/../../.." && pwd)
image=${TRAEFIK_IMAGE:-traefik:v3.7}
whoami=${WHOAMI_IMAGE:-traefik/whoami:v1.11}
node=${NODE_IMAGE:-node:22-slim}
work=$(mktemp -d)
# Traefik before 3.5 can't rewrite the status: the page shows, with a 404.
tag=${image##*:}
minor=${tag#v3.}
minor=${minor%%.*}
fallback_code=200
if [[ "$tag" == v3.* && "$minor" =~ ^[0-9]+$ ]] && ((minor < 5)); then fallback_code=404; fi
token=routing-contract-0123456789
p=vfy-routing
# Ports on this machine (override if taken).
http=${ROUTING_HTTP_PORT:-18080}
https=${ROUTING_HTTPS_PORT:-18443}
metrics=${ROUTING_METRICS_PORT:-18084}
ext_port=${ROUTING_EXT_PORT:-18083}
agent_port=${ROUTING_AGENT_PORT:-17421}
dyn=$work/state/proxy/dynamic

cleanup() {
  if [[ "${KEEP:-0}" == 1 ]]; then
    echo "kept $work and the containers (agent pid ${agent_pid:-})"
    return
  fi
  [[ -n "${agent_pid:-}" ]] && kill "$agent_pid" 2>/dev/null || true
  sleep 1
  # (Two label filters would have to match both.)
  for a in rt-web rt-echo; do docker ps -aq --filter "label=vercelify.app-name=$a" | xargs -r docker rm -f >/dev/null 2>&1 || true; done
  docker rm -f vercelify-proxy "$p-ext" >/dev/null 2>&1 || true
  docker rmi "$p-web:contract" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

fail=0
pass() { echo "  ✓ $1"; }
miss() {
  echo "  ✗ $1"
  fail=1
}
check() { if (eval "$2") >/dev/null 2>&1; then pass "$1"; else miss "$1"; fi; }
# Wait up to $3 seconds for a check to pass (the proxy reloads files async).
eventually() {
  local i
  for ((i = 0; i < ${3:-15}; i++)); do
    if (eval "$2") >/dev/null 2>&1; then
      pass "$1"
      return
    fi
    sleep 1
  done
  miss "$1"
}

# A request through the proxy: c HOST PATH [curl args…] → "code location".
c() {
  local host=$1 path=$2
  shift 2
  local out
  out=$(curl -s -o /dev/null -m 10 -w '%{http_code} %header{location}' -H "Host: $host" "$@" "http://127.0.0.1:$http$path")
  echo "$(date +%T) http://$host$path $* → $out" >>"$work/probes.log"
  echo "$out"
}
body() {
  local host=$1 path=$2
  shift 2
  curl -s -m 10 -H "Host: $host" "$@" "http://127.0.0.1:$http$path"
}
hdr() { # hdr HOST PATH HEADER [curl args…]
  local host=$1 path=$2 name=$3
  shift 3
  curl -s -o /dev/null -m 10 -w "%header{$name}" -H "Host: $host" "$@" "http://127.0.0.1:$http$path"
}
secure() { # secure PATH [curl args…] → body over https (self-signed default certificate)
  local path=$1
  shift
  curl -sk -m 10 --resolve "echo.test:$https:127.0.0.1" "$@" "https://echo.test:$https$path"
}
secure_code() {
  local path=$1 out
  shift
  out=$(curl -sk -o /dev/null -m 10 -w '%{http_code} %header{location}' --resolve "echo.test:$https:127.0.0.1" "$@" "https://echo.test:$https$path")
  echo "$(date +%T) https://echo.test$path $* → $out" >>"$work/probes.log"
  echo "$out"
}

for port in "$http" "$https" "$metrics" "$ext_port" "$agent_port"; do
  if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
    echo "port $port is already in use (set ROUTING_*_PORT)" >&2
    exit 2
  fi
done

# A second address of this machine: requests from it are not "local", so the
# tunnel's Cf-Connecting-Ip header must be ignored for them.
ext_ip=$(hostname -I 2>/dev/null | tr ' ' '\n' | grep -Ev '^(127\.|172\.(1[6-9]|2[0-9]|3[01])\.|$)' | grep -v : | head -1 || true)

echo "==> agent and proxy ($image)"
agent=${AGENT_BIN:-$repo/target/debug/vercelify-agent}
if [[ ! -x "$agent" ]]; then (cd "$repo" && cargo build -q -p vercelify-agent); fi
mkdir -p "$work/state" "$work/run"
echo "role=main" >"$work/install.env"
docker rm -f vercelify-proxy "$p-ext" >/dev/null 2>&1 || true
# The "external" site of the proxy rewrite runs on this machine, which a real
# server refuses (rewrite_private_targets): allow it here.
VERCELIFY_CONFIG=/nonexistent VERCELIFY_ENGINE=platform VERCELIFY_LISTEN=127.0.0.1 VERCELIFY_PORT="$agent_port" \
  VERCELIFY_TOKEN="$token" VERCELIFY_ROLE=main VERCELIFY_INSTALL_ENV="$work/install.env" \
  VERCELIFY_STATE_DIR="$work/state" VERCELIFY_RUN_DIR="$work/run" \
  VERCELIFY_PLATFORM_DB="$work/state/platform.db" VERCELIFY_MASTER_KEY_FILE="$work/state/master.key" \
  VERCELIFY_PROXY_IMAGE="$image" VERCELIFY_PROXY_HTTP_PORT=$http VERCELIFY_PROXY_HTTPS_PORT=$https \
  VERCELIFY_PROXY_METRICS_PORT=$metrics VERCELIFY_TRAEFIK_METRICS_URL="http://127.0.0.1:$metrics/metrics" \
  VERCELIFY_WATCH_INTERVAL=0 VERCELIFY_AUTODEPLOY_INTERVAL=0 VERCELIFY_WAKE_LISTEN='' \
  VERCELIFY_REWRITE_PRIVATE_TARGETS=1 VERCELIFY_BUILD_DRIVER=docker \
  VERCELIFY_TOKENS_FILE="$work/state/tokens.json" VERCELIFY_AUDIT_LOG="$work/state/audit.log" \
  "$agent" serve >"$work/agent.log" 2>&1 &
agent_pid=$!
for _ in $(seq 60); do curl -fsS "http://127.0.0.1:$agent_port/v1/health" >/dev/null 2>&1 && break; sleep 1; done
curl -fsS "http://127.0.0.1:$agent_port/v1/health" >/dev/null || {
  echo "the agent did not start:" >&2
  tail -20 "$work/agent.log" >&2
  exit 1
}
ag() { curl -sS -m 60 -H "Authorization: Bearer $token" -H 'Content-Type: application/json' "http://127.0.0.1:$agent_port/v1$1" "${@:2}"; }
check "agent advertises projects and the engine" 'ag /health | jq -e "(.capabilities | index(\"projects\")) and (.capabilities | index(\"platform\"))"'
pa() { ag "/platform$1" "${@:2}"; }
# An engine call that must work (setup, not a check).
pok() {
  local out code
  out=$(pa "$@" -w '\n%{http_code}')
  code=${out##*$'\n'}
  [[ "$code" == 2* ]] || { echo "the engine refused $1: HTTP $code ${out%$'\n'*}" >&2; exit 1; }
}

echo "==> apps"
# The static site: an image built here, so nothing is pulled for it.
mkdir -p "$work/web/www/assets" "$work/web/www/blog"
# Over 1 KB, like a real page.
{
  echo '<!doctype html><title>index</title><h1>spa-index</h1>'
  head -c 2000 /dev/zero | tr '\0' 'x'
} >"$work/web/www/index.html"
echo 'console.log(1)' >"$work/web/www/assets/app.js"
echo 'about-us' >"$work/web/www/about-us.html"
echo 'blog' >"$work/web/www/blog/index.html"
cp "$here/routing_app.mjs" "$work/web/"
printf 'FROM %s\nCOPY . /app\nCMD ["node", "/app/routing_app.mjs", "8080", "/app/www", "/tmp/cron.log"]\n' "$node" >"$work/web/Dockerfile"
docker build -q -t "$p-web:contract" "$work/web" >/dev/null
docker run -d --name "$p-ext" --network host "$whoami" --port "$ext_port" >/dev/null
# Apps get only the domains given here (no wildcard).
pok /settings/domain -X PUT -d '{"wildcard":null,"https":false,"edge_tls":false}'
app() { # NAME IMAGE PORT → id
  pa /apps -X POST -d "$(jq -nc --arg n "$1" --arg i "$2" --argjson p "$3" \
    '{name: $n, kind: "image", source: {image: $i}, runtime: {port: $p, health_timeout_secs: 90, drain_secs: 1}}')" | jq -r .id
}
web=$(app rt-web "$p-web:contract" 8080)
echoapp=$(app rt-echo "$whoami" 80)
[[ -n "$web" && "$web" != null && -n "$echoapp" && "$echoapp" != null ]] || { echo "could not create the apps" >&2; tail -20 "$work/agent.log" >&2; exit 1; }
pok "/apps/rt-web/domains" -X POST -d '{"host":"web.test","https":false,"primary":true}'
pok "/apps/rt-echo/domains" -X POST -d '{"host":"echo.test","https":true,"primary":true}'
pok "/apps/rt-echo/basic-auth" -X PUT -d '{"user":"user","password":"password1"}'
pok "/apps/rt-web/env" -X PUT -d '{"vars":[{"key":"CRON_SECRET","value":"s3cret","secret":true}],"replace":false}'
pok /apps/rt-web/deploy -X POST -d '{}'
pok /apps/rt-echo/deploy -X POST -d '{}'
live() { pa "/apps/$1/deployments" | jq -e '.[0].status == "healthy"' >/dev/null; }
eventually "both apps deployed" 'live rt-web && live rt-echo' 180
eventually "the proxy serves the engine's routes" '[[ "$(c web.test /assets/app.js)" == "200 " ]]' 30
check "the app's basic auth guards the https app" '[[ "$(secure_code /)" == "401 " ]]'
# What the static app saw of the cron jobs.
cron_log() { docker exec "$(docker ps -q --filter label=vercelify.app-name=rt-web | head -1)" cat /tmp/cron.log 2>/dev/null; }

# upload APP VERCEL_JSON VERCELIFY_JSON
upload() {
  jq -n --arg a "$2" --arg b "$3" '{files: ([{name: "vercel.json", content: $a}] + (if $b == "" then [] else [{name: "vercelify.json", content: $b}] end))}' |
    ag "/projects/$1" -X PUT --data-binary @-
}

# The configurations below are used inside the eval'd checks.
# shellcheck disable=SC2034
web_json=$(
  cat <<'JSON'
{
  "$schema": "https://openapi.vercel.sh/vercel.json",
  "cleanUrls": true,
  "trailingSlash": false,
  "redirects": [
    { "source": "/old/:slug", "destination": "/new/:slug" },
    { "source": "/temp", "destination": "/", "permanent": false },
    { "source": "/gh", "destination": "https://github.com/vercel/vercel" },
    { "source": "/promo", "destination": "/sale?src=promo" },
    { "source": "/beta", "destination": "/beta-home", "has": [{ "type": "cookie", "key": "beta" }] }
  ],
  "rewrites": [
    { "source": "/ext/:path*", "destination": "http://host.docker.internal:EXT_PORT/v1/:path*" },
    { "source": "/(.*)", "destination": "/index.html" }
  ],
  "headers": [
    { "source": "/(.*)", "headers": [{ "key": "X-Frame-Options", "value": "DENY" }] },
    { "source": "/assets/(.*)", "headers": [{ "key": "Cache-Control", "value": "public, max-age=31536000, immutable" }] }
  ],
  "crons": [{ "path": "/api/cron", "schedule": "* * * * *" }],
  "functions": { "api/*.js": { "memory": 1024 } }
}
JSON
)
web_json=${web_json//EXT_PORT/$ext_port}
# shellcheck disable=SC2034
web_fw=$(jq -n --arg ip "${ext_ip:-192.0.2.254}" '{firewall: {
  blockIps: ["203.0.113.0/24", $ip],
  rules: [
    {name: "wp", path: "/wp-admin/(.*)", action: "deny"},
    {userAgent: "(?i)badbot", methods: ["POST"], action: "deny"},
    {path: "/api/limited", action: "rate_limit", requests: 3, window: 60}
  ]}}')
# shellcheck disable=SC2034
echo_json='{"redirects":[{"source":"/old","destination":"/new"}],"rewrites":[{"source":"/blog/:slug","destination":"/posts/:slug"}]}'
# shellcheck disable=SC2034
echo_fw='{"firewall":{"trustedIps":["198.51.100.7"]}}'

echo "==> check without applying"
check "a configuration is checked without applying it" \
  'jq -n --arg a "$web_json" "{files: [{name: \"vercel.json\", content: \$a}]}" | ag /projects/check -X POST --data-binary @- | jq -e ".redirects == 5 and .rewrites == 2 and (.warnings | map(select(test(\"functions\"))) | length == 1)"'
check "broken JSON is refused" '[[ "$(echo "{\"files\":[{\"name\":\"vercel.json\",\"content\":\"{\"}]}" | ag /projects/check -X POST --data-binary @- -o /dev/null -w "%{http_code}")" == 400 ]]'
check "nothing written yet" '! ls "$dyn" | grep -q vercelify-routes-'

echo "==> upload and apply"
check "web app's rules accepted" 'upload "$web" "$web_json" "$web_fw" | jq -e ".routes_active == true and .summary.firewall_rules == 4"'
check "echo app's rules accepted" 'upload "$echoapp" "$echo_json" "$echo_fw" | jq -e ".routes_active == true and (.hosts == [\"echo.test\"])"'
check "one routes file per app" '[[ -f "$dyn/vercelify-routes-$web.yaml" && -f "$dyn/vercelify-routes-$echoapp.yaml" ]]'
check "status lists both, with the proxy version" 'ag /projects | jq -e "(.projects | length == 2) and (.proxy_image | test(\"traefik\")) and .error == null"'

# Traefik applies file changes within a couple of seconds (it throttles reloads).
eventually "the proxy picked up both apps' rules" '[[ "$(c web.test /temp)" == "302 /" && "$(secure_code /old -u user:password1)" == "301 /new" ]]'

echo "==> redirects"
check "permanent redirect keeps the query" '[[ "$(c web.test "/old/hello?x=1")" == "301 /new/hello?x=1" ]]'
check "case-insensitive source" '[[ "$(c web.test /OLD/Hi)" == "301 /new/Hi" ]]'
check "temporary redirect" '[[ "$(c web.test /temp)" == "302 /" ]]'
check "redirect to another site" '[[ "$(c web.test /gh)" == "301 https://github.com/vercel/vercel" ]]'
check "destination query merged with the visitor's" '[[ "$(c web.test "/promo?a=1")" == "301 /sale?src=promo&a=1" ]]'
check "destination query alone" '[[ "$(c web.test /promo)" == "301 /sale?src=promo" ]]'
check "has: cookie present → redirect" '[[ "$(c web.test /beta -H "Cookie: x=1; beta=yes")" == "301 /beta-home" ]]'
check "has: cookie missing → no redirect" '[[ "$(c web.test /beta)" == "$fallback_code " ]]'
check "cleanUrls: /about-us.html → /about-us" '[[ "$(c web.test /about-us.html)" == "301 /about-us" ]]'
check "cleanUrls: /blog/index.html → /blog" '[[ "$(c web.test /blog/index.html)" == "301 /blog" ]]'
check "cleanUrls: /index.html → /" '[[ "$(c web.test /index.html)" == "301 /" ]]'
check "trailingSlash false: /docs/ → /docs" '[[ "$(c web.test "/docs/?q=1")" == "301 /docs?q=1" ]]'

echo "==> rewrites"
check "files are served as they are" '[[ "$(body web.test /assets/app.js)" == "console.log(1)" ]]'
check "SPA fallback: unknown path serves index.html ($fallback_code)" '[[ "$(c web.test /dashboard/settings)" == "$fallback_code " ]] && body web.test /dashboard/settings | grep -q spa-index'
check "proxy to another site with the path rewritten" 'body web.test /ext/a/b | grep -q "^GET /v1/a/b "'
check "the other site gets its own Host" 'body web.test /ext/x | grep -q "^Host: host.docker.internal:$ext_port"'
check "path rewrite with a parameter (behind basic auth, over https)" 'secure /blog/hello -u user:password1 | grep -q "^GET /posts/hello "'
check "the app's basic auth still runs first" '[[ "$(secure_code /blog/hello)" == "401 " ]]'
check "redirect on the https app" '[[ "$(secure_code /old -u user:password1)" == "301 /new" ]]'
check "the http→https redirect still applies" '[[ "$(c echo.test /old)" == "301 https://echo.test/old" ]]'

echo "==> headers"
check "catch-all header on pages" '[[ "$(hdr web.test / x-frame-options)" == DENY ]]'
check "catch-all header on the SPA fallback" '[[ "$(hdr web.test /some/route x-frame-options)" == DENY ]]'
check "catch-all header on redirects" '[[ "$(hdr web.test /temp x-frame-options)" == DENY ]]'
check "path header on its paths" '[[ "$(hdr web.test /assets/app.js cache-control)" == "public, max-age=31536000, immutable" ]]'
check "path header not elsewhere" '[[ -z "$(hdr web.test / cache-control)" ]]'
check "both headers on a covered path" '[[ "$(hdr web.test /assets/app.js x-frame-options)" == DENY ]]'

echo "==> firewall (direct visitors)"
check "deny by path" '[[ "$(c web.test /wp-admin/x.php)" == "403 " ]]'
check "deny by user agent + method" '[[ "$(c web.test / -X POST -A BadBot/1.0)" == "403 " ]]'
check "same user agent, other method passes" '[[ "$(c web.test / -A BadBot/1.0)" == "200 " ]]'
check "rate limit: 3 requests pass" 'for i in 1 2 3; do [[ "$(c web.test /api/limited)" == "$fallback_code " ]] || exit 1; done'
check "rate limit: the 4th is refused" '[[ "$(c web.test /api/limited)" == "429 " ]]'
check "tunnel header from this machine: blocked range" '[[ "$(c web.test / -H "Cf-Connecting-Ip: 203.0.113.9")" == "403 " ]]'
check "tunnel header from this machine: other visitor" '[[ "$(c web.test / -H "Cf-Connecting-Ip: 198.51.100.1")" == "200 " ]]'
check "trusted IPs: this machine itself passes" '[[ "$(secure_code / -u user:password1)" == "200 " ]]'
check "trusted IPs: a trusted visitor through the tunnel" '[[ "$(secure_code / -u user:password1 -H "Cf-Connecting-Ip: 198.51.100.7")" == "200 " ]]'
check "trusted IPs: anyone else gets 403" '[[ "$(secure_code / -u user:password1 -H "Cf-Connecting-Ip: 1.1.1.1")" == "403 " ]]'
if [[ -n "$ext_ip" ]]; then
  ec() { curl -s -o /dev/null -m 10 -w '%{http_code}' --interface "$ext_ip" -H "Host: $1" "${@:3}" "http://$ext_ip:$http$2"; }
  check "blocked direct visitor ($ext_ip)" '[[ "$(ec web.test /)" == 403 ]]'
  check "a direct visitor can't fake the tunnel header" '[[ "$(ec web.test / -H "Cf-Connecting-Ip: 198.51.100.1")" == 403 ]]'
  check "untrusted direct visitor refused by trusted IPs" '[[ "$(curl -sk -o /dev/null -w "%{http_code}" --interface "$ext_ip" --resolve "echo.test:$https:$ext_ip" -u user:password1 -H "Cf-Connecting-Ip: 198.51.100.7" "https://echo.test:$https/")" == 403 ]]'
else
  echo "  - no second address on this machine: direct-visitor checks skipped"
fi

echo "==> firewall behind the Cloudflare tunnel"
echo "domain_mode=cloudflare" >>"$work/install.env"
check "rules re-applied for tunnel mode" 'upload "$web" "$web_json" "$web_fw" | jq -e .routes_active'
eventually "rate limit counts per tunnel visitor" 'grep -q requestHeaderName "$dyn/vercelify-routes-$web.yaml"' 5
# Direct visitors used up this machine's allowance above; once the proxy
# counts per tunnel visitor, a new one passes.
eventually "the proxy applied it" '[[ "$(c web.test /api/limited -H "Cf-Connecting-Ip: 192.0.2.99")" != "429 " ]]' 15
check "visitor A: 3 requests pass, the 4th is refused" 'for i in 1 2 3; do [[ "$(c web.test /api/limited -H "Cf-Connecting-Ip: 192.0.2.10")" != "429 " ]] || exit 1; done; [[ "$(c web.test /api/limited -H "Cf-Connecting-Ip: 192.0.2.10")" == "429 " ]]'
check "visitor B is not limited by A" '[[ "$(c web.test /api/limited -H "Cf-Connecting-Ip: 192.0.2.11")" != "429 " ]]'

echo "==> cron jobs"
check "run a cron job now" 'ag "/projects/$web/crons/run" -X POST -d "{\"path\":\"/api/cron\"}" | jq -e ".kind == \"cron\""'
eventually "it reached the app with CRON_SECRET and Vercel's user agent" \
  'cron_log | jq -e "select(.authorization == \"Bearer s3cret\" and .user_agent == \"vercel-cron/1.0\" and .path == \"/api/cron\")"' 15
eventually "its result is recorded" 'ag /projects | jq -e ".projects[] | select(.key == \"$web\") | .summary.crons[0] | .last_status == 200 and .next_run_at != null"' 10
eventually "the schedule runs it again by itself (* * * * *)" '[[ $(cron_log | wc -l) -ge 2 ]]' 75
check "an unknown cron path is refused" '[[ "$(ag "/projects/$web/crons/run" -X POST -d "{\"path\":\"/nope\"}" -o /dev/null -w "%{http_code}")" == 404 ]]'

echo "==> a stopped app (asleep or redeploying)"
pok /apps/rt-echo -X PATCH -d '{"desired":"stopped"}'
eventually "its routes are skipped, not an error page from our rules" '[[ "$(secure_code /old -u user:password1)" == "404 " ]]' 30
check "other apps keep working" '[[ "$(c web.test /temp)" == "302 /" ]]'
pok /apps/rt-echo -X PATCH -d '{"desired":"running"}'
eventually "back when it starts again" '[[ "$(secure_code /old -u user:password1)" == "301 /new" ]]' 90

echo "==> removing rules"
check "remove the web app's rules" 'ag "/projects/$web" -X DELETE | jq -e .removed'
check "its file is gone" '[[ ! -e "$dyn/vercelify-routes-$web.yaml" ]]'
eventually "the proxy serves the app with its base routes again" '[[ "$(c web.test /temp)" == "404 " && "$(c web.test /wp-admin/x)" == "404 " ]]'
check "the other app keeps its rules" '[[ "$(secure_code /old -u user:password1)" == "301 /new" ]]'
check "proxy logged no configuration errors" '! docker logs vercelify-proxy 2>&1 | grep -E "level=error|ERR" | grep -v -E "acme|certificate|letsencrypt|does not exist" | grep -q .'

if [[ $fail != 0 ]]; then
  echo "--- agent log"
  tail -40 "$work/agent.log"
  echo "--- proxy log"
  docker logs --tail 40 vercelify-proxy 2>&1
  echo "--- last probes"
  tail -30 "$work/probes.log"
  for f in "$dyn"/*; do
    [[ -e "$f" ]] && echo "--- $f" && cat "$f"
  done
  exit 1
fi
echo "all routing checks passed"

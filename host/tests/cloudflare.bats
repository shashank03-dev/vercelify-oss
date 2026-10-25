#!/usr/bin/env bats
# lib/cloudflare.sh against a stateful mock of the Cloudflare API.

setup() {
  T=$(mktemp -d)
  python3 "$BATS_TEST_DIRNAME/mock_cloudflare.py" "$T/port" "$T/state.json" &
  MOCK_PID=$!
  for _ in $(seq 50); do [ -s "$T/port" ] && break; sleep 0.1; done
  export NO_COLOR=1 VERCELIFY_LOG="$T/log" CF_API_BASE="http://127.0.0.1:$(cat "$T/port")/client/v4"
  . "$BATS_TEST_DIRNAME/../lib/common.sh"
  . "$BATS_TEST_DIRNAME/../lib/cloudflare.sh"
  CF_TOKEN=test-cf-token
}

teardown() { kill "$MOCK_PID" 2>/dev/null; rm -rf "$T"; }

state() { jq -r "$1" "$T/state.json"; }

@test "zone lookup walks up from a subdomain" {
  run cf_find_zone apps.example.com
  [ "$status" -eq 0 ]
  [ "$output" = "zone123 acc456 example.com" ]
}

@test "unknown zone gives a clear error" {
  run cf_find_zone nothere.org
  [ "$status" -ne 0 ]
  [[ "$output" == *"no active Cloudflare zone found for nothere.org"* ]]
}

@test "bad token surfaces Cloudflare's message" {
  CF_TOKEN=wrong
  run cf_find_zone example.com
  [ "$status" -ne 0 ]
  [[ "$output" == *"Authentication error"* ]]
}

@test "tunnel is created once and then reused" {
  id1=$(cf_ensure_tunnel acc456 vercelify-laptop)
  id2=$(cf_ensure_tunnel acc456 vercelify-laptop)
  [ -n "$id1" ] && [ "$id1" = "$id2" ]
  [ "$(state '.tunnels | length')" -eq 1 ]
  [ "$(cf_tunnel_token acc456 "$id1")" = "eyJ-token-for-$id1" ]
}

@test "ingress routes wildcard + apex to Traefik with a catch-all" {
  id=$(cf_ensure_tunnel acc456 t1)
  cf_configure_ingress acc456 "$id" example.com
  [ "$(state ".configs[\"$id\"].ingress[0].hostname")" = "*.example.com" ]
  [ "$(state ".configs[\"$id\"].ingress[0].service")" = "http://localhost:80" ]
  [ "$(state ".configs[\"$id\"].ingress[1].hostname")" = "example.com" ]
  [ "$(state ".configs[\"$id\"].ingress[2].service")" = "http_status:404" ]
}

@test "a mesh coordinator gets mesh.DOMAIN to its control listener" {
  export VERCELIFY_ETC="$T/etc"
  mkdir -p "$VERCELIFY_ETC"
  printf 'role = "main"\n' >"$VERCELIFY_ETC/agent.toml"
  id=$(cf_ensure_tunnel acc456 t1)
  cf_configure_ingress acc456 "$id" example.com
  [ "$(state ".configs[\"$id\"].ingress | map(select(.hostname == \"mesh.example.com\")) | length")" -eq 0 ]
  printf 'mesh_coordinator = true\n' >>"$VERCELIFY_ETC/agent.toml"
  cf_configure_ingress acc456 "$id" example.com
  [ "$(state ".configs[\"$id\"].ingress[0].hostname")" = "mesh.example.com" ]
  [ "$(state ".configs[\"$id\"].ingress[0].service")" = "http://localhost:7443" ]
  # Before the wildcard, so it wins.
  [ "$(state ".configs[\"$id\"].ingress[1].hostname")" = "*.example.com" ]
}

@test "DNS: create, then update in place" {
  cf_upsert_cname zone123 "*.example.com" "t1.cfargotunnel.com"
  cf_upsert_cname zone123 "*.example.com" "t2.cfargotunnel.com"
  [ "$(state '[.dns[] | select(.name == "*.example.com")] | length')" -eq 1 ]
  [ "$(state '.dns[] | select(.name == "*.example.com") | .content')" = t2.cfargotunnel.com ]
  [ "$(state '.dns[] | select(.name == "*.example.com") | .proxied')" = true ]
}

@test "DNS: existing A record is left alone unless forced" {
  # seed an A record for the apex (an existing website)
  curl -fsS -X POST -H "Authorization: Bearer test-cf-token" -H 'Content-Type: application/json' \
    --data '{"type":"A","name":"example.com","content":"203.0.113.9"}' "$CF_API_BASE/zones/zone123/dns_records" >/dev/null
  run cf_upsert_cname zone123 example.com t1.cfargotunnel.com
  [ "$status" -eq 0 ]
  [[ "$output" == *"already has an A record — left untouched"* ]]
  [ "$(state '.dns[] | select(.name == "example.com" and .type == "A") | .content')" = 203.0.113.9 ]
  cf_upsert_cname zone123 example.com t1.cfargotunnel.com 1
  [ "$(state '[.dns[] | select(.name == "example.com" and .type == "A")] | length')" -eq 0 ]
  [ "$(state '.dns[] | select(.name == "example.com" and .type == "CNAME") | .content')" = t1.cfargotunnel.com ]
  # mail keeps working
  [ "$(state '.dns[] | select(.type == "MX") | .content')" = mail.example.com ]
}

@test "routes: apps on pooled servers get their own tunnel route, before the wildcard" {
  out=$(printf 'https://shop.example.com,http://www.shop.example.com|100.64.0.7\nhttps://other.org|100.64.0.8\n|100.64.0.9\nhttp://pr-3-shop.example.com|100.64.0.7\nhttps://evil.example.com|1.2.3.4;rm\n' |
    routes_from_rows example.com)
  [ "$(jq length <<<"$out")" -eq 3 ]
  [ "$(jq -r '.[] | select(.hostname == "shop.example.com") | .service' <<<"$out")" = "http://100.64.0.7:80" ]
  [ "$(jq -r '[.[].hostname] | sort | join(" ")' <<<"$out")" = "pr-3-shop.example.com shop.example.com www.shop.example.com" ]
  id=$(cf_ensure_tunnel acc456 t1)
  cf_configure_ingress acc456 "$id" example.com "$out"
  [ "$(state ".configs[\"$id\"].ingress[0].service")" = "http://100.64.0.7:80" ]
  [ "$(state ".configs[\"$id\"].ingress[3].hostname")" = "*.example.com" ]
  [ "$(state ".configs[\"$id\"].ingress[5].service")" = "http_status:404" ]
}

@test "routes: sync only talks to Cloudflare when the routes changed" {
  id=$(cf_ensure_tunnel acc456 t1)
  export VERCELIFY_ETC="$T/etc" VERCELIFY_STATE="$T/state"
  mkdir -p "$T/etc"
  printf 'CF_API_TOKEN=test-cf-token\nCF_ACCOUNT=acc456\nCF_TUNNEL=%s\nCF_DOMAIN=example.com\n' "$id" >"$T/etc/cloudflare.env"
  worker_app_rows() { echo 'https://shop.example.com|100.64.0.7'; }
  run routes_sync
  [ "$status" -eq 0 ]
  [[ "$output" == *"1 app hostname(s)"* ]]
  [ "$(state ".configs[\"$id\"].ingress[0].hostname")" = shop.example.com ]
  run routes_sync
  [ "$status" -eq 0 ]
  [ -z "$output" ]   # unchanged: no API call, no output
}

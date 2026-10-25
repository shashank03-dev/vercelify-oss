#!/usr/bin/env bash
# Vercelify's own platform engine (instead of Coolify): helpers for the CLI.
# The engine lives in the agent; the CLI talks to it through agent_api.

# platform_engine — true when agent.toml selects the platform engine.
platform_engine() {
  grep -Eq '^[[:space:]]*engine[[:space:]]*=[[:space:]]*"platform"' "${AGENT_CONFIG:-$VERCELIFY_ETC/agent.toml}" 2>/dev/null
}

# platform_domain_json WILDCARD_URL EDGE_TLS — the body of
# PUT /v1/platform/settings/domain for `http(s)://domain` (or empty: none).
platform_domain_json() {
  local url=$1 edge=${2:-false} host https=false
  host=${url#*://}
  host=${host%/}
  [[ $url == https://* ]] && https=true
  jq -nc --arg w "$host" --argjson h "$https" --argjson e "$edge" \
    '{wildcard: (if $w == "" then null else $w end), https: $h, edge_tls: $e}'
}

# platform_set_domain WILDCARD_URL EDGE_TLS — new apps get <name>.<domain>.
platform_set_domain() {
  local body
  body=$(platform_domain_json "$1" "${2:-false}")
  [[ "${VERCELIFY_DRY_RUN:-0}" == 1 ]] && { info "[dry-run] would set the platform domain: $body"; return 0; }
  agent_api PUT /v1/platform/settings/domain "$body" >/dev/null
  ok "wildcard domain → ${1:-none}"
}

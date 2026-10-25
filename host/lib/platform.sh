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

# github-app [--org ORG] — create a GitHub App for this server (design §11):
# prints the page that takes the browser to GitHub.
platform_github_app() {
  local org="" out
  case "${1:-}" in
    "") ;;
    --org) org=${2:?--org needs an organization} ;;
    *) die "usage: vercelify github-app [--org ORG]" ;;
  esac
  platform_engine || die "GitHub Apps are part of the platform engine (agent.toml: engine = \"platform\")"
  out=$(agent_api POST /v1/platform/github-app "$(jq -nc --arg o "$org" 'if $o == "" then {} else {org: $o} end')")
  printf '\n  Open this in a browser (within 30 minutes), confirm on GitHub, and it comes back here:\n\n    %s\n\n' \
    "$(jq -r .start_url <<<"$out")" >&2
  info "then install the app on your repositories (GitHub shows the link) and pick it as their git source"
}

# webhook SOURCE — a webhook secret for a git source, and where to send events.
platform_webhook() {
  local src=${1:-} out url
  [[ -n "$src" ]] || die "usage: vercelify webhook SOURCE   (a git source's id or name)"
  out=$(agent_api POST "/v1/platform/git-sources/$src/webhook" '{}')
  url=$(jq -r .url <<<"$out")
  if [[ -n "$url" ]]; then
    printf '  Payload URL: %s\n' "$url" >&2
  else
    warn "no public address: providers outside your network can't reach this server (polling still deploys)"
    printf '  On your network: %s\n' "$(jq -r '.lan_urls[0] // "—"' <<<"$out")" >&2
  fi
  printf '  Secret:      %s   (shown once)\n  Content type: %s; events: %s\n' \
    "$(jq -r .secret <<<"$out")" "$(jq -r .content_type <<<"$out")" "$(jq -r '.events | join(", ")' <<<"$out")" >&2
}

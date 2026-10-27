#!/usr/bin/env bash
# Vercelify's platform engine: helpers for the CLI.
# The engine lives in the agent; the CLI talks to it through agent_api.

# platform_engine — true when this server runs Vercelify's engine, decided
# as the agent does: agent.toml's `engine`, `platform` unless set.
platform_engine() {
  local cfg=${AGENT_CONFIG:-$VERCELIFY_ETC/agent.toml} engine
  engine=$(sed -n -E 's/^[[:space:]]*engine[[:space:]]*=[[:space:]]*"([a-z]+)".*/\1/p' "$cfg" 2>/dev/null | head -1)
  [[ -z "$engine" || "$engine" == platform ]]
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

# git-source [list | add NAME [--provider P] [--deploy-key] | rm NAME] —
# credentials for private repositories. A token is read from stdin (or asked
# for), never from the command line; a deploy key's public half is printed.
platform_git_source() {
  local sub=${1:-list} name provider=github auth=token token='' out id
  (( $# )) && shift
  platform_engine || die "git sources are part of the platform engine, which is off on this server"
  case "$sub" in
    list|ls)
      agent_api GET /v1/platform/git-sources | jq -r '.[] | [.name, .provider, .auth, (if .webhook then "webhook" else "" end), .id] | @tsv' |
        while IFS=$'\t' read -r name provider auth hook id; do
          printf '  %-20s %-7s %-10s %-8s %s\n' "$name" "$provider" "$auth" "$hook" "$id"
        done
      ;;
    add)
      name=${1:-}
      [[ -n "$name" ]] || die "usage: vercelify git-source add NAME [--provider github|gitlab|gitea|generic] [--deploy-key]"
      shift
      while (( $# )); do
        case "$1" in
          --provider) provider=${2:?--provider needs github, gitlab, gitea or generic}; shift ;;
          --deploy-key) auth=deploy_key ;;
          *) die "usage: vercelify git-source add NAME [--provider github|gitlab|gitea|generic] [--deploy-key]" ;;
        esac
        shift
      done
      [[ "$provider" =~ ^(github|gitlab|gitea|generic)$ ]] || die "unknown provider '$provider' (github, gitlab, gitea or generic)"
      if [[ "$auth" == token ]]; then
        if [[ -t 0 ]]; then
          read -r -s -p "Access token for $name (read-only access to the repositories): " token
          echo >&2
        else
          IFS= read -r token || true
        fi
        [[ -n "$token" ]] || die "no token given (paste it, or pipe it in: … | sudo vercelify git-source add $name)"
      fi
      # (jq gets the token through its environment, not its arguments.)
      out=$(agent_api POST /v1/platform/git-sources "$(VFY_TOKEN=$token jq -nc --arg n "$name" --arg p "$provider" --arg a "$auth" \
        '{name: $n, provider: $p, auth: $a} + (if env.VFY_TOKEN == "" then {} else {token: env.VFY_TOKEN} end)')")
      ok "git source $name added — pick it as an app's credentials (New app, or the app's Git card)"
      if [[ "$auth" == deploy_key ]]; then
        printf '\n  Add this read-only deploy key to the repository:\n\n    %s\n\n' "$(jq -r .public_key <<<"$out")" >&2
      fi
      ;;
    rm|remove)
      name=${1:?usage: vercelify git-source rm NAME}
      id=$(agent_api GET /v1/platform/git-sources | jq -r --arg n "$name" '[.[] | select(.name == $n or .id == $n)][0].id // empty')
      [[ -n "$id" ]] || die "no git source named $name (see: sudo vercelify git-source)"
      agent_api DELETE "/v1/platform/git-sources/$id" >/dev/null
      ok "git source $name removed"
      ;;
    *) die "usage: vercelify git-source [list | add NAME [--provider P] [--deploy-key] | rm NAME]" ;;
  esac
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

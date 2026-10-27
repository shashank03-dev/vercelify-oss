#!/usr/bin/env bash
# Public domains without opening router ports: a Cloudflare Tunnel that
# sends *.DOMAIN (and DOMAIN) to Vercelify's proxy on localhost:80, plus the DNS
# records for it. Needs an API token with:
#   Account › Cloudflare Tunnel › Edit
#   Zone › DNS › Edit      and   Zone › Zone › Read   (for the domain's zone)

: "${CF_API_BASE:=https://api.cloudflare.com/client/v4}"
CF_TOKEN=${CF_TOKEN:-}

# cf METHOD PATH [JSON] — prints .result, fails with Cloudflare's message.
cf() {
  local method=$1 path=$2 body=${3:-} resp
  local args=(-sS -m 30 -X "$method" -H "Authorization: Bearer $CF_TOKEN" -H "Content-Type: application/json")
  [[ -n "$body" ]] && args+=(--data "$body")
  resp=$(curl "${args[@]}" "$CF_API_BASE$path") || { err "Cloudflare API unreachable ($method $path)"; return 1; }
  if [[ "$(jq -r '.success // false' <<<"$resp" 2>/dev/null)" != true ]]; then
    err "Cloudflare API $method $path: $(jq -r '[.errors[]?.message] | join("; ") // "unknown error"' <<<"$resp" 2>/dev/null || echo "$resp")"
    return 1
  fi
  jq -c '.result' <<<"$resp"
}

# Find the zone for DOMAIN (which may be a subdomain such as apps.example.com).
# Prints "<zone_id> <account_id> <zone_name>".
cf_find_zone() {
  local domain=$1 candidate=$1 res
  while [[ "$candidate" == *.* ]]; do
    res=$(cf GET "/zones?name=$candidate&status=active") || return 1
    if [[ "$(jq 'length' <<<"$res")" -gt 0 ]]; then
      jq -r '.[0] | "\(.id) \(.account.id) \(.name)"' <<<"$res"
      return 0
    fi
    candidate=${candidate#*.}
  done
  err "no active Cloudflare zone found for $domain — add the domain to Cloudflare first"
  return 1
}

# Create or reuse a remotely-managed tunnel. Prints its id.
cf_ensure_tunnel() {
  local account=$1 name=$2 res secret
  res=$(cf GET "/accounts/$account/cfd_tunnel?name=$name&is_deleted=false") || return 1
  if [[ "$(jq 'length' <<<"$res")" -gt 0 ]]; then
    jq -r '.[0].id' <<<"$res"
    return 0
  fi
  secret=$(head -c 32 /dev/urandom | base64 | tr -d '\n')
  res=$(cf POST "/accounts/$account/cfd_tunnel" \
    "$(jq -nc --arg n "$name" --arg s "$secret" '{name: $n, config_src: "cloudflare", tunnel_secret: $s}')") || return 1
  jq -r '.id' <<<"$res"
}

cf_tunnel_token() { cf GET "/accounts/$1/cfd_tunnel/$2/token" | jq -r '.'; }

# The coordinator of the mesh (design §13.7) is reachable through the
# tunnel at mesh.DOMAIN: joins and the relay from outside the LAN.
mesh_coordinates() {
  grep -Eq '^[[:space:]]*mesh_coordinator[[:space:]]*=[[:space:]]*true' "${VERCELIFY_ETC:-/etc/vercelify}/agent.toml" 2>/dev/null
}

# Route *.DOMAIN and DOMAIN to Vercelify's proxy, and mesh.DOMAIN to the
# mesh's control listener on a coordinator. Apps on other servers are reached
# by the proxy itself over the mesh, so the tunnel never needs per-app routes.
cf_configure_ingress() {
  local account=$1 tunnel=$2 domain=$3 mesh=false body
  mesh_coordinates && mesh=true
  body=$(jq -nc --arg d "$domain" --argjson m "$mesh" '{config: {ingress: (
    (if $m then [{hostname: ("mesh." + $d), service: "http://localhost:7443", originRequest: {}}] else [] end) + [
    {hostname: ("*." + $d), service: "http://localhost:80", originRequest: {}},
    {hostname: $d, service: "http://localhost:80", originRequest: {}},
    {service: "http_status:404"}
  ])}}')
  cf PUT "/accounts/$account/cfd_tunnel/$tunnel/configurations" "$body" >/dev/null
}

# Reapply the tunnel's routes from the stored token (after this server
# starts or stops coordinating the mesh).
routes_sync() {
  local env="$VERCELIFY_ETC/cloudflare.env"
  [[ -r "$env" ]] || { info "no Cloudflare tunnel with a stored token — nothing to update"; return 0; }
  # shellcheck disable=SC1090
  . "$env"
  CF_TOKEN=$CF_API_TOKEN
  cf_configure_ingress "$CF_ACCOUNT" "$CF_TUNNEL" "$CF_DOMAIN" || return 1
  ok "tunnel routes: *.$CF_DOMAIN and $CF_DOMAIN → this server's proxy"
}

# Create/update a proxied CNAME NAME -> TARGET. Only A/AAAA/CNAME records
# conflict with a CNAME; MX/TXT/etc. (mail!) are never touched. Existing
# A/AAAA records are left alone unless FORCE=1, so an existing website keeps
# working.
cf_upsert_cname() {
  local zone=$1 name=$2 target=$3 force=${4:-0} res conflicts type id body
  body=$(jq -nc --arg n "$name" --arg t "$target" \
    '{type: "CNAME", name: $n, content: $t, proxied: true, ttl: 1, comment: "managed by vercelify"}')
  res=$(cf GET "/zones/$zone/dns_records?name=$name") || return 1
  conflicts=$(jq -c '[.[] | select(.type == "A" or .type == "AAAA" or .type == "CNAME")]' <<<"$res")
  if [[ "$(jq 'length' <<<"$conflicts")" -eq 0 ]]; then
    cf POST "/zones/$zone/dns_records" "$body" >/dev/null && ok "DNS $name → tunnel (created)"
    return
  fi
  type=$(jq -r '.[0].type' <<<"$conflicts")
  id=$(jq -r '.[0].id' <<<"$conflicts")
  if [[ "$type" == CNAME ]]; then
    cf PUT "/zones/$zone/dns_records/$id" "$body" >/dev/null && ok "DNS $name → tunnel (updated)"
  elif [[ "$force" == 1 ]]; then
    local rid
    for rid in $(jq -r '.[].id' <<<"$conflicts"); do cf DELETE "/zones/$zone/dns_records/$rid" >/dev/null || return 1; done
    cf POST "/zones/$zone/dns_records" "$body" >/dev/null && ok "DNS $name → tunnel (replaced $type)"
  else
    warn "DNS $name already has an $type record — left untouched (use --force-dns to replace it)"
  fi
}

install_cloudflared() {
  have cloudflared && return 0
  run_cmd mkdir -p --mode=0755 /usr/share/keyrings
  fetch https://pkg.cloudflare.com/cloudflare-main.gpg /usr/share/keyrings/cloudflare-main.gpg || die "could not fetch Cloudflare's apt key"
  echo 'deb [signed-by=/usr/share/keyrings/cloudflare-main.gpg] https://pkg.cloudflare.com/cloudflared any main' |
    write_file /etc/apt/sources.list.d/cloudflared.list 0644 || true
  _APT_UPDATED='' apt_update_once
  apt_install cloudflared
}

# Run the tunnel as a hardened systemd service reading its token from a file.
cloudflared_service() {
  local token=$1
  printf 'TUNNEL_TOKEN=%s\n' "$token" | write_file "$VERCELIFY_ETC/cloudflared.env" 0600 || true
  write_file "$SYSTEMD_DIR/cloudflared.service" 0644 <<'EOF' || true
[Unit]
Description=Cloudflare Tunnel (Vercelify public ingress)
After=network-online.target docker.service
Wants=network-online.target

[Service]
EnvironmentFile=/etc/vercelify/cloudflared.env
ExecStart=/usr/bin/cloudflared --no-autoupdate tunnel run
Restart=always
RestartSec=5
DynamicUser=yes
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
EOF
  run_cmd systemctl daemon-reload
  run_cmd systemctl enable --now cloudflared >/dev/null 2>&1
  run_cmd systemctl restart cloudflared
}

# setup_cloudflare_domain DOMAIN TOKEN TUNNEL_NAME [FORCE_DNS]
setup_cloudflare_domain() {
  local domain=$1 tunnel_name=$3 force=${4:-0} zone account zone_name tunnel token
  CF_TOKEN=$2
  step "Public domain via Cloudflare Tunnel ($domain)"
  [[ -n "$CF_TOKEN" ]] || die "a Cloudflare API token is required (--cloudflare-token)"
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] && { info "[dry-run] would create tunnel $tunnel_name for *.$domain"; return 0; }

  read -r zone account zone_name < <(cf_find_zone "$domain") || die "Cloudflare zone lookup failed"
  info "zone $zone_name (account ${account:0:8}…)"
  tunnel=$(cf_ensure_tunnel "$account" "$tunnel_name") || die "could not create the tunnel"
  ok "tunnel $tunnel_name ($tunnel)"
  cf_configure_ingress "$account" "$tunnel" "$domain" || die "could not configure tunnel routes"
  ok "routes: *.$domain and $domain → the proxy (localhost:80)"
  cf_upsert_cname "$zone" "*.$domain" "$tunnel.cfargotunnel.com" "$force" || die "DNS update failed"
  cf_upsert_cname "$zone" "$domain" "$tunnel.cfargotunnel.com" "$force" || warn "apex record not changed"
  token=$(cf_tunnel_token "$account" "$tunnel") || die "could not fetch the tunnel token"
  [[ -n "$token" && "$token" != null ]] || die "empty tunnel token"
  install_cloudflared
  cloudflared_service "$token"
  # Kept (0600) so the routes can be reapplied (mesh coordinator changes).
  printf 'CF_API_TOKEN=%s\nCF_ACCOUNT=%s\nCF_TUNNEL=%s\nCF_DOMAIN=%s\n' "$CF_TOKEN" "$account" "$tunnel" "$domain" |
    write_file "$VERCELIFY_ETC/cloudflare.env" 0600 || true
  # The Coolify-era timer that followed apps on pooled servers.
  if [[ -e "$SYSTEMD_DIR/vercelify-routes.timer" ]]; then
    run_cmd systemctl disable --now vercelify-routes.timer >/dev/null 2>&1 || true
    run_cmd rm -f "$SYSTEMD_DIR/vercelify-routes.timer" "$SYSTEMD_DIR/vercelify-routes.service"
    run_cmd systemctl daemon-reload
  fi
  ok "cloudflared running — https://anything.$domain now reaches this server's apps (and those on other servers)"
}

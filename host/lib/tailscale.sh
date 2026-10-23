#!/usr/bin/env bash
# Tailscale: install, join the tailnet, report the address.

ts_state() { tailscale status --json 2>/dev/null | jq -r '.BackendState // "Unknown"' 2>/dev/null || echo Unknown; }
ts_ipv4() { tailscale ip -4 2>/dev/null | head -1; }
ts_dns_name() { tailscale status --json 2>/dev/null | jq -r '.Self.DNSName // ""' | sed 's/\.$//'; }

# install_tailscale HOSTNAME [AUTHKEY]
install_tailscale() {
  local hostname=$1 authkey=${2:-}
  step "Tailscale mesh VPN"
  if ! have tailscale; then
    local tmp
    tmp=$(mktemp)
    fetch https://tailscale.com/install.sh "$tmp" || die "could not download the Tailscale installer"
    run_cmd env -u DRY_RUN sh "$tmp" >>"$VERCELIFY_LOG" 2>&1 || die "Tailscale installation failed (see $VERCELIFY_LOG)"
    [[ "$VERCELIFY_DRY_RUN" == 1 ]] || have tailscale || die "the Tailscale installer finished but tailscale is missing (see $VERCELIFY_LOG)"
    rm -f "$tmp"
  fi
  run_cmd systemctl enable --now tailscaled >/dev/null 2>&1 || true
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] && return 0

  if [[ "$(ts_state)" == Running ]]; then
    ok "already connected as $(ts_dns_name) ($(ts_ipv4))"
    return 0
  fi

  local args=(up --hostname="$hostname" --accept-dns=true)
  if [[ -n "$authkey" ]]; then
    run_cmd tailscale "${args[@]}" --authkey="$authkey" || die "tailscale up failed (is the auth key valid?)"
  elif has_tty; then
    info "Open the link below on any device to add this machine to your tailnet:"
    # `tailscale up` prints the login URL and blocks until it is approved.
    run_cmd tailscale "${args[@]}" --timeout=10m || die "Tailscale login did not complete"
  else
    warn "not logged in — run: sudo tailscale up --hostname=$hostname"
    return 0
  fi
  ok "connected as $(ts_dns_name) ($(ts_ipv4))"
  info "tip: in the Tailscale admin console, choose 'Disable key expiry' for this machine"
}

#!/usr/bin/env bash
# `vercelify remote`: reach the main server from anywhere, with no Cloudflare
# tunnel and no manual port forwarding. The router is asked (UPnP IGD, else
# NAT-PMP) to forward two ports to this machine: the mesh's WireGuard port
# (UDP) and its control port (TCP). The public address then goes into
# agent.toml, so invites and the mesh map carry it. A timer renews the
# lease and follows a public address that changes.
#
# Nothing else is opened: WireGuard answers only peers it already knows, and
# the control port only takes sealed, single-use, rate-limited joins.

: "${REMOTE_ENV:=$VERCELIFY_ETC/remote.env}"
: "${REMOTE_LEASE_SECS:=3600}"
: "${UPNPC:=upnpc}"
: "${NATPMPC:=natpmpc}"
: "${AGENT_CONFIG:=$VERCELIFY_ETC/agent.toml}"

_remote_toml() { grep -E "^[[:space:]]*$1[[:space:]]*=" "$AGENT_CONFIG" 2>/dev/null | head -1 | sed -E 's/^[^=]*=[[:space:]]*//; s/^"//; s/"$//'; }

# The forwarded ports: "udp PORT" and "tcp PORT".
remote_ports() {
  local mesh control
  mesh=$(_remote_toml mesh_port)
  control=$(_remote_toml mesh_control_listen)
  control=${control##*:}
  printf 'udp %s\ntcp %s\n' "${mesh:-51820}" "${control:-7443}"
}

# This machine's address on the LAN (the route to the internet).
remote_lan_ip() {
  ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -1
}

# remote_public_ip IP — a public IPv4: not private, not CGNAT, not loopback.
remote_public_ip() {
  local ip=$1 a b
  [[ "$ip" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.[0-9]{1,3}\.[0-9]{1,3}$ ]] || return 1
  a=${BASH_REMATCH[1]} b=${BASH_REMATCH[2]}
  (( a == 10 || a == 127 || a == 0 || a >= 224 )) && return 1
  (( a == 172 && b >= 16 && b <= 31 )) && return 1
  (( a == 192 && b == 168 )) && return 1
  (( a == 169 && b == 254 )) && return 1
  (( a == 100 && b >= 64 && b <= 127 )) && return 1
  return 0
}

# Map every port with UPnP; print the router's external address.
remote_map_upnp() {
  local lan=$1 proto port
  have "$UPNPC" || return 1
  while read -r proto port; do
    "$UPNPC" -e vercelify -a "$lan" "$port" "$port" "${proto^^}" "$REMOTE_LEASE_SECS" >/dev/null 2>&1 || return 1
  done < <(remote_ports)
  "$UPNPC" -s 2>/dev/null | sed -n 's/^ExternalIPAddress = //p' | head -1
}

# The same with NAT-PMP (Apple routers, many others).
remote_map_natpmp() {
  local proto port out
  have "$NATPMPC" || return 1
  while read -r proto port; do
    out=$("$NATPMPC" -a "$port" "$port" "$proto" "$REMOTE_LEASE_SECS" 2>&1) || return 1
    [[ "$out" == *"Mapped public port"* ]] || return 1
  done < <(remote_ports)
  printf '%s\n' "$out" | sed -n 's/.*[Pp]ublic IP address : \([0-9.]*\).*/\1/p' | head -1
}

remote_unmap() {
  local proto port
  while read -r proto port; do
    have "$UPNPC" && "$UPNPC" -d "$port" "${proto^^}" >/dev/null 2>&1
    have "$NATPMPC" && "$NATPMPC" -a "$port" 0 "$proto" 0 >/dev/null 2>&1
  done < <(remote_ports)
  return 0
}

# Ask the router, record the public address in agent.toml (restarting the
# agent only when it changed). Fails with the reason.
remote_apply() {
  local lan ip method mesh control url changed=0
  lan=$(remote_lan_ip)
  [[ -n "$lan" ]] || { err "no LAN address (is this machine online?)"; return 1; }
  if ip=$(remote_map_upnp "$lan") && [[ -n "$ip" ]]; then method=upnp
  elif ip=$(remote_map_natpmp) && [[ -n "$ip" ]]; then method=natpmp
  else
    err "the router didn't open the ports: turn on UPnP (or NAT-PMP) in its settings, or forward $(remote_ports | tr '\n' ' ')to $lan by hand"
    return 1
  fi
  if ! remote_public_ip "$ip"; then
    remote_unmap
    err "the router's own address is $ip, not a public one: your ISP shares it (CGNAT), so nothing can be opened. Use a Cloudflare domain instead: sudo vercelify domain DOMAIN --cloudflare-token TOKEN"
    return 1
  fi
  read -r _ mesh < <(remote_ports | sed -n 1p)
  read -r _ control < <(remote_ports | sed -n 2p)
  if [[ "$(_remote_toml mesh_public_endpoints)" != "[\"$ip:$mesh\"]" ]]; then
    toml_set mesh_public_endpoints "[\"$ip:$mesh\"]" "$AGENT_CONFIG"; changed=1
  fi
  # A Cloudflare tunnel (https://mesh.DOMAIN) stays the control address.
  url=$(_remote_toml mesh_public_url)
  if [[ "$url" != https://* && "$url" != "http://$ip:$control" ]]; then
    toml_set mesh_public_url "\"http://$ip:$control\"" "$AGENT_CONFIG"; changed=1
  fi
  printf 'method=%s\npublic_ip=%s\nlan_ip=%s\nupdated_at=%s\n' "$method" "$ip" "$lan" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" |
    write_file "$REMOTE_ENV" 0644 || true
  if (( changed )); then
    run_cmd systemctl restart vercelify-agent 2>/dev/null || true
  fi
  ok "reachable from anywhere at $ip ($method; UDP $mesh and TCP $control forwarded to $lan)"
}

remote_on() {
  [[ $ROLE == main ]] || die "remote access is for the main server: workers and devices reach it through the mesh"
  apt_install miniupnpc natpmpc || warn "could not install miniupnpc/natpmpc"
  remote_apply || return 1
  write_file "$SYSTEMD_DIR/vercelify-remote.service" 0644 <<'EOF' || true
[Unit]
Description=Keep the router's port forwards for Vercelify's network
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/vercelify remote refresh
EOF
  write_file "$SYSTEMD_DIR/vercelify-remote.timer" 0644 <<'EOF' || true
[Unit]
Description=Renew the router's port forwards (and follow a new public IP)

[Timer]
OnBootSec=1min
OnUnitActiveSec=20min

[Install]
WantedBy=timers.target
EOF
  run_cmd systemctl daemon-reload
  run_cmd systemctl enable --now vercelify-remote.timer >/dev/null 2>&1 || true
}

remote_off() {
  local ip
  ip=$(sed -n 's/^public_ip=//p' "$REMOTE_ENV" 2>/dev/null)
  run_cmd systemctl disable --now vercelify-remote.timer >/dev/null 2>&1 || true
  run_cmd rm -f "$SYSTEMD_DIR/vercelify-remote.timer" "$SYSTEMD_DIR/vercelify-remote.service" "$REMOTE_ENV"
  remote_unmap
  # Only what `remote on` wrote: a hand-set or Cloudflare address stays.
  if [[ -n "$ip" && -f "$AGENT_CONFIG" ]]; then
    sed -i -E "/^[[:space:]]*mesh_public_endpoints[[:space:]]*=[[:space:]]*\[\"$ip:[0-9]+\"\]/d; /^[[:space:]]*mesh_public_url[[:space:]]*=[[:space:]]*\"http:\/\/$ip:[0-9]+\"/d" "$AGENT_CONFIG"
    run_cmd systemctl restart vercelify-agent 2>/dev/null || true
  fi
  ok "remote access off: the router forwards nothing for Vercelify"
}

remote_status() {
  if [[ -r "$REMOTE_ENV" ]]; then
    printf '  on (%s): public %s, last renewed %s\n' "$(sed -n 's/^method=//p' "$REMOTE_ENV")" \
      "$(sed -n 's/^public_ip=//p' "$REMOTE_ENV")" "$(sed -n 's/^updated_at=//p' "$REMOTE_ENV")"
  else
    printf '  off — on the LAN only (or through a Cloudflare domain). Turn on: sudo vercelify remote on\n'
  fi
}

remote_cli() {
  case "${1:-status}" in
    on) remote_on ;;
    off) remote_off ;;
    refresh) [[ -r "$REMOTE_ENV" ]] || exit 0; remote_apply ;;
    status) remote_status ;;
    *) die "usage: vercelify remote [status|on|off]" ;;
  esac
}

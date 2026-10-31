#!/usr/bin/env bash
# `vercelify remote`: reach the main server from anywhere, with nothing to set
# up. Two ways, the first that works:
#
#  1. Direct: the router is asked (UPnP IGD, else NAT-PMP) to forward two
#     ports to this machine, the mesh's WireGuard port (UDP) and its control
#     port (TCP). The public address goes into agent.toml.
#  2. A Cloudflare quick tunnel (no account, no domain) to the control port,
#     when the router says no or the connection is behind CGNAT. Joins and
#     the relay go through https://<random>.trycloudflare.com; machines still
#     connect directly to each other whenever their networks allow.
#
# A timer renews the forwards and follows a new public address or tunnel.
# Each change is signed by the agent (the mesh's coordinator key) and posted
# to a public message drop, where machines that lost this server look.
#
# Nothing else is opened: WireGuard answers only peers it already knows, and
# the control port only takes sealed, single-use, rate-limited joins.

: "${REMOTE_ENV:=$VERCELIFY_ETC/remote.env}"
: "${REMOTE_LEASE_SECS:=3600}"
: "${UPNPC:=upnpc}"
: "${NATPMPC:=natpmpc}"
: "${AGENT_CONFIG:=$VERCELIFY_ETC/agent.toml}"
: "${MESH_DIR:=/var/lib/vercelify/mesh}"
: "${QUICK_TUNNEL_METRICS:=127.0.0.1:20241}"
: "${QUICK_TUNNEL_WAIT:=40}"
: "${MESH_BEACON_URL:=https://ntfy.sh}"
: "${BEACON_WAIT:=20}"

# A key's value, or nothing (never a failure: the CLI runs with pipefail).
_remote_toml() { { grep -E "^[[:space:]]*$1[[:space:]]*=" "$AGENT_CONFIG" 2>/dev/null || true; } | head -1 | sed -E 's/^[^=]*=[[:space:]]*//; s/^"//; s/"$//'; }
_remote_toml_del() { [[ ! -f "$AGENT_CONFIG" ]] || sed -i -E "/^[[:space:]]*$1[[:space:]]*=/d" "$AGENT_CONFIG"; }

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

# A Cloudflare domain's tunnel (https://mesh.DOMAIN), set up by
# `vercelify domain`: it already reaches the control port from anywhere.
_remote_named_tunnel() { [[ "$1" == https://* && "$1" != *.trycloudflare.com* ]]; }

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
    # No router (or no client) is fine here: nothing to remove then.
    { have "$UPNPC" && "$UPNPC" -d "$port" "${proto^^}" >/dev/null 2>&1; } || true
    { have "$NATPMPC" && "$NATPMPC" -a "$port" 0 "$proto" 0 >/dev/null 2>&1; } || true
  done < <(remote_ports)
  return 0
}

# The quick tunnel's service: cloudflared with no account, to the control
# port on loopback. Its address is on cloudflared's metrics endpoint.
remote_tunnel_start() {
  local control=$1 unit="$SYSTEMD_DIR/vercelify-quicktunnel.service" want
  install_cloudflared || return 1
  want="[Unit]
Description=Vercelify: a Cloudflare quick tunnel to the private network's control port
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/bin/cloudflared tunnel --no-autoupdate --metrics $QUICK_TUNNEL_METRICS --url http://127.0.0.1:$control
Restart=always
RestartSec=5
DynamicUser=yes
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes

[Install]
WantedBy=multi-user.target"
  # A changed unit (another port, an upgrade) needs a restart: enable --now
  # leaves a running tunnel as it is.
  if [[ "$(cat "$unit" 2>/dev/null)" != "$want" ]]; then
    printf '%s\n' "$want" | write_file "$unit" 0644 || true
    run_cmd systemctl daemon-reload
    run_cmd systemctl enable vercelify-quicktunnel >/dev/null 2>&1 || true
    run_cmd systemctl restart vercelify-quicktunnel >/dev/null 2>&1 || return 1
  else
    run_cmd systemctl enable --now vercelify-quicktunnel >/dev/null 2>&1 || return 1
  fi
}

remote_tunnel_stop() {
  [[ -e "$SYSTEMD_DIR/vercelify-quicktunnel.service" ]] || return 0
  run_cmd systemctl disable --now vercelify-quicktunnel >/dev/null 2>&1 || true
  run_cmd rm -f "$SYSTEMD_DIR/vercelify-quicktunnel.service"
  run_cmd systemctl daemon-reload
}

# The tunnel's hostname, once cloudflared has one and is connected to
# Cloudflare (the name comes first; until /ready, it answers 530).
remote_tunnel_host() {
  local i host=''
  for ((i = 0; i < QUICK_TUNNEL_WAIT; i++)); do
    [[ -n "$host" ]] || host=$(curl -fsS -m 3 "http://$QUICK_TUNNEL_METRICS/quicktunnel" 2>/dev/null | jq -r '.hostname // empty' 2>/dev/null)
    if [[ "$host" =~ ^[a-z0-9-]+\.trycloudflare\.com$ ]] && curl -fsS -m 3 -o /dev/null "http://$QUICK_TUNNEL_METRICS/ready" 2>/dev/null; then
      printf '%s\n' "$host"
      return 0
    fi
    sleep 1
  done
  return 1
}

# Post the agent's signed note (mesh_dir/beacon: "<topic> <note>") to the
# public drop. Notes newer than `since` only: the agent rewrites it on start.
remote_post_beacon() {
  local since=${1:-0} f="$MESH_DIR/beacon" topic note i
  for ((i = 0; i < BEACON_WAIT; i++)); do
    [[ -s "$f" ]] && (( $(stat -c %Y "$f" 2>/dev/null || echo 0) >= since )) && break
    sleep 1
  done
  [[ -s "$f" ]] || return 0
  read -r topic note <"$f" || true
  [[ "$topic" =~ ^vfy[0-9a-f]{40}$ && "$note" == vfyb1.* ]] || return 0
  curl -fsS -m 15 -o /dev/null -d "$note" "$MESH_BEACON_URL/$topic" 2>/dev/null ||
    warn "could not post this server's address to $MESH_BEACON_URL (machines away from home find it once it works)"
}

_remote_record() {
  printf 'method=%s\npublic_ip=%s\npublic_url=%s\nlan_ip=%s\nupdated_at=%s\n' "$1" "$2" "$3" "$4" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" |
    write_file "$REMOTE_ENV" 0644 || true
}

# Restart the agent when its address changed (it signs a new note on start),
# then post the note.
_remote_settle() {
  local changed=$1 since
  since=$(date +%s)
  if (( changed )); then
    run_cmd systemctl restart vercelify-agent 2>/dev/null || true
  else
    since=0
  fi
  remote_post_beacon "$since"
}

# Directly, else through a quick tunnel; record the address in agent.toml.
# Fails with the reason.
remote_apply() {
  local lan ip='' method='' mesh control url changed=0 host
  lan=$(remote_lan_ip)
  [[ -n "$lan" ]] || { err "no LAN address (is this machine online?)"; return 1; }
  read -r _ mesh < <(remote_ports | sed -n 1p)
  read -r _ control < <(remote_ports | sed -n 2p)
  url=$(_remote_toml mesh_public_url)
  if ip=$(remote_map_upnp "$lan") && [[ -n "$ip" ]]; then method=upnp
  elif ip=$(remote_map_natpmp) && [[ -n "$ip" ]]; then method=natpmp
  fi
  if [[ -n "$method" ]] && ! remote_public_ip "$ip"; then
    remote_unmap
    info "the router's own address is $ip, not a public one: your ISP shares it (CGNAT)"
    method=''
  fi

  if [[ -n "$method" ]]; then
    remote_tunnel_stop
    if [[ "$(_remote_toml mesh_public_endpoints)" != "[\"$ip:$mesh\"]" ]]; then
      toml_set mesh_public_endpoints "[\"$ip:$mesh\"]" "$AGENT_CONFIG"; changed=1
    fi
    # A Cloudflare domain's tunnel stays the control address.
    if ! _remote_named_tunnel "$url" && [[ "$url" != "http://$ip:$control" ]]; then
      toml_set mesh_public_url "\"http://$ip:$control\"" "$AGENT_CONFIG"; changed=1
    fi
    _remote_record "$method" "$ip" "" "$lan"
    _remote_settle "$changed"
    ok "reachable from anywhere at $ip ($method; UDP $mesh and TCP $control forwarded to $lan)"
    return 0
  fi

  # The router didn't open anything: what `remote on` wrote for a direct
  # forward is wrong now.
  case "$(sed -n 's/^method=//p' "$REMOTE_ENV" 2>/dev/null)" in
    upnp | natpmp) [[ -n "$(_remote_toml mesh_public_endpoints)" ]] && { _remote_toml_del mesh_public_endpoints; changed=1; } ;;
  esac
  if _remote_named_tunnel "$url"; then
    remote_tunnel_stop
    _remote_record cloudflare "" "$url" "$lan"
    _remote_settle "$changed"
    ok "reachable from anywhere through your Cloudflare domain ($url)"
    return 0
  fi
  info "the router didn't open the ports: using a Cloudflare quick tunnel (no account needed)"
  if ! remote_tunnel_start "$control" || ! host=$(remote_tunnel_host); then
    err "the Cloudflare quick tunnel didn't start (journalctl -u vercelify-quicktunnel). Or turn on UPnP in your router, or forward $(remote_ports | tr '\n' ' ')to $lan by hand"
    (( changed )) && run_cmd systemctl restart vercelify-agent 2>/dev/null
    return 1
  fi
  if [[ "$url" != "https://$host" ]]; then
    toml_set mesh_public_url "\"https://$host\"" "$AGENT_CONFIG"; changed=1
  fi
  _remote_record tunnel "" "https://$host" "$lan"
  _remote_settle "$changed"
  ok "reachable from anywhere at https://$host (a Cloudflare quick tunnel; machines connect directly when they can)"
}

remote_on() {
  [[ $ROLE == main ]] || die "remote access is for the main server: workers and devices reach it through the mesh"
  apt_install miniupnpc natpmpc || warn "could not install miniupnpc/natpmpc"
  remote_apply || return 1
  write_file "$SYSTEMD_DIR/vercelify-remote.service" 0644 <<'EOF' || true
[Unit]
Description=Keep Vercelify's network reachable from anywhere (port forwards or tunnel)
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/vercelify remote refresh
EOF
  write_file "$SYSTEMD_DIR/vercelify-remote.timer" 0644 <<'EOF' || true
[Unit]
Description=Renew the port forwards, follow a new address, post it for away machines

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
  local ip url
  ip=$(sed -n 's/^public_ip=//p' "$REMOTE_ENV" 2>/dev/null)
  url=$(_remote_toml mesh_public_url)
  run_cmd systemctl disable --now vercelify-remote.timer >/dev/null 2>&1 || true
  run_cmd rm -f "$SYSTEMD_DIR/vercelify-remote.timer" "$SYSTEMD_DIR/vercelify-remote.service" "$REMOTE_ENV"
  remote_tunnel_stop
  remote_unmap
  # Only what `remote on` wrote: a hand-set or Cloudflare domain address stays.
  if [[ -f "$AGENT_CONFIG" ]]; then
    [[ -n "$ip" ]] && sed -i -E "/^[[:space:]]*mesh_public_endpoints[[:space:]]*=[[:space:]]*\[\"$ip:[0-9]+\"\]/d; /^[[:space:]]*mesh_public_url[[:space:]]*=[[:space:]]*\"http:\/\/$ip:[0-9]+\"/d" "$AGENT_CONFIG"
    [[ "$url" == *.trycloudflare.com* ]] && _remote_toml_del mesh_public_url
    run_cmd systemctl restart vercelify-agent 2>/dev/null || true
  fi
  ok "remote access off: nothing forwarded or tunnelled for Vercelify"
}

remote_status() {
  local method
  if [[ -r "$REMOTE_ENV" ]]; then
    method=$(sed -n 's/^method=//p' "$REMOTE_ENV")
    case "$method" in
      tunnel | cloudflare) printf '  on (%s): %s, last checked %s\n' "$method" "$(sed -n 's/^public_url=//p' "$REMOTE_ENV")" "$(sed -n 's/^updated_at=//p' "$REMOTE_ENV")" ;;
      *) printf '  on (%s): public %s, last renewed %s\n' "$method" "$(sed -n 's/^public_ip=//p' "$REMOTE_ENV")" "$(sed -n 's/^updated_at=//p' "$REMOTE_ENV")" ;;
    esac
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

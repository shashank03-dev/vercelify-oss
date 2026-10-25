#!/usr/bin/env bash
# Vercelify's own private network (design §13, instead of Tailscale):
# `vercelify mesh …`. The coordinator lives in the agent of the main server;
# `vercelify-mesh` (a separate daemon, so the network survives agent
# restarts) runs WireGuard on vfy0 on every node.

: "${MESH_BIN:=/usr/local/bin/vercelify-mesh}"
: "${MESH_DIR:=/var/lib/vercelify/mesh}"
: "${MESH_RUN_DIR:=/run/vercelify}"

# toml_set KEY VALUE FILE — set a top-level `key = value` (added when
# missing, a commented-out example left alone). VALUE is TOML (quote strings).
toml_set() {
  local key=$1 value=$2 file=$3 tmp
  tmp=$(mktemp)
  if grep -Eq "^[[:space:]]*${key}[[:space:]]*=" "$file" 2>/dev/null; then
    awk -v k="$key" -v v="$value" '
      $0 ~ "^[[:space:]]*" k "[[:space:]]*=" && !done { print k " = " v; done = 1; next } { print }' "$file" >"$tmp"
  else
    # Top-level keys go before the first [table], if any.
    awk -v line="$key = $value" '
      /^[[:space:]]*\[/ && !done { print line; done = 1 } { print } END { if (!done) print line }' "$file" >"$tmp"
  fi
  cat "$tmp" >"$file"
  rm -f "$tmp"
}

# toml_list_add KEY ITEM FILE — add "ITEM" to a top-level string list
# (`listen = ["tailscale", "127.0.0.1"]`) unless it is there.
toml_list_add() {
  local key=$1 item=$2 file=$3 cur
  cur=$(grep -E "^[[:space:]]*${key}[[:space:]]*=" "$file" 2>/dev/null | head -1 | sed -E 's/^[^=]*=[[:space:]]*//')
  if [[ -z "$cur" ]]; then
    toml_set "$key" "[\"$item\"]" "$file"
    return
  fi
  [[ "$cur" == *"\"$item\""* ]] && return 0
  if [[ "$cur" =~ ^\[[[:space:]]*\]$ ]]; then
    toml_set "$key" "[\"$item\"]" "$file"
  else
    toml_set "$key" "${cur%]}, \"$item\"]" "$file"
  fi
}

mesh_install_unit() {
  write_file "$SYSTEMD_DIR/vercelify-mesh.service" 0644 <"$VERCELIFY_SRC/host/systemd/vercelify-mesh.service" || true
  run_cmd systemctl daemon-reload
  run_cmd systemctl enable vercelify-mesh >/dev/null 2>&1
  run_cmd systemctl restart vercelify-mesh
}

mesh_wait_up() { # IP
  [[ "${VERCELIFY_DRY_RUN:-0}" == 1 ]] && return 0
  for _ in $(seq 1 30); do
    ip -4 addr show vfy0 2>/dev/null | grep -q "inet $1/" && return 0
    sleep 1
  done
  warn "vfy0 isn't up yet — see: journalctl -u vercelify-mesh -n 50"
  return 1
}

# mesh init — make this main server the coordinator.
mesh_init() {
  [[ "$ROLE" == main ]] || die "the coordinator runs on the main server; on this one: sudo vercelify mesh join <token>"
  [[ -x "$MESH_BIN" ]] || die "$MESH_BIN is missing (re-run the installer)"
  local cfg="$VERCELIFY_ETC/agent.toml"
  toml_set mesh_coordinator true "$cfg"
  toml_list_add listen mesh "$cfg"
  run_cmd systemctl restart vercelify-agent
  for _ in $(seq 1 20); do [[ -s "$MESH_DIR/state.json" ]] && break; sleep 1; done
  [[ -s "$MESH_DIR/state.json" ]] || die "the agent didn't set up the coordinator — see: journalctl -u vercelify-agent -n 50"
  mesh_install_unit
  mesh_wait_up 10.77.0.1 && ok "this server coordinates the mesh as 10.77.0.1"
  info "add a machine: sudo vercelify mesh invite   (then run what it prints on that machine)"
}

# mesh invite [--device] [--role ROLE] [--ttl MINUTES] — a single-use join token.
mesh_invite() {
  local kind=server role=owner ttl=60 out
  while (( $# )); do
    case "$1" in
      --device) kind=device ;;
      --role) role=${2:?--role needs owner, deployer or viewer}; shift ;;
      --ttl) ttl=${2:?--ttl needs minutes}; shift ;;
      *) die "usage: vercelify mesh invite [--device] [--role ROLE] [--ttl MINUTES]" ;;
    esac
    shift
  done
  [[ "$ttl" =~ ^[0-9]+$ ]] || die "--ttl must be a number of minutes"
  out=$(agent_api POST /v1/mesh/invites "$(jq -nc --arg k "$kind" --arg r "$role" --argjson t "$ttl" '{kind: $k, role: $r, ttl_minutes: $t}')")
  printf 'On the new machine (valid until %s, once):\n\n  sudo vercelify mesh join %s\n\n' \
    "$(jq -r .expires_at <<<"$out")" "$(jq -r .token <<<"$out")"
}

# mesh join TOKEN [--name NAME] — put this machine on the mesh.
mesh_join() {
  local token=${1:-} name=() cfg="$VERCELIFY_ETC/agent.toml" ip
  [[ "$token" == vfyj1.* ]] || die "usage: vercelify mesh join vfyj1.… [--name NAME]  (from: sudo vercelify mesh invite)"
  shift
  if [[ "${1:-}" == --name ]]; then name=(--name "${2:?--name needs a name}"); fi
  [[ -x "$MESH_BIN" ]] || die "$MESH_BIN is missing (re-run the installer)"
  run_cmd "$MESH_BIN" --dir "$MESH_DIR" join "$token" "${name[@]}"
  [[ -f "$cfg" ]] && toml_list_add listen mesh "$cfg"
  mesh_install_unit
  ip=$(jq -r .ipv4 "$MESH_DIR/state.json" 2>/dev/null || true)
  [[ -n "$ip" ]] && mesh_wait_up "$ip" && ok "on the mesh as $ip"
  run_cmd systemctl restart vercelify-agent 2>/dev/null || true
}

mesh_nodes() {
  agent_api GET /v1/mesh/nodes | jq -r '.[] |
    [(if .revoked_at then "revoked" elif .coordinator then "coordinator" else .kind end), .name, .ipv4,
     (.endpoints | join(",")), (.last_seen_at // "never")] | @tsv' |
    while IFS=$'\t' read -r kind name ip eps seen; do
      printf '%-12s %-18s %-13s %-30s %s\n' "$kind" "$name" "$ip" "${eps:--}" "$seen"
    done
}

mesh_cli() {
  local sub=${1:-status}
  (( $# )) && shift
  case "$sub" in
    init) need_root "mesh init"; mesh_init ;;
    invite) need_root "mesh invite"; mesh_invite "$@" ;;
    join) need_root "mesh join"; mesh_join "$@" ;;
    status) "$MESH_BIN" --run-dir "$MESH_RUN_DIR" status "$@" ;;
    nodes) need_root "mesh nodes"; mesh_nodes ;;
    revoke) need_root "mesh revoke"
      agent_api DELETE "/v1/mesh/nodes/${1:?usage: vercelify mesh revoke NAME}" >/dev/null
      ok "$1 is off the mesh; its peers drop it within seconds" ;;
    rotate) need_root "mesh rotate"; "$MESH_BIN" --run-dir "$MESH_RUN_DIR" rotate ;;
    *) die "usage: vercelify mesh init|invite|join|status|nodes|revoke NAME|rotate" ;;
  esac
}

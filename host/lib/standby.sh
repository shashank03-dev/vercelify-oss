#!/usr/bin/env bash
# The warm standby (design §8.4): `vercelify standby` on the main server
# chooses a worker that gets a sealed copy of the main server every 5
# minutes; `vercelify promote` on that worker makes it the main server when
# the old one is gone; `vercelify demote` turns a replaced main server into
# a machine that can join the new one as a worker.

: "${STANDBY_DIR:=/var/lib/vercelify/standby}"
: "${PLATFORM_DB:=/var/lib/vercelify/platform.db}"

# standby [NAME|off] — choose the standby, turn it off, or show it.
standby_cli() {
  local out
  case "${1:-}" in
    "") out=$(agent_api GET /v1/platform/settings/standby) ;;
    off|none) out=$(agent_api PUT /v1/platform/settings/standby '{"node": null}') ;;
    -*) die "usage: vercelify standby [NAME|off]" ;;
    *) out=$(agent_api PUT /v1/platform/settings/standby "$(jq -nc --arg n "$1" '{node: $n}')") ;;
  esac
  if [[ "$(jq -r '.node // empty' <<<"$out")" == "" ]]; then
    info "no standby: if this server dies, its apps' settings and data live only in its backups"
    info "choose one: sudo vercelify standby <worker>   (a server joined with: sudo vercelify mesh join … --platform)"
    return 0
  fi
  ok "standby: $(jq -r .node <<<"$out")"
  if [[ "$(jq -r '.last_sent_at // empty' <<<"$out")" != "" ]]; then
    info "last copy $(jq -r .last_sent_at <<<"$out") ($(( $(jq -r '.bytes // 0' <<<"$out") / 1024 )) KiB); a new one every 5 minutes"
  else
    info "the first copy is on its way"
  fi
  local err
  err=$(jq -r '.last_error // empty' <<<"$out")
  [[ -z "$err" ]] || warn "the last copy failed: $err"
}

# A top-level `key = value` of a TOML file (the raw value), or nothing.
toml_get() {
  grep -E "^[[:space:]]*$1[[:space:]]*=" "$2" 2>/dev/null | head -1 | sed -E 's/^[^=]*=[[:space:]]*//'
}

set_role() { # ROLE
  local env="$VERCELIFY_ETC/install.env"
  if grep -q '^role=' "$env" 2>/dev/null; then
    sed "s/^role=.*/role=$1/" "$env" | write_file "$env" 0644 || true
  else
    printf 'role=%s\n' "$1" >>"$env"
  fi
  toml_set role "\"$1\"" "$VERCELIFY_ETC/agent.toml"
}

# promote [--force] — on the standby: become the main server.
cmd_promote() {
  need_root promote
  local force=() cfg="$VERCELIFY_ETC/agent.toml" main_toml k v
  case "${1:-}" in
    "") ;;
    --force) force=(--force) ;;
    *) die "usage: vercelify promote [--force]" ;;
  esac
  [[ "$ROLE" == worker ]] || die "promote runs on the standby (a worker); this server is the main server"
  [[ -s "$STANDBY_DIR/snapshot.sealed" ]] ||
    die "no copy of the main server here: on the main server, sudo vercelify standby $(hostname -s)"
  if [[ -r "$STANDBY_DIR/snapshot.json" ]]; then
    info "copy of $(jq -r .from "$STANDBY_DIR/snapshot.json") from $(date -u -d "@$(jq -r .taken_at "$STANDBY_DIR/snapshot.json")" '+%Y-%m-%d %H:%M UTC' 2>/dev/null || echo '?')"
  fi
  confirm "Make this server the main server? Do this only when the old one is gone for good" n ||
    die "nothing changed"
  run_cmd systemctl stop vercelify-agent
  run_cmd systemctl stop vercelify-mesh
  if ! run_cmd "$AGENT_BIN" --config "$cfg" promote "${force[@]}"; then
    run_cmd systemctl start vercelify-mesh
    run_cmd systemctl start vercelify-agent
    die "not promoted (see above); this server is still the standby"
  fi
  set_role main
  toml_set engine '"platform"' "$cfg"
  toml_set mesh_coordinator true "$cfg"
  toml_list_add listen mesh "$cfg"
  # The old main server's settings that aren't about its hardware.
  main_toml="$STANDBY_DIR/main-agent.toml"
  for k in mesh_public_url mesh_public_endpoints acme_email; do
    v=$(toml_get "$k" "$main_toml")
    [[ -n "$v" ]] && toml_set "$k" "$v" "$cfg"
  done
  # Its Cloudflare tunnel: the same token runs it from here.
  if [[ -s "$STANDBY_DIR/main-cloudflared.env" ]]; then
    write_file "$VERCELIFY_ETC/cloudflared.env" 0600 <"$STANDBY_DIR/main-cloudflared.env" || true
    [[ -s "$STANDBY_DIR/main-cloudflare.env" ]] &&
      { write_file "$VERCELIFY_ETC/cloudflare.env" 0600 <"$STANDBY_DIR/main-cloudflare.env" || true; }
    local TUNNEL_TOKEN=""
    # shellcheck disable=SC1091
    . "$VERCELIFY_ETC/cloudflared.env"
    if [[ -n "$TUNNEL_TOKEN" ]]; then
      install_cloudflared
      cloudflared_service "$TUNNEL_TOKEN"
      ok "the Cloudflare tunnel runs from here now"
    fi
  fi
  run_cmd systemctl start vercelify-agent
  for _ in $(seq 1 30); do [[ -s "$MESH_DIR/state.json" ]] && break; sleep 1; done
  run_cmd systemctl start vercelify-mesh
  mesh_wait_up 10.77.0.1 && ok "this server is the main server now (10.77.0.1); every node follows it"
  printf '\n  Next:\n' >&2
  printf '    • Router port forwards (80/443, UDP 51820, TCP 7443) and DNS that pointed at the old main server: point them here\n' >&2
  printf '    • Apps that ran on the old main server start here within a few minutes\n' >&2
  printf '    • If the old main server comes back, it steps down; then: sudo vercelify demote on it\n\n' >&2
}

# demote [--force] — on a main server a standby replaced.
cmd_demote() {
  need_root demote
  local stamp c
  case "${1:-}" in
    ""|--force) ;;
    *) die "usage: vercelify demote [--force]" ;;
  esac
  [[ -e "$MESH_DIR/superseded.json" || "${1:-}" == --force ]] ||
    die "no standby replaced this server (sudo vercelify promote on a standby does that); --force demotes anyway"
  confirm "Stop being the main server here (its apps, database and mesh identity go aside)?" n || die "nothing changed"
  stamp=$(date -u +%Y%m%d%H%M%S)
  run_cmd systemctl stop vercelify-agent
  run_cmd systemctl stop vercelify-mesh
  if have docker; then
    for c in $(docker ps -aq --filter label=vercelify.managed=platform 2>/dev/null) vercelify-proxy vfy-registry; do
      run_cmd docker rm -f "$c" >/dev/null 2>&1 || true
    done
  fi
  [[ -e "$PLATFORM_DB" ]] && run_cmd mv "$PLATFORM_DB" "$PLATFORM_DB.demoted-$stamp"
  [[ -d "$MESH_DIR" ]] && run_cmd mv "$MESH_DIR" "$MESH_DIR.demoted-$stamp"
  set_role worker
  toml_set mesh_coordinator false "$VERCELIFY_ETC/agent.toml"
  run_cmd systemctl start vercelify-agent
  ok "this server is no longer the main server (the old state is kept as *.demoted-$stamp)"
  info "join the new main server: on it, sudo vercelify mesh invite; here, sudo vercelify mesh join TOKEN --platform"
}

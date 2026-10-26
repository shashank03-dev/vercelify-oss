#!/usr/bin/env bash
# `vercelify migrate` (design §18): from Coolify and Tailscale to Vercelify's
# own platform engine and mesh. The agent does the work (`vercelify-agent
# migrate …`, agent/src/migrate.rs); this runs its steps around restarting
# the agent and changing agent.toml.
#
#   vercelify migrate                 what would happen (nothing changes)
#   vercelify migrate --apply         take over, restart on the platform engine, check every app
#   vercelify migrate --check         ask every migrated app again
#   vercelify migrate --rollback      back to Coolify, as it was
#   vercelify migrate --finish        remove Coolify (--purge: its data too)
#   vercelify migrate --tailscale-off when every device is on the mesh

: "${MIGRATE_STATE:=/var/lib/vercelify/migrate/state.json}"

migrate_phase() {
  jq -r '.phase // empty' "$MIGRATE_STATE" 2>/dev/null || true
}

migrate_agent() {
  run_cmd "$AGENT_BIN" --config "$VERCELIFY_ETC/agent.toml" migrate "$@"
}

migrate_cli() {
  local cmd=plan force=() purge=() arg
  for arg in "$@"; do
    case "$arg" in
      --apply) cmd=apply ;;
      --check) cmd=check ;;
      --rollback) cmd=rollback ;;
      --finish) cmd=finish ;;
      --status) cmd=status ;;
      --tailscale-off) cmd=tailscale ;;
      --force) force=(--force) ;;
      --purge) purge=(--purge) ;;
      *) die "usage: vercelify migrate [--apply [--force] | --check | --rollback | --finish [--purge] | --status | --tailscale-off [--force]]" ;;
    esac
  done
  need_root migrate
  [[ "$ROLE" == main ]] || die "migrate runs on the main server (Coolify's); workers join the new main server: sudo vercelify mesh join … --platform"
  case "$cmd" in
    plan) migrate_plan ;;
    apply) migrate_apply "${force[@]}" ;;
    check) migrate_agent check ;;
    rollback) migrate_rollback ;;
    finish) migrate_finish "${purge[@]}" ;;
    status) migrate_agent status ;;
    tailscale) migrate_tailscale_off "${force[@]}" ;;
  esac
}

migrate_plan() {
  if platform_engine && [[ -z "$(migrate_phase)" ]]; then
    ok "this server runs Vercelify's platform engine already: nothing to migrate"
    return 0
  fi
  migrate_agent plan
  printf '\n  Nothing changed. To go ahead: sudo vercelify migrate --apply\n' >&2
}

migrate_apply() {
  local cfg="$VERCELIFY_ETC/agent.toml" phase
  phase=$(migrate_phase)
  case "$phase" in
    imported) info "taken over already: restarting on the platform engine and checking" ;;
    switched) ok "migrated already"; return 0 ;;
    finished) ok "migrated already, and Coolify removed"; return 0 ;;
    importing) die "an earlier migration stopped halfway: sudo vercelify migrate --rollback, then again" ;;
    *)
      platform_engine && die "this server runs the platform engine already"
      step "Migrating from Coolify"
      migrate_agent plan
      printf '\n' >&2
      info "Coolify stops for the switch (its containers are kept: --rollback brings it back)"
      info "apps answer again when their first deployment is healthy (no builds: Coolify's images are used)"
      confirm "Take these over and stop Coolify?" n || die "nothing changed"
      # The private network first, next to Tailscale (design §18).
      if [[ ! -s "$MESH_DIR/state.json" ]]; then
        step "Vercelify's network (next to Tailscale, which keeps working)"
        mesh_init
      fi
      if ! migrate_agent apply "$@"; then
        err "the migration stopped; going back to Coolify"
        migrate_agent rollback || true
        die "nothing was migrated (see above)"
      fi
      ;;
  esac
  toml_set engine '"platform"' "$cfg"
  run_cmd systemctl restart vercelify-agent
  step "Checking every app"
  if ! migrate_agent check --wait 600; then
    warn "not every app answers yet: open the app's deployment logs in the Mac app (or: vercelify logs)"
    info "check again: sudo vercelify migrate --check      go back: sudo vercelify migrate --rollback"
    return 1
  fi
  ok "every app runs on Vercelify's platform engine"
  printf '\n  Next:\n' >&2
  printf '    • Coolify is stopped, not removed. When you are happy (the plan keeps it 14 days): sudo vercelify migrate --finish\n' >&2
  printf '    • Pair the Mac app again (sudo vercelify pair): it then reaches this server over Vercelify'"'"'s network\n' >&2
  printf '    • When every device shows up in: sudo vercelify mesh nodes — sudo vercelify migrate --tailscale-off\n\n' >&2
}

migrate_rollback() {
  local cfg="$VERCELIFY_ETC/agent.toml"
  [[ -n "$(migrate_phase)" ]] || die "no migration on this server"
  confirm "Go back to Coolify? Changes made since the migration (deploys, data written to databases) are not kept" n ||
    die "nothing changed"
  run_cmd systemctl stop vercelify-agent
  if ! migrate_agent rollback; then
    run_cmd systemctl start vercelify-agent
    die "the rollback didn't finish (see above); run it again"
  fi
  toml_set engine '"coolify"' "$cfg"
  run_cmd systemctl start vercelify-agent
  ok "back on Coolify (http://$(hostname -I 2>/dev/null | awk '{print $1}'):8000)"
}

migrate_finish() {
  [[ "$(migrate_phase)" == switched ]] ||
    die "finish after the migration checked out: sudo vercelify migrate --check"
  confirm "Remove Coolify for good? There is no rollback afterwards" n || die "nothing changed"
  migrate_agent finish "$@"
  rm -f "$VERCELIFY_ETC/coolify.token"
  ok "Coolify is gone; the memory it used is free for your apps"
}

# Tailscale off once every device has used the mesh in the last day.
migrate_tailscale_off() {
  local nodes stale
  platform_engine || die "migrate to the platform engine first: sudo vercelify migrate --apply"
  [[ -s "$MESH_DIR/state.json" ]] || die "Vercelify's network isn't set up: sudo vercelify mesh init"
  have tailscale || { ok "Tailscale isn't installed"; return 0; }
  nodes=$(agent_api GET /v1/mesh/nodes)
  mesh_nodes
  stale=$(jq -r --argjson day "$(( ${MIGRATE_NOW:-$(date +%s)} - 86400 ))" '
    .[] | select(.revoked_at == null and .coordinator != true) |
    select((.last_seen_at // "1970-01-01T00:00:00Z" | sub("\\.[0-9]+"; "") | fromdateiso8601) < $day) | .name' <<<"$nodes")
  if [[ "${1:-}" != --force ]]; then
    [[ "$(jq '[.[] | select(.coordinator != true and .revoked_at == null)] | length' <<<"$nodes")" -gt 0 ]] ||
      die "no device or worker is on Vercelify's network yet: pair the Mac app again first (sudo vercelify pair)"
    [[ -z "$stale" ]] || die "not seen on the mesh in the last 24 hours: $(tr '\n' ' ' <<<"$stale")— or --force"
  fi
  confirm "Turn Tailscale off on this server? Devices reach it over Vercelify's network from now on" n ||
    die "nothing changed"
  run_cmd tailscale down || true
  run_cmd systemctl disable --now tailscaled >/dev/null 2>&1 || true
  ok "Tailscale is off (uninstall it with your package manager when you like)"
}

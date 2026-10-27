#!/usr/bin/env bash
# Reboot window: security updates (unattended-upgrades) often need a reboot
# to take effect (a new kernel, libc). A timer checks once a night and reboots
# only when one is needed and nothing important is happening. Source, don't
# execute.

: "${MAINTENANCE_CONF:=$VERCELIFY_ETC/maintenance.conf}"
: "${MAINTENANCE_STATE:=${VERCELIFY_STATE:-/var/lib/vercelify}/maintenance.json}"
: "${REBOOT_REQUIRED_FILE:=/var/run/reboot-required}"
: "${MODULES_DIR:=/lib/modules}"
: "${POWER_GUARD_STATE:=/run/vercelify/power-guard.state}"
: "${PROC_UPTIME:=/proc/uptime}"
: "${REBOOT_CMD:=systemctl reboot}"

maintenance_conf() { # KEY DEFAULT
  local v=''
  [[ -r "$MAINTENANCE_CONF" ]] && v=$(sed -n "s/^$1=//p" "$MAINTENANCE_CONF" | tail -1 | tr -d '"'"'")
  printf '%s' "${v:-$2}"
}

valid_window() { [[ "$1" =~ ^([01][0-9]|2[0-3]):[0-5][0-9]$ ]]; }

# Why a reboot is needed, or nothing.
reboot_reason() {
  if [[ -e "$REBOOT_REQUIRED_FILE" ]]; then
    local pkgs
    pkgs=$(tr '\n' ' ' <"$REBOOT_REQUIRED_FILE.pkgs" 2>/dev/null | sed 's/ *$//')
    echo "security updates installed${pkgs:+ ($pkgs)}"
    return
  fi
  # Raspberry Pi OS and plain Debian don't always write reboot-required: a
  # replaced kernel shows up as the running kernel's modules being gone. Only
  # when a kernel was installed since boot: a VPS or container running a
  # kernel the distro didn't provide must not reboot every night.
  local newest up
  [[ -d "$MODULES_DIR" && ! -d "$MODULES_DIR/$(uname -r)" ]] || return 0
  newest=$(ls -td "$MODULES_DIR"/*/ 2>/dev/null | head -1)
  [[ -n "$newest" ]] || return 0
  up=$(cut -d. -f1 "$PROC_UPTIME" 2>/dev/null || echo 0)
  if (( $(stat -c %Y "$newest") > $(date +%s) - up )); then
    echo "a new kernel was installed (running $(uname -r))"
  fi
  return 0
}

# Why not now, or nothing when it is safe to reboot.
# Is the engine building or rolling out an app? (An agent that doesn't
# answer blocks nothing: the reboot is what may bring it back.)
deployment_running() {
  [[ -r "$VERCELIFY_ETC/agent.token" ]] || return 1
  (API_TIMEOUT=10 agent_api GET /v1/platform/apps) 2>/dev/null |
    jq -e 'any(.[]; .last_status == "queued" or .last_status == "building" or .last_status == "deploying")' >/dev/null 2>&1
}

reboot_blocker() {
  local up
  up=$(cut -d. -f1 "$PROC_UPTIME" 2>/dev/null || echo 0)
  (( up >= 3600 )) || { echo "the machine started less than an hour ago"; return; }
  if [[ -r "$POWER_GUARD_STATE" ]] && grep -Eq '"state":"(warning|thermal_paused|shutting_down)"' "$POWER_GUARD_STATE"; then
    echo "the power guard reports: $(sed -E 's/.*"reason":"([^"]*)".*/\1/' "$POWER_GUARD_STATE")"
    return
  fi
  if deployment_running; then
    echo "a deployment is building"
    return
  fi
  local u
  for u in vercelify-backup vercelify-update; do
    systemctl is-active --quiet "$u" 2>/dev/null && { echo "${u#vercelify-} is running"; return; }
  done
  return 0
}

maintenance_record() { # RESULT DETAIL
  mkdir -p "$(dirname "$MAINTENANCE_STATE")"
  jq -n --arg r "$1" --arg d "$2" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{result: $r, detail: $d, at: $at}' >"$MAINTENANCE_STATE.tmp" && mv -f "$MAINTENANCE_STATE.tmp" "$MAINTENANCE_STATE"
}

# The nightly check (the timer runs it inside the window).
maintenance_run() {
  local reason blocker
  if [[ "$(maintenance_conf AUTO_REBOOT yes)" != yes ]]; then
    info "automatic reboots are off"
    return 0
  fi
  reason=$(reboot_reason)
  if [[ -z "$reason" ]]; then
    maintenance_record none "no reboot needed"
    ok "no reboot needed"
    return 0
  fi
  blocker=$(reboot_blocker)
  if [[ -n "$blocker" ]]; then
    maintenance_record skipped "$reason; not now: $blocker"
    warn "reboot needed ($reason) but skipped: $blocker — trying again tomorrow night"
    return 0
  fi
  maintenance_record rebooting "$reason"
  info "rebooting: $reason"
  logger -t vercelify "rebooting in the maintenance window: $reason" 2>/dev/null || true
  sync
  $REBOOT_CMD
}

# Installs (or updates) the timer; keeps the user's settings.
maintenance_setup() {
  if [[ ! -f "$MAINTENANCE_CONF" ]]; then
    printf '%s\n' '# Reboot for security updates in a nightly window (vercelify maintenance).' \
      'AUTO_REBOOT=yes' 'REBOOT_WINDOW=04:30' | write_file "$MAINTENANCE_CONF" 0644 || true
  fi
  local window
  window=$(maintenance_conf REBOOT_WINDOW 04:30)
  valid_window "$window" || window=04:30
  write_file "$SYSTEMD_DIR/vercelify-maintenance.service" 0644 <<'EOF' || true
[Unit]
Description=Vercelify reboot window (reboots only when updates need it)
After=network-online.target docker.service

[Service]
Type=oneshot
ExecStart=/usr/local/bin/vercelify maintenance run
EOF
  write_file "$SYSTEMD_DIR/vercelify-maintenance.timer" 0644 <<EOF || true
[Unit]
Description=Vercelify reboot window

[Timer]
OnCalendar=*-*-* $window
RandomizedDelaySec=10min

[Install]
WantedBy=timers.target
EOF
  run_cmd systemctl daemon-reload
  if [[ "$(maintenance_conf AUTO_REBOOT yes)" == yes ]]; then
    run_cmd systemctl enable --now vercelify-maintenance.timer >/dev/null 2>&1 || true
    ok "reboots for security updates only when needed, around $window (change: sudo vercelify maintenance)"
  else
    run_cmd systemctl disable --now vercelify-maintenance.timer >/dev/null 2>&1 || true
    ok "automatic reboots stay off (sudo vercelify maintenance on)"
  fi
}

maintenance_set() { # KEY VALUE
  local tmp
  tmp=$(mktemp)
  { grep -v "^$1=" "$MAINTENANCE_CONF" 2>/dev/null || true; echo "$1=$2"; } >"$tmp"
  write_file "$MAINTENANCE_CONF" 0644 <"$tmp" || true
  rm -f "$tmp"
}

maintenance_cli() {
  case "${1:-status}" in
    status)
      local reason last
      reason=$(reboot_reason)
      if [[ "$(maintenance_conf AUTO_REBOOT yes)" == yes ]]; then
        info "automatic reboots on, around $(maintenance_conf REBOOT_WINDOW 04:30) when updates need one"
      else
        info "automatic reboots off"
      fi
      info "reboot needed: ${reason:-no}"
      last=$(jq -r '"\(.at) \(.result): \(.detail)"' "$MAINTENANCE_STATE" 2>/dev/null || true)
      [[ -n "$last" ]] && info "last check: $last"
      return 0
      ;;
    on)
      if [[ -n "${2:-}" ]]; then
        valid_window "$2" || die "use a time like 04:30"
        maintenance_set REBOOT_WINDOW "$2"
      fi
      maintenance_set AUTO_REBOOT yes
      maintenance_setup
      ;;
    off) maintenance_set AUTO_REBOOT no; maintenance_setup ;;
    run) maintenance_run ;;
    *) die "usage: vercelify maintenance [status|on [HH:MM]|off|run]" ;;
  esac
}

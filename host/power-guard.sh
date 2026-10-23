#!/usr/bin/env bash
# vercelify power-guard — keeps a laptop-hosted PaaS safe.
#
#   * AC unplugged and battery <= BATTERY_SHUTDOWN_PCT (2 readings in a row):
#     stop app containers, then the Coolify core, sync, power off. Containers
#     stopped this way are remembered and started again on the next boot.
#   * CPU >= THERMAL_PAUSE_C: pause build containers and the AI model server,
#     and cap app containers' CPU so they keep serving, more slowly.
#   * CPU >= THERMAL_FREEZE_C: also freeze app containers. Databases and the
#     Coolify core are never frozen (other services wait on their locks).
#     Everything is undone once the CPU is at or below THERMAL_RESUME_C.
#   * CPU >= THERMAL_SHUTDOWN_C for 3 readings: graceful shutdown.
#
# Usage: power-guard.sh [--once | --status | --restore]
# Config: /etc/vercelify/power-guard.conf (KEY=VALUE, see defaults below).

set -uo pipefail

CONFIG_FILE=${POWER_GUARD_CONFIG:-/etc/vercelify/power-guard.conf}
# shellcheck disable=SC1090
[[ -r "$CONFIG_FILE" ]] && . "$CONFIG_FILE"
# Thresholds changed from the Vercelify app (written by the agent).
LOCAL_CONFIG_FILE=${POWER_GUARD_LOCAL_CONFIG:-/var/lib/vercelify/power-guard.local.conf}
# shellcheck disable=SC1090
[[ -r "$LOCAL_CONFIG_FILE" ]] && . "$LOCAL_CONFIG_FILE"

: "${BATTERY_SHUTDOWN_PCT:=15}"
: "${BATTERY_WARN_PCT:=30}"
: "${THERMAL_PAUSE_C:=92}"
: "${THERMAL_RESUME_C:=80}"
: "${THERMAL_SHUTDOWN_C:=98}"
: "${THERMAL_FREEZE_C:=$(( THERMAL_SHUTDOWN_C - 3 ))}"
# Fraction of the CPUs each app container may use while hot (percent).
: "${THERMAL_CPU_CAP_PCT:=50}"
: "${INTERVAL:=20}"
: "${CONTAINER_STOP_TIMEOUT:=60}"
: "${CHARGE_STOP_THRESHOLD:=}"
: "${PROTECTED_CONTAINERS:=coolify coolify-db coolify-redis coolify-realtime coolify-proxy coolify-sentinel}"
# Images that hold data: never frozen while hot.
: "${DATA_IMAGE_PATTERN:=postgres|postgis|mysql|mariadb|mongo|redis|valkey|keydb|dragonfly|clickhouse|minio}"
# Build containers (Coolify's helper image) and the local AI server.
: "${BUILD_IMAGE_PATTERN:=coolify-helper|vercelify-ollama|ollama/ollama}"
: "${SHUTDOWN_CMD:=systemctl poweroff}"
: "${POWER_SUPPLY_DIR:=/sys/class/power_supply}"
: "${SYS_CLASS_DIR:=/sys/class}"
: "${STATE_DIR:=/run/vercelify}"
: "${PERSIST_DIR:=/var/lib/vercelify}"
: "${DOCKER:=docker}"
: "${NOTIFY_URL:=}"

STATE_FILE="$STATE_DIR/power-guard.state"
COUNTERS_FILE="$STATE_DIR/power-guard.counters"
PAUSED_FILE="$STATE_DIR/power-guard.paused"
CAPPED_FILE="$STATE_DIR/power-guard.capped"
RESTORE_FILE="$PERSIST_DIR/power-guard.restore"

log() { printf '%s power-guard: %s\n' "$(date -Is)" "$*" >&2; }

notify() {
  [[ -n "$NOTIFY_URL" ]] || return 0
  # Works with ntfy.sh topics, Discord/Slack-compatible plain webhooks, etc.
  curl -fsS -m 10 -H "Title: Vercelify $(hostname)" -d "$*" "$NOTIFY_URL" >/dev/null 2>&1 || true
}

# --- readings ---------------------------------------------------------------

# Prints "<ac> <battery_present> <min_capacity> <discharging>"
#   ac: 1 | 0 | none
read_power() {
  local ac=none present=0 min=101 discharging=0 dev type scope online cap status
  for dev in "$POWER_SUPPLY_DIR"/*; do
    [[ -e "$dev/type" ]] || continue
    type=$(<"$dev/type")
    scope=$(cat "$dev/scope" 2>/dev/null || true)
    [[ "${scope,,}" == device ]] && continue
    case "$type" in
      Mains|USB|USB_C|USB_PD)
        if [[ -r "$dev/online" ]]; then
          online=$(<"$dev/online")
          if [[ "$online" == 1 ]]; then ac=1; elif [[ "$ac" == none ]]; then ac=0; fi
        fi ;;
      Battery)
        [[ "$(cat "$dev/present" 2>/dev/null || echo 1)" == 0 ]] && continue
        present=1
        cap=$(cat "$dev/capacity" 2>/dev/null || echo 101)
        [[ "$cap" =~ ^[0-9]+$ ]] && (( cap < min )) && min=$cap
        status=$(cat "$dev/status" 2>/dev/null || true)
        [[ "$status" == Discharging ]] && discharging=1 ;;
    esac
  done
  (( min == 101 )) && min=-1
  echo "$ac $present $min $discharging"
}

# Highest plausible temperature in whole °C, or -1 if no sensors.
read_max_temp() {
  local max=-1000 f v
  for f in "$SYS_CLASS_DIR"/thermal/thermal_zone*/temp "$SYS_CLASS_DIR"/hwmon/hwmon*/temp*_input; do
    [[ -r "$f" ]] || continue
    v=$(<"$f")
    [[ "$v" =~ ^-?[0-9]+$ ]] || continue
    v=$(( v / 1000 ))
    (( v > -40 && v < 130 && v > max )) && max=$v
  done
  (( max == -1000 )) && max=-1
  echo "$max"
}

# --- state ------------------------------------------------------------------

LOW_COUNT=0
HOT_COUNT=0
CURRENT_STATE=ok

load_counters() {
  # shellcheck disable=SC1090
  [[ -r "$COUNTERS_FILE" ]] && . "$COUNTERS_FILE"
  return 0
}

save_counters() {
  mkdir -p "$STATE_DIR"
  printf 'LOW_COUNT=%d\nHOT_COUNT=%d\nCURRENT_STATE=%s\n' "$LOW_COUNT" "$HOT_COUNT" "$CURRENT_STATE" >"$COUNTERS_FILE"
}

json_escape() { local s=${1//\\/\\\\}; s=${s//\"/\\\"}; printf '%s' "$s"; }

write_state() {
  local state=$1 reason=$2 tmp
  mkdir -p "$STATE_DIR"
  tmp=$(mktemp "$STATE_DIR/.state.XXXXXX")
  printf '{"state":"%s","reason":"%s","updated_at":"%s"}\n' \
    "$state" "$(json_escape "$reason")" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >"$tmp"
  chmod 0644 "$tmp"
  mv -f "$tmp" "$STATE_FILE"
  if [[ "$state" != "$CURRENT_STATE" ]]; then
    log "state $CURRENT_STATE -> $state ($reason)"
    [[ "$state" != ok ]] && notify "$state: $reason"
    [[ "$state" == ok && "$CURRENT_STATE" != ok ]] && notify "back to normal: $reason"
  fi
  CURRENT_STATE=$state
}

is_protected() {
  local name=$1 p
  for p in $PROTECTED_CONTAINERS; do [[ "$name" == "$p" ]] && return 0; done
  return 1
}

# Running containers as "id name image" lines.
running_containers() { "$DOCKER" ps --format '{{.ID}} {{.Names}} {{.Image}}' 2>/dev/null; }

is_data() { [[ "$2" =~ ($DATA_IMAGE_PATTERN) || "$1" =~ ^($DATA_IMAGE_PATTERN) ]]; }
is_build() { [[ "$2" =~ ($BUILD_IMAGE_PATTERN) || "$1" =~ ($BUILD_IMAGE_PATTERN) ]]; }

cpu_count() { nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1; }

# --- actions ----------------------------------------------------------------

graceful_shutdown() {
  local reason=$1 id name apps=() core=()
  write_state shutting_down "$reason"
  save_counters
  mkdir -p "$PERSIST_DIR"
  while read -r id name _; do
    [[ -n "$id" ]] || continue
    if is_protected "$name"; then core+=("$id"); else apps+=("$id"); fi
    echo "$id" >>"$RESTORE_FILE"
  done < <(running_containers)
  # Paused containers cannot be stopped; unfreeze them first.
  [[ -s "$PAUSED_FILE" ]] && xargs -r "$DOCKER" unpause <"$PAUSED_FILE" >/dev/null 2>&1
  rm -f "$PAUSED_FILE" "$CAPPED_FILE"
  if (( ${#apps[@]} )); then
    log "stopping ${#apps[@]} app container(s)"
    "$DOCKER" stop -t "$CONTAINER_STOP_TIMEOUT" "${apps[@]}" >/dev/null 2>&1 || true
  fi
  if (( ${#core[@]} )); then
    log "stopping ${#core[@]} Coolify core container(s)"
    "$DOCKER" stop -t "$CONTAINER_STOP_TIMEOUT" "${core[@]}" >/dev/null 2>&1 || true
  fi
  sync
  log "powering off: $reason"
  # shellcheck disable=SC2086
  $SHUTDOWN_CMD
  return 0
}

pause_ids() { # pause_ids ID... — freeze and remember
  local id
  for id in "$@"; do
    grep -qx "$id" "$PAUSED_FILE" 2>/dev/null && continue
    "$DOCKER" pause "$id" >/dev/null 2>&1 && echo "$id" >>"$PAUSED_FILE"
  done
}

# Stage 1: pause builds and AI, cap app CPU. Prints a short summary.
thermal_pause() {
  local id name image builds=() apps=() cap
  touch "$PAUSED_FILE" "$CAPPED_FILE"
  while read -r id name image; do
    [[ -n "$id" ]] || continue
    is_protected "$name" && continue
    if is_build "$name" "$image"; then builds+=("$id")
    elif ! is_data "$name" "$image"; then apps+=("$id"); fi
  done < <(running_containers)
  pause_ids "${builds[@]}"
  cap=$(awk -v n="$(cpu_count)" -v p="$THERMAL_CPU_CAP_PCT" 'BEGIN { v = n * p / 100; if (v < 0.25) v = 0.25; printf "%.2f", v }')
  for id in "${apps[@]}"; do
    grep -q "^$id " "$CAPPED_FILE" 2>/dev/null && continue
    local prev
    prev=$("$DOCKER" inspect -f '{{.HostConfig.NanoCpus}}' "$id" 2>/dev/null || echo 0)
    [[ "$prev" =~ ^[0-9]+$ ]] || prev=0
    "$DOCKER" update --cpus "$cap" "$id" >/dev/null 2>&1 && echo "$id $prev" >>"$CAPPED_FILE"
  done
  log "hot: paused ${#builds[@]} build/AI container(s), capped ${#apps[@]} app(s) at $cap CPUs"
}

# Stage 2: freeze app containers too (never databases or the core).
thermal_freeze() {
  local id name image apps=()
  while read -r id name image; do
    [[ -n "$id" ]] || continue
    is_protected "$name" && continue
    is_data "$name" "$image" && continue
    apps+=("$id")
  done < <(running_containers)
  pause_ids "${apps[@]}"
  log "very hot: froze app containers"
}

thermal_resume() {
  [[ -s "$PAUSED_FILE" ]] && xargs -r "$DOCKER" unpause <"$PAUSED_FILE" >/dev/null 2>&1
  if [[ -s "$CAPPED_FILE" ]]; then
    local id prev
    while read -r id prev; do
      [[ -n "$id" ]] || continue
      "$DOCKER" update --cpus "$(awk -v n="${prev:-0}" 'BEGIN { printf "%.2f", n / 1e9 }')" "$id" >/dev/null 2>&1 || true
    done <"$CAPPED_FILE"
  fi
  rm -f "$PAUSED_FILE" "$CAPPED_FILE"
  log "resumed paused containers and restored CPU limits"
}

# Start containers that power-guard stopped before the last shutdown.
restore_after_boot() {
  [[ -s "$RESTORE_FILE" ]] || return 0
  local ac present cap _d
  read -r ac present cap _d < <(read_power)
  if [[ "$present" == 1 && "$ac" != 1 ]] && (( cap >= 0 && cap <= BATTERY_SHUTDOWN_PCT + 10 )); then
    log "battery still low (${cap}%), not restoring containers yet"
    return 0
  fi
  # Retry: dockerd may still be starting.
  for _ in 1 2 3 4 5 6; do "$DOCKER" info >/dev/null 2>&1 && break; sleep 5; done
  log "restoring $(sort -u "$RESTORE_FILE" | wc -l) container(s) stopped before shutdown"
  sort -u "$RESTORE_FILE" | xargs -r "$DOCKER" start >/dev/null 2>&1 || true
  rm -f "$RESTORE_FILE"
}

apply_charge_threshold() {
  [[ -n "$CHARGE_STOP_THRESHOLD" ]] || return 0
  local f
  for f in "$POWER_SUPPLY_DIR"/BAT*/charge_control_end_threshold; do
    [[ -w "$f" ]] || continue
    echo "$CHARGE_STOP_THRESHOLD" >"$f" 2>/dev/null && log "battery charge limit set to ${CHARGE_STOP_THRESHOLD}% ($f)"
  done
}

# --- main loop --------------------------------------------------------------

tick() {
  local ac present cap discharging temp on_battery=0 reasons=()
  read -r ac present cap discharging < <(read_power)
  temp=$(read_max_temp)

  if [[ "$present" == 1 ]]; then
    if [[ "$ac" == 0 ]] || [[ "$ac" == none && "$discharging" == 1 ]]; then on_battery=1; fi
  fi

  # Battery
  if (( on_battery )) && (( cap >= 0 && cap <= BATTERY_SHUTDOWN_PCT )); then
    LOW_COUNT=$((LOW_COUNT + 1))
    if (( LOW_COUNT >= 2 )); then
      graceful_shutdown "on battery at ${cap}% (<= ${BATTERY_SHUTDOWN_PCT}%)"
      return 0
    fi
  else
    LOW_COUNT=0
  fi

  # Thermal
  if (( temp >= THERMAL_SHUTDOWN_C )); then
    HOT_COUNT=$((HOT_COUNT + 1))
    if (( HOT_COUNT >= 3 )); then
      graceful_shutdown "CPU at ${temp}°C (>= ${THERMAL_SHUTDOWN_C}°C)"
      return 0
    fi
  else
    HOT_COUNT=0
  fi

  if [[ "$CURRENT_STATE" == thermal_paused ]]; then
    if (( temp >= 0 && temp <= THERMAL_RESUME_C )); then
      thermal_resume
    else
      if (( temp >= THERMAL_FREEZE_C )); then
        thermal_freeze
        reasons+=("CPU at ${temp}°C: apps frozen, builds paused until <= ${THERMAL_RESUME_C}°C")
      else
        thermal_pause
        reasons+=("CPU at ${temp}°C: builds paused, apps slowed until <= ${THERMAL_RESUME_C}°C")
      fi
      write_state thermal_paused "${reasons[*]}"
      save_counters
      return 0
    fi
  elif (( temp >= THERMAL_PAUSE_C )); then
    thermal_pause
    (( temp >= THERMAL_FREEZE_C )) && thermal_freeze
    write_state thermal_paused "CPU at ${temp}°C (>= ${THERMAL_PAUSE_C}°C): builds paused, apps slowed"
    save_counters
    return 0
  fi

  if (( on_battery )); then
    if (( cap >= 0 && cap <= BATTERY_WARN_PCT )); then
      write_state warning "on battery at ${cap}% — shutdown at ${BATTERY_SHUTDOWN_PCT}%"
    else
      write_state warning "running on battery (${cap}%)"
    fi
  else
    local msg="AC power"
    [[ "$present" == 0 ]] && msg="no battery"
    (( temp >= 0 )) && msg+=", CPU ${temp}°C"
    write_state ok "$msg"
  fi
  save_counters
}

status() {
  local ac present cap discharging
  read -r ac present cap discharging < <(read_power)
  printf 'ac_online=%s battery_present=%s battery_pct=%s discharging=%s max_temp_c=%s\n' \
    "$ac" "$present" "$cap" "$discharging" "$(read_max_temp)"
  [[ -r "$STATE_FILE" ]] && cat "$STATE_FILE"
  return 0
}

main() {
  case "${1:-}" in
    --status) status; return ;;
    --once) load_counters; tick; return ;;
    --restore) restore_after_boot; return ;;
    ""|--daemon) ;;
    -h|--help) sed -n '2,15p' "$0"; return ;;
    *) echo "unknown option: $1" >&2; return 2 ;;
  esac
  load_counters
  CURRENT_STATE=ok
  apply_charge_threshold
  restore_after_boot
  log "started (shutdown<=${BATTERY_SHUTDOWN_PCT}% pause>=${THERMAL_PAUSE_C}°C every ${INTERVAL}s)"
  trap 'log "stopping"; [[ -s "$PAUSED_FILE" || -s "$CAPPED_FILE" ]] && thermal_resume; exit 0' TERM INT
  while :; do
    tick
    sleep "$INTERVAL" &
    wait $!
  done
}

main "$@"

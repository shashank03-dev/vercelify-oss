#!/usr/bin/env bash
# Sourced by power-guard.sh. Opt-in policy; shutdown remains independent.
: "${BATTERY_POLICY_ENABLED:=0}"
: "${BATTERY_BUILD_HOLD_PCT:=50}"
: "${BATTERY_WORK_PAUSE_PCT:=40}"
: "${BATTERY_PREVIEW_SLEEP_PCT:=30}"
: "${BATTERY_APP_SLEEP_PCT:=20}"
: "${BATTERY_HYSTERESIS_PCT:=5}"
: "${BATTERY_RECOVERY_AC_SECONDS:=60}"
: "${BATTERY_DEBOUNCE_READINGS:=2}"
BATTERY_STAGE=0 BATTERY_CANDIDATE=0 BATTERY_COUNT=0 BATTERY_AC_SINCE=0 BATTERY_LAST_NOW=0
BATTERY_JOURNAL="$PERSIST_DIR/power-guard.battery"
BATTERY_PAUSED="$PERSIST_DIR/power-guard.battery-paused"
# shellcheck disable=SC1090
[[ -r "$BATTERY_JOURNAL" ]] && . "$BATTERY_JOURNAL"

battery_config_valid() {
  local n
  for n in "$BATTERY_BUILD_HOLD_PCT" "$BATTERY_WORK_PAUSE_PCT" "$BATTERY_PREVIEW_SLEEP_PCT" "$BATTERY_APP_SLEEP_PCT" "$BATTERY_HYSTERESIS_PCT" "$BATTERY_RECOVERY_AC_SECONDS" "$BATTERY_DEBOUNCE_READINGS"; do
    [[ "$n" =~ ^[0-9]+$ ]] || return 1
  done
  (( BATTERY_BUILD_HOLD_PCT <= 95 && BATTERY_BUILD_HOLD_PCT > BATTERY_WORK_PAUSE_PCT && BATTERY_WORK_PAUSE_PCT > BATTERY_PREVIEW_SLEEP_PCT && BATTERY_PREVIEW_SLEEP_PCT > BATTERY_APP_SLEEP_PCT && BATTERY_APP_SLEEP_PCT > BATTERY_SHUTDOWN_PCT && BATTERY_HYSTERESIS_PCT >= 1 && BATTERY_HYSTERESIS_PCT <= 20 && BATTERY_RECOVERY_AC_SECONDS >= 1 && BATTERY_DEBOUNCE_READINGS >= 2 ))
}
battery_save() {
  local tmp
  mkdir -p "$PERSIST_DIR"
  tmp=$(mktemp "$PERSIST_DIR/.battery.XXXXXX") || return 1
  printf 'BATTERY_STAGE=%d\nBATTERY_CANDIDATE=%d\nBATTERY_COUNT=%d\nBATTERY_AC_SINCE=%d\nBATTERY_LAST_NOW=%d\n' "$BATTERY_STAGE" "$BATTERY_CANDIDATE" "$BATTERY_COUNT" "$BATTERY_AC_SINCE" "$BATTERY_LAST_NOW" >"$tmp"
  chmod 0600 "$tmp"; mv -f "$tmp" "$BATTERY_JOURNAL"
}
battery_pause_work() {
  local id name image paused
  while read -r id name image; do
    is_build "$name" "$image" || continue
    grep -qx "$id" "$BATTERY_PAUSED" 2>/dev/null && continue
    paused=$("$DOCKER" inspect -f '{{.State.Paused}}' "$id" 2>/dev/null) || continue
    [[ "$paused" == true ]] && continue
    # Claim before acting: a guard restart can finish or undo its own action.
    (umask 077; echo "$id" >>"$BATTERY_PAUSED")
    "$DOCKER" pause "$id" >/dev/null 2>&1 || true
  done < <(running_containers)
}
battery_resume_work() {
  local id
  [[ -r "$BATTERY_PAUSED" ]] || return 0
  while read -r id; do
    [[ "$id" =~ ^[a-zA-Z0-9]+$ ]] || continue
    # Unpause does not start stopped/deleted work, or enqueue a new build.
    "$DOCKER" unpause "$id" >/dev/null 2>&1 || true
  done <"$BATTERY_PAUSED"
  rm -f "$BATTERY_PAUSED"
}
battery_policy_tick() { # on_battery ac present capacity
  local on_battery=$1 ac=$2 present=$3 cap=$4 target=0 clock threshold state
  [[ "$BATTERY_POLICY_ENABLED" == 1 || "$BATTERY_STAGE" -gt 0 ]] || return 1
  if ! battery_config_valid; then
    write_state battery_recovery "invalid battery policy; repair configuration before resuming work"
    return 0
  fi
  clock=$(date +%s)
  if (( clock < BATTERY_LAST_NOW )); then BATTERY_AC_SINCE=0; fi
  BATTERY_LAST_NOW=$clock
  if (( on_battery )) && [[ "$BATTERY_POLICY_ENABLED" == 1 ]] && (( cap >= 0 && cap <= 100 )); then
    BATTERY_AC_SINCE=0
    (( cap <= BATTERY_BUILD_HOLD_PCT )) && target=1
    (( cap <= BATTERY_WORK_PAUSE_PCT )) && target=2
    (( cap <= BATTERY_PREVIEW_SLEEP_PCT )) && target=3
    (( cap <= BATTERY_APP_SLEEP_PCT )) && target=4
    if (( target > BATTERY_STAGE )); then
      if (( target == BATTERY_CANDIDATE )); then BATTERY_COUNT=$((BATTERY_COUNT + 1)); else BATTERY_CANDIDATE=$target; BATTERY_COUNT=1; fi
      if (( BATTERY_COUNT >= BATTERY_DEBOUNCE_READINGS )); then BATTERY_STAGE=$target; BATTERY_COUNT=0; fi
    else BATTERY_COUNT=0; BATTERY_CANDIDATE=$target; fi
  else
    BATTERY_COUNT=0; BATTERY_CANDIDATE=0
    if [[ "$ac" == 1 && "$present" == 1 ]] && (( cap >= 0 && cap <= 100 )); then
      (( BATTERY_AC_SINCE == 0 )) && BATTERY_AC_SINCE=$clock
      case "$BATTERY_STAGE" in
        1) threshold=$BATTERY_BUILD_HOLD_PCT ;;
        2) threshold=$BATTERY_WORK_PAUSE_PCT ;;
        3) threshold=$BATTERY_PREVIEW_SLEEP_PCT ;;
        *) threshold=$BATTERY_APP_SLEEP_PCT ;;
      esac
      if (( BATTERY_STAGE > 0 && clock - BATTERY_AC_SINCE >= BATTERY_RECOVERY_AC_SECONDS && cap >= threshold + BATTERY_HYSTERESIS_PCT )); then
        battery_resume_work; BATTERY_STAGE=0
      fi
    else BATTERY_AC_SINCE=0; fi
  fi
  battery_save
  (( BATTERY_STAGE > 0 )) || return 1
  if (( on_battery )) && [[ "$BATTERY_POLICY_ENABLED" == 1 ]]; then
    case "$BATTERY_STAGE" in
      1) state=battery_build_hold ;;
      2) state=battery_work_paused ;;
      3) state=battery_preview_sleep ;;
      *) state=battery_app_sleep ;;
    esac
    (( BATTERY_STAGE >= 2 )) && battery_pause_work
    write_state "$state" "battery policy tier $BATTERY_STAGE at ${cap}%; only explicitly enabled sleep targets may sleep"
  else
    write_state battery_recovery "waiting for stable AC and recovery charge; missing sensors do not authorize resume"
  fi
  return 0
}

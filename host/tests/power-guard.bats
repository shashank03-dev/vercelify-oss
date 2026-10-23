#!/usr/bin/env bats
load helpers

PG="$BATS_TEST_DIRNAME/../power-guard.sh"

setup() { setup_sandbox; cpu_temp 50; }
teardown() { teardown_sandbox; }

@test "on AC: state ok, nothing touched" {
  laptop 1 80 Charging
  run "$PG" --once
  [ "$status" -eq 0 ]
  [ "$(state)" = ok ]
  grep -q '"reason":"AC power, CPU 50°C"' "$STATE_DIR/power-guard.state"
  ! grep -q -E 'stop|pause|POWEROFF' "$CALLS"
}

@test "on battery above warn threshold: warning" {
  laptop 0 60 Discharging
  "$PG" --once
  [ "$(state)" = warning ]
  grep -q 'running on battery (60%)' "$STATE_DIR/power-guard.state"
}

@test "low battery needs two consecutive readings before shutdown" {
  laptop 0 14 Discharging
  "$PG" --once
  ! grep -q POWEROFF "$CALLS"
  [ "$(state)" = warning ]
  "$PG" --once
  grep -q POWEROFF "$CALLS"
  [ "$(state)" = shutting_down ]
}

@test "shutdown stops apps before Coolify core and remembers them" {
  laptop 0 10 Discharging
  "$PG" --once; "$PG" --once
  apps_line=$(grep -n 'docker stop -t 60 aaa ccc' "$CALLS" | cut -d: -f1)
  core_line=$(grep -n 'docker stop -t 60 bbb ddd' "$CALLS" | cut -d: -f1)
  off_line=$(grep -n POWEROFF "$CALLS" | cut -d: -f1)
  [ -n "$apps_line" ] && [ -n "$core_line" ] && [ -n "$off_line" ]
  [ "$apps_line" -lt "$core_line" ]
  [ "$core_line" -lt "$off_line" ]
  [ "$(sort -u "$PERSIST_DIR/power-guard.restore" | tr '\n' ' ')" = "aaa bbb ccc ddd " ]
}

@test "a reading back on AC resets the low-battery debounce" {
  laptop 0 12 Discharging; "$PG" --once
  laptop 1 12 Charging;    "$PG" --once
  laptop 0 12 Discharging; "$PG" --once
  ! grep -q POWEROFF "$CALLS"
}

@test "restore starts remembered containers when back on AC" {
  laptop 1 50 Charging
  printf 'aaa\nbbb\naaa\n' >"$PERSIST_DIR/power-guard.restore"
  "$PG" --restore
  grep -q 'docker start aaa bbb' "$CALLS"
  [ ! -e "$PERSIST_DIR/power-guard.restore" ]
}

@test "restore waits while battery is still low" {
  laptop 0 20 Discharging
  printf 'aaa\n' >"$PERSIST_DIR/power-guard.restore"
  "$PG" --restore
  ! grep -q 'docker start' "$CALLS"
  [ -s "$PERSIST_DIR/power-guard.restore" ]
}

@test "thermal: hot = builds paused, apps slowed, data and core untouched; resume when cool" {
  laptop 1 90 Charging
  echo 'eee 1a2b3c ghcr.io/coollabsio/coolify-helper:1.0.8' >>"$SANDBOX/containers"
  cpu_temp 93
  STUB_NANOCPUS=1500000000 "$PG" --once
  [ "$(state)" = thermal_paused ]
  grep -q 'docker pause eee' "$CALLS"                 # the build
  grep -q 'docker update --cpus .* aaa' "$CALLS"      # the app keeps serving, slower
  ! grep -q 'docker pause aaa' "$CALLS"
  ! grep -q -E 'docker (pause|update).* (bbb|ccc|ddd)$' "$CALLS"   # core + databases
  cpu_temp 85        # between resume and pause: stay in this state
  "$PG" --once
  [ "$(state)" = thermal_paused ]
  ! grep -q unpause "$CALLS"
  [ "$(grep -c 'docker update' "$CALLS")" -eq 1 ]    # not capped twice
  cpu_temp 70
  "$PG" --once
  [ "$(state)" = ok ]
  grep -q 'docker unpause eee' "$CALLS"
  grep -q 'docker update --cpus 1.50 aaa' "$CALLS"    # previous limit restored
}

@test "thermal: very hot freezes apps too, but never databases" {
  laptop 1 90 Charging
  cpu_temp 96
  "$PG" --once
  [ "$(state)" = thermal_paused ]
  grep -q 'docker pause aaa' "$CALLS"
  ! grep -q 'docker pause ccc' "$CALLS"
  ! grep -q 'docker pause ddd' "$CALLS"
  cpu_temp 60
  "$PG" --once
  grep -q 'docker unpause aaa' "$CALLS"
  grep -q 'docker update --cpus 0.00 aaa' "$CALLS"    # no limit before = no limit after
}

@test "thermal thresholds come from the config (Raspberry Pi profile)" {
  printf 'THERMAL_PAUSE_C=78\nTHERMAL_RESUME_C=68\nTHERMAL_SHUTDOWN_C=85\n' >"$POWER_GUARD_CONFIG"
  cpu_temp 80
  "$PG" --once
  [ "$(state)" = thermal_paused ]
  cpu_temp 83          # freeze defaults to shutdown - 3 = 82
  "$PG" --once
  grep -q 'docker pause aaa' "$CALLS"
}

@test "thermal: sustained critical temperature shuts down" {
  laptop 1 90 Charging
  cpu_temp 99
  "$PG" --once; "$PG" --once
  ! grep -q POWEROFF "$CALLS"
  "$PG" --once
  grep -q POWEROFF "$CALLS"
}

@test "peripheral batteries are ignored; desktops without battery are ok" {
  set_attr "$POWER_SUPPLY_DIR/hidpp_battery_0/type" Battery
  set_attr "$POWER_SUPPLY_DIR/hidpp_battery_0/scope" Device
  set_attr "$POWER_SUPPLY_DIR/hidpp_battery_0/capacity" 3
  set_attr "$POWER_SUPPLY_DIR/hidpp_battery_0/status" Discharging
  "$PG" --once; "$PG" --once
  [ "$(state)" = ok ]
  grep -q 'no battery' "$STATE_DIR/power-guard.state"
  ! grep -q POWEROFF "$CALLS"
}

@test "config file overrides thresholds" {
  echo 'BATTERY_SHUTDOWN_PCT=50' >"$POWER_GUARD_CONFIG"
  laptop 0 45 Discharging
  "$PG" --once; "$PG" --once
  grep -q POWEROFF "$CALLS"
}

@test "--status prints readings" {
  laptop 1 77 Charging
  run "$PG" --status
  [[ "$output" == *"ac_online=1 battery_present=1 battery_pct=77"* ]]
}

@test "thresholds changed from the app override the config file" {
  laptop 0 25 Discharging
  echo 'BATTERY_SHUTDOWN_PCT=15' >"$POWER_GUARD_CONFIG"
  "$PG" --once; "$PG" --once
  ! grep -q POWEROFF "$CALLS"
  echo 'BATTERY_SHUTDOWN_PCT=30' >"$POWER_GUARD_LOCAL_CONFIG"
  "$PG" --once; "$PG" --once
  grep -q POWEROFF "$CALLS"
}

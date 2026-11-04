#!/usr/bin/env bats
bats_require_minimum_version 1.5.0
load helpers
PG="$BATS_TEST_DIRNAME/../power-guard.sh"
setup() {
  setup_sandbox; cpu_temp 50
  export PATH="$SANDBOX/bin:$PATH"
  printf '1000\n' >"$SANDBOX/clock"
  cat >"$SANDBOX/bin/date" <<'STUB'
#!/usr/bin/env bash
if [[ "$1" == +%s ]]; then cat "$SANDBOX/clock"; else /usr/bin/date "$@"; fi
STUB
  chmod +x "$SANDBOX/bin/date"
  printf 'BATTERY_POLICY_ENABLED=1\n' >"$POWER_GUARD_CONFIG"
  printf 'aaa app-web node:22 platform\nbbb vercelify-proxy traefik:v3.7\nccc postgres-xyz postgres:17 platform\nddd vfy-registry registry:2\neee buildx_buildkit_vercelify0 moby/buildkit:v0.18.2\nfff unrelated-build moby/buildkit:v0.18.2\n' >"$SANDBOX/containers"
}
teardown() { teardown_sandbox; }
stable_readings() { "$PG" --once; "$PG" --once; }
@test "battery tiers are opt-in and require consecutive samples" {
  laptop 0 45 Discharging
  : >"$POWER_GUARD_CONFIG"; stable_readings; [ "$(state)" = warning ]
  printf 'BATTERY_POLICY_ENABLED=1\n' >"$POWER_GUARD_CONFIG"
  "$PG" --once; [ "$(state)" = warning ]
  "$PG" --once; [ "$(state)" = battery_build_hold ]
}
@test "tier escalation pauses only the installed builder and selects preview then app sleep" {
  laptop 0 35 Discharging; stable_readings
  [ "$(state)" = battery_work_paused ]
  grep -q 'docker pause eee' "$CALLS"
  run ! grep -q 'docker pause fff' "$CALLS"
  laptop 0 25 Discharging; stable_readings; [ "$(state)" = battery_preview_sleep ]
  laptop 0 18 Discharging; stable_readings; [ "$(state)" = battery_app_sleep ]
}
@test "AC bounce and clock rollback restart the stable recovery interval" {
  laptop 0 35 Discharging; stable_readings
  laptop 1 80 Charging; "$PG" --once; [ "$(state)" = battery_recovery ]
  printf '1030\n' >"$SANDBOX/clock"; laptop 0 35 Discharging; "$PG" --once
  laptop 1 80 Charging; "$PG" --once
  printf '990\n' >"$SANDBOX/clock"; "$PG" --once
  printf '1040\n' >"$SANDBOX/clock"; "$PG" --once
  run ! grep -q 'docker unpause eee' "$CALLS"
  printf '1051\n' >"$SANDBOX/clock"; "$PG" --once; [ "$(state)" = ok ]
  grep -q 'docker unpause eee' "$CALLS"
}
@test "missing sensors do not release remembered battery work" {
  laptop 0 35 Discharging; stable_readings
  rm -rf "$POWER_SUPPLY_DIR/AC" "$POWER_SUPPLY_DIR/BAT0"
  printf '2000\n' >"$SANDBOX/clock"; "$PG" --once
  [ "$(state)" = battery_recovery ]
  run ! grep -q 'docker unpause eee' "$CALLS"
}
@test "charge hysteresis retains a tier until its recovery charge headroom exists" {
  laptop 0 35 Discharging; stable_readings
  laptop 1 40 Charging; "$PG" --once
  printf '1100\n' >"$SANDBOX/clock"; "$PG" --once
  [ "$(state)" = battery_recovery ]
  laptop 1 46 Charging; "$PG" --once; [ "$(state)" = ok ]
}
@test "guard restart retains tier ownership and never resumes an already-paused builder" {
  laptop 0 35 Discharging
  STUB_PAUSED=true "$PG" --once; STUB_PAUSED=true "$PG" --once
  [ "$(state)" = battery_work_paused ]
  run ! grep -q 'docker pause eee' "$CALLS"
  laptop 1 80 Charging; "$PG" --once
  printf '1061\n' >"$SANDBOX/clock"; "$PG" --once
  [ "$(state)" = ok ]
  run ! grep -q 'docker unpause eee' "$CALLS"
}
@test "owned shutdown leaves unrelated containers out of its stop and restore manifest" {
  laptop 0 10 Discharging; stable_readings
  grep -q POWEROFF "$CALLS"
  run ! grep -q fff "$PERSIST_DIR/power-guard.restore"
  run ! grep -q 'docker stop.*fff' "$CALLS"
}
@test "post-shutdown restore waits for stable AC then starts only core" {
  laptop 1 80 Charging
  printf 'aaa\nbbb\n' >"$PERSIST_DIR/power-guard.restore"
  "$PG" --restore; [ "$(state)" = battery_recovery ]
  run ! grep -q 'docker start' "$CALLS"
  printf '1061\n' >"$SANDBOX/clock"; "$PG" --once
  [ "$(state)" = ok ]
  grep -q 'docker start bbb' "$CALLS"
  run ! grep -q 'docker start aaa' "$CALLS"
  [ ! -e "$PERSIST_DIR/power-guard.restore" ]
}
@test "turning policy off retains safe recovery without escalating new sleep tiers" {
  laptop 0 35 Discharging; stable_readings
  printf 'BATTERY_POLICY_ENABLED=0\n' >"$POWER_GUARD_CONFIG"
  laptop 0 18 Discharging; stable_readings
  [ "$(state)" = battery_recovery ]
  laptop 1 40 Charging; "$PG" --once
  printf '1061\n' >"$SANDBOX/clock"; "$PG" --once
  [ "$(state)" = battery_recovery ]
  laptop 1 46 Charging; "$PG" --once
  [ "$(state)" = ok ]
}

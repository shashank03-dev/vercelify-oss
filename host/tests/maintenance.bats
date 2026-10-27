#!/usr/bin/env bats
bats_require_minimum_version 1.5.0
# `vercelify maintenance`: the nightly reboot window and its guards.

setup() {
  T=$(mktemp -d)
  mkdir -p "$T/bin" "$T/etc" "$T/state" "$T/systemd" "$T/modules/$(uname -r)" "$T/run"
  printf 'role=main\nversion=test\n' >"$T/etc/install.env"
  printf '#!/usr/bin/env bash\necho "docker $*" >>"$CALLS"\n[[ "$1" == ps ]] && cat "$T/images" 2>/dev/null\nexit 0\n' >"$T/bin/docker"
  cat >"$T/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
echo "systemctl $*" >>"$CALLS"
[[ "$1" == is-active ]] && [[ " ${ACTIVE_UNITS:-} " == *" $3 "* ]] && exit 0
[[ "$1" == is-active ]] && exit 3
exit 0
STUB
  printf '#!/usr/bin/env bash\necho REBOOT >>"$CALLS"\n' >"$T/bin/fake-reboot"
  printf '#!/usr/bin/env bash\nexit 0\n' >"$T/bin/logger"
  chmod +x "$T/bin/"*
  echo '7200.5 100.0' >"$T/uptime"
  export CALLS="$T/calls" PATH="$T/bin:$PATH" NO_COLOR=1 VERCELIFY_TEST=1 T
  export VERCELIFY_ETC="$T/etc" VERCELIFY_STATE="$T/state" VERCELIFY_LOG="$T/setup.log" SYSTEMD_DIR="$T/systemd"
  export REBOOT_REQUIRED_FILE="$T/run/reboot-required" MODULES_DIR="$T/modules" PROC_UPTIME="$T/uptime"
  export POWER_GUARD_STATE="$T/run/power-guard.state" REBOOT_CMD="$T/bin/fake-reboot"
  : >"$CALLS"
  CLI="$BATS_TEST_DIRNAME/../vercelify"
}

teardown() { rm -rf "$T"; }

result() { jq -r .result "$T/state/maintenance.json"; }

@test "nothing to do without pending updates" {
  run "$CLI" maintenance run
  [ "$status" -eq 0 ]
  [ "$(result)" = none ]
  run ! grep -q REBOOT "$CALLS"
}

@test "reboots when updates need it and nothing is running" {
  touch "$REBOOT_REQUIRED_FILE"; echo linux-image-6.1 >"$REBOOT_REQUIRED_FILE.pkgs"
  run "$CLI" maintenance run
  [ "$status" -eq 0 ]
  grep -q REBOOT "$CALLS"
  [ "$(result)" = rebooting ]
  jq -r .detail "$T/state/maintenance.json" | grep -q 'linux-image-6.1'
}

@test "a kernel replaced since boot counts as needing a reboot" {
  rm -rf "$T/modules/$(uname -r)"; mkdir -p "$T/modules/9.9.9-new"
  run "$CLI" maintenance run
  grep -q REBOOT "$CALLS"
}

@test "a kernel the distro didn't provide (VPS, container) never reboots every night" {
  rm -rf "$T/modules/$(uname -r)"; mkdir -p "$T/modules/6.1.0-distro"
  touch -d '3 days ago' "$T/modules/6.1.0-distro"   # installed long before this boot (2 h ago)
  run "$CLI" maintenance run
  [ "$(result)" = none ]
  run ! grep -q REBOOT "$CALLS"
  rm -rf "$T/modules"
  run "$CLI" maintenance run
  [ "$(result)" = none ]
}

@test "never during a build, a backup, on battery, or right after boot" {
  touch "$REBOOT_REQUIRED_FILE"
  # the engine reports a build in progress
  echo token >"$T/etc/agent.token"
  printf '#!/usr/bin/env bash\ncat "$T/apps"; printf "\\n200"\n' >"$T/bin/curl"; chmod +x "$T/bin/curl"
  echo '[{"name":"shop","last_status":"healthy"},{"name":"blog","last_status":"building"}]' >"$T/apps"
  run "$CLI" maintenance run
  [ "$(result)" = skipped ]; jq -r .detail "$T/state/maintenance.json" | grep -q 'deployment is building'
  echo '[{"name":"shop","last_status":"healthy"}]' >"$T/apps"
  ACTIVE_UNITS=vercelify-backup run "$CLI" maintenance run
  jq -r .detail "$T/state/maintenance.json" | grep -q 'backup is running'
  echo '{"state":"warning","reason":"running on battery (60%)","updated_at":"x"}' >"$POWER_GUARD_STATE"
  run "$CLI" maintenance run
  jq -r .detail "$T/state/maintenance.json" | grep -q 'battery'
  rm "$POWER_GUARD_STATE"; echo '120.0 1.0' >"$T/uptime"
  run "$CLI" maintenance run
  jq -r .detail "$T/state/maintenance.json" | grep -q 'less than an hour'
  run ! grep -q REBOOT "$CALLS"
}

@test "off means off; on sets the window and the timer" {
  touch "$REBOOT_REQUIRED_FILE"
  run "$CLI" maintenance off
  [ "$status" -eq 0 ]
  grep -q 'disable --now vercelify-maintenance.timer' "$CALLS"
  run "$CLI" maintenance run
  run ! grep -q REBOOT "$CALLS"
  run "$CLI" maintenance on 03:15
  [ "$status" -eq 0 ]
  grep -q 'OnCalendar=\*-\*-\* 03:15' "$T/systemd/vercelify-maintenance.timer"
  grep -q '^AUTO_REBOOT=yes' "$T/etc/maintenance.conf"
  grep -q 'enable --now vercelify-maintenance.timer' "$CALLS"
  run "$CLI" maintenance on 25:00
  [ "$status" -ne 0 ]
  run "$CLI" maintenance status
  [[ "$output" == *"around 03:15"* && "$output" == *"reboot needed: security updates"* ]]
}

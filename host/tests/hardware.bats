#!/usr/bin/env bats
bats_require_minimum_version 1.5.0
# Hardware profiles, Pi hardening decisions, the registry commands and the staged
# update with rollback.

setup() {
  T=$(mktemp -d)
  export VERCELIFY_SOURCE_ONLY=1 VERCELIFY_DRY_RUN=0 NO_COLOR=1 VERCELIFY_LOG="$T/log"
  export DEVICE_TREE_MODEL="$T/model" CGROUP_CONTROLLERS="$T/controllers" PI_THROTTLED_FILE="$T/throttled"
  export MEMINFO_FILE="$T/meminfo" POWER_SUPPLY_DIR="$T/power_supply" PI_CMDLINE="$T/cmdline.txt"
  export DOCKER_DAEMON_JSON="$T/daemon.json" SYSTEMD_DIR="$T/systemd"
  mkdir -p "$T/power_supply" "$T/bin"
  printf '#!/usr/bin/env bash\necho "systemctl $*" >>"%s/calls"\n' "$T" >"$T/bin/systemctl"
  chmod +x "$T/bin/systemctl"
  PATH="$T/bin:$PATH"
  # shellcheck source=../setup-host.sh
  . "$BATS_TEST_DIRNAME/../setup-host.sh"
}

teardown() { rm -rf "$T"; }

mem() { printf 'MemTotal: %d kB\n' "$(( $1 * 1024 ))" >"$MEMINFO_FILE"; }

@test "profiles: Raspberry Pi 4/5, laptop, desktop" {
  printf 'Raspberry Pi 5 Model B Rev 1.0\0' >"$DEVICE_TREE_MODEL"
  [ "$(detect_profile)" = pi5 ]
  printf 'Raspberry Pi 4 Model B Rev 1.4\0' >"$DEVICE_TREE_MODEL"
  [ "$(detect_profile)" = pi4 ]
  rm "$DEVICE_TREE_MODEL"
  [ "$(detect_profile)" = pc ]
  mkdir -p "$POWER_SUPPLY_DIR/BAT0"
  [ "$(detect_profile)" = laptop ]
}

@test "Pi thresholds sit below the firmware's own throttling; laptops keep theirs" {
  mem 4096
  [ "$(profile_value pi4 pause)" = 78 ]
  [ "$(profile_value pi4 shutdown)" = 85 ]
  [ "$(profile_value laptop pause)" = 92 ]
  [ "$(profile_value pi4 builds)" = no ]
  [ "$(profile_value pi4 reserve_mb)" = 512 ]
  mem 8192
  [ "$(profile_value pi5 builds)" = yes ]
  [ "$(profile_value laptop reserve_mb)" = 1024 ]
  mem 2048
  [ "$(profile_value pi reserve_mb)" = 384 ]
}

@test "throttling bits are explained" {
  [ -z "$(describe_throttled 0)" ]
  [ "$(describe_throttled 50005)" = "under-voltage now, throttled now, under-voltage since boot, throttled since boot" ]
  echo 50000 >"$PI_THROTTLED_FILE"
  [ "$(pi_throttled)" = 50000 ]
}

@test "SD card roots are recognised" {
  on_sd_card /dev/mmcblk0p2
  run ! on_sd_card /dev/sda2
  run ! on_sd_card /dev/nvme0n1p2
}

@test "memory cgroup: added to cmdline.txt once, and a reboot is requested" {
  echo 'cpuset cpu io pids' >"$CGROUP_CONTROLLERS"
  echo 'console=tty1 root=PARTUUID=abc rootwait' >"$PI_CMDLINE"
  ensure_memory_cgroup 2>/dev/null
  [ "$NEEDS_REBOOT" = 1 ]
  [ "$(cat "$PI_CMDLINE")" = "console=tty1 root=PARTUUID=abc rootwait cgroup_enable=memory cgroup_memory=1" ]
  [ "$(wc -l <"$PI_CMDLINE")" -eq 1 ]
  ensure_memory_cgroup 2>/dev/null
  [ "$(grep -o cgroup_enable=memory "$PI_CMDLINE" | wc -l)" -eq 1 ]
  [ -f "$PI_CMDLINE.vercelify.bak" ]
  echo 'cpuset cpu io memory pids' >"$CGROUP_CONTROLLERS"
  NEEDS_REBOOT=0
  ensure_memory_cgroup 2>/dev/null
  [ "$NEEDS_REBOOT" = 0 ]
}

@test "containers get a memory-capped slice that leaves the OS its reserve" {
  mem 8000
  HW_PROFILE=laptop
  max=$(container_slice 2>/dev/null)
  [ "$max" = $((8000 - 1024)) ]
  grep -q "MemoryMax=${max}M" "$SYSTEMD_DIR/vercelify.slice"
  grep -q "MemoryHigh=$((max * 92 / 100))M" "$SYSTEMD_DIR/vercelify.slice"
}

@test "daemon.json: cgroup-parent and live-restore, but not with the cgroupfs driver" {
  echo 'memory' >"$CGROUP_CONTROLLERS"
  DOCKER_POOL=10.0.0.0/8 docker_daemon_config
  [ "$(jq -r '."cgroup-parent"' "$DOCKER_DAEMON_JSON")" = vercelify.slice ]
  [ "$(jq -r '."live-restore"' "$DOCKER_DAEMON_JSON")" = true ]
  echo '{"exec-opts":["native.cgroupdriver=cgroupfs"]}' >"$DOCKER_DAEMON_JSON"
  DOCKER_POOL=10.0.0.0/8 docker_daemon_config
  [ "$(jq -r '."cgroup-parent" // "none"' "$DOCKER_DAEMON_JSON")" = none ]
}

@test "the Coolify-era registry commands point at the engine's own registry" {
  . "$BATS_TEST_DIRNAME/../lib/registry.sh"
  run registry_cli install
  [ "$status" -ne 0 ]
  [[ "$output" == *"its own registry"* ]]
  run registry_cli trust laptop:5000
  [ "$status" -ne 0 ]
}

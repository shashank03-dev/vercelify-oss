#!/usr/bin/env bash
# Hardware profiles and board-specific hardening.
#
# The profile (pi5 | pi4 | pi | laptop | pc) decides defaults that differ
# between an old laptop and a Raspberry Pi: thermal thresholds, swap kind,
# memory reserved for the OS and whether builds should run on the node.
# Every path is overridable so the bats suite can run against fixtures.

: "${DEVICE_TREE_MODEL:=/proc/device-tree/model}"
: "${CGROUP_CONTROLLERS:=/sys/fs/cgroup/cgroup.controllers}"
: "${PI_THROTTLED_FILE:=/sys/devices/platform/soc/soc:firmware/get_throttled}"
: "${MEMINFO_FILE:=/proc/meminfo}"
: "${POWER_SUPPLY_DIR:=/sys/class/power_supply}"
: "${PI_CMDLINE:=}"
: "${WATCHDOG_DEV:=/dev/watchdog0}"
: "${SYSTEMD_CONF_DIR:=/etc/systemd}"

HW_PROFILE=${HW_PROFILE:-}
NEEDS_REBOOT=0

board_model() { [[ -r "$DEVICE_TREE_MODEL" ]] && tr -d '\0' <"$DEVICE_TREE_MODEL"; return 0; }

mem_total_mb() { awk '/^MemTotal:/ {print int($2/1024)}' "$MEMINFO_FILE" 2>/dev/null || echo 0; }

# Board family of this machine.
detect_profile() {
  local model
  model=$(board_model)
  case "$model" in
    *"Raspberry Pi 5"*|*"Raspberry Pi Compute Module 5"*) echo pi5 ;;
    *"Raspberry Pi 4"*|*"Raspberry Pi Compute Module 4"*) echo pi4 ;;  # also matches the Pi 400
    *"Raspberry Pi"*) echo pi ;;
    *)
      if compgen -G "$POWER_SUPPLY_DIR/BAT*" >/dev/null; then echo laptop; else echo pc; fi ;;
  esac
}

is_pi() { [[ "${1:-$HW_PROFILE}" == pi* ]]; }

# profile_value PROFILE KEY — the default for KEY on that profile.
# Keys: pause resume shutdown reserve_mb builds
profile_value() {
  local p=$1 key=$2 mem
  mem=$(mem_total_mb)
  case "$key" in
    pause) if is_pi "$p"; then echo 78; else echo 92; fi ;;
    resume) if is_pi "$p"; then echo 68; else echo 80; fi ;;
    shutdown) if is_pi "$p"; then echo 85; else echo 98; fi ;;
    reserve_mb)
      if (( mem < 2500 )); then echo 384
      elif [[ "$p" == pi4 || "$p" == pi ]]; then echo 512
      elif [[ "$p" == pi5 ]]; then echo 768
      else echo 1024; fi ;;
    builds)
      # Building a Next.js app needs ~2 GB on top of what already runs.
      if (( mem < 5000 )) && is_pi "$p"; then echo no; else echo yes; fi ;;
    *) return 1 ;;
  esac
}

# Block device holding /, e.g. /dev/mmcblk0p2.
root_device() { findmnt -no SOURCE / 2>/dev/null | head -1; }

# SD card or eMMC: flash that wears out under constant small writes.
on_sd_card() { [[ "${1:-$(root_device)}" == /dev/mmcblk* ]]; }

# The Raspberry Pi firmware's throttling bits (hex), empty when unknown.
pi_throttled() {
  local v=''
  if [[ -r "$PI_THROTTLED_FILE" ]]; then v=$(tr -d '\0\n ' <"$PI_THROTTLED_FILE")
  elif have vcgencmd; then v=$(vcgencmd get_throttled 2>/dev/null | sed -n 's/^throttled=//p'); fi
  [[ -n "$v" ]] && printf '%s\n' "${v#0x}"
  return 0
}

# Human description of throttling bits; empty when all clear.
describe_throttled() {
  local hex=${1:-} n out=()
  [[ "$hex" =~ ^[0-9a-fA-F]+$ ]] || return 0
  n=$(( 16#$hex ))
  (( n & 0x1 )) && out+=("under-voltage now")
  (( n & 0x4 )) && out+=("throttled now")
  (( n & 0x8 )) && out+=("at the soft temperature limit")
  (( n & 0x10000 )) && out+=("under-voltage since boot")
  (( n & 0x40000 )) && out+=("throttled since boot")
  local i
  for i in "${!out[@]}"; do
    (( i )) && printf ', '
    printf '%s' "${out[i]}"
  done
}

memory_cgroup_enabled() {
  [[ -r "$CGROUP_CONTROLLERS" ]] || return 0  # cgroup v1 or unknown: assume yes
  grep -qw memory "$CGROUP_CONTROLLERS"
}

pi_cmdline_file() {
  if [[ -n "$PI_CMDLINE" ]]; then echo "$PI_CMDLINE"; return; fi
  local f
  for f in /boot/firmware/cmdline.txt /boot/cmdline.txt; do [[ -f "$f" ]] && { echo "$f"; return; }; done
}

# Docker memory limits are silently ignored without the memory controller,
# which Raspberry Pi kernels have shipped disabled. Turn it on (next boot).
ensure_memory_cgroup() {
  memory_cgroup_enabled && { ok "memory limits enforceable (cgroup memory controller on)"; return 0; }
  local f
  f=$(pi_cmdline_file)
  if [[ -z "$f" ]]; then
    warn "the kernel's memory cgroup is off — container memory limits will be ignored (add cgroup_enable=memory to the kernel command line)"
    return 0
  fi
  if ! grep -qw 'cgroup_enable=memory' "$f"; then
    if [[ "$VERCELIFY_DRY_RUN" == 1 ]]; then
      info "[dry-run] would add cgroup_enable=memory cgroup_memory=1 to $f"
    else
      [[ -f "$f.vercelify.bak" ]] || cp -p "$f" "$f.vercelify.bak"
      # cmdline.txt is a single line; append to it.
      sed -i '1 s/[[:space:]]*$/ cgroup_enable=memory cgroup_memory=1/' "$f"
    fi
  fi
  # shellcheck disable=SC2034  # read by setup-host.sh's summary
  NEEDS_REBOOT=1
  warn "enabled the memory cgroup in $f — reboot once so container memory limits work"
}

# Compressed swap in RAM: no writes to flash, and faster than a swapfile.
setup_zram() {
  local mem size
  mem=$(mem_total_mb)
  size=$(( mem / 2 )); (( size > 4096 )) && size=4096
  if apt_install systemd-zram-generator 2>/dev/null; then
    write_file "$SYSTEMD_CONF_DIR/zram-generator.conf" 0644 <<EOF && run_cmd systemctl daemon-reload || true
# Vercelify: compressed swap in RAM (no flash wear)
[zram0]
zram-size = $size
compression-algorithm = zstd
swap-priority = 100
EOF
    run_cmd systemctl start systemd-zram-setup@zram0.service 2>/dev/null || true
  elif apt_install zram-tools 2>/dev/null; then
    write_file /etc/default/zramswap 0644 <<EOF && run_cmd systemctl restart zramswap 2>/dev/null || true
# Vercelify: compressed swap in RAM
ALGO=zstd
SIZE=$size
PRIORITY=100
EOF
  else
    warn "could not install zram — running without swap"
    return 0
  fi
  # zram is cheap to use: let the kernel swap idle pages to it.
  write_file "$SYSCTL_DIR/91-vercelify-zram.conf" 0644 <<'EOF' && run_cmd sysctl --system >/dev/null 2>&1 || true
vm.swappiness = 100
vm.page-cluster = 0
EOF
  ok "zram swap: ${size} MB (zstd)"
}

# Keep logs from wearing out an SD card and from filling small disks.
limit_journal() {
  local extra=''
  on_sd_card && extra=$'SyncIntervalSec=5m\n'
  write_file "$SYSTEMD_CONF_DIR/journald.conf.d/90-vercelify.conf" 0644 <<EOF && run_cmd systemctl restart systemd-journald 2>/dev/null || true
# Vercelify: bounded logs
[Journal]
SystemMaxUse=200M
${extra}
EOF
}

# Boards without a battery-backed clock boot with a stale time after a power
# cut; TLS and git fail until NTP catches up. Wait for it (bounded).
ensure_time_sync() {
  if ! have chronyd && ! systemctl list-unit-files 2>/dev/null | grep -q '^ntp\.service\|^ntpsec\.service'; then
    apt_install systemd-timesyncd 2>/dev/null || true
    run_cmd systemctl enable --now systemd-timesyncd >/dev/null 2>&1 || true
  fi
  is_pi || return 0
  write_file "$SYSTEMD_DIR/systemd-time-wait-sync.service.d/90-vercelify.conf" 0644 <<'EOF' || true
# Vercelify: don't hold the boot forever when offline.
[Service]
TimeoutStartSec=90
EOF
  run_cmd systemctl enable systemd-time-wait-sync.service >/dev/null 2>&1 || true
  ok "services wait (up to 90 s) for the clock to sync after boot"
}

# A hung kernel or a stuck disk reboots the machine instead of leaving it dead.
enable_watchdog() {
  [[ -e "$WATCHDOG_DEV" || -e /dev/watchdog ]] || { info "no hardware watchdog found — skipping"; return 0; }
  if write_file "$SYSTEMD_CONF_DIR/system.conf.d/90-vercelify-watchdog.conf" 0644 <<'EOF'
# Vercelify: reboot automatically if the system hangs.
[Manager]
RuntimeWatchdogSec=30s
RebootWatchdogSec=2min
EOF
  then
    run_cmd systemctl daemon-reexec 2>/dev/null || true
  fi
  ok "hardware watchdog armed (reboots after a 30 s hang)"
}

# Wi-Fi power saving drops packets and connections on servers.
tune_wifi() {
  local dev
  have ip || return 0
  dev=$(ip -4 route show default 2>/dev/null | awk '{for (i=1;i<NF;i++) if ($i=="dev") {print $(i+1); exit}}') || true
  [[ "$dev" == wl* ]] || return 0
  warn "this machine reaches the internet over Wi-Fi ($dev) — Ethernet is much more reliable for a server"
  if [[ -d /etc/NetworkManager ]]; then
    write_file /etc/NetworkManager/conf.d/90-vercelify-wifi.conf 0644 <<'EOF' && run_cmd systemctl reload NetworkManager 2>/dev/null || true
# Vercelify: no Wi-Fi power saving on a server
[connection]
wifi.powersave = 2
EOF
  fi
  have iw && run_cmd iw dev "$dev" set power_save off 2>/dev/null || true
}

# All containers live in vercelify.slice, capped below total RAM, so sshd,
# tailscaled and the agent always have memory left when an app leaks.
container_slice() {
  local mem reserve max high
  mem=$(mem_total_mb)
  reserve=$(profile_value "$HW_PROFILE" reserve_mb)
  max=$(( mem - reserve ))
  (( max < 512 )) && max=512
  high=$(( max * 92 / 100 ))
  write_file "$SYSTEMD_DIR/vercelify.slice" 0644 <<EOF || true
[Unit]
Description=Vercelify containers (memory-capped so the host stays reachable)

[Slice]
MemoryAccounting=yes
MemoryHigh=${high}M
MemoryMax=${max}M
EOF
  run_cmd systemctl daemon-reload
  run_cmd systemctl start vercelify.slice 2>/dev/null || true
  echo "$max"
}

# Hardware-specific setup; run after the kernel tuning step.
harden_hardware() {
  local root t desc model
  model=$(board_model | head -c 60)
  step "Hardware (${HW_PROFILE}${model:+: $model})"
  root=$(root_device)
  if on_sd_card "$root"; then
    warn "the system runs from an SD card ($root): it wears out under a server's writes."
    warn "  Boot from a USB SSD or an NVMe HAT for anything you care about."
  fi
  if is_pi; then
    t=$(pi_throttled)
    desc=$(describe_throttled "$t")
    if [[ -n "$desc" ]]; then warn "Raspberry Pi power: $desc — use the official power supply"; else ok "power supply OK (no under-voltage)"; fi
    ensure_memory_cgroup
  fi
  if is_pi || on_sd_card "$root"; then setup_zram; else ensure_swap; fi
  limit_journal
  ensure_time_sync
  enable_watchdog
  tune_wifi
  if [[ "$(profile_value "$HW_PROFILE" builds)" == no ]]; then
    info "builds: this machine is small — pull prebuilt images to it instead of building here"
  fi
}

#!/usr/bin/env bash
# Base packages, laptop-as-server tuning, kernel parameters and swap.

# Right after boot Debian's own apt-daily / unattended-upgrades (or cloud-init)
# hold apt's locks for a few minutes: wait for them instead of failing.
APT_LOCK_WAIT=${APT_LOCK_WAIT:-600}
APT_LOCK_POLL=${APT_LOCK_POLL:-5}

# Another apt or dpkg is running. Not unattended-upgrade-shutdown, which waits
# for shutdown all the time (the kernel names it `unattended-upgr` too).
apt_busy() {
  pgrep -x 'apt|apt-get|aptitude|dpkg' >/dev/null 2>&1 ||
    pgrep -f '/unattended-upgrade( |$)' >/dev/null 2>&1
}

apt_wait_idle() {
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] && return 0
  local waited=0
  while apt_busy; do
    (( waited == 0 )) && info "waiting for another package install (system updates) to finish…"
    (( waited >= APT_LOCK_WAIT )) && { warn "still running after ${APT_LOCK_WAIT}s — trying anyway"; return 0; }
    sleep "$APT_LOCK_POLL"
    waited=$(( waited + APT_LOCK_POLL ))
  done
}

apt_get() {
  apt_wait_idle
  run_cmd env DEBIAN_FRONTEND=noninteractive apt-get -o "DPkg::Lock::Timeout=$APT_LOCK_WAIT" "$@"
}

# Package lists are refreshed only when something is missing: a re-run or an
# update with everything installed doesn't touch the network for apt.
apt_install() {
  local missing=() p
  for p in "$@"; do dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p"); done
  (( ${#missing[@]} )) || return 0
  apt_update_once
  info "installing: ${missing[*]}"
  apt_get install -y -qq --no-install-recommends "${missing[@]}" >/dev/null
}

# A list that can't be fetched (a third-party repository without its key, a
# mirror timing out) isn't fatal: apt keeps the lists it had, and an install
# that really needs the missing one fails with its own message. The errors
# go to the log, not the screen.
apt_update_once() {
  [[ -n "${_APT_UPDATED:-}" ]] && return 0
  _APT_UPDATED=1
  local errs
  apt_wait_idle   # its message on screen, not in the captured errors
  info "refreshing package lists…"
  errs=$(mktemp)
  if ! retry 3 3 apt_get update -qq >/dev/null 2>"$errs"; then
    warn "some package lists could not be refreshed (network, or a repository's key); using the ones already here — details in $VERCELIFY_LOG"
  elif grep -q '^[WE]:' "$errs"; then
    info "some package lists could not be refreshed; using the ones already here"
  fi
  _log_file "apt-get update: $(tr '\n' ' ' <"$errs")"
  rm -f "$errs"
  return 0
}

# apt_available PKG — the configured repositories offer PKG.
apt_available() {
  local c
  c=$(apt-cache policy "$1" 2>/dev/null | sed -n 's/^ *Candidate: *//p' | head -1)
  [[ -n "$c" && "$c" != "(none)" ]]
}

install_base_packages() {
  step "Base packages"
  apt_install ca-certificates curl gnupg jq util-linux e2fsprogs parted iptables \
    openssh-server unattended-upgrades qrencode rsync git smartmontools
  # Security updates install automatically; nothing else changes underneath you.
  write_file /etc/apt/apt.conf.d/20auto-upgrades 0644 <<'EOF' || true
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
EOF
  ok "base packages ready (unattended security upgrades on)"
}

: "${LOGIND_DROPIN:=/etc/systemd/logind.conf.d/90-vercelify.conf}"

# A laptop server must not suspend when the lid closes or on idle.
laptop_server_mode() {
  step "Laptop server mode"
  if write_file "$LOGIND_DROPIN" 0644 <<'EOF'
# Vercelify: keep running with the lid closed and on idle.
[Login]
HandleLidSwitch=ignore
HandleLidSwitchExternalPower=ignore
HandleLidSwitchDocked=ignore
HandleSuspendKey=ignore
HandleHibernateKey=ignore
IdleAction=ignore
EOF
  then
    # Restarting logind would kill a desktop session; reload is enough for lid handling.
    run_cmd systemctl kill -s HUP systemd-logind 2>/dev/null || true
  fi
  # Uninstall unmasks them again, unless they were masked before Vercelify.
  # A dry run leaves no markers, or a later real install would misread them.
  if [[ "$VERCELIFY_DRY_RUN" == 1 ]]; then
    run_cmd systemctl mask --quiet sleep.target suspend.target hibernate.target hybrid-sleep.target
    ok "lid close / suspend disabled"
    return 0
  fi
  if [[ "$(systemctl is-enabled suspend.target 2>/dev/null || true)" == masked && ! -e "$VERCELIFY_ETC/sleep.vercelify" ]]; then
    mkdir -p "$VERCELIFY_ETC" && : >"$VERCELIFY_ETC/sleep-masked-before"
  fi
  run_cmd systemctl mask --quiet sleep.target suspend.target hibernate.target hybrid-sleep.target 2>/dev/null || true
  { mkdir -p "$VERCELIFY_ETC" && : >"$VERCELIFY_ETC/sleep.vercelify"; } 2>/dev/null || true
  # Blank the console after 60s so the panel isn't lit 24/7 (mpv wakes it).
  { echo 60 >/sys/module/kernel/parameters/consoleblank; } 2>/dev/null || true
  ok "lid close / suspend disabled"
}

tune_kernel() {
  step "Kernel & memory tuning"
  write_file "$SYSCTL_DIR/90-vercelify.conf" 0644 <<'EOF' || true
# Vercelify host tuning (8 GB RAM laptop running Docker builds)
vm.swappiness = 10
vm.vfs_cache_pressure = 50
vm.dirty_background_ratio = 5
vm.dirty_ratio = 15
# File watchers for Next.js/Vite builds
fs.inotify.max_user_watches = 524288
fs.inotify.max_user_instances = 1024
# Bigger socket buffers: mesh (WireGuard) throughput + display stream receive
net.core.rmem_max = 26214400
net.core.wmem_max = 26214400
net.core.netdev_max_backlog = 5000
# Containers and the mesh
net.ipv4.ip_forward = 1
net.ipv6.conf.all.forwarding = 1
EOF
  run_cmd sysctl --system >/dev/null 2>&1 || warn "sysctl --system reported errors"
  ok "vm.swappiness=10, inotify and network buffers raised"
}

# 4 GB swapfile on the SSD when the machine has no swap at all.
ensure_swap() {
  if [[ -n "$(swapon --noheadings --show 2>/dev/null)" ]]; then
    ok "swap present: $(swapon --noheadings --show=SIZE 2>/dev/null | paste -sd+ -)"
    return 0
  fi
  local fstype size_gb=4 avail_gb
  fstype=$(findmnt -no FSTYPE / 2>/dev/null)
  case "$fstype" in
    overlay|tmpfs|zfs|squashfs|nfs*|fuse*)
      warn "root filesystem is $fstype — skipping the swapfile (add swap or zram yourself)"
      return 0 ;;
  esac
  avail_gb=$(( $(df --output=avail -k / | tail -1) / 1024 / 1024 ))
  (( avail_gb < 30 )) && size_gb=2
  if [[ "$fstype" == btrfs ]]; then
    run_cmd btrfs filesystem mkswapfile --size "${size_gb}g" /swapfile >/dev/null || { warn "could not create btrfs swapfile"; return 0; }
  else
    run_cmd fallocate -l "${size_gb}G" /swapfile 2>/dev/null || run_cmd dd if=/dev/zero of=/swapfile bs=1M count=$((size_gb * 1024)) status=none
    run_cmd chmod 600 /swapfile
    run_cmd mkswap /swapfile >/dev/null
  fi
  if ! run_cmd swapon /swapfile 2>/dev/null; then
    warn "swapon failed on this filesystem — continuing without swap"
    run_cmd rm -f /swapfile
    return 0
  fi
  grep -qs "^/swapfile[[:space:]]" "$FSTAB_FILE" || fstab_set none_swapfile "/swapfile none swap sw 0 0" >/dev/null || true
  ok "created ${size_gb} GB swapfile"
}

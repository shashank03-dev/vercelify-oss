#!/usr/bin/env bash
# Base packages, laptop-as-server tuning, kernel parameters and swap.

apt_install() {
  local missing=() p
  for p in "$@"; do dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p"); done
  (( ${#missing[@]} )) || return 0
  info "installing: ${missing[*]}"
  run_cmd env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends "${missing[@]}" >/dev/null
}

apt_update_once() {
  [[ -n "${_APT_UPDATED:-}" ]] && return 0
  retry 3 3 run_cmd env DEBIAN_FRONTEND=noninteractive apt-get update -qq >/dev/null || die "apt-get update failed"
  _APT_UPDATED=1
}

install_base_packages() {
  step "Base packages"
  apt_update_once
  apt_install ca-certificates curl gnupg jq util-linux e2fsprogs parted iptables \
    openssh-server unattended-upgrades qrencode rsync git smartmontools
  # Security updates install automatically; nothing else changes underneath you.
  write_file /etc/apt/apt.conf.d/20auto-upgrades 0644 <<'EOF' || true
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
EOF
  ok "base packages ready (unattended security upgrades on)"
}

# A laptop server must not suspend when the lid closes or on idle.
laptop_server_mode() {
  step "Laptop server mode"
  if write_file /etc/systemd/logind.conf.d/90-vercelify.conf 0644 <<'EOF'
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
  run_cmd systemctl mask --quiet sleep.target suspend.target hibernate.target hybrid-sleep.target 2>/dev/null || true
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
# Bigger socket buffers: Tailscale throughput + display stream receive
net.core.rmem_max = 26214400
net.core.wmem_max = 26214400
net.core.netdev_max_backlog = 5000
# Containers and Tailscale subnet routes
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

# Let Coolify (and `vercelify add-worker`) log in as root with keys only.
configure_ssh() {
  write_file /etc/ssh/sshd_config.d/90-vercelify.conf 0644 <<'EOF' && run_cmd systemctl reload ssh 2>/dev/null || true
# Vercelify: Coolify manages servers over SSH as root using keys.
PermitRootLogin prohibit-password
PubkeyAuthentication yes
EOF
}

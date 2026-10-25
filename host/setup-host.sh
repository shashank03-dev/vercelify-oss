#!/usr/bin/env bash
# Vercelify host setup — turns a Debian/Ubuntu machine into a self-hosted
# Vercel: Coolify + Docker + Tailscale + storage tiers + the cockpit agent.
#
# Re-runnable at any time: every step checks what is already done.
# Run `setup-host.sh --help` for options.

VERCELIFY_SRC=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$VERCELIFY_SRC/Cargo.toml" 2>/dev/null | head -1)
VERSION=${VERSION:-dev}

# shellcheck source=lib/common.sh
. "$VERCELIFY_SRC/host/lib/common.sh"
# shellcheck source=lib/system.sh
. "$VERCELIFY_SRC/host/lib/system.sh"
# shellcheck source=lib/hardware.sh
. "$VERCELIFY_SRC/host/lib/hardware.sh"
# shellcheck source=lib/storage.sh
. "$VERCELIFY_SRC/host/lib/storage.sh"
# shellcheck source=lib/docker.sh
. "$VERCELIFY_SRC/host/lib/docker.sh"
# shellcheck source=lib/tailscale.sh
. "$VERCELIFY_SRC/host/lib/tailscale.sh"
# shellcheck source=lib/coolify.sh
. "$VERCELIFY_SRC/host/lib/coolify.sh"
# shellcheck source=lib/cloudflare.sh
. "$VERCELIFY_SRC/host/lib/cloudflare.sh"
# shellcheck source=lib/agent.sh
. "$VERCELIFY_SRC/host/lib/agent.sh"
# shellcheck source=lib/backup.sh
. "$VERCELIFY_SRC/host/lib/backup.sh"
# shellcheck source=lib/maintenance.sh
. "$VERCELIFY_SRC/host/lib/maintenance.sh"

usage() {
  cat <<EOF
Vercelify host setup $VERSION

Usage: sudo $0 [options]

Most people need no options: the installer asks a few questions.

Role
  --worker                 Worker node (e.g. Raspberry Pi): Docker + Tailscale +
                           agent only; deployments are managed by the main host
  --coolify-pubkey KEY     (worker) Coolify's SSH public key to authorise for root

Network & domain
  --hostname NAME          Machine name on your tailnet (default: $(hostname -s 2>/dev/null || echo host))
  --tailscale-authkey KEY  Join Tailscale without opening a login link
  --domain DOMAIN          Your domain: apps get *.DOMAIN (e.g. blog.DOMAIN)
  --cloudflare-token TOK   Cloudflare API token: creates a tunnel + DNS for you
                           (no port forwarding, works behind CGNAT)
  --force-dns              Replace existing DNS records for *.DOMAIN and DOMAIN

Storage
  --storage-device DEV     Disk for databases/volumes, mounted at /mnt/storage
                           (default: auto-detect; 'none' = single-disk mode)
  --format-storage         Allow formatting that disk as ext4 (ERASES IT)

Coolify
  --admin-email EMAIL      Dashboard login (default: admin@example.com; the domain
                           must exist in DNS — Coolify checks it)
  --admin-password PASS    Dashboard password (default: generated, shown at the end)

Features
  --ai [MODEL]             Also install Ollama for local AI models, and download
                           MODEL (default: llama3.2:3b, or gemma3:1b under 8 GB)
  --no-firewall            Don't restrict the dashboard and AI API to Tailscale
  --no-power-guard         Don't install the battery/thermal guard
  --no-display             Don't install mpv (secondary-monitor receiver)
  --no-backup              Don't turn on nightly backups (main server)

General
  -y, --yes                Accept all defaults, never prompt
  --dry-run                Print what would be done without changing anything
  -h, --help               Show this help

Examples
  sudo $0
  sudo $0 --domain example.com --cloudflare-token cf_XXXX -y
  sudo $0 --worker --hostname pi5
EOF
}

# --- options -----------------------------------------------------------------
ROLE=main
TS_HOSTNAME=$(hostname -s 2>/dev/null || echo vercelify)
TS_AUTHKEY=${TS_AUTHKEY:-}
DOMAIN=''
# cloudflare | direct — remembered in install.env so updates keep the domain
DOMAIN_MODE=''
CF_API_TOKEN=${CF_API_TOKEN:-}
FORCE_DNS=0
STORAGE_DEVICE=auto
FORMAT_STORAGE=0
ADMIN_EMAIL=''
ADMIN_PASSWORD=''
COOLIFY_PUBKEY=''
WITH_FIREWALL=1
WITH_POWER_GUARD=1
WITH_DISPLAY=1
WITH_AI=0
AI_MODEL=''
WITH_BACKUP=1
ASSUME_YES=${ASSUME_YES:-0}

parse_args() {
  while (( $# )); do
    case "$1" in
      --worker) ROLE=worker ;;
      --coolify-pubkey) COOLIFY_PUBKEY=${2:?}; shift ;;
      --hostname) TS_HOSTNAME=${2:?}; shift ;;
      --tailscale-authkey) TS_AUTHKEY=${2:?}; shift ;;
      --domain) DOMAIN=${2:?}; shift ;;
      --cloudflare-token) CF_API_TOKEN=${2:?}; shift ;;
      --force-dns) FORCE_DNS=1 ;;
      --storage-device) STORAGE_DEVICE=${2:?}; shift ;;
      --format-storage) FORMAT_STORAGE=1 ;;
      --admin-email) ADMIN_EMAIL=${2:?}; shift ;;
      --admin-password) ADMIN_PASSWORD=${2:?}; shift ;;
      --no-firewall) WITH_FIREWALL=0 ;;
      --no-power-guard) WITH_POWER_GUARD=0 ;;
      --no-display) WITH_DISPLAY=0 ;;
      --no-backup) WITH_BACKUP=0 ;;
      --ai)
        WITH_AI=1
        if [[ -n "${2:-}" && "$2" != -* ]]; then AI_MODEL=$2; shift; fi
        ;;
      -y|--yes) ASSUME_YES=1; VERCELIFY_NONINTERACTIVE=1 ;;
      --dry-run) VERCELIFY_DRY_RUN=1 ;;
      -h|--help) usage; exit 0 ;;
      *) usage >&2; die "unknown option: $1" ;;
    esac
    shift
  done
  DOMAIN=${DOMAIN,,}
  DOMAIN=${DOMAIN#http://}; DOMAIN=${DOMAIN#https://}; DOMAIN=${DOMAIN%/}
  DOMAIN=${DOMAIN#\*.}
  export ASSUME_YES VERCELIFY_NONINTERACTIVE VERCELIFY_DRY_RUN
}

valid_domain() { [[ "$1" =~ ^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$ ]]; }
valid_hostname() { [[ "$1" =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$ ]]; }

# --- steps -------------------------------------------------------------------

preflight() {
  step "Preflight"
  [[ $EUID -eq 0 || "$VERCELIFY_DRY_RUN" == 1 ]] || die "run as root: sudo $0 $*"
  detect_os
  local level=0
  os_support_level || level=$?
  case $level in
    0) ok "$OS_PRETTY" ;;
    1) warn "$OS_PRETTY is not tested — continuing (Debian 12/13, Ubuntu 22.04/24.04 are)" ;;
    *) die "$OS_PRETTY is not supported (need Debian or Ubuntu)" ;;
  esac
  HW_PROFILE=$(detect_profile)
  HW_MODEL=$(board_model)
  ok "hardware profile: $HW_PROFILE${HW_MODEL:+ ($HW_MODEL)}"
  ARCH=$(detect_arch)
  [[ "$ARCH" == amd64 || "$ARCH" == arm64 ]] || die "CPU architecture $(uname -m) is not supported (need x86_64 or 64-bit ARM; on a Pi install the 64-bit OS)"
  ok "architecture $ARCH"

  local mem_mb disk_gb
  mem_mb=$(awk '/MemTotal/ {print int($2/1024)}' /proc/meminfo)
  disk_gb=$(( $(df --output=avail -k / | tail -1) / 1024 / 1024 ))
  if (( mem_mb < 1800 )); then die "at least 2 GB RAM is required (found ${mem_mb} MB)"; fi
  (( mem_mb < 3800 )) && [[ $ROLE == main ]] && warn "${mem_mb} MB RAM: Coolify works but builds will be slow; 4 GB+ recommended"
  ok "${mem_mb} MB RAM, ${disk_gb} GB free on /"
  (( disk_gb < 20 )) && [[ $ROLE == main ]] && warn "less than 20 GB free on / — Docker images and builds need room"

  curl -fsS -m 10 -o /dev/null https://get.docker.com || die "no internet access (https://get.docker.com unreachable)"
  ok "internet access"

  if [[ -r /proc/sys/net/ipv6/conf/all/disable_ipv6 && "$(cat /proc/sys/net/ipv6/conf/all/disable_ipv6)" == 1 ]] ||
     [[ ! -e /proc/net/if_inet6 ]]; then
    warn "IPv6 is disabled in the kernel — Coolify's web server needs it; enable IPv6 (remove ipv6.disable=1) if the dashboard never becomes healthy"
  fi
  valid_hostname "$TS_HOSTNAME" || die "invalid --hostname '$TS_HOSTNAME' (lowercase letters, digits, '-')"
}

wizard() {
  has_tty || return 0
  [[ "$ASSUME_YES" == 1 ]] && return 0
  printf '\n%s%s Vercelify %s — self-hosted Vercel on this machine%s\n' "$C_BOLD" "$C_CYAN" "$VERSION" "$C_RESET" >&2
  printf '%s   Role: %s · Host: %s · Answer a few questions (Enter = default).%s\n' "$C_DIM" "$ROLE" "$TS_HOSTNAME" "$C_RESET" >&2
  if (( ! WITH_AI )); then
    printf '\n  Vercelify can also run open AI models (Llama, Qwen, Gemma…) on this machine,\n  for your Mac and your apps. About 4 GB to download (~12 GB on disk); you can add it later.\n' >&2
    if confirm "Set up local AI models too?" n; then WITH_AI=1; fi
  fi
  [[ $ROLE == worker ]] && return 0

  if [[ -z "$ADMIN_EMAIL" ]] && ! coolify_installed 2>/dev/null; then
    ask ADMIN_EMAIL "E-mail for your Coolify login (any real address):" "admin@example.com"
  fi
  if [[ -z "$DOMAIN" ]]; then
    printf '\n  Apps need addresses. Without a domain they are private to your tailnet\n  (http://<app>.<tailscale-ip>.sslip.io). With one they get https://<app>.<domain>.\n' >&2
    ask DOMAIN "Your domain (blank = private only, add one later with 'vercelify domain'):" ""
    DOMAIN=${DOMAIN,,}
  fi
  if [[ -n "$DOMAIN" && -z "$CF_API_TOKEN" && -z "$DOMAIN_MODE" ]]; then
    printf '\n  Is %s on Cloudflare? Paste an API token and Vercelify creates a tunnel + DNS\n  records for you — no router port forwarding, works on any ISP.\n  Create one at https://dash.cloudflare.com/profile/api-tokens with:\n    Account › Cloudflare Tunnel › Edit, Zone › DNS › Edit, Zone › Zone › Read\n' "$DOMAIN" >&2
    ask_secret CF_API_TOKEN "Cloudflare API token (blank = I'll point DNS/ports myself):"
  fi
}

plan() {
  step "Plan"
  info "role:       $ROLE"
  info "hostname:   $TS_HOSTNAME (Tailscale MagicDNS)"
  info "storage:    $STORAGE_DEVICE → $STORAGE_MOUNT"
  (( WITH_AI )) && info "AI models:  Ollama${AI_MODEL:+ + $AI_MODEL}"
  if [[ $ROLE == main ]]; then
    if [[ "$(domain_mode)" == cloudflare ]]; then info "domain:     *.$DOMAIN via Cloudflare Tunnel (HTTPS at Cloudflare)"
    elif [[ -n "$DOMAIN" ]]; then info "domain:     *.$DOMAIN via your public IP (Let's Encrypt; forward ports 80+443)"
    else info "domain:     none — apps private to your tailnet via sslip.io"; fi
  fi
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] && warn "dry run: nothing will be changed"
  return 0
}

# Re-runs (`vercelify update`) pass no --domain: keep the one set before.
load_previous_install() {
  local file="$VERCELIFY_ETC/install.env" prev mode
  [[ -r "$file" && -z "$DOMAIN" ]] || return 0
  prev=$(sed -n 's/^domain=//p' "$file" | tail -1)
  mode=$(sed -n 's/^domain_mode=//p' "$file" | tail -1)
  [[ -n "$prev" ]] || return 0
  DOMAIN=$prev
  # Installs before domain_mode existed: a tunnel unit means Cloudflare.
  if [[ -z "$mode" ]]; then
    if [[ -f "$SYSTEMD_DIR/cloudflared.service" ]]; then mode=cloudflare; else mode=direct; fi
  fi
  DOMAIN_MODE=$mode
}

domain_mode() {
  if [[ -z "$DOMAIN" ]]; then echo ''
  elif [[ -n "$CF_API_TOKEN" || "$DOMAIN_MODE" == cloudflare ]]; then echo cloudflare
  else echo direct; fi
}

coolify_wildcard() {
  if [[ "$(domain_mode)" == cloudflare ]]; then
    echo "http://$DOMAIN"         # TLS terminates at Cloudflare; Traefik serves HTTP to the tunnel
  elif [[ -n "$DOMAIN" ]]; then
    echo "https://$DOMAIN"        # Traefik gets a Let's Encrypt certificate per app
  else
    local ip
    ip=$(ts_ipv4)
    [[ -n "$ip" ]] && echo "http://$ip.sslip.io" || echo ""
  fi
}

authorize_coolify_key() {
  [[ -n "$COOLIFY_PUBKEY" ]] || return 0
  [[ "$COOLIFY_PUBKEY" =~ ^(ssh-ed25519|ssh-rsa|ecdsa-sha2-[a-z0-9-]+)\ [A-Za-z0-9+/=]+ ]] || die "--coolify-pubkey does not look like an SSH public key"
  run_cmd install -d -m 0700 /root/.ssh
  touch /root/.ssh/authorized_keys && chmod 600 /root/.ssh/authorized_keys
  if ! grep -qF "${COOLIFY_PUBKEY%% coolify*}" /root/.ssh/authorized_keys 2>/dev/null; then
    [[ "$VERCELIFY_DRY_RUN" == 1 ]] || printf '%s\n' "$COOLIFY_PUBKEY" >>/root/.ssh/authorized_keys
  fi
  ok "authorised the main host's Coolify key for root"
}

generate_password() { printf 'Vf-%s-%s!9' "$(random_hex 6)" "$(random_hex 6 | tr 'a-f' 'A-F')"; }

MAC_INSTALL="curl -fsSL https://raw.githubusercontent.com/$VERCELIFY_REPO/main/install-mac.sh | bash"

summary() {
  local dns ip pair='' short='' dashboard summary_file="$VERCELIFY_ETC/summary.txt"
  ip=$(ts_ipv4 || true)
  dns=$(ts_dns_name || true)
  dashboard="http://${dns:-${ip:-<tailscale-ip>}}:8000"
  if [[ "$VERCELIFY_DRY_RUN" != 1 ]] && [[ -x "$AGENT_BIN" ]]; then
    # One device token for both codes; the short one lasts long enough to
    # install the Mac app first.
    local both
    if both=$("$AGENT_BIN" --config "$VERCELIFY_ETC/agent.toml" pair --short --with-code --ttl 3600 2>/dev/null) &&
       [[ $(wc -l <<<"$both") -eq 2 ]]; then
      short=$(sed -n 1p <<<"$both")
      pair=$(sed -n 2p <<<"$both")
    else
      pair=$("$AGENT_BIN" --config "$VERCELIFY_ETC/agent.toml" pair 2>/dev/null || true)
      short=$("$AGENT_BIN" --config "$VERCELIFY_ETC/agent.toml" pair --short --ttl 3600 2>/dev/null || true)
    fi
  fi

  {
    printf '\n%s%s✔ Vercelify is ready%s  (%s, %s)\n\n' "$C_BOLD" "$C_GREEN" "$C_RESET" "$ROLE" "$VERSION"
    if [[ $ROLE == main ]]; then
      printf '  %sCoolify dashboard%s  %s\n' "$C_BOLD" "$C_RESET" "$dashboard"
      [[ -n "$ADMIN_EMAIL" ]] && printf '  %sLogin%s              %s / %s\n' "$C_BOLD" "$C_RESET" "$ADMIN_EMAIL" "${ADMIN_PASSWORD:-<the password you chose>}"
      local apps
      apps=$(coolify_wildcard | sed 's#://#://<app>.#')
      printf '  %sApps%s               %s\n' "$C_BOLD" "$C_RESET" "${apps:-connect Tailscale, then: sudo vercelify domain}"
    fi
    printf '  %sTailscale%s          %s (%s)\n' "$C_BOLD" "$C_RESET" "${dns:-not connected}" "${ip:--}"
    printf '  %sStorage%s            %s\n' "$C_BOLD" "$C_RESET" "$(findmnt -no SOURCE,SIZE "$STORAGE_MOUNT" 2>/dev/null || echo "single disk ($STORAGE_MOUNT on /)")"
    (( WITH_AI )) && printf '  %sAI models%s          http://%s:11434 (Ollama, OpenAI-compatible at /v1)\n' "$C_BOLD" "$C_RESET" "${dns:-${ip:-<tailscale-ip>}}"
    if [[ -n "$pair" ]]; then
      printf '\n  %sConnect your Mac%s\n' "$C_BOLD" "$C_RESET"
      printf '    1. Install the app (and Tailscale):\n         %s\n' "$MAC_INSTALL"
      if [[ -n "$short" ]]; then
        printf '    2. Open Vercelify, pick %s%s%s and type the code   %s%s%s   (valid 1 hour)\n' \
          "$C_BOLD" "${dns%%.*}" "$C_RESET" "$C_BOLD" "$short" "$C_RESET"
        printf '       or paste the full code:\n         %s\n' "$pair"
      else
        printf '    2. Open Vercelify and paste this code:\n         %s\n' "$pair"
      fi
      printf '\n  New code any time: sudo vercelify pair\n'
    fi
    if [[ $ROLE == worker ]]; then
      printf '\n  Next: pair this worker in the Mac app, then on its Servers page click\n  %sAdd to a pool%s so your main server deploys here.\n' "$C_BOLD" "$C_RESET"
      printf '  (Or on the main host: sudo vercelify add-worker root@%s)\n' "${dns:-$TS_HOSTNAME}"
    elif [[ "$(domain_mode)" == direct ]]; then
      printf '\n  %sDNS to create at your registrar%s (public IP %s):\n' "$C_BOLD" "$C_RESET" "$(curl -fsS -m 5 https://api.ipify.org 2>/dev/null || echo '?')"
      printf '    A  *.%s  → that IP      A  %s  → that IP\n  and forward TCP 80 + 443 on your router to this machine.\n' "$DOMAIN" "$DOMAIN"
    fi
    if (( NEEDS_REBOOT )); then
      printf '\n  %sReboot once%s to finish (sudo reboot): the kernel needs it for container memory limits.\n' "$C_BOLD" "$C_RESET"
    fi
    printf '\n  Health check: sudo vercelify doctor     Logs: %s\n\n' "$VERCELIFY_LOG"
  } >&2

  if [[ "$VERCELIFY_DRY_RUN" != 1 ]]; then
    {
      echo "role=$ROLE"
      echo "dashboard=$dashboard"
      echo "admin_email=$ADMIN_EMAIL"
      echo "admin_password=$ADMIN_PASSWORD"
      echo "domain=$DOMAIN"
      echo "wildcard=$(coolify_wildcard)"
      echo "pairing_code=$pair"
    } | write_file "$summary_file" 0600 || true
  fi
}

on_error() {
  local code=$? line=${BASH_LINENO[0]:-?}
  # errtrace also fires inside $(...); let the parent report the failure.
  (( BASH_SUBSHELL > 0 )) && return "$code"
  err "setup failed (exit $code near line $line) — full log: $VERCELIFY_LOG"
  err "fix the problem and simply run the installer again; finished steps are skipped"
  exit "$code"
}

main() {
  parse_args "$@"
  mkdir -p "$(dirname "$VERCELIFY_LOG")" 2>/dev/null || true
  trap on_error ERR
  preflight "$@"
  load_previous_install
  wizard
  if [[ -n "$DOMAIN" ]]; then valid_domain "$DOMAIN" || die "'$DOMAIN' is not a valid domain name"; fi
  if [[ $ROLE == main ]] && ! coolify_installed 2>/dev/null; then
    ADMIN_EMAIL=$(coolify_safe_email "${ADMIN_EMAIL:-admin@example.com}")
  fi
  if [[ $ROLE == main && -z "$ADMIN_PASSWORD" ]] && ! coolify_installed 2>/dev/null; then ADMIN_PASSWORD=$(generate_password); fi
  plan

  install_base_packages
  laptop_server_mode
  tune_kernel
  harden_hardware
  configure_ssh
  setup_storage "$STORAGE_DEVICE" "$FORMAT_STORAGE"
  [[ $ROLE == main ]] && bind_coolify_backups
  container_slice >/dev/null
  install_docker
  install_tailscale "$TS_HOSTNAME" "$TS_AUTHKEY"
  (( WITH_DISPLAY )) && { apt_install mpv || warn "could not install mpv"; }

  if [[ $ROLE == main ]]; then
    if coolify_installed; then install_coolify; else install_coolify "$ADMIN_EMAIL" "$ADMIN_PASSWORD"; fi
    local wildcard
    wildcard=$(coolify_wildcard)
    configure_coolify "$wildcard"
    coolify_api_token
    if [[ -n "$DOMAIN" && -n "$CF_API_TOKEN" ]]; then
      setup_cloudflare_domain "$DOMAIN" "$CF_API_TOKEN" "vercelify-$TS_HOSTNAME" "$FORCE_DNS"
    fi
  else
    authorize_coolify_key
  fi

  install_agent
  install_mesh_binary || warn "the mesh daemon isn't installed yet — the next update brings it"
  (( WITH_POWER_GUARD )) && install_power_guard
  (( WITH_FIREWALL )) && install_firewall
  install_cli
  step "Reboot window"
  maintenance_setup
  printf 'role=%s\nversion=%s\nchannel=%s\nref=%s\nprofile=%s\ndomain=%s\ndomain_mode=%s\n' \
    "$ROLE" "$VERSION" "${VERCELIFY_INSTALL_CHANNEL:-stable}" "${VERCELIFY_INSTALL_REF:-local}" "$HW_PROFILE" "$DOMAIN" "$(domain_mode)" |
    write_file "$VERCELIFY_ETC/install.env" 0644 || true
  # The agent reports its role from install.env.
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] || systemctl try-restart vercelify-agent >/dev/null 2>&1 || true
  if (( WITH_AI )); then install_ai "${AI_MODEL:-$(default_ai_model)}"; fi
  if [[ $ROLE == main ]] && (( WITH_BACKUP )) && [[ "$VERCELIFY_DRY_RUN" != 1 ]] && [[ ! -r "$BACKUP_ENV_FILE" ]]; then
    step "Backups"
    # Not fatal: the doctor keeps reminding until backups work.
    ( backup_setup ) || warn "backups could not be set up now — later: sudo vercelify backup setup"
  fi
  summary
}

# Allow tests to source the functions without running anything.
if [[ "${VERCELIFY_SOURCE_ONLY:-0}" != 1 ]]; then
  set -Eeuo pipefail
  main "$@"
fi

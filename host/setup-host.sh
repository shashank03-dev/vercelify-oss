#!/usr/bin/env bash
# Vercelify host setup — turns a Debian/Ubuntu machine into a self-hosted
# Vercel: Docker + Vercelify's engine and private network (the agent and
# vercelify-mesh) + storage tiers.
#
# Re-runnable at any time: every step checks what is already done.
# Run `setup-host.sh --help` for options.

VERCELIFY_SRC=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$VERCELIFY_SRC/Cargo.toml" 2>/dev/null | head -1)
VERSION=${VERSION:-dev}

# What's installed, said exactly: the release number, plus the branch or
# commit when it was built from one (`0.2.5+main.1a2b3c4`), so main can be
# told apart from the release it's numbered after.
build_version() {
  local ref=${VERCELIFY_INSTALL_REF:-} sha
  if [[ -z "$ref" || "$ref" == "v$VERSION" || "$ref" == "$VERSION" ]] &&
     [[ "${VERCELIFY_INSTALL_CHANNEL:-stable}" != edge ]]; then
    echo "$VERSION"; return
  fi
  sha=$(git -C "$VERCELIFY_SRC" rev-parse --short=7 HEAD 2>/dev/null || true)
  ref=${ref:-edge}
  ref=${ref//[^0-9A-Za-z-]/-}
  echo "$VERSION+$ref${sha:+.$sha}"
}

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
# shellcheck source=lib/platform.sh
. "$VERCELIFY_SRC/host/lib/platform.sh"
# shellcheck source=lib/mesh.sh
. "$VERCELIFY_SRC/host/lib/mesh.sh"
# shellcheck source=lib/cloudflare.sh
. "$VERCELIFY_SRC/host/lib/cloudflare.sh"
# shellcheck source=lib/agent.sh
. "$VERCELIFY_SRC/host/lib/agent.sh"
# shellcheck source=lib/remote.sh
. "$VERCELIFY_SRC/host/lib/remote.sh"
# shellcheck source=lib/desktop.sh
. "$VERCELIFY_SRC/host/lib/desktop.sh"
# shellcheck source=lib/backup.sh
. "$VERCELIFY_SRC/host/lib/backup.sh"
# shellcheck source=lib/maintenance.sh
. "$VERCELIFY_SRC/host/lib/maintenance.sh"

usage() {
  cat <<EOF
Vercelify host setup $VERSION

Usage: sudo $0 [options]

Most people need no options: install, then connect with the code shown.
Use --configure to choose optional features during installation.

Role
  --worker                 Worker (e.g. a Raspberry Pi): runs apps the main server
                           places on it
  --join TOKEN             (worker) Join the main server's network with this invite
                           (the Mac app puts it in the command; or on the main
                           server: sudo vercelify mesh invite)

Network & domain
  --hostname NAME          This machine's name (default: $(hostname -s 2>/dev/null || echo host))
  --domain DOMAIN          Your domain: apps get *.DOMAIN (e.g. blog.DOMAIN)
  --cloudflare-token TOK   Cloudflare API token: creates a tunnel + DNS for you
                           (no port forwarding, works behind CGNAT)
  --force-dns              Replace existing DNS records for *.DOMAIN and DOMAIN
  --remote                 Reach this server from anywhere: the router forwards two
                           ports by itself (UPnP/NAT-PMP), else a free Cloudflare
                           quick tunnel (no account, works behind CGNAT)
  --no-remote              Don't ask about it (LAN only, or a Cloudflare domain)

Storage
  --storage-device DEV     Disk for databases/volumes, mounted at /mnt/storage
                           (default: none; 'auto' = detect a second disk)
  --format-storage         Allow formatting that disk as ext4 (ERASES IT)

Features
  --ai [MODEL]             Also install Ollama for local AI models, and download
                           MODEL (default: llama3.2:3b, or gemma3:1b under 8 GB)
  --no-firewall            Don't keep the agent's and AI API's ports private
  --no-power-guard         Don't install the battery/thermal guard
  --no-display             Don't install mpv (secondary-monitor receiver)
  --desktop                Use this machine's desktop from the Mac (remote desktop):
                           install and start Sunshine (asked when there's a desktop)
  --no-desktop             Don't ask about remote desktop
  --no-backup              Don't turn on nightly backups (main server)

General
  --configure              Ask about optional domains, remote access, AI and desktop
  -y, --yes                Accept all defaults, never prompt
  --dry-run                Print what would be done without changing anything
  -h, --help               Show this help

Examples
  sudo $0
  sudo $0 --domain example.com --cloudflare-token cf_XXXX -y
  sudo $0 --worker --hostname pi5 --join vfyj1.…
EOF
}

# --- options -----------------------------------------------------------------
ROLE=main
ROLE_EXPLICIT=0
MACHINE_NAME=$(hostname -s 2>/dev/null || echo vercelify)
MACHINE_NAME=${MACHINE_NAME,,}
RENAME=0
JOIN_TOKEN=''
DOMAIN=''
# cloudflare | direct — remembered in install.env so updates keep the domain
DOMAIN_MODE=''
CF_API_TOKEN=${CF_API_TOKEN:-}
FORCE_DNS=0
# 1 on, 0 off, '' ask (main server without a Cloudflare domain)
REMOTE=''
DESKTOP=''
STORAGE_DEVICE=none
FORMAT_STORAGE=0
WITH_FIREWALL=1
WITH_POWER_GUARD=1
WITH_DISPLAY=1
WITH_AI=0
PREVIOUS_INSTALL=0
AI_MODEL=''
WITH_BACKUP=1
CONFIGURE=0
ASSUME_YES=${ASSUME_YES:-0}

parse_args() {
  while (( $# )); do
    case "$1" in
      --configure) CONFIGURE=1 ;;
      --worker) ROLE=worker; ROLE_EXPLICIT=1 ;;
      --join) JOIN_TOKEN=${2:?}; shift ;;
      --hostname) MACHINE_NAME=${2:?}; RENAME=1; shift ;;
      --domain) DOMAIN=${2:?}; shift ;;
      --cloudflare-token) CF_API_TOKEN=${2:?}; shift ;;
      --force-dns) FORCE_DNS=1 ;;
      --remote) REMOTE=1 ;;
      --no-remote) REMOTE=0 ;;
      --desktop) DESKTOP=1 ;;
      --no-desktop) DESKTOP=0 ;;
      --storage-device) STORAGE_DEVICE=${2:?}; shift ;;
      --format-storage) FORMAT_STORAGE=1 ;;
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
  (( mem_mb < 3800 )) && [[ $ROLE == main ]] && warn "${mem_mb} MB RAM: apps run, but builds will be slow; 4 GB+ recommended"
  ok "${mem_mb} MB RAM, ${disk_gb} GB free on /"
  (( disk_gb < 20 )) && [[ $ROLE == main ]] && warn "less than 20 GB free on / — Docker images and builds need room"

  if [[ "$VERCELIFY_DRY_RUN" != 1 ]]; then
    have curl || apt_install curl ca-certificates
    curl -fsS -m 10 -o /dev/null https://get.docker.com || die "no internet access (https://get.docker.com unreachable)"
    ok "internet access"
  fi
  valid_hostname "$MACHINE_NAME" || die "invalid --hostname '$MACHINE_NAME' (lowercase letters, digits, '-')"
  [[ -z "$JOIN_TOKEN" || "$JOIN_TOKEN" == vfyj1.* ]] || die "--join needs an invite (vfyj1.…) from: sudo vercelify mesh invite"
}

wizard() {
  if [[ "$CONFIGURE" != 1 ]]; then
    info "Installing the private server. Optional features can be added later in the app or with sudo vercelify."
    return 0
  fi
  has_tty || return 0
  [[ "$ASSUME_YES" == 1 ]] && return 0
  printf '\n%s%s Vercelify %s — self-hosted Vercel on this machine%s\n' "$C_BOLD" "$C_CYAN" "$VERSION" "$C_RESET" >&2
  printf '%s   Role: %s · Host: %s · Answer a few questions (Enter = default).%s\n' "$C_DIM" "$ROLE" "$MACHINE_NAME" "$C_RESET" >&2
  if (( ! WITH_AI )); then
    printf '\n  Vercelify can also run open AI models (Llama, Qwen, Gemma…) on this machine,\n  for your Mac and your apps. About 4 GB to download (~12 GB on disk); you can add it later.\n' >&2
    if confirm "Set up local AI models too?" n; then WITH_AI=1; fi
  fi
  [[ $ROLE == worker ]] && return 0

  if [[ -z "$DOMAIN" ]]; then
    printf '\n  Apps need addresses. Without a domain they are private to your Vercelify network\n  (http://<app>.%s.sslip.io). With one they get https://<app>.<domain>.\n' "$MESH_MAIN_IP" >&2
    ask DOMAIN "Your domain (blank = private only, add one later with 'vercelify domain'):" ""
    DOMAIN=${DOMAIN,,}
  fi
  if [[ -n "$DOMAIN" && -z "$CF_API_TOKEN" && -z "$DOMAIN_MODE" ]]; then
    printf '\n  Is %s on Cloudflare? Paste an API token and Vercelify creates a tunnel + DNS\n  records for you — no router port forwarding, works on any ISP.\n  Create one at https://dash.cloudflare.com/profile/api-tokens with:\n    Account › Cloudflare Tunnel › Edit, Zone › DNS › Edit, Zone › Zone › Read\n' "$DOMAIN" >&2
    ask_secret CF_API_TOKEN "Cloudflare API token (blank = I'll point DNS/ports myself):"
  fi
  if [[ -z "$REMOTE" && -z "$CF_API_TOKEN" && "$DOMAIN_MODE" != cloudflare && ! -r "$REMOTE_ENV" ]]; then
    printf '\n  Connect your Mac and other machines from any network, not just this Wi-Fi?\n  Vercelify asks your router to forward its private network'"'"'s two ports here\n  (UPnP/NAT-PMP), or else opens a free Cloudflare quick tunnel (no account). Only\n  Vercelify'"'"'s encrypted network answers there; your apps and its API stay private.\n' >&2
    if confirm "Reach this server from anywhere?" y; then REMOTE=1; else REMOTE=0; fi
  fi
  # Only with a desktop to share (Ubuntu Desktop and the like).
  if [[ -z "$DESKTOP" ]] && desktop_user >/dev/null 2>&1; then
    printf '\n  Use this laptop'"'"'s desktop from your Mac (remote desktop with Moonlight)? It\n  installs Sunshine, which streams the logged-in desktop over Vercelify'"'"'s network.\n' >&2
    if confirm "Set up remote desktop?" y; then DESKTOP=1; else DESKTOP=0; fi
  fi
}

plan() {
  step "Plan"
  info "role:       $ROLE"
  info "name:       $MACHINE_NAME"
  info "storage:    $STORAGE_DEVICE → $STORAGE_MOUNT"
  (( WITH_AI )) && info "AI models:  Ollama${AI_MODEL:+ + $AI_MODEL}"
  if [[ $ROLE == main ]]; then
    if [[ "$(domain_mode)" == cloudflare ]]; then info "domain:     *.$DOMAIN via Cloudflare Tunnel (HTTPS at Cloudflare)"
    elif [[ -n "$DOMAIN" ]]; then info "domain:     *.$DOMAIN via your public IP (Let's Encrypt; forward ports 80+443)"
    else info "domain:     none — apps private to your Vercelify network via sslip.io"; fi
  elif [[ -n "$JOIN_TOKEN" ]]; then
    info "network:    joins its main server's (invite ${JOIN_TOKEN:0:12}…)"
  fi
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] && warn "dry run: nothing will be changed"
  return 0
}

# Re-runs (`vercelify update`) pass no --domain: keep the one set before.
load_previous_install() {
  local file="$VERCELIFY_ETC/install.env" prev mode
  [[ -r "$file" ]] || return 0
  PREVIOUS_INSTALL=1
  if [[ "$ROLE_EXPLICIT" == 0 ]]; then
    local saved_role
    saved_role=$(sed -n 's/^role=//p' "$file" | tail -1)
    case "$saved_role" in main|worker) ROLE=$saved_role ;; esac
  fi
  [[ -z "$DOMAIN" ]] || return 0
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

# The main server's address on its own network (`vercelify mesh init`).
MESH_MAIN_IP=10.77.0.1

# Where new apps get their addresses: <app>.<this>.
app_wildcard() {
  if [[ "$(domain_mode)" == cloudflare ]]; then
    echo "http://$DOMAIN"         # TLS terminates at Cloudflare; the proxy serves HTTP to the tunnel
  elif [[ -n "$DOMAIN" ]]; then
    echo "https://$DOMAIN"        # the proxy gets a Let's Encrypt certificate per app
  else
    echo "http://$(mesh_ipv4 || echo "$MESH_MAIN_IP").sslip.io"
  fi
}

# This machine's address on Vercelify's network, if it is on one.
mesh_ipv4() { jq -r '.ipv4 // empty' "$MESH_DIR/state.json" 2>/dev/null | grep . ; }

# The name everything shows (the agent, the network, the Mac app) is the
# machine's own: --hostname renames it.
set_machine_name() {
  (( RENAME )) || return 0
  [[ "$(hostname -s 2>/dev/null)" == "$MACHINE_NAME" ]] && return 0
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] && { info "[dry-run] would rename this machine to $MACHINE_NAME"; return 0; }
  run_cmd hostnamectl set-hostname "$MACHINE_NAME" || { warn "could not rename this machine to $MACHINE_NAME"; return 0; }
  if grep -q '^127\.0\.1\.1[[:space:]]' /etc/hosts 2>/dev/null; then
    sed -i "s/^127\.0\.1\.1[[:space:]].*/127.0.1.1\t$MACHINE_NAME/" /etc/hosts
  else
    printf '127.0.1.1\t%s\n' "$MACHINE_NAME" >>/etc/hosts
  fi
  ok "this machine is now called $MACHINE_NAME"
}

# Vercelify's own network (design §13): the main server coordinates it; a
# worker joins with the invite its install command carries.
setup_network() {
  step "Private network (vercelify-mesh)"
  if [[ "$VERCELIFY_DRY_RUN" == 1 ]]; then
    if [[ -n "$JOIN_TOKEN" ]]; then info "[dry-run] would join the main server's network"
    else info "[dry-run] would set up this server's network"; fi
    return 0
  fi
  if [[ -n "$(mesh_ipv4)" ]]; then
    systemctl is-active --quiet vercelify-mesh 2>/dev/null || mesh_install_unit
    ok "on the network as $(mesh_ipv4)"
    return 0
  fi
  if [[ $ROLE == main ]]; then
    ( mesh_init ) || warn "the network isn't set up yet — later: sudo vercelify mesh init"
  elif [[ -n "$JOIN_TOKEN" ]]; then
    ( mesh_join "$JOIN_TOKEN" --platform ) || warn "joining failed — on the main server: sudo vercelify mesh invite, then here: sudo vercelify mesh join <token> --platform"
  else
    toml_set engine '"platform"' "$VERCELIFY_ETC/agent.toml"
    warn "not on a network yet — on the main server run: sudo vercelify mesh invite, then here what it prints"
  fi
}

MAC_INSTALL="curl -fsSL https://raw.githubusercontent.com/$VERCELIFY_REPO/main/install-mac.sh | bash"

summary() {
  local ip pair='' short='' summary_file="$VERCELIFY_ETC/summary.txt" wildcard=''
  ip=$(mesh_ipv4 || true)
  [[ $ROLE == main ]] && platform_engine && wildcard=$(app_wildcard)
  # An update (non-interactive re-run): the Macs are paired already, and a
  # fresh code would only land in the update's log.
  local updating=0
  [[ "${PREVIOUS_INSTALL:-0}" == 1 && "${VERCELIFY_NONINTERACTIVE:-0}" == 1 ]] && updating=1
  if [[ "$VERCELIFY_DRY_RUN" != 1 ]] && [[ -x "$AGENT_BIN" ]] && (( ! updating )); then
    # One device token for both codes; the short one lasts long enough to
    # install the Mac app first.
    local both
    if both=$("$AGENT_BIN" --config "$VERCELIFY_ETC/agent.toml" pair --short --with-code --ttl 3600 2>/dev/null) &&
       [[ $(wc -l <<<"$both") -eq 2 ]]; then
      short=$(sed -n 1p <<<"$both")
      pair=$(sed -n 2p <<<"$both")
    else
      pair=$("$AGENT_BIN" --config "$VERCELIFY_ETC/agent.toml" pair 2>/dev/null || true)
    fi
  fi

  local agent_ready=0 network_ready=0
  if [[ "$VERCELIFY_DRY_RUN" != 1 ]]; then
    curl -fsS -m 3 "$AGENT_LOCAL_URL/v1/health" >/dev/null 2>&1 && agent_ready=1
    if [[ -n "$ip" ]] && systemctl is-active --quiet vercelify-mesh 2>/dev/null &&
       ip -4 addr show dev vfy0 2>/dev/null | grep -Fq "inet $ip/"; then network_ready=1; fi
  fi
  {
    if [[ "$VERCELIFY_DRY_RUN" == 1 ]]; then
      printf '\nVercelify installation plan complete (dry run; nothing installed)\n'
    elif (( agent_ready && network_ready )); then
      printf '\n%s%s✔ Vercelify is ready%s  (%s, %s)\n\n' "$C_BOLD" "$C_GREEN" "$C_RESET" "$ROLE" "$VERSION"
    else
      printf '\n%sVercelify is installed — connection needs attention%s\n\n' "$C_BOLD" "$C_RESET"
    fi
    if (( agent_ready )); then printf '  Agent              Ready\n'
    else printf '  Agent              Not confirmed — run: sudo vercelify doctor\n'; fi
    if (( ! network_ready )); then
      if [[ $ROLE == main ]]; then printf '  Network recovery   sudo vercelify mesh init\n'
      else printf '  Network recovery   Get a fresh invite on the main server: sudo vercelify mesh invite\n'; fi
    fi
    if [[ -n "$wildcard" ]]; then
      printf '  %sApps%s               %s\n' "$C_BOLD" "$C_RESET" "${wildcard/:\/\//:\/\/<app>.}"
    fi
    printf '  %sNetwork%s            %s\n' "$C_BOLD" "$C_RESET" "${ip:+$(hostname -s) at }${ip:-not on a Vercelify network yet}${ip:+ (vfy0)}"
    printf '  %sStorage%s            %s\n' "$C_BOLD" "$C_RESET" "$(findmnt -no SOURCE,SIZE "$STORAGE_MOUNT" 2>/dev/null || echo "single disk ($STORAGE_MOUNT on /)")"
    (( WITH_AI )) && printf '  %sAI models%s          http://%s:11434 (Ollama, OpenAI-compatible at /v1)\n' "$C_BOLD" "$C_RESET" "${ip:-<this server>}"
    if [[ -n "$pair" && $ROLE == main ]]; then
      printf '\n  %sConnect your Mac%s — run this in its Terminal (installs the app and pairs it):\n' "$C_BOLD" "$C_RESET"
      printf '    %s -s -- --pair %s\n' "$MAC_INSTALL" "$pair"
      if [[ -n "$short" ]]; then
        printf '  App already installed? On the same Wi-Fi pick %s%s%s and type   %s%s%s   (valid 1 hour)\n' \
          "$C_BOLD" "$(hostname -s)" "$C_RESET" "$C_BOLD" "$short" "$C_RESET"
      fi
      printf '  New code any time: sudo vercelify pair\n'
    elif (( updating )) && [[ $ROLE == main ]]; then
      printf '\n  Paired Macs keep working. To add another: sudo vercelify pair\n'
    fi
    if [[ $ROLE == worker ]]; then
      if [[ -n "$ip" ]]; then
        printf '\n  Its main server places apps here now; nothing else to do.\n'
      else
        printf '\n  Next: on the main server run %ssudo vercelify mesh invite%s, then here what it prints.\n' "$C_BOLD" "$C_RESET"
      fi
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
      echo "agent_ready=$agent_ready"
      echo "network_ready=$network_ready"
      echo "role=$ROLE"
      echo "network=$ip"
      echo "domain=$DOMAIN"
      echo "wildcard=$wildcard"
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
  # The log holds the run's output (addresses, the pairing step): root only.
  { touch "$VERCELIFY_LOG" && chmod 0600 "$VERCELIFY_LOG"; } 2>/dev/null || true
  trap on_error ERR
  load_previous_install
  preflight "$@"
  wizard
  if [[ -n "$DOMAIN" ]]; then valid_domain "$DOMAIN" || die "'$DOMAIN' is not a valid domain name"; fi
  plan

  install_base_packages
  set_machine_name
  laptop_server_mode
  tune_kernel
  harden_hardware
  setup_storage "$STORAGE_DEVICE" "$FORMAT_STORAGE"
  container_slice >/dev/null
  install_docker
  (( WITH_DISPLAY )) && { apt_install mpv || warn "could not install mpv"; }

  # The agent reads its role from install.env from its first start on.
  run_cmd mkdir -p "$VERCELIFY_ETC"
  printf 'role=%s\nversion=%s\nchannel=%s\nref=%s\nprofile=%s\ndomain=%s\ndomain_mode=%s\n' \
    "$ROLE" "$(build_version)" "${VERCELIFY_INSTALL_CHANNEL:-stable}" "${VERCELIFY_INSTALL_REF:-local}" "$HW_PROFILE" "$DOMAIN" "$(domain_mode)" |
    write_file "$VERCELIFY_ETC/install.env" 0644 || true
  # $VERCELIFY_ETC is root's: `vercelify version` reads this one without sudo.
  run_cmd mkdir -p "$VERCELIFY_LIB"
  build_version | write_file "$VERCELIFY_LIB/VERSION" 0644 || true
  install_mesh_binary || die "could not install vercelify-mesh, Vercelify's private network"
  install_agent
  # The tunnel before the network: machines outside the LAN join through it.
  if [[ $ROLE == main && -n "$DOMAIN" && -n "$CF_API_TOKEN" ]]; then
    setup_cloudflare_domain "$DOMAIN" "$CF_API_TOKEN" "vercelify-$MACHINE_NAME" "$FORCE_DNS"
  fi
  setup_network
  if [[ $ROLE == main && "$REMOTE" == 1 && "$VERCELIFY_DRY_RUN" != 1 ]]; then
    step "Remote access"
    ( remote_on ) || warn "not reachable from outside your network yet — later: sudo vercelify remote on"
  fi
  if [[ "$DESKTOP" == 1 && "$VERCELIFY_DRY_RUN" != 1 ]]; then
    step "Remote desktop"
    ( desktop_on ) || warn "remote desktop isn't set up — later: sudo vercelify desktop on"
  fi
  if [[ $ROLE == main ]]; then
    if platform_engine; then
      step "App addresses"
      local edge=false
      [[ "$(domain_mode)" == cloudflare ]] && edge=true
      platform_set_domain "$(app_wildcard)" "$edge" || warn "could not set the app domain now — later: sudo vercelify domain"
    fi
  fi
  (( WITH_POWER_GUARD )) && install_power_guard
  (( WITH_FIREWALL )) && install_firewall
  install_cli
  step "Reboot window"
  maintenance_setup
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

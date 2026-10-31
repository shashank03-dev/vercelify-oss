#!/usr/bin/env bash
# `vercelify desktop`: this machine's desktop on the Mac (remote desktop).
# Sunshine streams the logged-in desktop; the Mac app's Screencast tab starts
# Moonlight against it over Vercelify's network.
#
# Sunshine comes from its GitHub release for this distribution, checked
# against the SHA-256 GitHub records for the file. It runs as upstream
# intends: its user unit, in the desktop session (display, audio, input),
# enabled for graphical logins. The agent learns whose it is
# (`desktop_user`) to show its status and restart it, and pairs Moonlight
# with it: it sets Sunshine's first login itself and hands over the PIN the
# Mac started Moonlight with, so the user never types either.

: "${SUNSHINE_RELEASE_API:=https://api.github.com/repos/LizardByte/Sunshine/releases/latest}"
: "${OS_RELEASE:=/etc/os-release}"
: "${AGENT_CONFIG:=$VERCELIFY_ETC/agent.toml}"
: "${VERCELIFY_STATE:=/var/lib/vercelify}"
SUNSHINE_UNIT=app-dev.lizardbyte.app.Sunshine.service

# The user of the logged-in desktop (a graphical session), else the one
# running sudo when a display manager is set up; fails without a desktop.
desktop_user() {
  local id type user
  while read -r id _; do
    [[ -n "$id" ]] || continue
    type=$(loginctl show-session "$id" -p Type --value 2>/dev/null || true)
    [[ "$type" == x11 || "$type" == wayland ]] || continue
    user=$(loginctl show-session "$id" -p Name --value 2>/dev/null || true)
    [[ -n "$user" && "$user" != root ]] && { printf '%s\n' "$user"; return 0; }
  done < <(loginctl list-sessions --no-legend 2>/dev/null || true)
  if [[ -n "${SUDO_USER:-}" && "$SUDO_USER" != root ]] && systemctl is-enabled --quiet display-manager 2>/dev/null; then
    printf '%s\n' "$SUDO_USER"
    return 0
  fi
  return 1
}

# The end of the release file's name for this system: `+ubuntu24.04_amd64.deb`.
sunshine_asset_suffix() {
  local id ver code ucode arch dist
  # shellcheck disable=SC1090
  read -r id ver code ucode < <(. "$OS_RELEASE" 2>/dev/null; printf '%s %s %s %s\n' "${ID:-x}" "${VERSION_ID:-x}" "${VERSION_CODENAME:-x}" "${UBUNTU_CODENAME:-x}")
  arch=$(dpkg --print-architecture 2>/dev/null) || return 1
  case "$id" in
    ubuntu) dist="ubuntu$ver" ;;
    debian) dist="debian$code" ;;
    *)
      # Ubuntu-based (Linux Mint, Pop!_OS, Zorin…): Ubuntu's package.
      case "$ucode" in
        jammy) dist=ubuntu22.04 ;;
        noble) dist=ubuntu24.04 ;;
        *) return 1 ;;
      esac
      ;;
  esac
  printf '+%s_%s.deb\n' "$dist" "$arch"
}

sunshine_install() {
  local suffix json name url digest tmp got
  have sunshine && return 0
  suffix=$(sunshine_asset_suffix) || { err "no Sunshine package for this system: see https://github.com/LizardByte/Sunshine/releases"; return 1; }
  json=$(curl -fsSL -m 30 "$SUNSHINE_RELEASE_API" 2>/dev/null) || { err "couldn't reach GitHub for Sunshine"; return 1; }
  read -r name url digest < <(jq -r --arg s "$suffix" \
    '[.assets[] | select(.name | endswith($s))][0] // empty | "\(.name) \(.browser_download_url) \(.digest // "-")"' <<<"$json" 2>/dev/null) || true
  [[ -n "${url:-}" ]] || { err "Sunshine has no package for this system (*$suffix): see https://github.com/LizardByte/Sunshine/releases"; return 1; }
  [[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]] || { err "GitHub lists no checksum for $name: not installing it"; return 1; }
  [[ "$name" =~ ^sunshine_[A-Za-z0-9.+_~-]+\.deb$ ]] || { err "unexpected Sunshine file name: $name"; return 1; }
  tmp=$(mktemp -d)
  info "downloading $name"
  if ! fetch "$url" "$tmp/$name"; then rm -rf "$tmp"; err "couldn't download $name"; return 1; fi
  got=$(sha256sum "$tmp/$name" | cut -d' ' -f1)
  if [[ "sha256:$got" != "$digest" ]]; then
    rm -rf "$tmp"
    err "$name: checksum mismatch, not installing it"
    return 1
  fi
  ok "checksum verified"
  # apt resolves its dependencies; the package sets Sunshine's screen-capture
  # capability and the input device rules.
  chmod 0755 "$tmp" && chmod 0644 "$tmp/$name"
  if ! run_cmd env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$tmp/$name" >/dev/null; then
    rm -rf "$tmp"
    err "installing $name failed"
    return 1
  fi
  rm -rf "$tmp"
}

# Sunshine's tray icon aborts it when it runs as a service (seen on GNOME,
# X11); the Mac app and Sunshine's web page are its interface. Written as the
# user, once: a system_tray line the user set stays.
sunshine_configure() {
  local user=$1 home dir
  home=$(getent passwd "$user" | cut -d: -f6)
  [[ -n "$home" && -d "$home" ]] || return 0
  dir="$home/.config/sunshine"
  grep -qE '^[[:space:]]*system_tray[[:space:]]*=' "$dir/sunshine.conf" 2>/dev/null && return 0
  run_cmd runuser -u "$user" -- sh -c 'mkdir -p "$1" && printf "system_tray = disabled\n" >>"$1/sunshine.conf"' _ "$dir" ||
    warn "couldn't write $dir/sunshine.conf: if Sunshine stops at 'Starting system tray', add system_tray = disabled there"
}

desktop_on() {
  local user session
  user=$(desktop_user) || {
    err "no desktop here: remote desktop streams a logged-in desktop (Ubuntu Desktop and the like). Log in on the laptop's screen, or use it as the Mac's second monitor instead"
    return 1
  }
  sunshine_install || return 1
  sunshine_configure "$user"
  run_cmd modprobe uinput 2>/dev/null || true
  run_cmd modprobe uhid 2>/dev/null || true
  run_cmd udevadm trigger --subsystem-match=misc 2>/dev/null || true
  # Every graphical login starts it; now too, when that user is logged in.
  run_cmd systemctl --global enable "$SUNSHINE_UNIT" >/dev/null 2>&1 || { err "couldn't enable Sunshine's service"; return 1; }
  session=$(systemctl --user -M "$user@" is-system-running 2>/dev/null || true)
  if [[ "$session" == running || "$session" == degraded || "$session" == starting ]]; then
    run_cmd systemctl --user -M "$user@" daemon-reload >/dev/null 2>&1 || true
    # Earlier crashes may have left it failed, past systemd's restart limit.
    run_cmd systemctl --user -M "$user@" reset-failed "$SUNSHINE_UNIT" >/dev/null 2>&1 || true
    run_cmd systemctl --user -M "$user@" restart "$SUNSHINE_UNIT" >/dev/null 2>&1 || warn "Sunshine didn't start yet: it starts with $user's next login"
  else
    info "Sunshine starts when $user logs in to the desktop"
  fi
  if [[ "$(_desktop_toml desktop_user)" != "$user" ]]; then
    local running
    if declare -F agent_api >/dev/null; then
      running=$( (API_TIMEOUT=5 agent_api GET /v1/jobs) 2>/dev/null | jq -r '.[] | select(.status == "running") | .title' 2>/dev/null || true)
      if [[ -n "$running" ]]; then
        err "Cannot restart the agent while jobs are running: $running. Let them finish, then retry desktop on."
        return 1
      fi
    fi
    warn "Updating the desktop user restarts the agent; running downloads and backups will be interrupted. Wait for them to finish first."
    toml_set desktop_user "\"$user\"" "$AGENT_CONFIG"
    run_cmd systemctl restart vercelify-agent 2>/dev/null || true
  fi
  ok "remote desktop on ($user's desktop). On the Mac: Second screen → Launch Moonlight (it pairs by itself)"
}

desktop_off() {
  local user
  user=$(_desktop_toml desktop_user)
  run_cmd systemctl --global disable "$SUNSHINE_UNIT" >/dev/null 2>&1 || true
  [[ -n "$user" ]] && { run_cmd systemctl --user -M "$user@" stop "$SUNSHINE_UNIT" >/dev/null 2>&1 || true; }
  ok "remote desktop off (Sunshine stays installed; remove it with: sudo apt remove sunshine)"
}

desktop_status() {
  local user state
  user=$(_desktop_toml desktop_user)
  if ! have sunshine || [[ -z "$user" ]]; then
    printf '  off. Turn on: sudo vercelify desktop on\n'
    return 0
  fi
  state=$(systemctl --user -M "$user@" is-active "$SUNSHINE_UNIT" 2>/dev/null || true)
  printf '  Sunshine on %s'"'"'s desktop: %s\n' "$user" "${state:-not running (starts at login)}"
}

# Forget Sunshine's login so the agent sets its own at the next pairing:
# for a login made by hand on Sunshine's page, or a forgotten one. Paired
# devices stay paired.
desktop_reset_login() {
  local user home state
  user=$(_desktop_toml desktop_user)
  [[ -n "$user" ]] || { err "remote desktop is off: sudo vercelify desktop on"; return 1; }
  home=$(getent passwd "$user" | cut -d: -f6)
  [[ -n "$home" && -d "$home" ]] || { err "no home directory for $user"; return 1; }
  state="$home/.config/sunshine/sunshine_state.json"
  # Sunshine reads its login only when it starts.
  run_cmd systemctl --user -M "$user@" stop "$SUNSHINE_UNIT" >/dev/null 2>&1 || true
  if [[ -f "$state" ]]; then
    run_cmd runuser -u "$user" -- sh -c 'umask 077 && jq "del(.username, .salt, .password)" "$1" >"$1.tmp" && mv "$1.tmp" "$1"' _ "$state" ||
      { err "couldn't edit $state"; return 1; }
  fi
  run_cmd rm -f "$VERCELIFY_STATE/sunshine-login.json"
  run_cmd systemctl --user -M "$user@" start "$SUNSHINE_UNIT" >/dev/null 2>&1 || warn "Sunshine didn't start: it starts with $user's next login"
  ok "Sunshine's login is reset: Launch Moonlight on the Mac pairs it again"
}

_desktop_toml() { { grep -E "^[[:space:]]*$1[[:space:]]*=" "$AGENT_CONFIG" 2>/dev/null || true; } | head -1 | sed -E 's/^[^=]*=[[:space:]]*//; s/^"//; s/"$//'; }

desktop_cli() {
  case "${1:-status}" in
    on) desktop_on ;;
    off) desktop_off ;;
    status) desktop_status ;;
    reset-login) desktop_reset_login ;;
    *) die "usage: vercelify desktop [status|on|off|reset-login]" ;;
  esac
}

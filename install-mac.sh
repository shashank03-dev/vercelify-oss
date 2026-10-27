#!/usr/bin/env bash
# Vercelify for macOS: installs the Vercelify app and its network daemon
# (vercelify-mesh), then opens the app.
#
#   curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install-mac.sh | bash
#
# Options
#   --with-display   also install ffmpeg + Moonlight (laptop screen as a monitor, remote desktop)
#   --from-source    build the app here instead of downloading a release (needs Xcode CLT, Node 20+, Rust)
#   --no-mesh        skip vercelify-mesh, Vercelify's network (needed to reach servers off your LAN)
#   --mesh-system    run vercelify-mesh as root with a real interface (*.vfy for every app;
#                    asks for your password) instead of in the app's user session
#   --pair CODE      pair with a server right away (the full code its installer printed)
#   --dest DIR       install the app into DIR (default: /Applications, else ~/Applications)
#   -y, --yes        never ask
#
# Safe to run again: it updates the app in place. Written for the bash 3.2
# that macOS ships (no ${var,,}, no associative arrays).

set -euo pipefail

REPO=${VERCELIFY_REPO:-shashank03-dev/vercelify-oss}
GITHUB_API=${VERCELIFY_GITHUB_API:-https://api.github.com}
APP_NAME=Vercelify

WITH_DISPLAY=0
FROM_SOURCE=0
WITH_MESH=1
MESH_SYSTEM=0
MESH_LABEL=dev.vercelify.mesh
DEST=''
PAIR_CODE=''
ASSUME_YES=0

if [[ -t 2 && -z "${NO_COLOR:-}" ]]; then
  B=$'\e[1m'; D=$'\e[2m'; G=$'\e[32m'; Y=$'\e[33m'; R=$'\e[31m'; C=$'\e[36m'; N=$'\e[0m'
else
  B=''; D=''; G=''; Y=''; R=''; C=''; N=''
fi
step() { printf '\n%s==>%s %s%s%s\n' "$C" "$N" "$B" "$*" "$N" >&2; }
ok()   { printf '  %s✓%s %s\n' "$G" "$N" "$*" >&2; }
info() { printf '  %s•%s %s\n' "$C" "$N" "$*" >&2; }
warn() { printf '  %s!%s %s\n' "$Y" "$N" "$*" >&2; }
die()  { printf '  %s✗ %s%s\n' "$R" "$*" "$N" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# Works under `curl … | bash`, where stdin is the script.
confirm() {
  local reply=''
  [[ "$ASSUME_YES" == 1 ]] && return 0
  { : </dev/tty; } 2>/dev/null || return 1
  printf '  %s?%s %s [y/N] ' "$Y" "$N" "$1" >/dev/tty
  IFS= read -r reply </dev/tty || true
  case "$reply" in y | Y | yes | Yes | YES) return 0 ;; esac
  return 1
}

# Temporary files and a mounted disk image, cleaned up however we exit.
WORK=''
MOUNTED=''
cleanup() {
  if [[ -n "$MOUNTED" ]]; then hdiutil detach -quiet "$MOUNTED" 2>/dev/null || true; fi
  if [[ -n "$WORK" ]]; then rm -rf "$WORK"; fi
}
trap cleanup EXIT

usage() { sed -n '2,20p' "${BASH_SOURCE[0]:-/dev/null}" 2>/dev/null | sed 's/^# \{0,1\}//' || true; }

parse_args() {
  while (( $# )); do
    case "$1" in
      --with-display) WITH_DISPLAY=1 ;;
      --from-source) FROM_SOURCE=1 ;;
      --no-mesh) WITH_MESH=0 ;;
      --mesh-system) MESH_SYSTEM=1 ;;
      --dest) DEST=${2:?--dest needs a directory}; shift ;;
      --pair) PAIR_CODE=${2:?--pair needs the code the server printed}; shift ;;
      -y|--yes) ASSUME_YES=1 ;;
      -h|--help) usage; exit 0 ;;
      *) die "unknown option: $1 (see --help)" ;;
    esac
    shift
  done
  if [[ -n "$PAIR_CODE" ]]; then
    [[ "$PAIR_CODE" =~ ^vfy[12]\.[A-Za-z0-9_-]+$ ]] || die "--pair needs the full code (vfy2.…) from: sudo vercelify pair"
  fi
}

# Where the app goes: /Applications when writable (admin users), else ~/Applications.
app_dest() {
  if [[ -n "$DEST" ]]; then echo "$DEST"; return; fi
  if [[ -w /Applications ]]; then echo /Applications; else echo "$HOME/Applications"; fi
}

# pick_asset RELEASE_JSON_FILE ARCH — download URL of the best .dmg:
# universal first, then the one for this CPU.
pick_asset() {
  local json=$1 arch=$2 urls want
  urls=$(grep -o '"browser_download_url"[[:space:]]*:[[:space:]]*"[^"]*"' "$json" | sed 's/.*"\(https[^"]*\)"$/\1/')
  case "$arch" in
    arm64|aarch64) want='_aarch64\.dmg$' ;;
    *) want='_x64\.dmg$' ;;
  esac
  { grep -E '_universal\.dmg$' <<<"$urls" || grep -E "$want" <<<"$urls" || true; } | head -1
}

# mesh_asset RELEASE_JSON_FILE — the vercelify-mesh binary for macOS.
mesh_asset() {
  grep -o '"browser_download_url"[[:space:]]*:[[:space:]]*"[^"]*"' "$1" | sed 's/.*"\(https[^"]*\)"$/\1/' |
    { grep -E '/vercelify-mesh-universal-apple-darwin$' || true; } | head -1
}

# Release tag in the same JSON, e.g. v0.2.0.
release_tag() { grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' "$1" | head -1 | sed 's/.*"\([^"]*\)"$/\1/'; }

quit_running_app() {
  if pgrep -xq "$APP_NAME" 2>/dev/null; then
    info "closing the running $APP_NAME app to update it"
    osascript -e "tell application \"$APP_NAME\" to quit" >/dev/null 2>&1 || true
    sleep 2
  fi
}

# install_bundle PATH_TO_APP DEST_DIR
install_bundle() {
  local src=$1 dest=$2
  mkdir -p "$dest"
  quit_running_app
  rm -rf "$dest/$APP_NAME.app.new"
  ditto "$src" "$dest/$APP_NAME.app.new"
  rm -rf "$dest/$APP_NAME.app"
  mv "$dest/$APP_NAME.app.new" "$dest/$APP_NAME.app"
  # Downloaded unsigned builds are quarantined by Gatekeeper; we just verified where it came from.
  xattr -dr com.apple.quarantine "$dest/$APP_NAME.app" 2>/dev/null || true
  ok "installed $dest/$APP_NAME.app"
}

install_from_release() {
  local dest=$1 json url tag mnt app
  WORK=$(mktemp -d)
  json="$WORK/release.json"
  if ! curl -fsSL --retry 3 -H 'Accept: application/vnd.github+json' -o "$json" "$GITHUB_API/repos/$REPO/releases/latest" 2>/dev/null; then
    return 1
  fi
  url=$(pick_asset "$json" "$(uname -m)")
  [[ -n "$url" ]] || return 1
  tag=$(release_tag "$json")
  info "downloading $APP_NAME ${tag:-latest} ($(basename "$url"))…"
  curl -fL --retry 3 --progress-bar -o "$WORK/app.dmg" "$url" || die "download failed: $url"
  # The quarantine flag comes off below, so the image must be the one released.
  curl -fsSL --retry 3 -o "$WORK/app.dmg.sha256" "$url.sha256" || die "no checksum published for $(basename "$url"): not installing it"
  [[ "$(shasum -a 256 "$WORK/app.dmg" | cut -d' ' -f1)" == "$(cut -d' ' -f1 "$WORK/app.dmg.sha256")" ]] || die "$(basename "$url"): checksum mismatch"
  ok "checksum verified"
  mnt="$WORK/mnt"
  mkdir -p "$mnt"
  hdiutil attach -nobrowse -readonly -quiet -mountpoint "$mnt" "$WORK/app.dmg" || die "could not open the disk image"
  MOUNTED=$mnt
  app=$(find "$mnt" -maxdepth 1 -name '*.app' | head -1)
  [[ -n "$app" ]] || die "no app found in the disk image"
  install_bundle "$app" "$dest"
  hdiutil detach -quiet "$mnt" 2>/dev/null || true
  MOUNTED=''
}

install_from_source() {
  local dest=$1 src="${XDG_CACHE_HOME:-$HOME/Library/Caches}/vercelify-src"
  step "Building $APP_NAME from source (one time, 5–15 minutes)"
  xcode-select -p >/dev/null 2>&1 || die "install the Xcode Command Line Tools first: xcode-select --install"
  have node || die "Node.js 20+ is needed: brew install node"
  have cargo || die "Rust is needed: curl https://sh.rustup.rs -sSf | sh"
  if [[ -d "$src/.git" ]]; then
    git -C "$src" fetch --depth 1 origin main -q && git -C "$src" reset --hard -q origin/main
  else
    rm -rf "$src"
    git clone --depth 1 -q "https://github.com/$REPO" "$src"
  fi
  (cd "$src/cockpit" && npm ci --no-audit --no-fund >/dev/null && npx tauri build --bundles app) >&2 || die "the build failed (output above)"
  local app
  # Cargo workspace builds land in the root target/; older layouts used src-tauri/target/.
  app=$(find "$src/target/release/bundle/macos" "$src/cockpit/src-tauri/target/release/bundle/macos" -maxdepth 1 -name '*.app' 2>/dev/null | head -1 || true)
  [[ -n "$app" ]] || die "the build finished but no app bundle was produced"
  install_bundle "$app" "$dest"
}

# The `vfy` command-line tool (deploy, logs, env pull…), next to the app.
install_cli() {
  local bin=${VFY_BIN_DIR:-} url="https://raw.githubusercontent.com/$REPO/main/cli/vfy"
  if [[ -z "$bin" ]]; then
    if [[ -w /usr/local/bin ]]; then bin=/usr/local/bin; else bin="$HOME/.local/bin"; fi
  fi
  mkdir -p "$bin"
  if [[ -n "${VFY_SOURCE:-}" ]]; then cp "$VFY_SOURCE" "$bin/vfy"
  elif ! curl -fsSL --retry 2 -o "$bin/vfy" "$url"; then
    warn "could not download the vfy command-line tool (optional)"
    return 0
  fi
  chmod +x "$bin/vfy"
  ok "vfy installed in $bin (try: vfy apps)"
  case ":$PATH:" in *":$bin:"*) ;; *) info "add $bin to your PATH to use vfy" ;; esac
  have jq || info "vfy needs jq: brew install jq"
}

# Where vercelify-mesh keeps its key and state: the user's (app mode) or
# the system's (--mesh-system, root).
mesh_home() {
  if (( MESH_SYSTEM )); then echo "/Library/Application Support/Vercelify"
  else echo "${VERCELIFY_MESH_HOME:-$HOME/Library/Application Support/Vercelify}"; fi
}

# mesh_plist BIN HOME SYSTEM — the launchd job. App mode: no root, no
# interface; the app reaches servers through SOCKS on 127.0.0.1:7480.
# It waits until the app (or `vercelify-mesh pair CODE --join`) joins.
mesh_plist() {
  local bin=$1 home=$2 system=$3 mode
  if [[ "$system" == 1 ]]; then
    mode='<string>--port</string><string>0</string>'
  else
    mode='<string>--userspace</string><string>--port</string><string>0</string><string>--socks</string><string>127.0.0.1:7480</string>'
  fi
  cat <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$MESH_LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$bin</string>
    <string>--dir</string><string>$home/mesh</string>
    <string>--run-dir</string><string>$home/run</string>
    <string>up</string>
    $mode
  </array>
  <key>EnvironmentVariables</key>
  <dict><key>RUST_LOG</key><string>vercelify_mesh=info</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>StandardErrorPath</key><string>$home/run/mesh.log</string>
</dict>
</plist>
PLIST
}

# The binary: VERCELIFY_MESH_BINARY, else the release's, else built here.
obtain_mesh_binary() {
  local out=$1 json url src="${XDG_CACHE_HOME:-$HOME/Library/Caches}/vercelify-src"
  if [[ -n "${VERCELIFY_MESH_BINARY:-}" ]]; then cp "$VERCELIFY_MESH_BINARY" "$out"; return; fi
  json="${WORK:-$(mktemp -d)}/release.json"
  if [[ -s "$json" ]] || curl -fsSL --retry 3 -H 'Accept: application/vnd.github+json' -o "$json" "$GITHUB_API/repos/$REPO/releases/latest" 2>/dev/null; then
    url=$(mesh_asset "$json")
    if [[ -n "$url" ]] && curl -fsSL --retry 3 -o "$out" "$url" && curl -fsSL --retry 3 -o "$out.sha256" "$url.sha256"; then
      [[ "$(shasum -a 256 "$out" | cut -d' ' -f1)" == "$(cut -d' ' -f1 "$out.sha256")" ]] || die "vercelify-mesh: checksum mismatch"
      rm -f "$out.sha256"
      return 0
    fi
  fi
  have cargo || return 1
  [[ -d "$src/.git" ]] || git clone --depth 1 -q "https://github.com/$REPO" "$src" || return 1
  (cd "$src" && cargo build --release -q -p vercelify-mesh --features daemon) >&2 || return 1
  cp "$src/target/release/vercelify-mesh" "$out"
}

install_mesh() {
  step "Vercelify network (vercelify-mesh: your servers, privately, from anywhere)"
  local home bin plist sudo='' domain
  home=$(mesh_home)
  if (( MESH_SYSTEM )); then
    sudo=sudo
    plist="/Library/LaunchDaemons/$MESH_LABEL.plist"
    domain=system
  else
    plist="${VERCELIFY_LAUNCH_AGENTS:-$HOME/Library/LaunchAgents}/$MESH_LABEL.plist"
    domain="gui/$(id -u)"
  fi
  bin="$home/bin/vercelify-mesh"
  $sudo mkdir -p "$home/bin" "$home/mesh" "$home/run" "$(dirname "$plist")"
  local tmp
  tmp=$(mktemp)
  if ! obtain_mesh_binary "$tmp"; then
    rm -f "$tmp"
    warn "no vercelify-mesh build for this Mac yet: the app reaches servers on your LAN only (re-run with --from-source to build it)"
    return 0
  fi
  $sudo install -m 0755 "$tmp" "$bin"
  rm -f "$tmp"
  mesh_plist "$bin" "$home" "$MESH_SYSTEM" | $sudo tee "$plist" >/dev/null
  if [[ "${VERCELIFY_NO_LAUNCHCTL:-0}" != 1 ]]; then
    $sudo launchctl bootout "$domain/$MESH_LABEL" 2>/dev/null || true
    $sudo launchctl bootstrap "$domain" "$plist" || warn "launchctl couldn't start $MESH_LABEL (see $home/run/mesh.log)"
  fi
  if (( MESH_SYSTEM )); then
    ok "vercelify-mesh runs as root (*.vfy names on this Mac); join with:"
    info "sudo \"$bin\" --dir \"$home/mesh\" pair <code> --join"
  else
    ok "vercelify-mesh runs in your session; the app joins it to your servers' network when you pair"
  fi
}

# The app reads this once at start (and removes it): only this user can read it.
leave_pairing() {
  local dir="${VERCELIFY_APP_CONFIG:-$HOME/Library/Application Support/dev.vercelify.cockpit}"
  mkdir -p "$dir"
  ( umask 077; printf '%s\n' "$1" >"$dir/pending-pairing" )
}

install_display_tools() {
  step "Display streaming tools"
  have brew || { warn "Homebrew is needed for ffmpeg and Moonlight: https://brew.sh"; return 0; }
  have ffmpeg || brew install ffmpeg >/dev/null
  [[ -d /Applications/Moonlight.app ]] || brew install --cask moonlight >/dev/null || warn "could not install Moonlight"
  ok "ffmpeg and Moonlight ready"
}

main() {
  parse_args "$@"
  [[ "$(uname -s)" == Darwin ]] || die "this installer is for macOS — on Linux servers use install.sh"
  printf '\n%s%s Vercelify for Mac%s — your own Vercel on your old laptops\n' "$B" "$C" "$N" >&2

  local dest
  dest=$(app_dest)
  step "$APP_NAME app"
  if (( FROM_SOURCE )); then
    install_from_source "$dest"
  elif ! install_from_release "$dest"; then
    warn "no downloadable release of $REPO yet"
    if confirm "Build the app from source now (needs Xcode CLT, Node 20+ and Rust)?"; then
      install_from_source "$dest"
    else
      die "re-run with --from-source to build it, or download a release from https://github.com/$REPO/releases"
    fi
  fi

  (( WITH_MESH )) && install_mesh
  (( WITH_DISPLAY )) && install_display_tools
  install_cli

  printf '\n%s%s✔ Done.%s Opening %s…\n\n' "$B" "$G" "$N" "$APP_NAME" >&2
  if [[ -n "$PAIR_CODE" ]]; then
    leave_pairing "$PAIR_CODE"
    printf '  The app pairs with your server as it opens.\n\n' >&2
  else
    printf '  Next: on your old laptop run the server installer, then pick it in the app\n  and type the 8-character code it shows.\n%s    curl -fsSL https://raw.githubusercontent.com/%s/main/install.sh | sudo bash%s\n\n' "$D" "$REPO" "$N" >&2
  fi
  open "$dest/$APP_NAME.app" 2>/dev/null || true
}

if [[ "${VERCELIFY_SOURCE_ONLY:-0}" != 1 ]]; then
  main "$@"
fi

#!/usr/bin/env bash
# Vercelify for macOS: installs Tailscale and the Vercelify app, then opens it.
#
#   curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install-mac.sh | bash
#
# Options
#   --with-display   also install ffmpeg + Moonlight (laptop screen as a monitor, remote desktop)
#   --from-source    build the app here instead of downloading a release (needs Xcode CLT, Node 20+, Rust)
#   --no-tailscale   skip the Tailscale check
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
WITH_TAILSCALE=1
DEST=''
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

usage() { sed -n '2,16p' "${BASH_SOURCE[0]:-/dev/null}" 2>/dev/null | sed 's/^# \{0,1\}//' || true; }

parse_args() {
  while (( $# )); do
    case "$1" in
      --with-display) WITH_DISPLAY=1 ;;
      --from-source) FROM_SOURCE=1 ;;
      --no-tailscale) WITH_TAILSCALE=0 ;;
      --dest) DEST=${2:?--dest needs a directory}; shift ;;
      -y|--yes) ASSUME_YES=1 ;;
      -h|--help) usage; exit 0 ;;
      *) die "unknown option: $1 (see --help)" ;;
    esac
    shift
  done
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

# Release tag in the same JSON, e.g. v0.2.0.
release_tag() { grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' "$1" | head -1 | sed 's/.*"\([^"]*\)"$/\1/'; }

install_tailscale() {
  step "Tailscale (private network to your servers)"
  if [[ -d /Applications/Tailscale.app ]] || have tailscale; then
    ok "Tailscale is installed — make sure you are signed in with the same account as your servers"
    return 0
  fi
  if have brew; then
    info "installing Tailscale with Homebrew…"
    brew install --cask tailscale >/dev/null && { ok "Tailscale installed — open it from the menu bar and sign in"; open -a Tailscale 2>/dev/null || true; return 0; }
    warn "Homebrew could not install Tailscale"
  fi
  warn "install Tailscale from the Mac App Store or tailscale.com/download, then sign in"
  open "https://tailscale.com/download/mac" 2>/dev/null || true
}

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
  (( WITH_TAILSCALE )) && install_tailscale

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

  (( WITH_DISPLAY )) && install_display_tools
  install_cli

  printf '\n%s%s✔ Done.%s Opening %s…\n\n' "$B" "$G" "$N" "$APP_NAME" >&2
  printf '  Next: on your old laptop run the server installer, then pick it in the app\n  and type the 8-character code it shows.\n%s    curl -fsSL https://raw.githubusercontent.com/%s/main/install.sh | sudo bash%s\n\n' "$D" "$REPO" "$N" >&2
  open "$dest/$APP_NAME.app" 2>/dev/null || true
}

if [[ "${VERCELIFY_SOURCE_ONLY:-0}" != 1 ]]; then
  main "$@"
fi

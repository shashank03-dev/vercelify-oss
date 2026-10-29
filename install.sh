#!/usr/bin/env bash
# Vercelify bootstrap installer.
#
#   curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install.sh | sudo bash
#   curl -fsSL .../install.sh | sudo bash -s -- --domain example.com --cloudflare-token XXXX
#   curl -fsSL .../install.sh | sudo bash -s -- --worker          # Raspberry Pi / extra node
#
# Installs the latest *release* (scripts and agent binary of the same version).
# VERCELIFY_REF=v0.2.0 pins a release; VERCELIFY_CHANNEL=edge follows `main`.
# Uses the checkout this file lives in when run from a clone.

set -euo pipefail

REPO=${VERCELIFY_REPO:-shashank03-dev/vercelify-oss}
CHANNEL=${VERCELIFY_CHANNEL:-stable}
REF=${VERCELIFY_REF:-}
DEST=${VERCELIFY_SRC_DIR:-/opt/vercelify}
API=${VERCELIFY_GITHUB_API:-https://api.github.com}

if [[ $EUID -ne 0 ]]; then
  echo "Please run as root:  curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | sudo bash" >&2
  exit 1
fi

# Tag of the newest published release, or nothing.
latest_tag() {
  curl -fsSL --retry 3 -m 20 -H 'Accept: application/vnd.github+json' "$API/repos/$REPO/releases/latest" 2>/dev/null |
    sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1
}

resolve_ref() {
  [[ -n "$REF" ]] && { echo "$REF"; return; }
  if [[ "$CHANNEL" == edge ]]; then echo main; return; fi
  local tag
  tag=$(latest_tag || true)
  if [[ -n "$tag" ]]; then echo "$tag"; else
    echo "==> No release published yet — installing from main (edge)" >&2
    echo main
  fi
}

# Everything runs from main, called on the last line: a download cut short
# by the network runs nothing instead of half the steps.
main() {
  local here src ref
  here=''
  if [[ -n "${BASH_SOURCE[0]:-}" && -f "${BASH_SOURCE[0]}" ]]; then
    here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
  fi

  if [[ -n "$here" && -x "$here/host/setup-host.sh" ]]; then
    src=$here
    ref=${REF:-local}
  else
    command -v curl >/dev/null || { apt-get update -qq && apt-get install -y -qq curl ca-certificates >/dev/null; }
    command -v tar >/dev/null || apt-get install -y -qq tar >/dev/null
    ref=$(resolve_ref)
    [[ "$ref" =~ ^[A-Za-z0-9._/-]+$ ]] || { echo "invalid ref '$ref'" >&2; exit 1; }
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    echo "==> Downloading Vercelify ($REPO@$ref)…" >&2
    curl -fsSL --retry 3 "https://codeload.github.com/$REPO/tar.gz/$ref" | tar -xz -C "$tmp" --strip-components=1
    rm -rf "$DEST.new"
    mkdir -p "$DEST.new"
    cp -a "$tmp/." "$DEST.new/"
    rm -rf "$DEST.old"
    [[ -d "$DEST" ]] && mv "$DEST" "$DEST.old"
    mv "$DEST.new" "$DEST"
    rm -rf "$DEST.old"
    # exec below skips the EXIT trap: don't leave the download behind.
    rm -rf "$tmp"
    trap - EXIT
    src=$DEST
  fi

  export VERCELIFY_INSTALL_REF=$ref VERCELIFY_INSTALL_CHANNEL=$CHANNEL
  exec bash "$src/host/setup-host.sh" "$@"
}

main "$@"

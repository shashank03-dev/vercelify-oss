#!/usr/bin/env bash
# Seal the macOS app with a signature macOS can recognise across builds, then
# rebuild its disk image (and the .app.tar.gz) from the signed app.
#
#   seal-macos-app.sh APP DMG [TARBALL]
#
# MAC_SIGN_IDENTITY is a code-signing identity in MAC_SIGN_KEYCHAIN (a
# self-signed certificate is enough). Then macOS knows the app by its bundle
# id and that certificate, so Screen Recording stays allowed across updates.
# Without one the app is signed ad hoc: still a sealed bundle under its own id
# (the linker's signature alone isn't), but each build is a new app to macOS.
# Releases with a Developer ID certificate are signed by Tauri instead.
set -euo pipefail

app=${1:?usage: seal-macos-app.sh APP DMG [TARBALL]} dmg=${2:?usage: seal-macos-app.sh APP DMG [TARBALL]} tarball=${3:-}
identity=${MAC_SIGN_IDENTITY:--}
bundle_id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist")
name=$(basename "$app" .app)

args=(--force --timestamp=none --sign "$identity" --identifier "$bundle_id")
[[ -z "${MAC_SIGN_KEYCHAIN:-}" ]] || args+=(--keychain "$MAC_SIGN_KEYCHAIN")
codesign "${args[@]}" "$app"
codesign --verify --strict "$app"
codesign --display --requirements - "$app" 2>&1 | grep 'designated =>'

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
ditto "$app" "$stage/$name.app"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
hdiutil create -quiet -volname "$name" -srcfolder "$stage" -fs HFS+ -format UDZO "$dmg"
if [[ -n "$tarball" ]]; then
  tar -C "$(dirname "$app")" -czf "$tarball" "$name.app"
fi

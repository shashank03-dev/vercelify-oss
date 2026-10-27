#!/usr/bin/env bats
# install-mac.sh logic that can run anywhere (asset choice, options, guards).

setup() {
  T=$(mktemp -d)
  export VERCELIFY_SOURCE_ONLY=1 NO_COLOR=1
  # shellcheck source=../../install-mac.sh
  . "$BATS_TEST_DIRNAME/../../install-mac.sh"
  FIX="$BATS_TEST_DIRNAME/fixtures"
}

teardown() { rm -rf "$T"; }

@test "picks the .dmg for this Mac's CPU" {
  run pick_asset "$FIX/github-release.json" arm64
  [ "$output" = https://github.com/shashank03-dev/vercelify-oss/releases/download/v0.2.0/Vercelify_0.2.0_aarch64.dmg ]
  run pick_asset "$FIX/github-release.json" x86_64
  [ "$output" = https://github.com/shashank03-dev/vercelify-oss/releases/download/v0.2.0/Vercelify_0.2.0_x64.dmg ]
  run release_tag "$FIX/github-release.json"
  [ "$output" = v0.2.0 ]
}

@test "a universal build wins over per-CPU builds" {
  sed 's/_aarch64\.dmg/_universal.dmg/' "$FIX/github-release.json" >"$T/r.json"
  run pick_asset "$T/r.json" x86_64
  [[ "$output" == *_universal.dmg ]]
}

@test "a release without a Mac build gives nothing to download" {
  grep -v '\.dmg' "$FIX/github-release.json" >"$T/r.json"
  run pick_asset "$T/r.json" arm64
  [ -z "$output" ]
}

@test "options" {
  # --no-tailscale (from older instructions) is accepted and changes nothing
  parse_args --with-display --from-source --no-tailscale --dest "$T/apps" -y
  [ "$WITH_DISPLAY" = 1 ] && [ "$FROM_SOURCE" = 1 ] && [ "$ASSUME_YES" = 1 ]
  [ "$(app_dest)" = "$T/apps" ]
  run parse_args --bogus
  [ "$status" -ne 0 ]
  [[ "$output" == *"unknown option: --bogus"* ]]
}

@test "falls back to ~/Applications when /Applications is not writable" {
  DEST=''
  HOME=$T
  if [[ -w /Applications ]]; then skip "/Applications is writable here"; fi
  [ "$(app_dest)" = "$T/Applications" ]
}

@test "refuses to run anywhere but macOS" {
  [[ "$(uname -s)" == Darwin ]] && skip "running on macOS"
  run env VERCELIFY_SOURCE_ONLY=0 bash "$BATS_TEST_DIRNAME/../../install-mac.sh"
  [ "$status" -ne 0 ]
  [[ "$output" == *"this installer is for macOS"* ]]
}

@test "--help prints the options" {
  run env VERCELIFY_SOURCE_ONLY=0 bash "$BATS_TEST_DIRNAME/../../install-mac.sh" --help
  [ "$status" -eq 0 ]
  [[ "$output" == *"--with-display"* ]]
}

@test "the vfy CLI is installed next to the app" {
  VERCELIFY_SOURCE_ONLY=1 . "$BATS_TEST_DIRNAME/../../install-mac.sh"
  bin=$(mktemp -d)
  VFY_BIN_DIR="$bin" VFY_SOURCE="$BATS_TEST_DIRNAME/../../cli/vfy" run install_cli
  [ "$status" -eq 0 ]
  [ -x "$bin/vfy" ]
  [[ "$output" == *"vfy installed in $bin"* ]]
  rm -rf "$bin"
}

@test "the network daemon: release asset and launchd job" {
  printf '{"assets":[{"browser_download_url":"https://github.com/o/r/releases/download/v1/vercelify-mesh-universal-apple-darwin"},{"browser_download_url":"https://github.com/o/r/releases/download/v1/vercelify-mesh-universal-apple-darwin.sha256"}]}' >"$T/r.json"
  [ "$(mesh_asset "$T/r.json")" = https://github.com/o/r/releases/download/v1/vercelify-mesh-universal-apple-darwin ]
  [ -z "$(mesh_asset "$FIX/github-release.json")" ]
  run mesh_plist /x/vercelify-mesh "/Users/me/Library/Application Support/Vercelify" 0
  [[ "$output" == *"<string>--userspace</string>"* ]]
  [[ "$output" == *"<string>127.0.0.1:7480</string>"* ]]
  [[ "$output" == *"<string>/Users/me/Library/Application Support/Vercelify/mesh</string>"* ]]
  [[ "$output" == *"<key>KeepAlive</key><true/>"* ]]
  run mesh_plist /x/vercelify-mesh /L 1
  [[ "$output" != *"--userspace"* ]]
  # A real property list, with `up` where launchd will run it.
  mesh_plist /x /h 0 >"$T/job.plist"
  run python3 "$BATS_TEST_DIRNAME/fixtures/check_plist.py" "$T/job.plist"
  [ "$status" -eq 0 ]
}

@test "install_mesh puts the binary and the LaunchAgent in place" {
  printf '#!/bin/sh\necho mesh\n' >"$T/fake-mesh"
  HOME=$T VERCELIFY_MESH_BINARY="$T/fake-mesh" VERCELIFY_NO_LAUNCHCTL=1 run install_mesh
  [ "$status" -eq 0 ]
  [ -x "$T/Library/Application Support/Vercelify/bin/vercelify-mesh" ]
  grep -q "<string>$T/Library/Application Support/Vercelify/bin/vercelify-mesh</string>" "$T/Library/LaunchAgents/dev.vercelify.mesh.plist"
  parse_args --no-mesh --mesh-system
  [ "$WITH_MESH" = 0 ] && [ "$MESH_SYSTEM" = 1 ]
  [ "$(mesh_home)" = "/Library/Application Support/Vercelify" ]
}

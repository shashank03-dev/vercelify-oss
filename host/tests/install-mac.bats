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
  parse_args --with-display --from-source --no-tailscale --dest "$T/apps" -y
  [ "$WITH_DISPLAY" = 1 ] && [ "$FROM_SOURCE" = 1 ] && [ "$WITH_TAILSCALE" = 0 ] && [ "$ASSUME_YES" = 1 ]
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
  run env VERCELIFY_SOURCE_ONLY=0 bash "$BATS_TEST_DIRNAME/../../install-mac.sh" --no-tailscale
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

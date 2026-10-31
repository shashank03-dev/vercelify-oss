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
  parse_args --with-display --from-source --dest "$T/apps" -y
  [ "$WITH_DISPLAY" = 1 ] && [ "$FROM_SOURCE" = 1 ] && [ "$ASSUME_YES" = 1 ]
  [ "$(app_dest)" = "$T/apps" ]
  run parse_args --bogus
  [ "$status" -ne 0 ]
  [[ "$output" == *"unknown option: --bogus"* ]]
}

@test "--pair leaves the code for the app, readable by this user only" {
  run parse_args --pair 'not a code'
  [ "$status" -ne 0 ]
  [[ "$output" == *"needs the full code"* ]]
  parse_args --pair vfy2.abc_DEF-123
  [ "$PAIR_CODE" = vfy2.abc_DEF-123 ]
  VERCELIFY_APP_CONFIG="$T/cfg" leave_pairing "$PAIR_CODE"
  [ "$(cat "$T/cfg/pending-pairing")" = vfy2.abc_DEF-123 ]
  [ "$(ls -l "$T/cfg/pending-pairing" | cut -c1-10)" = "-rw-------" ]
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

@test "a new signature clears the stale Screen Recording entry, the same one keeps it" {
  mkdir -p "$T/bin" "$T/new.app" "$T/dest"
  # codesign reports the requirement written in the bundle; tccutil logs.
  printf '#!/bin/sh\nfor a; do last=$a; done\n[ -f "$last/req" ] && echo "designated => $(cat "$last/req")"\nexit 0\n' >"$T/bin/codesign"
  printf '#!/bin/sh\necho "tccutil $*" >>"%s/calls"\n' "$T" >"$T/bin/tccutil"
  printf '#!/bin/sh\nexit 1\n' >"$T/bin/pgrep"
  # macOS tools, for runs on Linux.
  printf '#!/bin/sh\ncp -R "$1" "$2"\n' >"$T/bin/ditto"
  printf '#!/bin/sh\nexit 0\n' >"$T/bin/xattr"
  chmod +x "$T/bin/"*
  PATH="$T/bin:$PATH"
  echo 'identifier "dev.vercelify.cockpit" and certificate leaf = H"aa"' >"$T/new.app/req"
  # First install: nothing to clear.
  run install_bundle "$T/new.app" "$T/dest"
  [ "$status" -eq 0 ]
  [ ! -f "$T/calls" ]
  # Same signature again: the permission stays.
  run install_bundle "$T/new.app" "$T/dest"
  [ ! -f "$T/calls" ]
  # A new one (an ad hoc build, a new certificate): cleared.
  echo 'cdhash H"bb"' >"$T/new.app/req"
  run install_bundle "$T/new.app" "$T/dest"
  [ "$status" -eq 0 ]
  grep -qx 'tccutil reset ScreenCapture dev.vercelify.cockpit' "$T/calls"
  [[ "$output" == *"Screen Recording will be asked for once more"* ]]
}

@test "optional display tool failures do not abort installation or claim success" {
  have() { [[ "$1" == brew ]]; }
  brew() { return 1; }
  run install_display_tools
  [ "$status" -eq 0 ]
  [[ "$output" != *"ffmpeg and Moonlight ready"* ]]
}

# A launchctl whose old job lingers for $T/linger prints, whose first
# bootstrap fails like macOS's "Bootstrap failed: 5", then runs the job.
fake_launchctl() {
  cat >"$T/launchctl" <<STUB
#!/usr/bin/env bash
echo "launchctl \$*" >>"$T/calls"
case "\$1" in
  bootout) echo 2 >"$T/linger"; exit 0 ;;
  print)
    n=\$(cat "$T/linger" 2>/dev/null || echo 0)
    if (( n > 0 )); then echo \$(( n - 1 )) >"$T/linger"; echo "state = running"; exit 0; fi
    [ -e "$T/loaded" ] && { [ -e "$T/never-runs" ] && echo "state = spawn scheduled" || echo "state = running"; exit 0; }
    exit 113 ;;
  bootstrap)
    if [ ! -e "$T/failed-once" ]; then touch "$T/failed-once"; echo "Bootstrap failed: 5: Input/output error" >&2; exit 5; fi
    touch "$T/loaded" ;;
esac
STUB
  chmod +x "$T/launchctl"
  printf '#!/bin/sh\nexit 0\n' >"$T/sleep"; chmod +x "$T/sleep"
  export LAUNCHCTL="$T/launchctl" PATH="$T:$PATH"
}

@test "the network job is restarted reliably: waits for the old one, retries, checks it runs" {
  fake_launchctl
  run mesh_job_restart "gui/501" "$T/job.plist"
  [ "$status" -eq 0 ]
  # bootstrap only after the old job was gone, and tried again after the failure
  [ "$(grep -c 'launchctl bootstrap' "$T/calls")" -eq 2 ]
  first_bootstrap=$(grep -n 'bootstrap' "$T/calls" | head -1 | cut -d: -f1)
  last_print_before=$(head -n "$first_bootstrap" "$T/calls" | grep -c 'print')
  [ "$last_print_before" -ge 3 ]
}

@test "a network job that never runs fails loudly instead of claiming success" {
  fake_launchctl
  touch "$T/never-runs"
  printf '#!/bin/sh\necho mesh\n' >"$T/fake-mesh"
  HOME=$T VERCELIFY_MESH_BINARY="$T/fake-mesh" MESH_JOB_WAIT=3 run install_mesh
  [ "$status" -ne 0 ]
  [[ "$output" == *"vercelify-mesh isn't running"* ]]
  [[ "$output" != *"runs in your session"* ]]
}

@test "an app built from source gets its network daemon from the same checkout" {
  mkdir -p "$T/src/target/release"
  printf '#!/bin/sh\necho "cargo $*" >>"%s/cargo.log"; printf "#!/bin/sh\\n" >"%s/src/target/release/vercelify-mesh"\n' "$T" "$T" >"$T/cargo"
  chmod +x "$T/cargo"
  printf '#!/bin/sh\necho "curl $*" >>"%s/curl.log"; exit 22\n' "$T" >"$T/curl"; chmod +x "$T/curl"
  SOURCE_DIR="$T/src" CARGO="$T/cargo" PATH="$T:$PATH" run obtain_mesh_binary "$T/out"
  [ "$status" -eq 0 ]
  [ -e "$T/out" ]
  grep -q 'build --release -q -p vercelify-mesh --features daemon' "$T/cargo.log"
  [ ! -e "$T/curl.log" ]   # no release download
}

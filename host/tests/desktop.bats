#!/usr/bin/env bats
bats_require_minimum_version 1.5.0
# `vercelify desktop`: Sunshine from its release (checksum checked), started
# as the desktop user's unit, and the agent told whose it is.

setup() {
  T=$(mktemp -d)
  mkdir -p "$T/bin" "$T/etc" "$T/dl"
  printf 'listen = ["127.0.0.1", "mesh"]\n' >"$T/etc/agent.toml"
  printf 'ID=ubuntu\nVERSION_ID="24.04"\nVERSION_CODENAME=noble\nUBUNTU_CODENAME=noble\n' >"$T/os-release"
  # The release file, and what GitHub says about it.
  printf 'not really a deb\n' >"$T/dl/pkg"
  DIGEST=$(sha256sum "$T/dl/pkg" | cut -d' ' -f1)
  cat >"$T/bin/loginctl" <<'STUB'
#!/usr/bin/env bash
case "$1" in
  list-sessions) [[ -n "${NO_DESKTOP:-}" ]] || printf '  3 1000 ana seat0 tty2\n  7 0 root - pts/1\n' ;;
  show-session)
    case "$2:$4" in
      3:Type) echo "${SESSION_TYPE:-wayland}" ;; 3:Name) echo ana ;;
      7:Type) echo tty ;; 7:Name) echo root ;;
    esac ;;
esac
STUB
  cat >"$T/bin/curl" <<'STUB'
#!/usr/bin/env bash
echo "curl $*" >>"$CALLS"
if [[ "$*" == *api.test* ]]; then
  cat <<JSON
{"tag_name":"v2026.1","assets":[
 {"name":"sunshine_2026.1-1+debiantrixie_amd64.deb","browser_download_url":"https://dl.test/trixie.deb","digest":"sha256:${DIGEST}"},
 {"name":"sunshine_2026.1-1+ubuntu24.04_amd64.deb","browser_download_url":"https://dl.test/noble.deb","digest":"sha256:${GH_DIGEST:-$DIGEST}"}]}
JSON
  exit 0
fi
# fetch URL FILE: curl … -o FILE URL
while [[ $# -gt 0 ]]; do [[ "$1" == -o ]] && { cp "$T/dl/pkg" "$2"; exit 0; }; shift; done
exit 0
STUB
  cat >"$T/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
echo "systemctl $*" >>"$CALLS"
[[ "$*" == *"is-system-running"* ]] && { echo "${USER_MANAGER:-running}"; [[ "${USER_MANAGER:-running}" == running ]]; exit; }
[[ "$*" == *"is-active"* ]] && { echo active; exit 0; }
exit 0
STUB
  printf '#!/usr/bin/env bash\necho "apt-get $*" >>"$CALLS"\n' >"$T/bin/apt-get"
  printf '#!/usr/bin/env bash\necho amd64\n' >"$T/bin/dpkg"
  printf '#!/usr/bin/env bash\necho "$(basename "$0") $*" >>"$CALLS"\n' >"$T/bin/modprobe"
  cp "$T/bin/modprobe" "$T/bin/udevadm"
  chmod +x "$T/bin/"*
  unset SUDO_USER
  export CALLS="$T/calls" PATH="$T/bin:$PATH" NO_COLOR=1 VERCELIFY_TEST=1 T DIGEST
  export VERCELIFY_ETC="$T/etc" VERCELIFY_LOG="$T/log" OS_RELEASE="$T/os-release"
  export SUNSHINE_RELEASE_API=https://api.test/latest
  : >"$CALLS"
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/common.sh"
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/mesh.sh"
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/desktop.sh"
  have() { [[ "$1" != sunshine ]] && command -v "$1" >/dev/null; }
}

teardown() { rm -rf "$T"; }

@test "the package for this system" {
  run sunshine_asset_suffix
  [ "$output" = "+ubuntu24.04_amd64.deb" ]
  printf 'ID=debian\nVERSION_ID="13"\nVERSION_CODENAME=trixie\n' >"$T/os-release"
  run sunshine_asset_suffix
  [ "$output" = "+debiantrixie_amd64.deb" ]
  printf 'ID=linuxmint\nVERSION_ID="22"\nUBUNTU_CODENAME=noble\n' >"$T/os-release"
  run sunshine_asset_suffix
  [ "$output" = "+ubuntu24.04_amd64.deb" ]
  printf 'ID=fedora\nVERSION_ID="41"\n' >"$T/os-release"
  run ! sunshine_asset_suffix
}

@test "the desktop's user: a graphical session, not root" {
  run desktop_user
  [ "$output" = ana ]
  SESSION_TYPE=tty run ! desktop_user
  NO_DESKTOP=1 run ! desktop_user
}

@test "on: the checked package, Sunshine for graphical logins and now, the agent told" {
  run desktop_on
  [ "$status" -eq 0 ]
  grep -q 'apt-get install -y -qq .*/sunshine_2026.1-1+ubuntu24.04_amd64.deb' "$CALLS"
  grep -q 'systemctl --global enable app-dev.lizardbyte.app.Sunshine.service' "$CALLS"
  grep -q 'systemctl --user -M ana@ restart app-dev.lizardbyte.app.Sunshine.service' "$CALLS"
  grep -qx 'desktop_user = "ana"' "$T/etc/agent.toml"
  grep -q 'systemctl restart vercelify-agent' "$CALLS"
  [[ "$output" == *"Launch Moonlight"* ]]
  # Again: nothing to tell the agent.
  : >"$CALLS"
  run desktop_on
  run ! grep -q 'restart vercelify-agent' "$CALLS"
}

@test "not logged in yet: enabled, starts at login" {
  USER_MANAGER=offline run desktop_on
  [ "$status" -eq 0 ]
  [[ "$output" == *"starts when ana logs in"* ]]
  grep -q 'systemctl --global enable' "$CALLS"
  run ! grep -q 'ana@ restart' "$CALLS"
}

@test "a checksum that doesn't match: nothing installed" {
  GH_DIGEST=$(printf '0%.0s' {1..64}) run desktop_on
  [ "$status" -ne 0 ]
  [[ "$output" == *"checksum mismatch"* ]]
  run ! grep -q 'apt-get' "$CALLS"
  run ! grep -q desktop_user "$T/etc/agent.toml"
}

@test "no desktop: says so, installs nothing" {
  NO_DESKTOP=1 run desktop_on
  [ "$status" -ne 0 ]
  [[ "$output" == *"no desktop here"* ]]
  run ! grep -q 'api.test' "$CALLS"
}

@test "off: no longer started" {
  printf 'desktop_user = "ana"\n' >>"$T/etc/agent.toml"
  run desktop_off
  [ "$status" -eq 0 ]
  grep -q 'systemctl --global disable app-dev.lizardbyte.app.Sunshine.service' "$CALLS"
  grep -q 'systemctl --user -M ana@ stop' "$CALLS"
}

@test "on, under the CLI's set -euo pipefail" {
  run bash -euo pipefail -c '. "$1/lib/common.sh"; . "$1/lib/mesh.sh"; . "$1/lib/desktop.sh"; desktop_on' _ "$BATS_TEST_DIRNAME/.."
  [ "$status" -eq 0 ]
  grep -qx 'desktop_user = "ana"' "$T/etc/agent.toml"
}

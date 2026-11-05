#!/usr/bin/env bats
# ./dev and the Docker test suites on a machine that is also a Vercelify server.

REPO="$BATS_TEST_DIRNAME/../.."

setup() {
  T=$(mktemp -d)
  mkdir "$T/etc"
}

teardown() { rm -rf "$T"; }

@test "the dev agent refuses to run on a Vercelify server" {
  VERCELIFY_ETC="$T/etc" run "$REPO/dev" agent
  [ "$status" -eq 1 ]
  [[ "$output" == *"this machine is a Vercelify server"* ]]
  [[ "$output" == *"./dev host"* ]]
}

@test "the proxy-replacing test suites refuse on a server, unless told to go ahead" {
  VERCELIFY_ETC="$T/etc" run bash -c ". '$REPO/host/tests/contract/lib-guard.sh'; echo ran"
  [ "$status" -eq 1 ]
  [[ "$output" == *"would take its sites offline"* ]]
  [[ "$output" != *ran* ]]
  VERCELIFY_ETC="$T/etc" VERCELIFY_TESTS_ON_SERVER=1 run bash -c ". '$REPO/host/tests/contract/lib-guard.sh'; echo ran"
  [ "$status" -eq 0 ]
  [ "$output" = ran ]
  VERCELIFY_ETC="$T/none" run bash -c ". '$REPO/host/tests/contract/lib-guard.sh'; echo ran"
  [ "$output" = ran ]
}

@test "the screencast contract names missing receiver tools before building anything" {
  [ "$(uname -s)" = Linux ] || skip "the Linux path"
  mkdir "$T/bin"
  for t in bash env uname dirname git grep sed date cat; do
    p=$(command -v "$t") && ln -s "$p" "$T/bin/$t"
  done
  PATH="$T/bin" HOME="$T" run "$REPO/dev" screencast
  [ "$status" -eq 1 ]
  [[ "$output" == *"missing mpv"* ]]
  [[ "$output" == *"apt install mpv ffmpeg"* ]]
}

@test "screencast builds a fresh agent and sender helper before running the contract" {
  [ "$(uname -s)" = Linux ] || skip "the Linux path"
  mkdir "$T/bin"
  for t in bash env uname dirname git grep sed date cat touch; do
    p=$(command -v "$t") && ln -s "$p" "$T/bin/$t"
  done
  for t in mpv ffmpeg tc; do
    printf '#!/bin/sh\nexit 0\n' >"$T/bin/$t"
  done
  cat >"$T/bin/cargo" <<'STUB'
#!/bin/sh
# Cargo's example filter selects only examples, even with several packages.
case "$*" in
  *--example*) touch "$BUILD_ARTIFACTS/helper" ;;
  *vercelify-agent*) touch "$BUILD_ARTIFACTS/agent" ;;
esac
STUB
  printf '#!/bin/sh\nexec "$@"\n' >"$T/bin/sudo"
  cat >"$T/bin/python3" <<'STUB'
#!/bin/sh
[ -f "$BUILD_ARTIFACTS/agent" ] && [ -f "$BUILD_ARTIFACTS/helper" ] || exit 1
echo 'contract ran with both fresh binaries'
STUB
  chmod +x "$T/bin/cargo" "$T/bin/sudo" "$T/bin/python3" "$T/bin/mpv" "$T/bin/ffmpeg" "$T/bin/tc"
  PATH="$T/bin" HOME="$T" BUILD_ARTIFACTS="$T" run "$REPO/dev" screencast api_validation
  [ "$status" -eq 0 ]
  [[ "$output" == *"contract ran with both fresh binaries"* ]]
}

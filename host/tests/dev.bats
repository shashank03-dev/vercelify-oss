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

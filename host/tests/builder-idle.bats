#!/usr/bin/env bats

@test "idle builder qualification refuses a serving host before running its test binary" {
  local sandbox
  sandbox=$(mktemp -d)
  mkdir -p "$sandbox/etc"
  printf '#!/bin/sh\ntouch "%s/ran"\n' "$sandbox" > "$sandbox/contract"
  chmod +x "$sandbox/contract"
  run env VERCELIFY_ETC="$sandbox/etc" VERCELIFY_TESTS_ON_SERVER= BUILDER_CONTRACT_BIN="$sandbox/contract" \
    bash "$BATS_TEST_DIRNAME/contract/builder-idle.sh"
  [ "$status" -ne 0 ]
  [[ "$output" == *"take its sites offline"* ]]
  [ ! -e "$sandbox/ran" ]
  rm -rf "$sandbox"
}

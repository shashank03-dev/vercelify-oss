#!/usr/bin/env bats

@test "off-host qualification refuses a serving host before touching either Docker daemon" {
  local fixture
  fixture=$(mktemp -d)
  mkdir "$fixture/etc"
  run env VERCELIFY_ETC="$fixture/etc" VERCELIFY_TESTS_ON_SERVER= \
    OFFHOST_TARGET_EXEC='["must-not-run"]' bash "$BATS_TEST_DIRNAME/contract/offhost-builds.sh"
  rm -rf "$fixture"
  [ "$status" -eq 1 ]
  [[ "$output" == *"would take its sites offline"* ]]
}

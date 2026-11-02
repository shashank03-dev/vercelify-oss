#!/usr/bin/env bats

@test "preview qualification refuses a serving host before executing the contract" {
  local fixture
  fixture=$(mktemp -d)
  mkdir "$fixture/etc"
  run env VERCELIFY_ETC="$fixture/etc" VERCELIFY_TESTS_ON_SERVER= \
    PREVIEW_CONTRACT_BIN=/must-not-run \
    bash "$BATS_TEST_DIRNAME/contract/preview-safety.sh"
  rm -rf "$fixture"
  [ "$status" -eq 1 ]
  [[ "$output" == *"would take its sites offline"* ]]
}

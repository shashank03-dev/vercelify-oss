#!/usr/bin/env bats
# Host helpers for the platform engine.

setup() {
  T=$(mktemp -d)
  export VERCELIFY_ETC="$T/etc" NO_COLOR=1
  mkdir -p "$VERCELIFY_ETC"
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/common.sh"
  # shellcheck source=/dev/null
  . "$BATS_TEST_DIRNAME/../lib/platform.sh"
}

teardown() { rm -rf "$T"; }

@test "the engine is read from agent.toml" {
  ! platform_engine
  printf 'port = 7420\n# engine = "platform"\n' >"$VERCELIFY_ETC/agent.toml"
  ! platform_engine
  printf 'port = 7420\nengine = "platform"\n' >"$VERCELIFY_ETC/agent.toml"
  platform_engine
  printf '  engine="platform"\n' >"$VERCELIFY_ETC/agent.toml"
  platform_engine
  printf 'engine = "coolify"\n' >"$VERCELIFY_ETC/agent.toml"
  ! platform_engine
}

@test "domain settings for each mode" {
  run platform_domain_json "https://example.com" false
  [ "$output" = '{"wildcard":"example.com","https":true,"edge_tls":false}' ]
  run platform_domain_json "http://example.com" true
  [ "$output" = '{"wildcard":"example.com","https":false,"edge_tls":true}' ]
  run platform_domain_json "http://10.77.0.1.sslip.io" false
  [ "$output" = '{"wildcard":"10.77.0.1.sslip.io","https":false,"edge_tls":false}' ]
  run platform_domain_json "" false
  [ "$output" = '{"wildcard":null,"https":false,"edge_tls":false}' ]
}

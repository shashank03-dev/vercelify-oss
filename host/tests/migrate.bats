#!/usr/bin/env bats
# `vercelify migrate`: the steps around the agent's (a fake agent binary
# here, systemctl stubbed). The real migration, against a real Coolify, is
# host/tests/contract/migrate.sh.

setup() {
  T=$(mktemp -d)
  python3 "$BATS_TEST_DIRNAME/mock_agent.py" "$T/port" "$T/requests.log" &
  MOCK_PID=$!
  for _ in $(seq 50); do [ -s "$T/port" ] && break; sleep 0.1; done
  mkdir -p "$T/etc" "$T/bin" "$T/mesh" "$T/migrate"
  echo test-agent-token >"$T/etc/agent.token"
  printf 'role=main\nversion=test\n' >"$T/etc/install.env"
  printf 'port = 7420\nengine = "coolify"\n' >"$T/etc/agent.toml"
  echo '{}' >"$T/mesh/state.json"
  echo coolify-token >"$T/etc/coolify.token"
  # The agent's steps: they log, and move the phase as the real ones do.
  cat >"$T/bin/vercelify-agent" <<'SH'
#!/usr/bin/env bash
shift 2  # --config FILE
echo "agent $*" >>"$T/calls"
state="$MIGRATE_STATE"
phase() { printf '{"phase": "%s"}\n' "$1" >"$state"; }
case "$1 ${2:-}" in
  "migrate plan") echo "  + app web — image traefik/whoami:latest" ;;
  "migrate apply"*) [[ -n "${FAIL_APPLY:-}" ]] && { echo "boom" >&2; exit 1; }; phase imported ;;
  "migrate check"*) [[ -n "${FAIL_CHECK:-}" ]] && { echo "  ✗ web — failed"; exit 1; }; echo "  ✓ web — healthy"; phase switched ;;
  "migrate rollback") phase rolled_back ;;
  "migrate finish"*) phase finished ;;
  "migrate status") cat "$state" ;;
esac
SH
  printf '#!/bin/sh\necho "systemctl $*" >>"%s/calls"\n' "$T" >"$T/bin/systemctl"
  printf '#!/bin/sh\necho "tailscale $*" >>"%s/calls"\n' "$T" >"$T/bin/tailscale"
  chmod +x "$T/bin/"*
  export NO_COLOR=1 VERCELIFY_ETC="$T/etc" VERCELIFY_LOG="$T/log" VERCELIFY_TEST=1 T
  export AGENT_LOCAL_URL="http://127.0.0.1:$(cat "$T/port")" AGENT_BIN="$T/bin/vercelify-agent"
  export MESH_DIR="$T/mesh" MIGRATE_STATE="$T/migrate/state.json" PATH="$T/bin:$PATH" ASSUME_YES=1
  CLI="$BATS_TEST_DIRNAME/../vercelify"
}

teardown() { kill "$MOCK_PID" 2>/dev/null; rm -rf "$T"; }

@test "migrate alone shows the plan and changes nothing" {
  run "$CLI" migrate
  [ "$status" -eq 0 ]
  [[ "$output" == *"+ app web"* && "$output" == *"Nothing changed"* ]]
  ! grep -q systemctl "$T/calls"
  grep -qx 'engine = "coolify"' "$T/etc/agent.toml"
}

@test "migrate --apply: takes over, restarts on the platform engine, checks" {
  run "$CLI" migrate --apply
  [ "$status" -eq 0 ]
  [[ "$output" == *"every app runs on Vercelify's platform engine"* ]]
  [[ "$output" == *"sudo vercelify migrate --finish"* ]]
  grep -qx 'engine = "platform"' "$T/etc/agent.toml"
  [ "$(grep -n 'agent migrate apply' "$T/calls" | cut -d: -f1)" -lt "$(grep -n 'systemctl restart vercelify-agent' "$T/calls" | cut -d: -f1)" ]
  grep -q 'agent migrate check --wait 600' "$T/calls"
  # Again: done already.
  run "$CLI" migrate --apply
  [[ "$output" == *"migrated already"* ]]
  [ "$(grep -c 'agent migrate apply' "$T/calls")" -eq 1 ]
}

@test "a failed apply goes back to Coolify and keeps the engine" {
  run env FAIL_APPLY=1 "$CLI" migrate --apply --force
  [ "$status" -ne 0 ]
  [[ "$output" == *"nothing was migrated"* ]]
  grep -q 'agent migrate apply --force' "$T/calls"
  grep -q 'agent migrate rollback' "$T/calls"
  grep -qx 'engine = "coolify"' "$T/etc/agent.toml"
}

@test "a failed check says how to go on or back" {
  run env FAIL_CHECK=1 "$CLI" migrate --apply
  [ "$status" -ne 0 ]
  [[ "$output" == *"sudo vercelify migrate --rollback"* ]]
  # Resumed: no second apply, the check again.
  run "$CLI" migrate --apply
  [ "$status" -eq 0 ]
  [ "$(grep -c 'agent migrate apply' "$T/calls")" -eq 1 ]
}

@test "rollback stops the agent first and goes back to the Coolify engine" {
  run "$CLI" migrate --rollback
  [ "$status" -ne 0 ]
  [[ "$output" == *"no migration on this server"* ]]
  "$CLI" migrate --apply >/dev/null 2>&1
  run "$CLI" migrate --rollback
  [ "$status" -eq 0 ]
  [ "$(grep -n 'systemctl stop vercelify-agent' "$T/calls" | cut -d: -f1)" -lt "$(grep -n 'agent migrate rollback' "$T/calls" | cut -d: -f1)" ]
  grep -qx 'engine = "coolify"' "$T/etc/agent.toml"
}

@test "finish only after the check, and it forgets Coolify's token" {
  printf '{"phase": "imported"}\n' >"$MIGRATE_STATE"
  run "$CLI" migrate --finish
  [ "$status" -ne 0 ]
  [[ "$output" == *"sudo vercelify migrate --check"* ]]
  printf '{"phase": "switched"}\n' >"$MIGRATE_STATE"
  run "$CLI" migrate --finish --purge
  [ "$status" -eq 0 ]
  grep -q 'agent migrate finish --purge' "$T/calls"
  [ ! -e "$T/etc/coolify.token" ]
}

@test "tailscale off only when every node used the mesh in the last day" {
  run "$CLI" migrate --tailscale-off
  [ "$status" -ne 0 ]
  [[ "$output" == *"migrate to the platform engine first"* ]]
  printf 'engine = "platform"\n' >"$T/etc/agent.toml"
  # cabin was last seen 2026-10-25 11:00 UTC (pi5 is revoked: ignored).
  run env MIGRATE_NOW="$(date -d 2026-10-27T00:00:00Z +%s)" "$CLI" migrate --tailscale-off
  [ "$status" -ne 0 ]
  [[ "$output" == *"not seen on the mesh in the last 24 hours: cabin"* ]]
  ! grep -q "tailscale down" "$T/calls"
  run env MIGRATE_NOW="$(date -d 2026-10-25T12:00:00Z +%s)" "$CLI" migrate --tailscale-off
  [ "$status" -eq 0 ]
  grep -q "tailscale down" "$T/calls"
  grep -q "systemctl disable --now tailscaled" "$T/calls"
}

@test "migrate refuses on a worker and explains its flags" {
  printf 'role=worker\n' >"$T/etc/install.env"
  run "$CLI" migrate
  [ "$status" -ne 0 ]
  [[ "$output" == *"migrate runs on the main server"* ]]
  run "$CLI" migrate --sideways
  [[ "$output" == *"usage: vercelify migrate"* ]]
}

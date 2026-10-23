#!/usr/bin/env bats
# `vercelify update` keeps the previous version and rolls back by itself.

setup() {
  T=$(mktemp -d)
  mkdir -p "$T/etc" "$T/lib/host" "$T/systemd" "$T/bin" "$T/www/v1"
  printf 'role=main\nversion=0.1.0\nchannel=stable\n' >"$T/etc/install.env"
  printf '#!/bin/sh\necho old-agent\n' >"$T/agent"; chmod +x "$T/agent"
  echo 'old cli' >"$T/lib/host/marker"
  echo '[Service]' >"$T/systemd/vercelify-agent.service"
  printf '#!/usr/bin/env bash\necho "systemctl $*" >>"%s/calls"\n' "$T" >"$T/bin/systemctl"
  chmod +x "$T/bin/systemctl"
  echo '{"ok":true}' >"$T/www/v1/health"
  export PATH="$T/bin:$PATH" NO_COLOR=1 VERCELIFY_TEST=1 VERCELIFY_ETC="$T/etc" VERCELIFY_LIB="$T/lib"
  export AGENT_BIN="$T/agent" SYSTEMD_DIR="$T/systemd" VERCELIFY_LOG="$T/log" UPDATE_HEALTH_SECS=1
  # A fake installer: replaces the agent and the CLI, then exits with $FAKE_EXIT.
  cat >"$T/installer.sh" <<'INST'
printf '#!/bin/sh\necho new-agent\n' >"$AGENT_BIN"
echo 'new cli' >"$VERCELIFY_LIB/host/marker"
echo "channel=$VERCELIFY_CHANNEL ref=$VERCELIFY_REF args=$*" >"$VERCELIFY_LIB/installer-args"
exit "${FAKE_EXIT:-0}"
INST
  export VERCELIFY_INSTALLER="$T/installer.sh"
  CLI="$BATS_TEST_DIRNAME/../vercelify"
}

teardown() { [ -n "${WEB_PID:-}" ] && kill "$WEB_PID" 2>/dev/null; rm -rf "$T"; }

serve_health() {
  local port
  port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
  python3 -m http.server "$port" --bind 127.0.0.1 --directory "$T/www" >/dev/null 2>&1 &
  WEB_PID=$!
  export AGENT_LOCAL_URL="http://127.0.0.1:$port"
  for _ in $(seq 50); do curl -fs "$AGENT_LOCAL_URL/v1/health" >/dev/null && return; sleep 0.1; done
}

@test "a failed installer run is rolled back to the previous agent and scripts" {
  serve_health
  FAKE_EXIT=3 run "$CLI" update
  [ "$status" -ne 0 ]
  [[ "$output" == *"restoring the previous agent"* ]]
  [[ "$output" == *"rolled back"* ]]
  [ "$("$T/agent")" = old-agent ]
  [ "$(cat "$T/lib/host/marker")" = "old cli" ]
  grep -q 'systemctl try-restart vercelify-agent' "$T/calls"
}

@test "an agent that doesn't come up after the update is rolled back too" {
  export AGENT_LOCAL_URL=http://127.0.0.1:9
  run "$CLI" update
  [ "$status" -ne 0 ]
  [ "$("$T/agent")" = old-agent ]
}

@test "a good update keeps the new version; rollback goes back on request" {
  serve_health
  run "$CLI" update --edge
  [ "$status" -eq 0 ]
  [[ "$output" == *"updated (channel edge)"* ]]
  grep -q 'channel=edge ref= args=-y' "$T/lib/installer-args"
  [ "$("$T/agent")" = new-agent ]
  run "$CLI" rollback
  [ "$status" -eq 0 ]
  [ "$("$T/agent")" = old-agent ]
  run "$CLI" update --ref 'v0.2.0'
  grep -q 'channel=stable ref=v0.2.0' "$T/lib/installer-args"
}

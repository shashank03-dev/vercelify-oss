#!/usr/bin/env bats
# `vercelify update` keeps the previous version and rolls back by itself.

setup() {
  T=$(mktemp -d)
  mkdir -p "$T/etc" "$T/lib/host" "$T/systemd" "$T/bin" "$T/www/v1"
  printf 'role=main\nversion=0.1.0\nchannel=stable\n' >"$T/etc/install.env"
  echo 'listen = ["127.0.0.1"]' >"$T/etc/agent.toml"
  # The old agent: `db-backup` copies the live database (unless NO_DB_BACKUP:
  # an agent from before that command).
  cat >"$T/agent" <<'AGENT'
#!/bin/sh
if [ "$3" = db-backup ]; then
  [ -n "${NO_DB_BACKUP:-}" ] && exit 2
  exec python3 -c 'import sqlite3,sys; sqlite3.connect(sys.argv[1]).backup(sqlite3.connect(sys.argv[2]))' "$PLATFORM_DB" "$4"
fi
echo old-agent
AGENT
  chmod +x "$T/agent"
  # The engine's state at schema 3, one row in it.
  python3 -c 'import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute("create table apps(name)"); c.execute("insert into apps values (\"web\")"); c.execute("pragma user_version=3"); c.commit()' "$T/lib/platform.db"
  echo 'old cli' >"$T/lib/host/marker"
  echo '[Service]' >"$T/systemd/vercelify-agent.service"
  printf '#!/usr/bin/env bash\necho "systemctl $*" >>"%s/calls"\n' "$T" >"$T/bin/systemctl"
  chmod +x "$T/bin/systemctl"
  echo '{"ok":true,"capabilities":["doctor","platform"]}' >"$T/www/v1/health"
  export PATH="$T/bin:$PATH" NO_COLOR=1 VERCELIFY_TEST=1 VERCELIFY_ETC="$T/etc" VERCELIFY_LIB="$T/lib"
  export AGENT_BIN="$T/agent" SYSTEMD_DIR="$T/systemd" VERCELIFY_LOG="$T/log" UPDATE_HEALTH_SECS=1
  export PLATFORM_DB="$T/lib/platform.db" UPDATE_ENGINE_SECS=1
  # A fake installer: replaces the agent and the CLI, then exits with $FAKE_EXIT.
  cat >"$T/installer.sh" <<'INST'
printf '#!/bin/sh\necho new-agent\n' >"$AGENT_BIN"
echo 'new cli' >"$VERCELIFY_LIB/host/marker"
echo 'listen = ["127.0.0.1", "mesh"]' >"$VERCELIFY_ETC/agent.toml"
# The new agent migrates the database on.
python3 -c 'import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute("pragma user_version=5"); c.commit()' "$PLATFORM_DB"
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
  # The old agent gets the configuration it can read back.
  [ "$(cat "$T/etc/agent.toml")" = 'listen = ["127.0.0.1"]' ]
  # Started again even though it was stopped for the rollback (try-restart
  # would leave it down).
  grep -q 'systemctl restart vercelify-agent' "$T/calls"
  ! grep -q 'systemctl try-restart vercelify-agent' "$T/calls"
}

@test "rollback puts back the database the old agent can open, taken fresh at update time" {
  serve_health
  # A row written after the engine's 5-minute copy: the snapshot must have it.
  echo stale >"$T/lib/platform.db.bak"
  python3 -c 'import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute("insert into apps values (\"api\")"); c.commit()' "$PLATFORM_DB"
  FAKE_EXIT=1 run "$CLI" update
  [ "$status" -ne 0 ]
  [ "$(python3 -c 'import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute("pragma user_version").fetchone()[0])' "$PLATFORM_DB")" = 3 ]
  [ "$(python3 -c 'import sqlite3,sys; print(",".join(r[0] for r in sqlite3.connect(sys.argv[1]).execute("select name from apps order by name")))' "$PLATFORM_DB")" = api,web ]
  # The migrated one is kept, not deleted.
  ls "$T/lib/" | grep -q 'platform.db.replaced-'
}

@test "an agent without db-backup is stopped for the copy and started again" {
  serve_health
  NO_DB_BACKUP=1 FAKE_EXIT=1 run "$CLI" update
  [ "$status" -ne 0 ]
  [ -s "$T/lib/rollback/platform.db" ]
  grep -q 'systemctl stop vercelify-agent' "$T/calls"
  grep -q 'systemctl start vercelify-agent' "$T/calls"
}

@test "an update whose engine doesn't open is rolled back, and the way out is named" {
  serve_health
  echo '{"ok":true,"capabilities":["doctor"]}' >"$T/www/v1/health"
  run "$CLI" update
  [ "$status" -ne 0 ]
  [ "$("$T/agent")" = old-agent ]
  [[ "$output" == *"vercelify restore-state"* ]]
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

@test "a local installer runs in place, so it installs the checkout it lives in" {
  serve_health
  echo 'echo "from=${BASH_SOURCE[0]}" >"$VERCELIFY_LIB/installer-path"' >>"$T/installer.sh"
  sed -i.orig '/^exit /d' "$T/installer.sh"
  run "$CLI" update
  [ "$status" -eq 0 ]
  grep -qx "from=$T/installer.sh" "$T/lib/installer-path"
}

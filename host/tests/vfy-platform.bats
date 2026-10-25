#!/usr/bin/env bats
# `vfy` against a server on Vercelify's own engine (paired without Coolify):
# apps, deploys with a streamed build log, logs, rollbacks, variables,
# restart, the service catalogue and `why`.

setup() {
  T=$(mktemp -d)
  python3 "$BATS_TEST_DIRNAME/mock_platform.py" "$T/port" "$T/requests.log" &
  MOCK_PID=$!
  for _ in $(seq 50); do [ -s "$T/port" ] && break; sleep 0.1; done
  URL="http://127.0.0.1:$(cat "$T/port")"
  cat >"$T/settings.json" <<JSON
{"servers": [{"agent_url": "$URL", "agent_token": "agent-secret", "coolify_url": "", "coolify_token": null, "hostname": "optiplex", "role": "main"}],
 "active": "$URL"}
JSON
  export VFY_SETTINGS="$T/settings.json" NO_COLOR=1
  VFY="$BATS_TEST_DIRNAME/../../cli/vfy"
  cd "$T"
}

teardown() { kill "$MOCK_PID" 2>/dev/null; rm -rf "$T"; }

@test "apps: state from the engine, no Coolify involved" {
  run "$VFY" apps
  [ "$status" -eq 0 ]
  [[ "$output" == *"✓ blog"*"live"*"https://blog.example.com"* ]]
  [[ "$output" == *"✗ api"*"failed"* ]]
  ! grep -q '/api/v1/' "$T/requests.log"
  grep -q '^GET /v1/platform/apps auth=Bearer agent-secret' "$T/requests.log"
}

@test "deploy streams the build log and reports the result" {
  run "$VFY" deploy blog
  [ "$status" -eq 0 ]
  [[ "$output" == *"Deploying blog (#5)"* ]]
  [[ "$output" == *"Detected Next.js 15"* ]]
  [[ "$output" == *"✓ blog is live at https://blog.example.com"* ]]
  grep -q '^GET /v1/platform/deployments/dpl_5/logs?follow=true ' "$T/requests.log"
  run "$VFY" deploy api
  [ "$status" -ne 0 ]
  [[ "$output" == *"[stderr] Module not found"* ]]
  [[ "$output" == *"the deployment failed: build failed: exit status 1 (vfy why api)"* ]]
  run "$VFY" deploy blog --no-follow
  [ "$status" -eq 0 ]
  [[ "$output" != *"Detected"* ]]
  run "$VFY" deploy nope
  [ "$status" -ne 0 ]
  [[ "$output" == *"app nope not found"* ]]
}

@test "logs, follow and restart" {
  run "$VFY" logs blog 50
  [ "$status" -eq 0 ]
  [[ "${lines[0]}" == "ready on :3000" ]]
  [[ "${lines[1]}" == "[stderr] warn: slow query" ]]
  grep -q '^GET /v1/platform/apps/blog/logs?tail=50 ' "$T/requests.log"
  run "$VFY" logs blog -f
  [ "$status" -eq 0 ]
  [[ "$output" == *"GET / 200"* ]]
  run "$VFY" logs blog many
  [ "$status" -ne 0 ]
  run "$VFY" restart blog
  [ "$status" -eq 0 ]
  [[ "$output" == *"restarted blog (2 container(s))"* ]]
}

@test "rollback by commit or deployment number" {
  run "$VFY" rollback blog 1234
  [ "$status" -eq 0 ]
  [[ "$output" == *"rolling blog back to #1"* ]]
  grep -q 'POST /v1/platform/apps/blog/rollback .*"deployment_id":"dpl_1"' "$T/requests.log"
  run "$VFY" rollback blog '#1'
  [ "$status" -eq 0 ]
  run "$VFY" rollback blog 9f8e7d6
  [ "$status" -ne 0 ]
  [[ "$output" == *"already live"* ]]
  # Failed builds and previews are not rollback targets.
  run "$VFY" rollback blog abcdef1
  [ "$status" -ne 0 ]
  [[ "$output" == *"no earlier live deployment"* ]]
  run "$VFY" rollback blog 4b1d2e7
  [ "$status" -ne 0 ]
  run "$VFY" rollback blog 'x; rm'
  [ "$status" -ne 0 ]
}

@test "env pull, set and push" {
  run "$VFY" env pull blog
  [ "$status" -eq 0 ]
  [ "$(stat -c %a .env 2>/dev/null || stat -f %Lp .env)" = 600 ]
  grep -qx 'DATABASE_URL="postgres://u:p@db/x"' .env
  ! grep -q PREVIEW_ONLY .env
  grep -q '^GET /v1/platform/apps/blog/env?reveal=true ' "$T/requests.log"
  run "$VFY" env pull blog
  [ "$status" -ne 0 ]
  run "$VFY" env set blog API_URL=https://x.example.com 'GREETING=hello world' --plain
  [ "$status" -eq 0 ]
  [[ "$output" == *"2 variables saved"* ]]
  req=$(grep '^PUT /v1/platform/apps/blog/env ' "$T/requests.log" | sed 's/^[^{]*//')
  [ "$(jq -r '.vars[1].value' <<<"$req")" = "hello world" ]
  [ "$(jq -r '.vars[0].secret' <<<"$req")" = false ]
  run "$VFY" env set blog nothing
  [ "$status" -ne 0 ]
  printf 'A=1\nB=2\n# c\n' >prod.env
  run "$VFY" env push blog prod.env
  [ "$status" -eq 0 ]
  [[ "$output" == *"2 variables imported from prod.env"* ]]
}

@test "the service catalogue" {
  run "$VFY" services
  [ "$status" -eq 0 ]
  [[ "$output" == *"umami"*"Umami"*"512 MB"* ]]
  [[ "$output" == *"plausible"*"2 GB"* ]]
  run "$VFY" services add umami stats
  [ "$status" -eq 0 ]
  [[ "$output" == *"stats is being created from umami"* ]]
  grep -q 'POST /v1/platform/templates/umami .*"name":"stats"' "$T/requests.log"
  run "$VFY" services add nope
  [ "$status" -ne 0 ]
}

@test "why APP reads the failed build from the engine" {
  run "$VFY" why api
  [ "$status" -eq 0 ]
  [[ "$output" == *"api failed at abcdef1"* ]]
  [[ "$output" == *"**Cause:** the import uses"* ]]
  req=$(grep '^POST /v1/llm/chat ' "$T/requests.log" | sed 's/^[^{]*//')
  user=$(jq -r '.messages[1].content' <<<"$req")
  [[ "$user" == *'Commit: abcdef1 "Add notes"'* ]]
  [[ "$user" == *"[stderr] Module not found"* ]]
  run "$VFY" why blog
  [ "$status" -eq 0 ]
}

@test "a server on the mesh is reached through vercelify-mesh's SOCKS proxy" {
  python3 "$BATS_TEST_DIRNAME/mock_socks.py" "$T/socks.port" "$T/socks.log" "127.0.0.1:$(cat "$T/port")" &
  SOCKS_PID=$!
  for _ in $(seq 50); do [ -s "$T/socks.port" ] && break; sleep 0.1; done
  cat >"$T/mesh.json" <<JSON
{"servers": [{"agent_url": "http://10.77.0.1:7420", "agent_token": "agent-secret", "coolify_url": "", "coolify_token": null, "hostname": "optiplex", "role": "main"}],
 "active": "http://10.77.0.1:7420"}
JSON
  # App mode (the daemon's status says userspace) → through the proxy.
  printf '{"interface": "userspace"}' >"$T/status.json"
  VFY_SETTINGS="$T/mesh.json" VFY_MESH_STATUS="$T/status.json" VFY_MESH_SOCKS="127.0.0.1:$(cat "$T/socks.port")" run "$VFY" apps
  kill "$SOCKS_PID" 2>/dev/null
  [ "$status" -eq 0 ]
  [[ "$output" == *"✓ blog"* ]]
  grep -q '^CONNECT 10.77.0.1:7420$' "$T/socks.log"
  # A server not on the mesh never goes through it.
  : >"$T/socks.log"
  VFY_MESH_SOCKS="127.0.0.1:1" run "$VFY" apps
  [ "$status" -eq 0 ]
  [ ! -s "$T/socks.log" ]
}

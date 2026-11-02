#!/usr/bin/env bats
bats_require_minimum_version 1.5.0
# `vfy` against a server on Vercelify's own engine:
# apps, deploys with a streamed build log, logs, rollbacks, variables,
# restart, the service catalogue and `why`.

setup() {
  T=$(mktemp -d)
  python3 "$BATS_TEST_DIRNAME/mock_platform.py" "$T/port" "$T/requests.log" &
  MOCK_PID=$!
  for _ in $(seq 50); do [ -s "$T/port" ] && break; sleep 0.1; done
  URL="http://127.0.0.1:$(cat "$T/port")"
  cat >"$T/settings.json" <<JSON
{"servers": [{"agent_url": "$URL", "agent_token": "agent-secret", "hostname": "optiplex", "role": "main"}],
 "active": "$URL"}
JSON
  export VFY_SETTINGS="$T/settings.json" NO_COLOR=1
  VFY="$BATS_TEST_DIRNAME/../../cli/vfy"
  cd "$T"
}

teardown() { kill "$MOCK_PID" 2>/dev/null; rm -rf "$T"; }

@test "apps: state from the engine" {
  run "$VFY" apps
  [ "$status" -eq 0 ]
  [[ "$output" == *"✓ blog"*"live"*"https://blog.example.com"* ]]
  [[ "$output" == *"✗ api"*"failed"* ]]
  run ! grep -q '/api/v1/' "$T/requests.log"
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

@test "deploy follows the log again when the stream drops mid-build" {
  touch "$T/flaky"
  run "$VFY" deploy blog
  [ "$status" -eq 0 ]
  [[ "$output" == *"dropped; following it again"* ]]
  [[ "$output" == *"✓ blog is live"* ]]
  # Each line once, though the log was read twice.
  [ "$(grep -c 'Detected Next.js 15' <<<"$output")" -eq 1 ]
  [[ "$output" == *"Deployment is live"* ]]
  [ "$(grep -c 'deployments/dpl_5/logs' "$T/requests.log")" -eq 2 ]
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
  grep -qx 'API_URL="https://api.example.com"' .env
  grep -qx 'REGION="home"' .env
  [[ "$output" == *"3 variables written"* ]]
  run ! grep -q PREVIEW_ONLY .env
  grep -q '^GET /v1/platform/apps/blog/env?reveal=true&resolved=true ' "$T/requests.log"
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
{"servers": [{"agent_url": "http://10.77.0.1:7420", "agent_token": "agent-secret", "hostname": "optiplex", "role": "main"}],
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

@test "forced env pull makes an existing public file private" {
  echo old > .env
  chmod 0644 .env
  VFY_FORCE=1 run "$VFY" env pull blog .env
  [ "$status" -eq 0 ]
  [ "$(stat -c %a .env 2>/dev/null || stat -f %Lp .env)" = 600 ]
}

@test "forced env export refuses symbolic links and preserves their targets" {
  echo original > target.env
  ln -s target.env .env
  VFY_FORCE=1 run "$VFY" env pull blog .env
  [ "$status" -ne 0 ]
  [ "$(cat target.env)" = original ]
}

@test "a rejected env import reports failure without claiming success" {
  printf 'INVALID\n' >bad.env
  run "$VFY" env push blog bad.env
  [ "$status" -ne 0 ]
  [[ "$output" == *"expected KEY=value"* ]]
  [[ "$output" != *"imported from"* ]]
}

@test "logs of an app that never ran show its latest build failure" {
  run "$VFY" logs api
  [ "$status" -eq 0 ]
  [[ "$output" == *"Module not found"* ]]
}

@test "build-here refuses an unavailable local runtime before enqueueing a server build" {
  mkdir -p "$T/local-bin"
  printf '#!/bin/sh\nexit 1\n' > "$T/local-bin/docker"
  chmod +x "$T/local-bin/docker"
  run env PATH="$T/local-bin:$PATH" "$VFY" deploy blog --build-here
  [ "$status" -ne 0 ]
  [[ "$output" == *"local Docker runtime"* ]]
  ! grep -q 'POST /v1/platform/apps/.*/deploy ' "$T/requests.log"
}

@test "build-here requires Buildx before creating a handoff job" {
  mkdir -p "$T/local-bin"
  printf '#!/bin/sh\n[ "$1" = info ]\n' > "$T/local-bin/docker"
  chmod +x "$T/local-bin/docker"
  run env PATH="$T/local-bin:$PATH" VFY_PACK_BIN=/bin/true "$VFY" deploy blog --build-here
  [ "$status" -ne 0 ]
  [[ "$output" == *"Buildx plugin"* ]]
  ! grep -q 'POST /v1/platform/apps/.*/build-jobs ' "$T/requests.log"
}

@test "a failed exact-commit fetch stops before planning or creating a build job" {
  mkdir -p "$T/local-bin"
  cat > "$T/local-bin/docker" <<'SH'
#!/bin/sh
exit 0
SH
  cat > "$T/local-bin/git" <<SH
#!/bin/sh
for arg; do
  if [ "\$arg" = fetch ]; then echo fetch-failed >&2; exit 42; fi
  if [ "\$arg" = checkout ]; then touch '$T/checkout-ran'; fi
done
exit 0
SH
  cat > "$T/local-bin/pack" <<SH
#!/bin/sh
touch '$T/pack-ran'
SH
  chmod +x "$T/local-bin/"*
  run env PATH="$T/local-bin:$PATH" VFY_PACK_BIN="$T/local-bin/pack" "$VFY" deploy blog --build-here
  [ "$status" -ne 0 ]
  [[ "$output" == *"fetch-failed"* ]]
  [ ! -e "$T/checkout-ran" ]
  [ ! -e "$T/pack-ran" ]
  ! grep -q 'POST /v1/platform/apps/.*/build-jobs ' "$T/requests.log"
}

@test "the build context excludes checkout credentials and Git metadata" {
  mkdir -p "$T/local-bin"
  printf '#!/bin/sh\nexit 0\n' > "$T/local-bin/docker"
  cat > "$T/local-bin/git" <<'SH'
#!/bin/sh
for last; do :; done
for arg; do
  if [ "$arg" = init ]; then mkdir -p "$last/.git"; fi
  if [ "$arg" = rev-parse ]; then printf '%040d\n' 1; fi
done
exit 0
SH
  cat > "$T/local-bin/pack" <<SH
#!/bin/sh
[ ! -e "\$2/.git" ] || exit 99
touch '$T/no-git-metadata'
exit 55
SH
  chmod +x "$T/local-bin/"*
  run env PATH="$T/local-bin:$PATH" VFY_PACK_BIN="$T/local-bin/pack" "$VFY" deploy blog --build-here
  [ "$status" -ne 0 ]
  [ -e "$T/no-git-metadata" ]
}

@test "a retained builder cannot silently use a different Docker runtime" {
  mkdir -p "$T/local-bin"
  cat > "$T/local-bin/docker" <<'SH'
#!/bin/sh
case "$1 $2" in
  'context show') echo selected-context ;;
  'buildx inspect') echo 'Endpoint: previous-context' ;;
esac
exit 0
SH
  cat > "$T/local-bin/git" <<'SH'
#!/bin/sh
for arg; do
  if [ "$arg" = rev-parse ]; then printf '%040d\n' 1; fi
done
exit 0
SH
  cat > "$T/local-bin/pack" <<'SH'
#!/bin/sh
cat >/dev/null
printf '{"dockerfile":"FROM scratch","generated":false,"context":".","port":80,"framework":"fixture","notes":[],"project_files":[]}'
SH
  chmod +x "$T/local-bin/"*
  run env -u DOCKER_CONTEXT -u DOCKER_HOST PATH="$T/local-bin:$PATH" VFY_PACK_BIN="$T/local-bin/pack" "$VFY" deploy blog --build-here
  [ "$status" -ne 0 ]
  [[ "$output" == *"another Docker runtime"* ]]
  ! grep -q 'POST /v1/platform/apps/.*/build-jobs ' "$T/requests.log"
}

@test "a CI image without EXPOSE asks for an app port before creating a job" {
  mkdir -p "$T/local-bin"
  cat > "$T/local-bin/docker" <<'SH'
#!/bin/sh
if [ "$1 $2" = 'image inspect' ]; then echo null; fi
exit 0
SH
  chmod +x "$T/local-bin/docker"
  run env PATH="$T/local-bin:$PATH" "$VFY" deploy blog --image "registry.test/image@sha256:$(printf '%064d' 1)" --commit "$(printf '%040d' 1)"
  [ "$status" -ne 0 ]
  [[ "$output" == *"set the app port"* ]]
  ! grep -q 'POST /v1/platform/apps/.*/build-jobs ' "$T/requests.log"
}

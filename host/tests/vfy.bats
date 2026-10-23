#!/usr/bin/env bats
# `vfy`, the Mac CLI, against a mock agent + Coolify, using the app's settings file.

setup() {
  T=$(mktemp -d)
  python3 "$BATS_TEST_DIRNAME/mock_coolify.py" "$T/port" "$T/requests.log" &
  MOCK_PID=$!
  for _ in $(seq 50); do [ -s "$T/port" ] && break; sleep 0.1; done
  URL="http://127.0.0.1:$(cat "$T/port")"
  cat >"$T/settings.json" <<JSON
{"servers": [
  {"agent_url": "$URL", "agent_token": "agent-secret", "coolify_url": "$URL", "coolify_token": "coolify-secret", "hostname": "laptop", "role": "main"},
  {"agent_url": "http://127.0.0.1:9", "agent_token": "x", "coolify_url": "", "coolify_token": null, "hostname": "pi", "role": "worker"}
], "active": "$URL"}
JSON
  export VFY_SETTINGS="$T/settings.json" NO_COLOR=1
  VFY="$BATS_TEST_DIRNAME/../../cli/vfy"
  cd "$T"
}

teardown() { kill "$MOCK_PID" 2>/dev/null; rm -rf "$T"; }

@test "servers and apps" {
  run "$VFY" servers
  [ "$status" -eq 0 ]
  [[ "${lines[0]}" == "* laptop"* ]]
  [[ "${lines[1]}" == "  pi"*"worker"* ]]
  run "$VFY" apps
  [ "$status" -eq 0 ]
  [[ "$output" == *"blog"*"running"*"https://blog.example.com"* ]]
  [[ "$output" == *"api"*"exited"* ]]
}

@test "deploy and rollback by app name; tokens never on the command line" {
  run "$VFY" deploy blog
  [ "$status" -eq 0 ]
  [[ "$output" == *"Deployment request queued."* ]]
  grep -q 'POST /api/v1/deploy?uuid=a1&force=false auth=Bearer coolify-secret' "$T/requests.log"
  # Coolify needs the full SHA: a short one is looked up in the app's
  # finished production deployments (not previews, not failed ones).
  run "$VFY" rollback blog 4b1d
  [ "$status" -eq 0 ]
  grep -q 'POST /api/v1/applications/a1/rollback .*"commit":"4b1d2e7a9c0b1d2e3f4a5b6c7d8e9f0a1b2c3d4e"' "$T/requests.log"
  run "$VFY" rollback blog 9f8e7d6c5b4a39281706f5e4d3c2b1a098765432
  [ "$status" -ne 0 ]
  [[ "$output" == *"already live"* ]]
  run "$VFY" rollback blog abcdef1
  [ "$status" -ne 0 ]
  [[ "$output" == *"no finished deployment of blog"* ]]
  run "$VFY" rollback blog 'x; rm'
  [ "$status" -ne 0 ]
  run "$VFY" deploy nope
  [[ "$output" == *"no app named nope"* ]]
}

@test "env pull writes production values, private, never overwrites" {
  run "$VFY" env pull blog
  [ "$status" -eq 0 ]
  [ "$(stat -c %a .env)" = 600 ]
  grep -qx 'DATABASE_URL="postgres://u:p@db/x"' .env
  grep -qx 'QUOTE="say \\"hi\\""' .env
  ! grep -q preview .env
  run "$VFY" env pull blog
  [ "$status" -ne 0 ]
  [[ "$output" == *"exists"* ]]
}

@test "status, backup and errors from the agent" {
  run "$VFY" status
  [ "$status" -eq 0 ]
  [[ "$output" == *"System disk"*"88% used"* ]]
  # without colour a warning still reads differently from ok
  [[ "$output" == *"! System disk"* ]]
  [[ "$output" == *"→ docker system prune -af"* ]]
  run "$VFY" backup
  [ "$status" -eq 0 ]
  run "$VFY" -s pi apps
  [ "$status" -ne 0 ]
  [[ "$output" == *"worker without Coolify"* ]]
  run "$VFY" -s nowhere apps
  [[ "$output" == *"no server named nowhere"* ]]
}

@test "sleep an app by name, or keep it awake" {
  run "$VFY" sleep blog 15
  [ "$status" -eq 0 ]
  [[ "$output" == *"blog sleeps after 15 idle minutes"* ]]
  grep -q 'PUT /v1/sleep/a1 auth=Bearer agent-secret {"enabled":true,"idle_minutes":15}' "$T/requests.log"
  run "$VFY" sleep blog off
  [ "$status" -eq 0 ]
  grep -q 'PUT /v1/sleep/a1 .*{"enabled":false}' "$T/requests.log"
  run "$VFY" sleep blog 2
  [ "$status" -ne 0 ]
  [[ "$output" == *"5 minutes to 24 hours"* ]]
  run "$VFY" sleep blog soon
  [ "$status" -ne 0 ]
}

@test "config check reads vercel.json and vercelify.json from a folder" {
  mkdir site
  echo '{"redirects":[{"source":"/a","destination":"/b"},{"source":"/c","destination":"/d"}],"cleanUrls":true,"functions":{}}' >site/vercel.json
  echo '{"crons":[{"path":"/api/cron","schedule":"0 5 * * *"}]}' >site/vercelify.json
  run "$VFY" config check site
  [ "$status" -eq 0 ]
  [[ "${lines[0]}" == "✓ 2 redirects · 1 cron job · clean URLs" ]]
  [[ "${lines[1]}" == *"! \`functions\` is not used here"* ]]
  # Both files sent, vercel.json first (vercelify.json wins on conflicts).
  grep -q 'POST /v1/projects/check auth=Bearer agent-secret {"files":\[{"name":"vercel.json"' "$T/requests.log"
  grep -q '"name":"vercelify.json"' "$T/requests.log"
  echo '{' >site/vercel.json
  run "$VFY" config check site
  [ "$status" -ne 0 ]
  [[ "$output" == *"vercel.json"*"(HTTP 400)"* ]]
  run "$VFY" config check empty-dir
  [ "$status" -ne 0 ]
  [[ "$output" == *"no vercel.json or vercelify.json in empty-dir"* ]]
}

@test "config push, sync, show and rm by app name" {
  echo '{"rewrites":[{"source":"/(.*)","destination":"/index.html"}]}' >vercel.json
  run "$VFY" config push blog
  [ "$status" -eq 0 ]
  [[ "${lines[0]}" == "✓ 1 rewrite (uploaded)" ]]
  grep -q 'PUT /v1/projects/a1 ' "$T/requests.log"
  run "$VFY" config sync blog
  [ "$status" -eq 0 ]
  [[ "${lines[0]}" == "✓ 2 redirects · 1 rewrite · 1 cron job · clean URLs (from the repository at 9f8e7d6)" ]]
  run "$VFY" config show blog
  [ "$status" -eq 0 ]
  [[ "$output" == *"functions"* ]]
  run "$VFY" config show api
  [ "$status" -ne 0 ]
  [[ "$output" == *"api has no vercel.json applied"* ]]
  run "$VFY" config rm blog
  [ "$status" -eq 0 ]
  grep -q 'DELETE /v1/projects/a1 ' "$T/requests.log"
  run "$VFY" config nope
  [ "$status" -ne 0 ]
}

@test "crons: list with the last run, run one now" {
  run "$VFY" crons blog
  [ "$status" -eq 0 ]
  [[ "${lines[0]}" == "✗ /api/cron"*"0 5 * * *"*"500 at 2026-10-22T05:00:00Z"*"next 2026-10-23T05:00:00Z" ]]
  run "$VFY" crons blog run /api/cron
  [ "$status" -eq 0 ]
  grep -q 'POST /v1/projects/a1/crons/run auth=Bearer agent-secret {"path":"/api/cron"}' "$T/requests.log"
  run "$VFY" crons blog run /nope
  [ "$status" -ne 0 ]
  [[ "$output" == *"no cron job for that path"* ]]
}

@test "why APP: the server's Gemma 4 explains the last failed build" {
  run "$VFY" why blog
  [ "$status" -eq 0 ]
  [[ "$output" == *"blog failed at 4b1d000"* ]]
  [[ "$output" == *"**Cause:** the import uses \`Header\` but the file is \`header.tsx\`."* ]]
  [[ "$output" == *"**Check:** \`npm run build\`"* ]]
  # Gemma 4 E2B is preferred over the other installed models, and the answer is capped.
  [[ "$output" == *"— gemma4:e2b on laptop, via Ollama · 10 tokens/s"* ]]
  req=$(grep '^POST /v1/llm/chat auth=Bearer agent-secret ' "$T/requests.log" | sed 's/^[^{]*//')
  [ "$(jq -r .model <<<"$req")" = gemma4:e2b ]
  [ "$(jq -r .max_tokens <<<"$req")" = 400 ]
  [ "$(jq -r '.messages[0].role' <<<"$req")" = system ]
  user=$(jq -r '.messages[1].content' <<<"$req")
  [[ "$user" == *"App: blog"*'Commit: 4b1d000 "Add header"'* ]]
  [[ "$user" == *"[stderr] Module not found: Can't resolve '../components/Header'"* ]]
  [[ "$user" != *"internal helper noise"* ]]
  # A model that isn't there: the error and how to get it.
  VFY_AI_MODEL=missing:1b run "$VFY" why blog
  [ "$status" -ne 0 ]
  [[ "$output" == *"model 'missing:1b' not found"*"sudo vercelify llm pull missing:1b"* ]]
}

@test "why --log: any build log with the local Ollama, no paired server" {
  cp "$BATS_TEST_DIRNAME/../../cli/examples/failed-build.log" build.log
  VFY_SETTINGS=/nonexistent VFY_OLLAMA_URL="$URL" run "$VFY" why --log build.log
  [ "$status" -eq 0 ]
  [[ "$output" == *"**Fix:** rename it."* ]]
  [[ "$output" == *"— gemma4:e2b via Ollama at $URL"* ]]
  req=$(grep '^POST /api/chat ' "$T/requests.log" | sed 's/^[^{]*//')
  [ "$(jq -r .model <<<"$req")" = gemma4:e2b ]
  [ "$(jq -r .options.num_predict <<<"$req")" = 400 ]
  user=$(jq -r '.messages[1].content' <<<"$req")
  [[ "$user" == *"Module not found: Can't resolve '../components/Header'"* ]]
  [[ "$user" == *"Deployment failed."* ]]
  # From stdin, with a vercel.json alongside.
  echo '{"cleanUrls": true}' >vercel.json
  VFY_OLLAMA_URL="$URL" run bash -c "printf 'Error: boom\n' | '$VFY' why --log - --vercel-json vercel.json"
  [ "$status" -eq 0 ]
  grep '^POST /api/chat ' "$T/requests.log" | tail -1 | grep -q 'cleanUrls'
  # Ollama not running: say how to start it.
  VFY_OLLAMA_URL=http://127.0.0.1:9 run "$VFY" why --log build.log
  [ "$status" -ne 0 ]
  [[ "$output" == *"Ollama isn't running at http://127.0.0.1:9"*"ollama pull gemma4:e2b"* ]]
  printf '  \n' >empty.log
  run "$VFY" why --log empty.log
  [[ "$output" == *"the log is empty"* ]]
}

@test "why --provider gemini: Gemma 4 through the Gemini API, key via stdin" {
  cp "$BATS_TEST_DIRNAME/../../cli/examples/failed-build.log" build.log
  export VFY_GEMINI_URL="$URL/v1beta"
  GEMINI_API_KEY= run "$VFY" why --log build.log --provider gemini
  [ "$status" -ne 0 ]
  [[ "$output" == *"GEMINI_API_KEY is not set"* ]]
  GEMINI_API_KEY=wrong run "$VFY" why --log build.log --provider gemini
  [[ "$output" == *"Gemini API: API key not valid"* ]]
  GEMINI_API_KEY=gem-key run "$VFY" why --log build.log --provider gemini
  [ "$status" -eq 0 ]
  [[ "$output" == "**Cause:** the import"* ]]
  [[ "$output" != *"thinking about"* ]]
  [[ "$output" == *"— gemma-4-26b-a4b-it via the Gemini API"* ]]
  run grep -q 'gem-key' "$T/requests.log"   # the mock logs the URL and body, never the key
  [ "$status" -ne 0 ]
  # Server mode works with it too.
  GEMINI_API_KEY=gem-key run "$VFY" why blog --provider gemini
  [ "$status" -eq 0 ]
  [[ "$output" == *"**Fix:** rename it."* ]]
}

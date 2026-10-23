#!/usr/bin/env bats
# `vercelify llm` and `vercelify autodeploy` against a mock agent API.

setup() {
  T=$(mktemp -d)
  python3 "$BATS_TEST_DIRNAME/mock_agent.py" "$T/port" "$T/requests.log" &
  MOCK_PID=$!
  for _ in $(seq 50); do [ -s "$T/port" ] && break; sleep 0.1; done
  mkdir -p "$T/etc"
  echo test-agent-token >"$T/etc/agent.token"
  printf 'role=main\nversion=test\n' >"$T/etc/install.env"
  export NO_COLOR=1 VERCELIFY_ETC="$T/etc" VERCELIFY_LOG="$T/log" LLM_POLL_SECS=0
  export AGENT_LOCAL_URL="http://127.0.0.1:$(cat "$T/port")" STORAGE_MOUNT="$T/no-hdd"
  CLI="$BATS_TEST_DIRNAME/../vercelify"
}

teardown() { kill "$MOCK_PID" 2>/dev/null; rm -rf "$T"; }

@test "llm: install, pull, list, chat and delete" {
  run "$CLI" llm
  [ "$status" -eq 0 ]
  [[ "$output" == *"not installed"* ]]

  run "$CLI" llm pull llama3.2:3b
  [ "$status" -ne 0 ]
  [[ "$output" == *"Ollama is not running"* ]]

  run "$CLI" llm install
  [ "$status" -eq 0 ]
  [[ "$output" == *"Ollama is running"* ]]
  grep -q 'POST /v1/llm/install .*"tier":"ssd"' "$T/requests.log"  # no HDD mounted

  run "$CLI" llm pull llama3.2:3b
  [ "$status" -eq 0 ]
  [[ "$output" == *"50% of"* ]]
  [[ "$output" == *"llama3.2:3b is ready"* ]]

  run "$CLI" llm status
  [ "$status" -eq 0 ]
  [[ "$output" == *"Ollama"*"running"*"0.12.3"* ]]
  [[ "$output" == *"endpoint  http://laptop.tail1234.ts.net:11434"* ]]
  [[ "$output" == *"model     llama3.2:3b"* ]]

  run "$CLI" llm chat llama3.2:3b "say hi"
  [ "$status" -eq 0 ]
  [ "$output" = "Hello from llama3.2:3b" ]

  run "$CLI" llm rm llama3.2:3b
  [ "$status" -eq 0 ]
  run "$CLI" llm rm llama3.2:3b
  [ "$status" -ne 0 ]
  [[ "$output" == *"is not downloaded"* ]]
}

@test "llm: a failed download exits non-zero with Ollama's reason" {
  "$CLI" llm install >/dev/null 2>&1
  run "$CLI" llm pull bad:1b
  [ "$status" -ne 0 ]
  [[ "$output" == *"file does not exist"* ]]
}

@test "llm: install puts models on the HDD when it is mounted" {
  mountpoint() { return 0; }
  export -f mountpoint
  run "$CLI" llm install
  [ "$status" -eq 0 ]
  grep -q '"tier":"hdd"' "$T/requests.log"
  run "$CLI" llm install floppy
  [ "$status" -ne 0 ]
}

@test "the agent token goes to curl on stdin, never on the command line" {
  "$CLI" llm >/dev/null
  grep -q 'auth=Bearer test-agent-token' "$T/requests.log"
  ! grep -q -- 'test-agent-token' <(ps -eo args) 2>/dev/null || true
  grep -q -- '-K -' "$CLI"
}

@test "autodeploy: add, list, check, remove" {
  run "$CLI" autodeploy
  [ "$status" -eq 0 ]
  [[ "$output" == *"no branches watched"* ]]

  run "$CLI" autodeploy add app1 https://github.com/you/blog.git dev --token ghp_x
  [ "$status" -eq 0 ]
  [[ "$output" == *"pushes to dev of https://github.com/you/blog.git now deploy app1"* ]]
  grep -q 'POST /v1/autodeploy .*"token":"ghp_x"' "$T/requests.log"
  grep -q 'POST /v1/autodeploy/app1/check' "$T/requests.log"

  run "$CLI" autodeploy add app2 https://github.com/you/site.git
  grep -q '"app_uuid":"app2","repo":"https://github.com/you/site.git","branch":"main"}' "$T/requests.log"

  run "$CLI" autodeploy list
  [ "$status" -eq 0 ]
  [[ "$output" == *"app1"*"dev"*"on"* ]]
  [[ "$output" == *"app2"*"main"* ]]

  run "$CLI" autodeploy check app1
  [ "$status" -eq 0 ]
  [[ "$output" == *"head 1111111111"* ]]

  run "$CLI" autodeploy rm app1
  [ "$status" -eq 0 ]
  run "$CLI" autodeploy rm app1
  [ "$status" -ne 0 ]
  [[ "$output" == *"no auto-deploy for this application"* ]]
}

@test "autodeploy: bad input and missing arguments" {
  run "$CLI" autodeploy add app1 https://github.com/you/blog.git --upload-pack
  [ "$status" -ne 0 ]
  [[ "$output" == *"not a valid branch name"* ]]
  run "$CLI" autodeploy add app1
  [ "$status" -ne 0 ]
  [[ "$output" == *"usage"* ]]
}

@test "without the token file the CLI asks for sudo" {
  rm "$T/etc/agent.token"
  run "$CLI" llm
  [ "$status" -ne 0 ]
  [[ "$output" == *"run with sudo"* ]]
}

@test "a dead agent gives a pointer to doctor" {
  export AGENT_LOCAL_URL=http://127.0.0.1:9
  run "$CLI" autodeploy
  [ "$status" -ne 0 ]
  [[ "$output" == *"vercelify doctor"* ]]
}

@test "doctor prints the agent's report (one source of truth) and fails on problems" {
  run "$CLI" doctor
  [ "$status" -eq 1 ]
  [[ "$output" == *"Docker — Docker 27.3.1"* ]]
  [[ "$output" == *"System disk type — running from an SD card"* ]]
  [[ "$output" == *"→ re-run the installer, then reboot"* ]]
  [[ "$output" == *"1 problem(s), 1 warning(s)"* ]]
  grep -q 'GET /v1/doctor?refresh=true' "$T/requests.log"
}

@test "doctor falls back to local checks when the agent is down" {
  AGENT_LOCAL_URL=http://127.0.0.1:9 run "$CLI" doctor
  [[ "$output" == *"the agent is not answering"* ]]
}

@test "sleep: turn on, list and off" {
  run "$CLI" sleep
  [ "$status" -eq 0 ]
  [[ "$output" == *"no app sleeps"* ]]
  run "$CLI" sleep on app1 15 --previews
  [ "$status" -eq 0 ]
  grep -q 'PUT /v1/sleep/app1 .*"idle_minutes":15,"previews":true' "$T/requests.log"
  run "$CLI" sleep list
  [[ "$output" == *"app1  after 15 min idle (and previews)  blog.example.com asleep"* ]]
  run "$CLI" sleep on app1 2
  [ "$status" -ne 0 ]
  [[ "$output" == *"5 minutes to 24 hours"* ]]
  run "$CLI" sleep on app1 soon
  [ "$status" -ne 0 ]
  run "$CLI" sleep off app1
  [ "$status" -eq 0 ]
  grep -q 'PUT /v1/sleep/app1 .*"enabled":false' "$T/requests.log"
}

@test "pair: both codes come from one agent call (one device token)" {
  # A fake agent that counts the tokens it mints.
  cat >"$T/agent" <<'STUB'
#!/bin/sh
echo "$*" >>"$T_CALLS"
case "$*" in
  *"pair --short --with-code"*) printf 'ABCD-EFGH\nvfy1.one\n' ;;
  *"pair --short"*) echo WXYZ-2345 ;;
  *pair*) echo vfy1.two ;;
esac
STUB
  chmod +x "$T/agent"
  export T_CALLS="$T/calls"
  AGENT_BIN="$T/agent" VERCELIFY_TEST=1 run "$CLI" pair
  [ "$status" -eq 0 ]
  [[ "$output" == *"ABCD-EFGH"* ]]
  [[ "$output" == *"vfy1.one"* ]]
  [ "$(wc -l <"$T/calls")" -eq 1 ]
}

@test "pair: an older agent without --with-code still gets both codes" {
  cat >"$T/agent" <<'STUB'
#!/bin/sh
echo "$*" >>"$T_CALLS"
case "$*" in
  *--with-code*) echo "error: unexpected argument '--with-code'" >&2; exit 2 ;;
  *"pair --short"*) echo WXYZ-2345 ;;
  *pair*) echo vfy1.two ;;
esac
STUB
  chmod +x "$T/agent"
  export T_CALLS="$T/calls"
  AGENT_BIN="$T/agent" VERCELIFY_TEST=1 run "$CLI" pair
  [ "$status" -eq 0 ]
  [[ "$output" == *"WXYZ-2345"* ]]
  [[ "$output" == *"vfy1.two"* ]]
}

@test "uninstall wakes sleeping apps and removes Vercelify's own containers" {
  "$CLI" sleep on app1 15 >/dev/null
  mkdir -p "$T/bin" "$T/systemd"
  # only Ollama is installed
  printf '#!/bin/sh\necho "docker $*" >>"%s/docker.log"\n[ "$1 $2" = "inspect vercelify-registry" ] && exit 1\nexit 0\n' "$T" >"$T/bin/docker"
  printf '#!/bin/sh\nexit 0\n' >"$T/bin/systemctl"
  chmod +x "$T/bin/"*
  PATH="$T/bin:$PATH" ASSUME_YES=1 VERCELIFY_TEST=1 SYSTEMD_DIR="$T/systemd" AGENT_BIN="$T/none" VERCELIFY_LIB="$T/lib" CLI_LINK="$T/cli-link" \
    run "$CLI" uninstall
  [ "$status" -eq 0 ]
  grep -q 'PUT /v1/sleep/app1 .*"enabled":false' "$T/requests.log"
  # without the firewall, Ollama's API would be open to the network
  grep -q 'docker rm -f vercelify-ollama' "$T/docker.log"
  ! grep -q 'docker rm -f vercelify-registry' "$T/docker.log"
  [[ "$output" == *"removed the vercelify-ollama container"* ]]
  [[ "$output" != *"vercelify-registry"* ]]
}

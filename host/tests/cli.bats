#!/usr/bin/env bats
bats_require_minimum_version 1.5.0
# `vercelify` commands (llm, sleep, doctor, pair…) against a mock agent API.

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
  [[ "$output" == *"endpoint  http://10.77.0.1:11434"* ]]
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
  mkdir -p "$T/bin"
  printf '#!/bin/sh\necho "$*" >>"%s"\nexec %s "$@"\n' "$T/curl-argv" "$(command -v curl)" >"$T/bin/curl"
  chmod +x "$T/bin/curl"
  PATH="$T/bin:$PATH" "$CLI" llm >/dev/null
  grep -q 'auth=Bearer test-agent-token' "$T/requests.log"
  [ -s "$T/curl-argv" ]
  run ! grep -q -- 'test-agent-token' "$T/curl-argv"
  grep -q -- '-K -' "$BATS_TEST_DIRNAME/../lib/agent.sh"
}

@test "without the token file the CLI asks for sudo" {
  rm "$T/etc/agent.token"
  run "$CLI" llm
  [ "$status" -ne 0 ]
  [[ "$output" == *"run with sudo"* ]]
}

@test "a dead agent gives a pointer to doctor" {
  export AGENT_LOCAL_URL=http://127.0.0.1:9
  run "$CLI" sleep
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
  run "$CLI" sleep on blog 15 --previews
  [ "$status" -eq 0 ]
  grep -q 'PUT /v1/sleep/app1 .*"idle_minutes":15,"previews":true' "$T/requests.log"
  run "$CLI" sleep list
  [[ "$output" == *"blog  after 15 min idle (and previews)  blog.example.com asleep"* ]]
  run "$CLI" sleep on nope 15
  [ "$status" -ne 0 ]
  [[ "$output" == *"no app named nope"* ]]
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
  "$CLI" sleep on blog 15 >/dev/null
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
  [[ "$output" == *"removed the vercelify-ollama container"* ]]
  [[ "$output" != *"vercelify-registry"* ]]
  run ! grep -q 'docker rm -f vercelify-registry' "$T/docker.log"
}

@test "git-source: a token from stdin (never an argument), a deploy key, list and remove" {
  export VERCELIFY_TEST=1
  run bash -c 'echo ghp_secret123 | "$1" git-source add work --provider github' _ "$CLI"
  [ "$status" -eq 0 ]
  grep -q 'POST /v1/platform/git-sources .*"token":"ghp_secret123"' "$T/requests.log"
  # the token is in no command's arguments: agent_api sends the body through a descriptor
  grep -q -- '--data-binary "@/dev/fd/3"' "$BATS_TEST_DIRNAME/../lib/agent.sh"
  run "$CLI" git-source add ci --provider gitea --deploy-key </dev/null
  [ "$status" -eq 0 ]
  [[ "$output" == *"ssh-ed25519 AAAAdeploykey"* ]]
  run "$CLI" git-source
  [[ "$output" == *"work"*"github"*"token"* && "$output" == *"ci"*"gitea"*"deploy_key"* ]]
  run "$CLI" git-source rm work
  [ "$status" -eq 0 ]
  grep -q 'DELETE /v1/platform/git-sources/git_1' "$T/requests.log"
  run "$CLI" git-source add empty </dev/null
  [ "$status" -ne 0 ]
  [[ "$output" == *"no token given"* ]]
  run "$CLI" git-source add x --provider bitbucket </dev/null
  [[ "$output" == *"unknown provider"* ]]
}

@test "pair --host reaches the agent; anything else is refused" {
  printf '#!/bin/sh\necho "$*" >>"$T_CALLS"\nprintf "ABCD-EFGH\\nvfy1.lan\\n"\n' >"$T/agent"
  chmod +x "$T/agent"
  export T_CALLS="$T/calls"
  AGENT_BIN="$T/agent" VERCELIFY_TEST=1 run "$CLI" pair --host 192.168.1.20
  [ "$status" -eq 0 ]
  grep -q 'pair --host 192.168.1.20 --short --with-code' "$T/calls"
  AGENT_BIN="$T/agent" VERCELIFY_TEST=1 run "$CLI" pair --bogus
  [ "$status" -ne 0 ]
  [[ "$output" == *"usage: vercelify pair [--host HOST]"* ]]
}

@test "ask sets the caller's variable even when the caller has a local of the same name" {
  run bash -c '. "$1/lib/common.sh"; has_tty() { return 1; }
    f() { local reply; ask reply "Sure?" fallback; printf "%s" "$reply"; }; f
    g() { local reply; confirm "Sure?" y && echo yes || echo no; }; g' _ "$BATS_TEST_DIRNAME/.."
  [ "$status" -eq 0 ]
  [ "$output" = "fallbackyes" ]
}

@test "a declined uninstall says so, exits non-zero and changes nothing" {
  mkdir -p "$T/bin" "$T/systemd" "$T/lib"
  printf '#!/bin/sh\necho "$*" >>"%s/calls"\n' "$T" >"$T/bin/systemctl"
  printf '#!/bin/sh\necho "docker $*" >>"%s/calls"\n' "$T" >"$T/bin/docker"
  chmod +x "$T/bin/"*
  touch "$T/systemd/vercelify-agent.service"
  PATH="$T/bin:$PATH" VERCELIFY_NONINTERACTIVE=1 VERCELIFY_TEST=1 SYSTEMD_DIR="$T/systemd" VERCELIFY_LIB="$T/lib" \
    run "$CLI" uninstall
  [ "$status" -eq 1 ]
  [[ "$output" == *"cancelled"* ]]
  [ -f "$T/systemd/vercelify-agent.service" ]
  [ ! -s "$T/calls" ]
}

@test "uninstall stops the apps and removes the proxy, builder and slice, and gives the lid back" {
  mkdir -p "$T/bin" "$T/systemd" "$T/lib" "$T/logind"
  printf '#!/bin/sh\necho "systemctl $*" >>"%s/calls"\n' "$T" >"$T/bin/systemctl"
  cat >"$T/bin/docker" <<EOF
#!/bin/sh
echo "docker \$*" >>"$T/calls"
case "\$1 \$2" in
  "ps -aq") echo c1; echo c2 ;;
  "inspect vercelify-proxy"|"inspect buildx_buildkit_vercelify0") exit 0 ;;
  inspect*) exit 1 ;;
esac
exit 0
EOF
  chmod +x "$T/bin/"*
  touch "$T/systemd/vercelify.slice" "$T/logind/90-vercelify.conf" "$T/etc/sleep.vercelify"
  echo '{"cgroup-parent":"vercelify.slice","log-driver":"json-file"}' >"$T/daemon.json"
  PATH="$T/bin:$PATH" ASSUME_YES=1 VERCELIFY_TEST=1 SYSTEMD_DIR="$T/systemd" AGENT_BIN="$T/none" VERCELIFY_LIB="$T/lib" CLI_LINK="$T/cli-link" \
    LOGIND_DROPIN="$T/logind/90-vercelify.conf" DOCKER_DAEMON_JSON="$T/daemon.json" AGENT_LOCAL_URL=http://127.0.0.1:9 \
    run "$CLI" uninstall
  [ "$status" -eq 0 ]
  grep -q 'docker update --restart=no c1 c2' "$T/calls"
  grep -q 'docker stop c1 c2' "$T/calls"
  [[ "$output" == *"stopped 2 app container(s)"* ]]
  grep -q 'docker rm -f vercelify-proxy' "$T/calls"
  grep -q 'docker rm -f buildx_buildkit_vercelify0' "$T/calls"
  [ ! -e "$T/systemd/vercelify.slice" ]
  [ "$(jq -c . "$T/daemon.json")" = '{"log-driver":"json-file"}' ]
  [ ! -e "$T/logind/90-vercelify.conf" ]
  grep -q 'systemctl unmask --quiet sleep.target' "$T/calls"
}

@test "uninstall with remote access on but no router doesn't stop halfway" {
  mkdir -p "$T/bin" "$T/systemd" "$T/lib"
  printf '#!/bin/sh\nexit 0\n' >"$T/bin/systemctl"
  printf '#!/bin/sh\n[ "$1" = inspect ] && exit 1\nexit 0\n' >"$T/bin/docker"
  printf '#!/bin/sh\nexit 1\n' >"$T/bin/upnpc"
  chmod +x "$T/bin/"*
  echo 'enabled=1' >"$T/etc/remote.env"
  PATH="$T/bin:$PATH" ASSUME_YES=1 VERCELIFY_TEST=1 SYSTEMD_DIR="$T/systemd" AGENT_BIN="$T/none" VERCELIFY_LIB="$T/lib" CLI_LINK="$T/cli-link" \
    UPNPC="$T/bin/upnpc" NATPMPC="$T/bin/none" AGENT_LOCAL_URL=http://127.0.0.1:9 run "$CLI" uninstall
  [ "$status" -eq 0 ]
  [[ "$output" == *"removed."* ]]
}

@test "version without sudo: install.env unreadable, the world-readable copy answers" {
  rm "$T/etc/install.env"
  mkdir -p "$T/lib"
  echo '0.2.5+main.1a2b3c4' >"$T/lib/VERSION"
  VERCELIFY_LIB="$T/lib" AGENT_BIN="$T/none" run "$CLI" version
  [ "$status" -eq 0 ]
  [[ "$output" == "vercelify 0.2.5+main.1a2b3c4"* ]]
}

@test "llm --cancel stops running AI jobs" {
  run "$CLI" llm --cancel
  [ "$status" -eq 0 ]
  grep -q 'DELETE /v1/jobs/job-ai' "$T/requests.log"
  [[ "$output" == *"cancel"* ]]
}

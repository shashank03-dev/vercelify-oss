#!/usr/bin/env bash
# Install the vercelify-agent binary, config, token and systemd unit.

: "${VERCELIFY_REPO:=shashank03-dev/vercelify-oss}"
: "${AGENT_BIN:=/usr/local/bin/vercelify-agent}"

# Locate or obtain an agent binary for this architecture; prints its path.
obtain_agent_binary() {
  local arch target tmp url
  arch=$(detect_arch)
  target=$(rust_target "$arch") || die "unsupported architecture $(uname -m) (need x86_64 or aarch64)"

  # 1. Explicit binary (offline installs, CI)
  if [[ -n "${VERCELIFY_AGENT_BINARY:-}" ]]; then
    [[ -x "$VERCELIFY_AGENT_BINARY" ]] || die "VERCELIFY_AGENT_BINARY=$VERCELIFY_AGENT_BINARY is not executable"
    echo "$VERCELIFY_AGENT_BINARY"; return
  fi
  # 2. Shipped inside a release tarball next to this script
  if [[ -x "$VERCELIFY_SRC/dist/vercelify-agent-$target" ]]; then
    echo "$VERCELIFY_SRC/dist/vercelify-agent-$target"; return
  fi
  # 3. The release asset of *this* version, so scripts and agent always match
  #    (checksum verified). Edge installs from main fall through to a build.
  tmp=$(mktemp -d)
  local tag="v${VERSION:-0}"
  url="https://github.com/$VERCELIFY_REPO/releases/download/$tag/vercelify-agent-$target"
  if [[ "${VERSION:-dev}" != dev ]] &&
     curl -fsSL --connect-timeout 15 -o "$tmp/agent" "$url" 2>/dev/null &&
     curl -fsSL --connect-timeout 15 -o "$tmp/agent.sha256" "$url.sha256" 2>/dev/null; then
    if (cd "$tmp" && echo "$(awk '{print $1}' agent.sha256)  agent" | sha256sum -c --quiet -); then
      chmod +x "$tmp/agent"
      info "downloaded agent $tag" >&2
      echo "$tmp/agent"; return
    fi
    warn "checksum mismatch for downloaded agent — ignoring it" >&2
  fi
  # 4. Build from source in a throwaway Rust container (a few minutes)
  [[ -f "$VERCELIFY_SRC/Cargo.toml" ]] || die "no agent binary available and no source tree to build from"
  have docker || die "Docker is required to build the agent from source"
  info "building the agent from source in Docker (one-time, ~5 min on a laptop CPU)…" >&2
  run_cmd docker run --rm -v "$VERCELIFY_SRC":/src:ro -v "$tmp":/out -w /tmp rust:1-alpine sh -ec '
      apk add --no-cache musl-dev >/dev/null
      cp -r /src /tmp/build && cd /tmp/build
      cargo build --release --locked -p vercelify-agent
      cp target/release/vercelify-agent /out/agent' >&2 || die "building the agent failed"
  echo "$tmp/agent"
}

install_agent() {
  step "Vercelify agent"
  local bin
  if [[ "$VERCELIFY_DRY_RUN" == 1 ]]; then
    info "[dry-run] would install $AGENT_BIN"
    bin=/dev/null
  else
    bin=$(obtain_agent_binary) || exit 1
  fi
  if [[ ! -x "$AGENT_BIN" ]] || ! cmp -s "$bin" "$AGENT_BIN"; then
    run_cmd install -m 0755 "$bin" "$AGENT_BIN"
  fi
  run_cmd mkdir -p "$VERCELIFY_ETC"
  run_cmd chmod 0750 "$VERCELIFY_ETC"
  if [[ ! -f "$VERCELIFY_ETC/agent.toml" ]]; then
    run_cmd install -m 0644 "$VERCELIFY_SRC/host/config/agent.toml" "$VERCELIFY_ETC/agent.toml"
  fi
  if [[ ! -s "$VERCELIFY_ETC/agent.token" ]]; then
    random_hex 32 | write_file "$VERCELIFY_ETC/agent.token" 0600 || true
  fi
  # The agent may authorise a main host's Coolify key here (server pools);
  # systemd only lets it write to this directory if it exists.
  run_cmd install -d -m 0700 /root/.ssh
  run_cmd install -d -m 0700 /var/lib/vercelify
  write_file "$SYSTEMD_DIR/vercelify-agent.service" 0644 <"$VERCELIFY_SRC/host/systemd/vercelify-agent.service" || true
  run_cmd systemctl daemon-reload
  run_cmd systemctl enable vercelify-agent >/dev/null 2>&1
  run_cmd systemctl restart vercelify-agent
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] && return 0
  for _ in $(seq 1 20); do
    curl -fsS -m 2 http://127.0.0.1:7420/v1/health >/dev/null 2>&1 && { ok "agent $("$AGENT_BIN" --version | awk '{print $2}') running on :7420"; return 0; }
    sleep 1
  done
  warn "agent did not answer yet — check: journalctl -u vercelify-agent -n 50"
}

install_power_guard() {
  step "Power & thermal guard"
  run_cmd mkdir -p "$VERCELIFY_LIB"
  run_cmd install -m 0755 "$VERCELIFY_SRC/host/power-guard.sh" "$VERCELIFY_LIB/power-guard.sh"
  if [[ ! -f "$VERCELIFY_ETC/power-guard.conf" ]]; then
    # Thresholds suited to this machine (see hardware.sh).
    local p=${HW_PROFILE:-laptop}
    sed -e "s/^THERMAL_PAUSE_C=.*/THERMAL_PAUSE_C=$(profile_value "$p" pause)/" \
        -e "s/^THERMAL_RESUME_C=.*/THERMAL_RESUME_C=$(profile_value "$p" resume)/" \
        -e "s/^THERMAL_SHUTDOWN_C=.*/THERMAL_SHUTDOWN_C=$(profile_value "$p" shutdown)/" \
        "$VERCELIFY_SRC/host/config/power-guard.conf" | write_file "$VERCELIFY_ETC/power-guard.conf" 0644 || true
  fi
  write_file "$SYSTEMD_DIR/vercelify-power-guard.service" 0644 <"$VERCELIFY_SRC/host/systemd/vercelify-power-guard.service" || true
  run_cmd systemctl daemon-reload
  run_cmd systemctl enable vercelify-power-guard >/dev/null 2>&1
  run_cmd systemctl restart vercelify-power-guard
  if compgen -G "/sys/class/power_supply/BAT*" >/dev/null; then
    ok "battery found — graceful shutdown at $(. "$VERCELIFY_ETC/power-guard.conf" 2>/dev/null; echo "${BATTERY_SHUTDOWN_PCT:-15}")% when unplugged"
  else
    ok "no battery — guarding temperature only"
  fi
}

install_firewall() {
  step "Firewall"
  run_cmd mkdir -p "$VERCELIFY_LIB"
  run_cmd install -m 0755 "$VERCELIFY_SRC/host/firewall.sh" "$VERCELIFY_LIB/firewall.sh"
  write_file "$SYSTEMD_DIR/vercelify-firewall.service" 0644 <"$VERCELIFY_SRC/host/systemd/vercelify-firewall.service" || true
  run_cmd systemctl daemon-reload
  run_cmd systemctl enable vercelify-firewall >/dev/null 2>&1
  run_cmd systemctl restart vercelify-firewall
  ok "dashboard and AI API reachable only over Tailscale (apps on 80/443 stay public)"
}

install_cli() {
  run_cmd mkdir -p "$VERCELIFY_LIB"
  # Keep a copy of the scripts so `vercelify update/add-worker/domain` work offline.
  run_cmd rsync -a --delete --exclude target --exclude node_modules --exclude .git \
    "$VERCELIFY_SRC/host/" "$VERCELIFY_LIB/host/"
  run_cmd install -m 0644 "$VERCELIFY_SRC/install.sh" "$VERCELIFY_LIB/install.sh" 2>/dev/null || true
  run_cmd ln -sf "$VERCELIFY_LIB/host/vercelify" /usr/local/bin/vercelify
}

# install_ai [MODEL] — Ollama through the agent (same code path as
# `vercelify llm`), then optionally a first model. AI is optional: failures
# only warn.
install_ai() {
  local model=${1:-}
  step "Local AI models (Ollama)"
  if [[ "$VERCELIFY_DRY_RUN" == 1 ]]; then
    info "[dry-run] would install Ollama${model:+ and download $model}"
    return 0
  fi
  local cli="$VERCELIFY_LIB/host/vercelify"
  if ! LLM_POLL_SECS=3 "$cli" llm install; then
    warn "Ollama could not be installed now — later run: sudo vercelify llm install"
    return 0
  fi
  if [[ -n "$model" ]]; then
    LLM_POLL_SECS=3 "$cli" llm pull "$model" || warn "downloading $model failed — later run: sudo vercelify llm pull $model"
  fi
}

# A first model that suits this machine's memory.
default_ai_model() {
  local mem_mb
  mem_mb=$(awk '/MemTotal/ {print int($2/1024)}' /proc/meminfo 2>/dev/null || echo 0)
  if (( mem_mb >= 7000 )); then echo llama3.2:3b; else echo gemma3:1b; fi
}

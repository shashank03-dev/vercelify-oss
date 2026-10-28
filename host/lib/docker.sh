#!/usr/bin/env bash
# Docker Engine install + daemon configuration.

# Pick a container address pool that cannot collide with the LAN.
docker_address_pool() {
  local gw
  gw=$(ip -4 route show default 2>/dev/null | awk '{print $3; exit}')
  if [[ "$gw" == 10.* ]]; then echo "172.30.0.0/16"; else echo "10.0.0.0/8"; fi
}

# Merge our settings into daemon.json (log rotation, an address pool that
# can't collide with the LAN, live-restore).
# Containers go into vercelify.slice (memory-capped, see hardware.sh) when
# Docker uses the systemd cgroup driver, i.e. on cgroup v2 hosts.
docker_cgroup_parent() {
  if [[ -n "${DOCKER_CGROUP_PARENT+x}" ]]; then printf '%s' "$DOCKER_CGROUP_PARENT"; return; fi
  [[ -r "$CGROUP_CONTROLLERS" ]] && printf 'vercelify.slice'
  return 0
}

docker_daemon_config() {
  local pool=${DOCKER_POOL:-$(docker_address_pool)} parent desired current merged
  parent=$(docker_cgroup_parent)
  # live-restore: containers keep running while dockerd restarts (updates).
  desired=$(jq -n --arg pool "$pool" --arg parent "$parent" '{
    "log-driver": "json-file",
    "log-opts": {"max-size": "10m", "max-file": "3"},
    "default-address-pools": [{"base": $pool, "size": 24}],
    "features": {"buildkit": true},
    "shutdown-timeout": 30,
    "live-restore": true
  } + (if $parent != "" then {"cgroup-parent": $parent} else {} end)')
  if [[ -s "$DOCKER_DAEMON_JSON" ]]; then
    current=$(cat "$DOCKER_DAEMON_JSON")
    jq -e . >/dev/null 2>&1 <<<"$current" || die "$DOCKER_DAEMON_JSON is not valid JSON — fix or remove it"
    # Existing values win so we never fight a user's choices.
    merged=$(jq -n --argjson a "$desired" --argjson b "$current" '$a * $b')
    # A cgroupfs driver takes a path, not a systemd slice: leave it out then.
    if jq -e '(."exec-opts" // []) | any(. == "native.cgroupdriver=cgroupfs")' >/dev/null <<<"$current"; then
      merged=$(jq 'if ."cgroup-parent" == "vercelify.slice" then del(."cgroup-parent") else . end' <<<"$merged")
    fi
  else
    merged=$desired
  fi
  jq . <<<"$merged" | write_file "$DOCKER_DAEMON_JSON" 0644
}

# Existing distro Docker installations can run containers while lacking the
# Buildx CLI plugin required by both of the platform's build drivers.
ensure_buildx() {
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] && { info "[dry-run] would verify Docker Buildx"; return 0; }
  docker buildx version >/dev/null 2>&1 && return 0
  info "Docker Buildx is missing — installing the build plugin"
  apt_update_once
  # Docker CE's repository and Ubuntu's repository use different names.
  if ! apt_install docker-buildx-plugin && ! apt_install docker-buildx; then
    die "Docker runs, but its Buildx plugin could not be installed. Install Docker from https://docs.docker.com/engine/install/ and rerun setup; existing apps and data are preserved."
  fi
  docker buildx version >/dev/null 2>&1 || die "Docker Buildx is still unavailable after package installation — repair Docker's CLI plugins and rerun setup"
  ok "Docker Buildx ready"
}

ensure_compose() {
  [[ "$VERCELIFY_DRY_RUN" == 1 ]] && { info "[dry-run] would verify Docker Compose"; return 0; }
  docker compose version >/dev/null 2>&1 && return 0
  info "Docker Compose is missing — installing the application runtime plugin"
  apt_update_once
  if ! apt_install docker-compose-plugin && ! apt_install docker-compose-v2; then
    die "Docker runs, but its Compose v2 plugin could not be installed. Install Docker from https://docs.docker.com/engine/install/ and rerun setup; existing apps and data are preserved."
  fi
  docker compose version >/dev/null 2>&1 || die "Docker Compose v2 is still unavailable after package installation — repair Docker's CLI plugins and rerun setup"
  ok "Docker Compose ready"
}

install_docker() {
  step "Docker Engine"
  local changed=0 mount_unit
  if docker_daemon_config; then changed=1; fi
  if [[ "$VERCELIFY_DRY_RUN" == 1 ]]; then
    info "[dry-run] would install or configure Docker (no installer downloaded)"
  elif have docker && docker info >/dev/null 2>&1; then
    ok "Docker $(docker version --format '{{.Server.Version}}' 2>/dev/null || true) already installed"
    if (( changed )); then
      info "daemon.json changed — restarting Docker"
      run_cmd systemctl restart docker
    fi
  else
    local tmp
    tmp=$(mktemp)
    fetch https://get.docker.com "$tmp" || die "could not download the Docker installer"
    # Third-party installers read generic variables such as DRY_RUN; never leak ours.
    run_cmd env -u DRY_RUN sh "$tmp" >>"$VERCELIFY_LOG" 2>&1 || die "Docker installation failed (see $VERCELIFY_LOG)"
    [[ "$VERCELIFY_DRY_RUN" == 1 ]] || have docker || die "the Docker installer finished but docker is missing (see $VERCELIFY_LOG)"
    rm -f "$tmp"
    run_cmd systemctl enable --now docker >/dev/null 2>&1
    ok "Docker $(docker version --format '{{.Server.Version}}' 2>/dev/null || true) installed"
  fi

  ensure_buildx
  ensure_compose

  # Start Docker after the storage tier is mounted. `Wants`, not `Requires`:
  # Docker must still come up when the HDD is missing.
  mount_unit=$(systemd-escape --path --suffix=mount "$STORAGE_MOUNT" 2>/dev/null || echo mnt-storage.mount)
  if write_file "$SYSTEMD_DIR/docker.service.d/10-vercelify.conf" 0644 <<EOF
[Unit]
Wants=network-online.target
After=network-online.target $mount_unit

[Service]
TimeoutStopSec=120
EOF
  then
    run_cmd systemctl daemon-reload
  fi

  if [[ -n "${SUDO_USER:-}" && "$SUDO_USER" != root ]] && ! id -nG "$SUDO_USER" | grep -qw docker; then
    run_cmd usermod -aG docker "$SUDO_USER" && info "added $SUDO_USER to the docker group (log in again to apply)"
  fi
}

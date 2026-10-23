#!/usr/bin/env bash
# A private image registry on the tailnet, so the strongest machine builds
# and small ones (a Raspberry Pi) only pull. Coolify supports build servers
# plus a registry; this provides the registry and the Docker trust for it.

: "${REGISTRY_CONTAINER:=vercelify-registry}"
: "${REGISTRY_PORT:=5000}"

valid_registry() { [[ "$1" =~ ^[a-z0-9]([a-z0-9.-]*[a-z0-9])?:[0-9]{2,5}$ ]]; }

# Let this machine's Docker pull from/push to HOST:PORT (plain HTTP over
# Tailscale, which is already encrypted). Returns 0 if daemon.json changed.
registry_trust() {
  local reg=$1 current merged
  valid_registry "$reg" || die "invalid registry '$reg' (use host:port, e.g. laptop:5000)"
  current='{}'
  [[ -s "$DOCKER_DAEMON_JSON" ]] && current=$(cat "$DOCKER_DAEMON_JSON")
  jq -e . >/dev/null 2>&1 <<<"$current" || die "$DOCKER_DAEMON_JSON is not valid JSON — fix or remove it"
  merged=$(jq --arg r "$reg" '."insecure-registries" = (((."insecure-registries" // []) + [$r]) | unique)' <<<"$current")
  if jq . <<<"$merged" | write_file "$DOCKER_DAEMON_JSON" 0644; then
    # live-restore keeps containers running through the restart.
    run_cmd systemctl restart docker
    ok "Docker on this machine trusts $reg"
    return 0
  fi
  ok "Docker already trusts $reg"
  return 1
}

registry_install() {
  local dir host
  if mountpoint -q "$STORAGE_MOUNT" 2>/dev/null; then dir="$STORAGE_MOUNT/registry"; else dir=/var/lib/vercelify/registry; fi
  host=$(ts_dns_name || true); host=${host%%.*}
  [[ -n "$host" ]] || host=$(hostname -s)
  if docker inspect "$REGISTRY_CONTAINER" >/dev/null 2>&1; then
    ok "registry already running"
  else
    run_cmd mkdir -p "$dir"
    run_cmd docker run -d --name "$REGISTRY_CONTAINER" --restart unless-stopped \
      --label vercelify.managed=registry -p "$REGISTRY_PORT:5000" -v "$dir:/var/lib/registry" \
      -e REGISTRY_STORAGE_DELETE_ENABLED=true registry:2 >/dev/null || die "could not start the registry"
    ok "registry running on $host:$REGISTRY_PORT (data in $dir)"
  fi
  registry_trust "$host:$REGISTRY_PORT" || true
  printf '\n  Next:\n' >&2
  printf '    1. On every server that should pull from it:  sudo vercelify registry trust %s:%s\n' "$host" "$REGISTRY_PORT" >&2
  printf '    2. Coolify → Servers → your strongest machine → "Use it as a build server"\n' >&2
  printf '    3. Coolify → app → General → Docker Registry: image %s:%s/<app>\n' "$host" "$REGISTRY_PORT" >&2
  printf '    Building for a Raspberry Pi on an x86 machine: sudo vercelify registry multiarch\n\n' >&2
}

# Build arm64 images on x86 (and the reverse) through QEMU emulation.
registry_multiarch() {
  run_cmd docker run --privileged --rm tonistiigi/binfmt --install arm64,amd64 >/dev/null ||
    die "could not register the emulators"
  ok "this machine can now build linux/arm64 and linux/amd64 images (slower than native)"
}

registry_cli() {
  case "${1:-}" in
    install) registry_install ;;
    trust) registry_trust "${2:?usage: vercelify registry trust HOST:PORT}" || true ;;
    multiarch) registry_multiarch ;;
    *) die "usage: vercelify registry [install|trust HOST:PORT|multiarch]" ;;
  esac
}

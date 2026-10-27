#!/usr/bin/env bash
# Remote access for real, over the internet: a main server installed with
# --remote where no router answers (so a Cloudflare quick tunnel), and a
# device on another network that joins through the tunnel and reaches the
# agent over the mesh. Then the tunnel gets a new address, as after a
# reboot: the signed note is posted and the device follows it.
#
#   sudo AGENT_BINARY=… MESH_BINARY=… host/tests/integration/remote.sh
#
# Needs Docker, internet access (Cloudflare, ntfy.sh) and prebuilt x86_64
# musl binaries. KEEP=1 leaves the containers running.
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
name=vercelify-remote-home
away=vercelify-remote-away
net=vercelify-remote-away
img=vercelify-it-debian12
: "${AGENT_BINARY:?prebuilt vercelify-agent}" "${MESH_BINARY:?prebuilt vercelify-mesh}"

docker build -q -t "$img" -f "$here/Dockerfile.debian12" "$here" >/dev/null
cleanup() {
  [[ "${KEEP:-0}" == 1 ]] && { echo "left running: docker exec -it $name bash; docker exec -it $away bash"; return; }
  docker rm -f "$name" "$away" >/dev/null 2>&1 || true
  docker volume rm -f vercelify-remote-docker >/dev/null 2>&1 || true
  docker network rm "$net" >/dev/null 2>&1 || true
}
trap cleanup EXIT
x() { docker exec "$name" bash -o pipefail -c "$*"; }
y() { docker exec "$away" bash -o pipefail -c "$*"; }
fail=0
check() { if "${@:2}" >/dev/null 2>&1; then echo "  ✓ $1"; else echo "  ✗ $1"; fail=1; fi; }

# REUSE=1: keep a server a previous KEEP=1 run installed (only its host/
# scripts are refreshed), and test remote access on it again.
if [[ "${REUSE:-0}" == 1 ]] && docker inspect "$name" >/dev/null 2>&1; then
  docker rm -f "$away" >/dev/null 2>&1 || true
  x 'cp -r /src/host/. "$(dirname "$(readlink -f /usr/local/bin/vercelify)")/"'
  x 'vercelify remote on' >/tmp/vercelify-remote.log 2>&1
  echo "==> reusing the installed main server"
else
docker rm -f "$name" "$away" >/dev/null 2>&1 || true
# A separate bridge: the device can't reach the server's LAN address.
docker network create "$net" >/dev/null 2>&1 || true

docker run -d --name "$name" --privileged --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
  --tmpfs /run --tmpfs /run/lock -v vercelify-remote-docker:/var/lib/docker -v "$repo":/src:ro \
  -v "$AGENT_BINARY":/opt/agent:ro -v "$MESH_BINARY":/opt/mesh:ro --device /dev/net/tun \
  --hostname home-box "$img" >/dev/null
for _ in $(seq 30); do x 'systemctl is-system-running 2>/dev/null | grep -qE "running|degraded"' && break; sleep 1; done

echo "==> installing the main server with --remote (no router answers here)"
x "cd /src && NO_COLOR=1 VERCELIFY_AGENT_BINARY=/opt/agent VERCELIFY_MESH_BINARY=/opt/mesh bash host/setup-host.sh -y --remote --no-display --hostname home-box --storage-device none" >/tmp/vercelify-remote.log 2>&1
fi
check "installed" x 'systemctl is-active vercelify-agent'
check "a quick tunnel, as the router said no" x 'grep -q "^method=tunnel" /etc/vercelify/remote.env'
url=$(x "sed -n 's/^public_url=//p' /etc/vercelify/remote.env")
echo "    $url"
check "the agent knows it" x "grep -qx 'mesh_public_url = \"$url\"' /etc/vercelify/agent.toml"
[[ "${REUSE:-0}" == 1 ]] || check "the installer prints a pairing code" grep -q 'install-mac.sh | bash -s -- --pair vfy2\.' /tmp/vercelify-remote.log
# A new name takes a little while to resolve everywhere.
check "the control port answers through the tunnel" bash -c "for _ in \$(seq 20); do curl -fsS -m 10 -o /dev/null '$url/v1/mesh/hello' && exit 0; sleep 3; done; exit 1"
read -r topic _ < <(x 'cat /var/lib/vercelify/mesh/beacon')
check "the signed address is posted" bash -c "curl -fsS -m 20 'https://ntfy.sh/$topic/json?poll=1&since=1h' | grep -q vfyb1"

echo "==> a device on another network joins through the tunnel"
token=$(x 'vercelify mesh invite --device' | grep -o 'vfyj1\.[A-Za-z0-9_-]*' | head -1)
docker run -d --name "$away" --network "$net" -v "$MESH_BINARY":/usr/local/bin/vercelify-mesh:ro \
  debian:12-slim sleep infinity >/dev/null
y 'apt-get update -qq && apt-get install -y -qq curl ca-certificates jq' >/dev/null 2>&1
check "it can't reach the server's LAN address" bash -c "! docker exec $away curl -fsS -m 3 -o /dev/null http://$(x "hostname -I | cut -d' ' -f1"):7443/"
check "joined" y "for _ in 1 2 3 4 5 6; do vercelify-mesh --dir /tmp/n join '$token' --name away-mac --port 0 && exit 0; sleep 5; done; exit 1"
docker exec -d "$away" sh -c 'RUST_LOG=info vercelify-mesh --dir /tmp/n --run-dir /tmp/r up --userspace --socks 127.0.0.1:1080 --port 0 >/tmp/up.log 2>&1'
health() { y 'curl -fsS -m 5 --socks5-hostname 127.0.0.1:1080 http://10.77.0.1:7420/v1/health' >/dev/null 2>&1; }
ok=0
for _ in $(seq 60); do health && { ok=1; break; }; sleep 2; done
check "it reaches the agent over the mesh" test "$ok" = 1
y 'vercelify-mesh --dir /tmp/n --run-dir /tmp/r status' 2>/dev/null | sed 's/^/    /' | head -8

echo "==> the tunnel gets a new address (as after a reboot)"
x 'systemctl restart vercelify-quicktunnel'
x 'vercelify remote refresh' >/dev/null 2>&1
new=$(x "sed -n 's/^public_url=//p' /etc/vercelify/remote.env")
echo "    $new"
check "a new address" test "$new" != "$url"
check "the new address is posted" bash -c "curl -fsS -m 20 'https://ntfy.sh/$topic/json?poll=1&since=1h' | grep -c vfyb1 | grep -qv '^1$'"
# Relayed through the old tunnel, the device can only learn the new address
# from the signed note (it looks after 3 minutes without the coordinator).
# With a direct path, the coordinator's new map reaches it anyway.
path=$(y "jq -r '.peers[] | select(.name==\"home-box\") | .path' /tmp/r/mesh-status.json" 2>/dev/null || true)
echo "    the device's path to the server: ${path:-unknown}"
ok=0
for _ in $(seq 100); do
  if [[ "$path" == relay ]]; then
    y "grep -q 'main server moved' /tmp/up.log" && health && { ok=1; break; }
  else
    health && { ok=1; break; }
  fi
  sleep 6
done
check "the device follows it" test "$ok" = 1
health; check "and still reaches the agent" health
y 'grep -E "moved|relay" /tmp/up.log' 2>/dev/null | tail -4 | sed 's/^/    /'

(( fail )) && { echo "FAILED"; tail -30 /tmp/vercelify-remote.log; exit 1; }
echo "remote access works from another network"

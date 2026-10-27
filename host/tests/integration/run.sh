#!/usr/bin/env bash
# Full installer integration test in a privileged systemd container.
#
#   sudo host/tests/integration/run.sh
#
# Runs the real host/setup-host.sh (no dry run): packages, kernel tuning, an
# ext4 "HDD" on a loop device mounted at /mnt/storage, Docker, the agent with
# Vercelify's engine and its proxy, the mesh (this server coordinates it),
# power guard, firewall and CLI, then checks the result with `vercelify
# doctor`-style assertions and deploys an app.
#
# Env: EXTRA_CA (PEM bundle to trust inside, for TLS-intercepting proxies),
#      REGISTRY_MIRROR (e.g. https://mirror.gcr.io), KEEP=1 (leave it running),
#      AGENT_BINARY / MESH_BINARY (prebuilt x86_64 musl vercelify-agent and
#      vercelify-mesh; otherwise the installer downloads a release or builds
#      from source).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
name=vercelify-it
img=vercelify-it-debian12

docker build -q -t "$img" -f "$here/Dockerfile.debian12" "$here" >/dev/null
docker rm -f "$name" >/dev/null 2>&1 || true

# A 2 GB ext4 "HDD" on a loop device (no partition table, like a reused disk).
disk=$(mktemp /tmp/vercelify-it-disk.XXXXXX)
truncate -s 2G "$disk"
mkfs.ext4 -q -F -L olddata "$disk"
loop=$(losetup --find --show "$disk")
cleanup() {
  [[ "${KEEP:-0}" == 1 ]] && { echo "left running: docker exec -it $name bash"; return; }
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker volume rm -f vercelify-it-docker vercelify-it-containerd >/dev/null 2>&1 || true
  losetup -d "$loop" 2>/dev/null || true
  rm -f "$disk"
}
trap cleanup EXIT

args=(--privileged --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw --tmpfs /run --tmpfs /run/lock
      -v vercelify-it-docker:/var/lib/docker -v vercelify-it-containerd:/var/lib/containerd -v "$repo":/src:ro --hostname it-host --device "$loop")
[[ -n "${EXTRA_CA:-}" ]] && args+=(-v "$EXTRA_CA":/usr/local/share/ca-certificates/extra.crt:ro)
agent_env=''
if [[ -n "${AGENT_BINARY:-}" ]]; then
  args+=(-v "$AGENT_BINARY":/opt/vercelify-agent-prebuilt:ro)
  agent_env='VERCELIFY_AGENT_BINARY=/opt/vercelify-agent-prebuilt'
fi
if [[ -n "${MESH_BINARY:-}" ]]; then
  args+=(-v "$MESH_BINARY":/opt/vercelify-mesh-prebuilt:ro)
  agent_env+=' VERCELIFY_MESH_BINARY=/opt/vercelify-mesh-prebuilt'
fi
# The mesh's WireGuard interface.
[[ -e /dev/net/tun ]] && args+=(--device /dev/net/tun)
docker run -d --name "$name" "${args[@]}" "$img" >/dev/null

x() { docker exec "$name" bash -o pipefail -c "$*"; }
for _ in $(seq 30); do x 'systemctl is-system-running 2>/dev/null | grep -qE "running|degraded"' && break; sleep 1; done
x 'udevadm trigger --action=add >/dev/null 2>&1; udevadm settle' || true
[[ -n "${EXTRA_CA:-}" ]] && x 'update-ca-certificates >/dev/null 2>&1'
if [[ -n "${REGISTRY_MIRROR:-}" ]]; then
  x "mkdir -p /etc/docker && printf '{\"registry-mirrors\":[\"%s\"]}\n' '$REGISTRY_MIRROR' > /etc/docker/daemon.json"
fi

echo "==> running the installer (this pulls Docker and the proxy; several minutes)"
start=$SECONDS
x "cd /src && NO_COLOR=1 $agent_env bash host/setup-host.sh -y --hostname it-host --storage-device $loop" 2>&1 | tee /tmp/vercelify-it.log | grep -E '^==>|✓|!|✗' || true
echo "    installer finished in $((SECONDS - start))s"

fail=0
check() { if x "$2" >/dev/null 2>&1; then echo "  ✓ $1"; else echo "  ✗ $1"; fail=1; fi; }
echo "==> assertions"
check "storage mounted at /mnt/storage by UUID with nofail" 'findmnt -no SOURCE /mnt/storage && grep -E "^UUID=.* /mnt/storage ext4 .*nofail" /etc/fstab'
check "volume + backup layout on the HDD" 'test -d /mnt/storage/volumes && test -d /mnt/storage/backups'
check "swappiness 10" '[[ $(sysctl -n vm.swappiness) == 10 ]]'
check "lid switch ignored" 'grep -q HandleLidSwitch=ignore /etc/systemd/logind.conf.d/90-vercelify.conf'
check "docker running with log rotation" 'docker info >/dev/null && jq -e ".\"log-opts\".\"max-size\"==\"10m\"" /etc/docker/daemon.json'
check "agent healthy on localhost" 'curl -fsS http://127.0.0.1:7420/v1/health'
check "agent rejects missing token" '[[ $(curl -s -o /dev/null -w "%{http_code}" http://127.0.0.1:7420/v1/system) == 401 ]]'
check "agent sees the HDD tier" 'curl -fsS -H "Authorization: Bearer $(cat /etc/vercelify/agent.token)" http://127.0.0.1:7420/v1/system | jq -e "any(.disks[]; .role==\"storage\")"'
check "power guard running and writing state" 'systemctl is-active --quiet vercelify-power-guard && jq -e .state /run/vercelify/power-guard.state'
check "firewall chains installed" 'iptables -n -L VERCELIFY-FWD && iptables -n -L VERCELIFY-IN'
# On the mesh, pairing codes are vfy2. (they carry the mesh join token).
check "vercelify CLI works" 'vercelify status && vercelify pair | grep -q "^vfy2\."'
check "summary written 0600" '[[ $(stat -c %a /etc/vercelify/summary.txt) == 600 ]]'
T='-H "Authorization: Bearer $(cat /etc/vercelify/agent.token)"'
check "the agent runs Vercelify's engine" "curl -fsS $T http://127.0.0.1:7420/v1/health | jq -e '.capabilities | index(\"platform\")'"
check "no Coolify, no Tailscale" '! docker inspect coolify >/dev/null 2>&1 && ! command -v tailscale'
check "the mesh: this server coordinates it as 10.77.0.1" 'systemctl is-active --quiet vercelify-mesh && ip -4 addr show vfy0 | grep -q "inet 10.77.0.1/"'
check "the agent listens on the mesh" 'curl -fsS http://10.77.0.1:7420/v1/health'
check "apps get <name>.10.77.0.1.sslip.io" "curl -fsS $T http://127.0.0.1:7420/v1/platform/settings/domain | jq -e '.wildcard == \"10.77.0.1.sslip.io\"'"
check "an invite for a worker" 'vercelify mesh invite | grep -q "mesh join vfyj1\..* --platform"'
echo "==> deploying an app"
x "curl -fsS $T -H 'Content-Type: application/json' -d '{\"name\":\"it-web\",\"kind\":\"image\",\"source\":{\"image\":\"traefik/whoami:v1.11\"},\"runtime\":{\"port\":80,\"health_timeout_secs\":90,\"drain_secs\":5}}' http://127.0.0.1:7420/v1/platform/apps >/dev/null" || fail=1
x "curl -fsS $T -X POST -H 'Content-Type: application/json' -d '{}' http://127.0.0.1:7420/v1/platform/apps/it-web/deploy >/dev/null" || fail=1
for _ in $(seq 60); do x "curl -fsS -H 'Host: it-web.10.77.0.1.sslip.io' http://127.0.0.1/ | grep -q Hostname" 2>/dev/null && break; sleep 3; done
check "the proxy serves it at it-web.10.77.0.1.sslip.io" "curl -fsS -H 'Host: it-web.10.77.0.1.sslip.io' http://127.0.0.1/ | grep -q Hostname"

echo "==> re-running the installer (must be idempotent)"
if x "cd /src && NO_COLOR=1 $agent_env bash host/setup-host.sh -y --hostname it-host --storage-device $loop" >/tmp/vercelify-it-2.log 2>&1; then
  echo "  ✓ second run succeeded"
  check "fstab has exactly one storage entry" '[[ $(grep -c " /mnt/storage ext4 " /etc/fstab) == 1 ]]'
else
  echo "  ✗ second run failed (see /tmp/vercelify-it-2.log)"; fail=1
fi

served='curl -fsS -H "Host: it-web.10.77.0.1.sslip.io" http://127.0.0.1/ | grep -q Hostname'
healthy='curl -fsS http://127.0.0.1:7420/v1/health'
echo "==> update, a failed update, rollback"
# `vercelify update` runs the installer it is given from a copy: this one runs
# the checkout's install.sh in place, so the update installs this tree.
x 'printf "#!/usr/bin/env bash\nexec bash /src/install.sh \"\$@\"\n" >/tmp/installer-local.sh'
# An update whose agent never comes up.
x 'printf "#!/usr/bin/env bash\ninstall -m 0755 /bin/false /usr/local/bin/vercelify-agent\nsystemctl restart vercelify-agent\n" >/tmp/installer-broken.sh'
if x "NO_COLOR=1 VERCELIFY_INSTALLER=/tmp/installer-local.sh $agent_env vercelify update" >/tmp/vercelify-it-update.log 2>&1; then
  echo "  ✓ update succeeded"
else
  echo "  ✗ update failed (see /tmp/vercelify-it-update.log)"; fail=1
fi
check "after the update: agent healthy, app served, previous version kept" "$healthy && $served && test -x /usr/local/lib/vercelify/rollback/vercelify-agent"
if x "NO_COLOR=1 UPDATE_HEALTH_SECS=20 VERCELIFY_INSTALLER=/tmp/installer-broken.sh vercelify update" >/tmp/vercelify-it-update-bad.log 2>&1; then
  echo "  ✗ a broken update reported success"; fail=1
else
  echo "  ✓ a broken update fails"
fi
check "a broken update puts the previous agent back" "! cmp -s /bin/false /usr/local/bin/vercelify-agent && $healthy && $served"
grep -q 'rolled back; the previous version is running again' /tmp/vercelify-it-update-bad.log ||
  { echo "  ✗ the broken update didn't say it rolled back (see /tmp/vercelify-it-update-bad.log)"; fail=1; }
check "vercelify rollback" "vercelify rollback && $healthy && $served"

echo "==> uninstall, then install again"
x "ASSUME_YES=1 NO_COLOR=1 vercelify uninstall" >/tmp/vercelify-it-uninstall.log 2>&1 || { echo "  ✗ uninstall failed (see /tmp/vercelify-it-uninstall.log)"; fail=1; }
# vercelify.slice stays: the apps' containers keep running in it, memory-capped.
check "no Vercelify services or timers left" '! systemctl list-unit-files "vercelify*" --no-legend | grep -v "^vercelify.slice " | grep -q .'
check "agent, mesh and CLI binaries removed" '! test -e /usr/local/bin/vercelify-agent && ! test -e /usr/local/bin/vercelify-mesh && ! command -v vercelify'
check "firewall chains removed" '! iptables -n -L VERCELIFY-IN && ! iptables -n -L VERCELIFY-FWD'
check "the mesh interface is gone" '! ip link show vfy0'
check "config, the engine's state and the app's container kept" 'test -s /etc/vercelify/agent.token && test -s /var/lib/vercelify/platform.db && docker ps -a --filter label=vercelify.app-name=it-web -q | grep -q .'
check "the app keeps running in its memory-capped slice" "systemctl cat vercelify.slice | grep -q MemoryMax= && $served"
if x "cd /src && NO_COLOR=1 $agent_env bash host/setup-host.sh -y --hostname it-host --storage-device $loop" >/tmp/vercelify-it-3.log 2>&1; then
  echo "  ✓ installed again"
else
  echo "  ✗ the install after uninstall failed (see /tmp/vercelify-it-3.log)"; fail=1
fi
for _ in $(seq 40); do x "$served" 2>/dev/null && break; sleep 3; done
check "the app is back, from the kept state" "curl -fsS $T http://127.0.0.1:7420/v1/platform/apps | jq -e 'any(.[]; .name == \"it-web\")' && $served"
exit $fail

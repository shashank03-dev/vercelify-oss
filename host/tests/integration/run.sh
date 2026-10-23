#!/usr/bin/env bash
# Full installer integration test in a privileged systemd container.
#
#   sudo host/tests/integration/run.sh
#
# Runs the real host/setup-host.sh (no dry run): packages, kernel tuning, an
# ext4 "HDD" on a loop device mounted at /mnt/storage, Docker, Tailscale
# (not logged in), Coolify, agent, power guard, firewall and CLI, then checks
# the result with `vercelify doctor`-style assertions.
#
# Env: EXTRA_CA (PEM bundle to trust inside, for TLS-intercepting proxies),
#      REGISTRY_MIRROR (e.g. https://mirror.gcr.io), KEEP=1 (leave it running),
#      AGENT_BINARY (prebuilt x86_64 musl agent; otherwise the installer
#      downloads a release or builds from source).
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
docker run -d --name "$name" "${args[@]}" "$img" >/dev/null

x() { docker exec "$name" bash -o pipefail -c "$*"; }
for _ in $(seq 30); do x 'systemctl is-system-running 2>/dev/null | grep -qE "running|degraded"' && break; sleep 1; done
x 'udevadm trigger --action=add >/dev/null 2>&1; udevadm settle' || true
[[ -n "${EXTRA_CA:-}" ]] && x 'update-ca-certificates >/dev/null 2>&1'
if [[ -n "${REGISTRY_MIRROR:-}" ]]; then
  x "mkdir -p /etc/docker && printf '{\"registry-mirrors\":[\"%s\"]}\n' '$REGISTRY_MIRROR' > /etc/docker/daemon.json"
fi

# Test-harness-only workaround for kernels without IPv6 (Coolify's nginx
# listens on [::]). Real hosts have IPv6; the installer warns otherwise.
if [[ ! -e /proc/net/if_inet6 ]]; then
  docker exec -i "$name" bash -c 'cat > /usr/local/bin/it-ipv6-workaround' <<'SH'
#!/bin/sh
until docker exec coolify true 2>/dev/null; do sleep 2; done
docker exec -u root coolify sh -c 'for f in /etc/nginx/site-opts.d/*.conf* /etc/nginx/sites-available/*; do sed -i "/listen \[::\]/d" "$f"; done'
SH
  x 'chmod +x /usr/local/bin/it-ipv6-workaround && (nohup /usr/local/bin/it-ipv6-workaround >/dev/null 2>&1 &)'
fi

echo "==> running the installer (this pulls Docker, Tailscale and Coolify; several minutes)"
start=$SECONDS
x "cd /src && NO_COLOR=1 $agent_env bash host/setup-host.sh -y --hostname it-host --storage-device $loop" 2>&1 | tee /tmp/vercelify-it.log | grep -E '^==>|✓|!|✗' || true
echo "    installer finished in $((SECONDS - start))s"

fail=0
check() { if x "$2" >/dev/null 2>&1; then echo "  ✓ $1"; else echo "  ✗ $1"; fail=1; fi; }
echo "==> assertions"
check "storage mounted at /mnt/storage by UUID with nofail" 'findmnt -no SOURCE /mnt/storage && grep -E "^UUID=.* /mnt/storage ext4 .*nofail" /etc/fstab'
check "volume + backup layout on the HDD" 'test -d /mnt/storage/volumes && test -d /mnt/storage/backups/coolify'
check "swappiness 10" '[[ $(sysctl -n vm.swappiness) == 10 ]]'
check "lid switch ignored" 'grep -q HandleLidSwitch=ignore /etc/systemd/logind.conf.d/90-vercelify.conf'
check "docker running with log rotation" 'docker info >/dev/null && jq -e ".\"log-opts\".\"max-size\"==\"10m\"" /etc/docker/daemon.json'
check "tailscaled running" 'systemctl is-active --quiet tailscaled'
check "Coolify healthy" '[[ $(curl -fsS http://127.0.0.1:8000/api/health) == OK ]]'
check "Coolify SSH user is root" '[[ $(docker exec coolify-db psql -U coolify -d coolify -tAc "select \"user\" from servers where id=0") == root ]]'
check "Coolify API token works" 'curl -fsS -H "Authorization: Bearer $(cat /etc/vercelify/coolify.token)" http://127.0.0.1:8000/api/v1/version'
check "preview URL template default" 'docker exec coolify-db psql -U coolify -d coolify -c "\d applications" | grep -F "pr-{{pr_id}}-{{domain}}"'
check "agent healthy on localhost" 'curl -fsS http://127.0.0.1:7420/v1/health'
check "agent rejects missing token" '[[ $(curl -s -o /dev/null -w "%{http_code}" http://127.0.0.1:7420/v1/system) == 401 ]]'
check "agent sees the HDD tier" 'curl -fsS -H "Authorization: Bearer $(cat /etc/vercelify/agent.token)" http://127.0.0.1:7420/v1/system | jq -e "any(.disks[]; .role==\"storage\")"'
check "power guard running and writing state" 'systemctl is-active --quiet vercelify-power-guard && jq -e .state /run/vercelify/power-guard.state'
check "firewall chains installed" 'iptables -n -L VERCELIFY-FWD && iptables -n -L VERCELIFY-IN'
check "vercelify CLI works" 'vercelify status && vercelify pair | grep -q "^vfy1\."'
check "summary written 0600" '[[ $(stat -c %a /etc/vercelify/summary.txt) == 600 ]]'

echo "==> re-running the installer (must be idempotent)"
if x "cd /src && NO_COLOR=1 $agent_env bash host/setup-host.sh -y --hostname it-host --storage-device $loop" >/tmp/vercelify-it-2.log 2>&1; then
  echo "  ✓ second run succeeded"
  check "fstab has exactly one storage entry" '[[ $(grep -c " /mnt/storage ext4 " /etc/fstab) == 1 ]]'
else
  echo "  ✗ second run failed (see /tmp/vercelify-it-2.log)"; fail=1
fi
exit $fail

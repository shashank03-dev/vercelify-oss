#!/usr/bin/env bash
# Real systemd ordering, using exclusively owned units; never stops real Docker.
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'run as root on an isolated systemd host'; exit 1; }
repo=$(cd "$(dirname "$0")/../../.." && pwd)
work=$(mktemp -d)
prefix=vfy-ordering-$$
slice=$prefix.slice
service=$prefix.service
cleanup() {
  systemctl stop "$service" "$slice" >/dev/null 2>&1 || true
  rm -f "/run/systemd/system/$slice" "/run/systemd/system/$service"
  systemctl daemon-reload
  rm -rf "$work"
}
trap cleanup EXIT
export VERCELIFY_SOURCE_ONLY=1 VERCELIFY_DRY_RUN=0 NO_COLOR=1
export SYSTEMD_DIR=$work VERCELIFY_LOG=$work/log
# shellcheck source=../../setup-host.sh
. "$repo/host/setup-host.sh"
mem_total_mb() { echo 4096; }
run_cmd() { :; }
export HW_PROFILE=laptop
container_slice >/dev/null
# Substitute only the dependency target; exercise the generated slice unchanged.
sed "s/docker.service/$service/g" "$work/vercelify.slice" > "/run/systemd/system/$slice"
cat > "/run/systemd/system/$service" <<UNIT
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/true
ExecStop=/bin/sh -c 'sleep 1; systemctl is-active --quiet $slice && echo alive > $work/result'
UNIT
systemctl daemon-reload
systemctl start "$slice" "$service"
systemctl stop "$service" "$slice"
[[ -s "$work/result" ]] || { echo 'FAIL: workload slice stopped before the daemon completed shutdown'; exit 1; }
echo 'PASS: workload slice stays active until daemon shutdown completes'

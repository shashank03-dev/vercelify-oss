#!/usr/bin/env bash
# Real systemd ordering, using exclusively owned units; never stops real Docker.
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'run as root on an isolated systemd host'; exit 1; }
repo=$(cd "$(dirname "$0")/../../.." && pwd)
work=$(mktemp -d)
prefix=vfy-ordering-$$
slice=$prefix.slice
service=$prefix.service
scope=$prefix-container.scope
cleanup() {
  systemctl stop "$service" "$scope" "$slice" >/dev/null 2>&1 || true
  rm -f "/run/systemd/system/$slice" "/run/systemd/system/$service"
  rm -rf "/run/systemd/system/$prefix-.scope.d"
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
if [[ -f "$work/docker-.scope.d/90-vercelify-ordering.conf" ]]; then
  mkdir -p "/run/systemd/system/$prefix-.scope.d"
  sed "s/docker.service/$service/g" "$work/docker-.scope.d/90-vercelify-ordering.conf" > "/run/systemd/system/$prefix-.scope.d/90-vercelify-ordering.conf"
fi
cat > "/run/systemd/system/$service" <<UNIT
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/true
ExecStop=/bin/sh -c 'sleep 1; systemctl is-active --quiet $slice && systemctl is-active --quiet $scope && echo alive > $work/result'
UNIT
systemctl daemon-reload
systemctl start "$slice" "$service"
systemd-run --scope --unit="$scope" -p "Slice=$slice" -p DefaultDependencies=no /bin/sleep 300 &
scope_runner=$!
for ((i=0;i<50;i++)); do
  systemctl is-active --quiet "$scope" && break
  sleep .1
done
systemctl is-active --quiet "$scope"
systemctl stop "$service" "$scope" "$slice"
wait "$scope_runner" || true
[[ -s "$work/result" ]] || { echo 'FAIL: workload scope/slice stopped before daemon shutdown completed'; exit 1; }
echo 'PASS: workload scope and slice stay active until daemon shutdown completes'

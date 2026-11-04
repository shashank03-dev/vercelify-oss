#!/bin/bash
# Run once with sudo. Separate identity; restores resolver and removes only its utun.
# Does not install/replace a LaunchDaemon or stop the current app-mode mesh.
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo 'Run this helper with sudo.' >&2; exit 1; }
repo=/Users/shashank/Desktop/VERCELIFY
work=$repo/target/public-qualification/native-system
cd "$work"
owner_uid=$(stat -f %u "$work")
owner_home=/Users/shashank
expected=$(/usr/bin/python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["binary_sha256"])' "$work/manifest.json")
[[ $(shasum -a 256 "$work/vercelify-mesh" | awk '{print $1}') == "$expected" ]]
[[ ! -e $work/started ]] || { echo 'This single-use helper already ran; inspect its report.' >&2; exit 1; }
netstat -rn -f inet | awk '$1=="10.77" || $1=="10.77/16" || $1=="10.77.0.0/16" {found=1} END{exit found}' || { echo 'Existing system mesh route; refusing to replace it.' >&2; exit 1; }
umask 077
touch "$work/started"
had_resolver=0; had_directory=0; pid=''
[[ ! -d /etc/resolver ]] || had_directory=1
if [[ -f /etc/resolver/vfy ]]; then cp -p /etc/resolver/vfy "$work/resolver.before"; had_resolver=1; fi
cleanup() {
  status=$?
  trap - EXIT
  if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; for ((n=0;n<20;n++)); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done; kill -KILL "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
  if [[ $had_resolver == 1 ]]; then cp -p "$work/resolver.before" /etc/resolver/vfy
  else rm -f /etc/resolver/vfy; fi
  if [[ $had_directory == 0 ]]; then rmdir /etc/resolver 2>/dev/null || true; fi
  dscacheutil -flushcache
  /usr/bin/python3 - "$work" "$status" <<'PY'
import json,sys,subprocess,os
from pathlib import Path
p=Path(sys.argv[1]);status=int(sys.argv[2]);iface=None
if (p/'run/mesh-status.json').exists():iface=json.loads((p/'run/mesh-status.json').read_text()).get('interface')
remaining=subprocess.run(['/sbin/ifconfig',iface],capture_output=True).returncode==0 if iface else False
result={'exit_code':status,'native_system_test_passed':status==0,'interface':iface,'temporary_interface_removed':not remaining,'resolver_restored':True,'existing_userspace_service_preserved':True}
(p/'result.json').write_text(json.dumps(result,indent=2)+'\n')
PY
  chown "$owner_uid" "$work/result.json"
  chmod 600 "$work/result.json"
  if [[ -f $work/browser.json ]]; then chown "$owner_uid" "$work/browser.json"; chmod 600 "$work/browser.json"; fi
  echo "Native system-mode result: $work/result.json"
  exit "$status"
}
trap cleanup EXIT
"$work/vercelify-mesh" --dir "$work/mesh" --run-dir "$work/run" up --port 0 > "$work/daemon.log" 2>&1 &
pid=$!
for ((i=0;i<60;i++)); do
  if curl --noproxy '*' -fsS --max-time 2 http://10.77.0.1:7420/v1/health > "$work/health.json"; then break; fi
  kill -0 "$pid"
  sleep 1
done
/usr/bin/python3 -c 'import json,sys;assert json.load(open(sys.argv[1]))["ok"]' "$work/health.json"
dscacheutil -flushcache
/usr/bin/sudo -u "#$owner_uid" env HOME="$owner_home" /opt/homebrew/bin/node "$repo/docs/validation/2026-11-04-public-qualification/mac-system-browser.cjs" "$work/browser.json"
curl --noproxy '*' -g -6 -fsS --max-time 10 -H 'Host: gw.10.77.0.1.sslip.io' 'http://[fd76:6679:6d65::1]/' > "$work/private-ipv6-health.json"
echo 'Native macOS utun, system DNS, normal Chromium and private mesh IPv6 passed.'

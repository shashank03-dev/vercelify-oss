"""Reclaim only unused vfy fixture bridges inside the disposable daemon.

Stopped-container attachments are included; never use Docker's global prune.
"""
import json,subprocess
prefix=['limactl','shell','vfybench','--','sudo','docker','exec','vfy-m1-contract-docker']
def run(args):return subprocess.run(prefix+args,check=True,capture_output=True,text=True).stdout.strip()
run(['test','!','-e','/etc/vercelify'])
containers=run(['docker','ps','-aq']).split();used=set()
if containers:
 for line in run(['docker','inspect','--format','{{json .NetworkSettings.Networks}}',*containers]).splitlines():used.update(json.loads(line))
ids=run(['docker','network','ls','-q']).split()
networks=json.loads(run(['docker','network','inspect',*ids]))
removed=[]
for net in networks:
 name=net['Name'];labels=net.get('Labels') or {}
 if name not in used and (labels.get('vercelify.managed')=='platform' or name.startswith(('vfy-net-','vfy-preview-'))):
  run(['docker','network','rm',net['Id']]);removed.append(name)
print(json.dumps({'removed_unused_fixture_networks':removed,'preserved_attached_networks':len(used)},indent=2))

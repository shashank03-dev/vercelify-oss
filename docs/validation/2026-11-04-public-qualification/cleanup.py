"""Remove only the named owned fixtures and revoke only the test mesh identity."""
from pathlib import Path
import json,subprocess,os,signal
out=Path(__file__).parent;private=json.loads((out/'private-probe.json').read_text())
for key,expected in [('cloudflared_pid','cloudflared'),('ssh_pid','ssh')]:
 pid=private[key];r=subprocess.run(['ps','-p',str(pid),'-o','comm='],capture_output=True,text=True)
 if r.returncode==0:
  assert expected in r.stdout,(pid,r.stdout);os.kill(pid,signal.SIGTERM)
owner=private['owner_container'];assert owner=='vfy-final-qualification-docker'
prefix=['limactl','shell','vfybench','--','sudo','docker']
subprocess.run(prefix+['exec','-e','VERCELIFY_GUEST_CONTRACT=1',owner,'python3','/workspace/host/tests/contract/guest-links.py','stop'],check=True,stdout=subprocess.DEVNULL)
subprocess.run(prefix+['stop',owner],check=True,stdout=subprocess.DEVNULL)
node=json.loads((out/'native-system-prepared.json').read_text())['node_id']
remote='import json\nnode='+repr(node)+'\n'+r'''
from pathlib import Path
import urllib.request,subprocess,time,os,signal
root=Path('/var/lib/vercelify/qualification-public-20261104');s=json.loads((root/'owned.json').read_text());token=Path('/etc/vercelify/agent.token').read_text().strip()
def api(method,path):
 r=urllib.request.Request('http://127.0.0.1:7420/v1'+path,method=method,headers={'Authorization':'Bearer '+token});return json.load(urllib.request.urlopen(r,timeout=30))
for name in ['tunnel.pid','tunnel-default.pid']:
 p=root/name
 if p.exists():
  pid=int(p.read_text());cmd=Path('/proc/'+str(pid)+'/cmdline')
  if cmd.exists():assert str(root/'cloudflared') in cmd.read_bytes().decode(errors='replace');os.kill(pid,signal.SIGTERM)
assert s['name']=='public-network-qualification'
api('DELETE','/platform/apps/'+s['app_id']);api('DELETE','/platform/projects/'+s['project_id']);api('DELETE','/mesh/nodes/'+node)
for _ in range(60):
 r=subprocess.check_output(['docker','ps','-aq','--filter','label=vercelify.app='+s['app_id']],text=True).strip()
 if not r:break
 time.sleep(1)
else:raise RuntimeError('owned app cleanup deadline')
(root/'cleaned.json').write_text(json.dumps({'app':s['app_id'],'project':s['project_id'],'revoked_test_node':node}))
print(json.dumps({'physical_app_removed':True,'physical_project_removed':True,'physical_tunnels_stopped':True,'test_node_revoked':node}))
'''
with (out/'cleanup.json').open('w') as log:subprocess.run(['/Applications/Tailscale.app/Contents/MacOS/Tailscale','ssh','user@100.123.113.77','sudo -n python3 -'],input=remote,text=True,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=120)

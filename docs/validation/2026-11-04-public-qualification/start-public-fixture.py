"""Create only an isolated public qualification project/app and temporary tunnel."""
from pathlib import Path
import subprocess,json
out=Path(__file__).parent
asset=json.loads(Path('/tmp/vfy-cloudflared-asset.json').read_text());(out/'cloudflared-asset.json').write_text(json.dumps(asset,indent=2)+'\n')
server=Path('host/tests/contract/guest_app.mjs').read_text().replace('Guest fixture','Public Internet qualification fixture e8fb366')
remote='import json\nctx=json.loads('+repr(json.dumps({'asset':asset,'server':server}))+')\n'+r'''
from pathlib import Path
import urllib.request,subprocess,time,hashlib,os
root=Path('/var/lib/vercelify/qualification-public-20261104');root.mkdir(mode=0o700,exist_ok=True)
assert not (root/'owned.json').exists(),'existing fixture must be cleaned before restarting'
token=Path('/etc/vercelify/agent.token').read_text().strip()
def api(method,path,body=None):
 r=urllib.request.Request('http://127.0.0.1:7420/v1/platform'+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
 return json.load(urllib.request.urlopen(r,timeout=30))
project=api('POST','/projects',{'name':'public-network-qualification'})
app=api('POST','/apps',{'name':'public-network-qualification','project_id':project['id'],'kind':'image','source':{'image':'node:22-slim'},'runtime':{'command':['node','--input-type=module','-e',ctx['server']],'port':8080,'memory_mb':128,'cpus':0.5,'health_timeout_secs':180,'health':{'path':'/health'},'drain_secs':0},'domains':[]})
owned={'project_id':project['id'],'app_id':app['id'],'name':app['name'],'url':app['url']};(root/'owned.json').write_text(json.dumps(owned));(root/'owned.json').chmod(0o600)
binary=root/'cloudflared';subprocess.run(['curl','--retry','2','--max-time','180','-fsSL',ctx['asset']['browser_download_url'],'-o',str(binary)],check=True)
assert hashlib.sha256(binary.read_bytes()).hexdigest()==ctx['asset']['digest'].split(':')[1];binary.chmod(0o755)
d=api('POST','/apps/'+app['id']+'/deploy',{})
for _ in range(240):
 d=api('GET','/deployments/'+d['id'])
 if d['status']=='healthy':break
 assert d['status'] not in ['failed','cancelled'],d.get('error');time.sleep(1)
else:raise RuntimeError('fixture readiness deadline')
host=app['url'].split('://')[1].split('/')[0]
log=(root/'tunnel.log').open('wb');p=subprocess.Popen([str(binary),'tunnel','--url','http://127.0.0.1:80','--http-host-header',host,'--protocol','http2','--edge-ip-version','4','--no-autoupdate'],stdout=log,stderr=log,start_new_session=True);log.close();(root/'tunnel.pid').write_text(str(p.pid))
import re
for _ in range(90):
 s=(root/'tunnel.log').read_text(errors='replace');matches=re.findall(r'https://[a-z0-9-]+\.trycloudflare\.com',s)
 if matches:owned['tunnel_url']=matches[0];break
 assert p.poll() is None,'cloudflared exited';time.sleep(1)
else:raise RuntimeError('quick tunnel hostname deadline')
(root/'owned.json').write_text(json.dumps(owned));owned['cloudflared_sha256']=ctx['asset']['digest'];print(json.dumps(owned))
'''
with (out/'public-fixture.json').open('w') as log:subprocess.run(['/Applications/Tailscale.app/Contents/MacOS/Tailscale','ssh','user@100.123.113.77','sudo -n python3 -'],input=remote,text=True,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=600)

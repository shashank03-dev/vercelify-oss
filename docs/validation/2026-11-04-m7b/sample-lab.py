"""Read-only matched six-app measurement; call with disposable daemon stopped."""
import json
from pathlib import Path
import re
import statistics
import subprocess
import time
import urllib.request

out = Path('docs/validation/2026-11-04-m7b')
time.sleep(120)
# The private pairing token stays local and is never included in evidence.
settings=json.loads(Path('target/offhost-small-vm/settings.json').read_text())
token=settings['servers'][0]['agent_token']
request=urllib.request.Request('http://127.0.0.1:17427/v1/platform/apps',headers={'Authorization':'Bearer '+token})
apps=json.load(urllib.request.urlopen(request,timeout=15))
original=json.loads(Path('target/offhost-small-vm/m4-original-apps.json').read_text())
assert {a['id'] for a in apps}=={a['id'] for a in original}
probes=[]
for app in original:
    request=urllib.request.Request('http://127.0.0.1:17429/',headers={'Host':app['name']+'.10.77.0.1.sslip.io'})
    with urllib.request.urlopen(request,timeout=10) as response:
        assert response.status==200
        response.read()
        probes.append({'app':app['name'],'status':response.status})
(out/'original-apps.json').write_text(json.dumps(probes,indent=2)+'\n')
result = subprocess.run(['limactl','shell','vfybench','--','sudo','bash',
    '/Users/shashank/Desktop/VERCELIFY/docs/validation/2026-11-02-m1/sample-memory.sh'],
    check=True,capture_output=True,text=True)
(out/'vm-steady.txt').write_text(result.stdout)
samples=re.split(r'^sample \d+ ',result.stdout,flags=re.M)[1:]
assert len(samples)==12
used=[]
rss={name:[] for name in ['vercelify-agent','vercelify-mesh','traefik','dockerd','containerd','containerd-shim','buildkitd']}
for sample in samples:
    used.append(int(re.search(r'^Mem:\s+\d+\s+(\d+)',sample,re.M)[1]))
    entries=dict((name,int(size)/1024) for name,size in re.findall(r'^rss_kib (\S+) (\d+)',sample,re.M))
    for name in rss: rss[name].append(entries.get(name,0))
def summary(values): return {'median':statistics.median(values),'min':min(values),'max':max(values)}
data={'samples':len(samples),'system_used_mib':summary(used),'rss_mib':{name:summary(values) for name,values in rss.items()}}
(out/'vm-steady.json').write_text(json.dumps(data,indent=2)+'\n')
print(json.dumps(data))

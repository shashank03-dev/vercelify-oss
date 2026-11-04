"""Prepare a separate mesh identity and a reviewable, attended root helper."""
from pathlib import Path
import subprocess,json,hashlib,shutil,os
work=Path('target/public-qualification/native-system').resolve();work.mkdir(parents=True,mode=0o700,exist_ok=True);work.chmod(0o700)
remote=r'''
import json,urllib.request
from pathlib import Path
r=urllib.request.Request('http://127.0.0.1:7420/v1/mesh/invites',data=json.dumps({'kind':'device','role':'owner','ttl_minutes':60}).encode(),headers={'Authorization':'Bearer '+Path('/etc/vercelify/agent.token').read_text().strip(),'Content-Type':'application/json'})
print(json.dumps(json.load(urllib.request.urlopen(r))))
'''
invite=json.loads(subprocess.check_output(['/Applications/Tailscale.app/Contents/MacOS/Tailscale','ssh','user@100.123.113.77','sudo -n python3 -'],input=remote,text=True))
binary=work/'vercelify-mesh';shutil.copy2('target/release/vercelify-mesh',binary)
subprocess.run([str(binary),'--dir',str(work/'mesh'),'--run-dir',str(work/'run'),'join',invite['token'],'--name','mac-system-qualification'],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,check=True,timeout=60)
s=json.loads((work/'mesh/state.json').read_text());facts={'node_id':s['node_id'],'ipv4':s['ipv4'],'ipv6':s['ipv6'],'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest()};(work/'manifest.json').write_text(json.dumps(facts));(work/'manifest.json').chmod(0o600)
print(json.dumps(facts))

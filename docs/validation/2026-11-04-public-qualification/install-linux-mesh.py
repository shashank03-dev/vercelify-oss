"""Atomic mesh-only update on the authorized host, preserving identity/workloads."""
from pathlib import Path
import subprocess,hashlib,json
out=Path(__file__).parent;binary=Path('target/x86_64-unknown-linux-musl/release/vercelify-mesh');data=binary.read_bytes();sha=hashlib.sha256(data).hexdigest()
ssh=['/Applications/Tailscale.app/Contents/MacOS/Tailscale','ssh','user@100.123.113.77']
subprocess.run(ssh+["sudo -n sh -c 'umask 077; cat > /var/lib/vercelify/qualification-public-20261104/vercelify-mesh.final'"],input=data,check=True,timeout=120)
expected=json.loads(Path('docs/validation/2026-11-04-final-qualification/laptop-final-status.json').read_text())['protected_containers']
remote='import json\nsha='+repr(sha)+'\nexpected=json.loads('+repr(json.dumps(expected))+')\n'+r'''
from pathlib import Path
import subprocess,hashlib,shutil,os,time
root=Path('/var/lib/vercelify/qualification-public-20261104');candidate=root/'vercelify-mesh.final';assert hashlib.sha256(candidate.read_bytes()).hexdigest()==sha
binary=Path('/usr/local/bin/vercelify-mesh');backup=root/'mesh-before';backup.mkdir(mode=0o700);shutil.copy2(binary,backup/'vercelify-mesh');state=Path('/var/lib/vercelify/mesh/state.json');before=json.loads(state.read_text());key=Path('/var/lib/vercelify/mesh/node.key');key_hash=hashlib.sha256(key.read_bytes()).hexdigest();shutil.copytree(state.parent,backup/'state')
tmp=binary.with_name(binary.name+'.qualification-new');shutil.copyfile(candidate,tmp);tmp.chmod(0o755);os.replace(tmp,binary)
try:
 subprocess.run(['systemctl','restart','vercelify-mesh'],check=True);time.sleep(5)
 subprocess.run(['systemctl','is-active','--quiet','vercelify-mesh'],check=True)
 after=json.loads(state.read_text());assert all(before[k]==after[k] for k in ['node_id','ipv4','ipv6']);assert hashlib.sha256(key.read_bytes()).hexdigest()==key_hash
 rows=json.loads(subprocess.check_output(['docker','inspect',*expected]));current={r['Name'].lstrip('/'):{'id':r['Id'],'running':r['State']['Running'],'restarts':r['RestartCount']} for r in rows};assert current==expected
 print(json.dumps({'installed_mesh_sha256':sha,'identity_and_key_preserved':True,'protected_containers_unchanged':True,'backup':str(backup)}))
except BaseException:
 shutil.copy2(backup/'vercelify-mesh',binary);subprocess.run(['systemctl','restart','vercelify-mesh'],check=True);raise
'''
with (out/'linux-mesh-install.json').open('w') as log:subprocess.run(ssh+['sudo -n python3 -'],input=remote,text=True,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=90)

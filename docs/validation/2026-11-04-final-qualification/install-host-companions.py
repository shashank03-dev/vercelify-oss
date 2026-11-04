"""Stage matching mesh/backup/guard files; retain host settings and identity."""
from pathlib import Path
import subprocess,json,base64
files={
 '/usr/local/lib/vercelify/lib/battery-policy.sh':('host/lib/battery-policy.sh',0o644),
 '/usr/local/lib/vercelify/power-guard.sh':('host/power-guard.sh',0o755),
 '/usr/local/lib/vercelify/lib/backup.sh':('host/lib/backup.sh',0o644),
}
payload={p:{'bytes':base64.b64encode(Path(src).read_bytes()).decode(),'mode':mode} for p,(src,mode) in files.items()}
remote='import json\nctx=json.loads('+repr(json.dumps(payload))+')\n'+r'''
from pathlib import Path
import os,shutil,subprocess,time,hashlib,base64
root=Path('/var/lib/vercelify/qualification-final-20261104T022650Z/host-companions-before');root.mkdir(mode=0o700)
mesh=Path('/usr/local/bin/vercelify-mesh');candidate=root.parent/'vercelify-mesh.fixed'
assert hashlib.sha256(candidate.read_bytes()).hexdigest()=='1e6352e1787c4332679d2ffee14d1a4a8071999e588d1ff935a1b7c1d344b15c'
state=Path('/var/lib/vercelify/mesh/state.json');before=json.loads(state.read_text());identity={k:before[k] for k in ['node_id','ipv4','ipv6']}
shutil.copytree(state.parent,root/'mesh-state');shutil.copy2(mesh,root/'vercelify-mesh')
for name in ['/etc/vercelify/power-guard.conf','/var/lib/vercelify/power-guard.local.conf']:
 p=Path(name)
 if p.exists():shutil.copy2(p,root/p.name)
active={name:subprocess.run(['systemctl','is-active',name],capture_output=True,text=True).stdout.strip()=='active' for name in ['vercelify-mesh','vercelify-power-guard']}
assert Path('/sys/class/power_supply/AC/online').read_text().strip()=='1','no guard restart without stable AC inventory'
installed={}
try:
 for target,spec in ctx.items():
  p=Path(target);p.parent.mkdir(parents=True,exist_ok=True)
  if p.exists():shutil.copy2(p,root/p.name)
  tmp=p.with_name(p.name+'.qualification-new');tmp.write_bytes(base64.b64decode(spec['bytes']));tmp.chmod(spec['mode']);os.replace(tmp,p);installed[target]=hashlib.sha256(p.read_bytes()).hexdigest()
 temporary=mesh.with_name(mesh.name+'.qualification-new');shutil.copyfile(candidate,temporary);temporary.chmod(0o755);os.replace(temporary,mesh)
 for name,was_active in active.items():
  if was_active:subprocess.run(['systemctl','restart',name],check=True)
 time.sleep(8)
 for name,was_active in active.items():
  if was_active:assert subprocess.run(['systemctl','is-active','--quiet',name]).returncode==0,name
 after=json.loads(state.read_text());assert {k:after[k] for k in identity}==identity
 print(json.dumps({'installed':installed,'mesh_sha256':hashlib.sha256(mesh.read_bytes()).hexdigest(),'identity_preserved':identity,'restarted_previously_active_services':active,'settings_retained':True,'backup':str(root)}))
except BaseException:
 for target in ctx:
  backup=root/Path(target).name
  if backup.exists():shutil.copy2(backup,target)
 shutil.copy2(root/'vercelify-mesh',mesh)
 for name,was_active in active.items():
  if was_active:subprocess.run(['systemctl','restart',name],check=True)
 raise
'''
with (Path(__file__).parent/'host-companions-install.txt').open('w') as log:
 subprocess.run(['/Applications/Tailscale.app/Contents/MacOS/Tailscale','ssh','user@100.123.113.77','sudo -n python3 -'],input=remote,text=True,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=120)

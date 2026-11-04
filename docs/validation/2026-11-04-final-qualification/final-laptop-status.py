"""Read-only final consistency check; never prints credentials/configuration values."""
from pathlib import Path
import subprocess,json
out=Path(__file__).parent
expected=json.loads((out/'final-binaries.json').read_text())['x86_64']
protected=json.loads((out/'laptop-install-final.txt').read_text())['protected_after']
remote='import json\nexpected=json.loads('+repr(json.dumps(expected))+')\nprotected=json.loads('+repr(json.dumps(protected))+')\n'+r'''
from pathlib import Path
import hashlib,subprocess,sqlite3,urllib.request
for name in ['agent','mesh']:
 assert hashlib.sha256(Path('/usr/local/bin/vercelify-'+name).read_bytes()).hexdigest()==expected[name]['sha256']
rows=json.loads(subprocess.check_output(['docker','inspect',*protected]));now={r['Name'].lstrip('/'):{'id':r['Id'],'running':r['State']['Running'],'restarts':r['RestartCount']} for r in rows};assert now==protected
services={name:subprocess.check_output(['systemctl','is-active',name],text=True).strip() for name in ['vercelify-agent','vercelify-mesh','vercelify-power-guard']};assert all(v=='active' for v in services.values())
with sqlite3.connect('file:/var/lib/vercelify/platform.db?mode=ro',uri=True) as db:
 schema=db.execute('pragma user_version').fetchone()[0];apps=db.execute('select name from apps order by name').fetchall();assert schema==13 and apps==[('gw',)]
strip=lambda p:[line for line in Path(p).read_text().splitlines() if not line.strip().startswith('mesh_public_url =')]
assert strip('/etc/vercelify/agent.toml')==strip('/var/lib/vercelify/qualification-m3-before/pre-crossbuild-agent.toml')
health=json.load(urllib.request.urlopen('http://127.0.0.1:7420/v1/health'));assert health['ok']
assert not any(Path('/var/lib/vercelify/build-uploads').glob('*'))
assert subprocess.run(['docker','inspect','vfy-finalq-pg17-source'],capture_output=True).returncode!=0
assert subprocess.run(['docker','volume','inspect','vfy-finalq-pg17-source-data'],capture_output=True).returncode!=0
print(json.dumps({'services':services,'schema':schema,'remaining_apps':[r[0] for r in apps],'protected_containers':now,'pre_m3_build_settings_restored':True,'final_binary_fingerprints_match':True,'upload_staging_empty':True,'owned_source_database_removed':True}))
'''
with (out/'laptop-final-status.json').open('w') as log:subprocess.run(['/Applications/Tailscale.app/Contents/MacOS/Tailscale','ssh','user@100.123.113.77','sudo -n python3 -'],input=remote,text=True,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=60)

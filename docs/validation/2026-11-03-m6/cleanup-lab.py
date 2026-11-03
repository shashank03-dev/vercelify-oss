"""Finish only the qualification resources recorded in the private journal."""
import ast,json,urllib.error
from pathlib import Path
path=Path('docs/validation/2026-11-03-m6/reboot-lab.py');tree=ast.parse(path.read_text())
# Load helpers without executing the destructive qualification again.
nodes=[]
for n in tree.body:
 if isinstance(n,(ast.Import,ast.ImportFrom,ast.FunctionDef)):nodes.append(n)
 elif isinstance(n,ast.Assign) and any(isinstance(v,ast.Name) and v.id in ['ROOT','OUT','PRIVATE','TOKEN'] for v in n.targets):nodes.append(n)
ns={};exec(compile(ast.Module(body=nodes,type_ignores=[]),str(path),'exec'),ns)
private=Path('target/offhost-small-vm/m6-fixture.json'); fixture=json.loads(private.read_text())
for app in fixture['apps']:
 try:ns['api']('/apps/'+app,method='DELETE')
 except urllib.error.HTTPError as e:
  if e.code!=404:raise
ns['until']('own containers removed',lambda:all(not ns['rows'](app) for app in fixture['apps']))
try:ns['api']('/projects/'+fixture['project'],method='DELETE')
except urllib.error.HTTPError as e:
 if e.code!=404:raise
apps=ns['api']('/apps');original=json.loads(Path('target/offhost-small-vm/m4-original-apps.json').read_text())
assert {a['id'] for a in apps}=={a['id'] for a in original}
for app in apps:ns['probe'](app['name']+'.10.77.0.1.sslip.io')
private.unlink()
Path('docs/validation/2026-11-03-m6/cleanup.json').write_text(json.dumps({'own_resources_removed':True,'original_six_routes':200,'inspection_race_retry':True},indent=2)+'\n')
print('Owned resources removed; original six routes HTTP 200.')

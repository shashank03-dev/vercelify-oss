from pathlib import Path
import time,json
exec(Path('target/guest-public-qualification/qualify.py').read_text().split("inv=issue();x=cloud('probe',inv)")[0])
inv=issue();cookie=redeem(inv);assert cloud('probe',inv,cookie)['root']['status']==200
restart="""import signal
pid=int((h['ROOT']/'pid').read_text());os.kill(pid,signal.SIGTERM)
for _ in range(60):
 p=Path('/proc/'+str(pid)+'/stat')
 if not p.exists() or p.read_text().split()[2]=='Z':break
 time.sleep(.1)
"""
inner(restart+"h['start_agent']()")
for _ in range(15):
 if cloud('probe',inv,cookie)['root']['status']==200:break
 time.sleep(1)
else:raise AssertionError('Public session did not survive agent restart')
rows=[{'case':'public-session-survives-agent-restart','passed':True}];print(json.dumps(rows[-1]),flush=True)
inner(restart+"key=h['ROOT']/'state/master.key';key.write_bytes(secrets.token_bytes(32));key.chmod(0o600)\nh['start_agent']()")
assert cloud('probe',inv,cookie)['root']['status']==401;rows.append({'case':'public-session-revoked-by-lab-master-key-replacement','passed':True});print(json.dumps(rows[-1]),flush=True)
for change in [{'previews':{'enabled':True,'access':'public','max':3}},{'previews':{'enabled':False,'access':'private','max':3}}]:
 inv=issue();cookie=redeem(inv)
 inner("h['api']('PATCH','/platform/apps/'+f['app_id'],"+repr(change)+")\nh['api']('PATCH','/platform/apps/'+f['app_id'],{'previews':{'enabled':True,'access':'private','max':3}})")
 assert cloud('probe',inv,cookie)['root']['status']==401
rows.append({'case':'public-privacy-and-preview-toggles-do-not-resurrect-session','passed':True});print(json.dumps(rows[-1]),flush=True);(out/'restart-security.json').write_text(json.dumps(rows,indent=2)+'\n')

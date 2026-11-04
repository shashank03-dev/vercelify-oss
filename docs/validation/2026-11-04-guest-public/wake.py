from pathlib import Path
import time,json
# Reuse private cloud-session/DNS helpers; never print invitation or cookie.
s=Path('target/guest-public-qualification/qualify.py').read_text().split("inv=issue();x=cloud('probe',inv)")[0];exec(s)
inner("h['api']('PUT','/sleep/'+f['app_id'],{'enabled':True,'idle_minutes':5,'previews':True})")
inv=issue();cookie=redeem(inv);(root/'wake-session.json').write_text(json.dumps({'invitation':inv,'cookie':cookie}))
began=time.monotonic();print('Waiting for the real five-minute idle interval in the isolated lab',flush=True)
for _ in range(350):
 st=inner("print(json.dumps(h['api']('PUT','/sleep/'+f['app_id'],{'enabled':True,'idle_minutes':5,'previews':True})))")
 targets=st.get('targets',[]) if isinstance(st,dict) else []
 asleep=[x for x in targets if x.get('pull_request_id')==7 and x.get('state')=='asleep']
 if asleep:break
 time.sleep(2)
else:raise AssertionError('Preview did not enter real idle sleep')
unauth=cloud('probe',inv);assert unauth['root']['status']==401
st=inner("print(json.dumps(h['api']('GET','/sleep')))");assert any(x.get('pull_request_id')==7 and x.get('state')=='asleep' for x in st['targets'])
wake=time.monotonic()
for _ in range(30):
 x=cloud('probe',inv,cookie)
 if x['root']['status']==200:break
 time.sleep(1)
else:raise AssertionError('Authenticated guest request did not recover preview')
inner("h['api']('PUT','/sleep/'+f['app_id'],{'enabled':False})")
row={'case':'public-guest-idle-sleep-and-wake','passed':True,'idle_wait_seconds':round(wake-began,2),'wake_to_200_seconds':round(time.monotonic()-wake,2),'unauthenticated_request_did_not_wake':True};(out/'wake.json').write_text(json.dumps(row,indent=2)+'\n');print(json.dumps(row),flush=True)

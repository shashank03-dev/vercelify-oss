from pathlib import Path
import json,subprocess,urllib.request,urllib.error,http.cookiejar,time,concurrent.futures,datetime
root=Path('target/guest-public-qualification');out=Path('docs/validation/2026-11-04-guest-public');cf='/Users/shashank/.bun/bin/cf';owner='vfy-final-qualification-docker';results=[]
def inner(code):
 pre="import runpy,json,os,time,secrets,sqlite3,datetime\nfrom pathlib import Path\nh=runpy.run_path('/workspace/host/tests/contract/guest-links.py',run_name='helper')\nf=h['facts']()\n"
 p=subprocess.run(['limactl','shell','vfybench','sudo','docker','exec','-i','-e','VERCELIFY_GUEST_CONTRACT=1','-e','AGENT_BIN=/workspace/target/aarch64-unknown-linux-musl/release/vercelify-agent',owner,'python3','-'],input=pre+code,text=True,capture_output=True,check=True)
 return json.loads(p.stdout) if p.stdout.strip() else None
access=json.loads((root/'cloud-access.json').read_text());op=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
try:op.open(access['url'],timeout=30).read()
except urllib.error.HTTPError as e:assert e.code==400

def cloud(mode,inv,cookie=None):
 b={'mode':mode,'host':inv['grant']['host']}
 if mode=='redeem':b['token']=inv['url'].split('#vfy=',1)[1]
 if cookie:b['cookie']=cookie
 req=urllib.request.Request(access['endpoint'],data=json.dumps(b).encode(),headers={'Content-Type':'application/json'},method='POST');return json.load(op.open(req,timeout=35))
def record(name,**details):
 row={'case':name,'passed':True,**details};results.append(row);(out/'lifecycle.json').write_text(json.dumps(results,indent=2)+'\n');print(json.dumps(row),flush=True)
def issue(single=False):
 inv=inner("print(json.dumps(h['issue']("+repr(single)+")))")
 o=json.loads((root/'owned.json').read_text());host=inv['grant']['host'];assert host.startswith('shr-') and host.endswith('.shashankdev03.me')
 body={'type':'CNAME','name':host,'content':o['tunnel_id']+'.cfargotunnel.com','proxied':True,'ttl':1,'comment':'Owned Vercelify guest lifecycle test'}
 d=json.loads(subprocess.check_output([cf,'dns','records','create','--zone','shashankdev03.me','--body',json.dumps(body)],text=True));o['dns'].append({'id':d['id'],'name':host});(root/'owned.json').write_text(json.dumps(o));(root/'last-invitation.json').write_text(json.dumps(inv));time.sleep(8);return inv

def redeem(inv):
 for _ in range(60):
  try:
   x=cloud('redeem',inv)
   if x.get('status')==204:break
  except urllib.error.HTTPError:pass
  time.sleep(1)
 else:raise AssertionError('public redemption readiness: '+str({k:x.get(k) for k in ['status','error']}))
 raw=x['headers']['set-cookie'];raw=raw[0] if isinstance(raw,list) else raw
 assert all(s in raw.lower() for s in ['secure','httponly','samesite=lax']);assert 'domain=' not in raw.lower();return raw.split(';',1)[0]
inv=issue();x=cloud('probe',inv)
for _ in range(20):
 if x.get('root',{}).get('error')!='ENOTFOUND':break
 time.sleep(2);x=cloud('probe',inv)
assert all(x[k]['status']==401 for k in ['root','headers','assets','advanced']);assert x['control']['status'] in [401,403,404];record('external-unauthenticated-assets-routes-and-forged-control-denied')
cookie=redeem(inv);x=cloud('probe',inv,cookie);assert all(x[k]['status']==200 for k in ['root','headers','assets','advanced']);seen=json.loads(x['headers']['body'])['headers'];assert seen.get('authorization')=='Bearer application-fixture';assert not any(k.startswith('x-vfy-') for k in seen);assert '__Host-vfy_guest' not in seen.get('cookie','');assert x['stream']['status']==200
assert 'no-store' in x['headers']['headers']['cache-control'] and x['headers']['headers']['referrer-policy']=='no-referrer';record('external-redemption-cookie-policy-stream-assets-and-internal-credential-stripping')
with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
 future=pool.submit(cloud,'watch',inv,cookie);time.sleep(4)
 inner("h['api']('DELETE','/platform/guest-links/'+"+repr(inv['grant']['id'])+")")
 w=future.result();assert w.get('upgraded') and w.get('echo') and w.get('closed') and not w.get('timeout'),w
assert cloud('probe',inv,cookie)['root']['status']==401;record('external-live-WebSocket-revocation',socket_ms=w['ms'])
inv=issue();cookie=redeem(inv)
with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
 future=pool.submit(cloud,'watch',inv,cookie);time.sleep(3)
 inner("h['sql']('UPDATE guest_grants SET expires_at=? WHERE id=?',(int(time.time())+2,"+repr(inv['grant']['id'])+"))")
 w=future.result();assert w.get('upgraded') and w.get('echo') and w.get('closed') and not w.get('timeout'),w
assert cloud('probe',inv,cookie)['root']['status']==401;record('external-live-WebSocket-expiry',socket_ms=w['ms'])
inv=issue();cookie=redeem(inv)
# Queue a new preview generation exactly as the source observer does; the agent
# owns admission, container startup, health and publication in this owned lab.
inner("""now=datetime.datetime.now(datetime.timezone.utc).isoformat().replace('+00:00','Z');dep='dpl_'+secrets.token_hex(8)
h['sql']('UPDATE previews SET head_sha=? WHERE app_id=? AND pr_number=7',('b'*40,f['app_id']))
with sqlite3.connect(h['ROOT']/'state/platform.db',timeout=10) as db:
 db.execute("INSERT INTO deployments(id,app_id,number,kind,pr_number,branch,commit_sha,commit_message,trigger,image,status,created_at) VALUES(?,?,2,'preview',7,'main',?,'public generation qualification','manual',?,'queued',?)",(dep,f['app_id'],'b'*40,h['IMAGE'],now))
h['wait_deploy'](dep)
""")
assert cloud('probe',inv,cookie)['root']['status']==401;record('external-generation-replacement-revokes-old-session')
inv=issue();cookie=redeem(inv);assert cloud('probe',inv,cookie)['root']['status']==200
inner("h['api']('DELETE','/platform/apps/'+f['app_id']+'/previews/7')")
assert cloud('probe',inv,cookie)['root']['status']==401;record('external-PR-close-revokes-session')
print('PASS: external cloud guest lifecycle through named tunnel and trusted public HTTPS',flush=True)

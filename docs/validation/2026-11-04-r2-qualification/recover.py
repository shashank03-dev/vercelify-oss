import json,time,urllib.request,urllib.error,copy,pathlib,subprocess
root=pathlib.Path('/var/lib/vfy-r2-recovery');artifact=json.loads(pathlib.Path('/workspace/target/r2-qualification/recovery-r2.json').read_text())
assert not pathlib.Path('/etc/vercelify').exists();assert not (root/'original-master.key').exists();assert artifact['manifest']['server_major']==17
base='http://127.0.0.1:17426/v1/platform'
def api(method,path,body=None):
 req=urllib.request.Request(base+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Authorization':'Bearer r2-qualification-only','Content-Type':'application/json'})
 return json.load(urllib.request.urlopen(req,timeout=30))
for _ in range(60):
 try:assert api('GET','/databases')==[];break
 except Exception:time.sleep(1)
else:raise RuntimeError('agent readiness')
def finish(job):
 for _ in range(240):
  job=api('GET','/restore-drills/'+job['id'])
  if job['state'] in ['succeeded','failed','cancelled']:
   assert not job['cleanup_pending'];return job
  time.sleep(1)
 raise RuntimeError('restore deadline')
def run(label,body,expected):
 job=finish(api('POST','/restore-drills/import',body));assert job['state']==expected,(label,job['state'],job.get('error'))
 row={'case':label,**{k:job.get(k) for k in ['state','credential_source','integrity','verified_at','cleanup_pending']}};print(json.dumps(row),flush=True)
 return job
run('live-R2-separate-ARM-recovery',artifact,'succeeded')
wrong=copy.deepcopy(artifact);wrong['recovery']['password']='deliberately-wrong-r2-key';run('wrong-recovery-key',wrong,'failed')
missing=copy.deepcopy(artifact);missing['recovery']['password']=''
try:run('missing-recovery-key',missing,'failed')
except urllib.error.HTTPError as e:assert e.code==400;print(json.dumps({'case':'missing-recovery-key','http_rejected':400}),flush=True)
wrongaccess=copy.deepcopy(artifact);wrongaccess['recovery']['access']['AWS_SECRET_ACCESS_KEY']='deliberately-wrong-secret';run('wrong-provider-credential',wrongaccess,'failed')
job=api('POST','/restore-drills/import',artifact);api('POST','/restore-drills/'+job['id']+'/cancel',{});job=finish(job);assert job['state']=='cancelled';print(json.dumps({'case':'interruption','state':job['state'],'cleanup_pending':job['cleanup_pending']}),flush=True)
assert api('GET','/databases')==[]
left=subprocess.check_output(['docker','ps','-a','--filter','label=vercelify.kind=restore-drill','--format','{{.Names}}'],text=True).strip();assert not left,left
print('PASS: R2 independent recovery, known data assertions, wrong/missing keys, wrong provider key, interruption, cleanup',flush=True)

"""Run product recovery API on separate ARM host, never original host state."""
from pathlib import Path
import subprocess
remote=r'''
import copy,json,time,urllib.request,urllib.error,shutil,subprocess
from pathlib import Path
root=Path('/var/lib/independent-recovery');artifact=json.loads((root/'recovery.json').read_text())
assert not Path('/etc/vercelify').exists()
assert artifact['manifest']['server_major']==17
base='http://127.0.0.1:17425/v1/platform'
def api(method,path,body=None):
 req=urllib.request.Request(base+path,method=method,data=None if body is None else json.dumps(body).encode(),headers={'Authorization':'Bearer offhost-contract-only','Content-Type':'application/json'})
 with urllib.request.urlopen(req,timeout=30) as r:return json.load(r)
def finish(job):
 for _ in range(240):
  job=api('GET','/restore-drills/'+job['id'])
  if job['state'] in ['succeeded','failed','cancelled']:
   assert not job['cleanup_pending'];return job
  time.sleep(1)
 raise AssertionError('restore deadline')
results=[]
def record(label,job):
 results.append({'case':label,**{k:job.get(k) for k in ['id','state','credential_source','integrity','verified_at','cleanup_pending']}})
 print(json.dumps(results[-1]),flush=True)
assert api('GET','/databases')==[],'destination must have no original database'
job=finish(api('POST','/restore-drills/import',artifact));assert job['state']=='succeeded' and job['verified_at'] and job['credential_source']=='provided' and job['integrity']=='full_data_checked',job.get('state');record('separate-host-success',job)
wrong=copy.deepcopy(artifact);wrong['recovery']['password']='intentionally-wrong-independent-key';job=finish(api('POST','/restore-drills/import',wrong));assert job['state']=='failed';record('wrong-key',job)
missing=copy.deepcopy(artifact);missing['recovery']['password']=''
try:job=finish(api('POST','/restore-drills/import',missing));assert job['state']=='failed';record('missing-key',job)
except urllib.error.HTTPError as error:assert error.code==400;results.append({'case':'missing-key','rejected_http':400});print(json.dumps(results[-1]),flush=True)
corrupt=root/'corrupt-repository';shutil.copytree(root/'repository',corrupt)
pack=next(p for p in (corrupt/'data').rglob('*') if p.is_file());data=bytearray(pack.read_bytes());data[len(data)//2]^=255;pack.write_bytes(data)
bad=copy.deepcopy(artifact);bad['recovery']['repository']=str(corrupt);job=finish(api('POST','/restore-drills/import',bad));assert job['state']=='failed';record('ciphertext-corruption',job)
job=api('POST','/restore-drills/import',artifact);api('POST','/restore-drills/'+job['id']+'/cancel',{});job=finish(job);assert job['state']=='cancelled';record('cancel-interruption',job)
assert api('GET','/databases')==[]
left=subprocess.check_output(['docker','ps','-a','--filter','label=vercelify.kind=restore-drill','--format','{{.Names}}'],text=True).strip();assert not left,left
(root/'results.json').write_text(json.dumps(results,indent=2)+'\n')
print('PASS: separate physical source removed; fresh ARM host control store/master key; known row/migration/value assertions; wrong/missing keys; corruption; cancellation and cleanup')
'''
with (Path(__file__).parent/'independent-recovery.txt').open('w') as log:
 subprocess.run(['limactl','shell','vfybench','--','sudo','docker','exec','-i','vfy-final-recovery-docker','python3','-'],input=remote,text=True,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=1200)

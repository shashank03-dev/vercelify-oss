import concurrent.futures,hashlib,json,statistics,subprocess,tarfile,threading,time,urllib.request
from pathlib import Path
folder=Path('target/offhost-amd64');settings=json.loads((folder/'settings.json').read_text());token=settings['servers'][0]['agent_token'];app=json.loads((folder/'fixture.json').read_text())['app_id']
def api(path,body=None,method=None):
 req=urllib.request.Request('http://127.0.0.1:17425/v1/platform'+path,method=method,data=None if body is None else json.dumps(body).encode(),headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
 return json.load(urllib.request.urlopen(req,timeout=20))
def publish():
 plan={'dockerfile':'FROM vfy/m3-offhost-next:amd64fixture','generated':False,'context':'.','port':3000,'framework':'pinned Next fixture','notes':[],'project_files':[]}
 job=api('/apps/'+app+'/build-jobs',{'commit_sha':'90cfee0f74c25bb3688369a0587e435f8b8f114d','platform':'linux/amd64','plan':plan})['job'];id=job['deployment_id']
 subprocess.run(['docker','tag','vfy/m3-offhost-next:amd64fixture',job['image']],check=True)
 archive=folder/'small-next.tar';subprocess.run(['docker','save','--output',str(archive),job['image']],check=True)
 with tarfile.open(archive) as saved:
  manifest=json.load(saved.extractfile('manifest.json'));digest='sha256:'+hashlib.sha256(saved.extractfile(manifest[0]['Config']).read()).hexdigest()
 auth=folder/'curl.conf';auth.touch(mode=0o600);auth.write_text('header = "Authorization: Bearer '+token+'"\n')
 start=time.monotonic()
 result=subprocess.run(['curl','-fsS','--max-time','240','-K',str(auth),'-X','PUT','-H','Content-Type: application/x-tar','--data-binary','@'+str(archive),'http://127.0.0.1:17425/v1/platform/build-jobs/'+id+'/image?generation=1&digest='+digest],capture_output=True,text=True)
 assert result.returncode==0,result.stderr
 assert json.loads(result.stdout)['phase']=='verified',result.stdout
 for i in range(120):
  deployment=api('/deployments/'+id)
  if deployment['status']=='healthy': break
  assert deployment['status'] not in ['failed','cancelled'],deployment
  time.sleep(1)
 else: raise AssertionError('health deadline')
 print(json.dumps({'deployment_id':id,'archive_bytes':archive.stat().st_size,'upload_to_healthy_seconds':round(time.monotonic()-start,2),'status':deployment['status']}),flush=True)
 return id

def load(seconds):
 end=time.monotonic()+seconds;rows=[];lock=threading.Lock()
 def worker():
  local=[]
  while time.monotonic()<end:
   start=time.monotonic()
   try:
    req=urllib.request.Request('http://127.0.0.1:17430/',headers={'Host':'offhost-next-amd64.test'})
    with urllib.request.urlopen(req,timeout=5) as response:
     body=response.read();ok=response.status==200 and len(body)>1000
    error=None if ok else 'invalid body'
   except Exception as e: error=str(e)
   local.append({'seconds':time.monotonic()-start,'error':error})
   time.sleep(max(0,0.25-(time.monotonic()-start)))
  with lock: rows.extend(local)
 with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
  list(executor.map(lambda _:worker(),range(4)))
 lat=sorted(row['seconds'] for row in rows)
 result={'duration_seconds':seconds,'concurrency':4,'per_worker_interval_seconds':0.25,'requests':len(rows),'failures':sum(r['error'] is not None for r in rows),'latency_ms_p50':round(statistics.median(lat)*1000,2),'latency_ms_p95':round(lat[int(len(lat)*0.95)]*1000,2),'latency_ms_max':round(max(lat)*1000,2)}
 print(json.dumps(result),flush=True)
 assert result['failures']==0,[r for r in rows if r['error']][:3]
 return result
if __name__=='__main__':
 publish();time.sleep(3)
 with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
  running=executor.submit(load,60)
  time.sleep(15);publish();running.result()
 print('PASS: emulated amd64 target initial import and rollout under declared request load',flush=True)

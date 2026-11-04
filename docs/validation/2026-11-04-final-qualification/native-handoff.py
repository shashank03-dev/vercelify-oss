"""Mac-built amd64 image handed to the final physical x86 agent.
Owns only the uniquely named qualification app and its images.
"""
import subprocess,json,urllib.request,urllib.error,hashlib,tarfile,time,os,threading
from pathlib import Path
OUT=Path(__file__).parent;WORK=Path('target/final-qualification-native');WORK.mkdir(parents=True,exist_ok=True)
SSH=['/Applications/Tailscale.app/Contents/MacOS/Tailscale','ssh','user@100.123.113.77']
token=subprocess.check_output(SSH+['sudo -n cat /etc/vercelify/agent.token'],text=True).strip()
BASE='http://10.77.0.1:7420/v1/platform';results=[]
auth=WORK/'curl.conf';auth.touch(mode=0o600);auth.write_text('header = "Authorization: Bearer '+token+'"\n');auth.chmod(0o600)
def api(method,path,body=None,archive=None):
 args=['curl','--socks5-hostname','127.0.0.1:7480','-sS','--max-time','600','-K',str(auth),'-X',method,'-w','\n%{http_code}']
 if archive:
  args+=['-H','Content-Type: application/x-tar','--data-binary','@'+str(archive)]
  if os.environ.get('SLOW_UPLOAD_BYTES_PER_SECOND'):args+=['--limit-rate',str(int(os.environ['SLOW_UPLOAD_BYTES_PER_SECOND']))]
 elif body is not None:args+=['-H','Content-Type: application/json','--data-binary',json.dumps(body)]
 stop=threading.Event()
 def renew():
  while not stop.wait(30):
   try:api('POST',path.split('/image',1)[0]+'/renew',{'generation':1})
   except Exception:return
 worker=threading.Thread(target=renew,daemon=True) if archive else None
 if worker:worker.start()
 try:output=subprocess.check_output(args+[BASE+path],text=True)
 finally:
  stop.set()
  if worker:worker.join()
 data,status=output.rsplit('\n',1)
 if int(status)>=400:raise urllib.error.HTTPError(BASE+path,int(status),json.loads(data).get('error','agent API error') if data.strip().startswith('{') else 'agent API error',{},None)
 return json.loads(data)
def run(args,**kw):return subprocess.run(args,check=True,capture_output=True,text=True,**kw)
source=WORK/'source';source.mkdir(exist_ok=True)
(source/'Dockerfile').write_text('FROM busybox:1.37\nRUN mkdir -p /www && echo native-x86-db1072b > /www/index.html\nEXPOSE 8080\nCMD ["httpd","-f","-p","8080","-h","/www"]\n')
plan=json.loads(run(['target/debug/vercelify-pack','plan',str(source)],input='{}').stdout)
app=None;tags=[]
try:
 app=api('POST','/apps',{'name':'final-x86-qualification','kind':'git','source':{'repository':'https://example.com/qualification-never-fetch.git'},'runtime':{'port':8080,'health_timeout_secs':30,'drain_secs':0,'memory_mb':64},'domains':[]})
 (WORK/'owned-app.json').write_text(json.dumps({'id':app['id']}))
 job=api('POST','/apps/'+app['id']+'/build-jobs',{'commit_sha':'d'*40,'platform':'linux/amd64','plan':plan})
 start=time.monotonic();built=run(['target/debug/vercelify-pack','build',str(source)],input=json.dumps({'build':app['build'],'app_name':app['name'],'deployment_id':job['job']['deployment_id'],'variables':job['variables'],'platform':'linux/amd64'}),timeout=300)
 (OUT/'native-build.txt').write_text(built.stderr)
 image=json.loads(built.stdout)['image'];tags.append(image);run(['docker','tag',image,job['job']['image']]);tags.append(job['job']['image'])
 archive=WORK/'native.tar';run(['docker','save','-o',str(archive),job['job']['image']])
 with tarfile.open(archive) as saved:
  manifest=json.load(saved.extractfile('manifest.json'));config=saved.extractfile(manifest[0]['Config']).read();digest='sha256:'+hashlib.sha256(config).hexdigest();architecture=json.loads(config)['architecture']
 assert architecture=='amd64'
 id=job['job']['deployment_id'];uploaded=api('PUT',f'/build-jobs/{id}/image?generation=1&digest={digest}',archive=archive);assert uploaded['phase']=='verified'
 for attempt in range(120):
  deployment=api('GET','/deployments/'+id)
  if deployment['status']=='healthy':break
  assert deployment['status'] not in ['failed','cancelled'],deployment['status'];time.sleep(1)
 else:raise AssertionError('deployment deadline')
 request=urllib.request.Request('http://100.123.113.77/',headers={'Host':'final-x86-qualification.10.77.0.1.sslip.io'})
 for attempt in range(15):
  try:
   with urllib.request.urlopen(request,timeout=5) as response:body=response.read().decode();assert 'native-x86-db1072b' in body;break
  except Exception:
   if attempt==14:raise
   time.sleep(1)
 results.append({'case':'native-handoff','architecture':architecture,'config_digest':digest,'archive_bytes':archive.stat().st_size,'build_import_to_healthy_seconds':round(time.monotonic()-start,2),'upload_rate_limit_bytes_per_second':os.environ.get('SLOW_UPLOAD_BYTES_PER_SECOND'),'status':deployment['status'],'body_assertion':True})
 try:
  api('POST','/apps/'+app['id']+'/build-jobs',{'commit_sha':'e'*40,'platform':'linux/arm64','plan':plan})
 except urllib.error.HTTPError as error:
  assert error.code in (400,409),error.code;results.append({'case':'incompatible-architecture-request','rejected_http':error.code})
 else:raise AssertionError('incompatible platform was accepted')
 print(json.dumps(results),flush=True)
finally:
 if app:api('DELETE','/apps/'+app['id'])
 for tag in tags:subprocess.run(['docker','image','rm',tag],capture_output=True)
 (OUT/'native-handoff.json').write_text(json.dumps(results,indent=2)+'\n')
 auth.unlink(missing_ok=True)

"""Owned four-GiB lab only: journal restart, pending drain/reboot, stopped/sleeping intent.

The sleeping fixture explicitly stops its own container then declares sleeping;
this qualifies recovery of that state, not idle detection. Never runs on laptop.
"""
from pathlib import Path
import concurrent.futures
import json
import subprocess
import threading
import time
import urllib.error
import urllib.request

ROOT=Path.cwd(); OUT=ROOT/'docs/validation/2026-11-03-m6'; PRIVATE=ROOT/'target/offhost-small-vm'
phase_lock=threading.Lock()
TOKEN=json.loads((PRIVATE/'settings.json').read_text())['servers'][0]['agent_token']
def command(args): return subprocess.run(args,check=True,capture_output=True,text=True).stdout.strip()
def guest(args): return command(['limactl','shell','vfybench','--','sudo',*args])
def api(path,body=None,method=None):
 req=urllib.request.Request('http://127.0.0.1:17427/v1/platform'+path,method=method,data=None if body is None else json.dumps(body).encode(),headers={'Authorization':'Bearer '+TOKEN,'Content-Type':'application/json'})
 with urllib.request.urlopen(req,timeout=30) as response:
  raw=response.read(); return json.loads(raw) if raw else None

def until(label,predicate,seconds=150):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
  try:
   value=predicate()
   if value:return value
  except (OSError,ValueError):pass
  time.sleep(1)
 raise AssertionError('timed out: '+label)
def probe(host):
 req=urllib.request.Request('http://127.0.0.1:17429/',headers={'Host':host})
 with urllib.request.urlopen(req,timeout=3) as response: assert response.status==200; response.read()
def rows(app):
 ids=guest(['docker','ps','-aq','--filter','label=vercelify.app='+app]).split()
 if not ids:return []
 result=subprocess.run(['limactl','shell','vfybench','--','sudo','docker','inspect','--format','{{json .}}',*ids],capture_output=True,text=True)
 if result.returncode and not any(message in result.stderr.lower() for message in ['no such object','no such container']):
  raise subprocess.CalledProcessError(result.returncode,result.args,result.stdout,result.stderr)
 data=[json.loads(line) for line in result.stdout.splitlines() if line.strip()]
 return [{'container':r['Name'].lstrip('/'),'running':r['State']['Running'],'restart':r['HostConfig']['RestartPolicy']['Name'],'deployment':r['Config']['Labels'].get('vercelify.deployment')} for r in data]
def healthy(id):
 d=api('/deployments/'+id);assert d['status'] not in ['failed','cancelled'],d
 return d if d['status']=='healthy' else None

original=api('/apps'); original_ids={a['id'] for a in original}
assert original_ids=={a['id'] for a in json.loads((PRIVATE/'m4-original-apps.json').read_text())}
for a in original: probe(a['name']+'.10.77.0.1.sslip.io')
project=api('/projects',{'name':'m6-recovery-qualification'})
owned=[]; result={}; phase={'value':'agent'}; stop=threading.Event(); samples=[]
def sampler():
 while not stop.is_set():
  for a in original:
   with phase_lock:
    at=time.monotonic();current_phase=phase['value']
    try:probe(a['name']+'.10.77.0.1.sslip.io');error=None
    except Exception as failure:error=type(failure).__name__
    samples.append({'phase':current_phase,'at':at,'app':a['name'],'error':error})
  stop.wait(.25)
thread=threading.Thread(target=sampler)
try:
 until('fresh capacity',lambda:any(r['sample'] and r['sample_age_secs']<15 for r in api('/capacity')))
 image=guest(['docker','image','inspect','--format','{{.Id}}','vfy/blog:8yywze68cc5wwk9b'])
 def create(name,compatible=False,drain=0):
  app=api('/apps',{'name':name,'kind':'image','project_id':project['id'],'source':{'image':image},'runtime':{'port':3000,'memory_mb':192,'cpus':1,'health_timeout_secs':60,'drain_secs':drain,'rollback_compatible':compatible,'rollback_grace_secs':0}})
  owned.append(app); (PRIVATE/'m6-fixture.json').write_text(json.dumps({'project':project['id'],'apps':[a['id'] for a in owned]}))
  api('/apps/'+app['id']+'/domains',{'host':name+'.test','https':False,'primary':True})
  d=api('/apps/'+app['id']+'/deploy',{})
  until(name+' healthy',lambda:healthy(d['id']))
  return app,d
 peers=[]
 for desired in ['stopped','sleeping']:
  app,d=create('m6-'+desired)
  if desired=='sleeping':
   for r in rows(app['id']):guest(['docker','stop',r['container']])
  api('/apps/'+app['id'],{'desired':desired},'PATCH')
  until('peer stopped',lambda:rows(app['id']) and all(not r['running'] for r in rows(app['id'])))
  peers.append(app)
 app,first=create('m6-reboot',True,60)
 second=api('/apps/'+app['id']+'/deploy',{})
 second=until('second healthy',lambda:healthy(second['id']))
 assert second['error']=='',second
 before=api('/apps/'+app['id']+'/deployment-guards')[0]
 assert before['state']=='watching' and len(rows(app['id']))==2,before
 result['guard_before']=before; thread.start()
 guest(['systemctl','restart','vercelify-agent'])
 after=until('guard after restart',lambda:next((g for g in api('/apps/'+app['id']+'/deployment-guards') if g['deployment_id']==second['id']),None))
 assert after['deadline']==before['deadline'] and after['baseline']==before['baseline']
 result['guard_after_agent_restart']=after
 with phase_lock:phase['value']='reboot'
 boot=time.monotonic()
 command(['limactl','stop','vfybench']);command(['limactl','start','vfybench','--tty=false'])
 command(['ssh','-F',str(Path.home()/'.lima/vfybench/ssh.config'),'-O','forward','-L','17429:127.0.0.1:80','-L','17427:127.0.0.1:7420','lima-vfybench'])
 until('original routes after reboot',lambda:all((probe(a['name']+'.10.77.0.1.sslip.io') is None) for a in original))
 result['boot_and_forward_recovery_seconds']=time.monotonic()-boot
 with phase_lock:phase['value']='recovered'
 after=api('/apps/'+app['id']+'/deployment-guards')[0]
 assert after['deadline']==before['deadline'] and after['baseline']==before['baseline']
 result['guard_after_vm_reboot']=after
 result['quiet_peers']={}
 for peer in peers:
  actual=rows(peer['id']);assert actual and all(not r['running'] for r in actual),actual
  check=api('/apps/'+peer['id']+'/serving-check');assert check['desired'] in ['stopped','sleeping'] and check['runtime'] is None
  result['quiet_peers'][peer['name']]={'containers':actual,'check':check}
 until('single current generation',lambda:len(rows(app['id']))==1 and rows(app['id'])[0]['deployment']==second['id'],120)
 result['current_generation']=rows(app['id'])
 probe('m6-reboot.test')
 result['serving_check']=api('/apps/'+app['id']+'/serving-check')
 assert result['serving_check']['runtime'] and result['serving_check']['ingress']
 api('/apps/'+app['id'],method='DELETE');until('fixture removed',lambda:not rows(app['id']))
 network='vfy-net-'+project['id'].lower().replace('_','-')
 guest(['docker','network','inspect',network]);result['stopped_peer_bridge_preserved']=True
finally:
 stop.set()
 if thread.is_alive():thread.join(timeout=15)
 result['probes']={p:{'requests':sum(s['phase']==p for s in samples),'failures':sum(s['phase']==p and s['error'] is not None for s in samples)} for p in ['agent','reboot','recovered']}
 (OUT/'reboot-probes.json').write_text(json.dumps(samples,indent=2)+'\n')
 (OUT/'reboot.json').write_text(json.dumps(result,indent=2)+'\n')
 for app in reversed(owned):
  try:api('/apps/'+app['id'],method='DELETE')
  except urllib.error.HTTPError as error:
   if error.code!=404:raise
 until('all own containers removed',lambda:all(not rows(a['id']) for a in owned))
 api('/projects/'+project['id'],method='DELETE')
 assert {a['id'] for a in api('/apps')}==original_ids
 for a in original:probe(a['name']+'.10.77.0.1.sslip.io')
 (PRIVATE/'m6-fixture.json').unlink(missing_ok=True)
assert result['probes']['agent']['failures']==0,result['probes']
print(json.dumps(result,indent=2))

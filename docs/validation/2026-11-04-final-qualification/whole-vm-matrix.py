"""Cold/warm shared-pack fixture in a newly created, exclusively owned Lima VM."""
from pathlib import Path
import subprocess,re,json
OUT=Path(__file__).parent;VM='vfy-final-resources'
remote=r'''
from pathlib import Path
import subprocess,json,time,statistics,threading,os
owner=Path('/var/lib/vercelify-final-qualification-owner')
if owner.exists():assert owner.read_text()=='whole-vm-fixture-only'
else:owner.write_text('whole-vm-fixture-only')
assert not Path('/etc/vercelify').exists()
def run(args,**kw):return subprocess.run(args,check=True,capture_output=True,text=True,**kw)
# This VM was created exclusively for this matrix. No serving daemon/state is
# reused. Clear both image stores, with their services stopped, for every row.
run(['systemctl','stop','docker.service','docker.socket','containerd.service'])
run(['rm','-rf','/var/lib/docker','/var/lib/containerd'])
run(['systemctl','start','containerd.service','docker.service']);run(['swapoff','-a'])
assert not run(['docker','images','-q']).stdout.strip()
mem=lambda:{k:int(v.split()[0]) for k,v in [l.split(':',1) for l in Path('/proc/meminfo').read_text().splitlines()]}
assert mem()['SwapTotal']==0
row={'scope':'whole, disposable ARM Linux VM; no original apps or databases','architecture':os.uname().machine,'kernel':os.uname().release,'ram_kib':mem()['MemTotal'],'swap_kib':mem()['SwapTotal'],'cpus':os.cpu_count(),'docker_version':run(['docker','version','--format','{{.Server.Version}}']).stdout.strip(),'initial_image_cache':'empty; Docker and containerd stores recreated','load':'one shared-pack static HTTP build; then 2 clients x 20 requests','page_cache':'guest cache dropped before import; host cache uncontrolled','loadavg_before':Path('/proc/loadavg').read_text().strip()}
used=[];stop=threading.Event()
def sample():
 while not stop.is_set():
  s=mem();used.append(s['MemTotal']-s['MemAvailable']);stop.wait(.1)
t=threading.Thread(target=sample,daemon=True);t.start()
try:
 start=time.monotonic();run(['docker','pull','busybox:1.37']);row['cold_download_seconds']=round(time.monotonic()-start,3)
 image=json.loads(run(['docker','image','inspect','busybox:1.37']).stdout)[0];row.update(image_id=image['Id'],repo_digests=image['RepoDigests'])
 run(['docker','save','-o','/var/lib/final-fixture.tar','busybox:1.37']);run(['docker','image','rm','busybox:1.37']);assert not run(['docker','images','-q']).stdout.strip()
 run(['sh','-ec','sync; echo 3 > /proc/sys/vm/drop_caches'])
 start=time.monotonic();run(['docker','load','-i','/var/lib/final-fixture.tar']);row['cold_image_store_import_seconds']=round(time.monotonic()-start,3)
 src=Path('/var/lib/final-source');src.mkdir(exist_ok=True);(src/'Dockerfile').write_text('FROM busybox:1.37\nRUN mkdir -p /www && echo whole-vm-shared-pack > /www/index.html\nEXPOSE 8080\nCMD ["httpd","-f","-p","8080","-h","/www"]\n')
 pack='/Users/shashank/Desktop/VERCELIFY/target/aarch64-unknown-linux-musl/release/vercelify-pack'
 tags=[]
 for label in ['cold','warm']:
  start=time.monotonic();built=run([pack,'build',str(src)],input=json.dumps({'app_name':'final-resource','deployment_id':'dpl_'+label,'build':{},'variables':[],'platform':'linux/arm64'}),timeout=300)
  image=json.loads(built.stdout)['image'];tags.append(image);row[label+'_shared_pack_build_seconds']=round(time.monotonic()-start,3);row[label+'_cached_steps']=built.stderr.count('CACHED')
 assert row['cold_cached_steps']==0 and row['warm_cached_steps']>0,row
 row['fixture_image_id']=json.loads(run(['docker','image','inspect',tags[-1]]).stdout)[0]['Id']
 run(['docker','run','-d','--name','final-resource-fixture','--memory','32m','-p','127.0.0.1:18080:8080',tags[-1]])
 import urllib.request,concurrent.futures
 def client(_):
  rows=[]
  for i in range(20):
   start=time.monotonic()
   with urllib.request.urlopen('http://127.0.0.1:18080/',timeout=5) as r:assert r.status==200 and b'whole-vm-shared-pack' in r.read()
   rows.append((time.monotonic()-start)*1000)
  return rows
 with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:latency=sum(executor.map(client,range(2)),[])
 row.update(requests=len(latency),failures=0,latency_ms_p50=round(statistics.median(latency),3),latency_ms_p95=round(sorted(latency)[int(.95*len(latency))-1],3))
 run(['docker','rm','-f','final-resource-fixture'])
finally:stop.set();t.join()
row['sampled_used_kib_peak']=max(used);row['sampled_used_kib_median']=statistics.median(used);row['memory_samples']=len(used)
row['oom_kill_counter']=next(l for l in Path('/proc/vmstat').read_text().splitlines() if l.startswith('oom_kill '));assert row['oom_kill_counter']=='oom_kill 0'
print(json.dumps(row),flush=True)
'''
rows=[]
for gib in [1,2,4]:
 if gib!=1:
  subprocess.run(['limactl','stop',VM],check=True,stdout=subprocess.DEVNULL)
  config=Path.home()/'.lima'/VM/'lima.yaml';text=config.read_text();text,n=re.subn(r'^memory:.*$',f'memory: {gib}GiB',text,flags=re.M);assert n==1;config.write_text(text)
  with (OUT/('whole-vm-'+str(gib)+'g-boot.txt')).open('w') as log:subprocess.run(['limactl','start',VM,'--tty=false'],stdout=log,stderr=subprocess.STDOUT,check=True,timeout=300)
 with (OUT/('whole-vm-'+str(gib)+'g.txt')).open('w') as log:subprocess.run(['limactl','shell',VM,'--','sudo','python3','-'],input=remote,text=True,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=600)
 row=json.loads((OUT/('whole-vm-'+str(gib)+'g.txt')).read_text());row['configured_gib']=gib;rows.append(row);(OUT/'whole-vm-matrix.json').write_text(json.dumps(rows,indent=2)+'\n');print('PASS whole VM '+str(gib)+' GiB',flush=True)
subprocess.run(['limactl','stop',VM],check=True)

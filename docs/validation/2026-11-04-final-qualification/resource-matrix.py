"""Cold/warm fixtures at Docker cgroup budgets; not whole-machine RAM profiles."""
from pathlib import Path
import subprocess,json,time
remote=r'''
import json,subprocess,time,statistics,threading
results=[]
def outer(args,**kw):return subprocess.run(['docker',*args],check=True,capture_output=True,text=True,**kw)
for gib in [1,2,4]:
 name='vfy-finalq-resource-'+str(gib)
 assert subprocess.run(['docker','inspect',name],capture_output=True).returncode!=0
 outer(['run','-d','--privileged','--name',name,'--label','vercelify.qualification=final-resource','--memory',str(gib)+'g','--memory-swap',str(gib)+'g','-e','DOCKER_TLS_CERTDIR=','vfy-finalq-tools','--host=unix:///var/run/docker.sock'])
 def inner(args,**kw):return outer(['exec',name,*args],**kw)
 try:
  for _ in range(60):
   if subprocess.run(['docker','exec',name,'docker','info'],capture_output=True).returncode==0:break
   time.sleep(1)
  assert not inner(['docker','images','-q']).stdout.strip()
  row={'budget_gib':gib,'scope':'nested Docker cgroup limit, shared 4-GiB ARM VM; no whole-machine profile claim','architecture':'arm64','initial_image_cache':'empty','load':'single image pull/import/build; then 2 HTTP clients x 20 requests','disk_page_cache':'uncontrolled'}
  start=time.monotonic();inner(['docker','pull','busybox:1.37']);row['cold_download_seconds']=round(time.monotonic()-start,3)
  info=json.loads(inner(['docker','image','inspect','busybox:1.37']).stdout)[0];row.update(image_id=info['Id'],repo_digests=info['RepoDigests'])
  # Loading after removal proves image-store cold import; filesystem page cache
  # is deliberately recorded as uncontrolled rather than called disk-cold.
  inner(['docker','save','-o','/var/lib/fixture.tar','busybox:1.37']);inner(['docker','image','rm','busybox:1.37']);assert not inner(['docker','images','-q']).stdout.strip()
  start=time.monotonic();inner(['docker','load','-i','/var/lib/fixture.tar']);row['cold_image_store_import_seconds']=round(time.monotonic()-start,3)
  inner(['sh','-ec','mkdir /var/lib/fixture; printf "FROM busybox:1.37\\nRUN mkdir /www && echo resource-fixture > /www/index.html\\nCMD [\\"httpd\\",\\"-f\\",\\"-p\\",\\"8080\\",\\"-h\\",\\"/www\\"]\\n" > /var/lib/fixture/Dockerfile'])
  for label in ['cold_build','warm_build']:
   start=time.monotonic();built=inner(['docker','build','--progress=plain','-t','vfy/final-resource:test','/var/lib/fixture']);row[label+'_seconds']=round(time.monotonic()-start,3);row[label+'_cached_steps']=built.stderr.count('CACHED')
  row['fixture_image_id']=json.loads(inner(['docker','image','inspect','vfy/final-resource:test']).stdout)[0]['Id']
  inner(['docker','run','-d','--name','fixture','--memory','32m','-p','127.0.0.1:18080:8080','vfy/final-resource:test'])
  load='import urllib.request,time,json,concurrent.futures;'+"\ndef work(_):\n rows=[]\n for i in range(20):\n  start=time.monotonic()\n  with urllib.request.urlopen('http://127.0.0.1:18080/',timeout=5) as r:assert r.status==200 and b'resource-fixture' in r.read()\n  rows.append((time.monotonic()-start)*1000)\n return rows\nwith concurrent.futures.ThreadPoolExecutor(max_workers=2) as e:print(json.dumps(sum(e.map(work,range(2)),[])))"
  latency=json.loads(inner(['python3','-c',load]).stdout);row.update(requests=len(latency),failures=0,latency_ms_p50=round(statistics.median(latency),3),latency_ms_p95=round(sorted(latency)[int(.95*len(latency))-1],3))
  pid=json.loads(outer(['inspect',name]).stdout)[0]['State']['Pid'];cgroup=next(l.split(':',2)[2] for l in open('/proc/'+str(pid)+'/cgroup') if l.startswith('0::')).strip();cg='/sys/fs/cgroup'+cgroup
  row['cgroup_peak_bytes']=int(open(cg+'/memory.peak').read());row['cgroup_memory_events']=open(cg+'/memory.events').read().splitlines();assert 'oom_kill 0' in row['cgroup_memory_events']
  results.append(row);print(json.dumps(row),flush=True)
 finally:outer(['rm','-f','-v',name])
print('PASS: cgroup-budget matrix; whole-machine cold 1/2/4-GiB qualification remains separate')
'''
with (Path(__file__).parent/'resource-matrix.txt').open('w') as log:
 subprocess.run(['limactl','shell','vfybench','--','sudo','python3','-'],input=remote,text=True,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=1200)

"""Exercise the installed agent API/runner against an owned synthetic database.

Reads pairing credentials privately; evidence contains no request/recovery secret.
Only the named lab VM is touched; the original six app routes are probed throughout.
"""
import json
from pathlib import Path
import subprocess
settings=json.loads(Path('target/offhost-small-vm/settings.json').read_text())
context={'token':settings['servers'][0]['agent_token'],'original':json.loads(Path('target/offhost-small-vm/m4-original-apps.json').read_text())}
remote=r'''
import datetime,json,os,secrets,shutil,subprocess,sys,threading,time,urllib.request
from pathlib import Path
ctx=json.load(sys.stdin);token=ctx['token']
base='http://127.0.0.1:7420/v1/platform'
def api(method,path,body=None):
    req=urllib.request.Request(base+path,data=None if body is None else json.dumps(body).encode(),headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'},method=method)
    with urllib.request.urlopen(req,timeout=30) as response:return json.load(response)
def docker(args,**kwargs):return subprocess.run(['docker',*args],check=True,capture_output=True,**kwargs)
stop=threading.Event();counts={'requests':0,'failures':0}
def probe():
    while not stop.is_set():
        for app in ctx['original']:
            counts['requests']+=1
            try:
                req=urllib.request.Request('http://127.0.0.1/',headers={'Host':app['name']+'.10.77.0.1.sslip.io'})
                with urllib.request.urlopen(req,timeout=3) as response:assert response.status==200;response.read()
            except Exception:counts['failures']+=1
        stop.wait(.3)
thread=threading.Thread(target=probe,daemon=True);thread.start()
root=Path('/var/lib/vercelify/qualification-m7a-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S'))
root.mkdir(mode=0o700);repo=root/'repository';repo.mkdir(mode=0o700)
password=secrets.token_urlsafe(32)
image='restic/restic@sha256:4cf4a61ef9786f4de53e9de8c8f5c040f33830eb0a10bf3d614410ee2fcb6120'
docker(['pull',image])
env=os.environ.copy();env.update(RESTIC_PASSWORD=password,RESTIC_REPOSITORY='/repository')
helper=['run','--rm','--log-driver','none','--network','none','--memory','512m','--memory-swap','512m','--cpus','1','-e','RESTIC_PASSWORD','-e','RESTIC_REPOSITORY','--mount','type=bind,src='+str(repo)+',dst=/repository',image]
docker(helper+['init'],env=env)
source=None;job=None
try:
    source=api('POST','/databases',{'name':'m7a-native-'+secrets.token_hex(3),'kind':'postgresql','version':'17','memory_mb':256})
    (root/'owned.json').write_text(json.dumps({'database_id':source['id'],'container':source['container']}));os.chmod(root/'owned.json',0o600)
    for attempt in range(120):
        status=api('GET','/databases/'+source['id']+'/status')
        if status['state']=='running' and (status.get('operation') or {}).get('status')!='running':break
        time.sleep(1)
    else:raise RuntimeError('native source database did not start')
    def sql(statement):
        return docker(['exec',source['container'],'sh','-ec','exec psql -X -q -At -U "$POSTGRES_USER" -d "$POSTGRES_DB" -v ON_ERROR_STOP=1 -c "$1"','fixture',statement],text=True).stdout.strip()
    sql("CREATE TABLE entries(id integer PRIMARY KEY); INSERT INTO entries SELECT generate_series(1,128); CREATE TABLE migrations(version text); INSERT INTO migrations VALUES('20261103');")
    started=datetime.datetime.now(datetime.timezone.utc).isoformat()
    producer=subprocess.Popen(['docker','exec',source['container'],'sh','-ec','exec pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc'],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
    # The source is synthetic and bounded; bytes remain in a pipe/memory, never
    # in a plaintext archive file. Production is never a source or destination.
    dump=producer.stdout.read(4*1024*1024+1);producer.stdout.close();assert producer.wait()==0 and len(dump)<=4*1024*1024
    args=helper.copy();args.insert(1,'-i')
    output=docker(args+['backup','--stdin','--stdin-filename',source['name']+'.dump','--json'],input=dump,env=env).stdout.decode();size=len(dump);del dump
    snapshot=next(json.loads(line)['snapshot_id'] for line in output.splitlines() if json.loads(line).get('message_type')=='summary')
    catalog="SET search_path=pg_catalog; SELECT json_build_object('database_bytes',pg_database_size(current_database()),'server_major',current_setting('server_version_num')::int/10000,'extensions',(SELECT json_agg(json_build_array(extname,extversion) ORDER BY extname) FROM pg_extension),'objects',(SELECT json_agg(json_build_array(n.nspname,c.relname,c.relkind::text) ORDER BY n.nspname,c.relname,c.relkind) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname NOT LIKE 'pg_%' AND n.nspname<>'information_schema' AND c.relkind IN ('r','p','i','I','v','m','S','f')))"
    manifest=json.loads(sql(catalog));manifest['extensions'].sort();manifest['objects'].sort()
    manifest.update(version=1,database_id=source['id'],database_name=source['name'],filename=source['name']+'.dump',format='postgresql-custom',backup_started_at=started,bytes=size)
    recovery={'repository':str(repo),'password':password,'access':{}}
    artifact=root/'recovery.json';artifact.write_text(json.dumps({'manifest':manifest,'snapshot':snapshot,'recovery':recovery}));os.chmod(artifact,0o600)
    # Read the retained artifact, not the host's backup.env/master key.
    imported=json.loads(artifact.read_text());imported['full_integrity']=True;imported['assertions']=[{'name':'row-count','query':'SELECT count(*) FROM entries','expected':'128'},{'name':'migration','query':'SELECT version FROM migrations','expected':'20261103'}]
    job=api('POST','/restore-drills/import',imported)
    for attempt in range(180):
        job=api('GET','/restore-drills/'+job['id'])
        if job['state'] in ['succeeded','failed','cancelled']:break
        time.sleep(1)
    assert job['state']=='succeeded' and job['verified_at'] and not job['cleanup_pending'],job
    assert job['credential_source']=='provided' and job['integrity']=='full_data_checked',job
    assert sql('SELECT count(*) FROM entries')=='128'
    assert len(api('GET','/apps'))==6
    print(json.dumps({'job':job,'source_count':128,'retained_private_recovery_artifact':str(artifact),'scope':'installed agent API and background runner; provided recovery key, synthetic dataset; not physical machine-loss qualification'}))
finally:
    if job and job['state'] not in ['succeeded','failed','cancelled']:
        api('POST','/restore-drills/'+job['id']+'/cancel',{})
        for attempt in range(90):
            current=api('GET','/restore-drills/'+job['id'])
            if current['state'] in ['succeeded','failed','cancelled']:break
            time.sleep(1)
        assert not current['cleanup_pending'],'restore cleanup still pending'
    if source:
        name=source['container'];inspected=subprocess.run(['docker','inspect',name],capture_output=True,text=True)
        info=json.loads(inspected.stdout)[0] if inspected.returncode==0 else None
        volumes=[m['Name'] for m in info['Mounts'] if m['Type']=='volume'] if info else []
        api('DELETE','/databases/'+source['id']+'?purge=true')
        for attempt in range(60):
            inspected=subprocess.run(['docker','inspect',name],capture_output=True)
            volume_present=[v for v in volumes if subprocess.run(['docker','volume','inspect',v],capture_output=True).returncode==0]
            if inspected.returncode!=0 and not volume_present:break
            time.sleep(1)
        else:raise RuntimeError('owned source cleanup incomplete')
    stop.set();thread.join(timeout=10)
    assert counts['failures']==0,counts
    print(json.dumps({'original_app_probes':counts,'source_cleanup_complete':True}))
'''
# Separate code and stdin using -c: no token enters argv, stdout or evidence.
result=subprocess.run(['limactl','shell','vfybench','--','sudo','python3','-c',remote],input=json.dumps(context),text=True,capture_output=True)
Path('docs/validation/2026-11-03-m7a/native-drill.txt').write_text(result.stdout+result.stderr)
if result.returncode:raise RuntimeError('native drill failed; inspect its secret-free evidence')
print(result.stdout)

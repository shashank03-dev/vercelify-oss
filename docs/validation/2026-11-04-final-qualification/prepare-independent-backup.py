"""Produce encrypted synthetic PostgreSQL 17 backup on physical x86 laptop.
Only owned source is removed; credentials remain private and are transferred separately
from any original host control store or master key.
"""
from pathlib import Path
import subprocess
remote=r'''
import datetime,json,os,secrets,subprocess,time
from pathlib import Path
root=Path('/var/lib/vercelify/qualification-final-20261104T022650Z/independent-backup');root.mkdir(mode=0o700,exist_ok=True)
repo=root/'repository';repo.mkdir(mode=0o700,exist_ok=True);password=secrets.token_urlsafe(32)
name='vfy-finalq-pg17-source';volume='vfy-finalq-pg17-source-data'
def docker(args,**kw):return subprocess.run(['docker',*args],check=True,capture_output=True,**kw)
assert subprocess.run(['docker','inspect',name],capture_output=True).returncode!=0
assert subprocess.run(['docker','volume','inspect',volume],capture_output=True).returncode!=0
image='postgres:17-alpine';info=json.loads(docker(['image','inspect',image],text=True).stdout)[0];assert info['Architecture']=='amd64'
env=os.environ.copy();env.update(RESTIC_PASSWORD=password,RESTIC_REPOSITORY=str(repo))
def restic(args,**kw):return subprocess.run(['restic',*args],env=env,check=True,capture_output=True,**kw)
try:
 docker(['run','-d','--name',name,'--label','vercelify.qualification=final-independent-backup','--memory','256m','--cpus','1','-v',volume+':/var/lib/postgresql/data','-e','POSTGRES_USER=fixture','-e','POSTGRES_DB=fixture','-e','POSTGRES_PASSWORD='+secrets.token_urlsafe(32),image])
 for _ in range(90):
  if b'PostgreSQL init process complete' in subprocess.run(['docker','logs',name],capture_output=True).stdout and subprocess.run(['docker','exec',name,'pg_isready','-U','fixture'],capture_output=True).returncode==0:break
  time.sleep(1)
 else:raise RuntimeError('source readiness')
 def sql(q):return docker(['exec',name,'psql','-X','-q','-At','-U','fixture','-d','fixture','-v','ON_ERROR_STOP=1','-c',q],text=True).stdout.strip()
 sql("CREATE TABLE entries(id integer PRIMARY KEY, value text); INSERT INTO entries SELECT i, 'final-'||i FROM generate_series(1,128) AS i; CREATE TABLE migrations(version text); INSERT INTO migrations VALUES('db1072b');")
 assert sql('SELECT count(*) FROM entries')=='128'
 started=datetime.datetime.utcnow().isoformat()+'Z';dump=docker(['exec',name,'pg_dump','-U','fixture','-d','fixture','-Fc']).stdout;assert len(dump)<4*1024*1024
 restic(['init']);output=restic(['backup','--stdin','--stdin-filename','fixture.dump','--json'],input=dump).stdout.decode();snapshot=next(json.loads(l)['snapshot_id'] for l in output.splitlines() if json.loads(l).get('message_type')=='summary')
 snapshot=json.loads(restic(['snapshots','--json'],text=True).stdout)[-1]['id']
 catalog="SET search_path=pg_catalog; SELECT json_build_object('database_bytes',pg_database_size(current_database()),'server_major',current_setting('server_version_num')::int/10000,'extensions',(SELECT json_agg(json_build_array(extname,extversion) ORDER BY extname) FROM pg_extension),'objects',(SELECT json_agg(json_build_array(n.nspname,c.relname,c.relkind::text) ORDER BY n.nspname,c.relname,c.relkind) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname NOT LIKE 'pg_%' AND n.nspname<>'information_schema' AND c.relkind IN ('r','p','i','I','v','m','S','f')))"
 manifest=json.loads(sql(catalog));manifest['extensions'].sort();manifest['objects'].sort();manifest.update(version=1,database_id='db_independentphysicalfixture',database_name='fixture',filename='fixture.dump',format='postgresql-custom',backup_started_at=started,bytes=len(dump))
 artifact={'manifest':manifest,'snapshot':snapshot,'recovery':{'repository':'/var/lib/independent-recovery/repository','password':password,'access':{}},'full_integrity':True,'assertions':[{'name':'rows','query':'SELECT count(*) FROM entries','expected':'128'},{'name':'migration','query':'SELECT version FROM migrations','expected':'db1072b'},{'name':'values','query':"SELECT value FROM entries WHERE id=128",'expected':'final-128'}]}
 (root/'recovery.json').write_text(json.dumps(artifact));(root/'recovery.json').chmod(0o600)
 restic(['check','--read-data'])
 print(json.dumps({'source_host':'physical-x86','source_image_id':info['Id'],'source_repo_digests':info['RepoDigests'],'architecture':info['Architecture'],'snapshot':snapshot,'dump_bytes':len(dump),'asserted_rows':128,'root':str(root)}))
finally:
 subprocess.run(['docker','rm','-f',name],capture_output=True,check=True);subprocess.run(['docker','volume','rm',volume],capture_output=True,check=True)
print('Owned source container and source volume removed before independent recovery')
'''
with (Path(__file__).parent/'independent-backup-source.txt').open('w') as log:
 subprocess.run(['/Applications/Tailscale.app/Contents/MacOS/Tailscale','ssh','user@100.123.113.77','sudo -n python3 -'],input=remote,text=True,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=180)

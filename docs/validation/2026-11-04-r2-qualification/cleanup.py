from pathlib import Path
import subprocess,json,xml.etree.ElementTree as ET,time
r=Path('target/r2-qualification');e=json.loads((r/'environment.json').read_text());o=json.loads((r/'owned.json').read_text());cf='/Users/shashank/.bun/bin/cf'
base=f'https://{o["account"]}.r2.cloudflarestorage.com/{o["bucket"]}'
def request(method,url,out):
 cfg=f'url = "{url}"\naws-sigv4 = "aws:amz:auto:s3"\nuser = "{e["AWS_ACCESS_KEY_ID"]}:{e["AWS_SECRET_ACCESS_KEY"]}"\nsilent\nshow-error\nmax-time = 30\nwrite-out = "%{{http_code}}"\noutput = "{out}"\n'
 cfg+= 'head\n' if method=='HEAD' else f'request = "{method}"\n'
 p=subprocess.run(['curl','--config','-'],input=cfg,capture_output=True,text=True,check=True);return int(p.stdout)
# Recheck remote integrity after repair, before clearing only this owned bucket.
env=__import__('os').environ.copy();env.update(e);p=subprocess.run(['restic','check','--read-data'],env=env,capture_output=True,text=True,timeout=120);assert p.returncode==0,p.stderr[:400]
assert request('GET',base+'?list-type=2',r/'objects.xml')==200
xml=ET.parse(r/'objects.xml');ns={'s':'http://s3.amazonaws.com/doc/2006-03-01/'};assert xml.findtext('s:IsTruncated',namespaces=ns)=='false'
objects=[(x.findtext('s:Key',namespaces=ns),int(x.findtext('s:Size',namespaces=ns))) for x in xml.findall('s:Contents',ns)];assert objects and all(k.startswith('postgresql17/') for k,_ in objects)
for k,_ in objects:assert request('DELETE',base+'/'+k,'/dev/null')==204
assert request('HEAD',base,'/dev/null')==200
subprocess.run([cf,'accounts','tokens','delete',o['token_id'],'--force'],capture_output=True,text=True,check=True)
for _ in range(20):
 status=request('HEAD',base,'/dev/null')
 if status in [401,403]:break
 time.sleep(2)
assert status in [401,403],status
subprocess.run([cf,'r2','buckets','delete',o['bucket'],'--force'],capture_output=True,text=True,check=True)
after=json.loads(subprocess.check_output([cf,'r2','buckets','list'],text=True));before=json.loads(Path('/tmp/vfy-r2-before.json').read_text());assert sorted(x['name'] for x in after['buckets'])==sorted(x['name'] for x in before['buckets'])
result={'owned_objects_deleted':len(objects),'stored_bytes':sum(n for _,n in objects),'integrity_after_repair':'passed','credential_revoked_http':status,'owned_bucket_removed':True,'existing_buckets_unchanged':True,'credential_bucket_scoped':True,'credential_max_lifetime_hours':2,'repository':'encrypted restic on real Cloudflare R2 S3 endpoint'}
Path('docs/validation/2026-11-04-r2-qualification/cleanup.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))

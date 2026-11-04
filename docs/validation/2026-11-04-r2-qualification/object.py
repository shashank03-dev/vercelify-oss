from pathlib import Path
import json,subprocess
r=Path('target/r2-qualification');e=json.loads((r/'environment.json').read_text());owned=json.loads((r/'owned.json').read_text());pack=(r/'pack-id').read_text().strip()
url=f"https://{owned['account']}.r2.cloudflarestorage.com/{owned['bucket']}/postgresql17/data/{pack[:2]}/{pack}"
def request(method,path):
 config=f'url = "{url}"\naws-sigv4 = "aws:amz:auto:s3"\nuser = "{e["AWS_ACCESS_KEY_ID"]}:{e["AWS_SECRET_ACCESS_KEY"]}"\nsilent\nshow-error\nmax-time = 30\nwrite-out = "%{{http_code}}"\n'
 if method=='GET':config+=f'output = "{path}"\n'
 else:config+=f'upload-file = "{path}"\noutput = "/dev/null"\n'
 p=subprocess.run(['curl','--config','-'],input=config,capture_output=True,text=True,check=True);assert p.stdout=='200',(method,p.stdout,p.stderr[:200])
if __name__=='__main__':
 import sys
 if sys.argv[1]=='corrupt':
  request('GET',r/'pack-original');data=bytearray((r/'pack-original').read_bytes());data[len(data)//2]^=255;(r/'pack-corrupt').write_bytes(data);request('PUT',r/'pack-corrupt');print('Owned R2 test pack corrupted for negative recovery check')
 elif sys.argv[1]=='repair':request('PUT',r/'pack-original');print('Original test pack bytes restored')

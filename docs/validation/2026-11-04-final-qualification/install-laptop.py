"""Upgrade the authorized physical host, preserving workloads and rollback state."""
import subprocess
from pathlib import Path
remote=r'''
import datetime,hashlib,json,os,shutil,sqlite3,subprocess,time,urllib.request,threading,http.client
from pathlib import Path
root=Path('/var/lib/vercelify/qualification-final-20261104T022650Z');binary=Path('/usr/local/bin/vercelify-agent');dbpath=Path('/var/lib/vercelify/platform.db');candidate=root/'vercelify-agent.streaming'
assert hashlib.sha256(candidate.read_bytes()).hexdigest()=='a38bff60f3cb810d86523570056aeb2a27915cbffa2ba89171f18d8a9a9a299b'
protected=['vfy-gw-pr1-re0wxze2','vfy-db-gwdb','vfy-gw-2cv2ryfw','vercelify-proxy','voxbento-mediamtx-1']
def inventory():
 rows=json.loads(subprocess.check_output(['docker','inspect',*protected]));return {r['Name'].lstrip('/'):{'id':r['Id'],'running':r['State']['Running'],'restarts':r['RestartCount']} for r in rows}
before=inventory();assert all(x['running'] for x in before.values())
with sqlite3.connect(str(dbpath)) as db:
 assert db.execute('pragma user_version').fetchone()[0]>=8
 retire=db.execute("select value from settings where key='preview_safety_retire'").fetchone();assert not retire or json.loads(retire[0])==[], 'unsafe legacy preview retirement is pending'
 urls=['gw.10.77.0.1.sslip.io']+[r[0].split('://')[-1].split('/')[0] for r in db.execute("select url from previews where status='active'")]
baseline={}
for host in urls:
 c=http.client.HTTPConnection('127.0.0.1',80,timeout=3);c.request('GET','/',headers={'Host':host});r=c.getresponse();r.read();baseline[host]=r.status;c.close()
assert baseline['gw.10.77.0.1.sslip.io']==200
assert all(code in (200,403) for code in baseline.values()), baseline
stop=threading.Event();probes=[]
def monitor():
 while not stop.is_set():
  for host in urls:
   try:
    c=http.client.HTTPConnection('127.0.0.1',80,timeout=3);c.request('GET','/',headers={'Host':host});r=c.getresponse();r.read();status=r.status;c.close()
   except Exception:status=0
   probes.append({'time':time.time(),'host':host,'status':status})
  stop.wait(.5)
thread=threading.Thread(target=monitor,daemon=True);thread.start()
backup=root/'pre-final-install-upload-fix';backup.mkdir(mode=0o700);shutil.copy2(binary,backup/'vercelify-agent');shutil.copy2('/etc/vercelify/agent.toml',backup/'agent.toml')
def service(action):subprocess.run(['systemctl',action,'vercelify-agent'],check=True)
try:
 service('stop')
 with sqlite3.connect(str(dbpath)) as src,sqlite3.connect(str(backup/'platform.db')) as dst:src.backup(dst)
 os.chmod(backup/'platform.db',0o600)
 temporary=binary.with_name('vercelify-agent.qualification-new');shutil.copyfile(candidate,temporary);temporary.chmod(0o755);os.replace(temporary,binary);service('start')
 for attempt in range(60):
  try:
   health=json.load(urllib.request.urlopen('http://127.0.0.1:7420/v1/health',timeout=2))
   with sqlite3.connect(str(dbpath)) as db:version=db.execute('pragma user_version').fetchone()[0]
   if version==13 and 'preview-guest-links' in health.get('capabilities',[]):break
  except Exception:pass
  time.sleep(1)
 else:raise RuntimeError('candidate readiness deadline')
 time.sleep(10);after=inventory();assert before==after,(before,after)
 assert all(p['status']==baseline[p['host']] for p in probes), 'serving route changed from baseline'
 print(json.dumps({'installed_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'architecture':subprocess.check_output(['uname','-m'],text=True).strip(),'schema':version,'protected_before':before,'protected_after':after,'probes':len(probes),'baseline_status':baseline,'probe_failures':sum(p['status']!=baseline[p['host']] for p in probes),'rollback_backup':str(backup)}))
except BaseException:
 service('stop');shutil.copy2(backup/'vercelify-agent',binary)
 if (backup/'platform.db').exists():
  for suffix in ['-wal','-shm']:dbpath.with_name(dbpath.name+suffix).unlink(missing_ok=True)
  shutil.copy2(backup/'platform.db',dbpath)
 service('start');raise
finally:
 stop.set();thread.join();(root/'upgrade-probes-upload-fix.json').write_text(json.dumps(probes));(root/'upgrade-probes-upload-fix.json').chmod(0o600)
'''
with (Path(__file__).parent/'laptop-install-final.txt').open('w') as log:
 subprocess.run(['/Applications/Tailscale.app/Contents/MacOS/Tailscale','ssh','user@100.123.113.77','sudo -n python3 -'],input=remote,text=True,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=150)

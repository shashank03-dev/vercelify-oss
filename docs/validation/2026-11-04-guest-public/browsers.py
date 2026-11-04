from pathlib import Path
import os,subprocess,json
exec(Path('target/guest-public-qualification/qualify.py').read_text().split("inv=issue();x=cloud('probe',inv)")[0])
for engine in ['firefox','webkit']:
 inv=issue(True)
 inner("f['browser_invitation']="+repr(inv)+"\nf['preview_container']=h['sql']('SELECT container FROM instances WHERE deployment_id=(SELECT deployment_id FROM previews WHERE app_id=? AND pr_number=7) ORDER BY rowid DESC LIMIT 1',(f['app_id'],))[0][0]\nh['save'](f)")

 for _ in range(20):
  ready=subprocess.run(['curl','-s','--max-time','5','-o','/dev/null','-w','%{http_code}', 'https://'+inv['grant']['host']+'/__vfy/invite'],capture_output=True,text=True)
  if ready.stdout=='200' and ready.returncode==0:break
  time.sleep(1)
 else:raise AssertionError('public browser hostname not ready')
 env=os.environ.copy();env['VFY_GUEST_BROWSER']=engine
 p=subprocess.run(['node','docs/validation/2026-11-04-guest-public/browser.cjs'],env=env,capture_output=True,text=True,timeout=90)
 (out/('browser-'+engine+'.txt')).write_text(p.stdout+p.stderr);assert p.returncode==0,engine+' browser qualification failed';print(engine+' public guest browser passed',flush=True)

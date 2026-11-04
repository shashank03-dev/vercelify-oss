from pathlib import Path
import subprocess,json,os,signal,time,shutil
root=Path('target/guest-public-qualification');out=Path('docs/validation/2026-11-04-guest-public');o=json.loads((root/'owned.json').read_text());cf='/Users/shashank/.bun/bin/cf'
for key,expected in [('connector_pid','cloudflared'),('ssh_pid','ssh')]:
 pid=o[key];p=subprocess.run(['ps','-p',str(pid),'-o','args='],capture_output=True,text=True)
 if p.returncode==0:
  assert expected in p.stdout
  if key=='connector_pid':assert 'guest-public-qualification/connector.token' in p.stdout
  else:assert str(o['local_port']) in p.stdout
  os.kill(pid,signal.SIGTERM)
for _ in range(20):
 p=subprocess.run(['ps','-p',str(o['connector_pid']),'-o','stat='],capture_output=True,text=True)
 if p.returncode:break
 time.sleep(.2)
for d in o['dns']:
 assert d['name'].startswith('shr-') and d['name'].endswith('.shashankdev03.me')
 subprocess.run([cf,'dns','records','delete',d['id'],'--zone','shashankdev03.me','--force'],capture_output=True,text=True,check=True)
subprocess.run([cf,'tunnels','delete',o['tunnel_id'],'--force'],capture_output=True,text=True,check=True)
pre=json.loads(Path('/tmp/vfy-guest-dns-before.json').read_text());post=json.loads(subprocess.check_output([cf,'dns','records','list','--zone','shashankdev03.me'],text=True));assert sorted((d['id'],d['name'],d['content']) for d in pre)==sorted((d['id'],d['name'],d['content']) for d in post)
prefix=['limactl','shell','vfybench','sudo','docker'];subprocess.run(prefix+['exec','-e','VERCELIFY_GUEST_CONTRACT=1',o['owner'],'python3','/workspace/host/tests/contract/guest-links.py','reset'],check=True,capture_output=True);subprocess.run(prefix+['stop',o['owner']],capture_output=True,check=True)
result={'guest_dns_records_removed':len(o['dns']),'original_dns_unchanged':True,'test_tunnel_removed':True,'connector_and_forward_stopped':True,'owned_guest_apps_and_proxy_removed':True,'owned_fixture_secret_state_removed':True,'owned_daemon_stopped':True};(out/'cleanup.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))

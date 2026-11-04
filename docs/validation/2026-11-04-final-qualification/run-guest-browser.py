"""Real browser over an owned localhost SSH forward; local test DNS/TLS only."""
from pathlib import Path
import subprocess,json,time,socket
owner='vfy-final-qualification-docker';prefix=['limactl','shell','vfybench','--','sudo','docker','exec']
helper=prefix+['-e','VERCELIFY_GUEST_CONTRACT=1','-e','AGENT_BIN=/workspace/target/aarch64-unknown-linux-musl/release/vercelify-agent',owner,'python3','/workspace/host/tests/contract/guest-links.py']
subprocess.run(helper+['reset'],check=True,stdout=subprocess.DEVNULL)
subprocess.run(helper+['start'],check=True,stdout=subprocess.DEVNULL)
with socket.socket() as sock:
 sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
remote='PORT='+str(port)+'\n'+r'''
import json,urllib.request
from pathlib import Path
p=Path('/var/lib/vfy-guest-contract/facts.json');s=json.loads(p.read_text())
def api(method,path,body):
 r=urllib.request.Request('http://127.0.0.1:17430/v1'+path,data=json.dumps(body).encode(),method=method,headers={'Authorization':'Bearer guest-contract-only-2026','Content-Type':'application/json'})
 return json.load(urllib.request.urlopen(r))
api('PUT','/platform/settings/guest-links',{'domain':'guests.example.test','public_port':PORT})
s['browser_invitation']=api('POST','/platform/apps/'+s['app_id']+'/previews/7/guest-links',{'expires_in':3600,'single_use':True});p.write_text(json.dumps(s));p.chmod(0o600)
'''
subprocess.run(prefix+['-i',owner,'python3','-'],input=remote,text=True,check=True)
ip=subprocess.check_output(['limactl','shell','vfybench','--','sudo','docker','inspect',owner,'--format','{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}'],text=True).strip()
forward=subprocess.Popen(['ssh','-F',str(Path.home()/'.lima/vfybench/ssh.config'),'-S','none','-N','-o','ExitOnForwardFailure=yes','-L','127.0.0.1:'+str(port)+':'+ip+':18443','lima-vfybench'],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
try:
 time.sleep(2);assert forward.poll() is None, forward.stderr.read().decode()
 with (Path(__file__).parent/'guest-browser.txt').open('w') as log:subprocess.run(['node',str(Path(__file__).parent/'guest-browser.cjs')],stdout=log,stderr=subprocess.STDOUT,check=True,timeout=120)
finally:
 forward.terminate();forward.wait(timeout=10)
 subprocess.run(prefix+['-e','VERCELIFY_GUEST_CONTRACT=1',owner,'python3','/workspace/host/tests/contract/guest-links.py','stop'],check=True)

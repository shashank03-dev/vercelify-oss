"""Expose only an owned disposable private preview for negative external tests."""
from pathlib import Path
import subprocess,json,socket,time,os,re
out=Path(__file__).parent;work=Path('target/public-qualification');owner='vfy-final-qualification-docker';prefix=['limactl','shell','vfybench','--','sudo','docker']
subprocess.run(prefix+['start',owner],stdout=subprocess.DEVNULL,check=True)
for _ in range(60):
 if subprocess.run(prefix+['exec',owner,'docker','info'],capture_output=True).returncode==0:break
 time.sleep(1)
else:raise RuntimeError('owned nested daemon readiness deadline')
helper=prefix+['exec','-e','VERCELIFY_GUEST_CONTRACT=1','-e','AGENT_BIN=/workspace/target/aarch64-unknown-linux-musl/release/vercelify-agent',owner,'python3','/workspace/host/tests/contract/guest-links.py']
subprocess.run(helper+['reset'],stdout=subprocess.DEVNULL,check=True);subprocess.run(helper+['start'],stdout=subprocess.DEVNULL,check=True,timeout=180)
host=subprocess.check_output(prefix+['exec',owner,'python3','-c',"import sqlite3,urllib.parse;db=sqlite3.connect('/var/lib/vfy-guest-contract/state/platform.db');print(urllib.parse.urlsplit(db.execute('select url from previews where pr_number=7').fetchone()[0]).hostname)"],text=True).strip()
ip=subprocess.check_output(prefix+['inspect',owner,'--format','{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}'],text=True).strip()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
sshlog=(work/'private-forward.log').open('w');ssh=subprocess.Popen(['ssh','-F',str(Path.home()/'.lima/vfybench/ssh.config'),'-S','none','-N','-o','ExitOnForwardFailure=yes','-L',f'127.0.0.1:{port}:{ip}:18080','lima-vfybench'],stdout=sshlog,stderr=sshlog,start_new_session=True);sshlog.close();time.sleep(1);assert ssh.poll() is None
log=(work/'private-tunnel.log').open('w');tunnel=subprocess.Popen(['cloudflared','tunnel','--url',f'http://127.0.0.1:{port}','--http-host-header',host,'--protocol','http2','--edge-ip-version','4','--no-autoupdate'],stdout=log,stderr=log,start_new_session=True);log.close()
for _ in range(90):
 content=(work/'private-tunnel.log').read_text();matches=re.findall(r'https://[a-z0-9-]+\.trycloudflare\.com',content)
 if matches:break
 assert tunnel.poll() is None;time.sleep(1)
else:raise RuntimeError('private probe tunnel hostname deadline')
facts={'private_host':host,'public_probe_url':matches[0],'ssh_pid':ssh.pid,'cloudflared_pid':tunnel.pid,'owner_container':owner,'local_port':port};(work/'private-probe-owned.json').write_text(json.dumps(facts));(out/'private-probe.json').write_text(json.dumps(facts,indent=2)+'\n');print(json.dumps(facts))

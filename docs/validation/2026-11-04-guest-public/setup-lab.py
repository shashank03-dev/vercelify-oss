from pathlib import Path
import subprocess,json,os,socket,time
os.umask(0o077);root=Path('target/guest-public-qualification');prefix=['limactl','shell','vfybench','sudo','docker'];owner='vfy-final-qualification-docker'
code=r'''
import runpy,pathlib,subprocess,json
h=runpy.run_path('/workspace/host/tests/contract/guest-links.py',run_name='helper');r=h['ROOT'];d=r/'state/proxy/dynamic';certs=d/'qualification-certs';certs.mkdir(exist_ok=True)
def cmd(*args):subprocess.run(['openssl',*args],check=True,capture_output=True)
cmd('req','-x509','-newkey','rsa:2048','-nodes','-keyout',str(certs/'ca.key'),'-out',str(certs/'ca.pem'),'-days','1','-subj','/CN=Vercelify owned qualification CA','-addext','basicConstraints=critical,CA:TRUE')
cmd('req','-new','-newkey','rsa:2048','-nodes','-keyout',str(certs/'origin.key'),'-out',str(certs/'origin.csr'),'-subj','/CN=*.shashankdev03.me')
(certs/'extensions').write_text('subjectAltName=DNS:*.shashankdev03.me\nbasicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n')
cmd('x509','-req','-in',str(certs/'origin.csr'),'-CA',str(certs/'ca.pem'),'-CAkey',str(certs/'ca.key'),'-CAcreateserial','-out',str(certs/'origin.pem'),'-days','1','-extfile',str(certs/'extensions'))
(d/'qualification-tls.yml').write_text('tls:\n  certificates:\n    - certFile: /etc/traefik/dynamic/qualification-certs/origin.pem\n      keyFile: /etc/traefik/dynamic/qualification-certs/origin.key\n')
for p in certs.iterdir():p.chmod(0o600)
h['api']('PUT','/platform/settings/guest-links',{'domain':'shashankdev03.me','public_port':443});f=h['facts']();f['browser_invitation']=h['issue'](True);h['save'](f)
print(json.dumps({'host':f['browser_invitation']['grant']['host']}))
'''
p=subprocess.run(prefix+['exec','-i','-e','VERCELIFY_GUEST_CONTRACT=1',owner,'python3','-'],input=code,capture_output=True,text=True,check=True);facts=json.loads(p.stdout)
ca=subprocess.check_output(prefix+['exec',owner,'cat','/var/lib/vfy-guest-contract/state/proxy/dynamic/qualification-certs/ca.pem']);(root/'origin-ca.pem').write_bytes(ca)
ip=subprocess.check_output(prefix+['inspect',owner,'--format','{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}'],text=True).strip()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
log=(root/'ssh.log').open('ab');p=subprocess.Popen(['ssh','-F',str(Path.home()/'.lima/vfybench/ssh.config'),'-S','none','-N','-o','ExitOnForwardFailure=yes','-L',f'127.0.0.1:{port}:{ip}:18443','lima-vfybench'],stdout=log,stderr=log,start_new_session=True);log.close();facts.update(ssh_pid=p.pid,local_port=port,owner=owner);(root/'owned.json').write_text(json.dumps(facts));print(json.dumps(facts))

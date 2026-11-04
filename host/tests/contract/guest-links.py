#!/usr/bin/env python3
"""Owns only a disposable nested daemon: VERCELIFY_GUEST_CONTRACT=1.

No invitation/cookie is printed. Private handoff facts remain mode 0600 outside
this repository. start/qualify/stop permit a real external browser between phases.
"""
import base64, datetime, hashlib, http.client, json, os, secrets, shutil, signal, socket, sqlite3, ssl, subprocess, sys, threading, time
from pathlib import Path
ROOT=Path('/var/lib/vfy-guest-contract')
TOKEN='guest-contract-only-2026'
API=17430; WAKE=17431; HTTP=18080; HTTPS=18443
IMAGE='vfy-guest-contract:local'
assert os.environ.get('VERCELIFY_GUEST_CONTRACT')=='1'
assert not Path('/etc/vercelify').exists(),'disposable daemon only'
def docker(args,**kw):return subprocess.run(['docker',*args],check=True,capture_output=True,**kw)
def api(method,path,body=None):
    c=http.client.HTTPConnection('127.0.0.1',API,timeout=30)
    payload=None if body is None else json.dumps(body)
    c.request(method,'/v1'+path,payload,{'Authorization':'Bearer '+TOKEN,'Content-Type':'application/json'})
    r=c.getresponse();raw=r.read();c.close()
    if r.status>=400:raise RuntimeError('API '+method+' '+path+' returned '+str(r.status))
    return json.loads(raw) if raw else None
def sql(statement,args=()):
    with sqlite3.connect(ROOT/'state/platform.db',timeout=10) as db:return db.execute(statement,args).fetchall()
def facts():return json.loads((ROOT/'facts.json').read_text())
def save(value):
    p=ROOT/'facts.json';p.write_text(json.dumps(value));p.chmod(0o600)
def start_agent():
    env=os.environ.copy();env.update(VERCELIFY_CONFIG=str(ROOT/'agent.toml'),VERCELIFY_ENGINE='platform',VERCELIFY_ROLE='main',VERCELIFY_LISTEN='0.0.0.0',VERCELIFY_PORT=str(API),VERCELIFY_TOKEN=TOKEN,VERCELIFY_STATE_DIR=str(ROOT/'state'),VERCELIFY_RUN_DIR=str(ROOT/'run'),VERCELIFY_PLATFORM_DB=str(ROOT/'state/platform.db'),VERCELIFY_MASTER_KEY_FILE=str(ROOT/'state/master.key'),VERCELIFY_PROXY_HTTP_PORT=str(HTTP),VERCELIFY_PROXY_HTTPS_PORT=str(HTTPS),VERCELIFY_PROXY_METRICS_PORT='18082',VERCELIFY_WATCH_INTERVAL='0',VERCELIFY_AUTODEPLOY_INTERVAL='0',VERCELIFY_WAKE_LISTEN='0.0.0.0:'+str(WAKE),VERCELIFY_TOKENS_FILE=str(ROOT/'state/tokens.json'),VERCELIFY_AUDIT_LOG=str(ROOT/'state/audit.log'),VERCELIFY_MESH_DIR=str(ROOT/'mesh'))
    log=(ROOT/'agent.log').open('ab')
    process=subprocess.Popen([os.environ['AGENT_BIN'],'serve'],env=env,stdout=log,stderr=log);log.close();(ROOT/'pid').write_text(str(process.pid))
    for _ in range(90):
        try:
            ready=api('GET','/platform/settings/guest-links')['gateway_available']
            cap=api('GET','/platform/capacity')
            if ready and cap and cap[0].get('sample'):break
        except (OSError,ValueError,RuntimeError):pass
        time.sleep(1)
    else:raise RuntimeError('disposable agent/inventory did not become ready')
    sql("INSERT INTO settings(key,value,updated_at) VALUES('mesh_ingress',?,datetime('now')) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",(json.dumps({'node':'main','cidrs':['10.77.0.0/16','fd76:6679:6d65::/48']}),))
    return process

def wait_deploy(id):
    for _ in range(90):
        d=api('GET','/platform/deployments/'+id)
        if d['status']=='healthy':return d
        if d['status']=='failed':raise RuntimeError('fixture deploy failed: '+d.get('error',''))
        time.sleep(1)
    raise RuntimeError('fixture deploy timed out')
def issue(single=False):
    f=facts();return api('POST',f"/platform/apps/{f['app_id']}/previews/7/guest-links",{'expires_in':3600,'single_use':single})
def request(host,path='/',cookie=None,method='GET',body=None,extra=None):
    c=http.client.HTTPSConnection('127.0.0.1',HTTPS,timeout=5,context=ssl._create_unverified_context())
    headers={'Host':host+':'+str(HTTPS)}
    if cookie:headers['Cookie']=cookie
    if extra:headers.update(extra)
    if body is not None:body=json.dumps(body);headers['Content-Type']='application/json'
    c.request(method,path,body,headers);r=c.getresponse();data=r.read();result=(r.status,{k.lower():v for k,v in r.getheaders()},data);c.close();return result
def redeem(invitation):
    host=invitation['grant']['host'];token=invitation['url'].split('#vfy=',1)[1]
    status,headers,_=request(host,'/__vfy/redeem',method='POST',body={'token':token},extra={'Origin':'https://'+host+':'+str(HTTPS)})
    assert status==204,'redemption failed with '+str(status)
    return headers['set-cookie'].split(';',1)[0]
def masked_frame(data):
    mask=secrets.token_bytes(4);payload=data.encode();assert len(payload)<126
    return bytes([0x81,0x80|len(payload)])+mask+bytes(v^mask[i%4] for i,v in enumerate(payload))
def websocket(host,cookie):
    raw=socket.create_connection(('127.0.0.1',HTTPS),timeout=5);stream=ssl._create_unverified_context().wrap_socket(raw,server_hostname=host);stream.settimeout(4)
    key=base64.b64encode(secrets.token_bytes(16)).decode()
    stream.sendall((f'GET /ws HTTP/1.1\r\nHost: {host}:{HTTPS}\r\nCookie: {cookie}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n\r\n').encode())
    head=b''
    while b'\r\n\r\n' not in head:head+=stream.recv(1)
    assert head.startswith(b'HTTP/1.1 101'),'websocket upgrade failed'
    stream.sendall(masked_frame('hello'));echo=b''
    while len(echo)<7:echo+=stream.recv(7-len(echo))
    assert echo==b'\x81\x05hello','websocket did not echo'
    return stream

def stop():
    if (ROOT/'pid').exists():
        pid=int((ROOT/'pid').read_text())
        if Path(f'/proc/{pid}/comm').exists() and Path(f'/proc/{pid}/comm').read_text().strip()=='vercelify-agent':
            os.kill(pid,signal.SIGTERM)
            for _ in range(50):
                if not Path(f'/proc/{pid}/comm').exists():break
                time.sleep(.1)
    if (ROOT/'facts.json').exists():
        f=facts()
        for app in [f.get('app_id'),f.get('production_id')]:
            if app:
                ids=docker(['ps','-aq','--filter','label=vercelify.app='+app],text=True).stdout.split()
                if ids:docker(['rm','-f',*ids])
    subprocess.run(['docker','rm','-f','vercelify-proxy'],capture_output=True)
    print('PASS: owned agent/apps/proxy stopped; private synthetic state retained for review')

def start():
    if (ROOT/'pid').exists():raise RuntimeError('stop the previous owned fixture before starting again')
    ROOT.mkdir(mode=0o700,parents=True,exist_ok=True);(ROOT/'state').mkdir(exist_ok=True);(ROOT/'run').mkdir(exist_ok=True)
    (ROOT/'agent.toml').write_text('build_concurrency=0\nmesh_coordinator=false\n')
    source=ROOT/'source';source.mkdir(exist_ok=True)
    (source/'Dockerfile').write_text('FROM node:22-slim\nCOPY server.mjs /server.mjs\nEXPOSE 8080\nCMD ["node","/server.mjs"]\n')
    (source/'server.mjs').write_text((Path(__file__).parent/'guest_app.mjs').read_text())
    docker(['build','-q','-t',IMAGE,str(source)],env={**os.environ,'DOCKER_BUILDKIT':'0'})
    start_agent()
    prod=api('POST','/platform/apps',{'name':'guest-sentinel','kind':'image','source':{'image':IMAGE},'runtime':{'port':8080,'memory_mb':128,'cpus':1,'drain_secs':0,'health':{'path':'/health'}}})
    app=api('POST','/platform/apps',{'name':'guest-preview','kind':'image','source':{'image':IMAGE},'runtime':{'port':8080,'memory_mb':128,'cpus':1,'drain_secs':0,'health':{'path':'/health'}},'previews':{'enabled':True,'access':'private','max':3}})
    save({'app_id':app['id'],'production_id':prod['id']})
    api('POST',f"/platform/apps/{prod['id']}/domains",{'host':'guest-sentinel.test','https':False,'primary':True})
    wait_deploy(api('POST',f"/platform/apps/{prod['id']}/deploy",{})['id'])
    # The HTTP manual-deploy endpoint intentionally deploys production only.
    # Seed the same queued PR fixture records Git observation creates, then let
    # the real agent dispatch/admit/start/publish the image. No production API
    # or authentication rule is weakened to make test setup convenient.
    alphabet='0123456789abcdefghjkmnpqrstvwxyz'
    identifier=lambda prefix:prefix+'_'+''.join(secrets.choice(alphabet) for _ in range(16))
    now=datetime.datetime.now(datetime.timezone.utc).isoformat(timespec='seconds').replace('+00:00','Z')
    dep=identifier('dpl')
    with sqlite3.connect(ROOT/'state/platform.db',timeout=10) as db:
        db.execute("INSERT INTO previews(id,app_id,pr_number,head_sha,title,created_at) VALUES(?,?,7,?,?,?)",(identifier('prv'),app['id'],'a'*40,'guest privacy fixture',now))
        db.execute("INSERT INTO deployments(id,app_id,number,kind,pr_number,branch,commit_sha,commit_message,trigger,image,status,created_at) VALUES(?,?,1,'preview',7,'main',?,'guest privacy fixture','manual',?,'queued',?)",(dep,app['id'],'a'*40,IMAGE,now))
    d=wait_deploy(dep)
    api('PUT','/platform/settings/guest-links',{'domain':'guests.example.test','public_port':HTTPS})
    invitation=issue(True);f=facts();f.update(browser_invitation=invitation,preview_deployment=d['id'],preview_container=sql('SELECT container FROM instances WHERE deployment_id=? ORDER BY rowid DESC LIMIT 1',(d['id'],))[0][0]);save(f)
    print('PASS: real agent, production sentinel and private preview ready; invitation retained only in mode-0600 facts.json')

def qualify():
    f=facts();stop_probes=threading.Event();counts={'requests':0,'failures':0}
    def probes():
        while not stop_probes.is_set():
            try:
                c=http.client.HTTPConnection('127.0.0.1',HTTP,timeout=2);c.request('GET','/',headers={'Host':'guest-sentinel.test'});r=c.getresponse();assert r.status==200;r.read();c.close()
            except Exception:counts['failures']+=1
            counts['requests']+=1;stop_probes.wait(.1)
    thread=threading.Thread(target=probes,daemon=True);thread.start()
    try:
        invitation=issue();host=invitation['grant']['host']
        for path in ['/','/headers','/assets/private.js','/ws','/v1/platform/apps']:
            assert request(host,path)[0]==401
            assert request(host,path,extra={'X-Forwarded-For':'10.77.0.2','X-Vfy-Bridge':'forged','Authorization':'Basic Zm9yZ2VkOmZvcmNlZA=='})[0] in [401,403,404]
        cookie=redeem(invitation)
        for _ in range(15):
            status,headers,body=request(host,'/headers',cookie,extra={'Authorization':'Bearer app-only-fixture'})
            if status==200:break
            assert status in [401,404,503];time.sleep(.3)
        assert status==200
        seen=json.loads(body)['headers'];assert seen.get('authorization')=='Bearer app-only-fixture';assert not any(k.startswith('x-vfy-') for k in seen);assert '__Host-vfy_guest' not in seen.get('cookie','')
        assert 'no-store' in headers.get('cache-control','') and headers.get('referrer-policy')=='no-referrer'
        assert "worker-src 'none'" in headers.get('content-security-policy','')
        print('PASS: real TLS guest redemption, header/cookie stripping, app authorization preservation and cache/referrer/worker policy')
        stream=websocket(host,cookie);began=time.monotonic();api('DELETE','/platform/guest-links/'+invitation['grant']['id'])
        try:closed=not stream.recv(64)
        except (ConnectionError,ssl.SSLError):closed=True
        assert closed and time.monotonic()-began<3;stream.close()
        assert request(host,'/headers',cookie)[0]==401
        print('PASS: revoked invitation/cookie and live WebSocket fail closed within three seconds')
        invitation=issue();cookie=redeem(invitation);host=invitation['grant']['host']
        c=http.client.HTTPSConnection('127.0.0.1',HTTPS,timeout=4,context=ssl._create_unverified_context());c.request('GET','/stream',headers={'Host':host+':'+str(HTTPS),'Cookie':cookie});r=c.getresponse();assert r.status==200 and r.readline()==b'open\n'
        began=time.monotonic();api('DELETE','/platform/guest-links/'+invitation['grant']['id'])
        try:r.read()
        except (http.client.IncompleteRead,ConnectionError,ssl.SSLError):pass
        assert time.monotonic()-began<3;c.close();print('PASS: revocation terminates an already-streaming HTTP response')
        invitation=issue();cookie=redeem(invitation);host=invitation['grant']['host'];stream=websocket(host,cookie)
        sql('UPDATE guest_grants SET expires_at=? WHERE id=?',(int(time.time())+2,invitation['grant']['id']))
        began=time.monotonic()
        try:closed=not stream.recv(64)
        except (ConnectionError,ssl.SSLError):closed=True
        assert closed and time.monotonic()-began<4;stream.close();assert request(host,'/headers',cookie)[0]==401
        print('PASS: induced server-side expiry ends an upgraded socket and cannot be bypassed by its cookie')
    finally:
        stop_probes.set();thread.join();assert counts['failures']==0,counts;print(json.dumps({'unchanged_production_probes':counts}))

def lifecycle():
    def ready(host,cookie):
        for _ in range(40):
            if request(host,'/headers',cookie)[0]==200:return
            time.sleep(.25)
        raise AssertionError('session route did not recover')
    def restart(rotate=False):
        pid=int((ROOT/'pid').read_text());os.kill(pid,signal.SIGTERM)
        for _ in range(50):
            state=Path(f'/proc/{pid}/stat')
            if not state.exists() or state.read_text().split()[2]=='Z':break
            time.sleep(.1)
        if rotate:
            key=ROOT/'state/master.key';key.write_bytes(secrets.token_bytes(32));key.chmod(0o600)
        start_agent()
    f=facts();invitation=issue();host=invitation['grant']['host'];cookie=redeem(invitation)
    restart();ready(host,cookie)
    print('PASS: actual agent restart preserves a valid hashed guest session')
    restart(True);assert request(host,'/headers',cookie)[0]==401
    print('PASS: actual control master-key replacement permanently revokes guest sessions')
    for change in [{'previews':{'enabled':True,'access':'public','max':3}}, {'previews':{'enabled':False,'access':'private','max':3}}]:
        invitation=issue();cookie=redeem(invitation);host=invitation['grant']['host']
        api('PATCH','/platform/apps/'+f['app_id'],change)
        api('PATCH','/platform/apps/'+f['app_id'],{'previews':{'enabled':True,'access':'private','max':3}})
        assert request(host,'/headers',cookie)[0]==401
    print('PASS: privacy and preview enablement toggles cannot resurrect an old invitation')
    invitation=issue();cookie=redeem(invitation);host=invitation['grant']['host']
    api('DELETE',f"/platform/apps/{f['app_id']}/previews/7")
    assert request(host,'/headers',cookie)[0]==401
    print('PASS: PR close retires guest access on the real proxy path')

def reset():
    stop();assert str(ROOT)=='/var/lib/vfy-guest-contract';shutil.rmtree(ROOT,ignore_errors=True)

if __name__=='__main__':
    {'start':start,'qualify':qualify,'lifecycle':lifecycle,'stop':stop,'reset':reset}[sys.argv[1]]()

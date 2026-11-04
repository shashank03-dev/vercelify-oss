#!/usr/bin/env python3
"""Owned nested-daemon software qualification; no real machine power-off."""
import importlib.util, json, os, signal, subprocess, threading, time
from pathlib import Path
assert os.environ.get('VERCELIFY_POWER_CONTRACT')=='1'
assert not Path('/etc/vercelify').exists(),'disposable daemon only'
os.environ['VERCELIFY_GUEST_CONTRACT']='1'
root=Path('/var/lib/vfy-guest-contract')
power=root/'power-run/power-guard.state'
os.environ['VERCELIFY_POWER_GUARD_STATE']=str(power)
spec=importlib.util.spec_from_file_location('guest_fixture',Path(__file__).with_name('guest-links.py'))
f=importlib.util.module_from_spec(spec);spec.loader.exec_module(f)

def state(value):
    power.parent.mkdir(parents=True,exist_ok=True)
    power.write_text(json.dumps({'state':value,'reason':'synthetic lab battery','updated_at':f.datetime.datetime.now(f.datetime.timezone.utc).isoformat()}))
def sleep(app):return f.api('PUT','/sleep/'+app,{'enabled':True,'previews':True,'idle_minutes':5})
def running(container):
    p=f.docker(['inspect','-f','{{.State.Running}}',container],text=True)
    return p.stdout.strip()=='true'
def wait(check,label):
    for _ in range(45):
        if check():return
        time.sleep(.3)
    raise AssertionError(label)
def restart():
    pid=int((root/'pid').read_text());os.kill(pid,signal.SIGTERM)
    for _ in range(50):
        p=Path(f'/proc/{pid}/stat')
        if not p.exists() or p.read_text().split()[2]=='Z':break
        time.sleep(.1)
    f.start_agent()
def probe():
    c=f.http.client.HTTPConnection('127.0.0.1',f.HTTP,timeout=3);c.request('GET','/',headers={'Host':'guest-sentinel.test'});r=c.getresponse();assert r.status==200;r.read();c.close()

f.reset();extra=None;builder_started=False
stop_probes=threading.Event();counts={'requests':0,'failures':0};thread=None
def probe_loop():
    while not stop_probes.is_set():
        try:probe()
        except Exception:counts['failures']+=1
        counts['requests']+=1;stop_probes.wait(.1)
try:
    f.start();thread=threading.Thread(target=probe_loop,daemon=True);thread.start();facts=f.facts();app=facts['app_id'];preview=facts['preview_container'];state('ok')
    extra=f.api('POST','/platform/apps',{'name':'battery-noncritical','kind':'image','source':{'image':f.IMAGE},'runtime':{'port':8080,'memory_mb':128,'cpus':1,'drain_secs':0,'health':{'path':'/health'}}})['id']
    dep=f.wait_deploy(f.api('POST','/platform/apps/'+extra+'/deploy',{})['id'])
    container=f.sql('SELECT container FROM instances WHERE deployment_id=?',(dep['id'],))[0][0]
    sleep(app);sleep(extra)
    # Model a paused build's durable Building status. There is deliberately
    # no second deployment job or runtime for this synthetic journal record.
    paused='dpl_'+f.secrets.token_hex(8)
    created=f.datetime.datetime.now(f.datetime.timezone.utc).isoformat()
    f.sql("INSERT INTO deployments(id,app_id,number,kind,branch,commit_sha,commit_message,trigger,image,status,created_at) VALUES(?,?,9999,'production','main',?,'paused-build journal fixture','manual',?,'building',?)",(paused,extra,'b'*40,f.IMAGE,created))
    state('battery_preview_sleep');sleep(app)
    f.sql('DELETE FROM deployments WHERE id=?',(paused,))
    assert not running(preview) and running(container);probe()
    invitation=f.issue();cookie=f.redeem(invitation)
    status,_,body=f.request(invitation['grant']['host'],'/',cookie)
    assert status==503 and b'Waiting for power' in body
    assert not running(preview)
    print('PASS: preview tier sleeps only opted-in preview; authorized visit cannot bypass battery hold',flush=True)
    restart();sleep(app);assert not running(preview)
    journal=json.loads((root/'state/sleep.json').read_text());assert any(v.get('reason')=='battery' and v.get('deployment_id')==facts['preview_deployment'] for v in journal['asleep'].values())
    print('PASS: actual agent restart preserves generation-scoped power-sleep ownership',flush=True)
    state('battery_app_sleep');sleep(app);assert not running(container);probe()
    f.api('PATCH','/platform/apps/'+extra,{'desired':'stopped'})
    state('ok')
    # Fresh capacity/heartbeat can lag the guard transition; re-run the same
    # idempotent policy tick until admitted wake finishes.
    for _ in range(30):
        sleep(app)
        if running(preview):break
        time.sleep(.5)
    assert running(preview) and not running(container)
    assert f.api('GET','/platform/apps/'+extra)['desired']=='stopped';probe()
    print('PASS: stable recovery wakes matching preview through admission; manual stop wins for production',flush=True)
    state('battery_preview_sleep');sleep(app);assert not running(preview)
    f.api('DELETE',f'/platform/apps/{app}/previews/7')
    state('ok');sleep(app);probe()
    assert f.sql('SELECT status FROM previews WHERE app_id=? AND pr_number=7',(app,))[0][0]=='closed'
    print('PASS: PR close during battery sleep cannot be undone by recovery',flush=True)
    # Exercise the independent shell guard with an actual Docker builder.
    name='buildx_buildkit_vercelify0'
    p=subprocess.run(['docker','inspect','-f','{{.State.Running}}',name],capture_output=True,text=True)
    if p.returncode==0:
        # This daemon is owned exclusively by the contract. Its idle builder
        # may have auto-restarted when the nested daemon was restarted.
        if p.stdout.strip()=='true':f.docker(['stop',name])
        f.docker(['start',name]);builder_started=True
    else:
        f.docker(['run','-d','--name',name,'--memory=128m','--cpus=1',f.IMAGE]);builder_started=True
    sysfs=root/'power-sys';(sysfs/'AC').mkdir(parents=True);(sysfs/'BAT0').mkdir()
    for path,value in [('AC/type','Mains'),('AC/online','0'),('BAT0/type','Battery'),('BAT0/present','1'),('BAT0/capacity','35'),('BAT0/status','Discharging')]: (sysfs/path).write_text(value)
    config=root/'battery.conf';config.write_text('BATTERY_POLICY_ENABLED=1\nBATTERY_RECOVERY_AC_SECONDS=1\n')
    env={**os.environ,'POWER_GUARD_CONFIG':str(config),'POWER_GUARD_LOCAL_CONFIG':str(root/'no-overrides'),'POWER_SUPPLY_DIR':str(sysfs),'SYS_CLASS_DIR':str(root/'empty-sensors'),'STATE_DIR':str(power.parent),'PERSIST_DIR':str(root/'guard'),'SHUTDOWN_CMD':'/bin/false'}
    def guard():subprocess.run(['bash','/workspace/host/power-guard.sh','--once'],env=env,check=True,capture_output=True)
    guard();guard();assert f.docker(['inspect','-f','{{.State.Paused}}',name],text=True).stdout.strip()=='true';probe()
    (sysfs/'AC/online').write_text('1');(sysfs/'BAT0/capacity').write_text('80');guard();time.sleep(1.1);guard()
    assert f.docker(['inspect','-f','{{.State.Paused}}',name],text=True).stdout.strip()=='false';probe()
    print('PASS: independent guard pauses/resumes the actual owned builder with AC debounce; serving sentinel remains healthy',flush=True)
finally:
    if builder_started:subprocess.run(['docker','unpause','buildx_buildkit_vercelify0'],capture_output=True);subprocess.run(['docker','stop','buildx_buildkit_vercelify0'],capture_output=True)
    if extra:
        ids=f.docker(['ps','-aq','--filter','label=vercelify.app='+extra],text=True).stdout.split()
        if ids:f.docker(['rm','-f',*ids])
    stop_probes.set()
    if thread:thread.join();print(json.dumps({'unchanged_production_probes':counts}),flush=True)
    f.stop()
    assert counts['failures']==0,counts

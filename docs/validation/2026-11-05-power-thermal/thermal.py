import glob,json,time,datetime,subprocess,os,signal,pathlib
out=pathlib.Path('/var/lib/vercelify/qualification-power-thermal-20261105/thermal.jsonl')
def temps():
    t={}
    for z in glob.glob('/sys/class/thermal/thermal_zone*'):
        try: t[open(z+'/type').read().strip()]=int(open(z+'/temp').read())//1000
        except: pass
    for h in glob.glob('/sys/class/hwmon/hwmon*/temp*_input'):
        try:
            n=open(os.path.dirname(h)+'/name').read().strip()
            v=int(open(h).read())//1000
            if 0<v<150: t[n+':'+os.path.basename(h)]=v
        except: pass
    return t
def state():
    try: return open('/run/vercelify/power-guard.state').read().strip()[:200]
    except Exception as e: return str(e)
procs=[subprocess.Popen(['sh','-c','while :; do :; done']) for _ in range(os.cpu_count())]
start=time.time(); reason='time limit'
with open(out,'a') as f:
    while time.time()-start<1500:
        t=temps(); m=max(t.values())
        frz=subprocess.run(['grep','-c','frozen','/proc/1/status'],capture_output=True)
        f.write(json.dumps({'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'max':m,'t':t,'guard':state(),'mhz':open('/proc/cpuinfo').read().count('MHz') and [l.split(':')[1].strip() for l in open('/proc/cpuinfo') if l.startswith('cpu MHz')]})+'\n'); f.flush()
        if m>=100: reason=f'watchdog {m}C'; break
        time.sleep(5)
for p in procs: p.kill()
with open(out,'a') as f: f.write(json.dumps({'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'load_stopped':reason})+'\n')

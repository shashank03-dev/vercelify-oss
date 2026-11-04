import json,time,datetime,os
p='/var/lib/vercelify/qualification-power-thermal-20261105/battery.jsonl'
fd=os.open(p,os.O_WRONLY|os.O_APPEND|os.O_CREAT,0o644)
def r(f):
    try: return open(f).read().strip()
    except: return None
while True:
    b='/sys/class/power_supply/BAT0/'
    d={'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'ac':r('/sys/class/power_supply/AC/online'),'status':r(b+'status'),'cap':r(b+'capacity'),'v':r(b+'voltage_now'),'charge_now':r(b+'charge_now'),'energy_now':r(b+'energy_now'),'guard':r('/run/vercelify/power-guard.state')}
    os.write(fd,(json.dumps(d)+'\n').encode()); os.fsync(fd)
    time.sleep(2)

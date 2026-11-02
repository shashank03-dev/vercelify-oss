import json,time
from pathlib import Path
end=time.monotonic()+240
while time.monotonic()<end and not Path('/tmp/vfy-m3-small-handoff-complete').exists():
 mem={k:int(v.split()[0]) for k,v in (s.split(':',1) for s in Path('/proc/meminfo').read_text().splitlines())}
 vm=dict(s.split() for s in Path('/proc/vmstat').read_text().splitlines())
 print(json.dumps({'time':time.time(),'total_kib':mem['MemTotal'],'available_kib':mem['MemAvailable'],'swap_total_kib':mem['SwapTotal'],'swap_free_kib':mem['SwapFree'],'oom_kill':int(vm['oom_kill'])}),flush=True)
 time.sleep(.2)

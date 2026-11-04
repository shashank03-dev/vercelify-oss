"""Run only inside the owned disposable nested Docker daemon, serially."""
from pathlib import Path
import subprocess
import time
out=Path('docs/validation/2026-11-04-m7b')
base=Path('target/aarch64-unknown-linux-musl/release/deps')
cases=[('restore-drills','restore_drill_docker','VERCELIFY_RESTORE_DRILLS=1'),('engine','engine_docker','VERCELIFY_DOCKER_TESTS=1'),('postgres-previews','preview_db_docker','VERCELIFY_PREVIEW_DB_TESTS=1'),('preview-safety','preview_safety','VERCELIFY_PREVIEW_CONTRACT=1')]
for label,name,flag in cases:
    candidates=[p for p in base.glob(name+'-*') if p.is_file() and p.suffix=='']
    assert candidates
    candidates=[max(candidates,key=lambda p:p.stat().st_mtime_ns)]
    result=out/(label+'.txt')
    if result.exists() and 'test result: ok.' in result.read_text():
        print(label+': existing completed result',flush=True);continue
    start=time.monotonic()
    with (out/(label+'.txt')).open('w') as log:
        subprocess.run(['limactl','shell','vfybench','--','sudo','docker','exec','-e','TMPDIR=/var/lib/vfy-contract-tmp','-e',flag,'vfy-m1-contract-docker','/workspace/'+str(candidates[0]),'--include-ignored','--nocapture'],stdout=log,stderr=subprocess.STDOUT,check=True)
    print(f'{label}: passed in {time.monotonic()-start:.2f}s',flush=True)

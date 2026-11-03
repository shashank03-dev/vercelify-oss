"""Run serially: these contracts each own the disposable daemon's proxy."""
from pathlib import Path
import subprocess
import time

out=Path('docs/validation/2026-11-03-m6')
base='/workspace/target/aarch64-unknown-linux-musl/release/deps/'
cases=[
 ('deploy-guard','deploy_guard_docker-fce2e52f7ff03539','VERCELIFY_GUARD_TESTS=1'),
 ('engine','engine_docker-e9e4f85fefec36e5','VERCELIFY_DOCKER_TESTS=1'),
 ('postgres-previews','preview_db_docker-838053ea13802bfe','VERCELIFY_PREVIEW_DB_TESTS=1'),
 ('preview-safety','preview_safety-d2e54fb2296f21b8','VERCELIFY_PREVIEW_CONTRACT=1'),
]
for label,binary,flag in cases:
 if (out/(label+'.txt')).exists() and 'test result: ok.' in (out/(label+'.txt')).read_text():
  print('Existing completed result: '+label,flush=True);continue
 print('Running '+label,flush=True);start=time.monotonic()
 with (out/(label+'.txt')).open('w') as log:
  subprocess.run(['limactl','shell','vfybench','--','sudo','docker','exec',
    '-e','TMPDIR=/var/lib/vfy-contract-tmp','-e',flag,'vfy-m1-contract-docker',
    base+binary,'--include-ignored','--nocapture'],stdout=log,stderr=subprocess.STDOUT,check=True)
 print(f'Passed {label}: {time.monotonic()-start:.2f}s',flush=True)

# The HTTP commit gate also covers image handoff. Use the existing local pack
# contract against the same disposable target, and stop it even on failure.
path=out/'offhost-builds.txt'
if not path.exists() or 'revoking the device during a complete valid upload' not in path.read_text():
 import os
 prefix=['limactl','shell','vfybench','--','sudo','docker','exec']
 try:
  subprocess.run(prefix+['-e','AGENT_BIN=/workspace/target/aarch64-unknown-linux-musl/release/vercelify-agent','vfy-m1-contract-docker','bash','/workspace/host/tests/contract/offhost-target.sh','start'],check=True)
  env=os.environ.copy();env.update(OFFHOST_TARGET_EXEC='["limactl","shell","vfybench","--","sudo","docker","exec","vfy-m1-contract-docker"]',OFFHOST_TARGET_ROOT='/workspace',VFY_PACK_BIN=str(Path.cwd()/'target/debug/vercelify-pack'))
  with path.open('w') as log:
   subprocess.run(['bash','host/tests/contract/offhost-builds.sh'],env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
  print('Passed offhost',flush=True)
 finally:
  subprocess.run(prefix+['vfy-m1-contract-docker','bash','/workspace/host/tests/contract/offhost-target.sh','stop'],check=True)

"""Run reviewed ignored contracts serially in the explicitly owned nested daemon.
Never accepts an arbitrary Docker endpoint or serving-host bypass.
"""
from pathlib import Path
import subprocess, json, time, hashlib
OUT=Path(__file__).parent
BASE=Path('target/aarch64-unknown-linux-musl/release/deps')
PREFIX=['limactl','shell','vfybench','--','sudo','docker','exec']
OWNER='vfy-final-qualification-docker'
def inner(args,**kw):
 return subprocess.run(PREFIX+[OWNER,*args],**kw)
inner(['sh','-ec','test ! -d /etc/vercelify; test -z "$(docker ps -q)"'],check=True)
cases=[
 ('docker','vercelify_platform','docker::tests::container_lifecycle','VERCELIFY_DOCKER_TESTS'),
 ('compose-config','vercelify_platform','compose::tests::compose_resolves_preview_volumes','VERCELIFY_DOCKER_TESTS'),
 ('compose-runtime','vercelify_platform','compose::tests::compose_up_and_down','VERCELIFY_DOCKER_TESTS'),
 ('registry','vercelify_platform','registry::tests::push_list_delete','VERCELIFY_DOCKER_TESTS'),
 ('build-fixtures','vercelify_platform','build::tests::builds_fixture_apps_with_docker','VERCELIFY_DOCKER_TESTS'),
 ('terminal','vercelify_agent','terminal::tests::into_a_container','VERCELIFY_DOCKER_TESTS'),
 ('builder-idle','builder_idle','idle_builder_retains_cache_and_restarts_for_repeated_builds','VERCELIFY_BUILDER_CONTRACT'),
 ('engine','engine_docker','deploy_redeploy_rollback_recover','VERCELIFY_DOCKER_TESTS'),
 ('databases','databases_docker','databases_end_to_end','VERCELIFY_DOCKER_TESTS'),
 ('deploy-guard','deploy_guard_docker','running_http_failure_crash_loop_and_failed_recovery_are_durable','VERCELIFY_GUARD_TESTS'),
 ('postgres-previews','preview_db_docker','isolated_postgres_roles_migrations_and_owned_recovery','VERCELIFY_PREVIEW_DB_TESTS'),
 ('preview-safety','preview_safety','previews_have_no_production_credentials_network_or_public_bypass','VERCELIFY_PREVIEW_CONTRACT'),
 ('restore-drills','restore_drill_docker','isolated_restore_drills_verify_selected_snapshots_and_fail_safely','VERCELIFY_RESTORE_DRILLS'),
]
results=json.loads((OUT/'ignored-results.json').read_text()) if (OUT/'ignored-results.json').exists() else []
for label,stem,test,flag in cases:
 paths=[p for p in BASE.glob(stem+'-*') if p.is_file() and p.suffix=='']
 for binary in sorted(paths,key=lambda p:p.stat().st_mtime_ns,reverse=True):
  listing=inner(['/workspace/'+str(binary),'--list'],capture_output=True,text=True)
  if listing.returncode==0 and test+': test' in listing.stdout:break
 else:raise AssertionError('compiled test missing: '+test)
 fingerprint=hashlib.sha256(binary.read_bytes()).hexdigest()
 if any(r['label']==label and r['passed'] and r['sha256']==fingerprint for r in results):
  print('ALREADY VERIFIED '+label,flush=True);continue
 start=time.monotonic();print('RUN '+label,flush=True)
 with (OUT/(label+'.txt')).open('w') as log:
  result=subprocess.run(PREFIX+['-e','TMPDIR=/var/lib/vfy-contract-tmp','-e',flag+'=1',OWNER,'/workspace/'+str(binary),'--ignored','--exact',test,'--nocapture'],stdout=log,stderr=subprocess.STDOUT,timeout=1200)
 content=(OUT/(label+'.txt')).read_text()
 passed=result.returncode==0 and '1 passed; 0 failed' in content
 results.append(dict(label=label,test=test,binary=str(binary),sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),passed=passed,exit_code=result.returncode,seconds=round(time.monotonic()-start,2)))
 (OUT/'ignored-results.json').write_text(json.dumps(results,indent=2)+'\n')
 print(('PASS ' if passed else 'FAIL ')+label,flush=True)
 # Each subsequent test must start free of leftover fixture containers. Only
 # this exclusively owned daemon is cleaned; the VM serving daemon is separate.
 inner(['sh','-ec','docker ps -aq | xargs -r docker rm -f'],check=True,stdout=subprocess.DEVNULL)

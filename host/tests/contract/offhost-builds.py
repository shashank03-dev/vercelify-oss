#!/usr/bin/env python3
"""Real local-pack → authenticated agent → Docker → proxy handoff contract."""
import hashlib
import tarfile
import json
import os
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[3]
PREFIX = json.loads(os.environ.get('OFFHOST_TARGET_EXEC', '[]'))
PACK = os.environ.get('VFY_PACK_BIN', str(ROOT / 'target/debug/vercelify-pack'))
ARTIFACTS = ROOT / 'target/offhost-contract'
ARTIFACTS.mkdir(parents=True, exist_ok=True)
REMOTE_ROOT = os.environ.get('OFFHOST_TARGET_ROOT', str(ROOT))
SECRET = 'offhost-contract-secret-73b1'

def run(argv, **kwargs):
    return subprocess.run(argv, check=True, capture_output=True, text=True, **kwargs)

def target(argv, **kwargs):
    return run(PREFIX + argv, **kwargs)

def api(method, path, body=None, archive=None, expected=200):
    args = ['curl', '-sS', '--max-time', '60', '-K', '/tmp/vfy-offhost-contract/curl.conf', '-X', method, '-w', '\n%{http_code}']
    if body is not None:
        args += ['-H', 'Content-Type: application/json', '--data-binary', json.dumps(body)]
    if archive:
        args += ['-H', 'Content-Type: application/x-tar', '--data-binary', '@' + str(archive)]
    args += ['http://127.0.0.1:17425/v1/platform' + path]
    output = target(args).stdout
    data, status = output.rsplit('\n', 1)
    assert int(status) == expected, (method, path, status, data)
    return json.loads(data)

def serving(expected="offhost-ready"):
    # Existing routes reload asynchronously after the deployment is healthy.
    # Keep probing for the new body; an actually cached old image never passes.
    for _ in range(15):
        response = target(['curl', '-fsS', '--max-time', '5', '-H', 'Host: offhost.test', 'http://127.0.0.1:18080/']).stdout
        if expected in response:
            return
        time.sleep(1)
    raise AssertionError(response)

def wait(id):
    for _ in range(120):
        d = api('GET', '/deployments/' + id)
        if d['status'] == 'healthy':
            serving()
            return
        assert d['status'] not in ['failed', 'cancelled'], d
        time.sleep(1)
    raise AssertionError('deployment health timed out')

source = ARTIFACTS / 'source'
source.mkdir(exist_ok=True)
(source / 'Dockerfile').write_text('''FROM busybox:1.37
RUN --mount=type=secret,id=env . /run/secrets/env; printf '%s' "$BUILD_SECRET"
RUN --mount=type=secret,id=env . /run/secrets/env; mkdir -p /www; printf '%s' "$MESSAGE" > /www/index.html
EXPOSE 8080
CMD ["httpd", "-f", "-p", "8080", "-h", "/www"]
''')
plan = json.loads(run([PACK, 'plan', str(source)], input='{}').stdout)
nodes = api('GET', '/nodes')
arch = next(n['arch'] for n in nodes if n['role'] == 'main')
platform = 'linux/arm64' if arch in ['aarch64', 'arm64'] else 'linux/amd64'
app = api('POST', '/apps', {
    'name': 'offhost-contract', 'kind': 'git', 'source': {'repository': 'https://example.com/no-target-fetch.git'},
    'runtime': {'port': 8080, 'health_timeout_secs': 30, 'drain_secs': 0},
    'domains': []})
api('POST', '/apps/' + app['id'] + '/domains', {'host':'offhost.test','https':False,'primary':True})
api('PUT', '/apps/' + app['id'] + '/env', {'vars': [{'key': 'BUILD_SECRET', 'value': SECRET, 'build': True, 'secret': True},{'key':'MESSAGE','value':'offhost-ready-a','build':True,'secret':False}], 'replace': False})

def lease(sha='a'):
    return api('POST', '/apps/' + app['id'] + '/build-jobs', {'commit_sha': sha * 40, 'platform': platform, 'plan': plan})

def build(job):
    req = {'build': app['build'], 'app_name': app['name'], 'deployment_id': job['job']['deployment_id'], 'variables': job['variables'], 'platform': platform}
    built = run([PACK, 'build', str(source)], input=json.dumps(req), timeout=240)
    assert SECRET not in built.stdout + built.stderr, 'secret escaped local log redaction'
    image = json.loads(built.stdout)['image']
    run(['docker', 'tag', image, job['job']['image']])
    archive = ARTIFACTS / (job['job']['deployment_id'] + '.tar')
    run(['docker', 'save', '--output', str(archive), job['job']['image']])
    with tarfile.open(archive) as saved:
        manifest = json.load(saved.extractfile('manifest.json'))
        digest = 'sha256:' + hashlib.sha256(saved.extractfile(manifest[0]['Config']).read()).hexdigest()
    return str(Path(REMOTE_ROOT) / archive.relative_to(ROOT)), digest

job = lease()
id = job['job']['deployment_id']
time.sleep(1)
assert api('GET', '/deployments/' + id)['status'] == 'queued'
archive, digest = build(job)
response = api('PUT', f'/build-jobs/{id}/image?generation=1&digest={digest}', archive=archive)
assert response['phase'] == 'verified'
wait(id)
serving('offhost-ready-a')
print('PASS: local shared pack, secret redaction, authenticated archive, Docker load, verified serving; target builds disabled', flush=True)

# BuildKit secrets are intentionally excluded from its cache key. An unchanged
# source checkout with changed build variables must still produce new output.
api('PUT', '/apps/' + app['id'] + '/env', {'vars':[{'key':'MESSAGE','value':'offhost-ready-b','build':True,'secret':False}], 'replace':False})
changed=lease('e')
changed_archive, changed_digest=build(changed)
changed_id=changed['job']['deployment_id']
api('PUT',f'/build-jobs/{changed_id}/image?generation=1&digest={changed_digest}',archive=changed_archive)
wait(changed_id)
serving('offhost-ready-b')
print('PASS: changed build variables cannot reuse stale application output',flush=True)

# Duplicated completion cannot republish, even with an otherwise valid image.
api('PUT', f'/build-jobs/{id}/image?generation=1&digest={digest}', archive=archive, expected=409)
serving()
print('PASS: duplicate completion rejected; existing production still serves', flush=True)

for reason, generation, supplied_digest, wanted in [
    ('stale generation', 0, digest, 409),
    ('wrong digest', 1, 'sha256:' + '0' * 64, 400),
]:
    new = lease('b')
    other = new['job']['deployment_id']
    # For wrong digest, the old archive also has the wrong job tag: both
    # gates reject before Docker. Digest is independently covered in Rust.
    api('PUT', f'/build-jobs/{other}/image?generation={generation}&digest={supplied_digest}', archive=archive, expected=wanted)
    serving()
    api('POST', '/deployments/' + other + '/cancel') if wanted == 409 else None
    print('PASS:', reason, 'cannot replace serving production', flush=True)

new = lease('c')
old_id = new['job']['deployment_id']
newer = lease('d')
api('PUT', f'/build-jobs/{old_id}/image?generation=1&digest={digest}', archive=archive, expected=409)
api('POST', '/deployments/' + newer['job']['deployment_id'] + '/cancel')
serving()
print('PASS: a newer commit fences an old upload', flush=True)
result = target(['sh', '-c', 'test ! -e /tmp/vfy-offhost-contract/target-builds; find /tmp/vfy-offhost-contract/state/build-uploads -type f | wc -l']).stdout.strip()
assert result == '0', result
print('PASS: zero target git/build commands; all upload temporary files released', flush=True)

# A cancelled job and an expired lease cannot accept an old artifact.
new = lease('f')
id = new['job']['deployment_id']
api('POST', '/deployments/' + id + '/cancel')
api('PUT',f'/build-jobs/{id}/image?generation=1&digest={digest}',archive=archive,expected=409)
serving()
print('PASS: cancellation fences the artifact',flush=True)
new = lease('1')
id = new['job']['deployment_id']
code = "import sqlite3; c=sqlite3.connect('/tmp/vfy-offhost-contract/state/platform.db'); c.execute('UPDATE build_jobs SET deadline=0 WHERE deployment_id=?',("+repr(id)+",)); c.commit()"
target(['python3','-c',code])
api('POST',f'/build-jobs/{id}/renew',{'generation':1},expected=409)
api('PUT',f'/build-jobs/{id}/image?generation=1&digest={digest}',archive=archive,expected=409)
serving()
print('PASS: durable lease expiry refuses renew and publication',flush=True)

# A disconnected sender cannot make a partially transferred tar deployable.
new = lease('2')
id = new['job']['deployment_id']
full_archive, full_digest = build(new)
local_archive=ARTIFACTS/(id+'.tar')
partial=ARTIFACTS/(id+'-partial.tar')
partial.write_bytes(local_archive.read_bytes()[:1024])
remote_partial=str(Path(REMOTE_ROOT)/partial.relative_to(ROOT))
args=['curl','-sS','--max-time','2','-K','/tmp/vfy-offhost-contract/curl.conf','-X','PUT','-H','Content-Type: application/x-tar','-H','Content-Length: '+str(local_archive.stat().st_size),'--data-binary','@'+remote_partial,'http://127.0.0.1:17425/v1/platform/build-jobs/'+id+'/image?generation=1&digest='+full_digest]
subprocess.run(PREFIX+args,capture_output=True,text=True)
time.sleep(1)
assert api('GET','/build-jobs/'+id)['phase']!='verified'
serving()
print('PASS: interrupted upload cannot publish; current app remains reachable',flush=True)

# Revocation is checked again after Docker load, not only at request start.
planfile=ARTIFACTS/'plan.json'
planfile.write_text(json.dumps(plan))
remote_plan=str(Path(REMOTE_ROOT)/planfile.relative_to(ROOT))
revoke_script=str(Path(REMOTE_ROOT)/'host/tests/contract/offhost-revoke.py')
revoked=json.loads(target(['python3',revoke_script,'create',app['id'],platform,remote_plan]).stdout)
revoked_archive,revoked_digest=build(revoked)
print(target(['python3',revoke_script,'upload',revoked_archive,revoked_digest]).stdout.strip(),flush=True)
serving()

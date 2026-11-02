"""Owned vfybench runtime-only matrix. Uses private local pairing settings.

Run from the repository after stopping the disposable Docker test daemon and
installing the candidate. Restores original app desired state, config, RAM and
swap in finally. Cached pinned Next image: this is not a cold-download gate.
"""
import concurrent.futures
import json
from pathlib import Path
import statistics
import subprocess
import threading
import time
import urllib.request

ROOT = Path.cwd()
PRIVATE = ROOT / 'target/offhost-small-vm'
OUT = ROOT / 'docs/validation/2026-11-02-m4'
TOKEN = json.loads((PRIVATE / 'settings.json').read_text())['servers'][0]['agent_token']


def command(args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout.strip()


def guest(code):
    return command(['limactl', 'shell', 'vfybench', '--', 'sudo', 'bash', '-c', code])


def api(path, body=None, method=None):
    request = urllib.request.Request('http://127.0.0.1:17427/v1/platform' + path,
        method=method, data=None if body is None else json.dumps(body).encode(),
        headers={'Authorization': 'Bearer ' + TOKEN, 'Content-Type': 'application/json'})
    return json.load(urllib.request.urlopen(request, timeout=30))


def until(label, predicate, seconds=150):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, ValueError):
            pass
        time.sleep(1)
    raise AssertionError('timed out: ' + label)


def boot(memory):
    command(['limactl', 'stop', 'vfybench'])
    command(['limactl', 'edit', 'vfybench', '--memory', str(memory), '--tty=false'])
    command(['limactl', 'start', 'vfybench', '--tty=false'])
    # Lima's SSH master is replaced by a VM reboot; recreate the probe forward.
    command(['ssh', '-F', str(Path.home() / '.lima/vfybench/ssh.config'),
        '-O', 'forward', '-L', '17429:127.0.0.1:80', '-L',
        '17427:127.0.0.1:7420', 'lima-vfybench'])
    until('agent after boot', lambda: api('/apps'))
    guest('swapoff -a')
    until('fresh capacity', lambda: next((r for r in api('/capacity')
        if r['sample'] and r['sample_age_secs'] < 10 and memory * 1024 * .85 < r['sample']['total_mb'] < memory * 1024), None))


def healthy(deployment):
    state = api('/deployments/' + deployment['id'])
    assert state['status'] not in ('failed', 'cancelled'), state
    return state['status'] == 'healthy'


def probe():
    request = urllib.request.Request('http://127.0.0.1:17429/', headers={'Host': 'm4-profile.test'})
    with urllib.request.urlopen(request, timeout=5) as response:
        assert response.status == 200 and len(response.read()) > 1000


def load(seconds=60):
    end = time.monotonic() + seconds
    def worker():
        rows = []
        while time.monotonic() < end:
            start = time.monotonic()
            try:
                probe()
                error = None
            except Exception as failure:
                error = str(failure)
            rows.append((time.monotonic() - start, error))
            time.sleep(max(0, .25 - (time.monotonic() - start)))
        return rows
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        rows = sum(list(pool.map(lambda _: worker(), range(4))), [])
    times = sorted(row[0] * 1000 for row in rows)
    result = {'requests': len(rows), 'failures': sum(row[1] is not None for row in rows),
        'duration_seconds': seconds, 'concurrency': 4, 'per_worker_interval_seconds': .25,
        'latency_ms_median': statistics.median(times), 'latency_ms_p95': times[int(len(times) * .95)]}
    assert result['failures'] == 0, result
    return result


original = api('/apps')
(PRIVATE / 'm4-original-apps.json').write_text(json.dumps(original))
fixture = None
project = None
results = []
try:
    # Save once, before changing this owned laboratory installation.
    guest('test -f /var/lib/vercelify/qualification-m4-before/agent.toml')
    guest("sed -i '\\|^/swapfile[[:space:]]|s|^|# M4 qualification |' /etc/fstab; swapoff -a")
    guest("sed -i '1i build_concurrency = 0' /etc/vercelify/agent.toml; systemctl restart vercelify-agent")
    until('agent ready', lambda: api('/apps'))
    for app in original:
        api('/apps/' + app['id'], {'desired': 'stopped'}, 'PATCH')
    until('original apps stopped', lambda: not guest("docker ps -q --filter label=vercelify.kind=app"))
    project = api('/projects', {'name': 'm4-profile-qualification'}, 'POST')
    fixture = api('/apps', {'name': 'm4-profile-next', 'kind': 'image', 'project_id': project['id'],
        'source': {'image': 'vfy/blog:8yywze68cc5wwk9b'},
        'runtime': {'port': 3000, 'memory_mb': 192, 'cpus': 1, 'drain_secs': 10, 'health_timeout_secs': 60}}, 'POST')
    api('/apps/' + fixture['id'] + '/domains', {'host': 'm4-profile.test', 'https': False}, 'POST')
    for memory in (1, 2, 4):
        print(f'START {memory} GiB: three cached runtime rollout runs, then reboot', flush=True)
        boot(memory)
        rows = []
        stop = threading.Event()
        def sample():
            while not stop.is_set():
                try:
                    rows.append(json.loads(guest("python3 -c 'import json,pathlib,time; m={k:int(v.split()[0]) for k,v in (s.split(\":\",1) for s in pathlib.Path(\"/proc/meminfo\").read_text().splitlines())}; v=dict(s.split() for s in pathlib.Path(\"/proc/vmstat\").read_text().splitlines()); print(json.dumps({\"time\":time.time(),\"total_kib\":m[\"MemTotal\"],\"available_kib\":m[\"MemAvailable\"],\"swap_kib\":m[\"SwapTotal\"],\"oom_kill\":int(v[\"oom_kill\"])}))'")))
                except Exception:
                    pass
                stop.wait(1)
        sampler = threading.Thread(target=sample)
        sampler.start()
        try:
            runs = []
            for run in range(3):
                deployment = api('/apps/' + fixture['id'] + '/deploy', {}, 'POST')
                until('cached Next deploy', lambda: healthy(deployment))
                until('Next route', lambda: (probe() is None))
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    serving = pool.submit(load)
                    time.sleep(15)
                    rollout = api('/apps/' + fixture['id'] + '/deploy', {}, 'POST')
                    until('overlapping rollout', lambda: healthy(rollout))
                    runs.append(serving.result())
                print(f'PASS {memory} GiB runtime run {run + 1}: ' + json.dumps(runs[-1]), flush=True)
            boot(memory)
            until('route after reboot', lambda: (probe() is None))
            after_reboot = load()
        finally:
            stop.set()
            sampler.join()
            (OUT / f'profile-{memory}g-samples.json').write_text(json.dumps(rows, indent=2) + '\n')
        result = {'ram_gib': memory, 'runtime_cap_mib': 192, 'swap': 'disabled',
            'builds': 'disabled', 'databases': 0, 'cached_image': True, 'runs': runs,
            'after_reboot': after_reboot, 'sample_count': len(rows),
            'minimum_available_mib': min(r['available_kib'] for r in rows) / 1024,
            'oom_kill_delta': rows[-1]['oom_kill'] - rows[0]['oom_kill'],
            'containers': guest('docker ps -q --filter label=vercelify.app-name=m4-profile-next | xargs -r docker inspect --format "{{.Name}} cap={{.HostConfig.Memory}} swap={{.HostConfig.MemorySwap}} oom={{.State.OOMKilled}} restarts={{.RestartCount}}"')}
        assert rows and all(row['oom_kill'] == 0 and row['swap_kib'] == 0 for row in rows)
        assert 'cap=201326592 swap=201326592 oom=false restarts=0' in result['containers'], result
        result['final_capacity'] = api('/capacity')
        results.append(result)
        (OUT / 'profile-matrix.json').write_text(json.dumps(results, indent=2) + '\n')
        print(json.dumps(result), flush=True)
finally:
    boot(4)
    if fixture:
        api('/apps/' + fixture['id'], method='DELETE')
        until('fixture removed', lambda: not guest('docker ps -q --filter label=vercelify.app-name=m4-profile-next'))
    if project:
        api('/projects/' + project['id'], method='DELETE')
    guest('cp /var/lib/vercelify/qualification-m4-before/agent.toml /etc/vercelify/agent.toml; cp /var/lib/vercelify/qualification-m4-before/fstab /etc/fstab; swapon /swapfile; systemctl restart vercelify-agent')
    until('agent restored', lambda: api('/apps'))
    for app in original:
        api('/apps/' + app['id'], {'desired': app['desired']}, 'PATCH')
    until('six original apps restored', lambda: len(guest('docker ps -q --filter label=vercelify.kind=app').split()) == len(original))
    def restored_routes():
        for app in original:
            request = urllib.request.Request('http://127.0.0.1:17429/', headers={'Host': app['domains'][0]['host']})
            with urllib.request.urlopen(request, timeout=5) as response:
                assert response.status == 200 and response.read()
        return True
    until('six original HTTP routes restored', restored_routes)
    (OUT / 'matrix-restored.txt').write_text(guest('free -m; docker ps --format "{{.Names}} {{.Status}}"; sha256sum /usr/local/bin/vercelify-agent') + '\nAll six original HTTP routes returned 200.\n')

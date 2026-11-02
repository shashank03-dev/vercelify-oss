#!/usr/bin/env python3
"""Real admission/restart/host-pressure contract, inside a disposable daemon only.

AGENT_BIN=/workspace/target/aarch64-unknown-linux-musl/release/vercelify-agent
python3 /workspace/host/tests/contract/admission.py
Uses offhost-target's private synthetic credentials; never run on a serving host.
"""
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import errno
import shutil
import tempfile
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import threading
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[3]
WORK = Path('/tmp/vfy-offhost-contract')
TARGET = ROOT / 'host/tests/contract/offhost-target.sh'
created = []
stop_probes = threading.Event()
probes = []
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def run(args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout.strip()


def api(method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request('http://127.0.0.1:17425/v1/platform' + path,
        data=data, method=method, headers={'Authorization': 'Bearer offhost-contract-only', 'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.load(response)


def until(label, predicate, seconds=100):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            value = predicate()
            if value:
                return value
        except (urllib.error.URLError, ConnectionError):
            pass
        time.sleep(1)
    raise AssertionError('timed out: ' + label)


def report():
    return next((r for r in api('GET', '/capacity') if r['sample'] and r['account']), None)


def create(name, memory):
    app = api('POST', '/apps', {'name': name, 'kind': 'image', 'source': {'image': 'traefik/whoami:v1.11'},
        'runtime': {'port': 80, 'memory_mb': memory, 'cpus': 1, 'health_timeout_secs': 30, 'drain_secs': 0}})
    created.append(app['id'])
    return app


def deploy(app):
    return api('POST', '/apps/' + app['id'] + '/deploy', {})


def healthy(deployment):
    item = api('GET', '/deployments/' + deployment['id'])
    assert item['status'] not in ('failed', 'cancelled'), item
    return item['status'] == 'healthy'


def probe():
    request = urllib.request.Request('http://127.0.0.1:18080/', headers={'Host': 'admission.test'})
    with urllib.request.urlopen(request, timeout=5) as response:
        assert response.status == 200
        assert b'Hostname:' in response.read()


def probe_loop():
    while not stop_probes.is_set():
        try:
            probe()
            probes.append(True)
        except Exception:
            probes.append(False)
        stop_probes.wait(.1)


def agent_gone(pid):
    try:
        return Path(f'/proc/{pid}/stat').read_text().split()[2] == 'Z'
    except FileNotFoundError:
        return True


def stop_agent():
    pid = int((WORK / 'pid').read_text())
    os.kill(pid, signal.SIGTERM)
    until('agent stopped', lambda: agent_gone(pid), 15)


@contextmanager
def limited_state_disk():
    # Only this fixture's state is mounted. The outer VM/daemon disk is never
    # filled. Docker's existing proxy bind and production containers keep running.
    state = (WORK / 'state').resolve()
    assert str(state) == '/var/lib/vfy-offhost-contract-state'
    with tempfile.TemporaryDirectory(prefix='vfy-m4-disk-', dir='/var/lib') as tmp:
        copy = Path(tmp) / 'state'
        proxy = Path(tmp) / 'proxy'
        proxy.mkdir()
        stop_agent()
        shutil.copytree(state, copy)
        # Keep the original proxy directory identity and its Docker bind mounts.
        run(['mount', '--bind', str(state / 'proxy'), str(proxy)])
        run(['mount', '-t', 'tmpfs', '-o', 'size=64m', 'vfy-m4-disk', str(state)])
        try:
            shutil.copytree(copy, state, dirs_exist_ok=True)
            run(['mount', '--bind', str(proxy), str(state / 'proxy')])
            run(['bash', str(TARGET), 'start'])
            until('limited disk inventory', report)
            yield state
        finally:
            # Restore a consistent stopped SQLite file/WAL set; proxy bindings
            # remain intact because their original directories are never removed.
            (state / 'owned-disk-pressure').unlink(missing_ok=True)
            stop_agent()
            latest = Path(tmp) / 'latest'
            latest.mkdir()
            for suffix in ['', '-wal', '-shm']:
                source = state / ('platform.db' + suffix)
                if source.exists():
                    shutil.copy2(source, latest / source.name)
            run(['umount', str(state / 'proxy')])
            run(['umount', str(state)])
            run(['umount', str(proxy)])
            for suffix in ['', '-wal', '-shm']:
                destination = state / ('platform.db' + suffix)
                destination.unlink(missing_ok=True)
                source = latest / destination.name
                if source.exists():
                    shutil.copy2(source, destination)
            run(['bash', str(TARGET), 'start'])
            until('restored disk inventory', report)


try:
    # The shell helper's serving-host guard runs before changing its proxy/state.
    run(['bash', str(TARGET), 'stop'])
    # API deletion is asynchronous; stop can precede its Docker cleanup.
    for name in ('base', 'a', 'b', 'pressure', 'disk'):
        containers = run(['docker', 'ps', '-aq', '--filter',
            'label=vercelify.app-name=m4-admission-' + name]).split()
        if containers:
            run(['docker', 'rm', '-f'] + containers)
    run(['bash', str(TARGET), 'start'])
    until('fresh inventory', report)
    baseline = create('m4-admission-base', 256)
    api('POST', '/apps/' + baseline['id'] + '/domains', {'host': 'admission.test', 'https': False, 'primary': True})
    base_deployment = deploy(baseline)
    until('healthy baseline', lambda: healthy(base_deployment))
    until('proxy baseline', lambda: (probe() is None))
    watcher = threading.Thread(target=probe_loop, daemon=True)
    watcher.start()
    # Wait until the first operation is represented by fresh Docker inventory.
    frame = until('settled baseline', lambda: (r if not r['account']['heavy'] else None) if (r := report()) else None)
    budget = frame['account']['workload_remaining_mb']
    each = int(budget * .60)
    assert each + 128 < frame['account']['available_mb'], frame
    apps = [create('m4-admission-a', each), create('m4-admission-b', each)]
    with ThreadPoolExecutor(max_workers=2) as pool:
        deployments = list(pool.map(deploy, apps))
    until('one admitted', lambda: any(healthy(d) for d in deployments))
    states = [api('GET', '/deployments/' + d['id']) for d in deployments]
    assert sorted(d['status'] for d in states) == ['healthy', 'queued'], states
    queued = next(d for d in states if d['status'] == 'queued')
    assert 'capacity' in queued['error'].lower() or 'memory' in queued['error'].lower(), queued
    print('PASS: parallel requests cannot spend the same workload budget; other request remains queued', flush=True)
    containers = json.loads(run(['docker', 'inspect'] + run(['docker', 'ps', '-q', '--filter', 'label=vercelify.kind=app']).split()))
    for container in containers:
        cap = container['HostConfig']['Memory']
        assert cap > 0 and container['HostConfig']['MemorySwap'] == cap
        assert container['HostConfig']['NanoCpus'] == 1_000_000_000
        assert not container['State']['OOMKilled']
    print('PASS: Docker enforces memory, swap and CPU limits on admitted apps', flush=True)
    # Abrupt agent death leaves Docker's production route serving throughout.
    pid = int((WORK / 'pid').read_text())
    os.kill(pid, signal.SIGABRT)
    until('agent stopped', lambda: agent_gone(pid), 15)
    run(['bash', str(TARGET), 'start'])
    until('capacity recovered', report)
    assert api('GET', '/deployments/' + queued['id'])['status'] == 'queued'
    print('PASS: abrupt agent restart reconciles inventory and keeps unsafe queued work deferred', flush=True)
    live = next(d for d in states if d['status'] == 'healthy')
    api('DELETE', '/apps/' + live['app_id'])
    created.remove(live['app_id'])
    until('queued request eventually proceeds', lambda: healthy(queued))
    api('DELETE', '/apps/' + queued['app_id'])
    created.remove(queued['app_id'])
    until('contenders removed', lambda: not run(['docker', 'ps', '-q', '--filter', 'label=vercelify.app-name=m4-admission-a']) and not run(['docker', 'ps', '-q', '--filter', 'label=vercelify.app-name=m4-admission-b']))
    old = report()['sample']['measured_at']
    fresh = until('fresh free capacity', lambda: r if (r := report())['sample']['measured_at'] > old and not r['account']['heavy'] else None)
    before = fresh['account']['available_mb']
    # An unrelated real process dirties 700 MiB; it is outside Vercelify's app labels.
    run(['docker', 'run', '-d', '--name', 'vfy-m4-pressure', '--memory', '800m', '--memory-swap', '800m',
        '--cpus', '1', '--entrypoint', 'node', 'node:22-slim', '-e',
        "global.hold=Buffer.alloc(700*1024*1024,7);setInterval(()=>{},1000)"])
    pressured = until('host pressure measured', lambda: r if (r := report())['account']['available_mb'] < before - 500 else None)
    requested = min(before - 128, fresh['account']['workload_remaining_mb'] - 128)
    assert requested > pressured['account']['available_mb'], (fresh, pressured)
    pressure_app = create('m4-admission-pressure', requested)
    pressure_deployment = deploy(pressure_app)
    time.sleep(3)
    assert api('GET', '/deployments/' + pressure_deployment['id'])['status'] == 'queued'
    run(['docker', 'rm', '-f', 'vfy-m4-pressure'])
    until('pressure release permits work', lambda: healthy(pressure_deployment))
    print('PASS: unrelated host RAM pressure defers work, then release permits it without stopping production', flush=True)
    api('DELETE', '/apps/' + pressure_app['id'])
    created.remove(pressure_app['id'])
    until('pressure app removed', lambda: not run(['docker', 'ps', '-q', '--filter',
        'label=vercelify.app-name=m4-admission-pressure']))
    with limited_state_disk() as disk:
        assert report()['account']['disk_remaining_mb'] == 0
        disk_app = create('m4-admission-disk', 64)
        disk_deployment = deploy(disk_app)
        time.sleep(3)
        assert api('GET', '/deployments/' + disk_deployment['id'])['status'] == 'queued'
        full = False
        with (disk / 'owned-disk-pressure').open('wb', buffering=0) as file:
            try:
                for _ in range(65):
                    file.write(bytes(1024 * 1024))
            except OSError as error:
                assert error.errno == errno.ENOSPC
                full = True
        assert full, 'fixture must actually encounter ENOSPC'
        time.sleep(3)
        assert not run(['docker', 'ps', '-q', '--filter', 'label=vercelify.deployment=' + disk_deployment['id']])
        probe()
    until('restored disk permits queued work', lambda: healthy(disk_deployment))
    print('PASS: real ENOSPC on the isolated state disk starts no unsafe workload; restoring disk resumes its queue', flush=True)
    assert probes and all(probes), f'{sum(not p for p in probes)} of {len(probes)} serving probes failed'
    print(f'PASS: unchanged production served all {len(probes)} probes during contention, restart and pressure', flush=True)
finally:
    stop_probes.set()
    run(['docker', 'rm', '-f', 'vfy-m4-pressure']) if run(['docker', 'ps', '-aq', '--filter', 'name=^vfy-m4-pressure$']) else None
    for app in created:
        try:
            api('DELETE', '/apps/' + app)
        except Exception:
            pass
    run(['bash', str(TARGET), 'stop'])
    for name in ('base', 'a', 'b', 'pressure', 'disk'):
        containers = run(['docker', 'ps', '-aq', '--filter',
            'label=vercelify.app-name=m4-admission-' + name]).split()
        if containers:
            run(['docker', 'rm', '-f'] + containers)

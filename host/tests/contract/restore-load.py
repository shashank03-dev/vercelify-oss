#!/usr/bin/env python3
"""Run the real database restore contract while an unchanged app serves traffic.

Disposable daemon only. Argument: compiled databases_docker test executable.
The Rust fixture restores a 20,000-row PostgreSQL table/index plus Valkey data.
"""
import json
from pathlib import Path
import subprocess
import sys
import threading
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[3]
subprocess.run(['bash', '-c', 'source "$1"', 'bash',
    str(ROOT / 'host/tests/contract/lib-guard.sh')], check=True)
assert len(sys.argv) == 2, 'provide the databases_docker test executable'
NAME = 'vfy-m4-restore-serving'
stopped = threading.Event()
probes = []
samples = []


def probe():
    with urllib.request.urlopen('http://127.0.0.1:18083/', timeout=5) as response:
        assert response.status == 200 and b'Hostname:' in response.read()


def observe():
    last = 0
    while not stopped.is_set():
        try:
            probe()
            probes.append(True)
        except Exception:
            probes.append(False)
        if time.monotonic() - last >= 1:
            memory = dict(line.split(':', 1) for line in Path('/proc/meminfo').read_text().splitlines())
            vm = dict(line.split() for line in Path('/proc/vmstat').read_text().splitlines())
            samples.append({'time': time.time(),
                'available_kib': int(memory['MemAvailable'].split()[0]), 'oom_kill': int(vm['oom_kill'])})
            last = time.monotonic()
        stopped.wait(.1)


try:
    subprocess.run(['docker', 'run', '-d', '--name', NAME, '--memory', '64m',
        '--memory-swap', '64m', '--cpus', '.5', '-p', '127.0.0.1:18083:80',
        'traefik/whoami:v1.11'], check=True)
    for attempt in range(30):
        try:
            probe()
            break
        except OSError:
            if attempt == 29:
                raise
            time.sleep(1)
    watcher = threading.Thread(target=observe)
    watcher.start()
    try:
        subprocess.run([sys.argv[1], '--include-ignored', '--nocapture'], check=True)
    finally:
        stopped.set()
        watcher.join()
    assert probes and all(probes), f'{probes.count(False)} of {len(probes)} requests failed'
    assert samples and max(row['oom_kill'] for row in samples) == min(row['oom_kill'] for row in samples)
    print(json.dumps({'requests': len(probes), 'failures': probes.count(False),
        'minimum_available_mib': min(row['available_kib'] for row in samples) / 1024,
        'oom_kill_delta': samples[-1]['oom_kill'] - samples[0]['oom_kill'], 'host_samples': samples}), flush=True)
    print('PASS: PostgreSQL table/index and Valkey restores preserve the unchanged serving app', flush=True)
finally:
    subprocess.run(['docker', 'rm', '-f', NAME], check=False)

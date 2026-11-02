#!/usr/bin/env python3
"""Summarize research §6 samples, excluding the preamble's free output."""
import json
import statistics
import sys
from pathlib import Path

for name in sys.argv[1:]:
    rows = []
    current = None
    for line in Path(name).read_text().splitlines():
        if line.startswith('sample '):
            current = {'timestamp': line.split(' ', 2)[2], 'rss_mib': {}}
            rows.append(current)
        elif current is not None and line.startswith('Mem:'):
            current['used_mib'] = int(line.split()[2])
        elif current is not None and line.startswith('rss_kib '):
            _, process, amount = line.split()
            current['rss_mib'][process] = int(amount) / 1024
    def summary(values):
        return {'median': statistics.median(values), 'min': min(values), 'max': max(values)}
    result = {'samples': len(rows), 'system_used_mib': summary([r['used_mib'] for r in rows]),
              'rss_mib': {p: summary([r['rss_mib'].get(p, 0) for r in rows])
                          for p in ['vercelify-agent', 'vercelify-mesh', 'traefik', 'dockerd',
                                    'containerd', 'containerd-shim', 'buildkitd']}}
    output = Path(name).with_suffix('.json')
    output.write_text(json.dumps(result, indent=2) + '\n')
    print(output, result)

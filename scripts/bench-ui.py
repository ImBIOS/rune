#!/usr/bin/env python3
"""Repeatable offscreen workload. Build `--profile perf` with HiveMind first; time those binaries locally."""
import argparse
import json
import pathlib
import re
import statistics
import subprocess

from perf_binary import perf_binary

ROOT = pathlib.Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser()
p.add_argument('--out', required=True, type=pathlib.Path)
p.add_argument('--runs', type=int, default=7)
p.add_argument('--only', default='')
a = p.parse_args()
a.out.mkdir(parents=True, exist_ok=True)
fixture = json.loads((ROOT / 'examples/opencode/assets/fixture.json').read_text())
sid = fixture['open']
history = []
for i in range(2000 - len(fixture['messages'][sid])):
    msg = {'id': f'msg_history{i:06}', 'type': 'user',
           'text': f'Synthetic message {i}: check the reconnect path and preserve the sidebar state.',
           'time': {'created': fixture['now'] - 10000000 + i}}
    if i % 2:
        msg.update(type='assistant', content=[{'type': 'text', 'text': msg.pop('text')}])
        msg['time']['completed'] = msg['time']['created'] + 1
    history.append(msg)
fixture['messages'][sid] = history + fixture['messages'][sid]
long = a.out.resolve() / 'long.json'
long.write_text(json.dumps(fixture))
f = ['--fixture', 'examples/opencode/assets/fixture.json']
workloads = {
    'stream': ('opencode', f + ['--scene', 'streaming', '--frames', '1440']),
    'scroll': ('opencode', f + ['--scene', 'session', '--script', 'examples/opencode/scripts/scroll.json', '--frames', '480']),
    'switch': ('opencode', f + ['--script', 'examples/opencode/scripts/switch.json', '--frames', '1080']),
    'settings': ('settings', ['--script', 'examples/settings/scripts/bench.json', '--frames', '360']),
    'long-stream': ('opencode', ['--fixture', str(long), '--scene', 'streaming', '--frames', '1440']),
    'long-scroll': ('opencode', ['--fixture', str(long), '--scene', 'session', '--script', 'examples/opencode/scripts/scroll.json', '--frames', '480']),
}
rx = re.compile(r'gpu init ([\d.]+) ms, first frame ([\d.]+) ms, frame cpu mean ([\d.]+) ms p50 ([\d.]+) p95 ([\d.]+) max ([\d.]+), (\d+) instances')
names = ['gpu_ms', 'first_ms', 'mean_ms', 'p50_ms', 'p95_ms', 'max_ms', 'instances']
result = {}
for name, (binary, args) in workloads.items():
    if a.only and name not in a.only.split(','):
        continue
    runs = []
    for i in range(a.runs + 1):
        cmd = [str(perf_binary(ROOT / 'target/perf' / binary)), *args, '--fps', '120']
        r = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, check=True)
        (a.out / f'{name}-{i}.log').write_text(r.stderr)
        m = rx.search(r.stderr)
        if not m:
            raise RuntimeError(r.stderr)
        if i:
            runs.append(dict(zip(names, map(float, m.groups()))))
    med = {k: statistics.median(r[k] for r in runs) for k in names}
    mad = {k: statistics.median(abs(r[k] - med[k]) for r in runs) for k in names}
    result[name] = {'median': med, 'mad': mad, 'runs': runs}
    print(name, json.dumps(med), flush=True)
    (a.out / 'results.json').write_text(json.dumps(result, indent=2) + '\n')

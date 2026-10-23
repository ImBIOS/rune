#!/usr/bin/env python3
"""Paired headless text benchmark. Supply saved `--profile perf` binaries; no build or server."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import statistics
import subprocess

from perf_binary import perf_binary

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser()
p.add_argument('--before', required=True, type=Path)
p.add_argument('--after', required=True, type=Path)
p.add_argument('--out', required=True, type=Path)
p.add_argument('--runs', type=int, default=7)
a = p.parse_args()
if a.runs < 1:
    p.error('--runs must be positive')
a.out = a.out.resolve()
a.out.mkdir(parents=True, exist_ok=True)
binaries = {'before': perf_binary(a.before), 'after': perf_binary(a.after)}
(a.out / 'binaries.json').write_text(json.dumps({stage: hashlib.sha256(path.read_bytes()).hexdigest()
    for stage, path in binaries.items()}, indent=2) + '\n')
fixture_path = ROOT / 'examples/opencode/assets/fixture.json'
fixture = json.loads(fixture_path.read_text())
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
long_path = a.out / 'long.json'
long_path.write_text(json.dumps(fixture))
rx = re.compile(r'gpu init ([\d.]+) ms, first frame ([\d.]+) ms, frame cpu mean ([\d.]+) ms p50 ([\d.]+) p95 ([\d.]+) max ([\d.]+), (\d+) instances')
keys = ['gpu_ms', 'first_ms', 'mean_ms', 'p50_ms', 'p95_ms', 'max_ms', 'instances']
results = {}
for name, path in [('stream', fixture_path), ('long-stream', long_path)]:
    samples = {'before': [], 'after': []}
    for i in range(a.runs + 1):
        for stage in (['before', 'after'] if i % 2 == 0 else ['after', 'before']):
            run = subprocess.run([str(binaries[stage]), '--fixture', str(path), '--scene', 'streaming',
                '--frames', '1440', '--fps', '120'], cwd=ROOT, capture_output=True, text=True, check=True)
            (a.out / f'{name}-{stage}-{i}.log').write_text(run.stderr)
            match = rx.search(run.stderr)
            if not match:
                raise RuntimeError(run.stderr)
            if i:
                samples[stage].append(dict(zip(keys, map(float, match.groups()))))
    results[name] = {}
    for stage, runs in samples.items():
        med = {k: statistics.median(r[k] for r in runs) for k in keys}
        mad = {k: statistics.median(abs(r[k] - med[k]) for r in runs) for k in keys}
        results[name][stage] = {'median': med, 'mad': mad, 'runs': runs}
        print(name, stage, json.dumps(med), flush=True)
    (a.out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')

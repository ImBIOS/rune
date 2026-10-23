#!/usr/bin/env python3
"""Paired CPU-fallback fixture regression checks for the opt-in GPU spring spike."""
import argparse
import hashlib
import json
import pathlib
import re
import statistics
import subprocess

from perf_binary import perf_binary

ROOT = pathlib.Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser()
p.add_argument('--before', required=True, type=pathlib.Path)
p.add_argument('--after', required=True, type=pathlib.Path)
p.add_argument('--out', required=True, type=pathlib.Path)
p.add_argument('--runs', type=int, default=7)
a = p.parse_args()
a.out.mkdir(parents=True, exist_ok=True)
binaries = [perf_binary(a.before), perf_binary(a.after)]
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
rx = re.compile(r'frame cpu mean ([\d.]+) ms p50 ([\d.]+) p95 ([\d.]+) max ([\d.]+), (\d+) instances')
results = {'binary_sha256': [hashlib.sha256(b.read_bytes()).hexdigest() for b in binaries]}
for name, fixture_path in [('stream', ROOT / 'examples/opencode/assets/fixture.json'), ('long-stream', long)]:
    runs = [[], []]
    args = ['--fixture', str(fixture_path), '--scene', 'streaming', '--frames', '1440', '--fps', '120']
    for run in range(a.runs + 1):
        for index in [run % 2, 1 - run % 2]:
            result = subprocess.run([str(binaries[index]), *args], cwd=ROOT, capture_output=True, text=True, check=True)
            (a.out / f'{name}-{index}-{run}.log').write_text(result.stderr)
            values = dict(zip(['mean_ms', 'p50_ms', 'p95_ms', 'max_ms', 'instances'], map(float, rx.search(result.stderr).groups())))
            if run:
                runs[index].append(values)
    summaries = []
    for values in runs:
        med = {key: statistics.median(v[key] for v in values) for key in values[0]}
        mad = {key: statistics.median(abs(v[key] - med[key]) for v in values) for key in values[0]}
        summaries.append({'median': med, 'mad': mad, 'runs': values})
    results[name] = summaries
    print(name, json.dumps(summaries), flush=True)
    for index, binary in enumerate(binaries):
        trace = a.out.resolve() / f'trace-{name}-{index}'
        trace.mkdir(exist_ok=True)
        subprocess.run([str(binary), *args, '--trace', str(trace / 'motion.jsonl')], cwd=ROOT, check=True)
    for time in ['0.4', '4', '12']:
        paths = [a.out.resolve() / f'{name}-{index}-{time}.png' for index in range(2)]
        for binary, path in zip(binaries, paths):
            subprocess.run([str(binary), '--fixture', str(fixture_path), '--scene', 'streaming', '--at', time,
                            '--fps', '120', '--snap', str(path)], cwd=ROOT, check=True)
        assert paths[0].read_bytes() == paths[1].read_bytes(), f'CPU fallback golden changed: {name} at {time}'
    (a.out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')

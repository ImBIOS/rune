#!/usr/bin/env python3
"""CPU submission benchmark for 16 × 60 Hz meters + 4 × 120 Hz cursors; no window."""
import argparse
import json
import re
import statistics
import subprocess

from perf_binary import perf_binary

p = argparse.ArgumentParser()
p.add_argument('--binary', default='target/perf/live-values')
p.add_argument('--baseline', action='store_true')
a = p.parse_args()
a.binary = str(perf_binary(a.binary))
modes = ['rebuild'] if a.baseline else ['rebuild', 'live']
samples = {mode: [] for mode in modes}
for run in range(8):
    # Alternate order so driver load and temperature cannot systematically favour a mode.
    for mode in modes[::(-1 if run % 2 else 1)]:
        cmd = [a.binary, '--frames', '480', '--fps', '120']
        if mode == 'rebuild':
            cmd.append('--rebuild')
        output = subprocess.run(cmd, check=True, capture_output=True, text=True).stderr
        match = re.search(r'frame cpu mean ([\d.]+) ms p50 ([\d.]+) p95 ([\d.]+)', output)
        if not match:
            raise RuntimeError(output)
        if run:
            samples[mode].append(dict(zip(['mean_ms', 'p50_ms', 'p95_ms'], map(float, match.groups()))))
results = {}
for mode, runs in samples.items():
    results[mode] = {k: statistics.median(r[k] for r in runs) for k in runs[0]}
    results[mode]['runs'] = runs
print(json.dumps(results, indent=2))

#!/usr/bin/env python3
"""Fresh-process launch to GPU-complete readable pixels; OS/driver caches stay warm."""
import argparse
import json
import pathlib
import selectors
import statistics
import subprocess
import time

p = argparse.ArgumentParser()
p.add_argument('--out', required=True, type=pathlib.Path)
p.add_argument('--runs', type=int, default=7)
p.add_argument('--long-fixture', type=pathlib.Path)
a = p.parse_args()
a.out.mkdir(parents=True, exist_ok=True)
cases = {
    'settings': ['target/release/settings'],
    'opencode-home': ['target/release/opencode', '--fixture', 'examples/opencode/assets/fixture.json', '--scene', 'home'],
    'opencode-session': ['target/release/opencode', '--fixture', 'examples/opencode/assets/fixture.json', '--scene', 'session'],
}
if a.long_fixture:
    cases['opencode-long'] = ['target/release/opencode', '--fixture', str(a.long_fixture), '--scene', 'session']
result = {}
for name, cmd in cases.items():
    runs = []
    for i in range(a.runs + 1):
        start = time.perf_counter()
        proc = subprocess.Popen(cmd + ['--first-pixels', '--frames', '1'], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(proc.stdout, selectors.EVENT_READ)
                if not selector.select(timeout=30):
                    raise TimeoutError(f'{name}: first pixels not ready after 30s')
                line = proc.stdout.readline().decode()
            elapsed = (time.perf_counter() - start) * 1000
            _, stderr = proc.communicate(timeout=30)
            (a.out / f'{name}-{i}.log').write_bytes(line.encode() + stderr)
            if proc.returncode or not line.startswith('rune-first-pixels '):
                raise RuntimeError(f'{name}: {line}\n{stderr.decode()}')
            row = json.loads(line.removeprefix('rune-first-pixels '))
            row['process_ms'] = elapsed
            if i:
                runs.append(row)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
    median = {k: statistics.median(r[k] for r in runs) for k in runs[0]}
    mad = {k: statistics.median(abs(r[k] - median[k]) for r in runs) for k in median}
    result[name] = {'median': median, 'mad': mad, 'runs': runs}
    print(name, json.dumps(median), flush=True)
    (a.out / 'results.json').write_text(json.dumps(result, indent=2) + '\n')

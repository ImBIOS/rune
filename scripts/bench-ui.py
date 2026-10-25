#!/usr/bin/env python3
"""Repeatable offscreen workloads; build and run on a headless GPU worker.

Frame-CPU mode:
  cargo build --profile perf -p settings -p opencode --bins --example frame-cpu --features frame-profile
  python3 scripts/bench-ui.py --frame-cpu --out target/frame-cpu --runs 9
Save the baseline binaries (including examples/frame-cpu) in saved/target/perf,
then build the candidate and add --compare saved/target/perf. Use separate Cargo
target directories for separate source roots. Comparison verifies binary hashes
differ, discards one warmup per side, and alternates paired runs; report medians
and MAD, and keep timing changes only above the observed noise.

Commit includes view/layout/shaping, and Layout includes shaping: phases overlap;
do not sum them. CPU clocks measure the calling thread; elapsed encode/submit may
include queue backpressure. The runner forces idle frames, so idle is not native
wake CPU; startup measures first completed pixels, not display presentation.
"""
import argparse
import hashlib
import json
import pathlib
import re
import statistics
import subprocess
import os
import selectors
import time

from perf_binary import perf_binary

ROOT = pathlib.Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
p.add_argument('--out', required=True, type=pathlib.Path)
p.add_argument('--runs', type=int, default=7)
p.add_argument('--only', default='')
p.add_argument('--frame-cpu', action='store_true', help='include dense fixtures and phase/allocation metrics (build --features frame-profile)')
p.add_argument('--compare', type=pathlib.Path, help='alternate with saved baseline binaries in a target/perf directory')
a = p.parse_args()
if a.runs < 1:
    p.error('--runs must be positive')
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
if a.frame_cpu:
    hover = a.out.resolve() / 'hover.json'
    hover.write_text(json.dumps({'events': [
        {'t': i / 120, 'type': 'move', 'x': 120, 'y': 40 + (i % 22) * 28}
        for i in range(1, 480)]}))
    scroll = a.out.resolve() / 'dense-scroll.json'
    scroll.write_text(json.dumps({'events': [
        {'t': 0.01, 'type': 'scroll', 'x': 200, 'y': 300, 'dy': -10000, 'dur': 3.9}]}))
    workloads.update({
        'idle': ('examples/frame-cpu', ['--scene', 'idle', '--frames', '480']),
        'spring': ('examples/frame-cpu', ['--scene', 'spring', '--frames', '480']),
        'hover': ('examples/frame-cpu', ['--script', str(hover), '--frames', '480']),
        'dense-scroll': ('examples/frame-cpu', ['--script', str(scroll), '--frames', '480']),
        'markdown-every-frame': ('examples/frame-cpu', ['--scene', 'stream', '--frames', '480']),
        'resize': ('examples/frame-cpu', ['--scene', 'resize', '--frames', '240']),
        'startup': ('settings', ['--first-pixels', '--frames', '1']),
    })
rx = re.compile(r'gpu init ([\d.]+) ms, first frame ([\d.]+) ms, frame cpu mean ([\d.]+) ms p50 ([\d.]+) p95 ([\d.]+) max ([\d.]+), (\d+) instances')
names = ['gpu_ms', 'first_ms', 'mean_ms', 'p50_ms', 'p95_ms', 'max_ms', 'instances']
result = {}
targets = {'current': pathlib.Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'perf'}
if a.compare:
    targets['before'] = a.compare.resolve()
for name, (binary, args) in workloads.items():
    if a.only and name not in a.only.split(','):
        continue
    binaries = {stage: perf_binary(target / binary) for stage, target in targets.items()}
    if a.compare:
        hashes = []
        for path in binaries.values():
            digest = hashlib.sha256()
            with path.open('rb') as source:
                for chunk in iter(lambda: source.read(1024 * 1024), b''):
                    digest.update(chunk)
            hashes.append(digest.digest())
        if hashes[0] == hashes[1]:
            p.error(f'{name}: current and before binaries have identical SHA-256 hashes; rebuild separate candidates')
    samples = {stage: [] for stage in targets}
    for i in range(a.runs + 1):
        for stage in list(targets)[::(-1 if i % 2 else 1)]:
            cmd = [str(binaries[stage]), *args, '--fps', '120']
            process_ms = None
            log_path = a.out / f'{name}-{stage}-{i}.log'
            if name == 'startup':
                with log_path.open('w+') as log:
                    start = time.perf_counter()
                    with subprocess.Popen(cmd, cwd=ROOT, stdout=subprocess.PIPE, stderr=log, text=True) as proc:
                        try:
                            with selectors.DefaultSelector() as selector:
                                selector.register(proc.stdout, selectors.EVENT_READ)
                                if not selector.select(timeout=30):
                                    raise TimeoutError('first pixels not ready after 30s')
                                first = proc.stdout.readline()
                            process_ms = (time.perf_counter() - start) * 1000
                            if not first.startswith('rune-first-pixels '):
                                raise RuntimeError(f'unexpected first-pixels marker: {first}')
                            stdout, _ = proc.communicate(timeout=30)
                            log.seek(0)
                            r = subprocess.CompletedProcess(cmd, proc.returncode, first + stdout, log.read())
                            r.check_returncode()
                        finally:
                            if proc.poll() is None:
                                proc.kill()
                                proc.wait()
            else:
                r = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, check=True, timeout=180)
                log_path.write_text(r.stderr)
            m = rx.search(r.stderr)
            if not m:
                raise RuntimeError(r.stderr)
            row = dict(zip(names, map(float, m.groups())))
            phase = re.search(r'rune-frame-profile (.+)', r.stderr)
            if a.frame_cpu and not phase:
                raise RuntimeError('rebuild with --features frame-profile')
            if phase:
                row.update({k: v for k, v in json.loads(phase[1]).items() if v is not None})
            pixels = re.search(r'rune-first-pixels (.+)', r.stdout)
            if pixels:
                row.update(json.loads(pixels[1]))
            if process_ms is not None:
                row['process_ms'] = process_ms
            if i:
                samples[stage].append(row)
    for stage, runs in samples.items():
        med = {k: statistics.median(r[k] for r in runs) for k in runs[0]}
        mad = {k: statistics.median(abs(r[k] - med[k]) for r in runs) for k in med}
        label = f'{name}.{stage}' if a.compare else name
        result[label] = {'median': med, 'mad': mad, 'runs': runs}
        print(label, json.dumps(med), flush=True)
        for key, value in med.items():
            print(f'METRIC {label}.{key}={value:.6f}', flush=True)
            print(f'METRIC {label}.{key}.mad={mad[key]:.6f}', flush=True)
    if a.compare:
        metric = 'thread_cpu_ms' if 'thread_cpu_ms' in samples['current'][0] and 'thread_cpu_ms' in samples['before'][0] else 'mean_ms'
        ratios = [cur[metric] / old[metric] for cur, old in zip(samples['current'], samples['before'])]
        ratio = statistics.median(ratios)
        spread = statistics.median(abs(r - ratio) for r in ratios)
        result[f'{name}.paired'] = {'metric': metric, 'ratio': ratio, 'mad': spread}
        print(f'{name}: paired ratio uses {metric}', flush=True)
        print(f'METRIC {name}.paired_ratio={ratio:.6f}', flush=True)
        print(f'METRIC {name}.paired_ratio.mad={spread:.6f}', flush=True)
    (a.out / 'results.json').write_text(json.dumps(result, indent=2) + '\n')

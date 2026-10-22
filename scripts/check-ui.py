#!/usr/bin/env python3
"""Fixture-only snapshot and motion gates for already-built release examples."""
import argparse
import hashlib
import pathlib
import subprocess

p = argparse.ArgumentParser()
p.add_argument('--out', required=True, type=pathlib.Path)
p.add_argument('--baseline', required=True, type=pathlib.Path)
p.add_argument('--long-fixture', required=True, type=pathlib.Path)
p.add_argument('--script', action='append', default=[], type=pathlib.Path)
p.add_argument('--case', action='append', default=[], help='Run only named motion cases')
p.add_argument('--existing-snaps', action='store_true', help='Check already-captured snapshots')
p.add_argument('--reviewed', action='append', default=[], metavar='NAME=SHA256',
               help='Accept only the exact digest of a visually reviewed replacement')
a = p.parse_args()
if not a.existing_snaps:
    subprocess.run(['python3', 'scripts/snap-ui.py', str(a.out / 'snaps')], check=True, capture_output=True)
reviewed = dict(value.split('=', 1) for value in a.reviewed)
snapshots = list(a.baseline.glob('*.png'))
assert len(snapshots) == 9, 'expected the nine regression snapshots'
assert reviewed.keys() <= {p.name for p in snapshots}, 'unknown reviewed snapshot'
for before in snapshots:
    after = a.out / 'snaps' / before.name
    if before.name in reviewed:
        assert hashlib.sha256(after.read_bytes()).hexdigest() == reviewed[before.name], f'reviewed snapshot changed: {before.name}'
    else:
        assert after.read_bytes() == before.read_bytes(), f'snapshot changed: {before.name}'
print(f'All nine snapshots pass ({len(reviewed)} exact reviewed replacements)', flush=True)
cases = {
    'stream': ['--scene', 'streaming', '--frames', '1440'],
    'send': ['--scene', 'session', '--script', 'examples/opencode/scripts/send.json', '--frames', '720'],
    'switch': ['--script', 'examples/opencode/scripts/switch.json', '--frames', '1080'],
    'scroll': ['--scene', 'session', '--script', 'examples/opencode/scripts/scroll.json', '--frames', '480'],
    'reduced': ['--scene', 'streaming', '--reduced-motion', '--frames', '1440'],
    'long-stream': ['--scene', 'streaming', '--frames', '1440'],
    'long-scroll': ['--scene', 'session', '--script', 'examples/opencode/scripts/scroll.json', '--frames', '480'],
    'send-overlap': ['--scene', 'session', '--script', 'examples/opencode/scripts/send-overlap.json', '--frames', '720'],
    'reduced-send': ['--scene', 'session', '--script', 'examples/opencode/scripts/send.json', '--reduced-motion', '--frames', '720'],
}
for script in a.script:
    cases[script.stem] = ['--scene', 'session', '--script', str(script), '--frames', '960']
    cases['reduced-' + script.stem] = [*cases[script.stem], '--reduced-motion']
assert set(a.case) <= cases.keys(), 'unknown motion case'
for name, args in cases.items():
    if a.case and name not in a.case:
        continue
    out = a.out / 'traces' / name
    out.mkdir(parents=True, exist_ok=True)
    fixture = a.long_fixture if name.startswith('long-') else pathlib.Path('examples/opencode/assets/fixture.json')
    run = subprocess.run(['target/release/opencode', '--fixture', str(fixture), '--fps', '120', *args,
                          '--trace', str(out / 'trace.jsonl')], capture_output=True, text=True)
    (out / 'run.log').write_text(run.stderr)
    if run.returncode:
        raise RuntimeError(run.stderr)
    lint = subprocess.run(['bun', str(pathlib.Path.home() / 'code/open-source/motion-lint/cli/motion-lint.ts'),
                           'analyze', str(out), '--check'], capture_output=True, text=True)
    (out / 'lint.log').write_text(lint.stdout + lint.stderr)
    print(name, (lint.stdout or lint.stderr).strip().split('\n')[0], flush=True)
    if lint.returncode:
        raise RuntimeError(lint.stdout + lint.stderr)

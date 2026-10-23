#!/usr/bin/env python3
"""Check a timestamp-only `pacing --native --trace FILE --frames 240` run."""
import json
import statistics
import sys

with open(sys.argv[1]) as source:
    header, *frames = [json.loads(line) for line in source]
assert header['clock'] != 'virtual', 'headless timestamps cannot prove native pacing'
assert len(frames) >= 60, f'only {len(frames)} native frames'
assert sum(frame.get('quality', {}).get('animating', False) for frame in frames) >= 10, 'no spring animation'
# Exclude the first frame/initial gap, not later commit or idle-resume gaps.
gaps = [b['t'] - a['t'] for a, b in zip(frames[1:], frames[2:])]
half_refresh = 500 / header['hz']
bad = [gap for gap in gaps if gap < half_refresh - 0.01]
print(json.dumps({'frames': len(frames), 'hz': header['hz'], 'half_refresh_ms': half_refresh,
    'minimum_gap_ms': min(gaps), 'median_gap_ms': statistics.median(gaps), 'short_gaps': len(bad)}, indent=2))
assert not bad, f'{len(bad)} sub-half-refresh gaps'

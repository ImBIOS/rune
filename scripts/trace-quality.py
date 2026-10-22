#!/usr/bin/env python3
"""Research-15 metrics from Rune traces. Virtual cadence is not measured display cadence."""
import json
import statistics
import sys

for path in sys.argv[1:]:
    rows = [json.loads(s) for s in open(path)]
    header = rows[0]
    frames = [r for r in rows if 'quality' in r]
    period = 1000 / header['hz']
    intervals = [b['t'] - a['t'] for a, b in zip(frames, frames[1:])]
    duration = frames[-1]['t'] - frames[0]['t']
    q = [r['quality'] for r in frames]
    cpu = [r['quality']['cpu_ms'] for r in frames[1:]]
    errors = sorted(r['position_error_px'] for r in q)
    print(json.dumps({
        'trace': path, 'clock': header['clock'],
        'position_frame_max_p99_px': errors[round((len(errors) - 1) * .99)],
        'position_max_px': max(errors),
        'retarget_lanes': sum(r['retarget_lanes'] for r in q),
        'retarget_velocity_max_pt_s': max(r['velocity_error_pt_s'] for r in q),
        'virtual_hitch_ms_s': sum(max(0, t - period) for t in intervals) / duration * 1000 if duration else 0,
        'virtual_cadence_sigma_periods': statistics.pstdev(t / period - 1 for t in intervals) if intervals else 0,
        'cpu_budget_excess_ms_s': sum(max(0, t - period) for t in cpu) / duration * 1000 if duration else 0,
        'animating_at_end': q[-1]['animating'],
    }, indent=2))

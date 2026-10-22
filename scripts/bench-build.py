#!/usr/bin/env python3
"""Run only inside HiveMind. A real one-line leaf edit, with source restored afterwards."""
import json
import os
import pathlib
import statistics
import subprocess
import time

src = pathlib.Path('examples/opencode/src/main.rs')
original = src.read_text()
times = []
try:
    for i in range(8):
        src.write_text(original + f'\n// compile benchmark edit {i}\n')
        start = time.monotonic()
        subprocess.run(['cargo', 'build', '--release', '-p', 'opencode'], check=True)
        elapsed = time.monotonic() - start
        if i:
            times.append(elapsed)
finally:
    src.write_text(original)
median = statistics.median(times)
print(json.dumps({'incremental': os.getenv('CARGO_PROFILE_RELEASE_INCREMENTAL', 'false'),
                  'median_s': median, 'mad_s': statistics.median(abs(t - median) for t in times),
                  'runs_s': times}), flush=True)

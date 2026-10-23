"""Pro-only, no capture. Post-commit cadence of the `pacing` example under /tmp/pro-gui.lock.

python3 scripts/native-pacing/commit-probe.py <out> <pacing executable> [runs]

Each burst starts after an idle gap. Prints the first acquisition (`nextDrawable` entry) and
positive Metal presentedTime intervals of each burst, plus totals over all bursts.
"""
import fcntl
import json
import os
import re
import socket
import statistics
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from perf_binary import perf_binary

assert socket.gethostname().startswith("Kits-Shiny-Computer"), "Pro only"
out, pacing = Path(sys.argv[1]).resolve(), perf_binary(sys.argv[2])
runs = int(sys.argv[3]) if len(sys.argv) > 3 else 3
out.mkdir(parents=True, exist_ok=True)
library = out / "steady-probe.dylib"
subprocess.run(["clang", "-dynamiclib", "-fobjc-arc", "-fblocks", "-O2", str(Path(__file__).with_name("steady-probe.m")),
                "-framework", "AppKit", "-framework", "QuartzCore", "-framework", "Metal", "-o", str(library)], check=True)
env = {k: v for k, v in os.environ.items() if not k.startswith("STEADY_")}
env["DYLD_INSERT_LIBRARIES"] = str(library)


def gaps(times):
    return [round((b - a) * 1000, 1) for a, b in zip(times, times[1:])]


summary = []
with open("/tmp/pro-gui.lock", "a") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    try:
        for run in range(runs):
            log = out / f"{run}-pacing.log"
            trace = out / f"{run}-pacing.jsonl"
            with log.open("w") as output:
                subprocess.run([str(pacing), "--native", "--trace", str(trace)], env=env, stdout=output,
                               stderr=subprocess.STDOUT, timeout=60, check=True)
            rows = [{k: float(v) for k, v in re.findall(r"(\w+)=([\d.]+)", line)}
                    for line in log.read_text().splitlines() if line.startswith("steady frame ")]
            rows.sort(key=lambda r: r["entered"])
            bursts, current = [], []
            for row in rows:
                if current and row["entered"] - current[-1]["entered"] > 0.05:
                    bursts.append(current)
                    current = []
                current.append(row)
            bursts.append(current)
            for index, burst in enumerate(bursts[1:], 1):
                presented = sorted(r["presented"] for r in burst if r["presented"] > 0)
                result = dict(run=run, burst=index, frames=len(burst), zero=sum(r["presented"] == 0 for r in burst),
                              acquisition_gaps_ms=gaps([r["entered"] for r in burst]),
                              presentation_gaps_ms=gaps(presented),
                              latency_ms=round(statistics.median((r["presented"] - r["entered"]) * 1000
                                                                for r in burst if r["presented"] > 0), 1),
                              wait_ms=round(statistics.median((r["acquired"] - r["entered"]) * 1000 for r in burst), 2))
                summary.append(result)
                print(json.dumps(result), flush=True)
    finally:
        library.unlink(missing_ok=True)
acquisition = [g for r in summary for g in r["acquisition_gaps_ms"]]
presentation = [g for r in summary for g in r["presentation_gaps_ms"]]
first = [g for r in summary for g in r["acquisition_gaps_ms"][:4]]
total = dict(bursts=len(summary), frames=sum(r["frames"] for r in summary),
             median_acquisition_ms=statistics.median(acquisition), median_presentation_ms=statistics.median(presentation),
             first_four_acquisition_gaps_under_12ms=sum(g < 12 for g in first), first_four_acquisition_gaps=len(first),
             presentation_gaps_under_12ms=sum(g < 12 for g in presentation),
             median_latency_ms=statistics.median(r["latency_ms"] for r in summary))
(out / "summary.json").write_text(json.dumps(dict(total=total, bursts=summary), indent=2) + "\n")
print(json.dumps(total), flush=True)

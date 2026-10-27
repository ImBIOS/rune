"""Pro-only, no capture. Same 3 batches x 3 intervals and endpoint as the overlay team's probe.

Run through hash-checked GUI admission on the build queue. A matched 1360x320 Rune surface is
tested with both injected 60 Hz redraws and its real continuous animation scheduler.
"""
import argparse
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import re
import socket
import signal
import statistics
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from perf_binary import perf_binary

parser = argparse.ArgumentParser()
parser.add_argument("out", type=Path)
parser.add_argument("rune", type=Path)
parser.add_argument("--reference", type=Path)
parser.add_argument("--modes", default="injected,native")
parser.add_argument("--display-link", choices=["cv", "ca"], default="cv")
parser.add_argument("--drawables", choices=["2", "3"])
parser.add_argument("--display-sync", choices=["0", "1"])
parser.add_argument("--guard", choices=["production", "no-half"], help="requires the diagnostic admission patch")
args = parser.parse_args()
assert socket.gethostname().startswith("Kits-Shiny-Computer"), "Pro only"
root, rune = args.out.resolve(), perf_binary(args.rune)
root.mkdir(parents=True, exist_ok=True)


def stats(values):
    if not values:
        return None
    return dict(n=len(values), median=statistics.median(values),
                p95=sorted(values)[math.ceil(len(values) * .95) - 1], min=min(values), max=max(values))


def parse(path, mode):
    rows, configs, clocks, guards = [], [], [], []
    for line in path.read_text().splitlines():
        if line.startswith("steady config "):
            configs.append(line)
        if line.startswith("steady clock "):
            clocks.append(line)
        if line.startswith("steady guard "):
            guards.append(line)
        if line.startswith("steady frame "):
            rows.append({k: float(v) for k, v in re.findall(r"(\w+)=([\d.]+)", line)})
    visible = [r for r in rows if r["visible"] and r["window"] > 0]
    assert visible, f"no visible frames: {path}"
    start = min(r["entered"] for r in visible) + 2
    selected = [r for r in visible if start <= r["entered"] < start + 9]
    valid = [r for r in selected if r["presented"] >= r["acquired"]]
    assert len(valid) >= 30, f"too few valid presentations: {path}"
    intervals = [stats([(r["presented"] - r["entered"]) * 1000 for r in valid
                        if start + i * 3 <= r["entered"] < start + (i + 1) * 3]) for i in range(3)]
    requests = [r for r in valid if 0 < r["requested"] <= r["entered"]]
    entries = sorted(r["entered"] for r in selected)
    gaps = [(b - a) * 1000 for a, b in zip(entries, entries[1:])]
    presented = sorted(set(r["presented"] for r in valid))
    presentation_gaps = [(b - a) * 1000 for a, b in zip(presented, presented[1:])]
    if args.guard and mode != "reference":
        assert guards and all(f"mode={args.guard} " in line for line in guards), "wrong admission-probe binary"
    return dict(configs=configs, clocks=clocks, guards=guards, positive=len(valid), visible_callbacks=len(selected),
                acquisition_hz=(len(entries)-1)/(entries[-1]-entries[0]),
                presentation_hz=(len(presented)-1)/(presented[-1]-presented[0]),
                presentation_gaps_ms=stats(presentation_gaps),
                zero_callbacks=sum(r["presented"] == 0 for r in selected),
                acquire_to_present_ms=stats([(r["presented"] - r["entered"]) * 1000 for r in valid]),
                next_drawable_wait_ms=stats([(r["acquired"] - r["entered"]) * 1000 for r in valid]),
                request_to_present_ms=stats([(r["presented"] - r["requested"]) * 1000 for r in requests]),
                acquisition_gaps_ms=stats(gaps), sub_half_refresh_gaps=sum(g < 4.166666 - .01 for g in gaps),
                interval_medians=intervals)


with open("/tmp/pro-gui.lock", "a") as lock:
    def lock_timeout(_signal, _frame):
        raise TimeoutError("Pro GUI lock remained busy for ten minutes")
    signal.signal(signal.SIGALRM, lock_timeout)
    signal.alarm(600)
    fcntl.flock(lock, fcntl.LOCK_EX)
    signal.alarm(0)
    for pattern in ["[r]une-smoke/ci/Hex.app", "[s]crawl lab"]:
        assert subprocess.run(["pgrep", "-f", pattern], capture_output=True).returncode != 0, pattern
    for name in ["overlay-lab", "pacing_overlay"]:
        assert subprocess.run(["pgrep", "-x", name], capture_output=True).returncode != 0, name
    library = root / "steady-probe.dylib"
    subprocess.run(["clang", "-dynamiclib", "-fobjc-arc", "-fblocks", "-O2",
                    str(Path(__file__).with_name("steady-probe.m")), str(Path(__file__).with_name("clock-probe.m")),
                    "-Wno-deprecated-declarations", "-framework", "AppKit", "-framework", "CoreVideo",
                    "-framework", "QuartzCore", "-framework", "Metal", "-o", str(library)], check=True)
    env = dict(os.environ, DYLD_INSERT_LIBRARIES=str(library))
    for key in ["OVERLAY_LAB_CONTROL", "OVERLAY_PROBE_REDRAW", "RUNE_OVERLAY_TRACE_INPUT",
                "STEADY_RUNE_REDRAW", "STEADY_MAX_DRAWABLES", "STEADY_TRANSACTION", "PACING_NATIVE",
                "STEADY_CA", "STEADY_CLOCK", "STEADY_DISPLAY_SYNC", "STEADY_GUARD"]:
        env.pop(key, None)
    modes = args.modes.split(",") + (["reference"] if args.reference else [])
    assert all(mode in ["injected", "native", "reference"] for mode in modes)
    results = []
    (root / "load-before.txt").write_bytes(subprocess.check_output(["top", "-l", "1", "-n", "0"]))
    try:
        for batch in range(3):
            for mode in (modes if batch % 2 == 0 else list(reversed(modes))):
                command = [str(rune)] if mode != "reference" else [str(args.reference.resolve()), "lab", "pill",
                    "tap-then-screen", "--repeat", "0", "--time-scale", "1", "--no-backdrop"]
                runenv = dict(env)
                if mode != "reference":
                    runenv["STEADY_CLOCK"] = "1"
                    if args.display_link == "ca": runenv["STEADY_CA"] = "1"
                    if args.drawables: runenv["STEADY_MAX_DRAWABLES"] = args.drawables
                    if args.display_sync: runenv["STEADY_DISPLAY_SYNC"] = args.display_sync
                    if args.guard: runenv["STEADY_GUARD"] = args.guard
                if mode == "injected":
                    runenv["STEADY_RUNE_REDRAW"] = "1"
                elif mode == "native":
                    runenv["PACING_NATIVE"] = "1"
                log = root / f"{batch}-{mode}.log"
                with log.open("w") as output:
                    process = subprocess.Popen(command, env=runenv, stdout=output, stderr=subprocess.STDOUT)
                    try:
                        process.wait(timeout=14 if mode == "reference" else 22)
                        assert mode != "reference" and process.returncode == 0, (mode, process.returncode)
                    except subprocess.TimeoutExpired:
                        if mode != "reference":
                            subprocess.run(["sample", str(process.pid), "1", "-file", str(root / f"{batch}-{mode}-timeout.txt")], timeout=10)
                            raise
                    finally:
                        if process.poll() is None:
                            process.terminate()
                            try:
                                process.wait(timeout=5)
                            except subprocess.TimeoutExpired:
                                process.kill()
                                process.wait(timeout=5)
                result = dict(batch=batch, mode=mode, **parse(log, mode))
                results.append(result)
                print(json.dumps(result), flush=True)
        summary = dict(endpoint="nextDrawable entry to positive Metal presentedTime",
                       rune_sha256=hashlib.sha256(rune.read_bytes()).hexdigest(), runs=results,
                       experiment=dict(display_link=args.display_link, drawables=args.drawables,
                                       display_sync=args.display_sync, guard=args.guard))
        if args.reference:
            summary["reference_sha256"] = hashlib.sha256(args.reference.read_bytes()).hexdigest()
        for mode in modes:
            medians = [interval["median"] for r in results if r["mode"] == mode
                       for interval in r["interval_medians"] if interval]
            assert len(medians) == 9, (mode, medians)
            summary[mode] = stats(medians)
        (root / "results.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps({k: v for k, v in summary.items() if k != "runs"}), flush=True)
    finally:
        library.unlink(missing_ok=True)
        (root / "load-after.txt").write_bytes(subprocess.check_output(["top", "-l", "1", "-n", "0"]))
print("All disposable apps exited; GUI lock released.", flush=True)

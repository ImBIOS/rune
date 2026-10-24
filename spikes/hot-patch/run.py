#!/usr/bin/env python3
"""Run only on a build worker, inside a cargo lease on the build queue.

Consumes actual frame events (no status polling). All clocks are on the worker.
The save timestamp is immediately after close; no network sync is in the metric.
"""
import argparse
import json
import math
import os
from pathlib import Path
import queue
import re
import signal
import statistics
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parent
SOURCE = ROOT / "src/main.rs"
BASE = SOURCE.read_text()
RESULTS = ROOT / "results"
RESULTS.mkdir(exist_ok=True)
ENV = dict(os.environ, CARGO_BUILD_JOBS="2", CARGO_INCREMENTAL="1", NO_COLOR="1")
# The worker normally uses sccache, which rejects explicit incremental builds.
ENV["RUSTC_WRAPPER"] = ""
DX = str(Path.home() / ".cargo/bin/dx")
COMMAND = [DX, "serve", "--hot-patch", "--platform", "server", "--open", "false",
           "--interactive", "false", "--addr", "127.0.0.1", "--port", "18743"]


def save(text):
    SOURCE.write_text(text)
    return time.time_ns()


def parse_output(frame):
    return dict(part.split("=", 1) for part in frame["output"].split(";"))


class Session:
    def __init__(self, name):
        self.events = queue.Queue()
        self.last = None
        self.patch_ns = []
        self.log = (RESULTS / f"{name}.log").open("w")
        self.started_ns = time.time_ns()
        self.process = subprocess.Popen(COMMAND, cwd=ROOT, env=ENV,
                                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                        text=True, bufsize=1, start_new_session=True)
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()

    def read(self):
        for line in self.process.stdout:
            self.log.write(line)
            self.log.flush()
            if "Build failed" in line or "could not compile" in line:
                self.events.put(None)
            patch = re.search(r'PATCH ns=(\d+)', line)
            if patch:
                self.patch_ns.append(int(patch.group(1)))
            match = re.search(r'FRAME (\{[^\n]+\})', line)
            if match:
                try:
                    self.events.put(json.loads(match.group(1)))
                except json.JSONDecodeError:
                    pass
        self.events.put(None)

    def until(self, predicate, timeout=120):
        deadline = time.monotonic() + timeout
        while True:
            item = self.events.get(timeout=max(0, deadline - time.monotonic()))
            if item is None:
                raise RuntimeError(f"dx exited: {self.process.poll()}")
            previous, self.last = self.last, item
            if predicate(item):
                return previous, item

    def ready(self):
        self.until(lambda f: f["counter"] >= 60, timeout=600)
        return (self.last["ns"] - self.started_ns) / 1e6

    def edit(self, text, predicate, timeout=30):
        before = self.last
        saved = save(text)
        try:
            previous, after = self.until(lambda f: f["ns"] >= saved and predicate(f), timeout)
            result = {
                "saved_ns": saved, "latency_ms": (after["ns"] - saved) / 1e6,
                "before": before, "previous": previous, "after": after,
                "same_pid": before["pid"] == after["pid"],
                "same_vec": before["vec_ptr"] == after["vec_ptr"],
                "same_state": before["state_addr"] == after["state_addr"],
                "counter_advanced": after["counter"] > before["counter"],
                "vector_invariant": after["vec0"] == after["counter"] + 73,
                "patch_ns": [ns for ns in self.patch_ns if ns >= saved],
            }
        except (queue.Empty, RuntimeError) as exc:
            result = {"saved_ns": saved, "error": type(exc).__name__ + ": " + str(exc),
                      "before": before, "last": self.last,
                      "dx_exit": self.process.poll()}
        print(json.dumps(result), flush=True)
        return result

    def close(self):
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(self.process.pid, signal.SIGKILL)
            self.process.wait()
        self.reader.join(timeout=5)
        self.log.close()


def benchmark():
    session = Session("benchmark")
    rows = []
    try:
        ready_ms = session.ready()
        for revision in range(1, 11):
            text = BASE.replace("let revision = 0;", f"let revision = {revision};")
            row = session.edit(text, lambda f: parse_output(f)["rev"] == str(revision))
            rows.append(row)
            if "error" in row:
                break
            # Let at least 30 real frames flow between saves; do not overlap builds.
            counter = row["after"]["counter"]
            session.until(lambda f: f["counter"] >= counter + 30)
        times = sorted(r["latency_ms"] for r in rows if "latency_ms" in r)
        result = {"command": COMMAND, "launch_to_tick60_ms": ready_ms, "rows": rows,
                  "median_ms": statistics.median(times) if times else None,
                  "p95_nearest_rank_ms": times[math.ceil(.95 * len(times)) - 1] if times else None}
        (RESULTS / "benchmark.json").write_text(json.dumps(result, indent=2))
    finally:
        session.close()
        save(BASE)


def probes():
    changes = {
        "generic": (BASE.replace('"g0:{value}"', '"g1:{value}"'),
                    lambda f: parse_output(f)["generic"].startswith("g1:")),
        "closure": (BASE.replace("value + 10", "value + 20"),
                    lambda f: int(parse_output(f)["closure"]) == f["counter"] + 20),
        "trait_impl": (BASE.replace('"t0:{}x{}"', '"t1:{}x{}"'),
                       lambda f: parse_output(f)["trait"].startswith("t1:")),
        "signature": (BASE.replace("fn render(state: &AppState)", "fn render(state: &AppState, tone: u64)")
                      .replace("let revision = 0;", "let revision = tone;")
                      .replace("            render(&state)", "            render(&state, 99)"),
                      lambda f: parse_output(f)["rev"] == "99"),
        # Only compare sizes; NEVER read the new field through the old instance.
        "state_field": (BASE.replace("// FIELD_PROBE", "extra: u64,")
                        .replace("// FIELD_INIT", "extra: 999,"),
                        lambda f: parse_output(f)["layout"] == "56"),
        "static_initializer": (BASE.replace("AtomicU64::new(1000)", "AtomicU64::new(9000)")
                               .replace("let revision = 0;", "let revision = 91;"),
                               lambda f: parse_output(f)["rev"] == "91"),
        "tls_initializer": (BASE.replace("Cell::new(2000)", "Cell::new(9000)")
                            .replace("let revision = 0;", "let revision = 92;"),
                            lambda f: parse_output(f)["rev"] == "92"),
    }
    rows = {}
    for name, (text, predicate) in changes.items():
        save(BASE)
        session = Session("probe-" + name)
        try:
            session.ready()
            rows[name] = session.edit(text, predicate, timeout=15)
        finally:
            session.close()
            save(BASE)
        (RESULTS / "probes.json").write_text(json.dumps(rows, indent=2))


def builds():
    target = os.environ.get("RUNE_NORMAL_TARGET", "target/normal")
    if (ROOT / target).exists():
        raise RuntimeError(f"{target} already exists; select a fresh RUNE_NORMAL_TARGET for a cold build")
    rows = []
    try:
        for name, text in [("cold", BASE),
                           ("incremental", BASE.replace("let revision = 0;", "let revision = 1;"))]:
            save(text)
            command = ["cargo", "build", "--target-dir", target]
            start = time.monotonic_ns()
            with (RESULTS / f"build-{name}.log").open("w") as out:
                run = subprocess.run(command, cwd=ROOT, env=ENV, stdout=out, stderr=subprocess.STDOUT)
            rows.append({"name": name, "elapsed_ms": (time.monotonic_ns() - start) / 1e6,
                         "exit": run.returncode, "command": command})
            print(json.dumps(rows[-1]), flush=True)
            if run.returncode:
                break
        (RESULTS / "builds.json").write_text(json.dumps(rows, indent=2))
    finally:
        save(BASE)


def extras():
    closure = """subsecond::call(|| {
            std::hint::black_box(&capture_guard);
            render(&state)
        })"""
    cases = {
        "single_capture": BASE.replace(closure, "subsecond::call(|| render(&state))"),
        "explicit_hotfn": BASE.replace(closure, "subsecond::HotFn::current(render).call((&state,))"),
        "explicit_hotfn_signature": BASE.replace(closure, "subsecond::HotFn::current(render).call((&state,))"),
        "exported_static": BASE.replace("static GLOBAL:", "#[unsafe(no_mangle)]\npub static GLOBAL:"),
        "cached_trait_object": BASE.replace("// FIELD_PROBE", "cached: Box<dyn Label>,")
            .replace("// FIELD_INIT", "cached: Box::new(Panel { width: 42, height: 24 }),")
            .replace("state.panel.label()", "state.cached.label()"),
    }
    rows = {}
    for name, base in cases.items():
        if args.case and args.case != name:
            continue
        save(base)
        session = Session("extra-" + name)
        try:
            session.ready()
            text = base.replace("let revision = 0;", "let revision = 93;")
            if name == "explicit_hotfn_signature":
                text = text.replace("fn render(state: &AppState)", "fn render(state: &AppState, tone: u64)")
                text = text.replace("let revision = 93;", "let revision = tone;")
                text = text.replace(".call((&state,))", ".call((&state, 93))")
            if name == "exported_static":
                text = text.replace("AtomicU64::new(1000)", "AtomicU64::new(9000)")
            if name == "cached_trait_object":
                text = text.replace('"t0:{}x{}"', '"t1:{}x{}"')
            rows[name] = session.edit(text, lambda f: parse_output(f)["rev"] == "93", timeout=5)
            rows[name]["all_patch_ns"] = session.patch_ns
        finally:
            session.close()
            save(BASE)
        filename = f"extra-{args.case}.json" if args.case else "extras.json"
        (RESULTS / filename).write_text(json.dumps(rows, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["benchmark", "probes", "builds", "extras"])
    parser.add_argument("--case", help="Run only this extra probe")
    args = parser.parse_args()
    globals()[args.mode]()

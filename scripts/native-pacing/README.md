# Native presentation pacing

Timestamp-only Pro probe. No screen capture. It measures `nextDrawable` entry to
positive Metal `presentedTime`, using the overlay team's steady-state endpoint.
Three batches per mode, two-second warmup, then three three-second intervals;
the summary is the median of nine interval medians.

Build `cargo build --profile perf -j 2 -p rune --example pacing_overlay --example pacing`
through HiveMind. Generate a SHA-256 source manifest, verify it in each job, and
run jobs serially with `HIVEMIND_CAS_SYNC=0 HIVEMIND_STRICT_PIN=1`. Native jobs use
`hivemind run --kind gui --priority high --worker pro --allow-local --fetch <out>`.
Pass the worker's existing `target/perf/examples/` executable path, not a local executable;
the runners refuse binaries from any other profile.

```sh
python3 scripts/native-pacing/run.py <out> <pacing_overlay> \
  --scrawl ~/Applications/Scrawl.app/Contents/MacOS/scrawl
```

The runner asserts the Pro hostname, holds `/tmp/pro-gui.lock`, bounds app
lifetimes, and removes its instrumentation dylib. `--modes native` selects only
the continuous Rune workload; the default tests injected redraws and native
animation. Scrawl is optional and its order alternates between batches.

Rune uses a 680×160-point overlay. Native mode animates opacity continuously;
injected mode uses a static view with an AppKit 60-Hz redraw timer. Scrawl runs
its installed `lab pill tap-then-screen` scene on a different-sized surface.
Results include executable hashes, raw callback logs, drawable waits, interval
medians, acquisition gaps, and counts of excluded zero presentation timestamps.
These excluded callbacks are not a dropped-frame count. A timeout is a failed
run and cannot serve as a complete performance baseline.

The `pacing` example separately exercises state commits, idle, hide/show and
shutdown. Run it under the same Pro GUI lock with `--native --trace <file>`, then
check the file with `python3 scripts/check-native-pacing.py <file>`.

## Isolated refresh-rate experiments

Use a fresh output directory for each experiment:

```sh
# Same production executable; one change at a time, in this order.
python3 scripts/native-pacing/run.py <out-ca> <pacing_overlay> --modes native --display-link ca
python3 scripts/native-pacing/run.py <out-two> <pacing_overlay> --modes native --drawables 2
python3 scripts/native-pacing/run.py <out-unsynced> <pacing_overlay> --modes native --display-sync 0
```

`clock-probe.m` interposes CV callback registration/start/stop. Its default mode
counts real CV callbacks. CA mode supplies the same Rune callback from an NSView
`CADisplayLink` with `CAFrameRateRangeMake(80, 120, 120)` on macOS 14+. This adapter
selects the fixture's visible panel and supports this single-surface workload;
it is not a general replacement for Rune's native display-link implementation.
Layer overrides affect Rune only. An optional Scrawl reference keeps its settings.

For the admission-guard comparison, apply `admission-probe.patch` in a disposable
checkout, generate a new source manifest, and rebuild the example through
HiveMind. Run `--guard production` then `--guard no-half` on that same executable.
The latter removes only elapsed-half-refresh admission, retaining stale-target
and duplicate-target rejection. The runner requires matching diagnostic output.
Counters are printed after stopping the link, not in the callback. Revert the
patch and rebuild when finished. Never ship this patch; it exists to reproduce
the experiment and intentionally leaves the original unit-test helper unused.

`acquisition_hz` uses `nextDrawable` entry times. `presentation_hz` uses distinct,
positive `presentedTime` timestamps for frames acquired in the nine-second sample.
Callback Hz and guard counts cover the full app run, including warmup. Guard
reason counters can overlap; they are not an exhaustive partition of callbacks
because pending callbacks coalesce. The summary includes the selected overrides
and binary hash. Raw logs retain the actual layer settings.

With display sync disabled, positive presentation timestamps do not establish
tear-free 120-Hz scanout. Apple's API explicitly allows tearing in this mode.
See [findings](FINDINGS.md) for the measured results and limitations.

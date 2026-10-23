# Native presentation pacing

Timestamp-only Pro probe. No screen capture. It measures `nextDrawable` entry to
positive Metal `presentedTime`, using the overlay team's steady-state endpoint.
Three batches per mode, two-second warmup, then three three-second intervals;
the summary is the median of nine interval medians.

Build `cargo build --release -j 2 -p rune --example pacing_overlay --example pacing`
through HiveMind. Generate a SHA-256 source manifest, verify it in each job, and
run jobs serially with `HIVEMIND_CAS_SYNC=0 HIVEMIND_STRICT_PIN=1`. Native jobs use
`hivemind run --kind gui --priority high --worker pro --allow-local --fetch <out>`.
Pass the worker's existing release executable path, not a local executable.

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

# Live bindings for Rooms

Target: 16 meters at 60 Hz and four cursors at 120 Hz without rebuilding views.
One warmup, seven measured runs, median frame CPU, Studio headless. Source and
runtime tests/builds run through HiveMind on mini; no native window was opened.

Baseline (before binding integration): **16.417 µs** median CPU frame, seven run
medians 16.167–16.542 µs. `examples/live-values --bench`; synthetic poll, view,
retained layout, Scene and instance build included, GPU/driver excluded. The saved
baseline executable lives in ignored `perf/live-values/baseline` for paired runs.

Hypothesis: publishing to the existing Scene properties removes view allocation,
diff and layout work. Bindings use OPACITY/OFFSET/SCALE and the renderer's P_FILL;
no instance ABI or shader changes. Whole-value atomics avoid torn cursor/colour
lanes under concurrent writers. A visible-window wake is coalesced until the next
latch. The native shell acquires its drawable before the frame to avoid sampling
before a potentially blocking drawable wait. There is still no separate render
thread or presentation-time clock.

## Result

- CPU-only frame: **16.791 → 1.458 µs**, median of seven run medians in the same
  executable (11.5× faster). Rebuild run range 16.125–17.542 µs; live range
  1.458–1.459 µs. The pre-integration reference was 16.417 µs.
- Including wgpu encode/submit, seven alternating pairs: p50 **.059 → .055 ms**,
  p95 **.264 → .242 ms**. Driver/load noise dominates: per-run p50 ranges were
  .047–.232 and .043–.242 ms. This does **not** establish an end-to-end speedup.
  An earlier sequential run (.220 → .191 ms) was discarded as comparative proof.
- Simulated 120 Hz consumption: **120 coalesced wakes/second** for 1,440 source
  publications/second; **0 wakes and 0 scheduled frames** across 1,200 unchanged
  ticks after production stops. These are scheduler-model counters, not OS wakeups.
- Counting allocator: **0 producer allocations**, including the first publication
  on a fresh thread, and **0 allocations in 120 warmed CPU presentation frames**.
  GPU/driver and mounting/commit allocations are outside this gate.
- Targeted tests cover coherent concurrent colour writers, coalescing, visibility,
  teardown, shared sources across runtimes, unbinding with a pending wake, exact
  live fill across rebuilds, and interruptible smoothing across view rebuilding.
- Nine Settings/OpenCode before/after PNGs are byte-identical. Demo goldens are
  untouched. Smooth live-values trace: **0 motion-lint candidates / 840 frames**.
  Native windows and hardware input-to-photon latency remain untested.

Keep: the CPU-only and allocation wins are stable; the public API bypasses all
view/layout commits. No measured layout/text optimization was removed. Values
feed the same Scene property path as GPU springs; no renderer instance ABI change.

# NativeSurface measurement

Goal: independent 1080p tiles and one 4K/5K share inside Rune at 60 Hz, without CPU
pixel copies. Primary metric: import/encode/submit CPU milliseconds per frame.
Secondary: GPU render-pass timestamp milliseconds, exact matching-size pixels,
bounded latest-wins handoff and GPU-safe CoreVideo lifetime.

Benchmark: `target/release/video-tiles --bench-native`. One warmup and seven measured
30-frame runs at 60 Hz per configuration. Reports per-run p50/p95 and median of p50s;
timestamp-capable adapters must produce all 30 GPU samples in every run.
Three prefilled synthetic IOSurfaces per tile, fresh frame IDs per draw. Source setup
and GPU wait are outside main-thread CPU timing. Completion-worker CPU time and the
metadata-only resolve submission are not included in that number. No window or physical screen capture.

There was no existing native-video path to compare against. The initial implementation
establishes the baseline before any optimization. Do not compare its GPU times to the
UI benchmark's CPU-only timing or call completion a displayed-frame timestamp.

Files: `crates/rune/src/native_surface/`, `examples/video-tiles/`; renderer integration
only splits uber-quad batches around video nodes in the same render pass.

Verification so far: native IOSurface and portable byte paths pass all 128 scalar
colour cases (601/709 matrix, sRGB/709 transfer, full/video range, endpoint/colour
codes), exact 1:1 NV12/BGRA patterns, Retina node geometry, painter order, fit/fill,
rounded/rectangular clipping, and ownership through unsubmitted commands. Pro M2 Max.

Timing experiment: wgpu 30 forbids mixing its regular encoding API with `as_hal_mut`;
the attempted raw Metal completed handler panicked in validation and was removed.
Supported render-pass timestamp queries plus per-submission callbacks replace it.
Same-command-buffer resolves produced stale or zero timestamp pairs on M4/M5; each
submission now owns a fresh query set and resolves its counters after render completion.
Completion callbacks run through coalesced nonblocking polling, with 1 ms sleeps between
polls, so presentation's exclusive device lock is never held behind a fence wait. A final
idle frame still reports. GPU seconds use the GPU clock, not the CPU `Instant` epoch.
These are GPU pass durations and callback-observed completion, not exact raw
command-buffer/presentation feedback.

Results: pending verified release binary and headless measurements.

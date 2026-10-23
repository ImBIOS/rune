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
submission leases a distinct counter pair and resolves it after render completion.
One query set supplies eight pairs, retained through metadata readback; when all are
busy, frames still render and report CPU completion without GPU timestamps.
Completion callbacks run through coalesced nonblocking polling, with 1 ms sleeps between
polls, so presentation's exclusive device lock is never held behind a fence wait. A final
idle frame still reports. GPU seconds use the GPU clock, not the CPU `Instant` epoch.
These are GPU pass durations and callback-observed completion, not exact raw
command-buffer/presentation feedback.

## Results (2026-10-22)

M4 Pro, Metal, release build `52b5995`. Every run produced 30/30 valid GPU samples
(1,200 including warmup). Raw runs: `native-surface-m4-pro.jsonl`.

| Configuration | CPU ms/frame | GPU ms/frame | CPU ms/tile | GPU ms/tile |
| --- | ---: | ---: | ---: | ---: |
| 1 × 1080p | 1.568 | 0.384 | 1.568 | 0.384 |
| 4 × 1080p | 2.104 | 1.467 | 0.526 | 0.367 |
| 9 × 1080p | 1.523 | 3.271 | 0.169 | 0.363 |
| 1 × 4K | 0.661 | 1.409 | 0.661 | 1.409 |
| 1 × 5K | 0.642 | 2.482 | 0.642 | 2.482 |

These are medians of seven run medians. The shared worker was under CPU contention:
CPU p95 reached 48.8 ms and tile-count CPU medians are not monotonic. This verifies the
GPU budget and complete timing samples, not sustained end-to-end 60 Hz or isolated CPU
capacity. There is no decoder/network/presentation in this benchmark.

On Pro M2 Max the overlap regression test's exclusive device-lock operation took
0.082 ms and returned 179.789 ms before GPU completion (427.147 ms workload). It uses
`Buffer::destroy`, which acquires the same wgpu-core snatch write lock as `queue.present`;
no real drawable presentation was measured. The initial smaller workload finished before
the probe on M5 and failed loudly, so the test uses a longer compute workload.

All nine settings/opencode snapshots are byte-identical to the pre-video binaries.
Stream, send, switch and scroll traces pass motion-lint with zero candidates. The
synthetic four-tile Retina snapshot also renders successfully. Native colour/sharpness/
lifetime tests pass on M5; the overlap test passes on M2 Max. The original Windows
cross-check passed before the Host rebase; no native Windows run was performed.

## Sustained headless readback fix

Per-frame query-set allocation exhausted Metal counter sample buffers during queued
headless rendering. The driver reported `Cannot allocate sample buffer`; wgpu then
lost the device, so the final pixel mapping failed with `BufferAsyncError`.
The bounded counter-pair pool above replaces that allocation pattern.

On M4 Pro, the new regression failed before the fix and passed after it: eight bursts
of 61 four-tile frames, one pixel readback per burst, with all 1,952 surface completion
notifications delivered. The existing colour/lifetime and CPU/GPU-overlap tests also
passed. Actual `video-tiles --at 0` and `--at 1` snapshots passed and are byte-identical
to each other and desktop-fidelity's original `--at 0` golden.

# 22. GPU-evaluated springs spike

2026-10-22. Base `9fb5400`. Offscreen Metal on Studio M5 Max; no window or screen capture.
Builds and CPU tests run through HiveMind. This is a retained-renderer experiment,
not automatic promotion of the existing runtime's scene graph.

## Hypothesis and scope

For independently translating/fading retained geometry, moving the three analytic
tracks per node to the vertex shader should remove CPU sampling and instance-buffer
rewrites. The CPU should write a single 16-byte viewport/time uniform per frame.
The additional vertex arithmetic must not consume the saved frame budget.

`Renderer::with_springs` opts in. A separate immutable instance-to-node ID buffer
lets many primitives share one property node without enlarging the existing 128-byte
instance ABI. `prepare` uploads geometry; subsequent `present_at` calls upload only
the clock. `write_spring_node` uploads one 144-byte node (three 48-byte tracks).
The normal renderer keeps its existing shader, bindings and CPU evaluation path.

The spike implements translation and opacity, fixed screen-space clipping and
unchanged colours. It does **not** implement a hierarchical transform/clip/effect
tree, affine scaling/rotation, animated clip tables, or OKLab colour tracks. Those
need a later promotion contract and should not be inferred from this experiment.

Tracks precompute the same coefficients as rune-motion's `Layer::spring_unbounded`.
The shader evaluates them at the target presentation timestamp. Negative bounce
uses the CPU implementation's `zeta = 1 / (1 + bounce)`, not `1 - bounce` (the latter
applies only for nonnegative bounce). Near-critical damping uses the same 1e-4 branch.
Split integer/fraction seconds preserve time precision at a day of uptime; supported
clock values are nonnegative and below 2^24 seconds. No accumulated integration.

## CPU boundary and idle scheduling

Retargeting samples CPU `(position, velocity)` and constructs the replacement track
at that same timestamp. Arbitrary-start analytic settle bounds come from rune-motion,
including its existing 120-second cap. The cap is a runtime policy, not a proof that
every extreme spring has physically settled. The shader lands exactly at target
when the deadline is reached. CPU deadline bookkeeping changes only on retarget;
`springs_animating(time)` is O(1). Draw the terminal frame, then pause the display
link. Initial mounts use constant tracks.

Layout/re-layout, text reflow and glyph atlas phase selection, streaming growth
caps, dependent scroll placement, presence disposal and completion callbacks stay
on CPU. Hit testing/accessibility/inspection must sample the same CPU tracks on
demand. The current `Runtime` consumes presented values every frame for these jobs,
so this spike does not automatically migrate its animations. Reduced-motion policy
also stays at the commit boundary: remove spatial tracks and use the normal fade.

## Reproduction and correctness

Build through HiveMind, fetch the release executable, then run it headless on Studio:

```sh
hivemind run --kind cargo --fetch target/release/examples/gpu_springs -- \
  env RUSTC_WRAPPER= cargo build --release -p rune --example gpu_springs
target/release/examples/gpu_springs
hivemind run --kind cargo -- env RUSTC_WRAPPER= \
  cargo test --release -p rune gpu_springs::tests
```

If HiveMind spills to Studio, its local path is
`~/mac-remote.noindex/cargo-target/gpu-springs-6abb49d1/release/examples/gpu_springs`;
local execution does not copy that binary into the requested fetch destination.
Check the build receipt before timing: an early attempt accidentally ran the stale
fetched executable and was discarded. The first dependency-cold remote build timed
out at 20 minutes. The warmed spike build took 54 seconds; no shared cache was reset.

The differential harness evaluates the shared spring function in a **vertex** shader,
passes values flat to a fragment shader and reads RGBA32Float pixels. It covers
4,096 seeded springs, four segments each (12,288 mid-flight retargets), five times
per segment: 81,920 samples. Durations .08–2 s, bounce -.9 through .99 including
both sides of the critical cutoff, spatial values 0–2048 px, initial velocities up
to 4096 px/s, opacity values 0–1, and start times near zero and one day.

Tolerance chosen before measuring: position .01 device px (below the 1/64 px grid),
opacity .0001 (about 1/39 of an 8-bit alpha step).
This permits f32 time/coefficient rounding and WGSL FMA/reassociation and native
transcendentals versus rune-motion's deterministic polynomial math. It does not
promise bit identity on other GPUs or arbitrary parameter ranges. Actual RGBA8
renders allow at most one channel step at antialiased edges; unchanged CPU fixture
goldens must remain byte-identical.

**Results:** 81,920/81,920 positions/opacities pass. Maximum spatial error
**.00756836 device px**, maximum clamped opacity error **.00000304**. All 16 RGBA8
comparisons (four node counts × four times) are **byte-identical**, not merely within
the one-step allowance. The focused CPU tail-bound test passes across under/critical/
overdamped springs and retargets. Scheduler checks include cancelling the final
moving node's deadline when it is replaced by a constant node.

Three failures improved the experiment:

1. WGSL reserves `target`; renamed the shader field `goal` (ABI unchanged).
2. Split **floating** timestamps still lost precision through Metal fast-math
   reassociation: a day-old opacity sample differed by .000405. Subtracting the
   integer whole-second components before conversion to f32 fixes the problem.
3. Near-critical overdamped roots amplify native-exp differences through large
   cancelling coefficients. One of 81,920 samples missed the .01 px gate (.0106201).
   Using rune-motion's exact exp polynomial for the overdamped branch brought it
   under the original gate. Critical/underdamped paths keep native exponentials.

GPU velocity is diagnostic only: max error .250977 units/s, worst normalized error
.004311 (normalizer max(|v|,1)). It failed an initially proposed .1+.0002*|v| gate
in near-critical cancellation. GPU velocities are **not used** to retarget, dispatch
events, or choose shutter samples. Retargets inherit CPU rune-motion velocity; the
position/opacity gate covers those generated segments. Do not promote this shader's
velocity output into a new consumer without its own precision work.

## Measurements

Each node-count comparison alternates paths, one warmup and seven measured
runs, 120 frames each, offscreen 512x512. Three tracks per node; all stay active.
CPU reference uses `AnimationStore`'s precomputed layers and dense active list,
with cached track IDs, not repeated coefficient setup or per-node hash lookup.
CPU time includes sampling/instance copy/upload (CPU path), uniform upload (GPU
path), command encoding and submission. GPU time is render-pass timestamp queries;
readback/wait happens outside CPU timing. Report medians of run medians and CPU MAD.
This serialized microbenchmark is not a physical display/pacing measurement.

| Nodes | CPU evaluator CPU µs (MAD) | GPU evaluator CPU µs (MAD) | CPU-path GPU µs | GPU-path GPU µs |
|---:|---:|---:|---:|---:|
| 1 | 32.750 (.292) | 30.583 (.292) | 9.375 | 10.458 |
| 100 | 34.750 (.500) | 30.208 (.958) | 11.958 | 13.000 |
| 1,000 | 67.125 (.875) | 29.958 (.625) | 19.792 | 21.000 |
| 10,000 | 355.417 (4.750) | 29.583 (.542) | 35.583 | 36.750 |

CPU reduction: 6.6%, 13.1%, 55.4%, 91.7%. At 10k the retained path saves
**.326 ms CPU/frame** for **.00117 ms GPU/frame**. A one-node win is mostly avoiding
the second buffer write; the convincing scaling win starts at 1k nodes. This measures
three scalar tracks and one small quad per node, not 10k full text-heavy cards.

Rebased verification on `8b37ced` (the root-workspace/text changes that landed during
the experiment) passes the CPU tail test, all 81,920 differential samples, all 16
byte-exact pixel comparisons and the idle/deadline-cancellation assertions. The
required recheck also repeats the seven-pair timing loop; its noisier measurements
are retained rather than replacing the first experiment:

| Nodes | CPU-path CPU µs (MAD) | GPU-path CPU µs (MAD) | CPU-path GPU µs | GPU-path GPU µs |
|---:|---:|---:|---:|---:|
| 1 | 34.917 (1.417) | 31.917 (1.625) | 10.250 | 10.875 |
| 100 | 44.416 (.874) | 43.250 (5.083) | 10.292 | 11.417 |
| 1,000 | 97.583 (13.042) | 43.250 (4.750) | 13.458 | 14.708 |
| 10,000 | 431.167 (46.376) | 32.666 (2.250) | 36.041 | 36.917 |

The 100-node saving is inside this run's noise; the 1k/10k win survives. Raw runs
and fixture summaries are in the worktree's `perf/gpu-springs/{spike.log,rebased.log,fixtures/results.json}`.

The initial same-command-buffer Metal query resolve returned reversed/stale timestamps
on some frames (unsigned subtraction then produced nonsense). Those timings were
discarded. Resolve now runs in a separate command buffer **after waiting for the draw**,
and asserts monotonic counters. The extra wait/resolve/readback is outside CPU timing.
The reported GPU times use only the render-pass timestamps. This is measurement
infrastructure, not a new frame-loop wait in the production renderer.

### OpenCode CPU fallback

`scripts/bench-gpu-springs.py` alternates seven fresh-process pairs after warmup for
the ordinary streaming fixture and research-19's synthetic 2,000-message fixture.
Both use the CPU runtime: no unsupported streaming track is silently promoted.

| Fixture | Before CPU p50/p95 ms | After CPU p50/p95 ms | Before/after p95 MAD ms |
|---|---:|---:|---:|
| Streaming | .294 / .558 | .309 / .643 | .045 / .050 |
| Long stream | .285 / 3.307 | .276 / 3.251 | .216 / .033 |

No streaming performance win is claimed. The ordinary fixture p95 rose .085 ms in
this busy-machine comparison; before runs ranged .463–.750 ms and after .419–.775 ms.
The baseline is the existing checked-out release executable, SHA-256
`02bfc65fb42c542e076f91797cd3468e034aa20e3f476ca53cf75024ac2e19e1`, not a newly built
exact-parent binary. Treat these as regression observations, not an isolated causal
benchmark of the optional binding check. Six fixture snapshots (both workloads at
.4, 4, 12 s) are byte-identical. Four 1,440-frame traces (before/after, each workload)
all pass motion-lint with **0 candidates**; each excuses 13 intended growth-height
changes. These trace findings concern the CPU fallback, not GPU-track instrumentation.

## Verdict

**Keep as an opt-in retained-renderer spike.** The scaling win is clear, correctness
passes the original position/opacity gates, and the default runtime stays CPU-owned.
Automatic runtime promotion is not justified yet: first build a retained-geometry
contract with lazy presented-value queries, per-node property ownership and a way
to combine CPU-driven growth/scroll with GPU transforms. Test animated clip/colour
and hierarchical transforms separately.

Independent Opus-medium review (dispatched by the parent) found **no blockers**.
Its non-blocking finding was fixed: `prepare` now writes only the eight viewport
bytes, preserving a timestamp previously supplied by `present_at` in the same frame.
The pixel harness deliberately calls `prepare` after `present_at` at all 16 sampled
states; all remain byte-identical and all 81,920 differential samples pass. Use
`gpu_springs --verify-only` to repeat correctness without rerunning timing loops.
Landing remains paused for the coordinated public-history reset; the old base is
`8b37ced6a841d5d9600ce030efcdb183501cd7d3`.

## Shutter motion blur

`rune-fx::MotionSamples::from_track` currently builds inverse poses on the CPU;
`src/shaders/motion.wgsl::pose` reads two vec4 records per shutter sample. GPU
track evaluation could replace these per-frame pose uploads, and reuse analytic
sub-frame times. It is **not free GPU work**: evaluating exponentials per pixel per
tap would multiply the cost drastically. Prefer a per-node/per-shutter compute
table (or vertex samples within varying limits), retaining the existing fragment
texture sampling and CPU speed/sample-count/culling decisions. A shutter crossing
a retarget requires the preceding segment too; a single latest-track record loses
that history. No motion-blur change is included in this spike.

## Sources

- [PLAN §6b/§6c](../PLAN.md): reference/fast path, GPU spring spike, idle guarantee.
- [Research 12 §2](12-research-rendering.md): vertex spring idea and CPU dependencies.
- [Research 17 §1](17-research-perf-architecture.md): property indirection and retained uploads.
- [Research 19](19-autoresearch-log.md): retained layout/virtualization/allocation wins preserved.
- [Chromium compositor design](https://chromium.googlesource.com/chromium/src/+/main/docs/how_cc_works.md): property-tree motivation (linked by research 17; no Chromium speed claim).
- [WGSL floating-point accuracy](https://www.w3.org/TR/WGSL/#floating-point-accuracy): fusion/reassociation and implementation-dependent transcendental accuracy.
- `crates/rune-motion/src/{spring,layer,animated}.rs`: CPU formula, damping branches and settle envelopes.
- `crates/rune-fx/src/shaders/motion.wgsl`, `crates/rune-fx/README.md`: current shutter pose table and idle/sample-count contract.

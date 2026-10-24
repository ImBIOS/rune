# 14. Determinism, verification and testing: cutting-edge ideas for Rune

Scope: tenet 7 (fast path + reference path, exact counters, instant checks) and §6b (same pixels everywhere,
bug reports are replays, every pixel traces to code). Written 2026-10-21.

**Provenance note.** Web search was down while writing. Items marked **[read]** I verified in source or docs this
session (vello checkout, WGSL spec, gungraun/FLIP/Kani/shuttle READMEs). Items marked **[recalled]** are from
memory of papers and tools and need a link check before anyone relies on a number.

## Summary

- The right architecture is already proven upstream: **Vello ships a `cpu_gpu_differential` cargo-fuzz target** that
  generates bounded structured scenes (shapes, strokes, gradients, clips, layers), renders with `vello_cpu` and
  `vello_gpu`, and compares RGBA8 with a per-channel tolerance (default 2, ≤4 outlier pixels)
  [read: `vello/fuzz/README.md`]. Rune's "reference renderer vs fast renderer" is this, with Rune's own scene model.
- **Bit-exact across GPUs is not available from WGSL/wgpu**: the spec lets an implementation reassociate, fuse
  (FMA), and use lower accuracy [read: WGSL §15.7.4–5]. So "same pixels everywhere" must be a *CPU-reference* lane
  (integer/fixed-point, exact), and GPU lanes are tolerance lanes. Don't chase GPU bit-exactness; chase a *bounded
  error with a proven budget*.
- A **u8/fixed-point CPU pipeline is the determinism anchor.** `vello_cpu` has a `u8_pipeline` (integer math) and an
  optional `f32_pipeline`, plus a `libm` feature so float functions don't depend on the platform libc [read:
  `vello_cpu/src/lib.rs`]. Integer math + `libm` + no FMA contraction gives identical bytes on x86 and arm64.
- **Perf gates by instruction count** (gungraun / iai-callgrind, CodSpeed) are exact and noise-free in virtualised CI
  [read: gungraun README], but Valgrind does not run on macOS arm64; run those on Linux (a Linux
  runner) and use Apple counters (`kpc`/Instruments) only for reserved-hardware confirmation.
- **Record/replay at the app level beats `rr`.** Rune owns the clock, input and randomness, so a deterministic
  event-log replay (KBs) gives most of rr/Pernosco's value on macOS/iOS, where rr does not run [read: rr README lists
  Linux only; Apple Silicon unsupported].
- **Concurrency**: the render thread/main thread handoff is small enough for **shuttle** (randomized, replayable
  schedules) and **loom** (exhaustive, tiny models) [read: shuttle README]. Cheap, high value.
- **Verification**: Kani (bit-precise model checking, strong for `unsafe` and overflow in fast-path kernels) is the
  practical formal tool; Verus only for 1–2 core invariants. Agents are good at writing Kani harnesses and
  specifications, which was the bottleneck for humans.

## Findings

### 1. Differential fuzzing of fast path vs reference (adopt now)
- Source: Vello's [`fuzz/` target](https://github.com/linebender/vello/tree/main/fuzz) [read]; cargo-fuzz +
  `arbitrary` for structured inputs; Vello also keeps `fuzz_regression.rs` so every crash becomes a snapshot test
  (tolerances recorded with the case) [read].
- Achieves: finds backend divergences automatically; the README notes known-different features (image quality
  Medium/High) are excluded so they don't drown new findings. That is the workflow lesson: **grow the generator
  feature by feature with a ratchet**.
- Rune: one `Scene` generator (`arbitrary`) feeding *all* subsystems: layout (fast SoA Taffy-like vs a naive
  recursive reference), springs (analytic closed form vs fine-step RK4 integration at 1e-6 dt), text (cached shaping
  vs fresh), renderer (GPU vs CPU reference). Tolerance modelled as (max channel diff, max outlier count), exactly
  Vello's. Every fuzz crash is auto-minimised (`tmin`) and committed as a named regression scene.
- Cost: low; generator maintenance. Risk: tolerance creep. Mitigation: tolerance is per-primitive and recorded;
  CPU fast path vs CPU reference is **zero tolerance**.
- Verdict: **adopt now.** Spike S: copy Vello's harness shape in the first renderer milestone.

### 2. A zero-tolerance exact lane built on integer/fixed-point CPU rendering (adopt now, scoped)
- Source: `vello_cpu` u8 pipeline + `libm` feature [read]; `tiny-skia` (Skia subset, SIMD with scalar fallbacks)
  [read README]; Skia's own GM tests live on a CPU "raster" backend as the baseline for GPU backends (Skia Gold
  compares across OS/arch/backend dimensions, >500k images per commit) [read:
  [docs](https://docs.skia.org/docs/dev/testing/skiagold/)].
- Achieves: bytes identical on any machine, so goldens are hashes, not images with fuzz.
- Rune: canonical goldens come from the CPU reference renderer. Rules for exactness: no FMA contraction (`-C
  llvm-args=-fp-contract=off` or explicit `mul_add` ban via clippy `disallowed_methods`), no float `sum()` with
  platform-dependent order, `libm` for transcendental functions, fixed iteration order, no HashMap iteration in
  render paths, pinned font bytes. Add a CI job that runs the *same* binary tests on x86-64 Linux and arm64 macOS and
  compares hashes; a mismatch is a hard failure that names the first differing scene.
- Cost: the CPU path must implement every primitive (UberQuad, glyph sprites, gradients, blur). That is already in
  the plan (§4.3 differential renderer). Risk: reference renderer drifts from "what the GPU should do"; the
  differential fuzz in item 1 is the leash.
- Verdict: **adopt now**; this is the plan's "canonical lane".

### 3. GPU float divergence: bound it, don't forbid it (adopt now as policy)
- Source: WGSL spec §15.7.4/5: implementations may reassociate, fuse, and be more or less accurate than the
  reference formula [read]; NVIDIA FLIP's precision note lists FMA, sqrt precision, and loop order as the reasons
  five implementations of one metric never match bit-for-bit [read:
  [precision.md](https://github.com/NVlabs/flip/blob/main/misc/precision.md)].
- Rune: write shaders in a **conservative numeric style**: premultiplied u8 / unorm in, fixed-point coverage
  (analytic AA from integer edge functions where possible, e.g. 8.8 subpixel positions snapped on CPU), quantize
  positions to 1/64 px on CPU before upload (already in §6b), keep the GPU doing only blend/sample of values the CPU
  decided. The more decisions are made in integers before the GPU, the smaller the cross-vendor surface. Then measure
  the residual: render the scene corpus on M-series, an NVIDIA box, and software Vulkan (lavapipe) via wgpu, record
  max per-channel error per primitive, and make that table the tolerance budget.
- Metal on Apple is one vendor family, so for macOS/iOS exact GPU hashes on a pinned device are plausible; treat
  as a bonus lane, not the contract.
- Verdict: **adopt now** (policy + a one-off measurement spike, 1 day).

### 4. Perceptual diff: FLIP plus a "does it matter" gate (spike)
- Source: [NVIDIA FLIP](https://github.com/NVlabs/flip) (LDR/HDR-FLIP, v1.7, in Ray Tracing Gems II; Rust port exists
  in the wild [recalled]); Skia Gold's triage model (positive/negative digests, baselines outside git) [read].
- Achieves: error maps that weight differences by human sensitivity; pooled score per image.
- Rune: for the *tolerance lanes* only (GPU vs CPU reference, cross-vendor), fail on `max(FLIP) > t` **or** a
  count of outlier pixels, whichever is stricter; exact lane stays hash-equal. Output FLIP heatmaps next to
  expected/actual/diff so an agent (or Kit) sees where. Don't use SSIM as the gate (blind to smooth gradient banding,
  which Rune cares about: dithering).
- Gold-style: store baselines as content hashes outside git (or in a `goldens/` manifest of hashes + a blob store),
  triage by "blame" commit. Overkill for a solo repo now; useful once multi-platform baselines multiply.
- Verdict: **spike** FLIP on the tolerance lane; **later** a Gold-like service.

### 5. Metamorphic testing for layout and rendering (adopt now; cheap, finds real bugs)
- Source: metamorphic testing literature applied to GUIs/renderers (e.g. graphics-shader compiler testing with
  semantics-preserving transforms in GraphicsFuzz/glsl-reduce, Donaldson et al. [recalled]; browser-layout
  differential/metamorphic work such as "Hidden costs of browser layout" style studies [recalled]). Treat as
  technique, not a specific citation.
- Relations Rune can assert without any oracle:
  1. **Translation invariance**: shift a subtree by integer px → output identical, shifted (exact on CPU lane).
  2. **Wrap invariance**: wrapping a node in a zero-style container or a no-op opacity(1) layer does not change
     pixels (catches layer/clip bugs; should be bit-equal).
  3. **Order independence** of non-overlapping siblings.
  4. **Resize monotonicity**: widening a container never increases text line count; height never shrinks when text
     scale grows (catches layout/shaping cache bugs).
  5. **Scale equivariance**: 2x scale-factor render, downsampled, is within tolerance of 1x render for vector content.
  6. **Retained vs immediate**: building the tree fresh vs mutating from a prior state gives identical output
     (catches stale-cache bugs: the classic retained-UI bug class). This is the highest-value relation.
  7. **Time-split invariance for springs**: stepping 1×16.6 ms == 2×8.3 ms == 1×(seek 16.6 ms) within 1e-9 (analytic
     springs make this *exact up to rounding*; a rare property few frameworks can claim).
  8. **Interrupt composition**: retarget at t, then evaluate: position and velocity continuous at t.
- Cost: tiny; each is ~30 lines over the scene generator. Verdict: **adopt now.**

### 6. Property-based and model-based UI testing with stateful shrinking (adopt now)
- Source: proptest state-machine testing / `proptest-state-machine` and quickcheck-style model tests; Hypothesis
  stateful testing [recalled]; Elm/Redux reducers as the model [recalled].
- Rune: model = a trivial `Vec<Item>` / form state; SUT = the real tree + reconciler + keyed diff + enter/exit.
  Generated operations = insert/remove/move/reorder, input events with timestamps, resize, time advance. Properties:
  identity preserved across moves (same node id → same spring state), no node unmounted at opacity > 0, a11y tree
  equals view tree, scroll offset clamped, no panics. Shrinking yields a *minimal event script* that is directly a
  replay file (item 8). Agents love this: the failing case is a 6-line JSON.
- Verdict: **adopt now.** Pair with the same generator as item 1.

### 7. GUI fuzzing and agentic exploration (later / spike)
- Source: Android GUI testing research (Monkey, Sapienz, Stoat, APE) [recalled]; LLM-driven GUI exploration papers
  2023–2025 (e.g. GPTDroid-style, VisionDroid-style) [recalled; unverified numbers]; Skia's fuzzing doc [link in
  Skia docs sidebar, not read].
- Rune advantage: no need for pixel/OCR-based exploration. The inspector exposes the semantic tree (roles, rects,
  actions), so **coverage-guided event fuzzing over the a11y tree** is straightforward: choose among actionable
  nodes, inject scripted input, coverage = distinct (node-role, state-hash) pairs plus code coverage (`-C
  instrument-coverage`). Run the fuzzer with a fake clock so it's thousands of interactions per second headless.
- Oracles: no panic, no layout or motion linter finding, no hitch beyond budget in the counter model, tree
  invariants. The internal motion linter as a *fuzz oracle* is a genuine differentiator (finds pops under random interrupt
  sequences).
- Verdict: **spike** after the scene/inspector exist; the LLM-guided variant is **later**.

### 8. Record/replay: app-level event log, with rr only as a Linux escape hatch (adopt now)
- Source: [rr](https://github.com/rr-debugger/rr) (Linux; README lists kernel ≥4.7, and some AArch64 parts) [read],
  [Pernosco](https://pernos.co) (omniscient debugging on rr traces) [recalled], Replay.io (browser/Node
  record-replay with app-level instrumentation) [recalled], Elm/Redux time-travel [recalled].
- Why not rr: it records syscalls and hardware events on Linux; Metal/CoreAnimation/AppKit are out of reach on macOS.
- Rune: inputs + clock + RNG + window metrics + font/asset manifest + environment hash are the *only* nondeterminism
  inputs (tenet: fixed clock, seeded randomness). Log them as a compact binary/JSONL stream; replay through the
  headless CPU renderer to identical pixels (the §6b "bug reports are replays" guarantee). Add:
  - **Periodic keyframes**: snapshot of state (arena dump) every N frames so replay can *seek* in O(N frames) and
    support reverse-step by replaying from the previous keyframe (cheap time-travel debugging, the Elm trick).
  - **State-hash per frame** in the log; on replay, first mismatching frame hash localises nondeterminism instantly
    (this is how deterministic game engines and lockstep netcode find desyncs [recalled]).
  - A `--chaos` mode that perturbs scheduling (shuttle, item 10) and asserts frame hashes unchanged.
- On Linux CI only, run failing replays under `rr record` and attach the trace for Pernosco if a crash is native.
- Verdict: **adopt now** (log + state-hash + keyframes); rr/Pernosco **skip** for macOS, **later** for Linux crashes.

### 9. Exact performance counters in CI (adopt now, with a platform split)
- Source: [gungraun](https://github.com/gungraun/gungraun) (successor of iai-callgrind; instruction counts,
  estimated cycles, DHAT, one-shot runs, usable in noisy virtualised CI; v0.20 on crates.io) [read];
  [iai](https://github.com/bheisler/iai) the cachegrind original [read]; CodSpeed (CPU-simulation instrument
  based on Valgrind, plus walltime on dedicated macro runners) and Bencher (tracks metrics incl. iai instruction
  counts with statistical thresholds) [recalled; docs pages 404'd on fetch, verify].
- Rune: bench the *pure* functions (layout of a fixed scene, spring step for N springs, glyph run encode, CPU
  rasterise a tile) under gungraun on Linux: instruction count, L1/LL misses, and **DHAT heap allocations** (this is
  the "zero allocation per frame" gate, stronger than a counting allocator because it also reports bytes and sites).
  Gate: >0.5% instruction regression fails the PR; the failing report includes callgrind diff by function so an
  agent can fix it. Wall-clock on reserved hardware only as confirmation, because instruction count ≠ time on Apple
  silicon.
- Caveat: Valgrind is not supported on macOS arm64, and instruction counts on x86-64 Linux don't predict Apple
  cache/NEON behaviour. Use for regression detection and algorithmic cost, not absolute 8.33 ms claims. For the
  latter: model budgets in counted units (instances, glyphs, nodes) which are platform independent (already planned).
- Verdict: **adopt now** (gungraun; pick CodSpeed/Bencher later only if a hosted trend UI is wanted).

### 10. Deterministic concurrency testing of the render-thread handoff (adopt now)
- Source: [shuttle](https://github.com/awslabs/shuttle) (randomized scheduler incl. PCT with probabilistic
  guarantees; replayable failing schedules; wrappers for tokio/rand) [read]; [loom](https://github.com/tokio-rs/loom)
  (exhaustive C11 memory-model exploration; small models only) [recalled]; Antithesis-style whole-system
  deterministic simulation [recalled, commercial, skip].
- Rune: the main↔render thread protocol (triple-buffered target/command queue, spring track updates, input timestamp
  handoff, frame-in-flight counting) should be a small isolated module whose sync primitives go through a `rune_sync`
  facade (`std::sync` in prod, `shuttle::sync`/`loom::sync` under `cfg`). Test: random schedules x thousands; the
  invariant is "presented frame sequence is a function of (input log, clock) only", which is the §6b replay
  guarantee under concurrency. Failing schedule is a seed that reproduces exactly.
- Cost: facade discipline (agents handle that well). Risk: loom state explosion; keep models to 2–3 threads.
- Verdict: **adopt now** for the handoff; the rest of the app is single-threaded by design.

### 11. Practical formal methods: Kani first, Miri always, Verus selectively (spike)
- Source: [Kani](https://github.com/model-checking/kani) (bit-precise model checker, UB detection for `unsafe`,
  function contracts, Linux/macOS) [read]; AWS/Rust Foundation [std-library verification
  effort](https://github.com/model-checking/verify-rust-std) with Kani, Verus-adjacent tools, VeriFast, Flux, etc.
  [read]; [Verus](https://github.com/verus-lang/verus) (SMT-checked Rust subset; Atmosphere/other large proofs)
  [read README; case studies recalled]; Miri for aliasing/UB in `unsafe` SIMD-adjacent code [recalled].
- Rune fits: small `unsafe` kernels (SoA arena index math, SIMD tile blend, arena free-list, fixed-point rounding)
  are exactly where Kani proves "no overflow/OOB for all inputs" in seconds and where an agent can write both the
  harness and the `#[kani::proof]`. Examples worth proving: `blend_u8(a,b)` equals the reference
  `(a*b+127)/255`-style formula for all 2^16 inputs (exhaustive, can also just be a brute-force test: **prefer
  brute-force for 8/16-bit domains**); arena generation counters never alias; 26.6 fixed-point snap is idempotent.
- Verus: only if a core invariant resists testing (e.g. keyed-diff reconciliation preserves identity). Heavy; mark
  **later**.
- Miri: run the whole unit suite under Miri nightly on the non-GPU crates; any `unsafe` merged without Miri green is
  rejected. Verdict: Miri **adopt now**, Kani **spike** (1 kernel), Verus **later**.

### 12. Pixel provenance / trace-to-source (adopt now, half of it is already in the plan)
- Source: general ideas: Chrome DevTools paint-flashing/"layer reasons", Flutter DevTools "creation location" of
  widgets (`debugCreator`/inspector `creationLocation`) [recalled], React `__source`.
- Rune: stamp each node with `file:line` at construction (`#[track_caller]` is free); renderer emits a compact
  **ID buffer** (u32 node id per pixel/tile) *only in debug/CI snapshot mode*, a second attachment on the CPU
  reference renderer (zero cost on the fast path). With it, `mk inspect --at x,y` returns the topmost node, the whole
  stack of contributors at that pixel (blend order), their `file:line`, and spring tokens. Diff reports then
  *blame by node*: "37 px differ, all belong to node #412 `pill.rs:88`, whose shadow radius changed from 12.0 → 12.5".
  That turns a screenshot diff into a semantic diff that an agent can act on without looking.
- Extension: **semantic diff first**: diff the (tree, layout rects, style hashes, spring states) before pixels;
  report "layout unchanged, only shadow colour changed" so pure-visual changes are separated from layout changes.
- Cost: modest on the CPU renderer; risk none to production. Verdict: **adopt now** (CPU ID buffer + semantic diff).

### 13. Frame-hash goldens with auto-explanation (adopt now)
- Rune: golden = BLAKE3 of the CPU frame plus the stored PNG only on failure (artifacts as expected/actual/diff/
  FLIP/ID-blame). `mk snap --bless` rewrites hashes; blessing prints which scenes changed and their semantic diff so
  an agent reviewing a PR sees intent. Per-scene sequence goldens (hash at t = 0, 0.1, … s) cover motion
  deterministically, because the clock is fixed; this *is* motion regression testing with no video.
- Verdict: **adopt now.**

## Steal
- Vello's `cpu_gpu_differential` fuzz target, tolerance model and regression-snapshot loop.
- `vello_cpu`'s dual u8/f32 pipeline idea (fast integer vs accurate float) and `libm` feature for portable floats.
- gungraun's one-shot instruction/DHAT gating; callgrind per-function diffs in the failure report.
- shuttle seeds as replayable schedules; Elm-style keyframed time travel; lockstep-game state-hash desync detection.
- Skia Gold's baselines-by-hash with blame-based triage (later).
- FLIP for tolerance lanes, with its explicit precision caveats.

## Avoid
- Promising GPU bit-exactness across vendors (WGSL forbids relying on it).
- SSIM/PSNR as the sole gate. Wall-clock benchmarks on shared CI. Full-system rr/Pernosco on macOS.
- Verus across the codebase; proof effort exceeds benefit outside tiny kernels.
- Fuzzing with tolerances so loose that they hide regressions; keep exact lanes exact, record every tolerance.

## Recommendation for Rune
1. Build CPU reference renderer as integer/`libm`, zero-FMA, hash-golden canonical lane; GPU lanes use measured,
   per-primitive tolerance budgets (items 2, 3).
2. One structured `Scene` generator drives differential fuzz, metamorphic relations, and stateful model tests (1, 5,
   6). Failures auto-minimise to replay scripts.
3. App-level replay with state hashes and keyframes (8); gungraun gates on Linux (9); shuttle for the handoff (10);
   Miri on every PR (11).
4. CPU ID-buffer and semantic diff for blame (12). FLIP and Kani as spikes.

Rejected: rr/Pernosco as the main replay story (platform), CodSpeed/Bencher as required infra (nice-to-have),
Verus broadly, GPU-exact hashing as the contract, Gold as a service (premature).

## Open questions
- Can arm64 macOS and x86 Linux give identical CPU-lane bytes with auto-vectorised code? Needs an actual two-machine
  hash test (the CPU lane may need explicitly-scalar or fixed-width `fearless_simd`-style kernels for exactness).
- Is the Apple GPU deterministic enough run-to-run (same device, same driver) for exact hashes as a bonus lane?
  Likely yes; unmeasured.
- Does text shaping (Parley/HarfRust) output remain bit-stable across versions and platforms with pinned fonts?
  Pin versions and add a shaping-hash lane.
- Instruction counts on Linux x86-64 vs Apple arm64: how well do they correlate for Rune's kernels? Calibrate once on
  a Mac with `kpc` counters.
- Exact figures for CodSpeed/Bencher/Replay.io/GUI-fuzzing papers need verification when search is back.

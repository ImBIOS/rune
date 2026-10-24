# 13 – Startup and input-to-photon latency tricks for Rune

Written from existing knowledge of the sources below; no web crawl this pass and nothing was built or benchmarked. Numbers are from the cited sources unless marked *(estimate)*. Verify each link before relying on it.

## Summary

- Instant first frame is mostly a **compositor-handoff problem**, not a rendering problem. The cheapest route is a `CALayer`/`IOSurface` (or Android splash) holding pre-rendered pixels that the OS shows before our code runs. The live tree then replaces it without a visible change.
- A pre-rendered frame is only invisible if the live first frame is **bit-identical**. Rune's deterministic headless renderer makes that testable, which GPUI/Flutter lack. This is the strongest unique advantage.
- Cold-start cost on macOS is dominated by dyld, page faults, and wgpu device/pipeline creation. Static linking, page-ordering and a pipeline cache attack all three.
- Input latency wins come from three places: **late latching** (sample input as late as possible), **predicted touches**, and **present-time control** (`CAMetalDisplayLink`, `presentAfterMinimumDuration`, DirectFlip, Wayland commit-timing).
- Perceptual thresholds justify the targets: touch-to-photon around 1 ms is perceptibly better for direct manipulation, and 100 ms is the floor for "instant" launch.
- Most of these ideas are impractical for humans to maintain (order files, generated pixel blobs per appearance and size, per-platform present loops). They are cheap if the build pipeline regenerates them.

## Findings

### 1. Static first-frame layer shown by the OS (iOS/macOS) — **adopt now**
- iOS: launch screen storyboard, plus the app-switcher snapshot taken by the system (`applicationDidEnterBackground` snapshot, state restoration). Apple's guidance is that the launch image must match the first screen ([HIG: Launching](https://developer.apple.com/design/human-interface-guidelines/launching)).
- Rune use: the build generates a launch storyboard-equivalent or, more flexibly, a `CAMetalLayer`/`CALayer` whose `contents` is an `IOSurface` or `CGImage` embedded in the binary (decoded lazily from a compressed blob). It is installed in `applicationDidFinishLaunching` before any wgpu work. The Metal layer later takes `contents` over, with the static layer removed in the same CA transaction as the first live present.
- On macOS, show a borderless window with that layer before the runtime initialises. Set window background to match, which avoids a white flash.
- Variants: per appearance (light/dark), per scale factor, per common window size. Generate by running the headless renderer in CI and storing as AVIF/zstd'd BC7 or raw 8888 for tiny sizes. A 1280×800 8888 frame is about 4 MB *(estimate)*, so prefer zstd/lz4. Decode cost is roughly 2–5 ms at that size *(estimate)*.
- Risk: dynamic content (user data, restored window sizes). Mitigate by snapshotting the **last session's** frame at quit (see 2) and falling back to the build-time frame.

### 2. Last-session snapshot, as in iOS app-switcher snapshots — **adopt now**
- Same mechanism as 1 but the pixels come from the final frame of the previous run (written at quit/background, keyed by appearance + size + scale). This is what iOS does for backgrounded apps and what browsers do with bfcache/tab thumbnails.
- Rune use: store `IOSurface` bytes in the app cache dir. Shown first on relaunch. Falls back to the build-time frame on mismatch (version hash changed).
- Risk: showing stale data. Require a "snapshot-safe" flag per view, with sensitive views excluded.

### 3. Chrome paint holding / View Transitions-style handoff — **adopt (principle)**
- [Chrome paint holding](https://developer.chrome.com/blog/paint-holding) keeps the previous page painted during same-origin navigation until the new page's first contentful paint, avoiding white flashes. The same principle applies: **never present until the live frame is ready; keep the old pixels up until then.**
- Rune use: handoff rule is "live swap occurs on the first live frame that passed the visual-equality gate or is ready". A transition (e.g. 80 ms cross-fade, springs-based) is used only when the golden diff is non-zero.
- Cost: low. It is a presentation policy in the shell.

### 4. Bit-identical handoff gate via deterministic rendering — **adopt now (Rune-specific)**
- The plan's guarantee ("golden diff = 0") needs the live GPU frame to equal the pre-render. Different GPUs and drivers produce different antialiasing/gamma/subpixel text; a CPU-rendered snapshot will differ from a GPU render by a few LSBs.
- Options: (a) render the snapshot with the same wgpu path on a reference GPU, accepting a small tolerance (e.g. ≤ 1/255 per channel) and fading over ~1 frame; (b) have the **first live frame** be the snapshot itself (a textured quad) and then re-render live, fading differences below the perceptual threshold; (c) snapshot per GPU family at first run (see 8).
- Ship (a)+(c). Measure max delta per device class in CI.

### 5. Android SplashScreen API + Baseline Profiles — **later (Android is last), but the idea transfers**
- [SplashScreen API](https://developer.android.com/develop/ui/views/launch/splash-screen) and [Baseline Profiles](https://developer.android.com/topic/performance/baselineprofiles/overview): Google reports around 30% faster cold start from AOT-compiling hot code paths (varies per app).
- Transfer: the profile-guided idea is generic. Collect page-fault and call-order traces from a headless launch, and feed them to binary ordering (item 7). The splash handoff is the same as items 1–3.

### 6. Process snapshotting: V8/Node startup snapshots, Emacs portable dumper, CRIU/CRaC — **spike (partial) / skip (full)**
- [Node startup snapshot](https://nodejs.org/api/cli.html#--build-snapshot) and V8 snapshots deserialize the heap in milliseconds instead of re-running init. [Emacs pdumper](https://www.gnu.org/software/emacs/manual/html_node/elisp/Dumping.html) does the same for Lisp state: startup drops from seconds to tens of ms. [CRaC](https://openjdk.org/projects/crac/) and CRIU restore a process after warmup (JVM startup from seconds to roughly 40–100 ms in the published demos).
- Rune use, practical subset: do not snapshot the process (GPU contexts and window system handles cannot be restored). Instead **snapshot the pure-CPU state**: initial tree, resolved layout, shaped text runs (Parley glyph clusters + positions), glyph atlas contents, and font tables, serialized in a zero-copy format (`rkyv`-style, mmap'd). Loading is `mmap` plus pointer fix-up (or none with relative pointers), so no parse or shape work at startup.
- Cost: format versioning. Agents can regenerate on every build. Risk is stale caches; key by content hash of the binary + fonts.
- Full-process CRIU on macOS is not available; skip.

### 7. Binary layout: order files, PGO, BOLT, static linking, dyld costs — **adopt now (cheap), measure first**
- macOS dyld: each dylib adds mapping, rebinding and initializer cost. dyld 4 plus [chained fixups](https://developer.apple.com/videos/play/wwdc2022/110362/) reduce it. A single statically linked Rust binary with system frameworks only is near the floor (about 10–15 ms for a hello-world process on Apple silicon *(estimate)*).
- Page-in dominates cold launch: first execution of code pulls pages from disk (each 16 KB page fault is about tens of µs warm and much more cold). Linker `-order_file` (ld64/lld) places startup-touched functions contiguously. Apple documents order files and [Xcode's "Generate Link Map"/order file profiling](https://developer.apple.com/documentation/xcode/improving-your-app-s-performance). Meta/Google report double-digit % launch improvement from function ordering ([LLVM Propeller](https://research.google/pubs/propeller-a-profile-guided-relinking-optimizer-for-warehouse-scale-applications/), [BOLT](https://github.com/llvm/llvm-project/tree/main/bolt); BOLT is ELF-focused, so use Propeller/order-files on Linux and `-order_file` on Mach-O).
- Rune use: CI runs the headless startup under a tracer (e.g. `-finstrument`-style or sampling plus page-fault logging), emits an order file, and release links with it. Also: `opt-level=3` + `lto="fat"` + `codegen-units=1`, `panic=abort`, no `std::fmt` heavy init in startup, avoid `lazy_static` fan-out; `mach_absolute_time` stamps at `main`, post-window, post-device, first-present. Hot/cold splitting for the startup path.
- Cost: low-medium; an agent can maintain the pipeline. Gain *(estimate)*: 10–30% of cold launch before first frame.

### 8. Pipeline/shader prewarming and caches — **adopt now**
- Metal pipeline creation (MSL compile + PSO) costs 5–50 ms each cold; [Metal binary archives](https://developer.apple.com/documentation/metal/mtlbinaryarchive) store compiled GPU binaries to avoid it. WebGPU/wgpu: pipeline caches exist in recent wgpu (`PipelineCache` on Vulkan; use naga → MSL ahead of time for Metal).
- Rune use: precompile all shaders (naga → MSL → `metallib`) at build time and load the library from the bundle; build a binary archive per GPU family on first run (and ship one for common Apple GPU families in the bundle). Bound the number of pipelines (small fixed set of uber-shaders plus specialization constants) so prewarm is a handful of calls, which can be issued on a background thread while the static frame is already on screen.
- Also prewarm the glyph atlas for the first frame (from item 6's cached shaped runs).

### 9. Lazy / parallel init: critical path = pixels — **adopt now**
- Order: (1) main thread: window + static layer (≤ 20 ms); (2) thread A: wgpu instance/adapter/device; (3) thread B: font load via mmap, Parley shaping from cache; (4) thread C: tree build from snapshot; then render. Everything else (a11y tree, IME, network, hot reload server) is after first present.
- Known costs to measure: `wgpu` adapter enumeration (~5–30 ms on macOS *(estimate)*), `CAMetalLayer.nextDrawable` first call, font fallback enumeration with CoreText (can be 50+ ms; avoid system font enumeration at startup).
- Flutter comparison: [Flutter AOT](https://docs.flutter.dev/resources/faq#what-technology-is-flutter-built-with) removes JIT cost, and [Impeller](https://docs.flutter.dev/perf/impeller) precompiles shaders because Skia's runtime shader compilation caused first-run jank. Rune's analogue is already planned (item 8).

### 10. Late-latching input before present (VR technique) — **adopt now**
- VR runtimes sample head pose as late as possible, after CPU frame construction and just before GPU submit ([Oculus late latching](https://developer.oculus.com/blog/optimizing-vr-graphics-with-late-latching/), reported roughly one frame of latency saved on mobile). 
- Rune use: the render thread (analytic springs, GPU-driven) reads the **latest pointer position/drag velocity** from a lock-free atomic slot immediately before encoding the final uniform buffer. Because springs are analytic and evaluated at a presentation timestamp, drag-following transforms can be written into a small "late" uniform region that is patched after the main command buffer is built (a ring-buffer slot written right before `commit`). Layout and text are not re-done; only transforms of the dragged node are late-latched.
- Cost: moderate; requires a "late uniform" concept for dragged transforms. Gain: up to 1 frame (8.3 ms at 120 Hz).

### 11. Predicted touches and pointer events — **adopt (iOS first), spike model choice**
- iOS: [`UIEvent.predictedTouches(for:)`](https://developer.apple.com/documentation/uikit/uievent/1613844-predictedtouches), and coalesced touches for the full-rate samples. Apple's own documentation recommends drawing predicted points then discarding them next frame. Chrome: [`getPredictedEvents()`](https://developer.mozilla.org/en-US/docs/Web/API/PointerEvent/getPredictedEvents) and the Chromium input predictor (kalman, least squares, linear/quadratic variants, [Chromium input prediction design](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/ui/base/prediction/)), which found Kalman filter preferable for scrolling and least-squares for drawing.
- Rune use: for direct-manipulation gestures, use `coalescedTouches` at 240 Hz for the real sample path (smooth velocity for spring handoff), and predict ~1 frame ahead using a constant-velocity or damped-acceleration Kalman model, rendering the predicted position but never committing it to state. On release, hand over the **measured** (not predicted) velocity to the spring. On macOS trackpad/mouse, use prediction only for drag.
- Risk: overshoot artifacts at direction changes; clamp prediction horizon by confidence (cap at 1 vs. 2 frames) and check with recorded real traces replayed deterministically (a good agent-verification fit: record input traces, replay headless, measure prediction error).

### 12. Present timing: CAMetalDisplayLink, minimum-duration, DirectFlip, Wayland — **adopt on Apple, spike elsewhere**
- Apple: [`CAMetalDisplayLink`](https://developer.apple.com/documentation/quartzcore/cametaldisplaylink) (iOS 17 / macOS 14) fires with a target present timestamp and a deadline for drawable submission, so a render thread can wake just-in-time rather than at vsync after a fixed offset. Use `drawable.present(afterMinimumDuration:)`/`present(at:)` to hold frame cadence, and `maximumDrawableCount = 2` for lowest latency on macOS (CAMetalLayer property). Set `displaySyncEnabled`, and `wantsExtendedDynamicRangeContent` off unless needed. Use `presentedTime` / `MTLDrawable.addPresentedHandler` for measured latency.
- Windows: DXGI flip model with [independent flip / DirectFlip](https://devblogs.microsoft.com/directx/dxgi-flip-model/) skips DWM composition when the swap chain covers the window (requires `DXGI_SWAP_EFFECT_FLIP_DISCARD`, a waitable object via `SetMaximumFrameLatency(1)` and `GetFrameLatencyWaitableObject`), cutting about a frame of latency in published tests. 
- Linux/Wayland: [`wp_presentation`](https://wayland.app/protocols/presentation-time) feedback gives real present timestamps; [`wp_commit_timing_v1`](https://wayland.app/protocols/commit-timing-v1) (staging, 2024–25) lets clients target a presentation time; [`wp_fifo_v1`](https://wayland.app/protocols/fifo-v1) gives proper vsync-paced commits. Use for pacing when supported.
- Rune use: a `PresentClock` trait per shell that exposes `next_present_deadline()` and `actual_present_time(frame)`. The render thread schedules: input latch → spring eval at *target present time* → encode → submit with slack about 1–2 ms before deadline. This unifies with the analytic springs (evaluated at the exact predicted display time, which removes judder).
- Risk: per-platform code and wgpu limits (wgpu hides presentation timing; may need to bypass wgpu surface presentation on Apple via `CAMetalLayer` directly, which the plan seems to allow). Verify `wgpu` exposes a frame-time hint, else raw `CAMetalLayer`.

### 13. Frame pacing research — **adopt (principles)**
- [Android Frame Pacing / Swappy](https://developer.android.com/games/sdk/frame-pacing) and Apple's own docs agree: pace on the display timeline, not on a timer; drop to a stable lower rate instead of oscillating; and measure present-to-present deltas, not frame times. Related research on input-to-photon (e.g., [Nvidia Reflex](https://www.nvidia.com/en-us/geforce/technologies/reflex/) cuts system latency by pacing CPU submission so the queue stays empty) is the same just-in-time idea as item 12.
- Rune use: keep render queue depth at 1; the render thread **sleeps until deadline minus a measured encode+GPU time budget (EWMA with a p99 margin)**, then latches. Trace it per frame in the frame traces the plan already wants.

### 14. Perceptual latency thresholds — **use as targets**
- Ng, Lepinski, Wigdor, Sanders, Dietz, [*Designing for Low-Latency Direct-Touch Input* (UIST 2012)](https://dl.acm.org/doi/10.1145/2380116.2380174): people notice improvement down to about 1 ms in direct touch dragging; 10 ms is the commonly noticed "feels connected" edge; and the follow-up [*How Fast is Fast Enough?* (CHI 2013, Jota et al.)](https://dl.acm.org/doi/10.1145/2470654.2481317) found dragging at 1 ms vs 10 ms vs 50 ms are differentiable. Microsoft Research's demo of 1 ms touch is the source of the "1 ms touch" claim.
- Rune use: budget for drag-follow: iOS hardware floor is about 2 frames at 120 Hz (≈16 ms) with prediction offsetting about 1 frame; set CI gates on measured touch-to-present (via high-speed camera in the lab later; via `presentedTime` minus event timestamp in software now). Launch: ≤ 100 ms is "instant"; the plan's 50 ms is well inside it.

### 15. Immediate draw on input for small updates — **spike**
- Draw feedback (press state, caret, drag handle) directly in the render thread without waiting for the main/UI thread to run layout/tree diff: input thread → render thread message → patch a retained "overlay" layer. This is what Core Animation's render server gives SwiftUI for free (`CAAnimation` runs out of process).
- Rune use: the render thread owns an "interaction layer" of transform/opacity/color overrides keyed by node id, updated by input events directly with analytic springs, so a press response never waits on the app thread. The app tree later confirms. Risk: two sources of truth; mitigate by making overrides explicit, ephemeral and traced.

## Steal
- iOS snapshot + paint-holding handoff policy (1–3).
- Impeller's "no runtime shader compilation" rule (8).
- Late latching and Reflex-style just-in-time submission (10, 13).
- Chromium's predictor families and its habit of testing predictors on recorded traces (11).
- Emacs/Node idea of loading precomputed state instead of computing it, restricted to pure data (6).
- Order files / Propeller for page-in (7).

## Avoid
- Full process checkpointing (CRIU/CRaC) — not portable to macOS/iOS and GPU state does not survive.
- Showing a stale snapshot for user-private or fast-changing views without a flag.
- Predicting beyond ~1 frame or committing predicted state to model data.
- Relying on BOLT for Mach-O (ELF-centric); use `-order_file`.
- Per-frame heap allocation or lock contention in the late-latch path.

## Recommendation for Rune (ranked)
1. **Static pixel layer + bit-identical handoff gate** (items 1–4). Make it the first milestone shell feature; CI asserts max-delta and launch timeline.
2. **Precompiled shaders + Metal binary archive + parallel init critical path** (8, 9). Biggest controllable chunk of the 50 ms budget.
3. **Frame clock built on `CAMetalDisplayLink` with just-in-time latch and analytic springs evaluated at the target present time** (12, 13, 10).
4. **Coalesced+predicted touch pipeline with recorded-trace replay tests** (11).
5. **Order-file/PGO startup layout pipeline and cached shaped-text/atlas snapshot** (6, 7).

Rejected: process snapshot (6, full); runtime shader JIT; fixed-offset vsync timers.

## Open questions
- Does wgpu expose enough presentation control on Metal (target timestamps, drawable count, `presentedTime`), or must the Apple shell own `CAMetalLayer` directly?
- Achievable max pixel delta between the CPU/headless snapshot and live GPU frame on each Apple GPU family; is tolerance + 1-frame fade enough to be invisible?
- Real wgpu device init and first-`nextDrawable` costs on M-series cold boot (measure on a reserved Mac, not a developer's desk display).
- Size and decode cost of embedded snapshots across appearance × scale × window-size variants; do we store raw, lz4, or BC7?
- iOS: can a launch-screen replacement be a CALayer image set before `didFinishLaunching`, or must it be a storyboard/`UILaunchScreen` image? (The latter requires a static asset, so the build must emit a PNG into the asset catalog.)
- Which predictor (Kalman vs least-squares) performs best on Rune's own recorded drag traces?

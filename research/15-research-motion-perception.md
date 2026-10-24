# 15 · Motion, perception and feel: research and tricks Rune can exploit

Scope: smoothness/judder perception, frame-time variance, motion blur at high refresh, direct-manipulation latency,
spring vs easing, interruptibility, Apple/Google fluid-interface research, crisp moving text, layout animation, velocity
estimation, scroll physics, and automatic smoothness metrics. Written 2026-10-21 for Rune ([PLAN §4.9, tenet 7, §6b](../PLAN.md)).

**Source quality legend.** *[read]* = I opened the source/paper page in this session. *[recall]* = from memory of the
talk/paper/source; cite-check before it goes into a spec. Numbers I could not verify are marked *(est.)*.
Rune's differentiators used throughout: analytic springs (exact x and v at any time), a render thread that owns every
presented value, a deterministic clock, and agents who can maintain fast-path + reference-path pairs.

## Summary

1. **Sample at the presentation time, not at "now".** Frame-time *variance* matters more than average fps, and with
   analytic tracks the cure is free: evaluate every track at the display's *target presentation timestamp*. A late
   frame then shows the right place instead of a velocity hiccup. (NVIDIA/UCSB: 12 ms of frame-time variation
   measurably degrades perceived smoothness, 4 ms barely does. [read])
2. **Smoothness metric = pixel error, not frame count.** Judder visibility is `velocity × timing error` in px/arcmin.
   Rune can compute, per frame and per node, `|rendered_pos − ideal_pos(t_present)|` exactly, plus Apple's hitch-time
   ratio. No other toolkit can score smoothness this precisely, because none owns both the ideal and the presented value.
3. **Late-latch input on the render thread.** Direct-manipulation latency down to ~1–10 ms is perceptible in dragging
   (Ng/Jota, Microsoft Research [recall]). Sample the pointer at the last moment before encode, resample to the
   presentation time like Android does, and extrapolate ≤ ~8 ms.
4. **Velocity: ship Android's impulse estimator** plus an LSQ2 reference, with a 100 ms horizon and a 40 ms "stopped"
   rule. Differential-test both against a recorded gesture corpus, and tune the choice by post-release prediction error.
5. **Text under motion is a policy, not a flag**: vertical snap + ¼-px horizontal phases in grayscale while moving,
   exact analytic settle time, then a sub-¼-px rest snap. Verify with a phase-sweep harness an agent can run.
6. **Layout animation by rect-space FLIP on presented rects**, never scale correction (Rune's edge tracks already avoid
   it), with *glyph/word-level FLIP* for text reflow as a distinctive spike.
7. **Effects springs ≠ spatial springs** (Material 3 Expressive): opacity/colour/blur channels never overshoot.
8. **Scroll/fling are analytic**: `x(t)` closed form (iOS 0.998/ms, Android spline) so they are frame-rate independent
   and retargetable; project the landing point and hand velocity to a spring for snapping.
9. **Synthetic motion blur: skip at 120 Hz**; keep a velocity-gated spike for flings, because Rune knows `v` exactly.
10. **Perception ground truth is thin for springs vs easing**; use Kit's blind A/B chooser (Bradley–Terry) as the oracle
    and build it into `mk tune`.

---

## Findings

### 1. Present-time sampling and frame pacing  — **adopt now**
- **Source:** NVIDIA Research, Klein/Spjut/Boudaoud/Kim, "Variable Frame Timing Affects Perception of Smoothness in
  First-Person Gaming," IEEE CoG 2024 ([page](https://research.nvidia.com/publication/2024-08_variable-frame-timing-affects-perception-smoothness-first-person-gaming),
  [arXiv](https://arxiv.org/abs/2306.01691)) [read]. Android FrameTimeline classifies jank by *predicted vs actual
  present time*, and flags BufferStuffing as a latency state even when cadence is smooth ([Perfetto](https://perfetto.dev/docs/data-sources/frametimeline)) [read].
- **What it shows:** frame-time variation of ±4–12 ms around 30–240 Hz; 12 ms degrades perceived smoothness, 4 ms has a
  diminished effect. Average fps alone doesn't predict it. A 120 Hz period is 8.33 ms, so *one* skipped vsync is already
  in the "visibly bad" regime. Practical corollary: at 120 Hz a frame that misses by half a period matters, at 60 Hz
  the same absolute miss is a smaller fraction.
- **Rune use:** on Apple use `CAMetalDisplayLink`/`CADisplayLink` `targetTimestamp` / `targetPresentationTimestamp`
  as the sample time `t_present` for all tracks; on other platforms use the compositor's predicted present time
  (Android Choreographer vsync id/`AChoreographer` frame time, Wayland presentation-time, DXGI `GetFrameStatistics`,
  WebGPU: `rAF` timestamp + one period). Never `Instant::now()`. A hitch becomes a one-frame *temporal* gap with
  correct geometry instead of a speed discontinuity. Use `wgpu` `desired_maximum_frame_latency = 1` (2 = stuffing).
- **Cost/risk:** low; one-time plumbing per platform. Risk: platforms that don't expose a prediction (web). Fallback: previous frame timestamp + period.
- **Verdict:** adopt now. It is the single highest-leverage item: all other metrics presuppose it.

### 2. Metrics a frame trace can compute that track perceived smoothness  — **adopt now**
Rune records, per frame: `t_target`, `t_actual_present` (where the OS reports it), `t_sample`, and per animated node
`x_rendered`, `x_ideal(t_actual)` (exact from the track), `v`. From that (all computable headlessly with the fake clock
injecting synthetic delays):

| Metric | Definition | Why it correlates |
|---|---|---|
| **Hitch time ratio** | Σ(excess present-interval over expected) ÷ animation duration, ms/s | Apple's own: WWDC20 "Eliminate animation hitches with XCTest" and WWDC21 "Detect and diagnose hitches" use thresholds ≈ < 5 ms/s good, 5–10 warn, > 10 critical [recall]. Use as the CI gate shape. |
| **Cadence deviation** | σ and max of `present_interval − period`, in periods | NVIDIA result above: variance, not mean. |
| **Position error px** | `|x_rendered − x_ideal(t_actual)|` per node per frame; report p99 and max | Judder visibility ≈ `v · Δt` in px; threshold in arcmin using the device's ppi and viewing distance (iPhone ≈ 460 ppi @ 35 cm ⇒ 1 px ≈ 0.55 arcmin, below acuity, but motion hyperacuity is finer). |
| **Visual jerk** | 3rd difference of presented position over *presented* frames, normalised by peak speed | Catches velocity snaps that hit-time metrics miss (e.g., retarget bugs). The motion linter's "velocity snap" rule, made exact. |
| **Velocity continuity at retarget** | `|v_after − v_before|` at every segment boundary (should be 0 up to float eps) | Exact in analytic tracks; interruption bugs show instantly. |
| **Input-to-photon (virtual)** | `t_present − t_input_hw` per touch-driven frame | Latency budget; reported per frame; trend on reserved hardware. |
| **Scroll Jank v4-style** | Chrome's per-frame scroll jank counting across input/frames ([blog, 2023](https://blog.google/chromium/smoother-scrolling-how-we-halved-scroll-jank-in-chrome-on-android/); [Chromium post](https://blog.chromium.org/2023/08/smoothing-out-scrolling-experience-in.html)) | Halved scroll jank by ensuring consistent input-to-frame delivery [read headline]. Copy the *definition*, not the code. |

- Android **JankStats / FrameMetrics / FrameTimeline** give the vocabulary ("deadline missed", "buffer stuffing",
  "prediction error") that Rune's hitch attribution should reuse: charge each hitch to a phase (view, layout, shape,
  encode, GPU) as PLAN §6b already says.
- **Agent trick:** since the clock is fake, run every scene through a *jitter fuzzer* (inject present-interval noise of
  0/4/12 ms σ, drop 1 in N frames) and assert that position-error stays ≤ ε. This proves "time-correct sampling" holds
  for every animated property, which nobody can claim for hand-written animation systems.
- **Verdict:** adopt now; it is mostly arithmetic over the trace Rune already plans to emit.

### 3. Direct-manipulation latency, late latching, resampling, prediction  — **adopt now (late latch + resample), spike (prediction)**
- **Sources:** Ng, Lepinski, Wigdor, Sanders, Dietz, "Designing for Low-Latency Direct-Touch Input," UIST 2012;
  Jota, Ng, Dietz, Wigdor, "How Fast is Fast Enough?" CHI 2013 ([PDF](https://www.tactuallabs.com/papers/howFastIsFastEnoughCHI13.pdf)) [recall; the PDF did not extract here].
  Headline as I remember it: drag-tracking differences are perceptible at single-digit ms (≈ 1 ms vs 10 ms users could
  tell), tapping tolerates more (tens of ms), and performance degrades measurably by ≈ 10–25 ms. *Verify exact numbers before quoting.*
- **Android input resampling** (read in AOSP `InputConsumer.cpp` [read]): sample time is `frame_time − RESAMPLE_LATENCY (5 ms)`;
  interpolate between two events if available; otherwise extrapolate, only if events are ≥ `RESAMPLE_MIN_DELTA (2 ms)`
  apart, treating gaps > `RESAMPLE_MAX_DELTA (20 ms)` as too stale, and cap extrapolation at
  `min(delta/2, RESAMPLE_MAX_PREDICTION = 8 ms)`. These are tuned constants Rune can start from.
- **Android MotionPredictor / AndroidX `motionprediction`:** Kalman-based pointer predictors whose prediction amount
  depends on "speed and jank" ([AndroidX source](https://android.googlesource.com/platform/frameworks/support/+/57cb7c62c62fe4c95643d4777db) , Kalman `SinglePointerPredictor`) [read search result]. iOS: `UIEvent.predictedTouches`
  and `coalescedTouches` (UIKit) [recall]. Chrome: input resampling + scroll prediction filters [recall].
- **Rune use:** (a) the render thread reads raw pointer events (hardware timestamps) and *samples the latest position
  immediately before encode*, instead of reading app-thread state; (b) resample to `t_present − ~5 ms` with the AOSP
  rules; (c) coalesced events go into the velocity estimator at full rate (see §4); (d) optional: Kalman/linear
  extrapolation capped at 8 ms *only for dragged objects*, with a no-snap blend when the real sample arrives; never for scroll content under the
  user's finger unless measured as better.
- **Cost/risk:** (a–c) low and fits PLAN §4.9 "drags apply 1:1 on the render thread". (d) prediction overshoot at
  reversals; gate by position-error metric on a recorded gesture corpus; ship off by default.
- **Verdict:** a–c adopt now; d spike.

### 4. Gesture velocity estimation  — **adopt now**
- **Source:** Android `VelocityTracker` impulse strategy ([AOSP `VelocityTracker.cpp`](https://android.googlesource.com/platform/frameworks/native/+/refs/heads/main/libs/input/VelocityTracker.cpp)) [read].
  The file documents a physical model: treat the touch as an object whose kinetic energy at release equals the total work
  done by the finger; `v_final = sqrt(2·Σ(Δv·|v|))` per segment, with the sign handled by `|v|` so braking subtracts
  energy; an initial-condition choice (v₀ = 0 vs already moving → ½v₁² start) is spelled out. Uses a ~100 ms history horizon;
  `ASSUME_POINTER_STOPPED_TIME = 40 ms` means a pause before release gives ~0 velocity [read]. The old default was LSQ2
  (2nd-order least squares over the horizon), now an alternative strategy; Flutter's tracker is a polynomial LSQ with a similar horizon and stop rule [recall].
- **Why it matters:** velocity handoff is the "release feel" (Apple's central rule: continue with the finger's velocity).
  LSQ is noisy at the endpoint (it fits the middle of the window), impulse is robust to jitter and un-ringing.
- **Rune use:** implement both (reference = LSQ2 in f64, fast = impulse over a ring buffer), use hardware timestamps,
  include coalesced samples, apply the 40 ms stopped rule, clamp to a platform max fling speed. **Agent trick:** record a corpus of real
  gestures (Kit's iPhone + Mac trackpad) with the trace recorder; score each estimator by how well `x(t_release) + ∫v`
  predicts the *finger's next 30–50 ms if it hadn't lifted* (use gestures cut at random points); pick/tune constants by minimising that error.
  Fixed hand-tuned constants are the norm elsewhere; an offline fit is cheap for agents.
- **Cost/risk:** tiny. Risk: trackpad momentum on macOS is OS-generated (§8), so this is for touch and pointer drags.
- **Verdict:** adopt now.

### 5. Springs, retargeting, projection, effects vs spatial (Apple and Material)  — **adopt now**
- **WWDC18 "Designing Fluid Interfaces"** (session 803, Karunamuni/Alonso) [recall]: response/damping ratio over
  duration; interrupt and redirect without waiting; fluid gestures never wait for the previous animation; momentum
  projection `dest = x + v·rate/(1−rate)` using the scroll deceleration rate; rubber banding `f(x) = (1 − 1/(x·c/d + 1))·d`
  (c ≈ 0.55); velocity handed from the gesture into the spring. WWDC23 "Animate with springs" introduces duration+bounce
  and `.smooth/.snappy/.bouncy`: Rune's parameterisation already matches it.
- **Material 3 Expressive (2025)** [recall; check [m3.material.io motion](https://m3.material.io/styles/motion/overview/specs)]:
  spring tokens come in two families, **spatial** (position, size, rotation; may overshoot) and **effects** (colour,
  opacity; never overshoot), each fast/default/slow. Direct support for Kit's "bounce 0 default" and avoids opacity > 1 or negative-alpha glitches.
- **Perception evidence is thin:** I found no controlled study that springs beat easing curves; the case is
  physical continuity (velocity preservation, interruptibility) and animation-principles heritage. Chevalier et al.,
  "Animations 25 Years Later" (AVI 2016) [recall] is the review. So treat "springs feel better" as design rationale, and let Kit's
  blind A/B chooser (Bradley–Terry) settle specific values.
- **Rune use:** add a per-track `class: Spatial | Effect`; Effect tracks clamp to [min,max] of start/target and use the
  critically damped solution even when asked for bounce; settle rule = analytic time at which `|x−target|<ε_px and |v|<ε_v`
  (ε_px = 1/64 px for geometry per Kit's snapping rule, 1/256 for opacity), then snap exactly and stop the clock (zero-cost-at-rest).
  Accept additive-animation behaviour (UIKit/CA retarget semantics) only if it matches exact segment retargeting; differential-test
  the two on random retarget sequences.
- **Verdict:** adopt now.

### 6. Interruptibility as a testable property  — **adopt now**
- Retargeting `(x, v)` is exact for analytic segments. Test generator: random event scripts (retarget at random times,
  random spring class), asserting (i) `x` continuous, (ii) `v` continuous, (iii) no extra pop at settle, (iv) `x` bounded by
  physical envelope. This is the motion linter's "reversal / velocity snap / stop-dead" family, but checked on *every frame
  of every fuzzed script* rather than on recorded clips.
- **Layered interruption (Apple "pause then redirect"):** a gesture that grabs a running animation must read the *presented*
  value and velocity (not model state), pause the track, drive 1:1, then restart from the finger's velocity. Rune's presented-tree design does this by construction.
- **Verdict:** adopt now (cheap, rides on §5).

### 7. Keeping text crisp while it moves  — **adopt policy now, spike the harness**
**Facts:** macOS dropped LCD subpixel text AA in Mojave (10.14, 2018), so Apple text is grayscale and safe to resample [recall].
Core Animation text inside a layer is rasterised to a texture, so fractional translation is *bilinear resampling*
(stable but soft); Chrome snaps composited-layer raster translation to integers where it can, otherwise text blurs [recall].
GPUI/Zed rasterise glyphs into an atlas at a few quantised horizontal subpixel phases (4 in x; verify the y count in
`crates/gpui/src/platform.rs`) [recall]; Rune lifts that code ([01](01-gpui-zed-warp.md)).

**Policy proposal** (the trick is making it *state-driven by the analytic track*):
1. **Y snaps, X doesn't:** snap baselines to whole device pixels whenever the text is not translating vertically; keep x at ¼-px phases (8 on non-retina). Stem thickness is then constant, only horizontal phase varies.
2. **Moving fast (> ~0.5–1 device px/frame):** allow exact fractional positions (grayscale AA, no hinting). Eye tracking blurs the stems anyway, and the atlas phase quantisation is invisible.
3. **Settling (spring tail):** the tail crawls through sub-pixel phases for a long time, which is the visible "shimmer/crawl". Because Rune knows the analytic time `t_end` when `|x−target|<ε`, **end the track there** (ε = ¼ px for text; Kit's 1/64 px rule applies to shape edges) and snap. A ¼-px final pop is invisible, a 300 ms crawl is not. Also try quantising the *phase* of text only after speed < threshold.
4. **Never mix:** text and its container share one track so there's no relative sub-pixel drift between a label and its pill (Kit's rule on snapping both edges).
5. **Hypothesis to test:** with linear coverage, no gamma/contrast fiddling and no hinting, total ink coverage per glyph is phase-invariant
   (box filter conserves area), so brightness shimmer comes from nonlinear text contrast/gamma and atlas quantisation, not from AA itself. A harness renders
   "lHEx" at 64 phases per size/weight and reports variance of per-glyph ink and per-stem width; CI-gate it. Agents can do this in seconds on the CPU reference renderer.
- **Vernier caution:** even at 460 ppi, motion *hyperacuity* (≈ 5–10 arcsec) makes 1-px stepping visible at slow speeds, so "round to device pixel while moving" is not safe; hence fractional-x while moving, snap at rest.
- **Verdict:** adopt the policy now (it's configuration of existing pieces); spike the phase-sweep harness.

### 8. Scroll physics  — **adopt now, analytic**
- **iOS:** `UIScrollView.DecelerationRate.normal = 0.998` per ms; closed form `x(t) = x0 + v0·(r^t − 1)/ln r`, stop time where `|v|<ε`; rubber band `c = 0.55` ([WWDC18 803](https://developer.apple.com/videos/play/wwdc2018/803/)) [recall]. Android: `OverScroller` spline fling (friction ≈ 0.015, inflexion 0.35) and Compose `splineBasedDecay` [recall]. Both are analytic in *time*, so fling is frame-rate independent and exactly retargetable: perfect for tracks.
- **Snap/targeting trick (Apple):** project the landing point with the decay, choose the snap target from the *projection*, then run a spring from the current `(x, v)` to that target; the user perceives "it knew where I was going".
- **macOS trackpad:** momentum phases arrive from the OS as events ([PLAN §4.9](../PLAN.md)); don't re-simulate. Resample them with the §3 rules, and
  keep your own decay only for programmatic scroll and for non-Apple platforms.
- **Verdict:** adopt now; implement as another track kind `Decay{v0, rate}` with the same retarget/present API.

### 9. Motion blur for UI at high refresh  — **skip by default; spike for flings**
- **Physics:** on sample-and-hold displays, perceived blur ≈ tracking speed × persistence; 1 ms ⇒ 1 px per 1000 px/s ([Blur Busters](https://blurbusters.com/blur-busters-law-amazing-journey-to-future-1000hz-displays-with-blurfree-sample-and-hold/) [recall]). 120 Hz halves hold blur vs 60 Hz, and per-frame displacement halves too, so stroboscopic/"phantom array" stepping disappears at moderate speeds.
- **Do UIs need synthetic blur?** Apple's UI doesn't use it for ordinary motion [recall]. It helps when per-frame displacement exceeds roughly 4–8 px *(est.)*, e.g. a hard fling of a text list; and Kit's rules already ban blur on persistent items (3–4 px defocus blur is only for in-place entry/exit). A directional shutter blur is a different thing from defocus blur, but Kit should decide whether it conflicts.
- **Rune advantage:** velocity is exact, so blur length `= v · shutter` (180° ≈ half a frame) costs nothing to compute. Implement as N instanced taps of a layer/sprite along `x(t_present ± shutter/2)` (analytic, no velocity buffer, no reprojection), applied only when `|v|·period > threshold`; it is the same shape as FLIP interpolation but in time.
- **Cost/risk:** N× fill for the layer; text readability; fights Kit's crispness rule. **Verdict:** skip at rest and normal speed, spike as a fling-only effect (opt-in token), test with a blind A/B.

### 10. Layout animation: FLIP, Motion projection, shared elements  — **adopt rect-space FLIP; spike text-reflow FLIP**
- **Prior art:** FLIP (Paul Lewis, 2015: First-Last-Invert-Play) [recall]; Motion/Framer Motion layout projection (Matt Perry): a projection tree measures old/new boxes, animates a transform delta, and **scale-corrects** children, border-radius and box-shadow so scaling doesn't distort them ([Motion layout docs](https://motion.dev/docs/react-layout-animations); internals in Perry's "Inside Framer Motion's layout animations") [recall]; SwiftUI `matchedGeometryEffect`; Compose `SharedTransitionLayout` (1.7, 2024); CSS View Transitions (browser snapshots old/new and morphs the pair) [recall].
- **Why Rune differs:** FLIP and projection exist because DOM layout is expensive and transforms are the only cheap animatable. Rune lays out once to the final geometry and animates each node's *edges* (PLAN §4.9), and draws at the presented size, so there is nothing to scale-correct. Keep that. Two extras worth taking from Motion:
  1. **Read the presented rect as "first"**, not the last target, so interrupting a layout animation resumes from where it is (Motion calls this latest-snapshot; matches Kit's retarget rule).
  2. **Single progress for shared-element travel** (Kit rule 12): interpolate source→dest rects from one value per element; never separately-sprung camera plus element.
- **Spike: word/glyph-level FLIP for text reflow.** When a container resizes and a paragraph rewraps, SwiftUI/CSS snap lines. Because text content is stable, each cluster has a perfect identity (its character index). Parley gives us old and new cluster rects, so each word's track runs old→new in rect space, and glyphs outside the visible clip fade via the soft mask. Impractical by hand (identity bookkeeping everywhere); trivial with a generated per-run track table. No mainstream precedent I know of, so it is a feel differentiator. Risks: words crossing each other mid-flight (monotonic-path rule: lines reflow in reading order so usually fine), cost on long text (cap by visible lines/word count), ligatures/shaping across the move (move *runs*, not glyphs).
- **Verdict:** rect-space FLIP adopt (already planned); text-reflow FLIP spike.

### 11. Dirty/idle correctness and display pacing details  — **adopt now**
- ProMotion/VRR: request the display's max rate during motion and drop to idle with zero wakeups once all tracks settle (PLAN §6b). `CAMetalDisplayLink` (iOS 17 / macOS 14) provides the target present timestamp and a latency preference [recall]. Pace with the OS, never sleep-based loops: Kit's own HUD lessons (no per-frame window resizes, `TimelineView` idle pause) apply.
- **Verdict:** adopt now (cheap, already in the plan).

### 12. Auto-tuning feel from comparisons  — **later**
Kit's chooser (blind A/B with Bradley–Terry stopping rule) generalises: let agents sweep 3–5 values of duration/bounce for one interaction and ask Kit for ← / → picks, update the preset JSON. That puts *Kit's perception* in the loop at 10 comparisons per parameter, replacing guessing about spring-vs-easing. Cost: UI in `mk tune`. Verdict: later, after presets exist.

---

## Steal
- Android `InputConsumer` resampling constants (5 / 2 / 20 / 8 ms) and the `VelocityTracker` impulse model with 100 ms horizon and 40 ms stop rule.
- Apple's hitch-time-ratio thresholds (5 / 10 ms/s) and the commit-vs-render hitch split.
- Perfetto FrameTimeline's jank taxonomy (deadline missed, buffer stuffing, prediction error).
- Chrome's "consistent input-to-frame delivery" framing for scroll jank.
- M3 Expressive's spatial vs effects spring classes.
- Motion's "measure presented rect as First" for interrupted layout animation.
- WWDC18's project-then-spring pattern for snapping and flings.

## Avoid
- Sampling animation by wall-clock "now" or by frame count/accumulated dt (error accumulates and hides hitches).
- Rescaling children to fake size animation (distorts text, radii, shadows).
- Rounding text to whole device pixels while moving slowly (crawl) or leaving it fractional forever after (shimmer, blur).
- Prediction on scroll content by default (overshoot at direction reversals, feels "swimmy").
- Quoting the 1 ms / 10 ms latency numbers without checking the primary PDF (unread here).
- Synthetic motion blur on persistent items (violates Kit rule 11/persistent-items).
- Treating average fps or "no dropped frames" as the smoothness gate.

## Recommendation for Rune
1. Build the present-time sampling and trace fields first (§1, §2); everything else is measured against them.
2. Put input on the render thread with late latching and Android-style resampling (§3); add the impulse + LSQ2 estimators and the gesture-corpus harness (§4).
3. Add track classes (Spatial/Effect/Decay), analytic settle times, and fuzzed interruption tests (§5, §6, §8).
4. Encode the text policy as defaults plus the phase-sweep harness (§7).
5. Spikes, in order: text-reflow FLIP (§10), fling-only blur (§9), capped prediction for drags (§3d), A/B chooser in `mk tune` (§12).

**Rejected:** a CA-style additive-animation model (equivalent to exact retarget but harder to reason about and lint);
average-fps gates; Motion-style transform/scale-correction projection (solves a DOM problem Rune lacks); always-on
motion blur (conflicts with Kit's rules, little benefit at 120 Hz); relying on OS compositor snapping for text.

## Open questions
- Exact ms numbers in Ng/Jota (PDF unreadable via the fetch tool; run `pdftotext` locally or read it).
- How much display latency does Metal/wgpu add on macOS ProMotion with `maximumDrawableCount = 2` vs 3, and does `presentAfterMinimumDuration`/`CAMetalDisplayLink` remove it? Needs a reserved-hardware spike (not a developer's desk display).
- GPUI glyph atlas subpixel phase counts in x and y (read `platform.rs` before reusing).
- Whether iOS exposes enough touch-sample timestamps to a Rust render thread outside UIKit's main-thread delivery (coalesced touches arrive via the main run loop); see [11](11-platform-shells.md).
- Apple hitch thresholds and the Material 3 Expressive spring tokens were recalled, not re-read; confirm before they become policy.
- Does Kit want a directional (shutter) blur at all, given rule 11 and the "persistent items never blur" rule?

# 10 · Audit of Kit's own repos: what he cares about, what to reuse, where GPUI fell short

Researched 2026-10-21 on Studio (and Psychopomp on the Pro over `ssh pro`). Sources are local source trees,
git history, and Kit's own words in OpenCode sessions, read through a read-only SQLite connection to
`~/.local/share/opencode-studio-migration/opencode.db`. Session IDs are cited so the full context can be found.
Nothing was built or run.

## Summary

- **Kit has rebuilt the same UI three ways in ten days and none of them is finished.** Scrawl's recording pill went
  GPUI → hand-rolled Metal in an `NSPanel` → SwiftUI in a helper process (Oct 10–20). Flit went GPUI → SwiftUI panel
  plus a Core Animation morph. Hex has a stalled `feat/native-macos` AppKit/SwiftUI branch, while `main` stays on
  GPUI because Hex has to ship on Windows and Linux. That churn is the strongest case for Rune: Kit keeps paying
  for a two-language bridge to get Apple-level feel, and still can't have it on other platforms.
- **The reasons are concrete and repeat across apps:** no backdrop blur and no Liquid Glass; animation frames that
  wait behind the app's main thread; text that looks thinner than native and stalls on cold glyphs; window and focus
  behaviour wrong for panels and HUDs; a crates.io release frozen at `gpui 0.2.2` since 2025-10-22, so fixes need a
  fork; and "it looks like shit" next to native apps. The per-reason list with evidence is §3.
- **The architecture Kit converged on, independently, three times:** Rust owns all state as a message-in,
  effects-out state machine, and the UI is a pure function of a snapshot (Flit `src/ui/native_view.rs`, Scrawl
  `helper/*.swift` driven by JSON lines, Hex `libhex` with a Ghostty-style C ABI). Rune should make that the native
  model with no serialization boundary. Keep "state machine in Rust, view = f(snapshot)". Drop the JSON bridge, which
  broke in ways Rust tests couldn't see (Flit `AGENTS.md`: Swift `Decodable` failures).
- **Reusable pieces already exist:** an analytic spring with position+velocity and a proven settling bound
  (Psychopomp `motion.rs`), a canonical duration+bounce palette with Rust/Swift/TS/CSS codegen (`motion-presets`),
  a trace format plus lint rules for pops, snaps and jumps (`motion-lint`), a remote tuning protocol with Swift
  codegen (`knobs` `createRemoteControls`), the island motion spec (`~/code/home/research/2026-10-16-motion-v3/island/NATIVE.md`),
  a headless-model-to-WASM parity check (Scrawl `scrawl-pill-web`), and a deterministic `wgpu` renderer with
  pixel-diff verification (Psychopomp `verify`).
- **Spring definitions don't agree across Kit's code.** Flit uses motion.dev's `visualDuration` (ω = 2π/(d·1.2)),
  while `motion-presets`, knobs, SwiftUI and OpenCode iOS use ω = 2π/d. So a "0.21 s" spring in Flit is about 20%
  slower than a "0.21 s" spring everywhere else. Rune should fix one definition (SwiftUI's) and generate the rest
  from it.
- **Even inside Apple's frameworks Kit has to mix three toolkits.** The OpenCode iOS app uses UIKit for 120 Hz scroll
  and gesture authority, SwiftUI for Liquid Glass morphs, Metal for per-frame visuals, and a private API for edge
  blur. It also hand-writes an analytic `SpringSolver` for what SwiftUI can't animate. The feel target is reached by
  *composition*, not by any one framework. Rune has to cover all of it, or compose with the system pieces it can't
  replace.
- **The constraint that matters most for Rune on Apple:** Liquid Glass and backdrop sampling exist only in the
  window server (`NSGlassEffectView`, SwiftUI `.glassEffect`, private `CABackdropLayer`). No in-process renderer can
  reproduce them. Rune needs a first-class way to **interleave its own Metal layers with system Core Animation
  layers and views**.
- **Recommended first dogfood target: the Scrawl recording pill.** It is small (one capsule and about eight states),
  its motion model is already headless Rust (`src/native/pill/model.rs`, 2,161 lines, already compiled to WASM) with
  live `tune.json`, `scrawl lab` replays and motion-lint traces, and there are three existing implementations to
  benchmark against (GPUI, Metal, SwiftUI). Kit has strong, recorded opinions on it. Second target: Flit's actions
  tray and launcher panel. Then Hex's cross-platform settings and HUD. OpenCode Desktop last.

---

## 1. What Kit cares about, inferred with evidence

### 1.1 Motion that never jumps, pops or stutters, judged frame by frame

- "I want this starting animation to be super beautiful. There's some weird issues with it if you go like frame by
  frame" (Scrawl session `ses_edb816b31ffe4B3nexhh9rqAJp`, 2026-10-11).
- "There should be no jumps, everything should smoothly transition, you know?" and "No, there is a bit of a jump. Pay
  really close attention to the transitions." (same session, 2026-10-13).
- "it's SOO janky??? can we make it smoother? use swift ui instead???", "can we not implement motion.dev style
  animotinos??", then "do it with the metal version and a FIXED WINDOW", "but we can use some metal rendering layers
  where tasetful." (2026-10-20 04:58–05:27).
- Flit's `docs/motion.md` is a frame-by-frame critique at 16 ms steps ("Open Frame 4 (64 ms): 'Copy Deeplink' is
  sliced horizontally in half"). It lists the defects Kit treats as bugs: garage-door clip wipes, double-exposed
  text, crop masks instead of layout animation, a flat corner radius.
- `~/.dotfiles/skills/kit-motion/SKILL.md` rule 11: "**No hard edges, ever** (2026-10-19)… never a hard crop and
  never an ellipsis." Rule 12: "Straight paths to where it's going." Plus craft rules learned on the Scrawl pill:
  snap both edges to 1/64 px, use a fixed max-size transparent `NSPanel`, and "never trust eyeballs alone".
- The island spec (`~/code/home/research/2026-10-16-motion-v3/island/NATIVE.md`): one shape whose width, height and
  radius are springs; content masked by the outline but never hidden by the mask; exits timed by time-to-contact with
  the edge; "keep velocity on retarget, as SwiftUI does"; hit-testing uses the model layer, not the presentation.

### 1.2 Springs defined by duration + bounce, interruptible, from one palette

- `motion-presets/presets/kit-motion.json` is "Kit's named spring palette": `instant .1/0`, `snap .25/0`,
  `settle .3/0`, `reflow .4/0`, `glide .32/0`, `morph .3/.16`, `pop .25/.15`, `drift .85/0`, each with a shorter exit.
  A blind A/B chooser with a Bradley–Terry fit tunes them, and the file generates `KitMotion.swift`, `kit_motion.rs`,
  `kitMotion.ts` and CSS (`motion-presets/README.md`).
- Nearly everything has bounce 0. Overshoot is for morphs and presses only. When the Scrawl pill felt flat, the fix
  was more bounce on *some* springs ("it moves on springs but looks like plain easing"). Exits always stay at bounce 0.

### 1.3 Native look, Liquid Glass, Mac-quality chrome

- Flit: "let's switch this to being a Mac native app… at least the UI should be Mac native, kind of like how Ghostty
  is set up. Because then I bet the UI could look way nicer and we can actually use liquid glass and stuff."
  (`ses_eb96316daffevhidtO7Z4W583K`, 2026-10-19 04:02).
- Hex: "This desktop app for Mac looks like shit. Can we make a native AppKit one? … Think about it like how Ghostty
  works" (quoted at the top of `hex-native/docs/plans/native-macos-app.md`).
- Scrawl: "oh wait what? .Oh, why don't we just make it Mac native instead and use metal? … I didn't realize it was
  GPUI…. METAL BABY" (2026-10-19 04:02–04:03).
- Kit is also picky about the glass itself: "this liquid glass effect is… too much. It's also really hard to see the
  hover row now. And there's like some weird glitching around the borders" (Flit, 2026-10-19 20:22). "The glass is
  less readable than the black one" (Scrawl, 2026-10-19). Glass has to be legible and tunable, not just present.

### 1.4 Instant response

- "Hitting command space it should open much faster Like almost immediately… There's really no need for a delay that
  makes it feel fast if it just booms opens" (Flit, 2026-10-20). Flit's motion tokens set launcher show/hide, view
  push and row selection to **0 ms**.
- Flit's `docs/perf-log.md` measures input-to-CPU-frame in microseconds and keeps only measured wins. Its open
  question is "real hotkey delivery and first GPU-presented frame", which is unmeasured because there's no
  frame-presentation instrumentation.

### 1.5 Agent-verifiable UI and tooling over eyeballing

- Every app grew its own replay, trace and tune loop: `scrawl lab pill … --trace`, `scrawl tune set …` (hot-reloaded
  within 250 ms), Flit `--demo` with hidden-frame benchmarks, OpenCode iOS `MotionXRay` plus `scripts/proof`,
  `motion-lint`, `layout-lint`, Psychopomp `verify baseline/compare`.
- Kit asked for the tooling to be consolidated: "There's one place where all the tooling lives for both TypeScript or
  web and native stuff for diagnosing these issues" (Scrawl, 2026-10-19 17:31). The result is the `kit-motion` skill.
- Agents keep hitting the same limit: "offscreen renders… don't draw glass, vibrancy or system colours, so colours,
  the menu's glass background and the red pill tone haven't been seen" (Flit "Port remaining views" session
  `ses_ea0a5ae08ffeh4S7O3bxWwG07r`). Headless renders that leave out materials can't verify what Kit cares about.

### 1.6 Live tuning instead of rebuilds

- "Give me some knobs on the page so I can play with exactly how those looks" (Scrawl, 2026-10-13). "Maybe we should
  use my knobs project" (same day). The Scrawl pill got a WebGPU playground with an "Apply to my Scrawl" button that
  pushes values into the running app.
- Hex's native plan defers KnobsKit but keeps every tunable in one `HexTuning` "so they can be wired to live knobs
  later without touching call sites."

### 1.7 Multiplatform, but without paying for it in feel

- Flit: "eventually we might want to make this multiplatform, so, I'm not sure… This might just be for me, so
  whatever is the best to work with." (2026-10-19 04:11).
- Scrawl: "I should also try to work on a version that'll work for Linux and Windows… Maybe GPUI. That could be
  separate for that version, I don't really care." (2026-10-18). Scrawl's Linux and Windows ports run on GPUI (PR #24
  and PR #3).
- Hex `main`: "one shared desktop app and settings.json on macOS, Windows, and Linux (#142)". The native macOS branch
  has been parked at "M4 cutover pending" since 2026-10-17.

### 1.8 Minimal UI, few labels, strict design tokens

- Flit `AGENTS.md`: "Keep the UI minimal. Use only necessary control labels and keyboard hints."
- Hex `docs/design-system.md`: tokens only, enforced by a test that greps for raw `rgb(0x…)` and `.text_size(`; five
  type steps; one accent per screen; "Verify by looking".

---

## 2. Reusable components for Rune

Verdicts: **Adopt** (lift the code or design as is), **Port** (re-implement against Rune's core and keep the
semantics and tests), **Learn** (take the lessons, not the code), **Skip**.

| Component | Path | What it is | Verdict |
| --- | --- | --- | --- |
| Analytic spring | Pro: `~/code/open-source/psychopomp/crates/psychopomp/src/motion.rs` (200 lines) | Closed-form `Spring::sample(initial: MotionState{position, velocity}, target, t)` for under- and critically damped springs, plus `settling_time` that bounds the *entire* remaining trajectory. Deterministic arbitrary-time sampling, tested. | **Adopt** as the core spring. Add the overdamped (negative bounce) branch and a duration+bounce constructor with the SwiftUI mapping. |
| Retarget scheduler | Pro: `psychopomp/src/timeline/retarget.rs`, `playback.rs` (764 lines) | Cancellation-safe retargeting: each channel inherits position and velocity across mid-flight reversals, unchanged destinations don't restart, exact `u64` nanosecond plan time, a pausable local clock that freezes once settled. | **Port.** It's the "interruptible, never restart from rest" semantics Rune needs for layout and transition animation. |
| Spring palette + codegen | `~/code/open-source/motion-presets/presets/kit-motion.json`, `presets/generated/kit_motion.rs` | Named enter and exit springs (duration, bounce), interaction roles, a blind A/B chooser, generated Rust/Swift/TS/CSS | **Adopt** as Rune's default motion tokens (`Motion::SETTLE`, …). Keep the JSON as the source of truth and add Rune as one more codegen target. |
| Kit-motion rules + island spec | `~/.dotfiles/skills/kit-motion/SKILL.md`, `~/code/home/research/2026-10-16-motion-v3/island/{NATIVE.md,model.js,shape.wgsl}` | Written rules with numbers: content follows walls, time-to-contact exits, blur clocks, two-edge 1/64 px snapping, soft masks, Reduce Motion fallbacks, SDF + smooth-min outline shader | **Adopt** as the spec for Rune's transition primitives and as test oracles. `shape.wgsl` ports directly. |
| motion-lint | `~/code/open-source/motion-lint` (`Sources/MotionLintRecorder`, `HitchProbe`, `src/analyze.ts`, `src/web/sweep`) | Per-frame JSONL trace of element frames, opacity, radius and clip; rules for `pop-in/out`, `opacity-snap`, `size-snap`, `jump`, `reversal`, overlap; hitches charged to named intervals; a gallery for labelling | **Adopt the trace format.** Rune's inspector should emit motion-lint JSONL natively from its own tree for free (no recorder needed, since Rune owns the tree), so the existing rules, baselines and galleries work unchanged. |
| layout-lint | `~/code/open-source/layout-lint` | Finds the shared lines on a page, near-misses 1–4 px off them, clipped and overflowing elements, off-centre icons; gated candidate fixes | **Port** the rules to run over Rune's layout tree (deterministic, no DOM). The finding shape is shared with motion-lint. |
| Remote tuning protocol | `~/code/open-source/knobs/src/core/remote.ts` (`createRemoteControls`, `ControlDescriptor`, `.swift()` codegen); OpenCode iOS `OpenCode/Tuning/{Tunable.swift,TuningRemote.swift}` | Descriptor-driven panel over WebSocket (`set`, `setMany`, `resetAll`, 600 ms echo hold), pairing link `opencode://tune?u=ws://…&c=CODE`, emits `TunableSpring(...)` Swift | **Adopt the protocol.** A Rune app publishes descriptors for its `Tunable` values, and the knobs web panel (or a Rune-native panel later) drives them on Mac or iPhone. Add a `.rust()` codegen next to `.swift()`. |
| knobs spring + feel code | `knobs/src/core/spring.ts`, `knobs/src/panel/{feel,give,swipe,thumb}.ts`, `panel/gpu/renderer.ts` | 480 Hz fixed-substep numeric spring; rubber-band, detents, precision-by-distance sliders; a WebGPU panel renderer | **Learn.** Use analytic springs instead of substepping. The slider and gesture feel recipes are good references for Rune's control library. |
| Scrawl motion plumbing | `~/code/open-source/scrawl/src/ui/motion.rs` (815 lines), `src/ui/lab.rs` | One animation clock with a live `timeScale`, a `Tune` struct overridden by `~/.config/scrawl/tune.json` (reloaded within 250 ms), a per-frame trace in motion-lint format, `scrawl lab pill <scene>` deterministic replays | **Port** as Rune built-ins: global time scale, `Tunable` values with a file or socket override, scene replay as a test fixture, trace to motion-lint. |
| Scrawl pill model | `scrawl/src/native/pill/model.rs` (2,161 lines), `pill.metal` (377), `atlas.rs` (519) | Headless pill state machine and layout (`layout_of`, `FrameState`), SDF capsule shader, CoreText glyph and icon atlas | **Port** as Rune's first app (see §4). The model already compiles to WASM with 0.0000125 pt parity (`scrawl-pill-web` `a802365`). |
| Pill parity playground | `~/code/open-source/scrawl-pill-web` (`lab/pill-web`) | The Rust model in WASM plus the Metal shader ported line for line to WGSL, real-pill screenshot comparison, frame-time plot, scrubbing | **Learn and generalise.** It is a hand-built preview of Rune's "same Rust UI on Mac and WebGPU". |
| Flit glass recipe | `~/code/open-source/flit/docs/glass.md`, `src/macos.rs` (`GalleryGlassChrome` ~l.1863, `transition_actions_overlay_ca` l.2850, `sample_ca_presented_state` l.2738) | Gallery glass as a CA layer stack: `CABackdropLayer` + blur/saturate/brightness `CAFilter`s, gloss gradient, highlight arcs, 135° specular rim, shadow with an even-odd cutout; a velocity-preserving CA spring handoff | **Port** as Rune's macOS "system material" backend. It is the exact layer recipe for glass Rune can't draw itself. Its interruption handoff (presentation layer → `initialVelocity`) is reusable. |
| Flit Rust↔UI snapshot protocol | `flit/src/ui/native_view.rs` (2,495 lines), `native/Bridge.swift` | `Snapshot` out and `LauncherInput` in; text-field revision counters so typed text isn't clobbered by echoes | **Learn.** The revision and echo pattern carries over to Rune's controlled text inputs. The JSON boundary goes away. |
| Flit perf harness | `flit/docs/perf-log.md`, `scripts/bench-*.py`, `examples/perf*` | Hidden-window CPU-frame benchmarks with interleaved baseline/candidate processes and p95 of process medians | **Adopt the method** for Rune's perf CI. |
| Hex design-token linter | `~/code/open-source/hex/src/design_tokens.rs`, `docs/design-system.md` | Tokens plus a test that fails on raw style literals | **Adopt the idea** as a Rune lint for app code. |
| Hex HUD shader | `hex/src/dictation_indicator.metal` (309 lines), `dictation_indicator.rs` | Whole pill SDF, glow, waveform and dots in one draw call; "<0.2% GPU… zero heap allocations per frame" (hex-native plan) | **Learn.** Evidence that one-shader HUDs are cheap. It's a second consumer for Rune's custom-shader node. |
| Hex native plan | `~/code/open-source/hex-native/docs/plans/native-macos-app.md` | Detailed AppKit vs SwiftUI vs TCA split, Ghostty C-ABI analysis, NSPanel recipes, FFI comparison (cbindgen vs UniFFI vs swift-bridge vs BoltFFI) | **Learn.** Required reading for Rune's macOS shell. Its NSPanel and menu-bar rules are directly applicable. |
| OpenCode iOS motion stack | `opencode-ios-motion-tokens/OpenCode/Core/Motion/{SpringSolver,MotionDisplayLink,PresenceGate}.swift`, `Tuning/MotionXRay.swift`, `docs/motion-tooling.md`, `docs/architecture.md` | Analytic spring solver; per-tick presentation-layer recorder with six detectors (`duplicateMorphSurface`, `opacityHandoff`, `geometryJump`, `prematureRetirement`, `overdrawHotspot`…); render-time slow motion through a single `AnimationTimeScale` | **Learn and port the detectors** into Rune's inspector. The `AnimationTimeScale` lesson ("true render-time slow motion instead of post-hoc frame stretching") belongs in Rune's clock. |
| HotReloadKit | `~/code/open-source/hot-reload-kit` | `@_dynamicReplacement` hot reload for SwiftUI, TCA, UIKit and AppKit, ~100–300 ms, state preserved; **simulator and Mac only** | **Learn.** It is the bar to beat (300 ms with state kept), and it shows what doesn't work on a physical iPhone (see §3.10). |
| Psychopomp renderer | Pro: `psychopomp/crates/psychopomp-render` (`wgpu 30`, `cosmic-text`, headless offscreen target, `verify` pixel diffs, live WGSL via `PSYCHOPOMP_SHADER_DIR`) | Deterministic `Time → Pixels`, stable-identity text sprites, SDF rounded boxes, thin-surface glass optics (`math/optics.rs`), linear-light accumulation | **Learn for the renderer, adopt for testing.** A prototype offline renderer with no input, layout engine or a11y (its `PRIOR_ART.md` §GPUI says to avoid those deliberately). Reuse `math::{easing,curve,shapes,optics}`, the `verify baseline/compare` workflow and the deterministic-pixels discipline. Note its finding: `glyphon`'s dynamic atlas "can invalidate glyph coordinates during atlas growth". |
| Psychopomp authoring DSL | Pro: `psychopomp/DSL.md` | Denotational design: a channel is `Time → (position, velocity)`; plan time in `u64` ns; why the old `Sequence/Parallel/Delay` algebra failed (physical settling ≠ choreographic span) | **Learn** for Rune's transition and choreography API (stagger, "not before", arrival-timed beats). |
| `@kit/gpu-canvas` | `~/code/open-source/gpu-gallery/packages/gpu-canvas`, `web/core/renderer.ts`, `tiles.wgsl.ts` (~8.4k lines in `web/core`) | 120 Hz WebGPU single-draw-call instanced renderer (28 floats per instance, 16 SDF and texture primitives), edge-blur post pass, UIScrollView-style momentum, pinch and pan | **Learn.** A working proof that Kit's web target runs at 120 Hz with SDF instancing. The edge-falloff pass is the "no hard edges" mask in a shader. |
| gpui-fork patches | `~/code/open-source/gpui-fork` commits `2582af8`, `2e52b98` | sRGB `CAMetalLayer` colorspace, triangular dithering on gradients and shadows, luminance-based glyph dilation honouring `AppleFontSmoothing`, display-link fallback, non-key panel class, a resign-key deadlock fix | **Learn.** A checklist of things Rune's mac backend must get right on day one (§3.5). |
| Usher onboarding | `flit/vendor/usher` (vendored), Scrawl `helper/PermissionPanel.swift` (1,218 lines) | Permission onboarding with a drag-the-app-into-System-Settings floating panel | **Skip for now.** It's SwiftUI and works. Revisit once Rune has drag sources and system-window placement. |
| `kitlangton/Hex` (archived Swift/TCA) | referenced in `hex-native` plan | Lesson: running 60–120 Hz audio meters and transcript tokens through TCA actions caused main-thread churn | **Learn.** High-frequency signals must bypass the reducer and update render properties directly. |

---

## 3. Where GPUI and Rust fell short, and why Kit reached for Apple frameworks

Each item says what broke, the evidence, what the Apple path gave, and the requirement for Rune.

### 3.1 No backdrop blur, so no Liquid Glass or frosted materials

- **What broke:** GPUI 0.2.2's macOS renderer is a single forward pass into one `CAMetalLayer` drawable, so a `div`
  can't sample or blur what's already beneath it (`flit/docs/glass.md` §3, `docs/motion.md` §4). Liquid Glass
  (`NSGlassEffectView`, SwiftUI `.glassEffect`) is drawn by the window server and can't be expressed inside GPUI's
  layer.
- **Workarounds Kit paid for:**
  - Flit put pills and the actions tray in a click-through **child `NSWindow`** above the GPUI window, so a
    `CABackdropLayer` (private class, with an `NSGlassEffectView` `.clear` fallback) could sample the parent
    window's Metal layer.
  - Scrawl's Metal pill hosts an `NSGlassEffectView` inside `NSGlassEffectContainerView`, driven on the same spring
    silhouette, with `CAMetalLayer.presentsWithTransaction` inside a shared `CATransaction`
    (`scrawl/src/native/pill/mod.rs:1-8`).
  - Hex's `quiet-native` branch swapped GPUI panels for `NSVisualEffectView`/`NSGlassEffectView` sidebars
    (`hex-quiet-native` `eac9d51`, `ef361c6`: "On macOS 26+ the .sidebar NSVisualEffectView renders near-opaque grey,
    so the sidebar and capsules now use NSGlassEffectView").
- **Rendering glitches from mixing the two:** "there's like some weird glitching around the borders here, you still
  see the normal box behind it" and "weird little edge issues… I think we need to make this, like, a fully
  transparent window over the whole thing" (Flit sessions, 2026-10-19 and 2026-10-20).
- **Even on iOS:** Liquid Glass *morphing* (`GlassEffectContainer`, `.glassEffectID`, matched-geometry glass) is
  **SwiftUI-only**. UIKit has no public equivalent (`opencode-ios-motion-tokens/docs/architecture.md` §3).
- **Rune requirement:**
  1. Its own backdrop pass (blur, saturate, brightness on what's already rendered) for in-app materials on every
     platform, including the web.
  2. On Apple, a compositor seam that can put a **system glass view or `CABackdropLayer` between Rune layers**,
     tracking a Rune-animated shape every frame in one `CATransaction`.
  3. A fixed transparent full-size window, so glass edges never show the window box.

### 3.2 Animation frames waited behind the app's main thread

- **What broke:** the Metal pill's frames "waited in line behind the rest of the app". Animation time advanced
  unevenly (8 ms, then 17 ms), and under load it dropped to about 30 fps with gaps up to 117 ms. The fix gave the
  pill its own display-link thread and advanced each frame by exactly the refresh interval. Dropped frames went
  85 → 6 on finish and 47 → 2 on tap → Area (Scrawl session, 2026-10-20 04:54).
- **What the Apple path gave:** Core Animation and SwiftUI animations run in the **render server, out of process**.
  "macOS's animation process plays it all at the screen's refresh rate… the Rust app being busy can't touch it"
  (2026-10-20 04:59). Flit moved its 120 Hz pill ↔ tray morph to `CASpringAnimation` in WindowServer, and GPUI renders
  the panel **once** at its final layout ("Static Final-Layout GPUI Render (0 per-frame CPU relayout)",
  `flit/docs/motion.md` §2).
- **Rune requirement:**
  - Render and animate on a dedicated thread with display-link pacing, separate from app logic.
  - Advance animation time by the vsync target timestamp, not wall-clock deltas.
  - Prefer **sampling analytic springs** (pure functions of time) over integrating frame by frame, so a late frame
    lands on the right pose.
  - For transform, opacity and bounds-only motion, consider handing animations to Core Animation (a "promote to
    render server" path), the way Flit does.

### 3.3 Resizing windows mid-animation, and the OS animating your windows

- **What broke:** jumps on open, a clipped shadow, and a stop-and-go morph. Measured on the Metal pill: "macOS was
  animating the panel's appearance… shrank the whole window by up to 10 pt and back", "Content was outrunning the
  shape… in 261 transitions", "The shadow was clipped", "Clicks during a morph landed where the capsule was heading
  rather than where it was drawn" (2026-10-20 05:51).
- **What fixed it:** a fixed max-size transparent `NSPanel` (640 × 84 pt in Scrawl, 750 × 460 in Flit's overlay), the
  shape animated inside it, clicks passing through outside the shape, and window appearance animation turned off.
  This became kit-motion law: "Never call `NSWindow.setFrame` per animation frame".
- **Rune requirement:** the shell's default for popovers, HUDs and pills is a fixed transparent surface with
  shape-based hit-testing, no OS window animation (`animationBehavior = .none`), and shadows inside the surface.
  Hit-testing has to be explicit about model vs presentation geometry, and the island spec says model.

### 3.4 Panel, focus and activation behaviour wrong for launchers and HUDs

- **GPUI focus events arrive a turn late:** "GPUI reports focus changes one turn late. So the GPUI window's 'lost
  focus' event from before it became key arrived after you clicked into it. Flit took it as clicking away and hid."
  Flit needed `gpui_activation_changed` and `accepts_activation_loss` workarounds
  (`ses_ea0a5ae08ffeh4S7O3bxWwG07r`; `native_view.rs` "GPUI reports activation a turn late").
- **Dock policy:** "gpui 0.2.2 makes the app Regular after launch despite `LSUIElement`, so `ui::stay_out_of_dock`
  sets Accessory again" (`scrawl/AGENTS.md`).
- **No non-activating panels:** the fork added `GPUINonKeyPanel` (`canBecomeKeyWindow = NO`) so clicking a capsule
  never takes key status from the frontmost app. It also fixed a main-thread **deadlock**: `resignKeyWindow` was
  called with the window-state lock held (`gpui-fork` `2e52b98`).
- **Responder chain:** Quick Look finds its controller through the key window's responder chain. "The classic GPUI
  window is not a Quick Look controller, so drive the panel directly" (`flit/native/Launcher.swift:432`).
- **Linux:** "Scrawl runs through XWayland, because the GPUI version we use can't pin windows on top under native
  Wayland" (Scrawl session, 2026-10-20 06:04). Hex hit a Wayland panic on keymap-less keyboards (`gpui-fork`
  `a0c6474`).
- **What the Apple path gave:** Flit's SwiftUI panel "never activates Flit, so the app the user was in stays
  frontmost" (`Launcher.swift:171-172`), and it gets Quick Look and paste-into-previous-app semantics for free.
- **Rune requirement:** a platform shell with first-class window kinds:
  - non-activating panel, status-level HUD, child overlay window, key-without-activate;
  - `canJoinAllSpaces` and `fullScreenAuxiliary`;
  - accessory/regular activation policy;
  - native responder-chain participation;
  - focus events delivered synchronously with the correct ordering;
  - layer-shell on Wayland.

### 3.5 Text and colour looked worse than native

- **Thin text:** GPUI rasterized glyphs with a black fill and no font-smoothing emulation, so light-on-dark text
  looked thinner than AppKit's. The fork adds **luminance-based glyph dilation** honouring `AppleFontSmoothing`,
  plus a 1 px raster-bounds dilation (`gpui-fork` `2582af8`, `text_system.rs`).
- **Colour mismatch and banding:** the fork sets the `CAMetalLayer` colorspace to sRGB and adds triangular-PDF
  dithering to gradients and shadows. Hex `1efe997`: "Improve motion springs, render-path caching, and Metal
  sRGB/dithering".
- **Cold glyphs stall frames:** "the first query exposing previously unseen glyphs still rasterizes synchronously in
  GPUI: first-ever `s` 13.462 ms". Fixing it took atlas warmup at startup (21.7 ms moved to launch), an explicit
  Apple Color Emoji font to avoid system fallback (106 → 71 ms first open), and a persistent shaped-line cache,
  because "GPUI 0.2.2 retains text layouts only across adjacent frames" (`flit/docs/perf-log.md`, Emoji follow-up).
- **The Metal pill skipped GPUI text entirely:** it rasterized glyphs and icons with **CoreText/CoreGraphics** into a
  Retina atlas (`scrawl/src/native/pill/atlas.rs:1`). "Every element is drawn from macOS's own text and icons"
  (Scrawl, 2026-10-19 04:05).
- **Rune requirement:**
  - Shape and rasterize with the platform's text stack on Apple (CoreText) to match native weight, smoothing and
    emoji, or at least match its gamma and dilation exactly.
  - Rasterize glyphs on a background thread and prewarm the atlas.
  - Retain shaped lines across frames by identity.
  - Use an atlas that never invalidates live coordinates on growth (Psychopomp's glyphon finding).
  - Use an sRGB/P3-correct swapchain, and dither gradients and shadows.

### 3.6 Text input and controls

- Flit's native panel couldn't take over the Mail and Messages reply composers because "they need a multi-line text
  field". They stayed in GPUI (`ses_ea0a5ae08ffeh4S7O3bxWwG07r`).
- Flit's GPUI single-line input pulls in `gpui-component 0.5.1`, which "unconditionally pulls in 71 crates
  (`tree-sitter`, `html5ever`, `lsp-types`, …)". `gpui-component` ≥ 0.6 moved to Longbridge's `gpui-pre` fork, so
  0.5.1 is the last release compatible with crates.io `gpui 0.2.2` (`flit/docs/rust-review.md` §2.1).
- OpenCode iOS keeps the composer in UIKit (`UITextView` subclass) to intercept huge pastes before TextKit lays them
  out, keep first-responder identity, and run `UIPasteControl` trust rules. Mic touch-down fires in `touchesBegan`
  (~2 ms) "before SwiftUI gesture disambiguation" (`docs/architecture.md` §3 table).
- **Rune requirement:** a first-party, dependency-light text input (single- and multi-line, IME, selection,
  platform key bindings, paste interception) is table stakes for the second dogfood target. Gesture recognizers need
  touch-down latency on par with UIKit.

### 3.7 Upstream is frozen, so every fix is a fork

- `gpui` on crates.io is still `0.2.2` (2025-10-22). "Zed Industries has not published a newer official `gpui` crate
  to crates.io since `0.2.2`" (`flit/docs/rust-review.md` §2.1).
- Kit's fork carries rendering, text, display-link and window fixes. Hex depends on it by git rev. Before that, Hex
  vendored an 84k-line copy of GPUI (`hex` `8f463e0`).
- Flit's `AGENTS.md` forbids local GPUI checkouts, which is why glass went to CA instead of a fork ("Adding a custom
  backdrop blur pass inside GPUI would require forking", `glass.md` §3).
- **Rune requirement:** owning the stack is the point. Kit has to be able to fix the renderer the day he finds a
  problem.

### 3.8 Aesthetics: "looks like shit" next to native

- Hex (quoted in §1.3), Flit (the same request), and Scrawl ("generally, it's a little uglier than the other one" on
  Flit's onboarding versus Scrawl's).
- What Apple frameworks gave for free: Liquid Glass, vibrancy, SF Symbols at the right weight, continuous-corner
  (`kCACornerCurveContinuous`) radii, focus rings, inactive-window dimming, native toolbars and traffic-light
  placement, sheets, Dynamic Type, system colours.
- Hex's "quiet native" GPUI branch shows what imitating that by hand costs: "unified toolbar, hairlines instead of
  boxed panels, macOS 24 pt controls, accent switches, no hover or pointer cursor outside menus, focus rings,
  inactive-window dimming, tabular figures… straight-alpha SF Symbols" (`hex-quiet-native` `eac9d51`, 1,677
  insertions).
- **Rune requirement:** a design language whose macOS and iOS defaults *are* the platform's metrics:
  - continuous corners, SF Symbols, system accent and colours;
  - inactive-window state and focus rings;
  - native window chrome hosting Rune content.

  Where a system component is better, such as `NSMenu`, the share sheet or the file picker, use it directly.

### 3.9 Motion API: GPUI has no springs, retargeting or layout animation

- Every app rebuilt springs by hand:
  - Flit: `resolve_motion_spring_physics`, `motion_spring_sample` in `src/ui/core.rs:1590-1650`, and dozens of
    `*_morph_*` fields on `Launcher` (the render-boundary map in `ses_eaab41635ffewZC1JM2bMV7ijM`).
  - Scrawl: `ui/motion.rs` and `Tune`.
  - Hex: `src/motion.rs`, 1,194 lines.
- Interruption was hand-built too. Flit samples the CA `presentationLayer` and converts it to
  `CASpringAnimation.initialVelocity` (`compute_ca_interruption_handoff`).
- **What SwiftUI gave:** "Each state change is one spring, like motion.dev: the capsule's size animates as a layout
  change, the chip highlight slides with `matchedGeometryEffect`, and chips fade and scale in and out" (2026-10-20
  04:59). `.spring(duration:bounce:)`, `.transition(.scale.combined(with: .opacity))`, `contentTransition(.numericText())`
  and velocity-preserving retargeting are all built in (`flit/native/LauncherView.swift` uses
  `.animation(.spring(duration: 0.22, bounce: 0.12), value: state.menu != nil)`).
- **But SwiftUI wasn't sufficient either:**
  - OpenCode iOS hand-wrote `SpringSolver` ("Promoted from `FlightSpring`… `retarget(to:preservingVelocity:)`") for
    photo flights and display-link mechanics.
  - Scrawl's SwiftUI pill gave up fine-grained tuning ("tuning is less fine-grained than our hand-built springs").
  - Flit's SwiftUI Settings were "really slow… It like jumps the page around" (Flit, 2026-10-20 23:31).
- **Rune requirement:**
  - Declarative implicit animation (`animate(value, spring)`), layout animation with identity (matched geometry),
    and enter/exit transitions that keep identity while exiting.
  - Every animated property is an analytic spring channel with velocity carried across retargets.
  - An imperative escape hatch for gesture-driven channels: set position and velocity, hand off on release.
  - All of it must be tunable at runtime and traceable.

### 3.10 Hot reload: values only, and never on a physical iPhone

- Today the fastest loops reload *values*, not code: Scrawl `tune.json` within 250 ms, knobs over WebSocket, and
  Psychopomp live WGSL (`PSYCHOPOMP_SHADER_DIR`).
- Rust code changes in GPUI mean a full rebuild and restart. Kit's agents route every build through HiveMind to
  another Mac.
- HotReloadKit gets code hot reload to "~100–300 ms" with state preserved, but only on simulator and Mac: "Physical
  iOS devices do not share the host filesystem or permit arbitrary unsigned `dlopen` bundles; the runtime is
  compiled out on device targets" (`hot-reload-kit/README.md` §Limitations).
- **Rune requirement:** Kit wants under 300 ms edit-to-pixels on a connected iPhone. On device that can't be native
  code injection (no JIT, no unsigned `dlopen`; this needs confirming in the hot-reload research). It has to be a data
  path:
  - view, layout, style and animation descriptions plus tunables shipped over the tuning socket and interpreted by
    the on-device runtime;
  - a full rebuild only for real logic changes.

  The knobs pairing link (`opencode://tune?u=ws://…&c=…`, restricted to private hosts) is a working transport to
  copy.

### 3.11 Verification and agent ergonomics of the hybrid stacks

- Two languages and two toolchains:
  - Flit needs Xcode on a remote Mac just to check that Swift can decode Rust's JSON
    (`hivemind run --worker pro --kind cargo -- scripts/check-native-decoding.sh`), because "Rust tests alone can't
    see Swift decode failures" (`flit/AGENTS.md`).
  - Hex's native plan needs cbindgen, an xcframework, TCA reducers and Swift 6 concurrency on top of the Rust core.
  - Scrawl's SwiftUI pill lives in a separate helper process, which then leaked into the user's own recordings
    (window exclusion had to be fixed).
- Headless renders can't see materials ("Those renders don't draw glass, vibrancy or system colours").
- Agents can't record Studio's screen. Proof video comes from the Pro under a GUI lock, or is rebuilt from timing
  logs ("No real footage… clips… rebuilt from frame timing logs").
- **Rune requirement:**
  - One language end to end.
  - Deterministic offscreen rendering that includes Rune's own materials (blur and glass approximations) for
    screenshot tests.
  - Frame traces and a queryable tree produced by the framework itself.
  - A `lab` mode that replays a scene at a time scale.

  These replace the screen-recording dependence that keeps tripping agents.

### 3.12 What GPUI did well (so Rune doesn't regress)

- Flit's warm frames, after the work in `perf-log.md`: toggle → CPU frame **0.53 ms**, emoji edit **0.85 ms**,
  processes refresh 0.82 ms (release, hidden CPU frames). Idle at **0.0% CPU** and ~89 MiB RSS.
- It runs on Linux and Windows. Hex's and Scrawl's ports are both GPUI.
- Rust-native state with GPU text and lists is fast enough for launcher-scale UI. The problems were material, motion,
  windowing, text fidelity and upstream velocity, not raw throughput.

---

## 4. Products Rune could power, and in what order

### 4.1 Candidates

| Product | UI tech today | Surfaces | Fit for Rune | Notes |
| --- | --- | --- | --- | --- |
| **Scrawl pill** (`scrawl/src/native/pill`, `scrawl-swiftpill/helper/Pill.swift`) | Rust model + hand-rolled Metal in a fixed `NSPanel` (default), or a SwiftUI helper process (`pillRenderer: "swiftui"`) | One capsule, about eight states (idle, recording, tap-reveal chips, chip → picker, hold switcher, processing, finish, cancel), SDF shape, CoreText labels, optional glass | **Excellent first target.** | Headless model exists; `scrawl lab` scenes and motion-lint config exist (`perf/pill/motion-lint.config.json`); three baselines to beat; no text input; small. |
| **Flit** actions tray, HUD toast, then launcher panel | SwiftUI panel (root, clipboard, emoji, …) + GPUI fallback (Camera, OpenCode chat, composers) + CA glass morph | Lists (200+ rows, emoji grid), search field, two-pane details, menus, morphing tray, Quick Look, HUD | **Very good second target.** | Rust already owns all state (`core.rs` 15.6k lines; `native_view.rs` snapshot builders per view), so Rune replaces both the Swift and GPUI views. It exercises text input, virtualization, the glyph atlas and backdrop blur. |
| **Scrawl** review window, Settings, onboarding, picker overlay | GPUI (review, picker) + SwiftUI helper (Settings, Onboarding, PermissionPanel) + Metal laser overlay | Forms, keycap shortcut recorder, permission rows, full-screen overlay at 120 Hz | Good third target | Forms test the control library and accessibility. The laser overlay already shows a 120 Hz Metal overlay from Rust. |
| **Hex** HUD + desktop app | GPUI on macOS, Windows and Linux (`hex` `main`, fork by rev); a parked native AppKit/SwiftUI/TCA branch (`hex-native`) | Dictation HUD (one-shader SDF), meeting capsules, Settings (sidebar, panes, tables, keycaps), transcript views | **Strategic fourth target, Rune's reason to exist.** | Hex needs native feel on Mac *and* ships to Windows and Linux users, with an external contributor (Hona) doing those ports. That's exactly the trade-off that stalled `hex-native` at M4. |
| **OpenCode iOS** | SwiftUI + UIKit + Metal + TCA (`opencode-ios*`) | Transcript `UICollectionView` with 120 Hz scroll authority, drawer pan container, glass morphs, composer | Later (iOS) | The hardest feel bar: 120 Hz synchronous scroll, gesture arbitration, keyboard tracking. It's the right proving ground for Rune-on-iOS, but only after Mac is solid. Embedding a Rune view inside the existing app (one surface, such as `CatchUpStack` or a Metal "scope" view) is the realistic first step. |
| **OpenCode Desktop** | Electron 44 + Solid + Vite (`opencode/packages/desktop`, `packages/app`, `packages/ui`; ~70k lines of TSX) | Full IDE-like app | Last, possibly never in full | Team-owned web stack. Rune-web (WebGPU) wouldn't replace DOM text and accessibility there. Plausible entry points: a Rune-rendered canvas surface (a GPU timeline, minimap or session graph) inside the web app, or a separate native Mac shell much later. |
| Psychopomp | `wgpu` offline renderer + winit presenter | Explainer films | A peer, not a consumer | Should *share* Rune's spring, timeline and text primitives. In the long run Psychopomp's native presenter could run on Rune's renderer. |
| gpu-gallery, arcade, knobs panel | WebGPU and DOM (TS) | Canvas apps, games, tuning panels | Web-reach consumers later | Proves Kit's web stack already wants 120 Hz WebGPU. Rune-web could host knobs' panel natively. |
| Life Hub / UpDown | React web app; UpDown is SwiftUI iOS | — | Not a near-term target | — |

### 4.2 Recommended order

1. **Scrawl pill on Rune (macOS)**: proves the renderer, springs, window shell and tooling loop.
2. **Flit actions tray and HUD toast, then the launcher panel**: proves text, lists, input, backdrop materials, and
   the "Rust state → Rune view" model at app scale. Flit's GPUI fallback views (Camera with an
   `AVCaptureVideoPreviewLayer` host, the OpenCode streaming chat, multi-line composers) are the stretch goals that
   delete GPUI from Flit.
3. **Scrawl review window, Settings and onboarding**: forms, accessibility, system-control parity. This deletes
   GPUI and the helper process from Scrawl.
4. **Hex desktop + HUD on macOS, then Windows and Linux**: the multiplatform proof, replacing GPUI on `main` and
   unparking the native-quality goal.
5. **One OpenCode iOS surface embedded natively**, then whole screens, which needs iPhone hot reload working.
6. **WebGPU target** via a knobs or gpu-gallery surface; OpenCode Desktop canvas surfaces last.

### 4.3 Smallest valuable first dogfood: the Scrawl recording pill

**Why this one:**

- **Kit already said what "done" means:**
  - the SwiftUI feel ("one spring, like motion.dev", "matchedGeometryEffect highlight");
  - the island rules (`NATIVE.md`);
  - a fixed window;
  - "metal rendering layers where tasteful";
  - snappy, kit-motion palette springs, no top-edge artifacts, the red dot's breathing glow.

  There are recorded complaints to check against.
- **The behaviour is already headless:** `model.rs` decides layout (`layout_of`), phases and springs from `Tune`. The
  Metal renderer and the SwiftUI helper are both thin views over it. A Rune view drops into the same seam (the
  `pillRenderer` switch, adding `"rune"`).
- **The tooling exists for an apples-to-apples comparison:**
  - `scrawl lab pill <scene> --time-scale 0.25 --trace f.jsonl`;
  - `perf/pill/record.sh` + motion-lint baselines;
  - the pill-web WebGPU playground with a frame-time plot;
  - the dropped-frame counts under artificial load (85 → 6 is the number to beat or match).
- **It is small but touches every hard part:**
  - a non-activating fixed panel with click-through outside the shape;
  - display-link pacing on its own thread;
  - interruptible analytic springs on width, height and radius;
  - per-element blur, scale and opacity, and content gated by time-to-contact with the walls;
  - CoreText-quality labels with tabular numerals and a numeric roll;
  - SF Symbol icons;
  - an optional glass style that forces the Rune ↔ `NSGlassEffectView` compositing seam (§3.1);
  - Reduce Motion.
- **No text input, no lists, no accessibility-heavy forms.** Those are exactly what would stall a first milestone.
- **Low blast radius:** behind a config flag, with two shipping fallbacks.

**What it must prove (acceptance, all measurable with existing tools):**

1. motion-lint: 0 candidates at or above `checkMinScore` across all `scrawl lab pill` scenes, with
   `perf/pill/motion-lint.config.json` unchanged.
2. Under the same artificial main-thread load used on 2026-10-20: dropped frames no worse than the fixed Metal pill
   (≤ 6 on finish, ≤ 2 on tap → Area) and close to the SwiftUI pill (render-server immune).
3. Idle: 0 frames drawn when nothing animates, except the recording dot, which must not freeze (the 2026-10-20 bug).
4. A headless screenshot of every scene matches the real panel to a pixel tolerance, glass excluded or approximated
   (this sets up Rune's screenshot-test story).
5. A `Tune` edit shows up in the running pill in under 250 ms (parity with `tune.json`). Stretch goal: a Rust view
   edit in under 300 ms.

**Why not Flit first:** Flit's panel needs a production text field with IME, a 200-row virtualized list, a glyph
atlas that handles emoji cold starts, Quick Look responder integration, and backdrop sampling of its own content.
Each is a milestone of its own. Flit's actions tray morph alone (CA today) is a good *1b*, because it adds the
backdrop-blur pass on top of the pill's work.

---

## Steal

- The analytic `Spring::sample(MotionState, target, t)` with a settling bound (Psychopomp), parameterized as SwiftUI
  `duration` + `bounce` (motion-presets). One definition, with codegen to Swift, TS and CSS.
- Retarget semantics: inherit position and velocity; unchanged targets don't restart; plan time in `u64` ns; a
  pausable clock that freezes when settled (Psychopomp `playback.rs`).
- A global animation time scale that slows *render-time* motion. No post-hoc frame stretching (Scrawl `motion.rs`,
  OpenCode iOS `AnimationTimeScale`).
- motion-lint JSONL as Rune's native frame trace; MotionXRay's six detectors and layout-lint's rules running over
  Rune's own tree.
- knobs' remote descriptor protocol and pairing link for live tuning on Mac and iPhone; a `Tunable` type in Rune
  that publishes itself.
- The fixed max-size transparent panel, shape hit-testing, no OS window animation, shadows inside the surface, two-edge
  1/64 px snapping.
- Flit's CA glass layer recipe for the system-material backend, and its presentation-layer → `initialVelocity`
  handoff.
- CoreText rasterization into an atlas on Apple; background glyph rasterization; atlas prewarm; shaped-line cache by
  identity; sRGB layer plus dithering (gpui-fork).
- "Rust owns state; view = f(snapshot)", with text-field revision counters against echo (Flit).
- Deterministic offscreen rendering plus `verify baseline/compare` pixel diffs (Psychopomp); a scene replay `lab`
  (Scrawl).
- Design tokens enforced by a test (Hex).

## Avoid

- A JSON or FFI bridge between the Rust core and the UI. It produced invisible decode failures, a second toolchain,
  and remote-Mac-only checks.
- Driving per-frame animation from the app's main thread or by integrating wall-clock deltas.
- Resizing OS windows per frame. Letting the OS animate panel appearance.
- Numeric substep springs as the core model (knobs' 480 Hz). Fine for a panel, but not sampleable at arbitrary
  times.
- Two spring conventions (motion.dev `visualDuration` × 1.2 vs SwiftUI duration). Flit's 0.21 s ≠ SwiftUI's 0.21 s.
- Heavy widget dependencies for one input (`gpui-component`'s 71 crates).
- Headless renders that skip materials, then claiming visual verification.
- Pushing high-frequency signals (audio meters, scroll offset) through a reducer or state-diff path (lesson from
  archived `kitlangton/Hex` TCA).
- Waiting on an upstream that doesn't ship (GPUI on crates.io).

## Recommendation for Rune

1. **Treat "Rust state machine + view = f(snapshot) + animation as sampled analytic channels" as the core
   programming model.** Kit's three apps already have this shape, so porting means replacing views, not rewriting
   logic.
2. **On Apple, build the renderer and compositor to interleave with Core Animation from day one:**
   - Rune content lives in one or more `CAMetalLayer`s in a fixed transparent window.
   - System views (`NSGlassEffectView`, `AVCaptureVideoPreviewLayer`, `WKWebView`, `NSMenu`) can be placed between
     them and driven by Rune's springs inside one `CATransaction`.
   - Render on a display-link thread.
   - Optionally promote simple transform, opacity and bounds animations to `CASpringAnimation`, so they survive
     main-thread stalls.
3. **Text on Apple goes through CoreText,** with background rasterization and a stable atlas. That isn't optional
   given §3.5.
4. **Make the tooling part of the framework, not per-app glue:** motion-lint traces, a queryable tree, a time scale,
   scene replay, headless deterministic screenshots, and a knobs-protocol tuning server.
5. **Dogfood order:** Scrawl pill → Flit tray and panel → Scrawl windows → Hex (cross-platform) → OpenCode iOS
   surface → web.

**Rejected alternatives:**

- *Keep GPUI and fork it further.* Kit is already maintaining a fork for text, colour and windowing. Backdrop blur,
  render-server-grade pacing, springs and layout animation would turn it into a rewrite inside someone else's
  architecture, with no upstream releases to track.
- *Go all-native (SwiftUI/AppKit) like Ghostty.* That's the `hex-native` and Flit path. It gets Mac feel but stalls on
  Windows and Linux (Hex), doubles toolchains, and still needs custom springs, UIKit for 120 Hz scroll, and private
  APIs (OpenCode iOS). Kit's stated goal is to own the stack across platforms.
- *SwiftUI helper processes for "the smooth parts" (the Scrawl pill).* Smooth, but an extra process, coarser tuning,
  capture-exclusion bugs, and no path to other platforms.
- *Psychopomp's renderer as Rune's renderer.* Deterministic and well tested, but offline-first. It deliberately
  rejects input, focus, accessibility and window lifecycle (`PRIOR_ART.md` §GPUI). Share math, springs, timeline and
  verification instead.

## Open questions

- Can Rune sample a *Rune-rendered* backdrop for in-app glass and still compose with real Liquid Glass when the
  user picks "system" material, without the border artifacts Flit hit? Does `CABackdropLayer` (private) need a
  public fallback, as Flit's `NSGlassEffectView .clear` provides?
- How much of SwiftUI's render-server advantage can a separate render thread match, versus promoting animations to
  CA? It needs a head-to-head on the Scrawl pill under the same synthetic load.
- iPhone hot reload: what's the minimal interpreted surface (layout, style, animation, tunables) that covers most of
  Kit's edits, given no JIT and no unsigned `dlopen` on device? This is for the hot-reload research note.
- Should Rune's text use CoreText on Apple and a portable shaper (cosmic-text or swash) elsewhere, accepting
  per-platform pixel differences in screenshot tests, as Psychopomp accepts per-profile differences?
- Canonical spring: adopt SwiftUI's mapping (ω = 2π/d), and migrate Flit's motion.dev-mapped constants? Kit should
  confirm, since some Flit tokens would get about 20% faster.
- Accessibility: none of the GPUI apps expose an accessibility tree, and the SwiftUI views got it for free. Which
  dogfood target is the right one to force VoiceOver support (Scrawl Settings is the natural candidate)?

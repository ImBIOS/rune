# 03 — Flutter (reach), SwiftUI/Core Animation (feel), Rive, Figma, Lottie, Compose

Research date: 2026-10-21. Sources are linked inline. Source code read from shallow clones of
`flutter/flutter` (HEAD `77f043bc`, committed 2026-10-21) and `rive-runtime` (HEAD as of 2026-10-21)
in a scratch directory. Apple claims come from WWDC transcripts and DocC JSON. Anything marked
**(inference)** is my reading and not a documented fact. Anything marked **(estimate)** is not measured.

---

## Summary

- **Flutter's reach comes from owning every pixel behind one thin C ABI**
  ([`embedder.h`](https://github.com/flutter/flutter/blob/main/engine/src/flutter/shell/platform/embedder/embedder.h)).
  Each platform shell only supplies a surface, a vsync, input, text input and accessibility.
  Web is the exception: it is a separate engine (CanvasKit/Skwasm) that skips the C++ engine and display
  lists ([Impeller FAQ](https://github.com/flutter/flutter/blob/main/docs/engine/impeller/docs/faq.md)).
  That fork is a cost Rune should avoid by building the same Rust renderer for wasm + WebGPU, which is
  what Figma does with C++/Dawn.
- **Impeller's main lesson is to make every GPU pipeline known at build time.** Shaders are authored once
  (GLSL 4.60), compiled offline to MSL, SPIR-V or GLSL ES, and reflected into C++ structs. No shader is
  compiled at runtime, so there is no first-run jank. Impeller is now the only renderer on iOS, and the
  default on Android, macOS, Windows and Linux as of 3.47 (Impeller
  [README availability matrix](https://github.com/flutter/flutter/blob/main/engine/src/flutter/impeller/README.md)).
- **Flutter's threading converged on what GPUI already does: UI code runs on the platform main thread.**
  The "great thread merge" shipped on iOS/Android in 3.29 and on macOS/Windows in 3.35. Rasterization
  stays on its own thread.
- **The feel benchmark is not "the render server does everything".** Core Animation's out-of-process
  render server keeps `CAAnimation`s running when the main thread blocks. But Apple's modern SwiftUI
  animations do **not** create `CAAnimation`s: they evaluate springs in-process and write presentation
  values directly, off the main thread for built-in effects
  ([WWDC24 10145](https://developer.apple.com/videos/play/wwdc2024/10145/),
  [WWDC23 10156](https://developer.apple.com/videos/play/wwdc2023/10156/)). So Rune can match the feel
  with an in-process animation clock that is off the main thread and has excellent frame pacing.
  It does not need to lower everything to CA.
- **The spring model to copy is exactly Apple's `spring(duration:bounce:)`**: mass 1, k = (2π/d)²,
  ζ = 1 − bounce for bounce ≥ 0 and 1/(1 + bounce) for bounce < 0. When an animation is retargeted it
  merges and keeps its velocity. Flutter now ships the identical conversion
  (`SpringDescription.withDurationAndBounce`). Gesture handoff should come from **merging interactive
  springs**, not from a hand-computed initial velocity (WWDC24).
- **iOS scroll feel is known, published arithmetic.** Deceleration is 0.998/ms (normal) or 0.99/ms (fast).
  The rubber band is `(1 − 1/(x·0.55/d + 1))·d`. Fling projection is `v/1000 · r/(1 − r)`. Flutter
  tunes its versions by overlaying a native scroll view and fitting curves. That is a good proof loop
  to steal.
- **Liquid Glass (iOS/macOS 26) cannot be honestly reproduced by a custom renderer**, because the system
  material adapts continuously to what is behind it. It *can* be hosted: `UIGlassEffect` and
  `NSGlassEffectView` sample whatever the render server composites beneath them, including a
  `CAMetalLayer` **(inference; matches the launcher's experience, verify in a spike)**. Flutter has no
  first-party Liquid Glass after 16 months
  ([flutter#170310](https://github.com/flutter/flutter/issues/170310)). Its apps fall back to platform
  views for it.
- **Rive's renderer is the best non-Rust reference for vector quality.** It uses analytic coverage via
  tessellated, signed-winding triangle patches; a "clockwise" fill rule; and pixel-local-storage or
  atomic interlock modes. Its **vector feathering** computes soft edges as the integral of a Gaussian
  along the real path edges, using a lookup texture, with no blur pass
  ([Rive blog](https://rive.app/blog/how-rive-reinvented-feathering-for-the-vectorian-era),
  `renderer/src/shaders/draw_path_common.glsl:186`). This is the right primitive for Kit's "soft
  gradient masks, never a hard clip".
- **Figma's lessons**: one C++ renderer compiled to both wasm and native, with Dawn underneath both. A
  draw API with explicit arguments (no global binding state). Batched uniform uploads. Hot device-loss
  fallback (WebGPU→WebGL mid-session). Per-PR perf tests on real hardware
  ([Figma WebGPU](https://www.figma.com/blog/figma-rendering-powered-by-webgpu/),
  [Keeping Figma fast](https://www.figma.com/blog/keeping-figma-fast/)).
- **Animation-as-data (Lottie/Skottie, Rive state machines) gives a pure function `seek(t) → scene`.**
  That is exactly what agent verification needs: deterministic frame N on demand. Flutter's test binding
  does the same with a fake clock (`FakeAsync`, clock fixed at 2015-01-01) and a box-glyph test font
  ("Ahem"), so golden images are deterministic.

---

## 1. Flutter + Impeller (the reach benchmark)

### 1.1 Engine architecture and threads

- **Pipeline.** The framework (Dart) runs build → layout → paint and produces a `Scene` (engine-side
  `LayerTree` of container, transform, clip, opacity, backdrop-filter and picture layers). Picture
  layers hold **DisplayLists**, Flutter's recorded command buffers. The Animator posts the layer tree into
  a pipeline. The Rasterizer on the raster thread runs `Preroll` + `Paint` over the layers, and those
  dispatch DisplayList ops to Impeller (or Skia)
  ([Life of a Flutter Frame](https://github.com/flutter/flutter/blob/main/docs/engine/Life-of-a-Flutter-Frame.md)).
  Display lists are the seam where renderers can be swapped: "the engine has Skia and Impeller
  dispatchers for display lists"
  ([Impeller FAQ](https://github.com/flutter/flutter/blob/main/docs/engine/impeller/docs/faq.md)).
- **Layout** is a single depth-first pass: constraints go down, sizes go up, O(n) for the initial layout
  and sublinear for updates thanks to relayout boundaries
  ([architectural overview](https://docs.flutter.dev/resources/architectural-overview),
  [Inside Flutter § Sublinear layout](https://docs.flutter.dev/resources/inside-flutter)).
- **Threads.** Historically there were four task runners: platform (OS main), UI (Dart), raster and IO.
  **Thread merge:** from 3.29, Dart runs on the platform thread on iOS and Android ("the UI thread is
  removed"). macOS and Windows followed in 3.35
  ([breaking-change doc](https://docs.flutter.dev/release/breaking-changes/macos-windows-merged-threads),
  [flutter#150525](https://github.com/flutter/flutter/issues/150525)). A secondary source says Linux
  is still opted out by default
  ([Medium](https://medium.com/easy-flutter/flutters-great-thread-merge-what-it-means-for-your-apps-1f712d6b253d)).
  The stated reason was that split threads "prevented Flutter apps and plugins from using Dart FFI to
  interoperate with native APIs that must be called on the platform thread". The raster thread is still
  separate. *Relevance:* Rune (Rust UI on the main thread + a render thread) matches where Flutter
  ended up. Synchronous native calls on the UI thread are a feature, not a bug.
- **Warm-up frame.** Before the first vsync, the framework builds, lays out and paints once so that the
  first real frame is cheap (Life of a Flutter Frame § Warm-up frame).

### 1.2 Why Impeller replaced Skia

- **The problem was shader compilation jank.** Skia generated and compiled shaders lazily, when a draw
  type was first seen, so the first animation stuttered. The old fix (SkSL warm-up capture) was brittle.
- **Impeller's objectives**
  ([README](https://github.com/flutter/flutter/blob/main/engine/src/flutter/impeller/README.md)):
  - "All shader compilation and reflection is performed offline at build time. All pipeline state
    objects are built upfront. Caching is explicit."
  - Every GPU resource is labelled ("Instrumentable").
  - Portable across backends.
  - Spreads a frame across threads when needed.
- **Shader pipeline:** GLSL 4.60 → SPIR-V via `impellerc` → MSL, Vulkan SPIR-V or GLSL ES 1.00 → a single
  blob embedded in the binary. A reflector emits C++ structs for the uniform and vertex layouts, so
  "changes to the shader interface lead to compile time errors". This idea maps directly onto Rust:
  WGSL/naga reflection into `#[repr(C)]` structs at build time.
- **Layering:** `renderer` (backend-agnostic HAL) → `entity` (2D renderer, pass-rewriting/eliding
  optimizations) → `display_list` (DisplayList dispatcher) → `typographer` (glyph atlases only; "Impeller
  does **not** do any text layout or shaping"). It also has a `playground` test harness that opens any
  test case in a window or attaches a frame debugger. Impeller-level golden tests live in
  `impeller/golden_tests`.
- **Anti-aliasing:** 4× MSAA everywhere. SDF rendering for primitives is on by default on desktop and
  off by default on mobile ([Impeller AA](https://docs.flutter.dev/perf/antialiasing)). This is weaker
  than Rive's or Vello's analytic coverage. For example, MSAA gives four coverage levels per pixel
  unless SDFs are used.
- **Status in 2026** (README matrix, 3.47 stable):
  - iOS: ⭐ exclusive. Skia removed in 3.29.
  - Android: ✅ default, Vulkan first with a fallback to Impeller-GLES. The 2026 roadmap aims to remove
    Skia on Android 10+ entirely
    ([Roadmap 2026](https://github.com/flutter/flutter/blob/master/docs/roadmap/Roadmap.md)).
  - macOS, Windows, Linux: ✅ default as of 3.47. They were preview or experimental through 3.44.
  - Web: 🚫. CanvasKit/Skwasm remain. The FAQ says porting the web engine to Impeller is a non-goal,
    although WGSL compilation was sanity-checked.
  - Skia pieces still in use: SkParagraph for text layout, and the Skia-wrapped image codecs (attempts
    to replace them with system decoders stalled, flutter#144438 and #177863).
- **Flutter GPU** is a Dart API over Impeller's HAL (`engine/src/flutter/lib/gpu`) for custom renderers,
  with Flutter Scene for 3D. In 2026 it is still "early preview… switching to the master channel is
  strongly recommended"
  ([Flutter-GPU.md](https://github.com/flutter/flutter/blob/main/docs/engine/impeller/Flutter-GPU.md)).

### 1.3 Platform views (the cost of hosting native views)

- **Android**
  ([docs](https://docs.flutter.dev/platform-integration/android/platform-views)) has three modes:
  - **Texture Layer:** the native view renders to a texture that Impeller composites. It is "janky during
    quick scrolling" and breaks SurfaceView, accessibility and the magnifier.
  - **Hybrid Composition:** the native view stays in the Android hierarchy and Flutter renders into
    `ImageReader`-backed overlays. It "causes thread merging of raster & platform, which degrades
    Flutter FPS". Before Android 10 it also copied every frame.
  - **HCPP (experimental):** both Flutter and the native view render to native Surfaces, and
    SurfaceFlinger composites them using native transaction sync. It requires API 34+, Vulkan and
    Impeller.
- **iOS** uses hybrid composition only. Flutter slices its output into extra overlay Metal layers above
  and below each `UIView` (`FlutterPlatformViewsController.mm`, `overlay_layer_pool.mm`). Mutators
  (clips, transforms, opacity) are replayed onto the UIView. `ShaderMask` and `ColorFiltered` are not
  supported over platform views, and `BackdropFilter` is limited
  ([iOS platform views](https://docs.flutter.dev/platform-integration/ios/platform-views)). The docs
  even suggest screenshotting the native view into a texture while animating.
- **macOS:** the raster thread must synchronize with the platform thread, so "any slow or blocking
  operations on the platform thread can negatively impact Flutter graphics performance"
  ([macOS platform views](https://docs.flutter.dev/platform-integration/macos/platform-views)).
- **Lesson:** hosting native views inside a self-drawn surface is a compositing-synchronization problem
  (the GPU frame and the OS layer transaction must land in the same vsync). It is not a drawing problem.
  The correct primitive is atomic present-with-transaction:
  - iOS/macOS: `CAMetalLayer.presentsWithTransaction` or IOSurface contents in a `CATransaction`.
  - Android: SurfaceControl transactions, which is what HCPP does.
  - Windows: DirectComposition commit.

### 1.4 Semantics (accessibility)

The framework builds a parallel **SemanticsNode** tree (labels, actions, flags, rects) and sends deltas
to the engine. Each embedder maps the tree onto native accessibility:

- iOS: `SemanticsObject.mm` creates `UIAccessibilityElement`s, plus `TextInputSemanticsObject` for
  fields and `FlutterSemanticsScrollView` for scroll containers.
- Android: `AccessibilityNodeInfo`.
- Web: a DOM shadow tree.

Test pumps default to `EnginePhase.sendSemanticsUpdate`, so semantics are exercised in every widget test
(`flutter_test/lib/src/binding.dart:1454`). *Steal:* make the semantics tree a first-class output of
every frame. It doubles as the agent-queryable inspector tree.

### 1.5 Text

- **Layout/shaping:** `txt/` wraps **SkParagraph** (Skia's paragraph module over HarfBuzz + ICU). It
  replaced the original libtxt, a fork of Android's Minikin (`engine/src/flutter/txt/src/skia/paragraph_skia.cc`).
- **Rasterization:** Impeller's typographer has a single backend, `typographer/backends/skia`, which
  rasterizes glyphs into atlases.
- **Text editing is the weak spot.** On iOS, Flutter implements `UITextInput` on a hidden proxy view and
  draws caret, selection and handles itself. `FlutterTextInputPlugin.mm` is 3,403 lines and handles:
  - marked text (`kInvalidFirstRect` hacks to hide or show IME candidates);
  - the floating cursor;
  - `UITextInteraction` for some modes;
  - `UIEditMenuInteraction` for the native edit menu;
  - dictation placeholders;
  - Scribble (`UIIndirectScribbleInteraction`).

  Every new iOS text feature has to be re-implemented, and parity always lags.

### 1.6 Hot reload (why it works and what it can't do)

- **Mechanism** ([docs](https://docs.flutter.dev/tools/hot-reload)):
  1. The tool recompiles only the changed libraries, plus the path from `main` to them, into Dart
     **kernel** files.
  2. It ships them over the VM service.
  3. The VM reloads the libraries in place. The heap and all objects stay alive, and "so far no code is
     re-executed".
  4. The framework then calls `reassemble`, which rebuilds, relayouts and repaints every widget.

  The docs show "Reloaded 1 of 448 libraries in 978ms". In practice it is often 200–800 ms
  **(estimate)**. It works over USB/Wi-Fi to physical iPhones in debug (JIT) builds.
- **Why it works so well:**
  - The JIT VM can swap function bodies and class shapes under a live heap.
  - UI is a pure function of state (`build`), so re-running `build` everywhere shows the new code with
    the old state.
  - State lives in `State` objects and Elements keyed by tree position, which survive the reload.
- **What it can't do:**
  - `main()` and `initState()` are not re-run.
  - Global and static field initializers are treated as state, so they are not re-evaluated (const is
    the exception).
  - Changing an enum to a class or vice versa needs a restart, as do generic arity changes and native
    code changes.
  - `CupertinoTabView` builders don't reload.
  - Release builds are AOT and can't reload at all. The 2026 roadmap explores "interpreted bytecode in
    the Dart runtime" for ephemeral code.
- **Rust implication:** there is no VM, so the equivalent has to come from:
  1. view and state code in a dylib that is reloaded, or a wasm module, or an interpreted UI DSL;
  2. state held outside the reloaded code (an arena keyed by stable ids or tree position);
  3. a framework `reassemble()` that rebuilds everything.

  Flutter's failure cases show the design rule: **anything evaluated once at startup won't reload**, so
  keep configuration in getters or rebuild paths.

### 1.7 Tooling and tests

- **DevTools** talks to the VM service (JSON-RPC over WebSocket) and offers a widget inspector (tree,
  layout explorer), a performance view (UI/raster frame bars, timeline), a CPU profiler and memory views
  ([DevTools](https://docs.flutter.dev/tools/devtools)). In 2025–26 the same service is exposed to
  agents through the Dart/Flutter **MCP server** (roadmap 2026, [docs](https://docs.flutter.dev/ai/mcp-server)).
- **Goldens:** `matchesGoldenFile` does an exact pixel comparison (`LocalFileComparator`) and writes
  master/test/diff/masked-diff images on failure (`flutter_test/lib/src/_goldens_io.dart`).
  `--update-goldens` regenerates them. Determinism comes from:
  - the **Ahem** test font (every glyph is a square), via `--use-test-fonts`;
  - `FakeAsync` with a fixed clock (`DateTime.utc(2015)`, `binding.dart:2687-2689`);
  - `pump(duration)`, which advances virtual time.
- **Device tests:** `integration_test` runs widget-test APIs on a real device. It replaced most
  `flutter_driver` use, which was a remote-control protocol.
- **Impeller playground:** any test fixture can open interactively or attach Xcode/RenderDoc captures.

### 1.8 What made Flutter reach six platforms

1. **It owns the pixels.** One renderer and one text stack give identical output everywhere, so a new
   platform means implementing an embedder, not a widget set.
2. **A stable C embedder API** (`embedder.h`). Third parties ship their own embedders (automotive,
   embedded Linux, flutter-pi), and the 2026 roadmap wants new platforms to be authored "out-of-tree".
3. **The same Dart in two compilation modes:** JIT for development (hot reload) and AOT for release.
4. **Platform channels and FFI** for native APIs. After the thread merge, FFI calls into main-thread-only
   APIs are synchronous.
5. **Adaptive but self-drawn design systems** (Material, Cupertino), which are now being decoupled into
   separate packages (roadmap 2026).

### 1.9 What still feels non-native

- **Scroll physics are approximations.** `BouncingScrollPhysics` is fitted by overlaying a native
  scroll view (`scroll_physics.dart:908`, "Use … platform_tests/scroll_overlay to test with Flutter and
  platform scroll views superimposed"):
  - friction 0.135 = 0.998¹⁰⁰⁰ (`scroll_simulation.dart:50-53`);
  - overscroll resistance `0.52·(1 − overscroll/viewport)²` (`scroll_physics.dart:826`), which is not
  Apple's 0.55 formula;
  - momentum carry `0.000816·|v|^1.967` (`:921`).
- **Gesture thresholds:** `kTouchSlop` = 18 logical px and `kMinFlingVelocity` = 50 lp/s
  (`gestures/constants.dart:65,90`). iOS's swipe hysteresis is "usually 10 points"
  ([WWDC18 803](https://developer.apple.com/videos/play/wwdc2018/803/)).
- **Text selection, IME, magnifier, context menus.** Re-implemented, as above. `SystemContextMenu` now
  uses the native menu on iOS.
- **Keyboard sync.** iOS has no per-frame keyboard callback, so Flutter animates a *hidden UIView* with
  the keyboard's curve and samples its `presentationLayer` every vsync
  (`KeyboardInsetManager.swift:22-36`). This is a clever, reusable trick for following any system
  animation.
- **New system design arrives late or never.** There is still no first-party Liquid Glass (flutter#170310).
  Packages such as `adaptive_platform_ui` and `cupertino_native` embed real UIKit controls as platform
  views.
- **ProMotion needs `CADisableMinimumFrameDurationOnPhone`** on iPhone. Flutter sets the
  `CADisplayLink.preferredFrameRateRange` to `[max/2, max]` (`VSyncClient.swift:89-104`,
  `DisplayLinkManager.swift:45-49`). It also replaced `CAMetalLayer` with **`FlutterMetalLayer`**, a
  custom IOSurface-backed `CALayer` that presents by setting `contents` inside a `CATransaction` on the
  main thread (`FlutterMetalLayer.h:10-15`, `.mm:325-363`). The comment reads "This is needed otherwise
  frame gets skipped on touch begin / end. Go figure." Even Google couldn't get stock `CAMetalLayer`
  pacing right.

### 1.10 Startup and binary size

- Old official numbers:
  - A 2021 FAQ measured a minimal release APK at about 4.3 MB (arm32) and 4.8 MB (arm64)
    **(older, from memory of the 2021 FAQ; the current FAQ only says "a few megabytes compressed")**
    ([FAQ](https://docs.flutter.dev/resources/faq#how-big-is-the-flutter-engine)).
  - The iOS demo app in Flutter 1.17 was 5.4 MB download and 13.7 MB installed
    ([app size](https://docs.flutter.dev/perf/app-size)).
- The engine tracks `hello_world_{android,ios}__compile/release_size_bytes` in CI
  ([Engine disk footprint](https://github.com/flutter/flutter/blob/main/docs/engine/Engine-disk-footprint.md)).
  I found no current public numbers for size or for cold-start time. Engine init plus isolate start is
  expensive enough that add-to-app docs recommend pre-warming a `FlutterEngine`
  ([docs](https://docs.flutter.dev/add-to-app/performance)).

---

## 2. SwiftUI + Core Animation (the feel benchmark)

### 2.1 The render loop and render server

- The five phases ([Explore UI animation hitches](https://developer.apple.com/videos/play/tech-talks/10855/)):
  1. **event**
  2. **commit**: the app updates the layer tree and "submits that to the render server"
  3. **render prepare**: in "a separate process called the render server" (backboardd on iOS,
     WindowServer on macOS)
  4. **render execute**: on the GPU
  5. **display**

  Frames are double-buffered, with a triple-buffer fallback that gives the render server an extra frame.
- **Why CA animations survive a blocked main thread:** a `CAAnimation` is committed once as a
  description (from/to/timing). The render server interpolates it every frame in its own process. The
  model layer holds the final value, and the presentation layer reflects the in-flight value.
- **Kit's launcher experience** (a 120 Hz glass morph moved to CA) is explained by this. Anything expressed
  as CA layer animations is immune to app-side hitches.

### 2.2 Implicit, additive and interruptible animation (UIKit/CA)

- **Implicit actions:** setting a layer property inside a `CATransaction` creates a default animation.
  UIView suppresses these outside animation blocks.
- **Additive by default since iOS 8**
  ([WWDC14 236 notes](https://wwdcnotes.com/documentation/wwdc14-236-building-interruptible-and-responsive-interactions/)).
  A new UIView animation on a property that is already animating no longer snaps or replaces. It adds a
  relative animation (from old target to new target) on top of the running one, so the presentation
  value stays continuous. That is how UIKit got interruptibility without reading presentation state.
- **`UIViewPropertyAnimator`** (iOS 10,
  [WWDC16 216](https://developer.apple.com/videos/play/wwdc2016/216/)) is interruptible, scrubbable
  (`fractionComplete`) and reversible, and continues with new timing parameters.
  `UISpringTimingParameters(initialVelocity:)` takes a *unit-relative* velocity (points/s ÷ distance).
- **`CASpringAnimation`:** iOS 17 added `init(perceptualDuration:bounce:)`, and
  `UIView.animate(duration:bounce:)` gives UIKit the same parameterisation
  ([WWDC23 10158](https://developer.apple.com/videos/play/wwdc2023/10158/)).

### 2.3 SwiftUI's Animation/Transaction model

- **Transactions.** State changes happen inside a `Transaction`, which carries the `Animation` and
  flags such as `disablesAnimations`. `withAnimation` sets it explicitly, and `.animation(_:value:)`
  sets it implicitly for a subtree. iOS 17 added custom `TransactionKey`s and completion criteria
  (`.logicallyComplete` uses the perceptual duration, not the settling time).
- **Animatable attributes vs. animations**
  ([WWDC23 10156](https://developer.apple.com/videos/play/wwdc2023/10156/)). The animatable data is a
  `VectorArithmetic` value. The animation is a function `(target, elapsed, context) → value?`. The
  `CustomAnimation` protocol (iOS 17) has `animate`, `velocity` and `shouldMerge`.
- **Merging.** When `shouldMerge` returns true, the new animation inherits the previous one's state and
  elapsed time and replaces it. Springs merge and **preserve velocity**. The default (false) runs both
  animations and combines their results, which is additive like UIKit
  ([shouldMerge docs](https://developer.apple.com/documentation/swiftui/customanimation/shouldmerge(previous:value:time:context:))).
  Docs for `spring(duration:bounce:blendDuration:)`: "each spring animation will be replaced by their
  successor, preserving velocity from one animation to the next."
- **Where the evaluation happens:**
  - "For built-in animatable attributes like scaleEffect, SwiftUI … is able to do this work off the
    main thread and without calling into any of your view code."
  - A custom `Animatable` view "will run body for every frame of the animation" (WWDC23 10156).
  - "The SwiftUI animation does not create a CAAnimation, but instead animates the view's layer's
    presentation values directly" (WWDC24 10145, on using SwiftUI `Animation` with UIKit in iOS 18).

  **So the state of the art is in-process, per-frame spring evaluation on a non-main animation thread.**
  It is not render-server keyframes.
- **Velocity handoff from gestures** (WWDC24 10145). During a drag, each change event starts an
  `.interactiveSpring` that retargets the previous one. On release, a normal spring is merged in, and
  "uses the velocity from the interactiveSprings, to carry the animation forward with continuous
  velocity". There is no manual velocity computation. iOS 18 brought the same behaviour to UIKit and
  AppKit (`UIView.animate(_ animation: Animation)`).
- **`matchedGeometryEffect`** (iOS 14) synchronizes geometry for views that share an id in a
  `@Namespace`. When one view is inserted and another removed in the same transaction, SwiftUI
  "interpolate[s] their frame rectangles in window space" ([docs](https://developer.apple.com/documentation/swiftui/view/matchedgeometryeffect(id:in:properties:anchor:issource:))).
  It links geometry only; the enter and exit rendering are normal transitions. Liquid Glass's
  `glassEffectID` is the same idea applied to glass morphing.

### 2.4 Springs: duration + bounce

Apple's conversion is on the "Spring Parameter Conversion" slide of WWDC23 10158. The transcript's text
version is garbled, so the formula below is derived and matches Flutter's implementation:

```
mass = 1
stiffness k = (2π / duration)²
damping  c = 4π(1 − bounce) / duration          bounce ≥ 0   (ζ = 1 − bounce)
         c = 4π / (duration · (1 + bounce))     bounce < 0   (ζ = 1/(1+bounce))
```

- "duration" is a **perceptual duration**. It is roughly the settle time, and for very bouncy springs it
  is the oscillation period. It is deliberately not the settling duration, which is unpredictable.
- Bounce ranges from −1 to 1. The default is 0 (critically damped), which is "what you get if you don't
  specify the bounce".
- Springs have been the default `withAnimation` animation since iOS 17.
- Presets: `.smooth` (no bounce), `.snappy` (small bounce), `.bouncy` (more bounce). Each takes
  `duration:` and `extraBounce:`.
- Flutter's identical implementation is in `physics/spring_simulation.dart:70-82`. Its doc comment says
  "This constructor produces the same result as SwiftUI's `spring(duration:bounce:blendDuration:)`".

Reference values (computed):

| spec | k | c / ζ |
|---|---|---|
| d=0.5, b=0 | 157.9 | 25.13 / 1.0 |
| d=0.5, b=0.15 | 157.9 | 21.36 / 0.85 |
| d=0.5, b=0.3 | 157.9 | 17.59 / 0.7 |
| Compose default `spring()` (k=1500, ζ=1) | 1500 | ≡ d≈0.162 s, b=0 |
| M3 Expressive default spatial (k=380, ζ=0.8) | 380 | ≡ d≈0.322 s, b=0.2 |

### 2.5 Scroll physics

- **Deceleration:** `UIScrollView.DecelerationRate.normal` = 0.998 and `.fast` = 0.99. Each is the
  velocity multiplier per millisecond (Flutter's comment confirms 0.998, `scroll_simulation.dart:50`).
  Velocity follows v(t) = v₀·rᵗᵐˢ.
- **Projection** (WWDC18 803 sample code): `project(v) = (v/1000)·r/(1 − r)`. With r = 0.998, a
  1000 pt/s fling travels about 499 pt. With 0.99 it travels about 99 pt. Use it to pick snap targets
  from the *projected* position, not the release position.
- **Rubber band:** `b = (1 − 1/(x·c/d + 1))·d` with c = 0.55 and d = the viewport dimension. This was
  reverse-engineered ([gist](https://gist.github.com/originell/6961057)) and is widely reused. On an
  844 pt iPhone, 100 pt of drag gives about 51.6 pt of movement and 400 pt gives about 174.5 pt. The
  release springs back with an overdamped-ish spring.
- **Scroll edge effects.** In iOS 26 the system applies a "subtle blur and fade" under bars
  (`scrollEdgeEffectStyle`, [WWDC25 323](https://developer.apple.com/videos/play/wwdc2025/323/)). That
  is Apple formalising Kit's "no hard clipped edges".

### 2.6 ProMotion frame pacing

- **`CADisplayLink.preferredFrameRateRange`** (iOS 15+) takes a min/max/preferred range. On iPhone,
  rates above 60 Hz need `CADisableMinimumFrameDurationOnPhone = true` in Info.plist (Flutter
  `DisplayLinkManager.swift:45-49`). CA animations get their rates from the system automatically.
- **`CAMetalDisplayLink`** (iOS 17, macOS 14,
  [docs](https://developer.apple.com/documentation/quartzcore/cametaldisplaylink)) is made for Metal
  renderers that "need more control over the timing window":
  - its delegate receives an update containing a drawable and a `targetPresentationTimestamp`
    ("update your animations based on the time difference between this timestamp and the previous
    timestamp");
  - `preferredFrameLatency` accepts only 1 or 2 frames, and "may be bigger … for windowed modes on
    macOS".
  - Caveat: Rive reported that `CAMetalDisplayLink` "is currently causing crashes on _some_ iOS
    devices" and fell back to `CADisplayLink`
    ([rive-ios#349](https://github.com/rive-app/rive-ios/issues/349)).
- **Adaptive-Sync on Mac** ([WWDC21 10147](https://developer.apple.com/videos/play/wwdc2021/10147/)):
  use `presentAfterMinimumDuration` / `presentAtTime` for even pacing on VRR displays. Relying on
  drawable back-pressure gives uneven frames.
- **Rule for Rune:** always animate against the *target presentation time*, not the callback time.
  The animation clock must advance by actual presented intervals, so that a dropped frame produces a
  bigger step rather than a stall. That is what keeps motion on its curve.

### 2.7 Liquid Glass (iOS/macOS 26)

- **What it is** ([WWDC25 219](https://developer.apple.com/videos/play/wwdc2025/219/)):
  - a material defined by **lensing** (refraction of the content behind it);
  - layers that "continuously adapt based on what's behind it" (shadow, tint, dynamic range,
    independent light/dark flipping);
  - morphing between control states.
  - Its rules: no glass on glass, and Regular and Clear variants are never mixed.
- **APIs:**
  - SwiftUI: `glassEffect(_:in:)`, `GlassEffectContainer`, `glassEffectID`.
  - UIKit: `UIVisualEffectView` + `UIGlassEffect` / `UIGlassContainerEffect`.
  - AppKit: `NSGlassEffectView` (`contentView`) / `NSGlassEffectContainerView` (`spacing`).
  - These are OS 26+ only ([glassEffect docs](https://developer.apple.com/documentation/swiftui/view/glasseffect(_:in:))).
- **Mechanics that matter for hosting:**
  - "This effect is achieved by sampling content from an area larger than itself. However, glass can
    not sample other glass" (WWDC25 323).
  - AppKit: put content in `contentView` so that the system applies vibrancy and legibility treatment.
    "Avoid placing the NSGlassEffectView behind your content as a sibling view"
    ([WWDC25 310](https://developer.apple.com/videos/play/wwdc2025/310/)).
- **Can a custom renderer reproduce it?** It could approximate it: an SDF capsule, backdrop
  blur/refraction with a displacement map, a rim highlight, adaptive tint from backdrop luminance. Rive
  feathering or a WGSL backdrop shader could get close. It will not track Apple's per-release tuning,
  accessibility settings (Reduce Transparency, Increase Contrast) or system-wide light response. Flutter
  contributors argue it would "not 100% match" and would lag behind
  ([flutter#170310 comment](https://github.com/flutter/flutter/issues/170310#issuecomment-2958004370)).
- **Can it host it? Yes, with constraints (inference; spike needed).**
  - The glass is a CA layer subtree. The render server samples whatever is composited below it, and a
    `CAMetalLayer`/IOSurface layer is ordinary composited content, so a Rune surface *under* a native
    glass view should refract correctly.
  - Content *on* the glass (labels, icons) must be inside the glass's `contentView` to get vibrancy. That
    means either native views, or a second Rune surface (a Metal layer) placed inside `contentView`,
    with legibility treatment applied by Rune rather than the system.
  - Glass geometry (position, size, morph) must be animated with CA or SwiftUI in the *same transaction*
    as Rune's frame. Otherwise glass and content drift by a frame. This is the same sync problem as
    Flutter's platform views.
  - In practice: drive glass shapes from Rune's layout, and commit them through `CATransaction` with
    the Metal present (`presentsWithTransaction`, or IOSurface `contents` like `FlutterMetalLayer`).
    Use the system's `GlassEffectContainer` morphing where possible.

### 2.8 What a non-Apple framework must replicate to feel native on iOS

The full checklist is in "What Rune must match" below. The core is:

- continuity of position and velocity on every interruption;
- spring-default motion with duration/bounce;
- gesture → animation handoff by merging;
- the exact scroll constants;
- 120 Hz pacing against the presentation timestamp;
- native text input and accessibility;
- system materials and transitions hosted rather than faked.

---

## 3. Rive, Figma, Lottie/Skottie

### 3.1 Rive renderer (`rive-runtime/renderer`, MIT)

- **Design.** It is a vector and raster renderer "custom-built for Rive content, for animation, and for
  runtime". Backends: Metal, Vulkan, D3D11/12, GL/WebGL2, WebGPU (`renderer/src/{metal,vulkan,d3d11,d3d12,gl,webgpu}`).
  `RenderContext` "manages all the GPU buffers… required for Rive's pixel local storage path rendering
  algorithm" (`include/rive/renderer/render_context.hpp:86-88`).
- **Paths.** Béziers are tessellated *on the GPU* (`shaders/tessellate.glsl`) into triangle patches
  around the edges. The antialiasing is described in the
  [feathering blog](https://rive.app/blog/how-rive-reinvented-feathering-for-the-vectorian-era):
  "triangle-based coverage interpolation… Depending on whether a triangle naturally falls clockwise or
  counterclockwise, it contributes positive or negative coverage to a pixel."
- **Interlock modes** (`include/rive/renderer/gpu.hpp:1073-1088`):
  - `rasterOrdering`: pixel local storage / raster order groups on Apple and Android tile GPUs;
  - `atomics`;
  - `clockwise` and `clockwiseAtomic`: these override fill rules with "clockwise" (only positive
    winding fills), which makes atomics usable without barriers;
  - `depthStencil` (MSAA fallback).

  Gradients go into a 512-wide ramp texture (`constants.glsl:61`). Advanced blend modes are implemented
  in-shader.
- **Vector feathering** (shipped February 2025):
  - "Instead of an anti-aliasing ramp that's one pixel wide, we could extend it to any width… shape that
    transition based on a normal distribution curve." The feather opacity "is the integral of the normal
    distribution function", centred on the true edge.
  - Corners and curves need a 2D integral. The shader uses a separability trick plus a precomputed
    **Gaussian-integral 1D texture** (`constants.glsl:64-73`, `GAUSSIAN_INTEGRAL_TEXTURE_STDDEVS = 3`)
    and evaluates per-corner coverage in `eval_feathered_fill` (`draw_path_common.glsl:186-204`).
  - Large feathers go through a feather atlas (`FEATHER_ATLAS_BLIT`).
  - It needs the Clockwise fill rule. NoesisGUI notes it is "tightly coupled to their own custom
    renderer" ([forum](https://www.noesisengine.com/forums/viewtopic.php?t=3718)).
- **Text:** HarfBuzz + SheenBidi shaping (`premake5_v2.lua:97-98`, `src/text/`). Glyphs render as paths
  through the same analytic AA, which is "Perfect text… crisp at any size".
- **The runtime is quietly becoming a UI framework.** It has:
  - Yoga layout (`dependencies/premake5_yoga_v2.lua`, `src/layout/`);
  - state machines with layers, blend states and transitions;
  - listeners for pointer, keyboard, focus, gamepad, plus `semantic_listener_group.cpp`;
  - data binding / view models (`src/viewmodel`);
  - Luau scripting (`src/lua`, `scripting/`);
  - a threaded command queue/server (`command_queue.cpp`, `command_server.cpp`).

  Rive's state machine is declarative animation-as-data: inputs (bool/number/trigger) or view-model
  properties drive transitions with conditions and blend durations. It is evaluated by
  `advance(dt)`, which is deterministic.

### 3.2 Figma renderer

- **Architecture** (2015, [Evan Wallace](https://www.figma.com/blog/building-a-professional-design-tool-on-the-web/)):
  - a C++ editor compiled with Emscripten (asm.js, then wasm);
  - all memory in one typed array, so there are no GC pauses;
  - a "highly-optimized **tile-based** engine with support for masking, blurring, dithered gradients,
    blend modes, nested layer opacity";
  - "our own DOM, our own compositor, our own **text layout engine**", because browser text layout
    "is inconsistent between browsers… and even… platforms". Their biggest web pain was "lack of access
    to glyph outlines and kerning tables".
- **WebGPU migration** (shipped September 2025,
  [Figma rendering: Powered by WebGPU](https://www.figma.com/blog/figma-rendering-powered-by-webgpu/)):
  1. They redesigned the graphics interface **before** touching WebGPU. Draw calls take explicit
     arguments, `context->draw(vertexBuffer, framebuffer, {texture}, material, …)`, instead of global
     binds. This "fixed a handful of bugs in our WebGL renderer before we even touched WebGPU".
  2. Uniforms are batched: `encodeDraw(uniforms, material)…submit()` uploads all uniform data to one
     buffer with offsets.
  3. Shaders stay in WebGL1 GLSL. A custom preprocessor upgrades them, then **naga** (wgpu's
     translator) emits WGSL, and layouts are reflected for the app.
  4. The same C++ renderer goes to wasm (browser WebGPU through Emscripten bindings, moving to Dawn's
     `emdawnwebgpu`) and to native x64/arm64 (server-side rendering, tests, debugging) via **Dawn**.
  5. Readback is async-only in WebGPU, so load-blocking GPU self-tests became impossible. Instead there
     is a **dynamic mid-session fallback** from WebGPU to WebGL, modelled on context-loss handling, plus
     device blocklists keyed by fallback rate.
  6. Results: "a performance improvement… on some classes of devices… neutral on others, but no
     regressions". Next up are compute-shader blur, MSAA and RenderBundles.
- **Perf process** ([Keeping Figma fast](https://www.figma.com/blog/keeping-figma-fast/)): every PR runs
  perf scenarios in under 10 min, in parallel, on GPU VMs plus a targeted real-hardware farm. They
  rejected counting CPU instructions because the bottleneck is sometimes the GPU or I/O.

### 3.3 Lottie and Skottie: animation as data

- **Lottie** is JSON exported from After Effects (Bodymovin, 2015). It is now an open spec stewarded by
  the Lottie Animation Community, with a published spec, a validator and compliance tests
  ([lottie.github.io](https://lottie.github.io/)).
- **Skottie** (Skia's player, `modules/skottie/include/Skottie.h`) does the following:
  - `Animation::Builder` parses the JSON once into an **sksg scene graph** (`sk_sp<sksg::RenderNode>`)
    plus a list of animators;
  - `seek(t)` / `seekFrame` / `seekFrameTime(t, InvalidationController*)` run the animators to mutate
    node properties and collect damage rects;
  - `render(canvas)` draws.
  - Hooks: `PropertyObserver` for runtime overrides, `MarkerObserver`, `PrecompInterceptor`,
    `ExpressionManager` (AE expressions), and a pluggable text shaper.
- **Why it matters for Rune:**
  1. The output is a pure function of t, so any frame can be rendered headlessly, which makes goldens
     of motion trivial.
  2. Designers author and engineers bind.
  3. Damage tracking falls out of property invalidation.

  Rive goes further with interactive state machines. Lottie is timeline-only.

---

## 4. Jetpack Compose (the Android feel reference, briefly)

- **Runtime.** A compiler plugin turns `@Composable` functions into calls against a **slot table**: a
  gap buffer holding groups, remembered values and keys, with positional memoization
  ([Under the hood of Compose, 2020](https://medium.com/androiddevelopers/under-the-hood-of-jetpack-compose-part-2-of-2-37b2c20c6cdd), older).
  Snapshot state tracks reads per scope, and recomposition reruns only the scopes that read changed
  state, skipping when inputs are stable. **New in 2026:** an opt-in linked-list slot table
  (`ComposeRuntimeFlags.isLinkBufferComposerEnabled`, default false) "designed to improve performance
  primarily when deleting, moving, and reordering" ([source](https://github.com/androidx/androidx/blob/androidx-main/compose/runtime/runtime/src/commonMain/kotlin/androidx/compose/runtime/ComposeRuntimeFlags.kt)).
- **Phases:** composition → layout (measure, then place, with separate restart scopes) → draw
  ([docs](https://developer.android.com/develop/ui/compose/phases)). **Deferred state reads** inside
  `Modifier.offset { }` or `graphicsLayer { }` lambdas skip composition and remeasure. That is how
  per-frame animations stay cheap, and it is a key pattern for Rune: animated values should
  invalidate *paint/transform only*, never rebuild.
- **Animation:**
  - `Animatable` (a coroutine-based single value): `animateTo` "continu[es] the current value and
    velocity", and `animateDecay` handles flings.
  - `animate*AsState` and `updateTransition` default to `spring()`, which "guarantees the continuity of
    velocity when target value changes" ([docs](https://developer.android.com/develop/ui/compose/animation/customize)).
  - `Spring` constants (`animation-core/api/current.txt:681-690`): `DampingRatioNoBouncy` 1.0,
    `LowBouncy` 0.75, `MediumBouncy` 0.5, `HighBouncy` 0.2. `StiffnessHigh` 10000, `Medium` 1500
    (the default), `MediumLow` 400, `Low` 200, `VeryLow` 50.
  - Material 3 Expressive `MotionScheme` tokens: default spatial k=380, ζ=0.8; fast spatial k=800,
    ζ=0.6; effects ζ=1 (`ExpressiveMotionTokens.kt`).
  - Compose parameterises springs by stiffness and damping ratio. Apple's duration/bounce is the more
    designer-legible API, and the two convert losslessly (table in §2.4).
- **The RenderThread analogue.** Android's HWUI RenderThread can run `RenderNodeAnimator`s, as ripples
  do, while the UI thread is blocked. That is a partial equivalent of CA's render server.

---

## What Rune must match to feel native on iOS/macOS

1. **120 Hz, paced to presentation time.**
   - iPhone needs `CADisableMinimumFrameDurationOnPhone` and a `preferredFrameRateRange` up to the
     maximum.
   - Animate against `targetPresentationTimestamp` (`CAMetalDisplayLink` on iOS 17+/macOS 14+, with a
     `CADisplayLink` fallback given Rive's crash report).
   - Use frame latency 1 when possible.
   - Drop to lower rates when idle.
2. **Present atomically with CA transactions.** If any native layer, glass or system view moves with
   Rune content, the Metal present and the CA geometry change must commit together. Learn from
   `FlutterMetalLayer`: IOSurface pool, `contents` swap inside `CATransaction`, skip stale-size frames.
3. **Springs exactly like Apple's:**
   - `spring(duration, bounce)` with default bounce 0, the formula in §2.4, perceptual-duration
     completion, and the presets smooth/snappy/bouncy.
   - Per-property independent springs that start and stop at different times.
4. **Retargeting that preserves position and velocity on every interruption:** merge semantics (springs
   replace and inherit velocity) and additive semantics for non-mergeable curves.
5. **Gesture handoff by merging:** interactive springs during a drag (short duration, tracking), then a
   final spring that inherits velocity. Choose snap targets by projected position
   (`v/1000·r/(1−r)`). Swipe hysteresis is about 10 pt.
6. **The scroll view, bit for bit:** deceleration 0.998/0.99 per ms, the 0.55 rubber band, momentum
   carry on repeated flings, the overscroll spring, paging, scroll-edge soft fade. Verify by overlaying a
   real `UIScrollView` and comparing traces (Flutter's `scroll_overlay` method).
7. **An animation clock off the main thread** for transform, opacity, clip, colour and filter, like
   SwiftUI's built-in animatable attributes. A main-thread stall should cost layout updates, not motion.
   Optionally, lower trivially describable animations to `CAAnimation` for full immunity.
8. **Layout animation with identity:** matched-geometry-style interpolation in window space across
   insert and remove, with transitions for enter and exit. Nothing pops.
9. **Soft edges as a primitive:** analytic feathered fills and masks (Rive-style Gaussian-integral
   coverage). Make scroll-edge fades and clip feathering cheap enough to be the default.
10. **Native text input and accessibility.** Implement `UITextInput`/`NSTextInputClient` properly,
    including marked text, dictation, Scribble, the floating cursor and edit menus. Expose a semantics
    tree to `UIAccessibility`/`NSAccessibility`. Follow system keyboard animations; Flutter's hidden-view
    `presentationLayer` sampling is a usable trick.
11. **Hosting, not faking, system materials and controls:** Liquid Glass via
    `UIGlassEffect`/`NSGlassEffectView` positioned by Rune's layout; system menus, sheets and share UI.

## What to steal from Flutter's reach strategy

- **Own the pixels behind a tiny embedder contract**: surface, vsync with timestamps, input, IME,
  accessibility tree, clipboard, windows. Write it down as a Rust trait plus a C ABI so third parties
  (and agents) can add platforms out of tree.
- **Precompile every pipeline** (Impeller). Use WGSL or Slang compiled at build time, reflected into
  Rust structs, with no runtime shader compilation.
- **A display-list seam** between UI and renderer, so recorded frames can be replayed headlessly,
  diffed, serialised for bug reports, and dispatched to multiple backends.
- **Semantics as a first-class frame output.** It is also the queryable inspector tree for agents.
- **Deterministic test mode:** a fake clock, a box-glyph test font option alongside real-font goldens,
  `pump(duration)`, exact-pixel goldens with diff and masked-diff artifacts, `--update-goldens`.
- **Hot reload with state kept outside the reloaded code** and a full `reassemble()` rebuild. Flutter's
  documented failure list (startup initializers, statics, type-shape changes) is the spec for what to
  design around.
- **One renderer on the web too.** Figma's architecture (same code on native and wasm, Dawn/wgpu under
  both) rather than Flutter's separate web engine.
- **Labelled GPU resources and a "playground" harness** that opens any test in a window or attaches a
  frame capture.

## What to avoid

- **Re-implementing text editing and selection.** This is Flutter's longest-running parity gap (3,400
  lines of `UITextInput` shim on iOS alone). Use the platform's text input protocols and system menus.
  Render glyphs yourself but delegate behaviour.
- **Platform views as an afterthought.** Thread merging, extra overlay layers per native view, and
  unsupported filters over native views all come from not designing for atomic CA/SurfaceControl
  composition up front.
- **Approximate scroll physics hand-tuned per platform** without a measurement loop. Constants should be
  either exact (Apple's are known) or fitted against an overlay test.
- **MSAA-only antialiasing** for UI vectors (Impeller defaults to 4× MSAA). Use analytic coverage.
- **A separate web engine.**
- **Imitating system design languages** (Cupertino imitating UIKit). They drift every year, as Liquid
  Glass shows. Host system components where "native" matters, and make Rune's own design language
  first-class everywhere else.
- **Stock `CAMetalLayer` assumptions.** Flutter needed a custom layer to avoid skipped frames, and Rive
  saw `CAMetalDisplayLink` crashes. Budget a pacing spike with frame traces on real devices.

---

## Recommendation for Rune

1. **Animation model.** Adopt Apple's semantics exactly:
   - `Spring { duration, bounce }` as the only user-facing spring type, with a stiffness/damping escape
     hatch;
   - merge-with-velocity for springs, additive composition for curves;
   - interactive-spring gesture tracking with merge-on-release;
   - perceptual-duration completion.

   Evaluate on a dedicated animation and render thread that reads a snapshot of the animatable
   properties. The main thread only commits targets. This reproduces SwiftUI's built-in behaviour and
   keeps motion alive through UI-thread hitches.
2. **Frame pacing.**
   - Darwin: `CAMetalDisplayLink` (latency 1) with a `CADisplayLink` fallback.
   - Present through a Flutter-style IOSurface `CALayer` *or* `CAMetalLayer` with
     `presentsWithTransaction` when native layers must stay in sync. Decide by spike with Instruments
     hitch traces on an iPhone Pro and a Mac with ProMotion.
3. **Renderer quality bar.** Analytic AA plus a feathered-edge primitive (Rive's approach) as table
   stakes. Feathered masks are the default clip style.
4. **Native integration.** Build first-class "host layer" nodes, whose CA layers are placed and
   animated in the same transaction as Rune frames, from day one. Spike Liquid Glass hosting (Metal
   content under `UIGlassEffect`, Rune-driven glass geometry) before committing to any custom glass.
5. **Testing.** Deterministic headless mode (fake clock, seekable animations, display-list dumps,
   golden PNGs with diff artifacts) plus a scroll/spring overlay harness that compares Rune traces
   against `UIScrollView` and `CASpringAnimation` traces numerically.

**Rejected alternatives**

- *Lower all motion to Core Animation (CA-as-renderer).* It gives perfect hitch immunity, but it ties
  the renderer to Apple, prevents custom shaders and feathering inside animated content, and doesn't
  port. Apple's own SwiftUI no longer works this way.
- *Wrap Flutter or Impeller.* Brief says own the stack. Impeller's MSAA/SDF quality is below the bar,
  and Dart is the wrong language.
- *Use Rive's renderer directly.* It is MIT and high quality, but it is C++ and specialised for `.riv`
  content. Port the algorithms (clockwise fill, analytic feathering) into Rune's Rust renderer, or
  compare against Vello (see other reports).
- *A Cupertino-style imitation widget set.* Ruled out by the Liquid Glass lesson.

## Open questions

1. Does a `CAMetalLayer` (or IOSurface layer) under `UIGlassEffect`/`NSGlassEffectView` refract with
   full fidelity, and can glass geometry be animated in lockstep with Metal presents at 120 Hz?
   (Spike on iOS 26 and macOS 26.)
2. How stable is `CAMetalDisplayLink` on current iOS (26.x)? Is the Rive crash still reproducible?
3. Does SwiftUI's off-main-thread evaluation survive a fully blocked main thread, or does it only avoid
   calling view code? Measure with a deliberate 200 ms main-thread stall during `.spring` scaleEffect,
   versus a `CASpringAnimation`.
4. Are Apple's exact rubber-band and paging constants unchanged in iOS 26? Measure by tracing
   `UIScrollView.contentOffset` per frame and fitting.
5. What is the cost of analytic feathering vs. a separable blur at UI scale (hundreds of soft-edged
   rects at 120 Hz on an A-series GPU)? Benchmark Rive's renderer `path_fiddle` vs. Vello on a reserved Mac.
6. Hot reload on a physical iPhone without a JIT: is a wasm UI module, an interpreted DSL, or dylib
   reload viable under iOS code-signing rules in debug builds? (Flutter relies on the Dart VM's JIT in
   debug.)

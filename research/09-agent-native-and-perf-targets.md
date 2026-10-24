# Agent-native tooling and performance targets for Rune

Research date: 2026-10-21. Reader: Kit and the Rune architecture team. This report supports a decision about the first tooling interfaces and the workloads that should define “fast.” All Rune interfaces and budgets below are proposals, not implemented features or measured results.

## Summary

- Make the scene runner, inspector protocol, clock, and trace schema part of the framework's first architecture. Adding them after applications exist makes deterministic verification much harder.
- Give agents stable semantic IDs, small tree queries, computed geometry, presentation values, source locations, and structured errors. A screenshot alone cannot explain which constraint, state update, or animation caused a failure.
- Use deterministic offscreen rendering for correctness and dedicated devices for performance. Virtual time can prove a spring's continuity; it cannot prove that the GPU presented its frames on time.
- Reuse Scrawl's motion-trace JSONL format, Scrawl's named scene labs, the layout linter's invariant checks, and knobs' remote parameter descriptors. Keep a richer internal trace so the compatibility format does not constrain the renderer.
- Published startup, memory, and size comparisons are useful only with their measurement boundaries attached. One widely cited toolkit benchmark measures container memory rather than process RSS, and its dependency sizes are Docker image deltas.
- Current small Rust app measurements put stripped executables roughly in the 4–14 MiB range for GPUI, egui, Iced, Slint, and Tauri. These are small todo apps on one Mac, not a universal hello-world baseline.
- Set initial Mac goals of fresh-process first presentation p95 ≤200 ms, idle hello-app process RSS ≤50 MiB, and a stripped arm64 hello executable ≤5 MiB. These are proposed engineering budgets, not claims that every platform can already meet them.
- At 120 Hz, gate a defined workload against 8.333 ms presentation opportunities. Start with 1,000 inexpensive animated quads, a separately specified effects scene, and one million virtualized rows. An unspecified “number of animated layers” is not a useful benchmark.
- Measure physical input-to-photon on a dedicated fixture and event-to-presentation in ordinary device CI. Keep the two metrics separate. Published latency rankings change with compositor configuration, refresh rate, and measurement method.
- The <300 ms reload goal should initially cover state-preserving UI, style, asset, and tuning edits. Ordinary Rust code changes need their own rebuild budget and explicit state migration behavior.

## A. Build an interface that agents can inspect and replay

### A1. Determinism requires a complete execution contract

Playwright explicitly warns that screenshots vary with OS, browser version, hardware, power settings, and headless mode. Its screenshot assertion waits for two matching screenshots before comparing, but this is a stabilization heuristic, not a determinism guarantee. Its default perceptual threshold is 0.2. [Playwright visual comparisons](https://playwright.dev/docs/test-snapshots)

Flutter's golden matcher documents its default Ahem test font and warns that fonts can render differently across platforms and Flutter versions. Point-Free's Swift snapshot library similarly requires the same simulator for image reference comparisons. These are good reasons to pin rendering environments and retain semantic snapshots alongside pixels. [Flutter goldens](https://api.flutter.dev/flutter/flutter_test/matchesGoldenFile.html), [Swift SnapshotTesting](https://github.com/pointfreeco/swift-snapshot-testing)

**Proposed contract:** every replay artifact records the framework revision, scene revision, renderer/backend, OS image, GPU/driver, logical viewport, pixel scale, output color space, font and asset hashes, locale, text direction, seed, clock policy, and fixture versions. Fix font fallback explicitly. An unavailable font should fail the fixture instead of silently changing line wrapping.

Control these inputs through injected services:

| Input | Deterministic scene behavior | Real-device behavior |
|---|---|---|
| Monotonic clock, timers, animation tick | One manually advanced clock | Host monotonic time, with recorded display deadlines |
| Wall clock, random values, IDs | Fixed wall epoch and seeded generators | Actual services; record seeds where applicable |
| Network, files, permissions, clipboard | Named fixture responses and ordered completions | Platform integration suite |
| Async completions | Replay scheduler releases recorded completions | Normal executor; record event order |
| Fonts and images | Pinned bytes; explicit decode/upload readiness | Same assets, real upload timings |
| Window and environment | Fixed size, scale, appearance, insets, locale | Physical display and current device configuration |
| Input | Timestamped events through normal hit-testing/dispatch | OS-delivered events and real touch/HID fixture |

“Idle” must mean no pending work relevant to the requested frame. It must not mean “wait forever for all tasks,” because an app can contain polling, cursors, and perpetual animations. Expose `settle` with a bounded deadline and a diagnostic list of remaining work. Expose a separate exact-time render that deliberately permits active animations.

Use two pixel lanes:

1. **Canonical correctness:** pinned offscreen backend and environment; exact hashes where repeatable. Save expected, actual, diff, semantic tree, and layout tree on failure.
2. **Platform fidelity:** actual Metal/WebGPU/other backend with separately keyed baselines. Allow only explicitly justified regional tolerance, with changed-pixel and edge limits. Font/platform changes require their own baseline review.

Skia Gold's accepted image hashes and configuration dimensions are a useful model. It supports multiple accepted exact images as well as fuzzy/Sobel matching. Broad fuzzy thresholds can hide real damage, so they should not be the default response to nondeterministic tests. [Chromium GPU pixel tests with Gold](https://chromium.googlesource.com/chromium/src/+/main/docs/gpu/gpu_pixel_testing_with_gold.md)

A software renderer, if Rune has one, would be a useful canonical lane but cannot stand in for testing the shipping GPU renderer. Avoid committing to a second renderer solely to get screenshots before assessing the maintenance cost.

### A2. The inspector should answer causes as well as shapes

Several existing tools provide complementary pieces:

| System | Useful mechanism | Limitation or lesson |
|---|---|---|
| Flutter Inspector | Creation locations, widget/render-object inspection, constraints and layout exploration | Rune needs both logical ownership and render/layout objects, rather than pretending one tree explains everything |
| Dart VM service | Versioned JSON-RPC 2.0 over WebSocket, subscriptions, object IDs, service extensions | Make ID lifetime, version negotiation, and stream loss explicit |
| Chrome DevTools Protocol | DOM snapshot, computed CSS, full accessibility tree, input dispatch, animation controls, virtual time | Copy a coherent subset; implementing the entire web protocol would impose irrelevant semantics |
| React DevTools | Compact tree-operation messages and lazy inspection of properties | Avoid serializing every property of every node on every frame |
| SwiftUI/Xcode | Mixed SwiftUI/UIKit hierarchy inspection; invalidation diagnostics | `_printChanges` is a debugging-only, best-effort explanation, not a stable machine protocol |
| Dioxus devtools | Source keys using file/line/column/index; build/PID checks before patching | Useful patch provenance; its transport should not be mistaken for a complete geometry inspector |

Sources: [Flutter Inspector](https://docs.flutter.dev/tools/devtools/inspector), [Dart VM service specification](https://raw.githubusercontent.com/dart-lang/sdk/main/runtime/vm/service/service.md), [CDP schema](https://raw.githubusercontent.com/ChromeDevTools/devtools-protocol/master/json/browser_protocol.json), [React DevTools architecture](https://github.com/facebook/react/blob/main/packages/react-devtools/OVERVIEW.md), [SwiftUI performance](https://developer.apple.com/videos/play/wwdc2023/10160/), [Xcode hierarchy debugging](https://developer.apple.com/videos/play/wwdc2019/412/), [Dioxus devtools source](https://raw.githubusercontent.com/DioxusLabs/dioxus/main/packages/devtools/src/lib.rs).

**Proposed node record:** stable app-defined key, generation-scoped runtime ID, role/name/state, logical parent, layout parent, render parent, source location, model/layout/presentation bounds, effective clip, transform, opacity, z-order, hit-test eligibility, focus, owning state dependencies, last invalidation cause, and active animations. Properties that are expensive to compute should be requested lazily.

Keep the coordinate systems explicit: logical points versus device pixels; local, window, and screen bounds; pre-transform versus post-transform; geometry versus visible clipped geometry. A model rectangle can be correct while its presentation rectangle jumps or its effective opacity becomes zero.

Add queries for the questions an agent actually asks:

- Why is this node invisible, unclickable, clipped, or outside its parent?
- Which constraint set its width, and which ancestor supplied that constraint?
- What invalidated this subtree, and how many layout/paint passes followed?
- Which node received this input, and why did the expected node lose hit-testing?
- Which state read keeps this component subscribed, and what was the last write?
- Which animation owns this presentation property, and what happened when it retargeted?

Use accessible role/name/state queries for normal automation. Playwright's ARIA snapshots show how compact a useful semantic contract can be. Retain explicit test IDs for repeated or unnamed objects, plus stable item keys for virtualized collections. Do not use array index as list identity. [Playwright ARIA snapshots](https://playwright.dev/docs/aria-snapshots)

### A3. A small versioned protocol can serve the CLI and visual inspector

The following API is a proposed design sketch. It is not a description of an existing Rune tool. Use JSON-RPC 2.0 with generated Rust types and JSON Schema; expose stdio for a spawned scene runner and an explicitly enabled local socket/WebSocket for a running development app. Ship no listening inspector in normal release builds.

| Domain | Initial methods | Important semantics |
|---|---|---|
| Session | `hello`, `capabilities`, `subscribe` | Protocol/schema versions, build ID, scene epoch, supported operations |
| Scene | `list`, `load`, `reset`, `settle` | Named fixtures, seeded reset, bounded completion, new epoch on reset |
| Tree | `query`, `snapshot`, `inspect`, `explain` | Role/key selectors, depth limits, lazy properties, invalidation reasons |
| Input | `pointer`, `key`, `text`, `scroll`, `touch` | Timestamp, pointer identity, coordinate space, dispatched/processed frame IDs |
| Clock | `pause`, `advance`, `seek`, `setRate` | Monotonic advance; backward seek resets and replays rather than reversing physics |
| Capture | `image`, `semantics`, `layout` | Capture all three at one frame ID; explicit GPU completion/readback |
| Animation | `list`, `inspect`, `retarget` | Position, velocity, target, duration/bounce, lifecycle, property ownership |
| Trace | `start`, `stop`, `status` | Chosen channels, bounded buffers, dropped-event count, capture overhead |
| Tune | `list`, `set`, `setBatch`, `reset` | Typed descriptors, atomic batch revision, applied frame ID |

Example request and abbreviated response:

```json
{"jsonrpc":"2.0","id":17,"method":"Tree.inspect","params":{"selector":{"key":"composer.send"},"epoch":4,"fields":["source","layout","presentation","hitTest"]}}
```

```json
{"jsonrpc":"2.0","id":17,"result":{"epoch":4,"frame":73,"node":"4:208","key":"composer.send","source":{"file":"src/composer.rs","line":82},"layout":{"space":"window-points","rect":[280,640,44,44]},"presentation":{"rect":[280,648,44,44],"opacity":1},"hitTest":{"eligible":false,"reason":"covered","by":"4:219"}}}
```

Node IDs expire on removal or epoch change. A stale selection returns a structured `StaleNode` error with the observed/current epoch; it must never resolve to a newly reused allocation. Mutation requests carry the expected scene/build revision. Stream records carry sequence numbers so a client can request a fresh snapshot after a gap. Large captures return artifact handles rather than embedding megabytes of PNG data in tree messages.

The CLI is a thin client of the same protocol:

```sh
# Proposed commands, not runnable today.
rune scene list --json
rune scene run composer/send --seed 7 --clock manual --headless --out run/
rune inspect --key composer.send --fields layout,presentation,source --json
rune input pointer --phase down --x 300 --y 660 --at-ms 200
rune clock advance --ms 8.333333
rune capture --frame current --image --layout --semantics --out frame/
rune trace stop --formats native,motion-trace,perfetto --out run/
```

Use stable machine-readable stdout, diagnostics on stderr, meaningful exit status, and bounded requests. `text` injection should be separate from physical key events and IME composition so an agent does not accidentally claim keyboard/IME coverage from a direct model mutation. Semantic activation is useful for functional tests, but it does not exercise pointer routing or establish latency.

### A4. Named scenes should replay real UI with fake external services

Scrawl already has a good local example: `scrawl lab pill <scene>` replays named capsule states with a pretend recording and no mic/capture/ingest side effects. The lab supports time scaling and per-frame motion traces; tuning values reload within 250 ms. This is stronger than a gallery of inert components because it drives the transition sequence used by the app. [Scrawl's AGENTS.md](https://github.com/kitlangton/scrawl/blob/main/AGENTS.md), implementation in `scrawl/src/ui/lab.rs` and `scrawl/src/ui/motion.rs`.

Rune should let every feature register scenes next to its implementation. A scene supplies initial state, fixture services, stable keys, and an input script. Include empty/loading/error states, long text, large text settings, right-to-left layout, keyboard appearance, interruptions, rapid repeated input, resize, and background/resume when relevant.

Proposed scene manifest:

```json
{
  "schema": 1,
  "scene": "composer/send-interrupted",
  "seed": 7,
  "viewport": {"width": 390, "height": 844, "scale": 3},
  "environment": {"locale": "en-US", "appearance": "dark"},
  "fixtures": {"messages": "two-short", "send": "complete-at-800ms"},
  "steps": [
    {"atMs": 0, "input": {"activateKey": "composer.send"}},
    {"atMs": 90, "input": {"activateKey": "composer.cancel"}},
    {"atMs": 120, "capture": ["image", "layout", "semantics"]},
    {"atMs": 500, "assert": {"key": "composer.send", "visible": true}}
  ]
}
```

This functional example uses semantic activation; a sibling pointer/touch scene should verify hit-testing and gesture cancellation. All operations happen through the same dispatch and animation machinery as the app. A fake transport completes a send; it does not swap in a different presentation implementation.

Record enough input and service completion data to replay a failure. Seeded random values alone do not make concurrent task scheduling reproducible. For deterministic tests, replay ordered external completions; for real-world race detection, run separate scheduling/fuzz tests.

### A5. Time control must preserve animation semantics

All springs, delays, transitions, gesture velocity calculations, and animation lifecycle decisions should read one injected monotonic clock. Keep wall time and networking deadlines separate. Pause should freeze animation without accidentally preventing the inspector from responding.

Test a transition at exact time, at a regular 60/120 Hz sample interval, and with irregular intervals. Retarget at several points while preserving current position and velocity. Test cancellation, removal/reinsertion with the same key, and reduced motion. Verify that lifecycle events occur once and that an outgoing surface remains until its visual contribution ends.

Do not implement time scaling by simply multiplying every spring parameter or by slowing some timers but not others. Advance the shared simulation clock; report both simulation and wall time. Define whether an animation uses analytic evaluation or integration and retain that definition across replay. Large time jumps must have explicit event-processing behavior.

CDP's `Emulation.setVirtualTimePolicy` and `Animation.seekAnimations`/`setPaused`/`setPlaybackRate` demonstrate the value of separate scheduling and animation controls. Rune can provide a more coherent clock because it owns both. [CDP schema](https://raw.githubusercontent.com/ChromeDevTools/devtools-protocol/master/json/browser_protocol.json)

### A6. Reuse Kit's trace tools without corrupting timing measurements

#### Scrawl's motion-trace JSONL is the initial exchange format

The current writer has a header with `type`, `launch`, `width`, `height`, `hz`, `scale`, `wall`, and optional `platform`, `clock`, `units`, `sampleHz`. Frame records use fields including `t`, `n`, `sc`, `ct`, `m`, `e`, `g`, `cm`, `c`, `cg`, `tx`, `tf`, `k`, `bg`, `s`, and `ev`. `t` is milliseconds; `ct` is seconds. Metadata entries use `{id,cls,nm,view,host,ax,pv}` and geometry entries start `[id,x,y,w,h,opacity,radius,flags]`, optionally followed by visible-rectangle values. The writer rounds geometry to 0.01 and opacity to 0.001. Sources: the internal motion linter's writer, run discovery and README.

Important compatibility detail: virtual sweeps deliberately use `hz:60` and `sc:60*dt`. A 120 Hz virtual sample therefore uses `sc:0.5`, with sampling frequency represented separately. Do not set `hz:120` in an adapter and silently change the motion linter's normalization assumptions. Validate an adapter against actual writer output and parser fixtures.

Export stable node IDs, presentation rectangles, effective opacity, visible clips, scalar channels, and transition events. Include channels for blur, mask progress, spring position/velocity, and ownership/gating where supported. Keep full-precision values in Rune's native trace because rounded motion-trace geometry is unsuitable for some numerical continuity checks.

#### The internal layout linter supplies a useful failure model

The internal layout linter reports shared-line near misses, clipping, overflow, and off-center icons. Its candidate-fix mode applies one change at a time, remeasures, and rejects changes that break previously satisfied invariants. Its scope isolation prevents comparing a transformed device stage's internal coordinates to unrelated page chrome. (the layout linter's README.)

For Rune, expose geometry so equivalent rules can run directly on layout snapshots. Return signed deltas, node keys, relevant ancestors, source locations, and the violated invariant. Do not automatically “fix” deliberate asymmetry. Let scenes declare alignment groups, allowed overlap, intentional overflow, and hit-target constraints.

#### A motion inspector supplies presentation-level evidence

A private iOS client's tooling document was recovered from Git history (revision `632aea45`) because it is absent from the current checkout. Its motion inspector's documentation describes capture of presentation geometry, opacity, transforms, blur, masks, Core Animation springs, source locations, and tunable keys. It reports approximately 0.8–2 ms/frame capture overhead, with serialization deferred until capture stops. These are historical tooling observations, not a Rune overhead target.

Reported failures caught by this tooling include duplicate morph surfaces, gaps during opacity handoffs, premature layer retirement, geometry jumps, and stacked blur overdraw. Those cases should become named Rune regression scenes. Continuity checks need property ownership and lifecycle data as well as rectangles; otherwise an intentional exit can look like a teleport or a missing surface can simply vanish from the sample set.

#### Knobs supplies the tuning interface

Knobs' remote descriptor protocol supports typed values, spring descriptors with `{duration,bounce}`, single and batched setters, and incoming synchronization without echo. Expose the same descriptor vocabulary through `Tune.*`; acknowledge the applied revision/frame so an agent knows which capture contains its change. Keep tune keys linked to source declarations and active animation IDs. ([knobs README](https://github.com/kitlangton/knobs#readme), remote controls section.)

#### Separate correctness capture from performance capture

The internal motion linter warns that walking all layers can add milliseconds per frame, that display-link probes miss render-server/GPU lateness, and that simulator hitch results are relative. A 2 ms observer would consume 24% of a 120 Hz budget. (the motion linter's README.)

Provide three modes:

| Mode | Data | Use |
|---|---|---|
| Full deterministic | Every node/property, exact state, input, screenshots on request | Correctness and explanation; no speed claims |
| Low-overhead real time | CPU spans, GPU timestamps, frame IDs, presentation feedback, counters, selected animations | Performance gates; record overhead and dropped data |
| Focused diagnosis | Selected subtree/layers and optional screenshots | Explain a known hitch; compare with the low-overhead run |

Record input arrival → dispatch → state change → layout → paint encoding → submission → GPU execution → actual presentation where the platform exposes it. Preserve clock domains and calibration uncertainty. GPU completion is not presentation, and a display callback is not proof the submitted frame was visible.

Prefer Perfetto TrackEvent protobuf for the detailed timeline; optionally export Chrome trace JSON for easy inspection. Chrome JSON has nesting constraints and is not an unrestricted event format. Tracy is a useful optional live profiler for CPU/GPU/allocations/locks, but integrate its client instead of promising an invented universal `.tracy` exporter. [Perfetto formats](https://perfetto.dev/docs/getting-started/other-formats), [conversion guidance](https://perfetto.dev/docs/getting-started/converting), [Tracy](https://github.com/wolfpld/tracy)

### A7. Rust APIs should make common agent mistakes hard to express

Zed's own contributor guidance documents incorrect context borrowing in nested closures, nested updates that panic, dropped tasks/subscriptions, missing notification after mutation, and misuse of test clocks. These are concrete failure modes in a mature Rust UI codebase. Dioxus documents runtime borrow panics when signal borrows cross `await`, and handles whose owner has already been disposed. [Zed contributor rules](https://raw.githubusercontent.com/zed-industries/zed/main/.rules), [Dioxus signals](https://dioxuslabs.com/learn/0.7/essentials/basics/signals/)

| Failure mode | Proposed Rune response |
|---|---|
| State changes without redraw | Updates invalidate automatically within a typed transaction; expose the dependency graph |
| Runtime borrow held across `await` | Async task API takes owned input snapshots and returns a message; keep UI mutation on the UI executor |
| Reentrant entity mutation | Queue an update or return a structured diagnostic with both call sites; avoid hidden nested borrowing |
| Dropped task/subscription | Explicit owner-scoped tasks and subscriptions, with visible cancellation reason and optional deliberate detach |
| Stale callback after component removal | Generation-checked weak target; cancellation tied to owner lifetime |
| List reorder resets state or motion | Required stable item keys for stateful virtualization; duplicate-key diagnostics |
| Layout depends on hidden context | Named constraints and computed provenance available through inspector |
| Model bounds mistaken for visible bounds | Separate model/layout/presentation/visible geometry in types and protocol |
| Incorrect hot-reload assumptions | Capability report says which edits patch, migrate, or restart; preserve old runnable app on failed patch |
| Examples use a different framework version | Versioned docs, small compiling examples, generated API schema, diagnostic links to installed version |

Favor small explicit operations and typed IDs over deeply generic builder types exposed in every error. Keep engine complexity behind modules. A useful compiler/runtime error names the operation, expected owner/thread/epoch, actual value, source location, and likely repair. Avoid making agents reverse-engineer a 200-line generic mismatch to discover an expired node.

The 2026 ecosystem experiment supplies additional agent-specific observations: agents had to consult vendored source because examples/docs did not match installed versions; a feature-gated Iced timer produced an unhelpful missing-function error; webview drag defaults swallowed intended events; and the GPUI example had to hand-roll a limited text input without IME/selection/clipboard. These are documented implementation experiences, not a controlled study of LLM success rates. Rune should ship a complete standard text-input path and version-matched executable examples before treating agent code generation as solved. [Interactive experiment and caveats](https://github.com/GoldStrikeArch/rust-gui-desktop-ecosystem-state/blob/main/report/11-interactive-results.md)

Dioxus distinguishes RSX/literal/asset hot reload from Rust hotpatching and changes that require rebuilding. Rune should be equally specific. “Hot reload” must not imply arbitrary Rust type-layout changes in <300 ms. [Dioxus hot reload](https://dioxuslabs.com/learn/0.7/essentials/ui/hotreload/)

## B. Published measurements support budgets, not a universal ranking

### B1. Evidence labels and measurement boundaries

The tables distinguish **reproducible-source measurements** (published harness or raw results), **reported observations** (a concrete configuration but incomplete reproduction), **speaker claims**, and **proposed targets**. Published source does not mean this research independently reran the benchmark. No framework builds or device benchmarks were run for this report.

Keep these boundaries separate:

- Fresh process versus cache-cold start versus first launch after installation.
- App entry, window creation, first GPU submission, first presentation, and interactive readiness.
- Single-process RSS, total app/process-tree RSS, physical footprint, GPU allocations, and container memory.
- Stripped executable, resources, app bundle, compressed download, installed runtime, and system-provided dependencies.
- Software event-to-screen-sample versus software event-to-presentation versus physical input-to-photon.
- Average fps, CPU frame duration, GPU duration, and missed presentation opportunities.

### B2. Startup results span different workloads and clocks

| Source and status | Workload/configuration | Published result | What it establishes |
|---|---|---|---|
| Zed March 2026 user measurement | Zed 0.226.5, M2 Pro, macOS 15.5; launch-to-first-frame log | New window/no warmup mean 258 ±78 ms; restored 273 ±51; warm 225 ±18; clean config/warm 221 ±14 | A full editor can report first frame in a few hundred ms. Occasional 1–2 s cold launches were reported but not quantified |
| September 2026 GPUI discussion, weaker report | M4 Max, 5K/144 Hz, 100k rows, 15 fresh processes | Median/p95: GPUI 152.5/159.6 ms; egui 158.1/165.2; Slint 190.2/252.4; AppKit 155.1/165.8 | Suggests an approximately 200 ms small-app budget is plausible; no linked raw harness, not cache-cold or hello-world |
| Kalbertodt 2023, reproducible comparison | Ubuntu 20.04, X11/Nvidia, OBS at 60 Hz | Tauri ≈380 ms; Iced ≈230 ms; egui ≈280 ms | Older small-app screen-based observation, platform-specific and frame-quantized |
| IronOxidizer raw results and harness | Containerized Linux toolkit samples; five launches | Iced 99 ms; egui 175 ms; Tauri 470 ms; Electron 717 ms | Startup measures image stabilization after container start, not a universal launch-to-first-present metric |
| Jacob Ras 2023 app comparison | Cat-image app, iPhone 12 mini, AppLaunch measurement | SwiftUI 1.441 s; Flutter 1.608 s | A concrete mobile app comparison; not hello-world and not directly comparable to desktop logging |

Sources: [Zed launch measurements](https://github.com/zed-industries/zed/discussions/51187), [September GPUI report](https://github.com/zed-industries/zed/discussions/63832), [2023 Rust GUI comparison](https://lukaskalbertodt.github.io/2023/02/03/tauri-iced-egui-performance-comparison.html), [IronOxidizer results](https://raw.githubusercontent.com/IronOxidizer/gui-toolkit-benchmarks/main/utils/result.json), [IronOxidizer harness](https://raw.githubusercontent.com/IronOxidizer/gui-toolkit-benchmarks/main/utils/benchmark.py), [Flutter/native/KMP app and measurements](https://github.com/jacobras/flutter-vs-native-vs-kmp).

The last source also reports a Pixel 4a Android median of 634.2 ms for Flutter versus 413.1 ms native. Keep mobile OS launch policy in view; a Mac process-start goal should not become an unexplained iPhone cold-launch promise.

### B3. Binary sizes are available; comparable hello-world idle RSS is missing

A July 2026 AI-assisted ecosystem study publishes raw measurements for identical small todo apps on M4 Pro/24 GiB, macOS 26.5.2, rustc 1.96.1. These are useful disclosed experiments, with less independent validation than a maintained benchmark suite. The following conversions use 1 MiB = 1,048,576 bytes. [Method/results](https://github.com/GoldStrikeArch/rust-gui-desktop-ecosystem-state/blob/main/report/10-empirical-results.md), [raw CSV](https://raw.githubusercontent.com/GoldStrikeArch/rust-gui-desktop-ecosystem-state/main/measurements/results-iter1.csv)

| Framework | Stripped executable, exact bytes | MiB | Clean / incremental build, seconds | Separate active-dashboard max RSS |
|---|---:|---:|---:|---:|
| GPUI | 4,467,704 | 4.26 | 56 / 1 | 79 MiB |
| egui | 11,054,344 | 10.54 | 27 / 1 | 109 MiB |
| Iced | 8,885,624 | 8.47 | 22 / 2 | 95 MiB |
| Slint | 13,873,752 | 13.23 | 42 / 4 | 95 MiB |
| Tauri | 6,682,264 | 6.37 | 36 / 10 | 211 MiB, including helpers |

Versions were GPUI 0.2.2, egui/eframe 0.35.0, Iced 0.14.0, Slint 1.17.1, and Tauri 2.11.5. In particular, the published GPUI crate does not represent every change on Zed's current main branch. Profiles used framework defaults with `strip -Sx`, rather than identical custom LTO settings.

The RSS column is the maximum over a 30-second sample of a different workload, a 10 Hz dashboard. The report explicitly did not retain a controlled idle dataset; webview helper attribution was best-effort. Do not call these idle hello-world numbers. Build caches had a warm registry but clean targets; incremental measurements after touching a source file do not prove the cost of a meaningful semantic edit. [Interactive results](https://github.com/GoldStrikeArch/rust-gui-desktop-ecosystem-state/blob/main/report/11-interactive-results.md)

Other requested frameworks and older comparison data:

| Framework/source | Available measurement | Why it does not fill the hello-world idle-RSS gap |
|---|---|---|
| Electron, IronOxidizer | Container memory 192,814 KiB; source/executable field 1 KiB; dependency-image delta 1,327,205 KiB | The 1 KiB entry is not a shipped Electron app. Docker image delta is not runtime distribution size |
| Iced, same harness | Container memory 47,397 KiB; executable 4,496 KiB | Container memory includes more than process RSS |
| egui, same harness | Container memory 52,100 KiB; executable 10,597 KiB | Same restriction; versions/platform differ from the Mac table |
| Tauri, same harness | Container memory 109,117 KiB; executable 4,918 KiB | Same restriction, including WebKit/runtime accounting |
| SwiftUI, Jacob Ras 2023 | Thinned iOS app 1.7 MB | Real cat-image app; system SwiftUI dependencies are not bundled |
| Flutter, same app | Thinned iOS app 17.9 MB | Same app/platform comparison is meaningful, but not a framework-only size |
| Makepad, 2022 talk | Desktop ZIP ≈1 MB; web Brotli ≈500 KB; UI construction ≈1 ms | Speaker claims about a particular demo; ZIP is not executable size and construction time is not launch-to-present |
| Makepad, 2025 Linux/Nvidia issue | User reported ≈300 MB for an empty release window and idle CPU activity | Bug report, not a current baseline; September 2026 closure reports idle fixes and lazy font loading |

Sources: [IronOxidizer raw data](https://raw.githubusercontent.com/IronOxidizer/gui-toolkit-benchmarks/main/utils/result.json), [measurement code](https://raw.githubusercontent.com/IronOxidizer/gui-toolkit-benchmarks/main/utils/benchmark.py), [SwiftUI/Flutter comparison](https://github.com/jacobras/flutter-vs-native-vs-kmp), [Makepad talk](https://gitnation.com/contents/makepad-leveraging-rust-wasm-webgl-to-build-amazing-cross-platform-applications), [Makepad issue #787](https://github.com/makepad/makepad/issues/787).

The critical IronOxidizer source expression is `container.stats()["memory_stats"]["usage"]`. Relabeling this field as RSS would create a false comparison. There is insufficient evidence here for a complete, current, same-device hello-world idle-RSS ranking of GPUI, egui, Iced, Slint, Tauri, Electron, SwiftUI, Flutter, and Makepad.

### B4. Input latency depends strongly on how it is measured

Typometer injects OS keyboard events and samples pixels. It measures a useful software path but excludes the physical input device and display scanout. A camera or photodiode covers a different interval. [Typometer implementation and method](https://github.com/pavelfatin/typometer)

Recomputed from matklad's published CSV, 200 samples per condition, nearest-rank p95:

| App / condition | Mean | p95 |
|---|---:|---:|
| VS Code, compositor off | 17.80 ms | 31.22 ms |
| Sublime Text 3, compositor off | 7.04 ms | 8.01 ms |
| VS Code, compositor on | 36.97 ms | 49.56 ms |
| Sublime Text 3, compositor on | 22.57 ms | 25.77 ms |

[Raw historical CSV](https://gist.githubusercontent.com/matklad/cf9080a2e6c600d05f12263b24e4b12e/raw/data.csv). These are older software measurements, not a current macOS or physical-input benchmark. The compositor difference is large enough to overwhelm many framework-level improvements.

Additional evidence:

- A 2022 VS Code latency experiment reports Typometer means at Windows/60 Hz of the proposed VS Code build 17.6 ms, Sublime 34.2 ms, and an Electron textarea with vsync disabled 16.7 ms. The author discusses the refresh-related floor and ultimately abandons the change because earlier DOM updates did not cause earlier browser rendering. This demonstrates why “Electron always has X ms latency” is not defensible. [VS Code PR #164192](https://github.com/microsoft/vscode/pull/164192)
- A weak 2026 user report gives M2 Pro means of Zed 26.3 ms, Sublime 25.9 ms, and VS Code 20.6 ms, with p95 36.8/33.5/31.7 ms. There is no raw harness, so use it only as contrary evidence against an assumed ranking. [User report](https://news.ycombinator.com/item?id=47959402)
- Historical 240 fps camera tests report VS Code at 116.7 ms on a Windows Dell XPS 13 and 95.8 ms on Ubuntu. A 240 fps camera quantizes observation at approximately 4.17 ms. These cannot be compared directly to Typometer numbers. [Is It Snappy](https://isitsnappy.com/)
- A June 2025 Flutter issue uses an FSR, photodiode, and Bela sampling at 44.1 kHz on an iPad M3 Pro. It reports basic Swift mean 67.8 ms, n=151, and basic Flutter 77.2 ms, n=209. Action semantics and application/network behavior matter; this is not a universal iPhone framework constant. [Physical touch benchmark](https://github.com/flutter/flutter/issues/170735)
- A recent Flutter touch-delivery issue reports smoothing delayed approximately 99% of packets by one vsync, alongside 120 Hz delivery-spread values around 0.18–0.21 ms. Delivery jitter is not end-to-end touch latency. [Flutter touch scheduling](https://github.com/flutter/flutter/issues/191199)

**Conclusion:** a reasonable Rune goal is minimal extra scheduling delay after input arrives, with physical qualification against an equivalent native control on the same device. Do not promise <8.33 ms physical touch-to-photon merely because rendering fits one 120 Hz frame.

### B5. “120 fps” needs an exact scene and actual presentation data

There is no defensible universal animated-layer count. One thousand opaque translated rectangles, one thousand reshaped text runs, and one thousand large blur surfaces are radically different GPU and CPU workloads. Report surface area, overlap, clips, effects, text shaping, resolution, scale, and how much changes per frame.

Robius's May 2024 benchmark provides source and xctrace scripts for a shadow-heavy iOS demo. On iPhone 12/13 running iOS 17.3, it reports Makepad at 60 fps versus Flutter/SwiftUI around 45 fps. These are 60 Hz devices, and the demo's bundled fonts were approximately 40 MB. It is useful evidence about an effects workload, not proof of 120 Hz capacity or minimal app size. [Robius benchmark](https://robius.rs/blog/performance-benchmarking-ios/)

The article titled “SwiftUI Scroll Performance: The 120FPS Challenge” actually performs its main testing at 60 Hz in Low Power Mode on iPhone 15 Pro. Its observations are useful, but the title alone is not a 120 Hz measurement. [Article and methodology](https://blog.jacobstechtavern.com/p/swiftui-scroll-performance-the-120fps)

At 120 Hz, the nominal interval is 8.333 ms. Frame work must leave room for the compositor and OS. Measure the observed presentation schedule and requested display rate, since ProMotion can vary refresh rate. CPU callback cadence alone cannot tell whether the screen displayed repeated frames.

### B6. One million rows is feasible only with explicit virtualization

LazyLayoutKit reports a one-million-item masonry workload on iPhone 14 Pro/A16/iOS 26.5.2 Release: 54 materialized items, 120 fps, p99 frame interval 8.34 ms, and approximately 2 μs query time during a 20-second recorded run. It also reports zero frames over 16.7 ms. The latter threshold alone would miss many 120 Hz deadline failures; use the p99 and raw presentation evidence rather than treating >16.7 ms as the 120 Hz criterion. [LazyLayoutKit](https://github.com/Dave861/LazyLayoutKit)

The same library reports a dense timeline at p99 33–38 ms with 17–31% of frames over 16.7 ms, and approximately 3.115 seconds for a 100k-text-item first measurement pass on A16. This is useful honesty: known-size arithmetic and first-pass arbitrary text measurement are different problems.

The 2026 Rust ecosystem study's 100k-row grids filter in roughly 1.5–6.1 ms across implementations, but one implementation materializes only 20 Slint rows. Neither row count nor filter time proves one-million-row 120 Hz scrolling. [Data-grid results](https://github.com/GoldStrikeArch/rust-gui-desktop-ecosystem-state/blob/main/report/16-data-grid-results.md)

Rune should benchmark fixed-height arithmetic indexing, variable heights with cached measurements, and previously unseen wrapped text separately. Require bounded realized nodes, stable focus/selection by item key, correct scroll anchoring when estimates change, and bounded work on jump-to-end. Test insertion and removal above the viewport, not just steady scrolling through immutable rows.

### B7. Large-file handling is mostly a data-model and scheduling problem

Zed reports include 100–300 MB log files taking seconds to open and a 43 MB JSON file stalling for 30–40 seconds on Windows. These are workload-specific user observations, not a controlled framework comparison. [Large log discussion](https://github.com/zed-industries/zed/discussions/52549), [JSON stall report](https://github.com/zed-industries/zed/discussions/45861)

A stronger primary source is Zed's August 2026 change to stream 1 MB blocks directly into its rope. For a 729 MB SQL file on Ryzen 5950X/Windows 11, the PR reports approximately 25% less peak memory and 28% less CPU. The release notes confirm the change shipped. [Zed PR #62748](https://github.com/zed-industries/zed/pull/62748), [Zed 1.17.2](https://zed.dev/releases/stable/1.17.2)

A separate gpui-component report describes 500 MB text consuming 3.2–5 GB. That editor component's storage/tokenization behavior should not be attributed to GPUI's renderer or to Zed. [gpui-component issue #2566](https://github.com/longbridge/gpui-component/issues/2566)

For Rune, the renderer contract is viewport-proportional work, incremental invalidation, cancellation, and bounded caches. A 100 MB editor demo also needs a streaming text store, deferred syntax work, and a long-line policy. Keep the demo's file-open budget separate from the renderer's frame budget.

## C. Proposed targets and automated measurement

### C1. Pin reference devices before calling a number a gate

Proposed qualification classes are an M2 Pro Mac with 16 GiB and a verified 120 Hz display mode, and an iPhone 14 Pro/A16. These choices are reference classes, not a claim that those exact devices are available. Pin the actual device, OS, toolchain, renderer, power mode, thermal state, viewport, and scale in the benchmark manifest. If the test hardware differs, establish a new baseline rather than relabeling its results.

Mac reference viewport: 1280×800 logical points at 2× for rendering workloads. iPhone: native full-screen viewport/scale, recorded explicitly. Use Release builds without inspector tree walks or video capture. Fonts and image assets remain fixed. The canonical offscreen lane runs on a pinned build worker; physical qualification runs on a dedicated device, never a developer's desk display.

All values in the following tables are **proposed initial budgets**. They need a first implementation and baseline before becoming release promises. Cross-framework evidence supports their order of magnitude, not a guarantee that Rune will achieve them.

### C2. Startup, footprint, and development budgets

| Metric / exact workload | Proposed target | Automated measurement |
|---|---|---|
| Mac hello app, fresh process, caches warm | First presentation p50 ≤150 ms, p95 ≤200 ms; interactive p95 ≤250 ms | External launcher timestamps spawn; correlate first actual presentation and successful input acknowledgment; 30 launches |
| Mac first launch after reboot, already installed | First presentation ≤500 ms in each of 5 scheduled qualification runs | Dedicated reboot sequence; retain every sample. Treat installer/quarantine first-run separately; 5 samples do not establish p95 |
| iPhone hello app, terminated app launch | OS-reported launch p95 ≤800 ms; app-entry-to-first-present p95 ≤250 ms | XCTest/Instruments plus app signposts, 30 launches; report both boundaries |
| Mac hello app idle | Process-tree RSS ≤50 MiB after 10 s settled; GPU allocations reported separately | Sample RSS over 30 s, include helpers; also retain physical footprint where available |
| iPhone hello app idle | Physical footprint ≤40 MiB after settling | Device memory metrics; keep GPU/resource accounting explicit; do not label footprint as RSS |
| Mac hello executable | Stripped arm64 ≤5 MiB; installed app/resources ≤10 MiB | Record executable bytes, bundle bytes, assets, debug symbols separately; system library dependencies listed |
| iPhone hello app size | Thinned installed app ≤10 MiB excluding optional content packs | Archive/export size report on release worker; separate download size |
| Settled UI without periodic app work | Zero framework animation ticks and GPU submissions during a 10 s sample | Internal counters after startup/cursor/animations disabled; report process CPU as a secondary noisy metric |
| Style/layout/assets/tuning reload, Mac and USB-connected iPhone | File-save-to-present p95 ≤300 ms; state preserved | At least 30 meaningful edits; timestamp save, patch, device receipt, apply, present; assert state/focus/scroll continuity |
| Ordinary Rust body edit in a small sample app | Warm incremental compile/link/restart p95 ≤2 s | Edit a consumed function body, not just `touch`; retain compiler/cache/linker versions; explicitly outside the <300 ms patch promise |
| Clean sample-app build, dependencies cached | ≤60 s on nominated build worker | Fixed job concurrency and cache state; this is a developer-cost budget, not a runtime benchmark |

The 5 MiB executable and 200 ms first-present goals are close to the published GPUI small-app range. The idle-memory budgets are deliberate goals in an evidence gap. The mobile launch and footprint budgets are provisional until measured on the reference iPhone. A framework may trade size against shaders, fonts, accessibility, or platform support; optional content packs must not hide dependencies required by the hello workload.

### C3. Frame, list, text, and latency budgets

For each 120 Hz scene below, run at least 60 seconds after a declared warmup and repeat five times. Target CPU UI work p95 ≤4 ms and GPU work p95 ≤4 ms, while separately requiring missed presentation opportunities <0.1% and no app-caused stall >25 ms. CPU and GPU times may overlap; neither their sum nor their individual percentiles establishes the presentation result. Report the full interval histogram and worst frame.

| Workload | Defined contents | Proposed additional target |
|---|---|---|
| Cheap animation | 1,000 24×24 pt rounded quads, independent spring-driven translation/opacity, no blur/shadow/text reshaping; fixed overlap seed | Meet the 120 Hz budget on both reference devices; report retarget cost separately |
| Effects animation | 12 overlapping 160×100 pt cards, 16 pt blur radius, gradient edge masks and shadows, fixed 3-layer maximum overlap | Meet the same budget; publish exact effects implementation and resolution. Do not extrapolate to 1,000 blur layers |
| Text/controls scene | 100 visible text rows with pinned font and mixed Unicode, selection/caret, fixed toolbar; scroll without reshaping unchanged runs | Meet the frame budget after glyph warmup; publish separate cold-glyph/upload measurements |
| One-million fixed-height rows | Generated data, 32 pt row height, stable keys, bounded overscan; fling, reverse, jump-to-end, mutate above viewport | Realized nodes O(viewport + overscan), ≤200 rows in reference viewport; list CPU p95 ≤1 ms; usable first viewport ≤100 ms after data source attach |
| One-million variable-height rows | Cached heights plus estimated unseen wrapped-text rows; fixed text corpus | List CPU p95 ≤2 ms in steady state; no eager million-row shaping; bounded scroll-anchor correction; report cold-measurement costs separately |
| 100 MB plain-text editor demo, Mac | Local UTF-8 file, normal lines, syntax/search deferred | First usable viewport p95 ≤250 ms with warm filesystem cache; complete indexing ≤2 s; incremental peak memory ≤2.5× file bytes plus measured empty-editor baseline |
| Pathological text | 100 MB single line; malformed input; very long grapheme sequence | Remain responsive and cancelable; report explicit truncation/chunking policy; never claim normal-line timings apply |
| Software input-to-presentation | OS event arrival to presentation of an identified changed pixel region | At 120 Hz, p95 ≤16.7 ms; dispatch-to-submit p95 ≤4 ms; phase-randomized input, at least 1,000 samples |
| Physical keyboard-to-photon, Mac | Dedicated HID trigger and photodiode, equivalent minimal text control | Provisional p95 ≤35 ms; compare native AppKit on same fixture; retain both distributions |
| Physical touch-to-photon, iPhone | Instrumented touch actuator/force trigger and photodiode; pointer-down visual response | Provisional p95 ≤80 ms and no more than one 120 Hz interval above equivalent native pointer-down control; do not mix touch-up action timings |

The physical budgets are intentionally looser than a frame interval because they include hardware and OS delivery. They are ambitious qualification targets, not derived claims about all iPhones. Measure drag tracking, touch-down feedback, and button activation separately. A button that activates on release can appear “slower” simply because the test includes finger dwell time.

For missed-frame accounting, infer expected opportunities from the display's actual rate and platform presentation feedback, with a documented timestamp tolerance. Do not call every interval above exactly 8.333000 ms a miss: clock quantization and refresh variation matter. Conversely, a >16.7 ms threshold misses single-slot failures at 120 Hz. Record rate changes and distinguish a system-downshifted display from an app that failed to deliver a requested frame.

### C4. Use a layered CI schedule

| When | Gate | Artifacts |
|---|---|---|
| Every relevant change | Deterministic scene replay, semantic/layout assertions, selected pixel goldens, spring continuity/lifecycle checks | Scene manifest, hashes, input log, tree diff, motion trace, image diff |
| Every renderer/state/scheduler change on reserved hardware | Defined animation/list scenes; startup warm-process samples; software latency; low-overhead trace | Raw samples, distributions, presentation timeline, CPU/GPU counters, machine manifest |
| Nightly or scheduled qualification | Mobile launch, cache-cold/reboot startup, physical input latency, long-run memory/cache growth | Fixture calibration, raw sensor data, all launch samples, memory timeline |
| Release | Binary/bundle size, cross-platform rendering checks, accessibility and IME/device integration | Size breakdown, platform baseline metadata, device reports |

Use both absolute budgets and paired baseline regressions. Initial regression policy: flag >10% deterioration when it also exceeds a metric-specific noise floor, such as 0.5 ms for frame work, 10 ms for startup, or 2 MiB for memory. These thresholds are policy proposals and must be calibrated from repeated baseline runs. Compare the same worker/device and interleave baseline/candidate runs where practical. Avoid universal percentage gates on tiny values.

No noisy retry-until-green policy: preserve the first failure, inspect thermal/background conditions, and rerun only with a stated reason. Store sample count, p50/p95/p99, worst value, and confidence/variability rather than one averaged score. Separate instrumentation-on versus instrumentation-off calibration; report dropped events. A proposed low-overhead trace budget is <0.2 ms/frame p95 observer cost, measured with paired runs. Full-tree captures have no such performance promise.

Physical latency requires hardware. Ordinary CI can automatically check event-to-presentation, but it cannot honestly claim input-to-photon without a sensor fixture. A high-speed camera can provide occasional external checks with stated frame quantization; a calibrated photodiode/HID or touch rig is better for automated repeated samples.

## Steal

- Flutter's source-linked layout inspection and separate logical/render concepts.
- Dart VM service's versioned RPC and subscriptions, CDP's explicit input/time controls, and React DevTools' compact updates with lazy property reads.
- Playwright's role-based selection and readable semantic snapshots, paired with pixels rather than substituted for them.
- Skia Gold's environment-keyed accepted images and explicit baseline review.
- Scrawl's named side-effect-free scene labs and one scaled animation clock.
- The motion inspector's presentation properties, property ownership, source/tunable links, and deferred serialization.
- The motion linter's format/rules, the layout linter's scope-aware invariants, and knobs' duration/bounce tuning descriptors.
- LazyLayoutKit's bounded materialization and its publication of workloads where performance degrades.
- Zed's streaming large-file approach and clear separation between loading, storage, and rendering work.

## Avoid

- Pixel tests that depend on arbitrary sleeps, system font fallback, network timing, or unspecified GPU/OS configurations.
- A screenshot-only “agent API,” pointer coordinates as the only selector, or a giant full-tree dump every frame.
- Animation tests that read target/model values while the visible presentation is elsewhere.
- Treating slowed/virtual motion as a hitch benchmark, or counting display callbacks as displayed frames.
- Unqualified “120 fps,” “cold start,” “idle memory,” or “binary size” claims.
- Treating a process exit, compilation success, or a zero-finding motion trace as proof that a visible surface exists.
- APIs that require remembering to notify after every mutation, hold runtime borrows across async work, or silently discard tasks.
- Promising arbitrary Rust hot reload in <300 ms without a proven compiler/runtime strategy.

## Recommendation for Rune

Build a small end-to-end vertical slice before expanding the widget catalog: a text field and send/cancel morph, a virtualized list, and the two animation workloads above. Give the slice stable identity, an injected clock, fixture services, semantic/layout/presentation snapshots, source-linked queries, and a motion-trace adapter. Use the same registered scenes in offscreen tests and the real Mac/iPhone app.

Keep scene state, interaction dispatch, layout, presentation, and backend submission observable at explicit boundaries. This supports agents and also exposes where runtime cost occurs. Add full Perfetto/performance instrumentation after frame IDs and clocks are coherent; otherwise traces will imply precision they do not possess.

Prioritize the <300 ms state-preserving UI patch path over general Rust hotpatching. Validate actual edit-to-present on a connected iPhone early, because serialization, transport, shader/asset upload, and preserved state are part of that budget. Retain a clear rebuild path for unsupported edits.

Rejected alternatives:

| Alternative | Reason to reject initially |
|---|---|
| Screenshot automation around a normal app as the only test interface | Cannot reliably control time, seed state, explain failures, or inspect invisible/retiring surfaces |
| Implement all of CDP | Large irrelevant web compatibility surface; a small generated protocol can expose Rune's actual model |
| Only a human visual inspector | Agents need queryable, bounded, structured operations; the GUI can be a client of the same protocol |
| Full layer tracing always enabled | Observer cost can consume a substantial fraction of the 8.333 ms frame budget |
| One “framework score” combining startup, memory, and fps | Hides workload boundaries and makes regressions difficult to diagnose |
| Mandatory new framework builds for this report | Published raw size data already answers the rough scale; new builds without a matched benchmark design would add cost without comparable evidence |
| One cross-platform pixel hash | Platform text and GPU differences make it brittle or force tolerances broad enough to miss defects |

## Open questions

1. Which exact Mac and iPhone can remain dedicated to performance qualification, and what display/power/thermal controls can the lab enforce?
2. Can the first renderer produce repeatable offscreen GPU pixels on a pinned worker, or does it need a small reference raster path for a subset of primitives?
3. What is the UI patch boundary: a compiled scene description, a data-driven view layer, dynamic code, or another representation? This determines what the <300 ms promise includes.
4. Which platforms expose reliable presentation feedback, and what proxy plus uncertainty must be reported elsewhere?
5. How much source/dependency provenance can remain available in development without adding per-frame allocation or invalidation cost?
6. Should the one-million-row and 100 MB editor workloads ship as framework examples or a separate benchmark application? The renderer should not inherit every editor data-model responsibility.
7. What exact effects scene best represents Scrawl's and the launcher's glass morph, including blur sampling and soft masks? The provisional 12-card benchmark must be replaced or supplemented by that real workload.
8. Does the internal motion linter's consumer need any schema extension for Rune's transform/mask/velocity data, or can its current scalar channels carry the first adapter without changes?

## Research scope and reproducibility notes

This report inspected local source/docs for internal motion and layout linters, Scrawl, knobs, and a private iOS client's historical motion tooling; read primary protocol/tooling documentation; examined published benchmark results and measurement code; and recomputed the cited Typometer CSV statistics. It did not independently reproduce the external performance measurements, run release-size builds, open GUI windows, or benchmark a desk display. No optional build time was consumed.

Repository links to `main` are mutable. Before using an external number as a release gate, pin its source revision and retain the harness, assets, raw data, and machine manifest in the benchmark artifact. Local relative links in this report point to sibling checkouts; the historical iOS document requires the Git revision given above.

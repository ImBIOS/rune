# 06 — State/reactivity model and animation as a first-class primitive

Research agent notes, 2026-10-21. Sources are cited inline. Kit's own code is cited by `repo/path:line`. Numbers marked *est.* are estimates, not measurements.

## Summary

- **State pick: a keyed retained tree rebuilt from pure view functions, with GPUI-style models and
  Xilem-style handlers.** App state lives in typed `Model<T>`s held in a store. Views are plain
  `fn(&mut Cx, ..) -> El`. Reading a model through `cx.read` subscribes that view scope. Handlers get
  `&mut T` when they run, queued between frames, so there is no `RefCell` re-entry and no
  `cx.notify()` to forget. The output is a type-erased `El` tree that is diffed by key. Fine-grained
  signals, GPUI's re-render-per-frame and immediate mode are rejected as the *primary* model (see the
  recommendation below).
- **Animation is never state.** Animated values never pass through view functions. A view states
  *targets*. The diff sees that a target changed and sends `Animate{prop, target, spring}` to a
  **render thread**. That thread owns the presented tree, the property tracks, scroll physics and
  drag bindings. When one value animates, the main thread does **zero** work: no view re-run, no
  layout, no diff. SwiftUI (attribute graph), Core Animation (render server), Compose (deferred
  draw-phase reads) and Motion (`MotionValue`s that skip React) all reach for this same split.
- **Every property is a piecewise analytic track.** A segment is `{t0, x0, v0, target, curve}`, and
  you sample position *and* velocity at any absolute time. Retargeting = sample `(x, v)` at the
  commit's presentation time, then start a new segment from there. This is psychopomp's model
  (`psychopomp/crates/psychopomp/src/timeline.rs:143-337`) generalised. It fixes the launcher's problem that Core
  Animation can't report velocity (`launcher/src/macos.rs:2735-2777` has to guess "mid-flight" from
  heights and fall back to an analytical model).
- **Springs are `Spring::new(duration, bounce)` with ω = 2π/duration and ζ = 1 − bounce**
  (1/(1+bounce) when bounce < 0). This matches SwiftUI, Flutter's `withDurationAndBounce`, knobs,
  the private iOS client and Kit's generated presets exactly. Each spring is a named token pair (enter/exit) loaded from
  Kit's motion presets, and the render thread re-resolves tokens live, so tuning
  retargets springs already in flight.
- **Layout animation sits between layout and commit.** Layout runs once, at the target. For each
  node whose frame changed, the runtime animates the **edges** (left, right, top, bottom) in
  parent space, with one spring per axis. A stationary edge gets no track, so it can't jiggle. Width
  leads by giving the X axis a faster spring. Rigid travel stays rigid. Backgrounds, borders,
  shadows and glass are parametric shapes drawn at the presented size, so there's no FLIP scale
  distortion.
- **Presence and identity are built into the runtime.** Removing a keyed node moves it to
  `Exiting{gen}`. By default it leaves layout flow (`popLayout`), plays its exit and unmounts only
  after its opacity track has *settled at 0*. If the same key comes back first, the node revives
  from where it is now. This is PresenceGate's generation guard
  (`PresenceGate.swift:12-69`) applied to every node.
- **Kit's rules become defaults, not discipline.** Direct manipulation applies instantly. Nothing
  animates on first mount. Reduced motion is applied per property class by the runtime. Both edges
  snap to 1/64 device px on the render thread. Every clip has soft edges by default; a hard clip is
  an explicit opt-out that the inspector reports. Persistent items never blur.
- **One clock, with time scaling and manual stepping.** Virtual time is `base + (host − anchor) ×
  scale` and re-anchors when the scale changes (`scrawl/src/ui/motion.rs:17-53`). Tracks are sampled
  at the frame's **target presentation timestamp**. Tests use `Clock::Manual{hz}`, and the frames
  come out bit-identical when the math uses deterministic `libm`.
- **The render thread writes motion traces natively** in Scrawl's JSONL format
  (`scrawl/src/ui/motion.rs:563-788`): the element tuple `[id,x,y,w,h,a,r,flags,(vis)]`, plus exact
  flag and intent bits (the motion linter's `src/analyze.ts:41-43`). The runtime *knows* which changes were
  intended, so the internal motion linter no longer has to infer it. Tracing costs nothing when it's off.

---

## Part 1 — State and reactivity

### 1.1 Criteria (from the brief and the task)

| Criterion | Why it matters for Rune |
| --- | --- |
| Per-frame CPU when one value animates | 120 Hz means an 8.33 ms budget. Animation must not depend on how complex the view tree is. |
| A 1M-row list | Infinite-software apps: logs, transcripts, mail. |
| Agent-writability | Explicit data flow, few silent failure modes, clear errors, fast compiles. |
| Hot-reload state preservation | Edit to pixels in under 300 ms with state kept, through code patches. |
| Borrow-checker ergonomics | Agents give up, or reach for `Rc<RefCell>`, when lifetimes fight them. |
| Test determinism | Headless rendering, golden frames, golden motion traces. |

### 1.2 The models compared

**Fine-grained signals: SolidJS, Leptos `reactive_graph`, Sycamore, Floem.**
Components run once and build a graph of signals, memos and effects. Writing a signal re-runs only
the effects that read it. Rust implementations keep signals in an arena and hand out `Copy` handles,
which removes lifetimes. Floem's `RwSignal` is just an `Id` plus phantom types
(`floem/reactive/src/signal.rs:223-228`), and Leptos 0.7 moved to arena-allocated `Copy` signals
([reactive_graph docs](https://docs.rs/reactive_graph)).
- *One value animating:* the cost is minimal: one effect runs and sets one property. In a native
  Rust renderer, though, the property write still triggers a style, layout and paint pass on the
  main thread (Floem animates through its style pass; `floem/src/animate/mod.rs`, `easing.rs:208`).
- *1M rows:* a keyed `<For>` diffs the whole source on each change (O(n)), and a signal per row
  costs memory (around 100 B or more each, *est.*, so ~100 MB). You still need virtualization; the
  signal graph doesn't help there.
- *Agents:* silent failure modes. Reading a signal outside a tracking scope gives a stale UI with no
  error. Effects can form cycles, ordering inside a batch is subtle, a disposed signal panics later,
  and `move ||` closures are everywhere.
- *Hot reload:* **the worst of the four.** State is captured in closures created by a component body
  that runs once. Patching that body means re-running it, which recreates the signals. Leptos
  hot-reloads `view!` templates, but a Rust logic change resets state
  ([cargo-leptos](https://github.com/leptos-rs/cargo-leptos)). Dioxus 0.7 keeps state across
  [Subsecond](https://github.com/DioxusLabs/dioxus/tree/main/packages/subsecond) patches precisely
  *because* it re-runs components and stores state in hooks, which is the retained model.
- *Determinism:* fine when single-threaded.

**Retained tree plus diff: React, Xilem, SwiftUI's AttributeGraph, Compose's slot table.**
View functions are re-run when their inputs change. The result is diffed against the retained tree
by structural identity plus explicit keys, and state lives in slots keyed by that identity.
- *One value animating:* naive React re-renders the component subtree every frame. That's why
  framer-motion skips React with `MotionValue`s. The serious systems take animation out of the view
  layer. SwiftUI interpolates `Animatable` attributes in the attribute graph without re-evaluating
  `body` ([WWDC23 Explore SwiftUI animation](https://developer.apple.com/videos/play/wwdc2023/10156/);
  for graph internals see [Rens Breur's reverse engineering](https://rensbr.eu/blog/swiftui-attribute-graph/)).
  Compose's `Modifier.graphicsLayer { }` lambda reads state only in the draw phase, so animating it
  skips composition and layout ([Compose phases](https://developer.android.com/develop/ui/compose/phases)).
  Compose's footgun is that reading the same value during composition silently recomposes every frame.
- *1M rows:* fine when the list is lazy (LazyColumn, `List`). Only visible rows exist as views.
- *Agents:* the best of the four. `view = f(state)` is explicit and testable. The footguns are
  React's rules of hooks (order-based identity) and deep generic view types (Xilem's
  `View<State, Action, Context>` with `build/rebuild/teardown`, `xilem/xilem_core/src/view.rs:53-90`),
  which mean long compiles and unreadable errors.
- *Hot reload:* **the best of the four.** After a patch, re-run every view; state survives because
  identity is stable (React Fast Refresh, Compose Live Edit, SwiftUI previews).
- *Borrow checker:* Xilem's handlers receive `&mut State` when they run, which is borrow-free at
  compile time. That's the nicest Rust ergonomics I found.

**GPUI: entities plus whole-view render.**
`Entity<T>` lives in `App`. `render(&mut self, window, cx)` rebuilds the element tree whenever the
view is notified. Layout (Taffy) and paint run for dirty views, and `AnyView::cached` reuses clean
subtrees (`zed/crates/gpui/src/view.rs:46,258`). As of today GPUI has analytic springs
(`crates/gpui/src/spring.rs:13-37`, stiffness/damping/mass) and `with_spring` that keeps position and
velocity per element ID (`elements/animation.rs:119-160`). But every animation frame calls
`window.request_animation_frame()`, which **notifies the current view**
(`window.rs:2742-2745`; `animation.rs:349`). So the view is re-rendered, laid out and painted on the
main thread every frame.
- *Agents:* forgetting `cx.notify()` gives a stale UI. Updating an entity while it's already being
  updated panics at runtime. Every call takes `window, cx`.
- *Hot reload:* reasonable. Entity state lives outside the code, but a changed struct layout breaks
  Subsecond-style patching.

**Immediate mode: egui, Clay.**
The whole UI is rebuilt and laid out every frame. Persistent bits sit in ID-keyed memory, such as
egui's `animate_bool`.
- *One value animating:* the full UI is rebuilt on the main thread every frame, around 1–3 ms for a
  rich app (*est.*), and nothing can be offloaded to a compositor. It costs battery on a 120 Hz
  display.
- *1M rows:* fine with manual virtualization.
- *Agents:* the simplest of the four, and state is just the app struct, which survives hot patching.
- *Gaps:* layout animation, presence and matched geometry all need retained identity and the
  previous frame's rects. Once you add those you've rebuilt a retained tree.

**Summary table** (✓ good, ~ mixed, ✗ poor):

| | One value animating | 1M rows | Agent footguns | Hot-reload state | Borrow ergonomics | Determinism |
| --- | --- | --- | --- | --- | --- | --- |
| Signals (Leptos/Floem) | ✓ (main-thread effect) | ~ | ✗ silent stale reads | ✗ | ✓ Copy handles | ✓ |
| Retained diff (SwiftUI/Compose/Xilem) | ✓ *if* animation lives below views | ✓ lazy | ✓ (hooks/generics aside) | ✓ | ✓ Xilem-style | ✓ |
| GPUI entity + render | ✗ re-render per frame | ✓ (`list` sum tree) | ~ notify/double borrow | ~ | ~ | ✓ |
| Immediate (egui/Clay) | ✗ full rebuild | ✓ manual | ✓ | ✓ | ✓ | ✓ |

### 1.3 The pick

**Retained, keyed, type-erased view tree. Model-granularity auto-tracking. Queued `&mut` handlers.
All animation below the view layer, on a render thread.**

1. **App state lives in `Model<T>`** (a `Copy` generational handle into the store, owned by the
   app). The store is the hot-reload unit: one place to snapshot, serialize and inspect.
2. **Views are plain functions returning `El`.** `El` is a type-erased builder (boxed nodes, no deep
   generics), which keeps compiles fast and errors short for agents. `cx.read(model)` returns `&T`
   and records a dependency for the current *view scope* (a component function or a list row).
   Writing a model marks its readers dirty. There's no manual notify.
3. **Handlers are `move |state: &mut T, event, tx| ..`**, queued and applied in order before the next
   build. They can't run while views are being built, so a double borrow can't happen. `tx` holds
   the transaction: animation choice, origin, extra model updates.
4. **Local state uses explicit string keys** within a scope: `cx.state("open", || false)`. Rejected:
   hook order (React's rules, a classic agent footgun) and `#[track_caller]` positions (Compose-style
   positional memoization; a hot patch shifts line numbers and loses the state). A duplicate key in
   one scope panics with the scope path in the message.
5. **Identity is the parent path plus an explicit `.key(..)`.** Keys are required on dynamic
   children (`list`, `for_each`); without one, the build fails loudly with a message saying how to
   fix it.
6. **Values that change every frame never re-run views.** Animations, scroll offsets and drag
   offsets are render-thread tracks or `Live<f32>` values (§2.4). Views state targets and
   bindings; they never see the per-frame value. If a view needs the presented geometry (hit
   testing, "fling from where it is"), `cx.presented(node)` gives a snapshot that is *not tracked*.
   This removes Compose's "read in the wrong phase" footgun by design.
7. **Finer invalidation comes later.** A `#[derive(Model)]` can record field-level reads, the way
   Swift's `@Observable` does. Model-level tracking is the v1 default because it's simpler and
   honest. Agents split models when invalidation is too coarse.

### 1.4 API sketch (as an agent would write it)

```rust
use rune::prelude::*;
use rune::motion; // generated from Kit's motion presets: SNAP, SETTLE, REFLOW, GLIDE, MORPH, ...

#[derive(Model, Default)] // Model = reflect (inspector) + serde (hot-reload migration)
pub struct Mail {
    pub threads: Vec<Thread>, // 1M is fine: only visible rows become views
    pub selected: Option<ThreadId>,
    pub query: String,
}

pub fn app(cx: &mut Cx) -> El {
    let mail = cx.model::<Mail>(); // created once, keyed by type (or cx.model_keyed("inbox"))
    let m = cx.read(mail);         // this scope re-runs when `mail` changes
    col()
        .child(
            search_field(&m.query)
                // direct manipulation: Origin::Direct, so applied instantly (Kit rule 5)
                .on_input(mail, |m, text, _tx| m.query = text),
        )
        .child(thread_list(mail, m.filtered()))
}

fn thread_list(mail: Model<Mail>, rows: impl ListSource<Item = Thread>) -> El {
    list(rows)                              // ListSource: len(), key(i), get(i); no Vec<El>
        .key(|t| t.id)
        .estimated_row_height(56.0)
        .row(move |cx, t| {                 // each row is its own tracked scope, built lazily
            let selected = cx.read(mail).selected == Some(t.id);
            thread_row(mail, t, selected)
        })
        .layout_animation(motion::REFLOW)   // siblings slide into gaps, container resizes on a spring
        .presence(Presence::pop())          // removed rows exit in place, out of flow
}

fn thread_row(mail: Model<Mail>, t: &Thread, selected: bool) -> El {
    let id = t.id;
    row()
        .padding(12)
        .fill(if selected { theme::ACCENT_SOFT } else { theme::CLEAR }) // a target, not a frame value
        .animate(Prop::Fill, motion::SNAP)                              // node default for this prop
        .child(text(&t.subject).weight(if t.unread { 600 } else { 400 })) // overflow: soft right fade
        .on_tap(mail, move |m, _ev, tx| {
            tx.animate(motion::SNAP); // opt in: a tap that selects isn't dragging the thing itself
            m.selected = Some(id);
        })
        .on_key(Key::Backspace, mail, move |m, _ev, tx| {
            tx.animate(motion::REFLOW);
            m.threads.retain(|x| x.id != id); // row exits, siblings slide
        })
}

fn disclosure(cx: &mut Cx, title: &str, body: impl FnOnce(&mut Cx) -> El) -> El {
    let open = cx.state("open", || false); // State<bool>, keyed by scope path + "open"
    col()
        .child(button(title).on_tap(open, |o, _, tx| { tx.animate(motion::SETTLE); *o = !*o; }))
        .child_if(*cx.get(open), body)     // enters and exits through presence, height reflows
        .resize(Resize::Mask { feather: 28.0 })
}
```

Remote and sync changes go through `app.apply(Origin::Remote, |m: &mut Mail| ...)`. Their default is
*animate with the node's declared spring*, because Kit animates undo, redo and remote changes, not
the user's own hand.

### 1.5 The 1M-row list

- `ListSource` is a trait (`len`, `key(i)`, `get(i)`, optional `height_hint(i)`). App data stays in the
  app's `Vec`, a database cursor or a rope. **No views and no signals per row.**
- Heights live in a **sum tree** keyed by row (GPUI's `list` does the same). That gives O(log n)
  offset↔index lookups, and unmeasured rows use estimates. Measured heights replace estimates with
  **scroll anchoring**: the first visible row keeps its screen position, which matters for streaming
  transcripts in the private iOS client.
- The main thread materializes the visible rows plus overscan (around 1.5 screens) as row scopes and
  diffs them by key: O(visible) per change, not O(n). The render thread scrolls through what is
  already there (§2.11) and publishes the offset so the main thread can top up overscan. Running out
  of rows (checkerboarding) is a hard problem, below.
- Inserting or deleting in the middle shifts indices but not keys. Presence and layout animation work
  on keys, so a deleted row exits and its visible neighbours slide.
- Rough budget (*est.*): 60 visible rows × (view build + diff + incremental layout) ≈ 0.3–0.8 ms on
  M-series when a model change touches every visible row; 0 ms per scroll frame between overscan
  top-ups.

### 1.6 Hot-reload state preservation

- **Code patch, same types** (Subsecond-style jump-table patching; see the hot-reload research doc):
  the store and every `cx.state` slot are untouched. The runtime marks every view scope dirty and
  rebuilds. The diff reconciles by key, and the render thread's tracks keep running. If a patched
  view emits new targets, they **retarget from the current position and velocity**, so you can tune
  a value while it's moving.
- **Changed model or state struct layout:** `#[derive(Model)]` includes serde. The runtime
  serializes the store and keyed slots, swaps in the new code (or restarts the process) and
  deserializes with defaults for new fields. Local `cx.state` slots with an incompatible type reset
  one at a time and log the key path.
- **Spring or token edits:** the token table is data (§2.18), so changing it needs no code patch.
- Why the alternatives lose: signals tie state to closures created by run-once component bodies;
  hook order and `#[track_caller]` identity break when a patch moves code.

### 1.7 Determinism and tests

- A single-threaded build/diff/layout pipeline. Handlers apply in queue order. A deterministic
  `HashMap` seed in test builds.
- `TestApp` runs the render thread's frame function **synchronously** on the test thread, with
  `Clock::Manual{hz: 120}`. Every frame lands at `n × 1/120 s`.
- Track math uses a deterministic `libm` (no platform `exp`/`sin` differences), so traces and PNGs
  are bit-identical on macOS and Linux CI. The extra cost is a few ns per call (*est.*).

```rust
#[test]
fn deleting_a_row_slides_siblings_and_exits_in_place() {
    let mut app = TestApp::new(app).size(390, 844).scale(3.0).hz(120);
    app.apply(Origin::Remote, |m: &mut Mail| m.threads = fixtures::threads(5));
    app.settle();                                    // runs until no track is active
    app.focus(key!("thread", 2)).press(Key::Backspace);
    let trace = app.record(|app| app.advance(ms(600))); // motion-trace JSONL, in memory
    trace.assert_clean();                            // pop-in, jump, reversal, fade-cutoff, ...
    assert!(app.node(key!("thread", 2)).is_none());  // unmounted only after its exit settled
    assert_eq!(app.node(key!("thread", 3)).unwrap().frame().y, 2.0 * 56.0);
    insta::assert_binary_snapshot!("after.png", app.png());
}
```

---

## Part 2 — Animation as a first-class primitive

### 2.1 Kit's prior art and what each one teaches

| Source | What it does | Lesson for Rune |
| --- | --- | --- |
| knobs `src/core/spring.ts:61-73` | `springMotion({duration,bounce})`: ω = 2π/d, ζ = 1−b (1/(1+b) below 0) | The canonical mapping. |
| knobs `spring.ts:1-2, 33-53` | Semi-implicit Euler in fixed 480 Hz substeps, dt clamped to 0.1 s; retarget keeps `v` | Works, but it can't be sampled at an arbitrary *t*. Rune uses closed form. |
| knobs `spring.ts:16-26` | `set(target, delay)`: delayed retargets (cascades); repeating the same target is a no-op | Delays and the no-op rule belong in the track model. |
| knobs `spring.ts:84-96` | Samples a spring into CSS `linear()` | The web backend can bake tracks into `linear()` only where WAAPI offload helps. |
| The private iOS client `SpringSolver.swift:86-90, 96-113, 168-200` (on `origin/main`; the brief's `Core/Utilities/Motion.swift` path doesn't exist, and local `main` is behind) | Closed form for under-, critically and over-damped springs; `retarget(to:preservingVelocity:)` | The exact math Rune's segment uses. Velocity override = gesture handoff. |
| The private iOS client `PresenceGate.swift:12-69` | Mounts immediately on open; unmounts only when *the latest generation's* completion fires while closed | Generation-guarded presence, in the runtime for every keyed node. |
| The private iOS client `PresenceGate.swift:71-97` | Blur forced to 0 at progress ≥ .98 (no offscreen cost at rest), but **kept until unmount on close** so the last frame doesn't flash sharp | Blur filters attach and detach on settle and unmount, never mid-transition. |
| The private iOS client `MotionDisplayLink.swift:4-11, 62-73` | One 80–120 Hz link; `dt` and `now` from `targetTimestamp` through `AnimationTimeScale` | Sample at the predicted presentation time, through the scaled clock. |
| The private iOS client `MotionConstants.swift:10-37` | Every spring and fade is a named, live-tunable token; `reducedFade` .20 s | A token table, not literals. |
| Scrawl `src/ui/motion.rs:17-53` | One clock: `base + elapsed × scale`; changing the scale re-anchors so running ages continue | Clock design, verbatim. |
| Scrawl `motion.rs:58-163, 316-360` | `Tune::DEFAULT` plus `tune.json` polled at 4 Hz in debug builds; presets; `scrawl tune rust` prints a paste-back | Live tuning loop; the source of truth stays in code. |
| Scrawl `motion.rs:551-788` | motion-trace JSONL: header, per-frame `e/m/g/c/cm/ev`, writes only what changed, `SCRAWL_MOTION_TRACE` | Rune emits the same format from the render thread. |
| Scrawl `src/native/pill/model.rs:130-200` | Closed-form critically damped step that **snaps to the target if it would cross it** | Prevents velocity overshoot, but it's a velocity cliff (the motion linter's `stop-dead`). Rune keeps C¹ by default (open question). |
| Scrawl `src/ui.rs:82-95` | `Spring::visual(d, b)` stored in a field called `response` | Naming drift: `response` means different things in psychopomp. Use `duration` everywhere. |
| The launcher `docs/motion.md:41-44` | Pill↔panel morph runs in WindowServer (CA); GPUI renders the panel **once at its final layout**; CA drives clip/scale/blur | Final-layout-once plus animated geometry is FLIP. It works at 120 Hz with zero CPU relayout. |
| The launcher `src/ui/core.rs:2425-2481` | `CaSpringSpec`; velocity **normalized by distance** for `CASpringAnimation.initialVelocity` | Keep velocities in property units; normalize only at a CA boundary. |
| The launcher `core.rs:2483-2522` | `eval_ca_spring_spec_at`: evaluates the CA spring analytically so `CAShapeLayer` paths stay within < 0.02 pt | If Rune mirrors tracks into CA (system glass), it must evaluate the same function. |
| The launcher `core.rs:2604-2705` + `macos.rs:2735-2777` | `compute_ca_interruption_handoff` (called from `src/ui.rs:1461,1695`): the analytical presented state, overridden by `presentationLayer` values only when "mid-flight" is detected by a height heuristic (`h > 34.5 && |h − analytic| < 80`) | **CA doesn't expose velocity, and presentation values are racy.** Rune's render thread owns the analytic tracks, so `(x, v)` is always exact. |
| Kit's motion presets (generated Rust) | `KitSpring{duration,bounce}`, `omega()/zeta()`; enter/exit pairs (`SETTLE` .3/.24, `REFLOW` .4/.32, `MORPH` .3/.16 → .27/.08, …) | Tokens are pairs. Exit springs run 10–25% shorter. |
| psychopomp `crates/psychopomp/src/motion.rs:43-73` | `Spring::sample(initial: MotionState{position,velocity}, target, elapsed)` | Pure sampling from `(x0, v0)`. |
| psychopomp `motion.rs:75-132` | `settling_time`: a deterministic time after which *both* error and speed stay under thresholds (envelope for ζ < 1, bisection for ζ = 1) | Precompute `settle_at` per segment, so finding idle tracks needs no sampling and no hysteresis. |
| psychopomp `timeline.rs:143-248` | Track = list of segments (`Set`/`Spring`/`Ease`); `sample_at` picks the last segment with start ≤ t | The track data structure. |
| psychopomp `timeline.rs:250-337`, `timeline/retarget.rs:62-67, 120-215` | A retarget samples the prior state at its start time; `Animate`/`Replay{from}`/`Immediate`; batches are transactional; "an unchanged Animate request retains its pending due time and existing trajectory" | Retarget semantics: transactional commits; setting the same target again is a no-op. |

### 2.2 Other frameworks

- **SwiftUI.** `withAnimation`/`Transaction` attach an animation to a *state change*, and the change
  propagates to every animatable attribute it touches. `Spring(duration:bounce:)` is the mapping
  above ([docs](https://developer.apple.com/documentation/swiftui/spring)). Springs **merge**: a
  retarget keeps velocity ([WWDC23 Animate with springs](https://developer.apple.com/videos/play/wwdc2023/10158/)).
  `CustomAnimation` (iOS 17) exposes `animate(value:time:context:)`, `velocity` and `shouldMerge`
  ([docs](https://developer.apple.com/documentation/swiftui/customanimation)). Non-merging animations
  run alongside the earlier ones and are combined. `matchedGeometryEffect` handles shared elements;
  `.transition` with `.blurReplace` covers presence. *Steal:* transactions, merge-by-default springs,
  velocity as part of the API. *Avoid:* animations evaluated on the main thread.
- **Core Animation.** A model tree and a presentation tree. Implicit animations come through actions
  and are batched by `CATransaction`. Interpolation runs out of process in the render server
  (`backboardd` on iOS, WindowServer on macOS), so a main-thread hitch doesn't stall motion. UIKit
  made animations interruptible with **additive** animations
  ([WWDC14 #236](https://developer.apple.com/videos/play/wwdc2014/236/)) and
  `UIViewPropertyAnimator` ([WWDC16 #216](https://developer.apple.com/videos/play/wwdc2016/216/)).
  `CASpringAnimation(perceptualDuration:bounce:)` arrived in iOS 17
  ([docs](https://developer.apple.com/documentation/quartzcore/caspringanimation/init(perceptualduration:bounce:))).
  *Steal:* the off-main render thread, model/presentation split, atomic transactions. *Avoid:* no
  velocity readback (the launcher's workaround above), no determinism, Apple-only.
- **Compose `Animatable`.** `animateTo(target, spec, initialVelocity = velocity)` cancels the running
  animation and continues from its current value and velocity. `animateDecay` handles flings
  ([docs](https://developer.android.com/develop/ui/compose/animation/value-based#animatable)). Deferred
  reads in `graphicsLayer {}` skip composition and layout. *Steal:* the retarget signature and phase
  separation. *Avoid:* leaving phase choice to the author.
- **Motion (framer-motion).** `layout` does FLIP with transforms plus *scale correction* for
  border-radius, shadows and children. `layoutId` handles shared elements, `AnimatePresence
  mode="popLayout"` lets exits leave layout flow, and `LayoutGroup` coordinates siblings
  ([layout docs](https://motion.dev/docs/react-layout-animations)). It computes deltas in a
  projection tree so nested elements don't drift. Note that Motion's `visualDuration` = SwiftUI
  duration / 1.2 (Kit's motion rules). *Steal:* popLayout, layoutId, projection to keep paths
  straight. *Avoid:* scale-based FLIP, which distorts text and needs corrections. Rune draws
  shapes parametrically instead.
- **Flutter.** `AnimationController` ticks on the UI (now main) thread with `Simulation`s
  (`SpringSimulation`, `FrictionSimulation`), and the raster thread only draws. Since 3.29 (iOS and
  Android) and 3.35 (macOS and Windows), Dart runs on the platform main thread, giving up a separate
  UI thread for synchronous platform interop
  ([breaking change](https://docs.flutter.dev/release/breaking-changes/macos-windows-merged-threads)).
  So Flutter animations hitch whenever the main thread does. `SpringDescription.withDurationAndBounce`
  matches SwiftUI (`flutter/packages/flutter/lib/src/physics/spring_simulation.dart:70-82`). Its scroll
  physics are the best open reference (§2.11). *Steal:* the simulation trait, scroll physics, velocity
  trackers. *Avoid:* animation evaluated on the main thread.
- **Rive.** State machines with typed inputs (bool, number, trigger), transitions with their own
  duration, interpolation and interruptibility, blend states, and `advance(dt)`, which is
  deterministic (`rive-runtime/src/animation/`, e.g. `state_machine_instance.cpp`,
  `blend_state_1d.cpp`). *Steal:* a transition table keyed by (from-phase, to-phase), the right shape
  for Scrawl-style pills with phases. Rive files could become a node type later.
- **GPUI.** Springs now exist (above), but they're evaluated on the main thread inside
  `request_layout`, which forces a view re-render every frame. *Avoid* that structure.
- **Xilem/Masonry.** `on_anim_frame(elapsed_ns)` runs as a pass over the widget tree on the main
  thread (`xilem/masonry_core/src/passes/anim.rs:11-51`). It's fine for simple widgets, but nothing
  happens off-thread.

### 2.3 Spring math (normative)

```
Spring::new(duration d, bounce b):   ω = 2π / d
                                     ζ = 1 − b        (b ≥ 0)
                                     ζ = 1 / (1 + b)  (b < 0)
mass 1, stiffness k = ω², damping c = 2ζω
Δ = x0 − T
ζ < 1:  ωd = ω√(1−ζ²);  x(t) = T + e^{−ζωt} (Δ cos ωd t + (v0 + ζωΔ)/ωd · sin ωd t)
ζ = 1:  x(t) = T + (Δ + (v0 + ωΔ) t) e^{−ωt}
ζ > 1:  r1,2 = −ω(ζ ∓ √(ζ²−1));  c1 = (v0 − r2Δ)/(r1 − r2), c2 = Δ − c1;  x = T + c1 e^{r1 t} + c2 e^{r2 t}
v(t) is the analytic derivative (SpringSolver.swift:168-200).
```

- **Units:** velocity is property units per second (pt/s, opacity/s). It's normalized only when a
  CA mirror needs it (the launcher `core.rs:2441-2454`).
- **Settling:** each segment stores `settle_at = t0 + settling_time(x0, v0, T, ε_pos, ε_vel)` using
  psychopomp's bound (`motion.rs:75-132`). `ε_pos` comes from the property class and is set *below the
  snap quantum*: 1/128 device px for geometry, 1/512 for opacity, 1/64 px for blur. So the final snap
  to `T` is invisible. After `settle_at` the track returns exactly `(T, 0)` and is retired.
- **Conversions** at the boundaries: psychopomp's `SpringPlan::visual` multiplies duration by 1.2,
  and Motion divides by 1.2 (Kit's motion rules and presets). Rune stores
  `(duration, bounce)` and exports ω and ζ.
- **Other curves**, as segment kinds that sample analytically:
  - `Decay{k}` (fling, scroll inertia): `x = x0 + v0/k · (1 − e^{−kt})`, `v = v0 e^{−kt}`.
  - `Tween{dur, cubic}` (fades, reduced motion): velocity from the curve's slope, as psychopomp's
    `Ease` does.
  - `Rubber{..}` (overscroll).
  - `Bound(live, expr)` (render-thread binding).
  - `Fixed`.

### 2.4 Runtime architecture

```
             platform main thread                                   render thread (one per display)
 ┌──────────────────────────────────────────────┐   commits   ┌────────────────────────────────────────┐
 │ input pump ─┬─ live-binding input ───────────┼────────────▶│ vsync (CAMetalDisplayLink / CVDL / rAF)│
 │             └─ app events → handler queue    │  (SPSC,     │ 1 drain commits with apply_at ≤ T      │
 │ apply handlers (&mut models, tx)             │   lock-free)│ 2 drain forwarded input → Live values   │
 │ rebuild dirty view scopes → El               │             │ 3 step scroll sims & bindings          │
 │ diff by key → node ops                       │             │ 4 sample tracks at target time T       │
 │ incremental layout (target frames)           │             │ 5 world xforms → 2-edge snap → masks   │
 │ layout-animation planner → Animate ops       │             │ 6 encode + submit GPU, present at T    │
 │ text shaping / display lists for changed     │◀────────────┤ 7 publish presented snapshot (triple   │
 │   content only                               │  events     │   buffer); Settled/Exited/Release evts │
 │ build Commit{apply_at, ops}                  │             │ 8 trace frame (if on); sleep if idle   │
 └──────────────────────────────────────────────┘             └────────────────────────────────────────┘
```

**On the render thread:**
- sampling every track;
- scroll position integration (decay, rubber band, spring-back, paging snap);
- 1:1 drag bindings;
- matched-geometry flights;
- world transforms;
- two-edge snapping;
- soft-mask and blur parameters;
- compositing and encoding of already-recorded display lists and glyph runs;
- presentation timing;
- trace output.

**Not on the render thread:**
- app logic, handlers and view functions;
- diff;
- layout;
- text shaping and line breaking;
- deciding a fling's target (the main thread decides; the render thread bridges, §2.10);
- accessibility tree;
- platform calls.

This matches Core Animation's split: the app builds the model tree, and the server interpolates and
composites.

**Why app logic stays on the platform main thread:** platform interop (text input, accessibility,
menus) is synchronous. Flutter merged its threads for exactly this reason. Animations stay smooth
while the main thread hitches, because the render thread keeps sampling and content updates simply
land a frame late. The input latency of render-thread-owned gestures still depends on the main
thread pumping events quickly. That pump is a tiny forwarder, kept apart from app work by
budgeting: rebuild and layout are time-sliced, and text shaping is async (open question 1).

**Data structures** (render thread):

```rust
#[derive(Copy, Clone)] struct NodeId(u32 /*index*/, u32 /*generation*/);
#[repr(u8)] enum Prop { L, T, R, B, Opacity, Blur, Scale, Rotation, Radius,
                        FillL, FillA, FillB, FillAlpha, MaskFeather, Custom(u8) }

#[derive(Copy, Clone)]
enum Curve {
    Spring { omega: f32, zeta: f32, token: TokenId },
    Decay { k: f32 },
    Tween { dur: f32, cubic: [f32; 4] },
    Rubber { min: f32, max: f32, dim: f32, c: f32 },
    Bound { live: LiveId, expr: ExprId }, // tiny pure expression, not a closure
    Fixed,
}
#[derive(Copy, Clone)]
struct Segment { t0: f64, x0: f32, v0: f32, target: f32, curve: Curve, settle_at: f64 }
struct Track { seg: Segment, pending: Option<(f64 /*due*/, Segment)>, gen: u32 } // knobs-style delay
struct Animator {
    tracks: SlotMap<TrackId, Track>,          // ~48 B each (est.)
    by_prop: HashMap<(NodeId, Prop), TrackId>,
    active: Vec<TrackId>,                     // compacted on settle; a timer wheel on settle_at if large
}
```

Colour channels animate in OKLab, so a hue change doesn't pass through grey. Rects animate as edges
(§2.7).

**Commit protocol** (main → render, applied atomically at one frame, like `CATransaction`):

```rust
struct Commit { id: u64, apply_at: f64 /* target presentation time */, origin: Origin, ops: Vec<Op> }
enum Op {
    Insert { node: NodeId, parent: NodeId, index: u32, props: PropSet, content: ContentId },
    Remove { node: NodeId, exit: Option<ExitSpec>, gen: u32 },
    Revive { node: NodeId, gen: u32 },
    Set { node: NodeId, prop: Prop, value: f32 },
    Animate { node: NodeId, prop: Prop, target: f32, curve: Curve,
              velocity: Velocity /* Inherit | Override(f32) */, delay: f32 },
    Content { node: NodeId, display_list: Arc<DisplayList> },
    Bind { node: NodeId, prop: Prop, live: LiveId, expr: ExprId },
    Matched { ns: u32, id: u64, incoming: NodeId, mode: MatchMode },
    Clip { node: NodeId, shape: RRect, feather: [f32; 4] },
}
```

`apply_at` is the next target presentation time at least one render lead (≈ 2 ms) after the commit
is ready. Every op in a commit starts on the same frame. Kit's rule that the pill width and the word
roll start together holds by construction.

**Per-frame cost when one value animates** (*est.*, M-series):
- main thread: 0;
- render thread: drain plus one closed-form sample (~30 ns), world-transform update for that subtree,
  snap, and re-encoding of the scene;
- the scene encode dominates (renderer doc), and 10,000 concurrently animating tracks still sample in
  about 0.3 ms.

When nothing is active, no commits are pending and no scroll or binding is live, the display link is
paused: zero idle CPU (Kit's idle rule).

### 2.5 How a property gets an animation (resolution order)

When the diff finds that a node's target for `prop` changed, the runtime decides between `Set` and
`Animate` in this order:

1. **Transaction override.** `tx.instant()` → `Set`. `tx.animate(spring)` or
   `tx.prop(Prop::Opacity, curve)` / `tx.axis(Axis::X, spring)` → that curve.
2. **Origin policy.** `Origin::Direct` (typing, dragging, arrow-key selection and other changes made
   with the user's own hand) → `Set` unless the transaction opted in. `Origin::Undo | Redo | Remote |
   System` → continue to step 3. (Kit rule 5.)
3. **The node's declared default**, `.animate(Prop::Fill, motion::SNAP)`, or the inherited
   `.layout_animation(..)` / `.presence(..)`.
4. **Otherwise `Set`.** Nothing animates unless something asked for it.
5. **First mount.** Nodes inserted in a scope's first build (`Mount::Initial`) always `Set` (`initial
   = false`, Kit rule 6). Descendants inserted together with an animating parent don't animate on
   their own; they ride the parent, which avoids cascades.
6. **Reduced-motion filter** (§2.12) is applied last.

Spring *pairs* (enter/exit tokens) are resolved by direction. Presence exits and `open → closed`
toggles (`child_if`, `.open(bool)`) use `.exit`; everything else uses `.enter`.

### 2.6 Retargeting

```rust
fn retarget(track: &mut Track, at: f64, target: f32, curve: Curve, vel: Velocity) {
    let (x, v) = track.seg.sample(at);               // exact, analytic
    if target == track.seg.target && curve.same_params(&track.seg.curve) && vel.is_inherit() {
        return;                                      // psychopomp: unchanged Animate keeps trajectory
    }
    let v0 = match vel { Velocity::Inherit => v, Velocity::Override(g) => g };
    track.seg = Segment::new(at, x, v0, target, curve); // recomputes settle_at
    track.gen += 1;
}
```

- **Sample-and-restart, not additive stacking.** Springs are linear ODEs. Restarting from the sampled
  `(x, v)` with the same spring gives *exactly* the motion UIKit's additive animations add up to,
  while using one segment per property, O(1) evaluation and no growing stack. A different spring on
  retarget is also C¹ (position and velocity continue). Only acceleration jumps, which nobody can see.
- `at` is the commit's `apply_at`. Main and render threads agree on it, so a test can predict the
  exact frame.
- Changing a token while tracks are live retargets every live track using that token, at the
  current frame, to the same target with the new ω and ζ. Live tuning is therefore smooth.
- Reversing mid-flight keeps velocity. There's no snap back to the start (Kit's motion checklist).

### 2.7 Layout animation with the layout engine (FLIP done natively)

Layout (Taffy-like, incremental, with two-edge rounding) runs **once, at the target state**. Then
the planner runs on the main thread:

```
for node in layout.changed_nodes():                      // frames in parent space, device-rounded
    old = node.prev_target; new = node.target
    policy = resolve(tx, node)                           // §2.5, gives per-axis springs or Set
    for axis in [X, Y]:
        (e0, e1) = edges(old, axis);  (n0, n1) = edges(new, axis)
        spring = policy.axis_spring(axis)                // "width leads": X faster than Y
        if e0 != n0: Animate(node, axis.start_edge, n0, spring, Inherit)
        if e1 != n1: Animate(node, axis.end_edge,   n1, spring, Inherit)
        // an unchanged edge gets no op at all, so it can't jiggle
```

- **Edges, not origin plus size.** Animating `(x, w)` with different springs moves an "anchored"
  edge: `x + w` drifts when x and w ride different curves. With edge tracks:
  - a stationary edge has no track;
  - rigid travel (both edges moving by the same Δ on the same spring with equal velocities) stays
    rigid, even across retargets;
  - two-edge snapping (§2.13) falls out of this directly.
- **Parent-relative by default.** If a child's parent-relative frame doesn't change, it rides its
  parent with no tracks of its own.
- **Straight paths (Kit rule 12).** One transaction gives every layout track the same spring and the
  same `t0`. With zero initial velocity, the composed screen path is `start + (end − start)·s(t)` for a
  single shape `s`, so it's a straight line. When that can't hold (different springs, or retargets
  with mixed velocities) use `.travel(Travel::Screen)`. Then the node's edges are tracked in the
  **space of the transaction's common ancestor** and converted to parent space at sample time using
  the parent's *presented* transform, which is Motion's projection idea. Matched-geometry flights
  always use screen space.
- **Content while a container resizes** is chosen per node with `.resize(..)`:
  - `Resize::Shape` (default for surfaces): background, border, shadow, radius and glass are
    parametric primitives drawn at the presented rect every frame. That's free, and nothing is
    scaled.
  - `Resize::Mask{feather}` (default for clipping containers): children are laid out at the target
    and revealed by a soft-edged mask that follows the presented rect (OpenCode's `TopAnchoredRow`,
    Kit's motion patterns #6; the launcher's "static final-layout render").
  - `Resize::Scale`: FLIP scale for media and thumbnails only.
  - `Resize::Relayout`: re-runs layout per frame with interpolated constraints on the main thread.
    Opt-in only, for text that must reflow. The inspector flags it as main-thread-per-frame.
- **Siblings sliding into gaps** need nothing extra. When the removed row leaves flow (popLayout),
  the siblings' targets change, so their edges animate on the transaction's spring (`REFLOW`).
- **Row growth** (streamed text, images loading): a `.growth(Growth::capped(max_speed, max_accel))`
  policy implements OpenCode's `RowGrowth`/`GrowthRide` (Kit's motion patterns #5). It caps speed per
  frame and cuts immediately on catch-up bursts.

### 2.8 Presence (exits that stay mounted, identity by key)

The main-thread node phase is `Present | Exiting{gen}`.

- **Removal.** When a keyed child disappears from the view output, the node's phase becomes
  `Exiting{gen+1}`.
  - `Presence::pop()` (the list default) takes it out of layout flow at its last presented rect in
    parent space. Siblings reflow.
  - `Presence::hold()` keeps it in flow and collapses its slot along the main axis with a mask.
  - `Op::Remove{exit: ExitSpec, gen}` sends the exit targets on the exit spring. The defaults are
    opacity → 0, blur → 4 px and scale → .96 on `SETTLE.exit` (.24/0, 20% shorter than enter).
- **Unmount.** The render thread sends `Exited{node, gen}` when the node's exit tracks pass
  `settle_at`. The main thread drops the node **only if** its phase is still `Exiting{gen}`. So
  nothing ever unmounts at opacity > 0. The blur filter stays attached until unmount, so the last
  frame doesn't flash sharp (`PresenceGate.swift:76-79`).
- **Revive.** If the same key comes back while it's exiting, the node returns to `Present` with
  `gen+1` (the late `Exited` is ignored). Its props retarget back to rest from their current values
  and velocities, and it isn't remounted. Identity is preserved.
- **Enter.** An insert after first mount starts at the enter-from props (opacity 0, blur 4, scale .96)
  and animates to rest in the same commit. `.enter_from(anchor)` makes an item that was hidden
  *behind* another start under the anchor's presented rect and travel a straight path, drawn beneath
  it (Kit rule 12).
- **Persistent items never blur.** Blur is only part of enter and exit specs. Nodes that move because
  of layout have no blur track. Validation clamps presence scale to ≥ .9 and rejects scale 0.
- **Nesting.** Children of an exiting parent ride the parent's exit and aren't exited on their own.
  Opt out with `.exit_independently()`.

### 2.9 Shared-element and matched geometry

- `.matched(ns, id)` — **move.** In one commit, when node A with `(ns, id)` leaves and node B with the
  same id arrives, B's edges start at A's *presented screen rect* and velocities, converted to B's
  space, and animate to B's layout target. A hides instantly. While in flight, B is drawn in a
  **flight layer** at the transaction's common ancestor, so it isn't clipped by a scroll container it
  is leaving. Its own soft masks still apply (z-order is an open question).
- `.morph(ns, id)` — **one shape** (Kit rule 9, Liquid Glass `matchedGeometry`, Kit's motion patterns
  #9). The *surface* (rect, radius, fill or glass) is a single primitive that interpolates from A's
  presented shape to B's. A's content fades out with blur, riding the surface, and B's content fades
  in on a short lead. Width leads through the X-axis spring (launcher morph: `width_vd = 0.78·vd`,
  `docs/motion.md:47-50`).
- Under reduced motion both become a .15–.2 s crossfade in place (Apple's `.materialize` analogue).

### 2.10 Gestures and velocity handoff

```rust
let offset = cx.live("drawer.y", 0.0);                   // Live<f32>: render-thread owned, keyed
drawer()
    .translate_y(offset)                                  // Bind op: no view re-run while dragging
    .draggable(Axis::Y, offset, Bounds::new(0.0, 640.0).rubber_band())
    .on_release(ui, move |u, fling, tx| {                 // main thread decides the target
        let land = fling.projected_y(Decel::IOS_NORMAL);  // x + v/1000 · r/(1−r)
        u.drawer_open = land < 320.0;
        tx.animate_live(offset, if u.drawer_open { 0.0 } else { 640.0 }, motion::GLIDE)
          .velocity(Velocity::Inherit);                   // keep the live velocity
    })
```

- **During the drag** (render thread): pointer deltas apply 1:1 to the `Live` value. That's the
  user's hand, so there's no spring (Kit rule 5). The only exception is an opt-in `follow` spring
  (palette: ω 34, ζ .86 lead).
- **Velocity estimation** uses platform event timestamps (not arrival time) and coalesced touches.
  - The default is least squares over the last 100 ms and 20 samples, treating 40 ms of silence as
    stopped (Flutter `velocity_tracker.dart:142-144`).
  - On iOS, scroll flings use UIScrollView-like weights over the last three deltas, 0.6/0.35/0.05
    (`velocity_tracker.dart:295-369`).
  - On macOS: 0.15/0.65/0.2 (`velocity_tracker.dart:416-439`, fetched from flutter `master`).
- **The release bridge.** The main thread's decision arrives one or two frames late. So on release
  the render thread immediately continues the `Live` value as a `Decay{k}` with the release velocity.
  When the main thread's `Animate{velocity: Inherit}` lands, it retargets from the decay's exact
  `(x, v)`. The result is C¹ with no stall at release, which is the problem UIKit solves by running
  everything on the main thread.
- **Projection.** Use `fling.projected(rate)` with the iOS formula from
  [WWDC18 Designing Fluid Interfaces](https://developer.apple.com/videos/play/wwdc2018/803/), so the
  target snap point respects the user's momentum.

### 2.11 Scroll physics per platform

A `ScrollPhysics` trait produces segment kinds for the render thread. Scroll offset is a `Live` value
the render thread owns, and the main thread gets offset snapshots for virtualization and
non-blocking `on_scroll` events.

| Platform | Inertia | Overscroll | Notes |
| --- | --- | --- | --- |
| iOS | `v(t) = v0 · r^(t_ms)`, r = 0.998 normal / 0.99 fast ([UIScrollView.DecelerationRate](https://developer.apple.com/documentation/uikit/uiscrollview/decelerationrate-swift.struct)); as `Decay{k}` with k = −1000·ln r ≈ 2.002 s⁻¹; Flutter uses the same constant, `0.998^1000 ≈ 0.135` (`scroll_simulation.dart:50-56`) | Rubber band `f(x) = (1 − 1/(x·c/d + 1))·d`, c = 0.55, d = viewport dimension ([Grant Paul, 2012](https://twitter.com/chpwn/status/285540192096497664), older but still the reference); release out of range → spring back, ζ ≈ 1 (Flutter default ≈ mass .5, k 100, ζ 1.1, `scroll_physics.dart:434-438`) | Hand off to the spring at the edge with friction velocity capped at 5000 pt/s (Flutter `maxSpringTransferVelocity`, `scroll_simulation.dart:79-81`). Paging = projected target snapped to a page, spring with inherited velocity. |
| macOS | **The OS generates momentum.** Trackpad `NSEvent`s carry `phase` and `momentumPhase` with system-computed deltas, which must be applied 1:1, not re-simulated | While the gesture or momentum pushes past the edge, apply the elastic formula to the deltas; spring back when the momentum phase ends | Mouse wheel (non-precise) deltas: animate line steps on `instant` (.1/0). Never double-apply inertia. |
| Android | Spline fling (`SplineOverScroller`): decel rate ln(.78)/ln(.9), inflexion .35, physical coefficient ≈ 9.8·39.37·ppi·0.84 (Flutter port `scroll_simulation.dart:201-259`) | Android 12+ **stretch** `EdgeEffect` (scale toward the edge), not glow | Velocity in px/s against ppi. |
| Windows | DirectManipulation supplies inertia for precision touchpads | Clamp or optional bounce | Wheel = discrete steps on `instant`. |
| Web | Wheel deltas on macOS *already include* OS momentum; touch is browser-native | Emulate per the host OS (UA-detected) | When possible, back the scroll container with a native scroller and read its offset. |

Every scroll viewport gets soft edge masks: 24 pt by default on edges where content overflows. The
mask strength is `min(1, overflow/feather)`, so it ramps in instead of popping (Kit rule 11). The
mask is attached to the viewport, not the content (Kit's "edge travel" rule). Scrolling runs on the
render thread, so checkerboarding is possible. The mitigations are an overscan sized to the current
scroll velocity and a placeholder skeleton (hard problem 3).

### 2.12 Reduced motion

The runtime applies it per property class, so authors can't forget it:

| Class | Under Reduce Motion |
| --- | --- |
| Spatial (edges, translate, scale, rotation, layout animation, matched flights, programmatic scroll) | `Set` (no travel) |
| Opacity, presence enter/exit | Opacity-only `Tween` of .15–.2 s (token `motion::REDUCED_FADE`; OpenCode `reducedFade` .20, `MotionConstants.swift:33-37`; the launcher 75 ms, `docs/motion.md:78`) |
| Blur | 0 (and the filter is never attached) |
| Morph | Crossfade in place |
| User's hand (drag, fling inertia, trackpad momentum) | Unchanged: it's the user's own motion |
| Continuous loops (shimmers, orbs) | Hold a calm static pose that still shows state |

The source is the OS setting (`accessibilityReduceMotion`, `prefers-reduced-motion`, Android
animator scale). Tests can override it: `TestApp::reduce_motion(true)`.

### 2.13 Two-edge 1/64 px snapping

This runs as the last step on the render thread, in device pixels. For axis-aligned world transforms,
`x0 = grid(left)` and `x1 = grid(right)`, and width is *derived* as `x1 − x0`, where
`grid(v) = round(v·64)/64` (Kit's motion patterns #3; Signal desk v3 `autocomplete.js`). With
rotation or non-uniform scale, only the translation is snapped.

At rest, layout has already rounded both edges to device pixels using the same cumulative two-edge
pattern Taffy uses (`taffy/src/compute/mod.rs:381-393`: `size = round(cum + w) − round(cum)`). So the
moving 1/64 grid and the resting pixel grid agree, and a node lands with no final shift. Settle
thresholds sit below the quantum (§2.3).

### 2.14 Soft-edge masks everywhere

- **The clip primitive** is `Clip{shape: RRect, feather: [l, t, r, b]}`, evaluated as an SDF ramp in
  the composite shader. It costs a few ALU ops per pixel. Nested clips multiply.
- **Defaults:**
  - scroll viewports: 24 pt on overflowing edges, ramped (§2.11);
  - text overflow: a 1.5 em right-edge fade, never an ellipsis;
  - panels and cards clipping children: a 16 pt inner fade on the clipping side;
  - rounded corners: an anti-aliased SDF, so no stair-steps.
- **Hard clips** need an explicit `.clip_hard(reason)`. The inspector and the trace (`flags.clipped`
  plus the `vis` rect) let the internal motion linter flag unmasked cuts. Every clipping ancestor and axis is
  checked, matching the skill's audit rule.
- **Group isolation.** When a masked subtree has overlapping translucent children, it's rendered to
  an offscreen layer, which the renderer can do automatically (renderer doc).

### 2.15 Blur and scale for enter and exit

- **Defaults:**
  - whole-element replacement in place: 3–4 px blur, scale ≈ .96, plus opacity;
  - exits run 10–25% shorter;
  - persistent and sliding content stays sharp.
- **Blur cost.** Blur is a filter on an offscreen layer (dual-Kawase at reduced resolution, radius
  ≤ 8 px). It's attached when a blur track becomes non-zero and detached when it *settles* at 0.
  On exit it stays until unmount. Resting UI pays no blur cost (the launcher removes `CAFilter`s at rest,
  `docs/motion.md:43`; `PresenceGate.swift:92-97`).

### 2.16 One clock, time scaling and deterministic stepping

```rust
enum ClockSource { Display, Manual { hz: f64 } }
struct Clock { anchor_host: f64, base: f64, scale: f64 }   // τ = base + (host − anchor)·scale
impl Clock {
    fn set_scale(&mut self, s: f64, host_now: f64) {         // re-anchor (scrawl motion.rs:42-53)
        self.base += (host_now - self.anchor_host) * self.scale;
        self.anchor_host = host_now;
        self.scale = s.clamp(0.02, 4.0);
    }
}
```

- **Every** consumer reads τ: tracks, scroll simulations, presence timing, trace `t` and `sc`. Slow
  motion is truthful only when every clock scales together (Kit's motion patterns #12). If a CA mirror
  is used, its `layer.speed` and `timeOffset` get re-anchored in the same step.
- Frames are sampled at the display link's **target presentation timestamp**
  (`CAMetalDisplayLink`/`CADisplayLink.targetTimestamp`; `MotionDisplayLink.swift:62-68`). Tracks
  sample absolute time, so a hitch lands motion where it should be, which is CA's behaviour.
  Eye-tracked flights can opt into a dt clamp (Kit's motion patterns #1).
- `Manual{hz}`: `app.advance(ms(500))` steps exact frames, `app.settle()` runs until there are no
  active tracks, and `app.frame()` steps one frame.

### 2.17 motion traces

Turn it on with `RUNE_MOTION_TRACE=<file>`, mirroring `SCRAWL_MOTION_TRACE`
(`scrawl/src/ui/motion.rs:551-561`). When it's off it costs one `OnceLock` read. In tests,
`app.record(..)` returns the trace in memory. The render thread writes after step 5, so the trace
records **exactly what was presented**: post-snap and post-mask.

```jsonc
{"type":"header","launch":"mail","width":1512,"height":982,"hz":120,"scale":2,
 "platform":"rune-macos","units":"pt","clock":"virtual"}
{"t":1234.567,"n":1,"sc":1,
 "ev":["tx#41 Direct tap thread:7 SNAP","retarget thread:3.T REFLOW v=-212.4"],
 "ai":[[12,16,112,358,56,2,400]],                 // intent: [id,x,y,w,h,bits,ms] from Animate ops
 "m":[{"id":12,"cls":"row","nm":"thread:3","host":"list:inbox","pv":4}],
 "e":[[12,16,140.25,358,56,1,8,258]],              // [id,x,y,w,h,alpha,radius,flags,(vis x,y,w,h)]
 "g":[9],                                          // unmounted (only after its exit settled)
 "cm":[{"id":1,"name":"scroll:inbox","kind":"position"}],"c":[[1,840.5]]}
```

- **Flags** use the motion linter's bits (`analyze.ts:41-43`): animated 1, ancestorAnimated 2, scrolling 4,
  clipped 8, filtered 16, occluded 32. Intent classes are packed at `<<8` (own) and `<<16`
  (ancestor): position 1, size 2, opacity 4, shape 8, transition 16, reveal 32, contents 64.
  Rune sets them from the actual track kinds, so the motion linter's "was this intended?" logic gets the
  truth instead of a guess.
- **Events** carry the commit id, origin, token name and retarget velocities. Channels carry scroll
  offsets and, optionally, per-track progress.
- **Scrubbing.** The runtime can also emit a *sweep*: sample a recorded commit sequence at any τ
  without running the app. Tracks are pure, which gives the motion linter's `sweepClock` the same power
  natively.
- **In-process invariants in debug builds.** "Unmount at opacity > 0" is impossible by construction.
  Warnings fire for a visible `Set` of a spatial prop whose origin isn't Direct (a likely pop), a
  `.clip_hard` without a reason, and blur on a persistent node.

### 2.18 Live tuning and tokens

- `motion::*` constants are *token ids* whose defaults are generated from Kit's motion presets
  (generated Rust today). The render thread resolves tokens through a `TokenTable`.
- Debug builds watch `~/.config/<app>/tune.json` (Scrawl's loop, `motion.rs:316-360`) and accept a
  knobs remote over WebSocket. A change retargets live tracks (§2.6).
- `rune tune rust` prints the paste-back for the token defaults (`scrawl tune rust`,
  `motion.rs:442-455`).
- The trace logs token names, so a clip can be matched to the knob that produced it.

### 2.19 Public animation API (a summary)

```rust
// springs
Spring::new(0.30, 0.0)                       // duration, bounce
SpringPair { enter: Spring::new(0.30, 0.16), exit: Spring::new(0.27, 0.08) } // motion::MORPH

// transactions (inside handlers)
tx.animate(motion::MORPH).axis(Axis::X, motion::SNAP)   // width leads
  .prop(Prop::Opacity, Curve::fade(0.16)).delay(Prop::Opacity, 0.012);
tx.instant();
tx.animate_live(live, target, spring).velocity(Velocity::Override(v));

// node modifiers
.animate(Prop::Fill, motion::SNAP)
.layout_animation(motion::REFLOW)
.presence(Presence::pop().enter(Fx::fade().blur(4.0).scale(0.96)).exit(Fx::fade().blur(4.0).scale(0.96)))
.enter_from(key!("pile-top"))
.matched(ns, id) / .morph(ns, id)
.travel(Travel::Screen)
.resize(Resize::Mask { feather: 28.0 })
.transitions(phase, |t| t.on(Phase::Recording, Phase::Picker, motion::MORPH)) // Rive-style table
.clip_soft([0.0, 24.0, 0.0, 24.0]) / .clip_hard("video surface owns its edges")

// live values
let v: Live<f32> = cx.live("drawer.y", 0.0);
node.translate_y(v).opacity(v.map(Expr::remap(0.0, 640.0, 1.0, 0.4)));
cx.presented(key!("thread", 3)) -> Option<PresentedRect>  // untracked snapshot
```

---

## Steal

- SwiftUI: transaction-scoped animation, merging springs, `Spring(duration:bounce:)`.
- Core Animation: an off-main render thread, model/presentation split, atomic commits.
- Compose: `animateTo(.., initialVelocity = velocity)` and phase separation.
- Motion: `popLayout`, `layoutId`, projection to keep paths straight.
- Flutter: the `Simulation` trait, scroll physics constants and velocity trackers.
- Rive: a transition table keyed by (from, to) phases.
- psychopomp: pure piecewise tracks, `settling_time`, transactional retargets.
- The private iOS client: `PresenceGate` generations, blur gating, one display link through `AnimationTimeScale`.
- Scrawl: the clock, live tuning and the trace writer.
- The launcher: final-layout-once plus animated geometry, and per-axis "width leads" springs.
- Xilem: handlers that receive `&mut State`.
- GPUI: `Model` entities as the hot-reload unit, and the `list` sum tree.
- Taffy: two-edge cumulative rounding.

## Avoid

- Animations evaluated on the main thread (GPUI's `request_animation_frame → notify`, Flutter, egui).
- Velocity you can't read back (CA `presentationLayer` heuristics).
- Numeric integrators that can't be sampled at an arbitrary *t* (knobs' 480 Hz Euler).
- Run-once components whose state lives in closures (signals) when hot reload matters.
- Hook-order or call-site identity.
- Deep generic view types (compile times, error messages).
- Scale-based FLIP for anything with text.
- Snapping a critically damped spring to its target when it crosses, which creates a velocity cliff.
- Manual `notify()`.
- `Task.sleep`-style unmount timers.
- Hard clips and ellipses.

## Recommendation

**Rune uses:**
- a **keyed retained tree built from pure view functions over `Model<T>` stores**, with model-level
  auto-tracking and queued `&mut` handlers;
- a **type-erased `El`** for compile speed;
- explicit string keys for local state;
- an **animation runtime on a dedicated render thread per display**: piecewise analytic tracks per
  property, transactional commits with presentation-time `apply_at`, sample-and-restart retargeting,
  edge-based layout animation, generation-guarded presence, render-thread scroll and drag bindings
  with a decay bridge on release, runtime-enforced reduced motion, two-edge snapping, soft masks by
  default, one scalable or manual clock, and native motion traces.

**Rejected alternatives:**

| Alternative | Why not |
| --- | --- |
| Fine-grained signals as the primary model | Their main advantage (a cheap per-frame update) is already delivered by render-thread tracks. They lose on hot reload and have silent stale-read footguns. Kept as a narrow tool: `Live<f32>` is effectively a render-thread signal. |
| GPUI's entity plus re-render per frame | Per-frame cost scales with view complexity; manual notify; runtime double-borrow panics. |
| Immediate mode | CPU every frame, nothing offloaded, and presence or layout animation would rebuild a retained tree anyway. |
| Core Animation as the engine | No velocity readback, Apple-only, no determinism, no custom shaders for soft masks or glass inside CA's content model. Keep it only as an optional mirror for system materials (`NSGlassEffectView`, `CABackdropLayer`). |
| Additive animation stacks | Equivalent for linear springs, but more state and harder to trace. |
| Running app logic off the main thread by default | Conflicts with synchronous platform APIs (Flutter's thread merge). Revisit if input latency measurements demand it. |

## Hard problems

1. **Straight paths with nested, independently timed animations.** Parent and child on different
   springs, or retargeted with mixed velocities, compose into curves. `Travel::Screen` fixes
   individual nodes, but choosing the coordinate root automatically, and reparenting mid-flight, is
   subtle (Motion's projection tree is roughly 2k lines for a reason).
2. **Content during resize.** Mask vs shape vs scale vs per-frame relayout. Text that must reflow
   during a size animation forces main-thread work every frame, so it needs a budget and a lint.
3. **Main/render consistency.**
   - Hit-testing must use *presented* geometry: a tap on a moving row should hit what you see.
   - Virtualization runs behind render-thread scrolling (checkerboarding on fast flings, sizing
     overscan to velocity).
   - Commit latency is one frame plus input-to-photon.
4. **Presence edge cases.**
   - Exits inside an exiting parent.
   - The z-order of exiting nodes versus new arrivals.
   - Exits inside a scroll container that is itself scrolling.
   - A key that moves to a different parent (a reparent: it should travel, not exit and enter).
5. **Matched geometry across clip and scroll ancestors.** Should the flight layer escape clipping
   (Flutter Hero) or stay under the chrome it's passing beneath (Kit's "emerge from behind")? This is
   probably per transition, which needs a z-policy API.
6. **macOS scrolling.** Mixing OS-provided momentum with Rune's rubber band, spring-back and
   render-thread offset, plus the rule that web wheel deltas must never get inertia twice.
7. **Determinism.** Cross-platform float math (deterministic `libm`), the order of commit
   application against forwarded input in tests, and time-scaling CA mirrors in lockstep.
8. **System materials (Liquid Glass) in lockstep with Rune's Metal layer.** Either mirror tracks
   into `CASpringAnimation`s evaluated with identical math (the launcher `eval_ca_spring_spec_at`), or drive
   CA every frame from the render thread with actions disabled (Scrawl's fixed `NSPanel` plus single
   `CATransaction`). Both have a one-frame-skew risk.
9. **Blur and mask cost at scale.** Offscreen passes per blurred or isolated group, and many
   simultaneous exits in a big list (cap concurrent blurred layers; degrade to opacity-only past N).
10. **Overshoot from velocity at bounce 0.** A fast fling into a critically damped spring physically
    overshoots. Is that "bounce"? Scrawl clamps, which causes a velocity cliff. My default is to allow
    the physics; an opt-in `no_overshoot` would clamp *velocity*, not position.
11. **Hot reload while animations are in flight**, when node identity changes shape (a key type
    changes, a list becomes a grid): decide between exit/enter and a best-effort matched transfer.
12. **Streaming text** (line-hold, growth caps) belongs half in layout and half here. The interface
    between text layout and the growth policy needs design together with the text doc.

## Open questions

1. App logic on the platform main thread (the default) vs a separate app thread with a thin
   main-thread forwarder. Measure input-to-photon for render-thread drags under a deliberately busy
   main thread before deciding.
2. Field-level read tracking (`@Observable`-style derive) in v1, or model-level only?
3. Does the renderer re-encode the whole scene every frame (Vello-style) or cache layer textures?
   This sets the render-thread budget. Coordinate with the renderer doc.
4. Port the motion linter's core rules to Rust for in-process `trace.assert_clean()`, or shell out to the
   TS CLI?
5. The default list presence: `pop` (siblings close the gap immediately, while the exit plays) or
   `hold` (the gap closes as the exit plays)? Kit's "siblings slide into the gap" works with both.
   Needs a blind A/B.
6. Confirm the semantic token names (`snap`, `settle`, `reflow`, `glide`, `morph`, `pop`, `drift`)
   proposed in Kit's motion presets. Kit's motion rules still treats the old palette as the reference.
7. Should `Origin::Direct` include keyboard selection moves? The skill says keys apply 1:1, but a
   selection *highlight* gliding on `SNAP` is often wanted. Probably the highlight animates and the
   content doesn't.

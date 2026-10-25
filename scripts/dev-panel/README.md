# Rune dev panel

Build `rune-dev-panel` with `--features dev` on a build worker, then run the
binary on the app host. It announces `http://127.0.0.1:7863/`: open that, the
app serves the knobs page and its WebSocket on the same port.
`--windowless` keeps a live runtime for protocol work without opening a window.

The page is knobs' own `mountPage`, pinned to
`@kitlangton/knobs@0.4.0-next.20`. Every Copy action (header, group, row) emits
`rune::tune!` lines through knobs' `formatRust`; playback and scene controls
are left out. Rune contains no panel implementation.

The page lives here as source (`index.ts`). Apps embed a minified bundle,
`crates/rune/src/dev/panel.js`; regenerate it after changing the page or the
knobs pin. `cargo test -p rune --features dev --lib dev` fails while it is stale.

```sh
(cd scripts/dev-panel && bun install)
bun scripts/dev-panel/build.ts   # rewrite the embedded bundle
bun scripts/dev-panel/serve.ts   # page development: rebuilt per start
```

`serve.ts` serves an unminified development build on `http://127.0.0.1:7862`
(`PORT` changes it). The page connects to the socket named by `?ws=`, else to
`/ws` on its own origin, so open
`http://127.0.0.1:7862/?ws=ws://127.0.0.1:7863/ws` to drive an app. For a
tailnet HTTPS preview, Tailscale Serve can point straight at the
app's port. Inspect Tailscale's existing Serve mappings first.

The same socket accepts JSON `set`, `setBatch`, `reset`, `action`, `get`, and
`tree` requests. Optional `id` is echoed in `ack`, `tree`, and `error`. An ack
means the command reached a committed frame. A transition ack marks the settled
source frame; its action runs on the following frame. `state` messages include
the current frame counter, descriptors, complete values, and watch readouts.
A `tree` reply leaves out elements playing their exit transition (and their
children), so a check that something left passes once it starts leaving. Add
`"exiting":true` to the request to keep them, each marked `"exiting":true`.

```json
{"type":"action","key":"rune.transition.Join","id":1}
{"type":"set","key":"rooms.join.spring","value":{"duration":0.5,"bounce":0},"id":2}
{"type":"set","key":"rune.paused","value":true,"id":3}
{"type":"action","key":"rune.step","id":4}
{"type":"tree","id":5}
```

Descriptors are knobs `ControlDescriptor`s, not a Rune panel schema:

| Rune value | knobs descriptor |
| --- | --- |
| Scene | choice (`rune.state`) |
| Transition | action (`rune.transition.<name>`) |
| Scale | number, 0.1–1 (`rune.scale`) |
| Pause | toggle (`rune.paused`) |
| Step | action (`rune.step`), one 1/60 logical second |
| Spring | spring, `{duration,bounce}` |
| f32 | number, optional min/max/step |
| Color | color, CSS colour string |
| Frame time | watch |

## Listener

Port 7863 is the default ("RUNE" on a phone keypad); port 0 announces a free
port. A busy port moves on to the next 15 and, past those, leaves the app running
without a panel. Headless runs (`--snap`, `--tree`, scripts) open it only with
`RUNE_DEV_PANEL=1`. Binding a Tailscale IP explicitly is allowed; wildcard or
public binding is rejected. The listener exists only inside an opted-in debug
runtime, with bounded messages and queues and owned shutdown. Socket threads
enqueue commands and wake the app; model and view mutation stays on the app
thread. `/` serves a one-line HTML shell and `/panel.js` the embedded bundle, so
opening the announced address needs no checkout or Bun. The bundle's header
carries an FNV-1a stamp of the page source that a unit test recomputes.

## App integration

```rust
let spec = rune::AppSpec {
    scenes: &[("lobby", lobby), ("joining", joining), ("room", room)],
    ..rune::AppSpec::new("Rooms", (960., 640.), init, view)
};
let app = rune::App::new(spec);
#[cfg(all(feature = "dev", debug_assertions))]
let app = app.dev_panel(rune::dev::Config::<Rooms>::default()
    .transition("Join", "lobby", |s, tx| s.join(tx))
    .transition("Leave", "room", |s, tx| s.leave(tx)));
app.run();

fn view(state: &Rooms, cx: &mut rune::Cx<Rooms>) -> rune::El {
    let spring = rune::tune!("rooms.join.spring", rune::Spring::new(0.35, 0.0));
    let width = rune::tune!("rooms.card.width", 320.0_f32);
    // ordinary Rune view using those values
}
```

The app forwards its `dev` feature to `rune/dev`. Without dev, or in a release
build, `tune!` expands to the default expression alone (including in const
contexts). With dev compiled but no `App::dev_panel` call, no listener or
registry exists. The default stays at its use site, and explicit names survive
code movement and copy-as-code. An optional numeric bounds form supplies
min/max/step.

`Config::transition` takes a plain closure whose state type comes from the
config, so the state is named once at the root. The dev configuration lives on
`App`, so `AppSpec` gains no field; `examples/dev-panel` compiles an exhaustive
`AppSpec` literal to keep it that way.

A frame-scoped thread-local registry makes `tune!` work in view helpers and
transition handlers without threading a context through. It restores the
previous scope on unwind and isolates multiple runtimes. Between frames
(`Compositor::render`, `AppHandle` updates) it falls back to the session
attached on that thread; a control first seen there wakes one more frame so the
panel hears about it. Calls on a thread with no attached runtime return their
default. Controls register lazily and stay available when states change.

### States and transitions

The panel reads `AppSpec::scenes` directly. Selecting a state rebuilds
`AppSpec::init`, runs the scene function, reconnects `AppSpec::setup` and
commits with settle. A scene such as `("list", |_| {})` is a patch of the
initial state, not a reset. `App::init`'s one-shot initializer is not replayed;
repeatable fixtures belong in `AppSpec::init` and `scenes`. `rune::scene_clock()`
gives scene functions, `init` and `setup` the running logical clock and the last
frame's scaled animation dt; `--scene` reads zero.

A transition names its source scene and a `fn(&mut S, &mut Tx)`. Replay commits
and settles the source for one frame, then runs the action on the next frame,
even if paused or at rest. Selecting a new state cancels an outstanding replay.

### Clock

An optional dev clock maps host elapsed time to a monotonic logical clock before
each frame; production clocks are unchanged. Scale and pause apply to both
animation dt and the app clock used by `poll` and `Tx::wake_after`. The logical
clock follows host wall time while animations step by the frame's own capped dt,
so a spring committed after idle never jumps by the idle time. Timer deadlines
stay logical; `wake_deadline()` converts the remainder back to wall time at the
current scale, and a paused clock exposes no deadline. Step advances 1/60 s
while staying paused. `Cx::time_scale` remains an extra animation-only
multiplier, and `El::time_scale` multiplies in for one subtree only, so a view can
freeze or slow a specimen while the panel around it keeps normal time. Scale
changes never stretch spring parameters, so poses don't jump.

Commands first bring motion and timers to their arrival time, then commit with
zero elapsed time, so an idle interval is never applied to a replayed
transition. With a panel attached, a hidden or windowless host steps animations
by real elapsed time at 60 Hz while anything moves, and sleeps at rest.

Tunables dirty the view. During a tuning commit, active same-target spring
tracks accept the changed parameters and keep their sampled velocity.

## Motion blur

The existing `lab` example also accepts `--features dev`: its Motion scene's
rune-fx shutter pass obeys the panel's Motion blur toggle. Compositors opt in
with `Config { motion_blur: true, ..Default::default() }` and read
`Runtime::motion_blur_enabled()` when constructing their effect pass.

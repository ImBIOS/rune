//! Desktop development only. Use `scripts/rune-dev`: it validates edits before dx sees them.
//! Long-lived state belongs to Runtime, never to tip-crate statics or cached view trait objects.

use std::path::Path;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::render::{Gpu, OFFSCREEN_FORMAT, Offscreen, Renderer};
use crate::runtime::{Env, Waker};
use crate::tooling::{AppSpec, Args, initial_state, runtime_for, tree_json};
use crate::{Cx, El, KeyEvent, Tx};
use std::any::Any;

// The hot tip crate owns only these thin typed adapters. Runtime<HostState>, layout,
// shell and headless loop are instantiated in the stable rune library, not recompiled per app edit.
// Any's vtable supplies type_id/drop only; no patchable behavior is cached in it.
pub(crate) struct HostState {
    pub(crate) value: Box<dyn Any>,
    spec: Box<dyn Any>,
    view: fn(&HostState, &mut Cx<HostState>) -> El,
    poll: fn(&mut HostState, f64, &mut Tx) -> bool,
    setup: fn(&mut HostState, Waker),
    key: fn(&mut HostState, &KeyEvent, &mut Tx) -> bool,
}

thread_local! {
    static INITIAL: std::cell::RefCell<Option<HostState>> = const { std::cell::RefCell::new(None) };
}

fn view_adapter<S: 'static>(host: &HostState, cx: &mut Cx<HostState>) -> El {
    let spec = host.spec.downcast_ref::<AppSpec<S>>().unwrap();
    let state = host.value.downcast_ref::<S>().unwrap();
    view_or_original(spec.view, state, cx, |state, typed| subsecond::HotFn::current(spec.view).call((state, typed)))
}

fn view_or_original<S>(original: fn(&S, &mut Cx<S>) -> El, state: &S, cx: &mut Cx<HostState>, patched: impl FnOnce(&S, &mut Cx<S>) -> El) -> El {
    let mut typed = cx.retype();
    let tree = match catch_unwind(AssertUnwindSafe(|| patched(state, &mut typed))) {
        Ok(tree) => tree,
        Err(_) => {
            eprintln!("rune-hot: view panicked; rendering the original view");
            // Discard context writes from the failed view and bypass the patch table.
            typed = cx.retype();
            original(state, &mut typed)
        }
    };
    cx.settings_from(typed);
    tree
}

fn poll_adapter<S: 'static>(host: &mut HostState, t: f64, tx: &mut Tx) -> bool {
    let spec = host.spec.downcast_ref::<AppSpec<S>>().unwrap();
    spec.poll.is_some_and(|f| f(host.value.downcast_mut::<S>().unwrap(), t, tx))
}

fn setup_adapter<S: 'static>(host: &mut HostState, waker: Waker) {
    let spec = host.spec.downcast_ref::<AppSpec<S>>().unwrap();
    if let Some(f) = spec.setup { f(host.value.downcast_mut::<S>().unwrap(), waker); }
}

fn key_adapter<S: 'static>(host: &mut HostState, key: &KeyEvent, tx: &mut Tx) -> bool {
    let spec = host.spec.downcast_ref::<AppSpec<S>>().unwrap();
    spec.on_key.is_some_and(|f| f(host.value.downcast_mut::<S>().unwrap(), key, tx))
}

pub(crate) fn run<S: 'static>(spec: AppSpec<S>, mut args: Args) {
    if args.list_scenes {
        for (name, _) in spec.scenes { println!("{name}"); }
        return;
    }
    let value = Box::new(initial_state(&spec, args.scene.take().as_deref()));
    let shell = AppSpec {
        title: spec.title, size: spec.size, min_size: spec.min_size, fonts: spec.fonts, families: spec.families,
        scenes: &[], init: || INITIAL.with(|s| s.borrow_mut().take().expect("hot initial state")),
        view: |s: &HostState, cx| (s.view)(s, cx),
        poll: Some(|s: &mut HostState, t, tx| (s.poll)(s, t, tx)),
        setup: Some(|s: &mut HostState, wake| (s.setup)(s, wake)),
        on_key: Some(|s: &mut HostState, key, tx| (s.key)(s, key, tx)),
    };
    let state = HostState { value, spec: Box::new(spec), view: view_adapter::<S>, poll: poll_adapter::<S>, setup: setup_adapter::<S>, key: key_adapter::<S> };
    run_host(shell, state, args);
}

#[inline(never)]
fn run_host(spec: AppSpec<HostState>, state: HostState, args: Args) {
    INITIAL.with(|slot| { assert!(slot.borrow().is_none()); *slot.borrow_mut() = Some(state); });
    if args.snap_on_patch.is_some() { headless(spec, args); }
    else if args.headless() { crate::tooling::headless(crate::App::new(spec), args); }
    else { crate::window::run(crate::App::new(spec), args); }
}

static ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn enable() {
    assert!(std::env::var_os("RUNE_DEV_GUARDED").is_some(), "use scripts/rune-dev: unguarded Rust patches can change live state layouts");
    ENABLED.store(true, Ordering::Release);
}

pub fn enabled() -> bool { ENABLED.load(Ordering::Acquire) }

pub(crate) struct Session {
    generation: Arc<AtomicU64>,
    seen: u64,
}

impl Session {
    pub(crate) fn connect(wake: Waker) -> Self {
        let generation = Arc::new(AtomicU64::new(0));
        let counter = generation.clone();
        subsecond::register_handler(Arc::new(move || {
            counter.fetch_add(1, Ordering::Release);
            wake();
        }));
        dioxus_devtools::connect_subsecond();
        Self { generation, seen: 0 }
    }

    pub(crate) fn changed(&mut self) -> bool {
        let next = self.generation.load(Ordering::Acquire);
        let changed = next != self.seen;
        self.seen = next;
        changed
    }

    fn pending(&self) -> bool { self.generation.load(Ordering::Acquire) != self.seen }
}

fn atomic_write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).unwrap(); }
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, bytes).unwrap();
    std::fs::rename(temp, path).unwrap();
}

fn control_input(rt: &mut crate::Runtime<HostState>, command: &Value) {
    if let Some(tap) = command.get("tap") {
        if let Some((x, y)) = tap.as_str().and_then(|key| rt.find(key)) {
            rt.pointer_move(x, y);
            rt.pointer_down(x, y);
            rt.pointer_up(x, y);
        } else {
            eprintln!("rune-hot: skipping tap: expected an existing element key");
        }
    }
    if let Some(scroll) = command.get("scroll") {
        match serde_json::from_value::<[f32; 3]>(scroll.clone()) {
            Ok([x, y, dy]) if [x, y, dy].iter().all(|v| v.is_finite()) => rt.wheel(x, y, dy),
            _ => eprintln!("rune-hot: skipping scroll: expected three finite numbers [x,y,dy]"),
        }
    }
}

/// Persistent offscreen GPU session. A control file is an atomically replaced JSON request:
/// `{"id":1,"tap":"login"}`, `{"id":2,"scroll":[800,400,100]}`,
/// `{"id":3,"pause":true}`, `{"id":4,"step":0.05}`, or `{"id":5,"quit":true}`.
/// Every request and patch publishes a PNG + JSON tree, then a flushed completion marker.
fn headless(spec: AppSpec<HostState>, args: Args) {
    assert!(enabled(), "--snap-on-patch requires --hot (use scripts/rune-dev)");
    let size = args.size.unwrap_or(spec.size);
    let scale = args.scale.unwrap_or(2.0);
    let state = initial_state(&spec, args.scene.as_deref());
    let (wake, sleep) = std::sync::mpsc::channel();
    let mut rt = runtime_for(&spec, state, size, Env { system_dark: args.theme.as_deref() != Some("light"), scale }, Arc::new(move || { let _ = wake.send(()); }));
    let gpu = Gpu::new(Gpu::instance(), None);
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let (w, h) = ((size.0 * scale).round() as u32, (size.1 * scale).round() as u32);
    let target = Offscreen::new(&gpu.device, w, h);
    let png = args.snap_on_patch.as_ref().unwrap();
    let mut first = true;
    let mut paused = false;
    let mut command = Value::Null;
    let mut previous = Instant::now();
    let mut woken = true;
    loop {
        let now = Instant::now();
        let elapsed = (now - previous).as_secs_f64();
        // `None`: a frame `elapsed` after the last, under the shared frame rule.
        let mut fixed = (first || paused).then_some(0.0);
        previous = now;
        let mut requested = false;
        if !first && let Some(path) = &args.hot_control
            && let Ok(bytes) = std::fs::read(path)
            && let Ok(next) = serde_json::from_slice::<Value>(&bytes)
            && next != command
        {
            command = next;
            if command["quit"] == true { break; }
            if let Some(pause) = command["pause"].as_bool() { paused = pause; fixed = Some(0.0); }
            if let Some(step) = command["step"].as_f64() { fixed = Some(step.clamp(0.0, 10.0)); }
            control_input(&mut rt, &command);
            requested = true;
        }
        let timer_due = rt.wake_deadline().is_some_and(|deadline| deadline <= rt.clock + elapsed);
        if !first && !requested && !rt.hot.as_ref().unwrap().pending() && (paused || (!woken && !rt.animating && !timer_due)) {
            // Paused/idle sessions must not compete with their own patch compiler.
            if !paused { rt.clock += elapsed; }
            woken = sleep.recv_timeout(Duration::from_millis(16)).is_ok();
            continue;
        }
        let before = rt.hot.as_ref().unwrap().seen;
        match fixed {
            Some(dt) => { rt.frame(dt); }
            None => { crate::frame_clock::advance_by(&mut rt, elapsed); }
        }
        let generation = rt.hot.as_ref().unwrap().seen;
        if first || requested || before != generation {
            renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
            renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (w, h));
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            renderer.draw(&mut enc, &target.view, rt.clear_color());
            target.copy(&mut enc);
            gpu.queue.submit([enc.finish()]);
            let pixels = target.read(&gpu.device);
            let pixels_ns = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let temp = png.with_extension("tmp.png");
            if let Some(parent) = temp.parent() { std::fs::create_dir_all(parent).unwrap(); }
            let mut encoder = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&temp).unwrap()), w, h);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fastest);
            encoder.write_header().unwrap().write_image_data(&pixels).expect("hot snapshot");
            std::fs::rename(temp, png).expect("publish hot snapshot");
            let mut tree = tree_json(&rt, rt.clock);
            tree["hot"] = json!({"generation": generation, "pid": std::process::id(), "state": format!("{:p}", rt.state.value.as_ref() as *const dyn Any as *const ()), "request": command["id"], "pixels_ns": pixels_ns, "scrolls": rt.hot_scrolls(), "motion": rt.hot_motion()});
            atomic_write(&png.with_extension("json"), &serde_json::to_vec_pretty(&tree).unwrap());
            println!("rune-hot-pixels {}", tree["hot"]);
            use std::io::Write;
            std::io::stdout().flush().unwrap();
            first = false;
        }
        // The patch callback wakes immediately; control files are checked at a bounded cadence.
        woken = sleep.recv_timeout(Duration::from_millis(16)).is_ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Key, TextEdit, col, el, text_input};

    struct State { pressed: bool, hovered: bool, secondary: bool, changed: bool, keyed: bool, edit: TextEdit }

    fn runtime<S: Default + 'static>(view: fn(&S, &mut Cx<S>) -> El) -> crate::Runtime<HostState> {
        let spec = AppSpec::new("test", (300.0, 200.0), S::default, view);
        let host = HostState { value: Box::new((spec.init)()), spec: Box::new(spec), view: view_adapter::<S>, poll: poll_adapter::<S>, setup: setup_adapter::<S>, key: key_adapter::<S> };
        crate::Runtime::new(host, |s, cx| (s.view)(s, cx), (300.0, 200.0), Env::new(true, 1.0))
    }

    #[test]
    fn panicking_patch_falls_back_without_losing_state_or_context() {
        fn original(s: &usize, _: &mut Cx<usize>) -> El {
            el().key(format!("count-{s}")).on_press(|s: &mut usize| *s += 1)
        }
        let mut rt = runtime(original);
        *rt.state.value.downcast_mut::<usize>().unwrap() = 7;
        let address = rt.state.value.as_ref() as *const dyn Any as *const ();
        rt.state.view = |host, cx| {
            let state = host.value.downcast_ref::<usize>().unwrap();
            let original = host.spec.downcast_ref::<AppSpec<usize>>().unwrap().view;
            view_or_original(original, state, cx, |_, typed| {
                typed.time_scale(0.125);
                panic!("broken patched view")
            })
        };
        rt.frame(0.0);
        assert!(rt.find("count-7").is_some());
        assert_eq!(rt.time_scale, 1.0, "failed view's context writes must be discarded");
        control_input(&mut rt, &json!({"tap": "count-7"}));
        rt.frame(0.0);
        assert!(rt.find("count-8").is_some(), "original handlers still operate on retained state");
        assert_eq!(rt.state.value.as_ref() as *const dyn Any as *const (), address);
        // A later healthy patch is still attempted rather than latching the failure.
        rt.state.view = |host, cx| {
            let state = host.value.downcast_ref::<usize>().unwrap();
            view_or_original(original, state, cx, |s, _| el().key(format!("recovered-{s}")))
        };
        rt.update(|_, _| {});
        rt.frame(0.0);
        assert!(rt.find("recovered-8").is_some());
    }

    #[test]
    fn invalid_control_input_is_skipped_and_later_requests_still_work() {
        let mut rt = runtime(|_: &usize, _: &mut Cx<usize>| el().key("button").on_press(|s: &mut usize| *s += 1));
        rt.frame(0.0);
        for command in [
            json!({"tap": "missing"}), json!({"tap": 1}),
            json!({"scroll": null}), json!({"scroll": "down"}),
            json!({"scroll": [1, 2]}), json!({"scroll": [1, 2, 3, 4]}),
            json!({"scroll": [1, "bad", 3]}), json!({"scroll": [1, 2, 1e100]}),
        ] {
            control_input(&mut rt, &command);
            rt.frame(0.0);
        }
        assert_eq!(*rt.state.value.downcast_ref::<usize>().unwrap(), 0);
        control_input(&mut rt, &json!({"tap": "button", "scroll": [10, 10, 20]}));
        rt.frame(0.0);
        assert_eq!(*rt.state.value.downcast_ref::<usize>().unwrap(), 1);
    }

    #[test]
    fn erased_host_routes_focus_physical_links_and_window_callbacks() {
        #[derive(Default)]
        struct Events { presses: usize, physical: usize, dismisses: usize, links: Vec<String>, paste: String, focused: bool, moved: bool, layout: Option<[f32; 4]> }
        fn view(_: &Events, _: &mut Cx<Events>) -> El {
            col().size(300.0, 200.0)
                .on_paste(|s: &mut Events, text, _| s.paste = text.into())
                .on_window_focus(|s: &mut Events, focused, _| s.focused = focused)
                .modal(false, |s: &mut Events| s.dismisses += 1)
                .child(el().key("button").size(100.0, 30.0).focusable(true)
                    .on_press(|s: &mut Events| s.presses += 1)
                    .on_pointer_move(|s: &mut Events, _| s.moved = true)
                    .on_layout(|s: &mut Events, rect| s.layout = Some(rect))
                    .on_physical_input(|s: &mut Events, ev, _| {
                        if matches!(ev, crate::PhysicalInput::Modifiers(_)) { s.physical += 1; }
                        false
                    }))
                .child(crate::text("docs").link(crate::Link::new(0..4, "docs"))
                    .on_link(|s: &mut Events, id, _| s.links.push(id.into())))
        }
        let mut rt = runtime(view);
        rt.frame(0.0);
        rt.frame(0.0); // Deliver the first layout report.
        rt.window_focus(true);
        rt.paste("window paste");
        // Opening the modal focuses its first control.
        rt.key(KeyEvent { key: Key::Enter, mods: Default::default() });
        rt.physical_input(crate::PhysicalInput::Modifiers(Default::default()));
        rt.frame(0.0);
        let (x, y) = rt.find("button").unwrap();
        rt.pointer_move(x, y);
        rt.key(KeyEvent { key: Key::Tab, mods: Default::default() });
        rt.key(KeyEvent { key: Key::Enter, mods: Default::default() });
        rt.key(KeyEvent { key: Key::Escape, mods: Default::default() });
        rt.frame(0.0);
        let s = rt.state.value.downcast_ref::<Events>().unwrap();
        assert_eq!((s.presses, s.physical, s.dismisses), (1, 1, 1));
        assert_eq!(s.links, ["docs"]);
        assert_eq!(s.paste, "window paste");
        assert!(s.focused && s.moved);
        assert_eq!(s.layout.unwrap()[2..], [100.0, 30.0]);
    }

    #[test]
    fn erased_host_preserves_context_reads_and_host_motion_preference() {
        #[derive(Default)]
        struct Reads { edit: TextEdit, seen: std::cell::RefCell<(bool, Option<[f32; 4]>, Option<[f32; 4]>)> }
        fn view(s: &Reads, cx: &mut Cx<Reads>) -> El {
            *s.seen.borrow_mut() = (cx.system_reduced_motion(), cx.layout("input"), cx.caret("input"));
            col().child(text_input(&s.edit, crate::TextStyle::new(14.0, 400.0)).key("input").size(180.0, 30.0)
                .on_edit(|s: &mut Reads| &mut s.edit))
        }
        let mut rt = runtime(view);
        rt.set_system_reduced_motion(true);
        rt.frame(0.0);
        let (x, y) = rt.find("input").unwrap();
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.paste("caret");
        rt.frame(0.0);
        rt.frame(0.0);
        let s = rt.state.value.downcast_ref::<Reads>().unwrap();
        let (reduced, layout, caret) = *s.seen.borrow();
        assert!(reduced);
        assert_eq!(layout.unwrap()[2..], [180.0, 30.0]);
        assert!(caret.unwrap()[0] > 0.0);
        assert_eq!(s.edit.text, "caret");
    }

    #[test]
    fn erased_host_routes_handlers_and_context_to_the_typed_state() {
        fn view(s: &State, cx: &mut Cx<State>) -> El {
            cx.time_scale(0.75);
            col().child(el().key("button").size(80.0, 30.0).on_press(|s: &mut State| s.pressed = true)
                    .on_hover(|s: &mut State| s.hovered = true)
                    .on_secondary_press(|s: &mut State, _, _| s.secondary = true))
                .child(text_input(&s.edit, crate::tok::TextStyle::new(14.0, 400.0)).key("input").size(180.0, 30.0)
                    .on_input_change(|s: &mut State, _| s.changed = true)
                    .on_input_key(|s: &mut State, _, _| { s.keyed = true; false })
                    .on_edit(|s: &mut State| &mut s.edit).on_submit(|s: &mut State, tx| { tx.copy(s.edit.text.clone()); }))
        }
        let spec = AppSpec::new("test", (200.0, 150.0), || State { pressed: false, hovered: false, secondary: false, changed: false, keyed: false, edit: TextEdit::new("") }, view);
        let host = HostState { value: Box::new((spec.init)()), spec: Box::new(spec), view: view_adapter::<State>, poll: poll_adapter::<State>, setup: setup_adapter::<State>, key: key_adapter::<State> };
        let mut rt = crate::Runtime::new(host, |s, cx| (s.view)(s, cx), (200.0, 150.0), Env { system_dark: true, scale: 1.0 });
        rt.frame(0.0);
        assert_eq!(rt.time_scale, 0.75);
        let (x, y) = rt.find("button").unwrap();
        rt.secondary_down(x, y);
        rt.secondary_up(x, y);
        rt.frame(0.0);
        for key in ["button", "input"] {
            let (x, y) = rt.find(key).unwrap();
            rt.pointer_move(x, y);
            rt.pointer_down(x, y);
            rt.pointer_up(x, y);
            rt.frame(0.0);
        }
        rt.paste("state kept");
        rt.frame(0.0);
        rt.key(KeyEvent { key: Key::Enter, mods: Default::default() });
        rt.frame(0.0);
        let s = rt.state.value.downcast_ref::<State>().unwrap();
        assert!(s.pressed);
        assert!(s.hovered);
        assert!(s.secondary && s.changed && s.keyed);
        assert_eq!(s.edit.text, "state kept");
        assert_eq!(rt.copy_request.as_deref(), Some("state kept"));
    }
}

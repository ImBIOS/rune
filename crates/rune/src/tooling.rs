//! Agent tooling: headless deterministic rendering, synthetic input scripts, element-tree dumps,
//! motion-lint traces and frame strips. Same renderer as the window, fixed virtual clock.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};

use crate::edit::{Key, KeyEvent, Mods};
use crate::el::{El, Tx};
use crate::render::{Gpu, OFFSCREEN_FORMAT, Offscreen, Renderer, write_png};
use crate::runtime::{Cx, Env, Runtime, Waker};
use crate::text::TextSystem;

pub struct AppSpec<S: 'static> {
    pub title: &'static str,
    pub size: (f32, f32),
    pub init: fn() -> S,
    pub view: fn(&S, &mut Cx<S>) -> El,
    /// Named states for snapshots: each modifies the initial state.
    pub scenes: &'static [(&'static str, fn(&mut S))],
    /// Font files replacing the built-in Inter (empty keeps it), and the sans/mono family names.
    pub fonts: &'static [&'static [u8]],
    pub families: (&'static str, &'static str),
    /// Called once with a waker other threads use when new data arrives.
    pub setup: Option<fn(&mut S, Waker)>,
    /// Called at the start of every frame with seconds since start (virtual when headless);
    /// true when state changed.
    pub poll: Option<fn(&mut S, f64, &mut Tx) -> bool>,
    /// App-level keys, before the focused text field; true consumes the key.
    pub on_key: Option<fn(&mut S, &KeyEvent, &mut Tx) -> bool>,
}

static EXTRA: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
static APP_ID: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// The application id the window reports to the desktop (Wayland `app_id`, X11 `WM_CLASS`), so
/// compositors match it to the app's `.desktop` entry, icon and window rules. Call before `run`.
pub fn set_app_id(id: &'static str) {
    let _ = APP_ID.set(id);
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn app_id() -> Option<&'static str> {
    APP_ID.get().copied()
}

/// Command-line arguments Rune didn't recognise, in order, for the app to interpret.
pub fn extra_args() -> &'static [String] {
    EXTRA.get().map_or(&[], |v| v.as_slice())
}

pub(crate) fn runtime_for<S: 'static>(spec: &AppSpec<S>, state: S, size: (f32, f32), env: Env, waker: Waker) -> Runtime<S> {
    let text = TextSystem::new(spec.fonts, spec.families);
    let mut rt = Runtime::with_text(state, spec.view, size, env, text);
    rt.poll = spec.poll;
    rt.on_key = spec.on_key;
    rt.set_live_waker(waker.clone());
    if let Some(f) = spec.setup {
        f(&mut rt.state, waker);
    }
    rt
}

#[derive(Default, Debug)]
pub struct Args {
    pub snap: Option<PathBuf>,
    pub scene: Option<String>,
    pub at: Option<f64>,
    pub size: Option<(f32, f32)>,
    pub scale: Option<f32>,
    pub theme: Option<String>,
    pub script: Option<PathBuf>,
    pub frames: Option<usize>,
    pub start: f64,
    pub fps: f64,
    pub out: Option<PathBuf>,
    pub tree: Option<PathBuf>,
    pub trace: Option<PathBuf>,
    pub strip: Option<PathBuf>,
    pub crops: Vec<[f32; 4]>,
    pub every: usize,
    pub cols: usize,
    pub list_scenes: bool,
    pub first_pixels: bool,
}

impl Args {
    pub fn parse() -> Args {
        Args::parse_from(std::env::args().skip(1).collect())
    }

    /// Parses `argv` (without the program name); unrecognised arguments become `extra_args`.
    pub fn parse_from(argv: Vec<String>) -> Args {
        let mut a = Args { fps: 60.0, every: 1, cols: 10, ..Default::default() };
        let mut extra = Vec::new();
        let mut i = 0;
        let val = |i: &mut usize| -> String {
            *i += 1;
            argv.get(*i).cloned().unwrap_or_else(|| die(&format!("{} needs a value", argv[*i - 1])))
        };
        while i < argv.len() {
            match argv[i].as_str() {
                "--snap" => a.snap = Some(val(&mut i).into()),
                "--scene" => a.scene = Some(val(&mut i)),
                "--at" => a.at = Some(num(&val(&mut i))),
                "--size" => {
                    let v = val(&mut i);
                    let (w, h) = v.split_once('x').unwrap_or_else(|| die("--size is WxH"));
                    a.size = Some((num(w) as f32, num(h) as f32));
                }
                "--scale" => a.scale = Some(num(&val(&mut i)) as f32),
                "--theme" => a.theme = Some(val(&mut i)),
                "--script" => a.script = Some(val(&mut i).into()),
                "--frames" => a.frames = Some(num(&val(&mut i)) as usize),
                "--start" => a.start = num(&val(&mut i)),
                "--fps" => a.fps = num(&val(&mut i)),
                "--out" => a.out = Some(val(&mut i).into()),
                "--tree" => a.tree = Some(val(&mut i).into()),
                "--trace" => a.trace = Some(val(&mut i).into()),
                "--strip" => a.strip = Some(val(&mut i).into()),
                "--crop" => {
                    let v: Vec<f32> = val(&mut i).split(',').map(|s| num(s) as f32).collect();
                    if v.len() != 4 {
                        die("--crop is x,y,w,h in points");
                    }
                    a.crops.push([v[0], v[1], v[2], v[3]]);
                }
                "--cols" => a.cols = (num(&val(&mut i)) as usize).max(1),
                "--every" => a.every = (num(&val(&mut i)) as usize).max(1),
                "--list-scenes" => a.list_scenes = true,
                "--first-pixels" => a.first_pixels = true,
                "-h" | "--help" => {
                    eprintln!("{HELP}");
                    std::process::exit(0);
                }
                other => extra.push(other.to_owned()),
            }
            i += 1;
        }
        let _ = EXTRA.set(extra);
        a
    }

    pub fn headless(&self) -> bool {
        self.snap.is_some()
            || self.tree.is_some()
            || self.trace.is_some()
            || self.frames.is_some()
            || self.strip.is_some()
            || self.list_scenes
            || self.first_pixels
    }
}

const HELP: &str = "usage: <app> [window]
  --snap out.png        render offscreen (no window) at --at seconds
  --scene NAME          start from a named scene (--list-scenes)
  --at SECONDS          virtual time of the snapshot/tree (default: end of frames or script)
  --size WxH            viewport in points      --scale N   device scale (default 2)
  --theme dark|light    system appearance
  --script FILE.json    timestamped pointer events (move/down/up/tap/drag, by x,y or key)
  --frames N --fps 60 --start S --out DIR     dump N frames from S seconds
  --strip out.png --crop x,y,w,h --every K    tile dumped frames into one image
                        (repeat --crop to stack several regions per frame; --cols N per row)
  --tree out.json       element tree with animated values
  --trace out.jsonl     per-frame rects/opacity in motion-lint JSONL
  --first-pixels        report when the first GPU-complete offscreen pixels are readable
  other arguments are passed to the app (rune::tooling::extra_args)";

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2)
}

fn num(s: &str) -> f64 {
    s.trim().parse().unwrap_or_else(|_| die(&format!("not a number: {s}")))
}

/// Opens the app's window and quits when it closes (`App` for other lifetimes), or renders
/// headless for the tooling arguments.
pub fn run<S: 'static>(spec: AppSpec<S>) {
    crate::app::App::new(spec).run();
}

pub fn initial_state<S: 'static>(spec: &AppSpec<S>, scene: Option<&str>) -> S {
    with_scene(spec, (spec.init)(), scene)
}

/// `state` with the named scene applied.
pub(crate) fn with_scene<S: 'static>(spec: &AppSpec<S>, mut s: S, scene: Option<&str>) -> S {
    if let Some(name) = scene {
        let Some((_, f)) = spec.scenes.iter().find(|(n, _)| *n == name) else {
            let names: Vec<_> = spec.scenes.iter().map(|(n, _)| *n).collect();
            die(&format!("no scene {name:?}; scenes: {}", names.join(", ")));
        };
        f(&mut s);
    }
    s
}

// -------------------------------------------------------------------------------------------
// Scripts

#[derive(Clone, Debug)]
enum Pos {
    Xy(f32, f32),
    Key(String),
    /// Relative to the last pointer-down position.
    Rel(f32, f32),
}

#[derive(Clone, Debug)]
enum Kind {
    Move,
    Down,
    Up,
    Key(KeyEvent),
    Paste(String),
    Wheel(f32),
    Fling(f32),
}

fn key_named(name: &str) -> Option<Key> {
    Some(match name {
        "Enter" => Key::Enter,
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "Left" => Key::Left,
        "Right" => Key::Right,
        "Up" => Key::Up,
        "Down" => Key::Down,
        "Home" => Key::Home,
        "End" => Key::End,
        "Escape" => Key::Escape,
        "Tab" => Key::Tab,
        f if f.len() > 1 && f.starts_with('F') && f[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => {
            Key::Function(f[1..].parse().unwrap())
        }
        c if c.chars().count() == 1 => Key::Text(c.to_owned()),
        _ => return None,
    })
}

#[derive(Clone, Debug)]
struct Ev {
    t: f64,
    kind: Kind,
    pos: Pos,
}

fn load_script(path: &PathBuf) -> Vec<Ev> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
    let list = v.get("events").cloned().unwrap_or(v);
    let Value::Array(items) = list else { die("script must be an array of events or {\"events\": [...]}") };
    let mut out = Vec::new();
    for e in items {
        let t = e["t"].as_f64().unwrap_or(0.0);
        let pos = if let Some(k) = e["key"].as_str() {
            Pos::Key(k.to_owned())
        } else if let (Some(x), Some(y)) = (e["x"].as_f64(), e["y"].as_f64()) {
            Pos::Xy(x as f32, y as f32)
        } else {
            Pos::Rel(0.0, 0.0)
        };
        let ty = e["type"].as_str().unwrap_or("tap");
        match ty {
            "move" => out.push(Ev { t, kind: Kind::Move, pos }),
            "down" => {
                out.push(Ev { t, kind: Kind::Move, pos: pos.clone() });
                out.push(Ev { t, kind: Kind::Down, pos });
            }
            "up" => out.push(Ev { t, kind: Kind::Up, pos }),
            "tap" | "click" => {
                out.push(Ev { t, kind: Kind::Move, pos: pos.clone() });
                out.push(Ev { t, kind: Kind::Down, pos });
                out.push(Ev { t: t + e["hold"].as_f64().unwrap_or(0.08), kind: Kind::Up, pos: Pos::Rel(0.0, 0.0) });
            }
            "drag" => {
                let dx = e["dx"].as_f64().unwrap_or(0.0) as f32;
                let dy = e["dy"].as_f64().unwrap_or(0.0) as f32;
                let dur = e["dur"].as_f64().unwrap_or(0.3).max(0.01);
                out.push(Ev { t, kind: Kind::Move, pos: pos.clone() });
                out.push(Ev { t, kind: Kind::Down, pos });
                let steps = (dur * 120.0).ceil() as usize;
                for s in 1..=steps {
                    let f = s as f32 / steps as f32;
                    // Ease in-out so the hand starts and stops gently.
                    let f = f * f * (3.0 - 2.0 * f);
                    out.push(Ev {
                        t: t + dur * s as f64 / steps as f64,
                        kind: Kind::Move,
                        pos: Pos::Rel(dx * f, dy * f),
                    });
                }
                out.push(Ev { t: t + dur + 0.02, kind: Kind::Up, pos: Pos::Rel(dx, dy) });
            }
            "type" => {
                let text = e["text"].as_str().unwrap_or("");
                let every = e["every"].as_f64().unwrap_or(0.035);
                for (k, c) in text.chars().enumerate() {
                    let ev = KeyEvent { key: Key::Text(c.to_string()), mods: Mods::default() };
                    out.push(Ev { t: t + every * k as f64, kind: Kind::Key(ev), pos: Pos::Rel(0.0, 0.0) });
                }
            }
            "key" => {
                let name = e["name"].as_str().unwrap_or("");
                let mods = Mods {
                    shift: e["shift"].as_bool().unwrap_or(false),
                    cmd: e["cmd"].as_bool().unwrap_or(false),
                    alt: e["alt"].as_bool().unwrap_or(false),
                    ctrl: e["ctrl"].as_bool().unwrap_or(false),
                    logo: e["logo"].as_bool().unwrap_or(false),
                };
                let key = if mods.cmd && name.len() == 1 {
                    Key::Command(name.chars().next().unwrap().to_ascii_lowercase())
                } else {
                    key_named(name).unwrap_or_else(|| die(&format!("unknown key {name}")))
                };
                out.push(Ev { t, kind: Kind::Key(KeyEvent { key, mods }), pos: Pos::Rel(0.0, 0.0) });
            }
            "paste" => out.push(Ev { t, kind: Kind::Paste(e["text"].as_str().unwrap_or("").to_owned()), pos: Pos::Rel(0.0, 0.0) }),
            "scroll" => {
                // A trackpad swipe: deltas eased over `dur`, then (with "fling") the release velocity.
                let dy = e["dy"].as_f64().unwrap_or(0.0) as f32;
                let dur = e["dur"].as_f64().unwrap_or(0.2).max(0.01);
                out.push(Ev { t, kind: Kind::Move, pos: pos.clone() });
                let steps = (dur * 120.0).ceil() as usize;
                let mut prev = 0.0;
                for s in 1..=steps {
                    let f = s as f32 / steps as f32;
                    let f = f * (2.0 - f);
                    out.push(Ev { t: t + dur * s as f64 / steps as f64, kind: Kind::Wheel(dy * f - prev), pos: pos.clone() });
                    prev = dy * f;
                }
                if e["fling"].as_bool().unwrap_or(false) {
                    let v = 2.0 * dy / dur as f32 * 0.25;
                    out.push(Ev { t: t + dur + 1e-4, kind: Kind::Fling(v), pos });
                }
            }
            "fling" => out.push(Ev { t, kind: Kind::Fling(e["v"].as_f64().unwrap_or(0.0) as f32), pos }),
            other => die(&format!("unknown event type {other}")),
        }
    }
    out.sort_by(|a, b| a.t.total_cmp(&b.t));
    out
}

// -------------------------------------------------------------------------------------------
// motion-lint trace (the format Scrawl writes)

struct Trace {
    out: std::io::BufWriter<std::fs::File>,
    ids: HashMap<String, u32>,
    described: HashSet<u32>,
    states: HashMap<u32, ([i64; 6], u32, [i64; 4])>,
}

fn r2(v: f32) -> f64 {
    (f64::from(v) * 100.0).round() / 100.0
}

impl Trace {
    fn open(path: &PathBuf, size: (f32, f32), fps: f64, scale: f32) -> Trace {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let file = std::fs::File::create(path).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
        let mut out = std::io::BufWriter::new(file);
        let header = json!({
            "type": "header", "launch": "rune", "width": size.0, "height": size.1,
            "hz": fps, "scale": scale, "platform": "rune", "units": "pt", "clock": "virtual",
        });
        let _ = writeln!(out, "{header}");
        Trace { out, ids: HashMap::new(), described: HashSet::new(), states: HashMap::new() }
    }

    fn id(&mut self, key: &str) -> u32 {
        let next = self.ids.len() as u32 + 1;
        *self.ids.entry(key.to_owned()).or_insert(next)
    }

    fn frame<S>(&mut self, rt: &mut Runtime<S>, t_ms: f64, cpu_ms: f64) {
        let els = rt.elements();
        let settled = rt.settled;
        let mut metas = Vec::new();
        let mut changed = Vec::new();
        let mut seen = HashSet::new();
        // A virtual list's invisible estimated extent changes when rows are measured or the
        // window rebases. Record the actual row surfaces, not that bookkeeping rectangle.
        for e in els.iter().filter(|e| e.key.is_some() && !e.virtual_extent) {
            let id = self.id(&e.path);
            seen.insert(id);
            if self.described.insert(id) {
                let mut meta = json!({ "id": id, "cls": "rune", "nm": e.path, "host": "rune", "role": e.role });
                if let Some(p) = e.parent.and_then(|p| els.get(p)).filter(|p| p.key.is_some() && !p.virtual_extent) {
                    meta["pv"] = self.id(&p.path).into();
                }
                metas.push(meta);
            }
            let [x, y, w, h] = e.rect;
            // motion-lint flags: animated 1, clipped 8. Own intents at << 8: position 1, size 2,
            // transition 16, reveal 32. A settle commit is a deliberate cut (everything lands at
            // once); a growth row reveals its content by growing; reveals fade words in.
            let clipped = e.rect.iter().zip(e.visible.iter()).any(|(a, b)| (a - b).abs() > 0.5);
            let mut intent = e.live_intent;
            if settled {
                intent |= 1 | 2 | 16 | 32;
            }
            // Reduced motion deliberately cuts geometry. Opacity still needs to fade;
            // do not declare transition/opacity intent for the whole reduced-motion tree.
            if rt.reduced_motion {
                intent |= 1 | 2;
            }
            if e.revealing {
                intent |= 32;
            }
            if e.in_growing_row {
                intent |= 2 | 32;
            }
            let flags = u32::from(e.moving || e.in_growing_row) | if e.scrolling { 4 } else { 0 } | if clipped { 8 } else { 0 } | intent << 8;
            let q = [
                (f64::from(x) * 100.0).round() as i64,
                (f64::from(y) * 100.0).round() as i64,
                (f64::from(w) * 100.0).round() as i64,
                (f64::from(h) * 100.0).round() as i64,
                (f64::from(e.opacity) * 1000.0).round() as i64,
                (f64::from(e.radius) * 100.0).round() as i64,
            ];
            let state = (q, flags, e.visible.map(|v| (v * 100.0).round() as i64));
            if self.states.get(&id) != Some(&state) {
                self.states.insert(id, state);
                let mut row = vec![
                    json!(id),
                    json!(r2(x)),
                    json!(r2(y)),
                    json!(r2(w)),
                    json!(r2(h)),
                    json!((f64::from(e.opacity) * 1000.0).round() / 1000.0),
                    json!(r2(e.radius)),
                    json!(flags),
                ];
                if clipped {
                    row.extend(e.visible.map(|v| json!(r2(v))));
                }
                changed.push(Value::Array(row));
            }
        }
        let gone: Vec<u32> = self.states.keys().copied().filter(|id| !seen.contains(id)).collect();
        // A settle unmounts what it replaces at once: a cut (transition | opacity) at its last place.
        let mut cuts = Vec::new();
        for id in &gone {
            if let Some((q, ..)) = self.states.remove(id)
                && settled
            {
                let [x, y, w, h] = [q[0], q[1], q[2], q[3]].map(|v| v as f64 / 100.0);
                cuts.push(json!([id, x, y, w, h, 16 | 4, 0]));
            }
        }

        let mut line = json!({ "t": (t_ms * 1000.0).round() / 1000.0, "n": 1 });
        let (edges, position_error) = rt.scene.sampling_error();
        let retarget = rt.scene.store_mut().take_retarget_audit();
        line["quality"] = json!({
            "cpu_ms": cpu_ms, "scene_edges": edges, "position_error_px": position_error,
            "retarget_lanes": retarget.lanes, "velocity_error_pt_s": retarget.max_velocity_error,
            "animating": rt.animating,
        });
        let events = std::mem::take(&mut rt.events);
        if !events.is_empty() {
            line["ev"] = events.into();
        }
        if !metas.is_empty() {
            line["m"] = metas.into();
        }
        if !changed.is_empty() {
            line["e"] = changed.into();
        }
        if !gone.is_empty() {
            line["g"] = gone.into();
        }
        if !cuts.is_empty() {
            line["ai"] = cuts.into();
        }
        let _ = writeln!(self.out, "{line}");
    }
}

fn tree_json<S>(rt: &Runtime<S>, real_t: f64) -> Value {
    let els: Vec<Value> = rt
        .elements()
        .iter()
        .map(|e| {
            let mut v = json!({
                "path": e.path,
                "role": e.role,
                "rect": e.rect.map(r2),
                "opacity": (e.opacity * 1000.0).round() / 1000.0,
                "fill": e.fill.hex(),
            });
            if let Some(k) = &e.key {
                v["key"] = k.clone().into();
            }
            if let Some(l) = &e.label {
                v["label"] = l.clone().into();
            }
            if let Some(t) = &e.text {
                v["text"] = t.clone().into();
            }
            if let Some(c) = e.text_color {
                v["color"] = c.hex().into();
            }
            if e.rect.iter().zip(e.target.iter()).any(|(a, b)| (a - b).abs() > 0.01) {
                v["target"] = e.target.map(r2).to_vec().into();
            }
            if e.moving {
                v["moving"] = true.into();
            }
            if e.hover > 0.001 {
                v["hover"] = r2(e.hover).into();
            }
            if e.press > 0.001 {
                v["press"] = r2(e.press).into();
            }
            v
        })
        .collect();
    json!({
        "t": real_t,
        "anim_t": rt.t,
        "viewport": [rt.viewport.0, rt.viewport.1],
        "scale": rt.env.scale,
        "dark_system": rt.env.system_dark,
        "reduced_motion": rt.reduced_motion,
        "time_scale": rt.time_scale,
        "animating": rt.animating,
        "elements": els,
    })
}

// -------------------------------------------------------------------------------------------
// Headless run

pub fn headless<S: 'static>(app: crate::app::App<S>, args: Args) {
    let launched = Instant::now();
    if args.list_scenes {
        for (n, _) in app.spec.scenes {
            println!("{n}");
        }
        return;
    }
    let (spec, state, shared) = app.into_parts(args.scene.as_deref());
    let scale = args.scale.unwrap_or(2.0);
    let size = args.size.unwrap_or(spec.size);
    let dark = args.theme.as_deref() != Some("light");
    let state_ms = launched.elapsed().as_secs_f64() * 1000.0;
    let mut rt = runtime_for(&spec, state, size, Env { system_dark: dark, scale }, std::sync::Arc::new(|| {}));
    let runtime_ms = launched.elapsed().as_secs_f64() * 1000.0 - state_ms;

    let script = args.script.as_ref().map(load_script).unwrap_or_default();
    let fps = args.fps.max(1.0);
    let dt = 1.0 / fps;
    let start_frame = (args.start * fps).round() as usize;
    let frames = args.frames.unwrap_or(0);
    let script_end = script.last().map_or(0.0, |e| e.t);
    let snap_t = args.at.unwrap_or(if frames > 0 {
        (start_frame + frames - 1) as f64 * dt
    } else if script.is_empty() {
        0.0
    } else {
        script_end + 1.0
    });
    let snap_frame = (snap_t * fps).round() as usize;
    let last_frame = snap_frame.max(start_frame + frames.max(1) - 1);

    let t0 = Instant::now();
    let gpu = Gpu::new(Gpu::instance(), None);
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let (wp, hp) = ((size.0 * scale).round() as u32, (size.1 * scale).round() as u32);
    let target = Offscreen::new(&gpu.device, wp, hp);
    let init_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let mut trace = args.trace.as_ref().map(|p| Trace::open(p, size, fps, scale));
    rt.scene.store_mut().audit_retargets(trace.is_some());
    let mut strip: Vec<Vec<u8>> = Vec::new();
    let crops_px: Vec<[u32; 4]> = args.crops.iter().map(|c| c.map(|v| (v * scale).round() as u32)).collect();
    let gap = (4.0 * scale) as u32;
    let mut cpu_ms = Vec::new();
    let mut anim_ms = Vec::new();
    let mut rest_ms = Vec::new();
    let mut next_ev = 0;
    let mut last_down = (0.0f32, 0.0f32);

    for k in 0..=last_frame {
        let real_t = k as f64 * dt;
        let frame_start = Instant::now();
        while next_ev < script.len() && script[next_ev].t <= real_t + 1e-9 {
            let e = &script[next_ev];
            next_ev += 1;
            let (x, y) = match &e.pos {
                Pos::Xy(x, y) => (*x, *y),
                Pos::Key(key) => rt.find(key).unwrap_or_else(|| die(&format!("script: no element with key {key:?}"))),
                Pos::Rel(dx, dy) => match e.kind {
                    Kind::Wheel(_) | Kind::Fling(_) => rt.pointer().unwrap_or((0.0, 0.0)),
                    _ => (last_down.0 + dx, last_down.1 + dy),
                },
            };
            match &e.kind {
                Kind::Move => rt.pointer_move(x, y),
                Kind::Down => {
                    last_down = (x, y);
                    rt.pointer_down(x, y);
                }
                Kind::Up => rt.pointer_up(x, y),
                Kind::Key(k) => rt.key(k.clone()),
                Kind::Paste(s) => rt.paste(s.clone()),
                Kind::Wheel(dy) => rt.wheel(x, y, *dy),
                Kind::Fling(v) => rt.fling(x, y, *v),
            }
        }
        for f in shared.take().updates {
            rt.update(f);
        }
        let animating = rt.frame(if k == 0 { 0.0 } else { dt });
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (wp, hp));
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        renderer.draw(&mut enc, &target.view, rt.clear_color());
        let dump = k >= start_frame && k < start_frame + frames && (args.out.is_some() || args.strip.is_some()) && (k - start_frame).is_multiple_of(args.every);
        let want_snap = k == snap_frame && args.snap.is_some();
        let want_pixels = dump || want_snap || (args.first_pixels && k == 0);
        if want_pixels {
            target.copy(&mut enc);
        }
        gpu.queue.submit([enc.finish()]);
        let ms = frame_start.elapsed().as_secs_f64() * 1000.0;
        cpu_ms.push(ms);
        if k > 0 {
            if animating { anim_ms.push(ms) } else { rest_ms.push(ms) }
        }

        if let Some(tr) = trace.as_mut() {
            tr.frame(&mut rt, real_t * 1000.0, ms);
        }
        if want_pixels {
            let wait = Instant::now();
            let px = target.read(&gpu.device);
            if args.first_pixels && k == 0 {
                let marker = json!({
                    "headless_ms": launched.elapsed().as_secs_f64() * 1000.0,
                    "state_ms": state_ms, "runtime_ms": runtime_ms, "gpu_init_ms": init_ms,
                    "frame_cpu_ms": ms, "readback_ms": wait.elapsed().as_secs_f64() * 1000.0,
                    "pixel_bytes": px.len(),
                });
                println!("rune-first-pixels {marker}");
                std::io::stdout().flush().unwrap();
            }
            if dump {
                if let Some(dir) = &args.out {
                    let p = dir.join(format!("frame_{:04}.png", k - start_frame));
                    write_png(&p, wp, hp, &px).unwrap_or_else(|e| die(&format!("{}: {e}", p.display())));
                }
                if args.strip.is_some() {
                    strip.push(tile(&px, wp, hp, &crops_px, gap));
                }
            }
            if want_snap {
                let p = args.snap.as_ref().unwrap();
                write_png(p, wp, hp, &px).unwrap_or_else(|e| die(&format!("{}: {e}", p.display())));
            }
        }
        if k == snap_frame
            && let Some(p) = &args.tree
        {
            let text = serde_json::to_string_pretty(&tree_json(&rt, real_t)).unwrap();
            std::fs::write(p, text).unwrap_or_else(|e| die(&format!("{}: {e}", p.display())));
        }
    }
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();

    if let (Some(p), false) = (&args.strip, strip.is_empty()) {
        let (cw, ch) = tile_size(wp, hp, &crops_px, gap);
        let cols = strip.len().min(args.cols) as u32;
        let rows = (strip.len() as u32).div_ceil(cols);
        let (sw, sh) = (cols * cw + (cols - 1) * gap, rows * ch + (rows - 1) * gap);
        let mut img: Vec<u8> = [128, 128, 128, 255].repeat((sw * sh) as usize);
        for (i, f) in strip.iter().enumerate() {
            let (cx, cy) = ((i as u32 % cols) * (cw + gap), (i as u32 / cols) * (ch + gap));
            for y in 0..ch {
                let dst = (((cy + y) * sw + cx) * 4) as usize;
                let src = (y * cw * 4) as usize;
                img[dst..dst + (cw * 4) as usize].copy_from_slice(&f[src..src + (cw * 4) as usize]);
            }
        }
        write_png(p, sw, sh, &img).unwrap_or_else(|e| die(&format!("{}: {e}", p.display())));
    }

    let mut sorted = cpu_ms.clone();
    sorted.sort_by(f64::total_cmp);
    let pct = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
    let steady: Vec<f64> = cpu_ms.iter().skip(1).copied().collect();
    let mean = if steady.is_empty() { cpu_ms[0] } else { steady.iter().sum::<f64>() / steady.len() as f64 };
    eprintln!(
        "rune: {} frames at {fps} fps, gpu init {init_ms:.1} ms, first frame {:.2} ms, frame cpu mean {mean:.3} ms p50 {:.3} p95 {:.3} max {:.3}, {} instances",
        cpu_ms.len(),
        cpu_ms[0],
        pct(0.5),
        pct(0.95),
        sorted[sorted.len() - 1],
        rt.instances.len()
    );
    let q = |v: &mut Vec<f64>, p: f64| {
        v.sort_by(f64::total_cmp);
        if v.is_empty() { 0.0 } else { v[((v.len() - 1) as f64 * p).round() as usize] }
    };
    let (a50, a95) = (q(&mut anim_ms, 0.5), q(&mut anim_ms, 0.95));
    let (r50, r95) = (q(&mut rest_ms, 0.5), q(&mut rest_ms, 0.95));
    eprintln!(
        "rune: animating frames {} (cpu p50 {a50:.3} p95 {a95:.3} ms), at-rest frames {} (the window draws none; forced cpu p50 {r50:.3} p95 {r95:.3} ms), layout nodes {}, glyphs {}",
        anim_ms.len(),
        rest_ms.len(),
        rt.stats.layout_nodes,
        rt.stats.glyphs
    );
}

fn tile_size(w: u32, h: u32, crops: &[[u32; 4]], gap: u32) -> (u32, u32) {
    if crops.is_empty() {
        return (w, h);
    }
    let cw = crops.iter().map(|c| c[2]).max().unwrap_or(w);
    let ch = crops.iter().map(|c| c[3]).sum::<u32>() + gap * (crops.len() as u32 - 1);
    (cw, ch)
}

/// The crops of one frame stacked vertically (the whole frame when there are none).
fn tile(px: &[u8], w: u32, h: u32, crops: &[[u32; 4]], gap: u32) -> Vec<u8> {
    if crops.is_empty() {
        return px.to_vec();
    }
    let (tw, th) = tile_size(w, h, crops, gap);
    let mut out: Vec<u8> = [128, 128, 128, 255].repeat((tw * th) as usize);
    let mut oy = 0;
    for &[x, y, cw, ch] in crops {
        assert!(x + cw <= w && y + ch <= h, "--crop outside the frame");
        for row in 0..ch {
            let s = (((y + row) * w + x) * 4) as usize;
            let d = (((oy + row) * tw) * 4) as usize;
            out[d..d + (cw * 4) as usize].copy_from_slice(&px[s..s + (cw * 4) as usize]);
        }
        oy += ch + gap;
    }
    out
}

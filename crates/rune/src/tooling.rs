//! Agent tooling: headless deterministic rendering, synthetic input scripts, element-tree dumps,
//! motion-lint traces and frame strips. Same renderer as the window, fixed virtual clock.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use crate::time::Instant;

use serde_json::{Value, json};

use crate::compose::{Compositor, Frame};
use crate::edit::{Key, KeyEvent, Mods};
use crate::el::{El, Tx};
use crate::render::{Gpu, OFFSCREEN_FORMAT, Offscreen, Renderer, write_png};
use crate::runtime::{Cx, Env, Runtime, Waker};
use crate::text::TextSystem;

pub use crate::headless_run::{HeadlessRun, PointerAction, ScriptEvent, ScriptPoint, TimedEvent, WindowFrame, headless_run};

pub struct AppSpec<S: 'static> {
    pub title: &'static str,
    pub size: (f32, f32),
    /// The smallest window the user can resize to, in points (None: 420×400).
    pub min_size: Option<(f32, f32)>,
    pub init: fn() -> S,
    pub view: fn(&S, &mut Cx<S>) -> El,
    /// Named states for snapshots: each modifies the initial state.
    pub scenes: &'static [(&'static str, fn(&mut S))],
    /// Font files replacing the built-in Inter (empty keeps it), and the sans/mono family names
    /// (CSS lists; `ui-monospace` stands for [`Family::SystemMono`](crate::Family::SystemMono)'s
    /// families, so mono spans can be SF Mono).
    pub fonts: &'static [&'static [u8]],
    pub families: (&'static str, &'static str),
    /// Called once with a waker other threads use when new data arrives.
    pub setup: Option<fn(&mut S, Waker)>,
    /// Called at the start of every frame with seconds since start (virtual when headless);
    /// true when state changed.
    pub poll: Option<fn(&mut S, f64, &mut Tx) -> bool>,
    /// Keys unhandled by the focused control, or every key while nothing has focus
    /// (`Tx::focused` tells which); true consumes the key. The rune README's "Key routing"
    /// lists the whole order.
    pub on_key: Option<fn(&mut S, &KeyEvent, &mut Tx) -> bool>,
}

impl<S: 'static> AppSpec<S> {
    /// The required parts; everything else at its default. Literals that set a few more fields
    /// end with `..AppSpec::new(..)`, so new fields don't break them.
    pub fn new(title: &'static str, size: (f32, f32), init: fn() -> S, view: fn(&S, &mut Cx<S>) -> El) -> Self {
        AppSpec {
            title,
            size,
            min_size: None,
            init,
            view,
            scenes: &[],
            fonts: &[],
            families: ("Inter", "Inter"),
            setup: None,
            poll: None,
            on_key: None,
        }
    }
}

/// Window frame options. Elements marked `.window_drag()` move the window on every platform;
/// a double press there zooms (macOS follows System Settings' titlebar double-click action;
/// elsewhere it toggles maximised).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chrome {
    /// macOS: the content view fills the window under a transparent, title-less titlebar (the
    /// app draws its own titlebar area). Off: a standard titled window. Other platforms keep
    /// their native frame either way.
    pub full_size_content: bool,
    /// macOS: the close button's top-left, in points from the window's top-left, kept there
    /// across resizes and full screen (Electron's `trafficLightPosition`). `None` leaves
    /// AppKit's placement. The three buttons keep AppKit's own size and spacing.
    pub traffic_lights: Option<(f32, f32)>,
    /// macOS: only `.window_drag()` regions move the window, so content under a full-size
    /// titlebar stays clickable (Electron's app regions). Off: AppKit's titlebar band drags too.
    /// Applies only to this window's content view when borderless or full-size-content.
    /// Ordinary titled windows and NSPanel overlays keep their native behaviour.
    pub drag_regions: bool,
}

impl Chrome {
    pub const DEFAULT: Chrome = Chrome { full_size_content: true, traffic_lights: None, drag_regions: false };
    /// Headless `--chrome` overlay and checks: macOS 26 draws 14 pt buttons on a 23 pt pitch.
    pub const TRAFFIC_LIGHT: f32 = 14.0;
    pub const TRAFFIC_PITCH: f32 = 23.0;

    /// Where macOS will draw the three buttons (x, y, w, h in points), assuming macOS 26 metrics
    /// and AppKit's default origin (7, 6) when unpositioned.
    pub fn traffic_light_rects(&self) -> Option<[[f32; 4]; 3]> {
        if !self.full_size_content && self.traffic_lights.is_none() {
            return None;
        }
        let (x, y) = self.traffic_lights.unwrap_or((7.0, 6.0));
        Some(std::array::from_fn(|i| [x + i as f32 * Self::TRAFFIC_PITCH, y, Self::TRAFFIC_LIGHT, Self::TRAFFIC_LIGHT]))
    }

    /// The trailing edge of [`Chrome::traffic_light_rects`]' zoom button, or 0 without them:
    /// what [`Cx::titlebar_inset`](crate::Cx::titlebar_inset) reports in a headless run.
    pub fn titlebar_inset(&self) -> f32 {
        self.traffic_light_rects().map_or(0.0, |r| r[2][0] + r[2][2])
    }
}

impl Default for Chrome {
    fn default() -> Self {
        Chrome::DEFAULT
    }
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
    #[cfg(feature = "hot")]
    if crate::hot::enabled() {
        rt.hot = Some(crate::hot::Session::connect(waker.clone()));
    }
    if let Some(f) = spec.setup {
        f(&mut rt.state, waker);
    }
    rt
}

#[derive(Clone, Default, Debug)]
pub struct Args {
    #[cfg(feature = "hot")]
    pub(crate) snap_on_patch: Option<PathBuf>,
    #[cfg(feature = "hot")]
    pub(crate) hot_control: Option<PathBuf>,
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
    /// Trace the native window's submission clock instead of a virtual headless clock.
    pub native: bool,
    /// Trace every element, keyed or not (paths name the unkeyed ones).
    pub trace_all: bool,
    pub strip: Option<PathBuf>,
    pub crops: Vec<[f32; 4]>,
    pub every: usize,
    pub cols: usize,
    pub list_scenes: bool,
    pub first_pixels: bool,
    pub window: Option<String>,
    pub list_windows: bool,
    /// Draw where macOS will put the traffic lights over headless frames, and report elements
    /// that overlap them.
    pub chrome_overlay: bool,
    /// Stream the dumped frames into ffmpeg, which writes this video.
    pub(crate) video: Option<PathBuf>,
    /// How long to dump frames for, when `frames` isn't given.
    pub(crate) duration: Option<Length>,
    /// `--at mark:NAME±S` and `--start mark:NAME±S`: a script mark's time plus an offset.
    pub(crate) at_mark: Option<String>,
    pub(crate) start_mark: Option<String>,
    /// `--keys [CORNER]`: keycaps for script keys and chords over dumped frames.
    pub(crate) keys: Option<crate::keycaps::Corner>,
    /// `--keys-theme light|dark`; None (`auto`) follows the app's theme.
    pub(crate) keys_theme: Option<bool>,
    /// `--pointer`: a ripple where the script presses.
    pub(crate) pointer: bool,
    /// `--desktop BACKDROP`: every visible window composited at its frame over this image.
    pub(crate) desktop: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Length {
    Seconds(f64),
    /// To the script's `end` mark, or one second after its last event.
    Auto,
}

/// Retain the whole expression until the script is loaded: a mark's name can end in `-1`.
fn mark_time(v: &str) -> Option<String> {
    v.strip_prefix("mark:").map(str::to_owned)
}

impl Args {
    pub fn parse() -> Args {
        #[cfg(target_arch = "wasm32")]
        return Args::parse_from(Vec::new());
        #[cfg(not(target_arch = "wasm32"))]
        Args::parse_from(std::env::args().skip(1).collect())
    }

    /// Parses `argv` (without the program name); unrecognised arguments become `extra_args`.
    pub fn parse_from(argv: Vec<String>) -> Args {
        #[cfg(feature = "hot")]
        crate::hot::enable_from_env();
        let mut a = Args { fps: 60.0, every: 1, cols: 10, ..Default::default() };
        let mut extra = Vec::new();
        let mut i = 0;
        let val = |i: &mut usize| -> String {
            *i += 1;
            argv.get(*i).cloned().unwrap_or_else(|| die(&format!("{} needs a value", argv[*i - 1])))
        };
        while i < argv.len() {
            match argv[i].as_str() {
                #[cfg(feature = "hot")]
                "--hot" => crate::hot::enable(),
                #[cfg(feature = "hot")]
                "--snap-on-patch" => a.snap_on_patch = Some(val(&mut i).into()),
                #[cfg(feature = "hot")]
                "--hot-control" => a.hot_control = Some(val(&mut i).into()),
                #[cfg(not(feature = "hot"))]
                "--hot" | "--snap-on-patch" | "--hot-control" => die("live editing requires the rune/hot feature; use scripts/rune-dev"),
                "--snap" => a.snap = Some(val(&mut i).into()),
                "--scene" => a.scene = Some(val(&mut i)),
                "--at" => {
                    let v = val(&mut i);
                    a.at_mark = mark_time(&v);
                    a.at = a.at_mark.is_none().then(|| num(&v));
                }
                "--size" => a.size = Some(parse_size(&val(&mut i)).unwrap_or_else(|e| die(&e))),
                "--scale" => a.scale = Some(parse_scale(&val(&mut i)).unwrap_or_else(|e| die(&e))),
                "--theme" => a.theme = Some(val(&mut i)),
                "--script" => a.script = Some(val(&mut i).into()),
                "--frames" => a.frames = Some(num(&val(&mut i)) as usize),
                "--start" => {
                    let v = val(&mut i);
                    a.start_mark = mark_time(&v);
                    if a.start_mark.is_none() { a.start = num(&v); }
                }
                "--fps" => a.fps = num(&val(&mut i)),
                "--out" => a.out = Some(val(&mut i).into()),
                "--video" => a.video = Some(val(&mut i).into()),
                "--duration" => {
                    let v = val(&mut i);
                    a.duration = Some(if v == "auto" { Length::Auto } else { Length::Seconds(num(&v)) });
                }
                "--keys" => {
                    let corner = argv.get(i + 1).and_then(|v| crate::keycaps::Corner::parse(v));
                    if corner.is_some() { i += 1; }
                    a.keys = Some(corner.unwrap_or_default());
                }
                "--keys-theme" => {
                    let v = val(&mut i);
                    a.keys_theme = crate::keycaps::parse_theme(&v).unwrap_or_else(|| die("--keys-theme is auto, light or dark"));
                }
                "--pointer" => a.pointer = true,
                "--desktop" => a.desktop = Some(val(&mut i).into()),
                "--tree" => a.tree = Some(val(&mut i).into()),
                "--trace" => a.trace = Some(val(&mut i).into()),
                "--native" => a.native = true,
                "--trace-all" => a.trace_all = true,
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
                "--window" => a.window = Some(val(&mut i)),
                "--list-windows" => a.list_windows = true,
                "--chrome" => a.chrome_overlay = true,
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
        #[cfg(feature = "hot")]
        if self.snap_on_patch.is_some() { return true; }
        self.snap.is_some()
            || self.tree.is_some()
            || (!self.native && (self.trace.is_some() || self.frames.is_some() || self.duration.is_some() || self.out.is_some()))
            || self.video.is_some()
            || self.strip.is_some()
            || self.list_scenes
            || self.first_pixels
            || self.list_windows
            || self.desktop.is_some()
    }
}

const HELP: &str = "usage: <app> [window]
  --snap out.png        render offscreen (no window) at --at seconds
  --scene NAME          start from a named scene (--list-scenes)
  --at SECONDS          virtual time of the snapshot/tree (default: end of frames or script);
                        mark:NAME or mark:NAME+0.5 for a script mark's time (also --start)
  --size WxH            viewport in points      --scale N   device scale (default 2)
  --theme dark|light    system appearance
  --script FILE.json    timestamped input (move/down/up/tap/drag, keys, {\"focus\":false}); a
                        key with \"repeat\":true is a held key's auto-repeat; {\"type\":\"mark\",\"name\":N}
                        marks a time; {\"type\":\"shortcut\",\"chord\":\"cmd+space\"} presses a chord
                        through its registered global shortcut handler, never on_key;
                        an object adds \"displays\" and \"pointer\" for place() (rune-placed, --tree)
                        menu-pick with an \"id\" and menu-cancel answer a pop-up menu
  --frames N --fps 60 --start S --out DIR     dump N frames from S seconds
  --video out.mp4       stream frames into ffmpeg (.mp4 H.264, .mov ProRes 4444, .webm VP9);
                        --frames N or --duration SECONDS|auto long (default auto: to the
                        script's \"end\" mark, or 1 s after its last event)
  --keys [CORNER]       draw keycaps for script keys and chords on frames (bottom-left default;
                        bottom, bottom-right, top-left, top, top-right); never in --tree
  --keys-theme auto|light|dark   cap colours (default auto: match the app's background)
  --pointer             draw a ripple where the script presses
  --desktop BACKDROP.png  draw every visible window at its placed frame, with corners and
                        shadows, over the backdrop (scaled to fill). The output is --size
                        points, else the script's primary display, else the backdrop's pixels
                        at --scale; script x/y are output points, keys go to the key window;
                        --window KEY picks the window --scene and --tree describe
  --strip out.png --crop x,y,w,h --every K    tile dumped frames into one image
                        (repeat --crop to stack several regions per frame; --cols N per row)
  --tree out.json       element tree with animated values
  --trace out.jsonl     per-frame rects/opacity in motion-lint JSONL, with the tracks each frame
                        sampled (tk) and why it was animating (quality.why)
  --trace-all           trace unkeyed elements too, by path
  --native             trace native submissions; --frames N stops after N frames
  --first-pixels        report when the first GPU-complete offscreen pixels are readable
  --window KEY         select a window for headless tools (run_windows, App::window)
  --list-windows       list window keys using a deterministic virtual display
  --chrome              draw where macOS puts the traffic lights; list elements overlapping them
  other arguments are passed to the app (rune::tooling::extra_args)";

pub(crate) fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2)
}

fn num(s: &str) -> f64 {
    s.trim().parse().unwrap_or_else(|_| die(&format!("not a number: {s}")))
}

fn positive(s: &str) -> Option<f32> {
    s.trim().parse::<f32>().ok().filter(|v| v.is_finite() && *v > 0.0)
}

/// `--size WxH`, in points.
fn parse_size(v: &str) -> Result<(f32, f32), String> {
    v.split_once('x').and_then(|(w, h)| Some((positive(w)?, positive(h)?)))
        .ok_or_else(|| format!("--size is WxH, both above 0: {v}"))
}

fn parse_scale(v: &str) -> Result<f32, String> {
    positive(v).ok_or_else(|| format!("--scale is a number above 0: {v}"))
}

/// Opens the app's window and quits when it closes (`App` for other lifetimes), or renders
/// headless for the tooling arguments.
pub fn run<S: 'static>(spec: AppSpec<S>) {
    #[cfg(feature = "hot")]
    crate::hot::run(spec, Args::parse());
    #[cfg(not(feature = "hot"))]
    crate::app::App::new(spec).run();
}

/// Create independent windows from a display-aware factory. Headless tools invoke the same
/// factory with one virtual display and select one retained tree using `--window KEY`.
/// Windows may have different state types: return `AnyWindowSpec`s (`WindowSpec::boxed`).
pub fn run_windows<I: IntoIterator<Item: Into<crate::AnyWindowSpec>>>(factory: impl FnOnce(&[crate::Display]) -> I + 'static) {
    run_windows_with(crate::RunOptions::default(), factory);
}

/// Run a window factory with process-level options, such as macOS accessory activation.
pub fn run_windows_with<I: IntoIterator<Item: Into<crate::AnyWindowSpec>>>(options: crate::RunOptions, factory: impl FnOnce(&[crate::Display]) -> I + 'static) {
    started(try_run_windows_with(options, factory));
}

/// `run_windows_with`, returning an error instead of panicking when the event loop can't start
/// (no display over ssh or in CI, no Wayland or X11 server) or fails.
pub fn try_run_windows_with<I: IntoIterator<Item: Into<crate::AnyWindowSpec>>>(options: crate::RunOptions, factory: impl FnOnce(&[crate::Display]) -> I + 'static) -> Result<(), String> {
    run_window_factory(options, window_args(None, Args::parse), erase(factory))
}

/// Run a window factory with explicit Rune arguments, without reading the host CLI.
/// Pass `[]` to isolate an embedding application's flags before display discovery.
pub fn run_windows_with_args<I: IntoIterator<Item: Into<crate::AnyWindowSpec>>>(options: crate::RunOptions, args: impl IntoIterator<Item = String>, factory: impl FnOnce(&[crate::Display]) -> I + 'static) {
    started(try_run_windows_with_args(options, args, factory));
}

/// `run_windows_with_args`, returning an error instead of panicking when the event loop can't
/// start or fails.
pub fn try_run_windows_with_args<I: IntoIterator<Item: Into<crate::AnyWindowSpec>>>(options: crate::RunOptions, args: impl IntoIterator<Item = String>, factory: impl FnOnce(&[crate::Display]) -> I + 'static) -> Result<(), String> {
    run_window_factory(options, window_args(Some(args.into_iter().collect()), Args::parse), erase(factory))
}

fn started(result: Result<(), String>) {
    if let Err(error) = result { panic!("{error}"); }
}

type Factory = Box<dyn FnOnce(&[crate::Display]) -> Vec<crate::AnyWindowSpec>>;

fn erase<I: IntoIterator<Item: Into<crate::AnyWindowSpec>>>(factory: impl FnOnce(&[crate::Display]) -> I + 'static) -> Factory {
    Box::new(move |displays| factory(displays).into_iter().map(Into::into).collect())
}

pub(crate) fn window_args(args: Option<Vec<String>>, process: impl FnOnce() -> Args) -> Args {
    args.map_or_else(process, Args::parse_from)
}

fn run_window_factory(options: crate::RunOptions, args: Args, factory: Factory) -> Result<(), String> {
    if args.headless() {
        let displays = desktop(&args, read_script(&args).as_ref()).displays();
        headless_window(factory(&displays), &displays, args, options)
    } else {
        crate::window::run_windows(factory, args, options)
    }
}

/// Lists the windows, or renders the one `--window KEY` selects (the first by default) with its
/// own state. Windows that apps added with `App::window` are selectable too.
fn headless_window(windows: Vec<crate::AnyWindowSpec>, displays: &[crate::Display], args: Args, options: crate::RunOptions) -> Result<(), String> {
    let mut windows = crate::windows::flatten(windows);
    crate::windows::validate(&windows).unwrap_or_else(|e| die(&e));
    if args.list_windows {
        for window in windows { println!("{}", window.key()); }
        return Ok(());
    }
    let index = match args.window.as_deref() {
        Some(key) => windows.iter().position(|w| w.key() == key).unwrap_or_else(|| die(&format!("no window {key:?}; use --list-windows"))),
        None if !windows.is_empty() => 0,
        None => die("the app created no windows"),
    };
    if args.desktop.is_some() {
        crate::desktop::run(windows, index, args, options.accessory);
        return Ok(());
    }
    let mut window = windows.remove(index);
    let args = window.resolve_args(args);
    if !args.headless() {
        return crate::window::run_windows(Box::new(move |_| vec![window]), args, options);
    }
    let scale = displays.iter().find(|d| d.primary).unwrap_or(&displays[0]).scale;
    window.headless(args, scale as f32);
    Ok(())
}

/// `run` with a compositor drawing every frame (see `compose`); `App::compositor` for other lifetimes.
pub fn run_with<S: 'static>(spec: AppSpec<S>, compositor: impl Compositor<S>) {
    crate::app::App::new(spec).compositor(compositor).run();
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
pub(crate) enum Pos {
    Xy(f32, f32),
    Key(String),
    /// Relative to the last pointer-down position.
    Rel(f32, f32),
}

#[derive(Clone, Debug)]
pub(crate) enum Kind {
    /// A named time with no input effect.
    Mark(String),
    Focus(bool),
    /// The app reopened (macOS: its Dock icon clicked).
    Reopen,
    Menu(String),
    /// Answers the open pop-up menu: an item's id, or `None` to dismiss it.
    PopupMenu(Option<String>),
    Physical(crate::PhysicalInput),
    /// A registered global shortcut, delivered only to its handler.
    Shortcut(crate::Shortcut),
    Move,
    Down,
    Up,
    SecondaryDown,
    SecondaryUp,
    /// The key and whether it is an auto-repeat.
    Key(KeyEvent, bool),
    Paste(String),
    /// A wheel delta (dx, dy) in points.
    Wheel(f32, f32),
    /// A release velocity (vx, vy) in points per second.
    Fling(f32, f32),
    /// A file drag from another app (`drag_files`, `drop_files`).
    Files(FileStep),
}

#[derive(Clone, Debug)]
pub(crate) enum FileStep {
    Hover(Vec<std::path::PathBuf>),
    Drop(Vec<std::path::PathBuf>),
    Leave,
}

/// A scripted wheel delta: vertical-only ones take the plain vertical path.
pub(crate) fn wheel<S: 'static>(rt: &mut Runtime<S>, (x, y): (f32, f32), dx: f32, dy: f32) {
    if dx == 0.0 { rt.wheel(x, y, dy) } else { rt.wheel_xy(x, y, dx, dy) }
}

/// A scripted release velocity: vertical-only ones take the plain vertical path.
pub(crate) fn fling<S: 'static>(rt: &mut Runtime<S>, (x, y): (f32, f32), vx: f32, vy: f32) {
    if vx == 0.0 { rt.fling(x, y, vy) } else { rt.fling_xy(x, y, vx, vy) }
}

pub(crate) fn key_named(name: &str) -> Option<Key> {
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
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
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
pub(crate) struct Ev {
    pub t: f64,
    pub kind: Kind,
    pub pos: Pos,
}

impl Ev {
    /// The event as apps read it through `headless_run`.
    pub(crate) fn public(&self) -> TimedEvent {
        let at = || match &self.pos {
            Pos::Xy(x, y) => ScriptPoint::At(*x, *y),
            Pos::Key(key) => ScriptPoint::Element(key.clone()),
            Pos::Rel(dx, dy) => ScriptPoint::FromDown(*dx, *dy),
        };
        let pointer = |action| ScriptEvent::Pointer { action, at: at() };
        let event = match &self.kind {
            Kind::Mark(name) => ScriptEvent::Mark(name.clone()),
            Kind::Focus(focused) => ScriptEvent::Focus(*focused),
            Kind::Reopen => ScriptEvent::Reopen,
            Kind::Menu(item) => ScriptEvent::Menu(item.clone()),
            Kind::PopupMenu(picked) => ScriptEvent::PopupMenu(picked.clone()),
            Kind::Physical(input) => ScriptEvent::Physical(*input),
            Kind::Shortcut(chord) => ScriptEvent::Shortcut(chord.clone()),
            Kind::Move => pointer(PointerAction::Move),
            Kind::Down => pointer(PointerAction::Down),
            Kind::Up => pointer(PointerAction::Up),
            Kind::SecondaryDown => pointer(PointerAction::SecondaryDown),
            Kind::SecondaryUp => pointer(PointerAction::SecondaryUp),
            Kind::Key(key, repeat) => ScriptEvent::Key { key: key.clone(), repeat: *repeat },
            Kind::Paste(text) => ScriptEvent::Paste(text.clone()),
            Kind::Wheel(0.0, dy) => pointer(PointerAction::Scroll(*dy)),
            Kind::Wheel(dx, dy) => pointer(PointerAction::ScrollXy(*dx, *dy)),
            Kind::Fling(0.0, v) => pointer(PointerAction::Fling(*v)),
            Kind::Fling(vx, vy) => pointer(PointerAction::FlingXy(*vx, *vy)),
            Kind::Files(FileStep::Hover(paths)) => ScriptEvent::DragFiles { paths: paths.clone(), at: at() },
            Kind::Files(FileStep::Drop(paths)) => ScriptEvent::DropFiles { paths: paths.clone(), at: at() },
            Kind::Files(FileStep::Leave) => ScriptEvent::DragFilesLeave,
        };
        TimedEvent { t: self.t, event }
    }
}

pub(crate) fn read_script(args: &Args) -> Option<Value> {
    args.script.as_ref().map(|path| {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
        serde_json::from_str(&text).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())))
    })
}

pub(crate) fn load_script(script: &Value) -> Vec<Ev> {
    let list = match script { Value::Object(o) => o.get("events").cloned().unwrap_or_else(|| json!([])), v => v.clone() };
    parse_script(list)
}

/// The script's displays and pointer, which headless `place()` resolves against. Without
/// scripted displays, a `--desktop` run has one display the size of its output: `--size`, else
/// the backdrop's pixels at `--scale`.
pub(crate) fn desktop(args: &Args, script: Option<&Value>) -> crate::virtual_desktop::VirtualDesktop {
    let scale = f64::from(args.scale.unwrap_or(2.0));
    let size = args.desktop.as_ref().map(|path| match args.size {
        Some((w, h)) => (f64::from(w), f64::from(h)),
        None => {
            let (w, h) = ::image::image_dimensions(path).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
            (f64::from(w) / scale, f64::from(h) / scale)
        }
    });
    crate::virtual_desktop::VirtualDesktop::from_script_sized(script, scale, size).unwrap_or_else(|e| die(&e))
}

fn parse_script(list: Value) -> Vec<Ev> {
    let Value::Array(items) = list else { die("script must be an array of events or {\"events\": [...]}") };
    let mut out = Vec::new();
    for e in items {
        let t = e["t"].as_f64().unwrap_or(0.0);
        if let Some(focused) = e["focus"].as_bool() {
            out.push(Ev { t, kind: Kind::Focus(focused), pos: Pos::Rel(0.0, 0.0) });
            continue;
        }
        if let Some(item) = e["menu"].as_str() {
            out.push(Ev { t, kind: Kind::Menu(item.into()), pos: Pos::Rel(0.0, 0.0) });
            continue;
        }
        if e["reopen"].as_bool() == Some(true) {
            out.push(Ev { t, kind: Kind::Reopen, pos: Pos::Rel(0.0, 0.0) });
            continue;
        }
        if let Some(chord) = e["physical"].as_str() {
            let chord: crate::Shortcut = chord.parse().unwrap_or_else(|e: crate::ShortcutError| die(&e.to_string()));
            let repeat = e["repeat"].as_bool().unwrap_or(false);
            for pressed in [true, false] {
                out.push(Ev { t, kind: Kind::Physical(crate::PhysicalInput::Key {
                    key: chord.physical_key(), mods: chord.modifiers(), pressed, repeat: repeat && pressed,
                }), pos: Pos::Rel(0.0, 0.0) });
            }
            // Then the modifiers come up, as on a real keyboard, so nothing stays held.
            if chord.modifiers() != Mods::default() {
                out.push(Ev { t, kind: Kind::Physical(crate::PhysicalInput::Modifiers(Mods::default())), pos: Pos::Rel(0.0, 0.0) });
            }
            continue;
        }
        if let Some(shortcut) = e["shortcut"].as_str() {
            let shortcut = shortcut.parse().unwrap_or_else(|e: crate::ShortcutError| die(&e.to_string()));
            out.push(Ev { t, kind: Kind::Shortcut(shortcut), pos: Pos::Rel(0.0, 0.0) });
            continue;
        }
        if parse_files(&e, t, &mut out) { continue; }
        if e["type"] == "shortcut" {
            let chord = e["chord"].as_str().unwrap_or_else(|| die("a \"shortcut\" event needs a \"chord\", like \"cmd+space\""));
            let chord = chord.parse().unwrap_or_else(|e: crate::ShortcutError| die(&e.to_string()));
            out.push(Ev { t, kind: Kind::Shortcut(chord), pos: Pos::Rel(0.0, 0.0) });
            continue;
        }
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
            "secondary-down" => out.push(Ev { t, kind: Kind::SecondaryDown, pos }),
            "secondary-up" => out.push(Ev { t, kind: Kind::SecondaryUp, pos }),
            "secondary-click" => {
                out.push(Ev { t, kind: Kind::SecondaryDown, pos: pos.clone() });
                out.push(Ev { t: t + e["hold"].as_f64().unwrap_or(0.08), kind: Kind::SecondaryUp, pos });
            }
            "tap" | "click" => {
                out.push(Ev { t, kind: Kind::Move, pos: pos.clone() });
                out.push(Ev { t, kind: Kind::Down, pos });
                out.push(Ev { t: t + e["hold"].as_f64().unwrap_or(0.08), kind: Kind::Up, pos: Pos::Rel(0.0, 0.0) });
            }
            "double-click" | "double-tap" | "triple-click" => {
                // Two (three) presses `gap` apart (default 0.15 s), inside the default double-click interval.
                let hold = e["hold"].as_f64().unwrap_or(0.06);
                let gap = e["gap"].as_f64().unwrap_or(0.15).max(hold + 0.01);
                let presses = if ty == "triple-click" { 3 } else { 2 };
                for start in (0..presses).map(|k| t + gap * k as f64) {
                    out.push(Ev { t: start, kind: Kind::Move, pos: pos.clone() });
                    out.push(Ev { t: start, kind: Kind::Down, pos: pos.clone() });
                    out.push(Ev { t: start + hold, kind: Kind::Up, pos: Pos::Rel(0.0, 0.0) });
                }
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
                    out.push(Ev { t: t + every * k as f64, kind: Kind::Key(ev, false), pos: Pos::Rel(0.0, 0.0) });
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
                let repeat = e["repeat"].as_bool().unwrap_or(false);
                out.push(Ev { t, kind: Kind::Key(KeyEvent { key, mods }, repeat), pos: Pos::Rel(0.0, 0.0) });
            }
            "paste" => out.push(Ev { t, kind: Kind::Paste(e["text"].as_str().unwrap_or("").to_owned()), pos: Pos::Rel(0.0, 0.0) }),
            "scroll" => {
                // A trackpad swipe: deltas eased over `dur`, then (with "fling") the release velocity.
                let dx = e["dx"].as_f64().unwrap_or(0.0) as f32;
                let dy = e["dy"].as_f64().unwrap_or(0.0) as f32;
                let dur = e["dur"].as_f64().unwrap_or(0.2).max(0.01);
                out.push(Ev { t, kind: Kind::Move, pos: pos.clone() });
                let steps = (dur * 120.0).ceil() as usize;
                let mut prev = (0.0, 0.0);
                for s in 1..=steps {
                    let f = s as f32 / steps as f32;
                    let f = f * (2.0 - f);
                    out.push(Ev { t: t + dur * s as f64 / steps as f64, kind: Kind::Wheel(dx * f - prev.0, dy * f - prev.1), pos: pos.clone() });
                    prev = (dx * f, dy * f);
                }
                if e["fling"].as_bool().unwrap_or(false) {
                    let v = |d: f32| 2.0 * d / dur as f32 * 0.25;
                    out.push(Ev { t: t + dur + 1e-4, kind: Kind::Fling(v(dx), v(dy)), pos });
                }
            }
            "fling" => {
                let v = |k: &str| e[k].as_f64().unwrap_or(0.0) as f32;
                out.push(Ev { t, kind: Kind::Fling(v("vx"), v("v")), pos });
            }
            "menu-pick" => {
                let id = e["id"].as_str().unwrap_or_else(|| die("menu-pick needs an \"id\""));
                out.push(Ev { t, kind: Kind::PopupMenu(Some(id.into())), pos: Pos::Rel(0.0, 0.0) });
            }
            "menu-cancel" => out.push(Ev { t, kind: Kind::PopupMenu(None), pos: Pos::Rel(0.0, 0.0) }),
            "mark" => {
                let name = e["name"].as_str().filter(|n| !n.is_empty()).unwrap_or_else(|| die("a mark needs a \"name\""));
                out.push(Ev { t, kind: Kind::Mark(name.to_owned()), pos });
            }
            other => die(&format!("unknown event type {other}")),
        }
    }
    out.sort_by(|a, b| a.t.total_cmp(&b.t));
    out
}

/// `{"drag_files": {"paths": […], "at": [x, y]}}` (or `"key": …` for an element's centre) moves a
/// file drag from another app over the window; `{"drag_files": "leave"}` takes it away.
/// `{"drop_files": {…, "hover": s}}` drags the files there for `s` seconds (default 0), then
/// drops them. Whether `e` was one.
fn parse_files(e: &Value, t: f64, out: &mut Vec<Ev>) -> bool {
    let (spec, drop) = match (e.get("drag_files"), e.get("drop_files")) {
        (Some(spec), _) => (spec, false),
        (None, Some(spec)) => (spec, true),
        (None, None) => return false,
    };
    if !drop && spec == "leave" {
        out.push(Ev { t, kind: Kind::Files(FileStep::Leave), pos: Pos::Rel(0.0, 0.0) });
        return true;
    }
    let name = if drop { "drop_files" } else { "drag_files" };
    let paths: Vec<std::path::PathBuf> = spec["paths"].as_array()
        .unwrap_or_else(|| die(&format!("{name} needs \"paths\": [\"a file\", …]")))
        .iter().map(|p| p.as_str().unwrap_or_else(|| die(&format!("{name}: paths are strings"))).into()).collect();
    let pos = match (spec["at"].as_array(), spec["key"].as_str()) {
        (Some(at), _) if at.len() == 2 && at.iter().all(Value::is_number) =>
            Pos::Xy(at[0].as_f64().unwrap_or(0.0) as f32, at[1].as_f64().unwrap_or(0.0) as f32),
        (None, Some(key)) => Pos::Key(key.to_owned()),
        _ => die(&format!("{name} needs \"at\": [x, y] or \"key\"")),
    };
    out.push(Ev { t, kind: Kind::Files(FileStep::Hover(paths.clone())), pos: pos.clone() });
    if drop {
        let hover = spec["hover"].as_f64().unwrap_or(0.0).max(0.0);
        out.push(Ev { t: t + hover, kind: Kind::Files(FileStep::Drop(paths)), pos });
    }
    true
}

/// Feeds a script's file drag step to `rt`, at `at` in window points.
pub(crate) fn file_input<S: 'static>(rt: &mut Runtime<S>, step: &FileStep, at: (f32, f32)) {
    match step {
        FileStep::Hover(paths) => rt.drag_files(paths.clone(), Some(at)),
        FileStep::Drop(paths) => rt.drop_files(paths.clone(), Some(at)),
        FileStep::Leave => rt.drag_files_leave(),
    }
}

#[cfg(test)]
#[test]
fn a_file_drop_hovers_then_drops_at_its_point() {
    let steps: Vec<_> = parse_script(json!([
        {"drag_files": {"paths": ["a.png"], "key": "zone"}},
        {"t": 0.5, "drop_files": {"paths": ["a.png", "b.png"], "at": [10, 20], "hover": 0.25}},
        {"t": 1.0, "drag_files": "leave"},
    ])).into_iter().map(|e| (e.t, format!("{:?}", e.kind), format!("{:?}", e.pos))).collect();
    let expected = [
        (0.0, r#"Files(Hover(["a.png"]))"#, r#"Key("zone")"#),
        (0.5, r#"Files(Hover(["a.png", "b.png"]))"#, "Xy(10.0, 20.0)"),
        (0.75, r#"Files(Drop(["a.png", "b.png"]))"#, "Xy(10.0, 20.0)"),
        (1.0, "Files(Leave)", "Rel(0.0, 0.0)"),
    ].map(|(t, k, p)| (t, k.to_owned(), p.to_owned()));
    assert_eq!(steps, expected);
}

#[cfg(test)]
#[test]
fn a_physical_chord_releases_its_modifiers_after_the_key() {
    use crate::PhysicalInput;
    let kinds: Vec<_> = parse_script(json!([{"physical": "alt+super+space"}, {"t": 0.1, "physical": "f5"}]))
        .into_iter().map(|e| match e.kind { Kind::Physical(event) => event, _ => panic!("physical parsed as another event") }).collect();
    let mods = "alt+super+space".parse::<crate::Shortcut>().unwrap().modifiers();
    let key = |key, mods, pressed| PhysicalInput::Key { key, mods, pressed, repeat: false };
    use crate::PhysicalKeyCode::{F5, Space};
    let expected = [key(Space, mods, true), key(Space, mods, false), PhysicalInput::Modifiers(Mods::default()),
        key(F5, Mods::default(), true), key(F5, Mods::default(), false)];
    assert_eq!(format!("{kinds:?}"), format!("{expected:?}"));
}

#[cfg(test)]
#[test]
fn script_events_answer_a_pop_up_menu() {
    let kinds: Vec<_> = parse_script(json!([{"t": 0.2, "type": "menu-cancel"}, {"t": 0.1, "type": "menu-pick", "id": "dark"}]))
        .into_iter().map(|e| {
            let ScriptEvent::PopupMenu(public) = e.public().event else { panic!("public event lost the pop-up answer") };
            let Kind::PopupMenu(picked) = e.kind else { panic!("parsed as another event") };
            assert_eq!(public, picked);
            picked
        }).collect();
    assert_eq!(kinds, [Some("dark".to_owned()), None]);
}

#[cfg(test)]
#[test]
fn a_script_key_can_be_a_held_keys_repeat() {
    let script = parse_script(json!([
        {"type": "key", "name": "Enter"},
        {"t": 0.1, "type": "key", "name": "Enter", "repeat": true},
        {"t": 0.2, "physical": "f5", "repeat": true},
    ]));
    let mut rt = Runtime::new(Vec::new(), |_: &Vec<bool>, _: &mut crate::Cx<Vec<bool>>| crate::el(), (100.0, 100.0), Env::default());
    rt.on_key = Some(|s, _, tx| { s.push(tx.key_repeat()); true });
    let mut physical = Vec::new();
    for event in script {
        match event.kind {
            Kind::Key(k, false) => rt.key(k),
            Kind::Key(k, true) => rt.repeat_key(k),
            Kind::Physical(crate::PhysicalInput::Key { pressed, repeat, .. }) => physical.push((pressed, repeat)),
            other => panic!("unexpected {other:?}"),
        }
    }
    rt.frame(0.0);
    assert_eq!(rt.state, [false, true]);
    assert_eq!(physical, [(true, true), (false, false)], "the release that ends a hold is not a repeat");
}

#[cfg(test)]
#[test]
fn window_panel_headless_focus_steps_preserve_false_and_event_order() {
    let script = parse_script(serde_json::json!([{"focus":true}, {"t":0.1,"focus":false}, {"t":0.2,"focus":false}]));
    let mut rt = Runtime::new(Vec::new(), |_: &Vec<bool>, _: &mut crate::Cx<Vec<bool>>| crate::el(), (100.0, 100.0), Env::default());
    rt.on_focus = Some(|s, focused, _| s.push(focused));
    assert_eq!(script.len(), 3);
    for event in script {
        let Kind::Focus(focused) = event.kind else { panic!("focus parsed as pointer action") };
        rt.window_focus(focused);
    }
    assert_eq!(rt.state, [true, false]);
}

// -------------------------------------------------------------------------------------------
// motion-lint trace (the format Scrawl writes)

pub(crate) struct Trace {
    out: std::io::BufWriter<std::fs::File>,
    ids: HashMap<String, u32>,
    described: HashMap<u32, (String, &'static str, &'static str)>,
    states: HashMap<u32, ([i64; 6], u32, [i64; 4])>,
    effects: HashMap<u32, [String; 2]>,
    channels: rune_motion::TraceChannels,
    path: PathBuf,
    /// Trace every element, not only keyed and named ones (`--trace-all`).
    pub(crate) all: bool,
    /// Lines waiting for present times; `None` writes each frame at once.
    presents: Option<crate::present_time::Presents>,
    /// The native surface's frame counts and `App::dev_stats`, printed when the trace closes.
    surface: Option<(std::sync::Arc<std::sync::Mutex<crate::SurfaceStats>>, Option<fn() -> String>)>,
}

/// A native frame's timing beyond its submission time.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FrameTiming {
    /// The display link's target, ms on the trace's clock.
    pub target_ms: Option<f64>,
    /// The drawable the frame presented, when present times are recorded.
    pub drawable: Option<u64>,
    /// The previous frame asked for this one right away (it wasn't idle).
    pub continuous: bool,
}

fn r2(v: f32) -> f64 {
    (f64::from(v) * 100.0).round() / 100.0
}

impl Trace {
    pub(crate) fn open(path: &PathBuf, size: (f32, f32), fps: f64, scale: f32) -> Trace {
        Self::open_clock(path, size, fps, scale, "virtual")
    }

    pub(crate) fn open_clock(path: &PathBuf, size: (f32, f32), fps: f64, scale: f32, clock: &str) -> Trace {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let file = std::fs::File::create(path).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
        let mut out = std::io::BufWriter::new(file);
        let mut header = json!({
            "type": "header", "launch": "rune", "width": size.0, "height": size.1,
            "hz": fps, "scale": scale, "platform": "rune", "units": "pt", "clock": clock,
        });
        if clock == "wall" { header["timestamp"] = "cpu-submit".into(); }
        let _ = writeln!(out, "{header}");
        Trace { out, ids: HashMap::new(), described: HashMap::new(), states: HashMap::new(), effects: HashMap::new(),
            channels: Default::default(), path: path.clone(), all: false, presents: None, surface: None }
    }

    /// Holds each frame's line until its present time arrives from `source`, then writes
    /// it with `present_ms` and `present_dt_ms`. `start` is the trace clock's zero.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn record_presents(&mut self, source: std::sync::Arc<dyn crate::present_time::Source>, start: Instant) {
        self.presents = Some(crate::present_time::Presents::new(source, start));
    }

    /// Prints the surface's frame counts, and the app's own, when the trace closes.
    pub(crate) fn record_surface(&mut self, stats: std::sync::Arc<std::sync::Mutex<crate::SurfaceStats>>, app: Option<fn() -> String>) {
        self.surface = Some((stats, app));
    }

    /// Waits up to `wait` for the last frames' present times, then writes every held line.
    pub(crate) fn settle_presents(&mut self, wait: std::time::Duration) {
        let Some(presents) = self.presents.as_mut() else { return };
        let until = Instant::now() + wait;
        while presents.waiting() && Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        presents.flush(&mut self.out);
    }

    fn id(&mut self, key: &str) -> u32 {
        let next = self.ids.len() as u32 + 1;
        *self.ids.entry(key.to_owned()).or_insert(next)
    }

    pub(crate) fn frame<S>(&mut self, rt: &mut Runtime<S>, t_ms: f64, cpu_ms: f64) {
        self.frame_timed(rt, t_ms, cpu_ms, FrameTiming::default());
    }

    pub(crate) fn frame_timed<S>(&mut self, rt: &mut Runtime<S>, t_ms: f64, cpu_ms: f64, timing: FrameTiming) {
        let els = rt.elements();
        let settled = rt.settled;
        let mut metas = Vec::new();
        let mut changed = Vec::new();
        let mut seen = HashSet::new();
        // A virtual list's invisible estimated extent changes when rows are measured or the
        // window rebases. Record the actual row surfaces, not that bookkeeping rectangle.
        let all = self.all;
        let traced = |e: &crate::runtime::ElementInfo| (all || e.key.is_some() || e.trace_name.is_some()) && !e.virtual_extent;
        for e in els.iter().filter(|e| traced(e)) {
            let id = self.id(&e.path);
            let name = e.trace_name.unwrap_or(&e.path);
            let (class, host) = e.trace_namespace.unwrap_or(("rune", "rune"));
            seen.insert(id);
            if self.described.get(&id).is_none_or(|old| old.0 != name || old.1 != class || old.2 != host) {
                self.described.insert(id, (name.to_owned(), class, host));
                let mut meta = json!({ "id": id, "cls": class, "nm": name, "host": host, "role": e.role });
                if let Some(p) = e.parent.and_then(|p| els.get(p)).filter(|p| traced(p)) {
                    meta["pv"] = self.id(&p.path).into();
                }
                metas.push(meta);
            }
            let effects = [format!("{name}.scale"), format!("{name}.blur")];
            for (name, value, unit) in [(&effects[0], e.scale, "ratio"), (&effects[1], e.blur, "pt")] {
                self.channels.sample(name, "position", value);
                self.channels.owner(name, id, unit);
            }
            if let Some(old) = self.effects.insert(id, effects.clone()) {
                for name in old.iter().filter(|n| !effects.contains(n)) { self.channels.remove(name); }
            }
            let [x, y, w, h] = e.rect;
            // A settle commit is a deliberate cut (everything lands at once); a growth row reveals
            // its content by growing; reveals fade words in.
            use rune_motion::{Intent as I, flags as F};
            let clipped = e.rect.iter().zip(e.visible.iter()).any(|(a, b)| (a - b).abs() > 0.5);
            let mut intent = e.live_intent;
            if settled {
                intent |= I::POSITION | I::SIZE | I::TRANSITION | I::REVEAL;
            }
            // Reduced motion deliberately cuts geometry. Opacity still needs to fade;
            // do not declare transition/opacity intent for the whole reduced-motion tree.
            if rt.reduced_motion {
                intent |= I::POSITION | I::SIZE;
            }
            if e.revealing {
                intent |= I::REVEAL;
            }
            if e.in_growing_row {
                intent |= I::SIZE | I::REVEAL;
            }
            let flags = if e.moving || e.in_growing_row { F::ANIMATED } else { 0 }
                | if e.scrolling { F::SCROLLING } else { 0 }
                | if clipped { F::CLIPPED } else { 0 }
                | intent << 8;
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
            if let Some(names) = self.effects.remove(id) {
                for name in names { self.channels.remove(&name); }
            }
            if let Some((q, ..)) = self.states.remove(id)
                && settled
            {
                let [x, y, w, h] = [q[0], q[1], q[2], q[3]].map(|v| v as f64 / 100.0);
                cuts.push(json!([id, x, y, w, h, rune_motion::Intent::TRANSITION | rune_motion::Intent::OPACITY, 0]));
            }
        }

        let mut line = json!({ "t": (t_ms * 1000.0).round() / 1000.0, "n": 1 });
        // motion-lint's touch count: motion under a held press follows the hand.
        if rt.pressing() {
            line["k"] = 1.into();
        }
        let (edges, position_error) = rt.scene.sampling_error();
        let retarget = rt.scene.store_mut().take_retarget_audit();
        line["quality"] = json!({
            "cpu_ms": cpu_ms, "scene_edges": edges, "position_error_px": position_error,
            "retarget_lanes": retarget.lanes, "velocity_error_pt_s": retarget.max_velocity_error,
            "animating": rt.animating,
            "why": crate::runtime::AnimSource::names(if rt.animating { rt.anim_sources } else { 0 }),
        });
        // What the motion store spent this frame on, by traced element id (or path when untraced).
        let touched = rt.touched_tracks();
        if !touched.is_empty() {
            line["tk"] = touched.into_iter().map(|(path, prop, active)| {
                let who = self.ids.get(&path).map_or_else(|| json!(path), |&id| json!(id));
                json!([who, prop, u8::from(active)])
            }).collect::<Vec<_>>().into();
        }
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
        if let Some(channels) = &rt.trace_channels {
            for (&name, &(kind, value)) in channels { self.channels.sample(name, kind, value); }
        }
        let mut line = line.to_string();
        line.pop(); // Append scalar fields to the same frame object.
        self.channels.write_json(&mut line);
        if let Some(target) = timing.target_ms {
            line.push_str(&format!(r#","target_ms":{target}"#));
        }
        if let Some(presents) = self.presents.as_mut() {
            presents.push(line, timing.drawable, timing.continuous, &mut self.out);
            return;
        }
        line.push('}');
        let _ = writeln!(self.out, "{line}");
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        if let Some((stats, app)) = &self.surface {
            eprintln!("rune: frames {}", crate::surface_stats::read(stats).summary());
            if let Some(app) = app { eprintln!("rune: app {}", app()); }
        }
        if self.presents.is_none() { return; }
        self.settle_presents(crate::present_time::SETTLE);
        let _ = self.out.flush();
        if let Some(summary) = std::fs::read_to_string(&self.path).ok().as_deref().and_then(crate::present_time::Summary::of_trace) {
            eprintln!("{summary}");
        }
    }
}

pub(crate) fn tree_json<S>(rt: &Runtime<S>, real_t: f64) -> Value {
    let videos = rt.videos();
    let els: Vec<Value> = rt
        .elements()
        .iter()
        .map(|e| {
            let mut v = json!({
                "path": e.path,
                "role": e.role,
                "rect": e.rect.map(r2),
                "opacity": (e.opacity * 1000.0).round() / 1000.0,
                "scale": (e.scale * 10000.0).round() / 10000.0,
                "blur": r2(e.blur),
                "focusable": e.focusable, "focused": e.focused,
                "focusVisible": e.focus_visible, "inert": e.inert,
                "loopPhase": e.loop_phase,
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
            if let Some(h) = &e.help {
                v["help"] = h.clone().into();
            }
            if let Some(s) = &e.selected {
                v["selected"] = s.clone().into();
            }
            if e.rect.iter().zip(e.target.iter()).any(|(a, b)| (a - b).abs() > 0.01) {
                v["target"] = e.target.map(r2).to_vec().into();
            }
            if e.moving {
                v["moving"] = true.into();
            }
            if e.exiting {
                v["exiting"] = true.into();
            }
            if e.hover > 0.001 {
                v["hover"] = r2(e.hover).into();
            }
            if e.press > 0.001 {
                v["press"] = r2(e.press).into();
            }
            if e.drag_out {
                v["dragOut"] = true.into();
            }
            if let Some(video) = videos.get(&e.path) {
                v["video"] = video_json(video);
            }
            v
        })
        .collect();
    let mut tree = json!({
        "t": real_t,
        "anim_t": rt.t,
        "viewport": [rt.viewport.0, rt.viewport.1],
        "scale": rt.env.scale,
        "dark_system": rt.env.system_dark,
        "reduced_motion": rt.reduced_motion,
        "time_scale": rt.time_scale,
        "animating": rt.animating,
        "elements": els,
        "warnings": rt.key_warnings.as_deref().unwrap_or_default(),
    });
    if let Some(menu) = rt.popup_json() { tree["popupMenu"] = menu; }
    if let Some((text, rect)) = rt.help_tooltip() { tree["tooltip"] = json!({ "text": text, "rect": rect.map(r2) }); }
    tree
}

fn video_json(v: &crate::video::Snapshot) -> Value {
    use crate::PlayerState;
    let (state, error) = match &v.state {
        PlayerState::Loading => ("loading", None),
        PlayerState::Ready => ("ready", None),
        PlayerState::Playing => ("playing", None),
        PlayerState::Ended => ("ended", None),
        PlayerState::Failed(e) => ("failed", Some(e.clone())),
    };
    let mut out = json!({
        "kind": match v.kind { crate::video::Kind::Video => "video", crate::video::Kind::Audio => "audio" },
        "state": state,
        "time": (v.time * 1000.0).round() / 1000.0,
        "duration": v.duration.map(|d| (d * 1000.0).round() / 1000.0),
        "size": v.size.map(|(w, h)| [w, h]),
        "rate": v.rate,
        "looping": v.looping,
        "muted": v.muted,
    });
    if let Some(e) = error { out["error"] = e.into(); }
    out
}

// -------------------------------------------------------------------------------------------
// Headless run

// Render targets contain sRGB-encoded premultiplied linear colour. PNG uses straight
// colour, so unpremultiply in linear space, not in the encoded byte domain.
#[cfg(test)]
pub(crate) fn straight_alpha(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = f32::from(pixel[3]) / 255.0;
        if alpha == 0.0 { pixel[..3].fill(0); }
        else if alpha < 1.0 {
            for c in &mut pixel[..3] {
                *c = (crate::color::srgb_encode((crate::color::srgb_decode(f32::from(*c) / 255.0) / alpha).clamp(0.0, 1.0)) * 255.0).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod window_tests {
    #[test]
    fn transparent_png_unpremultiplies_in_linear_space() {
        let mut pixels = [188, 0, 0, 128, 255, 255, 255, 0, 12, 34, 56, 255];
        super::straight_alpha(&mut pixels);
        assert_eq!(pixels, [255, 0, 0, 128, 0, 0, 0, 0, 12, 34, 56, 255]);
    }
}

pub fn headless<S: 'static>(app: crate::app::App<S>, args: Args) {
    started(try_headless(app, args));
}

/// `headless`, returning the error when a window whose own arguments aren't headless can't open.
pub(crate) fn try_headless<S: 'static>(mut app: crate::app::App<S>, args: Args) -> Result<(), String> {
    if app.windows.is_empty() && args.desktop.is_none() {
        if args.list_windows { println!("main"); return Ok(()); }
        if args.window.as_deref().is_some_and(|key| key != "main") {
            die("no such window; this app has only 'main'");
        }
        let scale = args.scale.unwrap_or(2.0);
        if let Some((w, h)) = app.window_options.physical_size {
            app.spec.size = (w as f32 / scale, h as f32 / scale);
        }
        let transparent = app.window_options.transparent;
        headless_configured(app, args, transparent);
        return Ok(());
    }
    let windows = std::mem::take(&mut app.windows);
    let displays = desktop(&args, read_script(&args).as_ref()).displays();
    let accessory = app.accessory;
    let main = crate::WindowSpec::from_app("main", app);
    headless_window(std::iter::once(main.into()).chain(windows).collect(), &displays, args, crate::RunOptions { accessory })
}

pub(crate) fn headless_configured<S: 'static>(mut app: crate::app::App<S>, args: Args, transparent: bool) {
    let chrome = app.chrome;
    let launched = Instant::now();
    if args.list_scenes {
        for (n, _) in app.spec.scenes {
            println!("{n}");
        }
        return;
    }
    #[cfg(feature = "hot")]
    if args.snap_on_patch.is_some() {
        return crate::hot::snap_window(app, args);
    }
    let mut comp = app.compositor.take();
    let srgb_blend = if comp.is_some() && app.srgb_blend {
        eprintln!("rune: srgb_blend is ignored when a compositor is set");
        false
    } else {
        app.srgb_blend
    };
    // Snapshots and scripts run in parallel; they open the panel only when asked to.
    #[cfg(all(feature = "dev", debug_assertions))]
    let dev = app.dev.take().filter(|_| std::env::var_os("RUNE_DEV_PANEL").is_some());
    let on_focus = app.on_focus;
    let on_reopen = app.on_reopen;
    let source = read_script(&args);
    let script = source.as_ref().map(load_script).unwrap_or_default();
    let timeline = Timeline::of(&args, &script).unwrap_or_else(|e| die(&e));
    let scale = args.scale.unwrap_or(2.0);
    let size = args.size.unwrap_or(app.spec.size);
    // Check the encoder before app initialization or GPU work.
    let mut writer = crate::record::Writer::new(((size.0 * scale).round() as u32, (size.1 * scale).round() as u32));
    if let Some(path) = &args.video {
        writer.video(path, timeline.fps).unwrap_or_else(|e| die(&e));
    }
    crate::headless_run::publish(crate::headless_run::HeadlessRun {
        script: args.script.clone(),
        events: script.iter().map(Ev::public).collect(),
        scale,
        fps: timeline.fps,
        size,
        start: timeline.start_frame as f64 * timeline.dt,
        frames: timeline.frames,
        end: timeline.last_frame as f64 * timeline.dt,
        desktop: None,
        windows: Default::default(),
    });
    let fit_height = app.fit_content_height;
    let app_options = app.window_options.clone();
    let modifier_keys = app.modifier_keys;
    let tab_visits_controls = app.tab_visits_controls;
    let (spec, state, shared) = app.into_parts(args.scene.as_deref());
    let shortcuts = crate::shortcuts::Guard::attach(&shared, std::sync::Arc::new(|| {}), true, false);
    let mut menus = crate::native_menu::Guard::attach(&shared, true);
    let dark = args.theme.as_deref() != Some("light");
    let state_ms = launched.elapsed().as_secs_f64() * 1000.0;
    let mut rt = runtime_for(&spec, state, size, Env::new(dark, scale), std::sync::Arc::new(|| {}));
    #[cfg(all(feature = "dev", debug_assertions))]
    if let Some(dev) = dev { crate::dev::launch(&mut rt, &spec, dev, std::sync::Arc::new(|| {})); }
    rt.set_titlebar_inset(if app_options.borderless { 0.0 } else { chrome.titlebar_inset() });
    rt.on_focus = on_focus;
    rt.transparent = transparent;
    rt.fit_height = fit_height;
    rt.set_srgb_blend(srgb_blend);
    rt.set_modifier_keys(modifier_keys);
    rt.set_tab_visits_controls(tab_visits_controls);
    // Only limits the app set: the canvas opens at any size, below the default minimum too.
    let limits = (app_options.min_size.or(spec.min_size).unwrap_or((0.0, 0.0)), app_options.max_size);
    if fit_height {
        let height = crate::window_frame::clamp_size((size.0, rt.content_height()), limits.0, limits.1).1;
        rt.set_viewport((size.0, height), scale);
    }
    if args.tree.is_some() { rt.key_warnings = Some(Vec::new()); }
    rt.track_marks = comp.is_some();
    let runtime_ms = launched.elapsed().as_secs_f64() * 1000.0 - state_ms;

    let desktop = desktop(&args, source.as_ref());
    let opened = desktop.open_at(app_options.position, size);
    rt.set_window_origin((opened[0], opened[1]));
    let mut placed = None;
    let Timeline { fps, dt, start_frame, frames, snap_frame, last_frame } = timeline;

    let t0 = Instant::now();
    let features = comp.as_ref().map_or(wgpu::Features::empty(), |c| c.features());
    let gpu = Gpu::try_with_features(Gpu::instance(), None, features).unwrap_or_else(|e| die(&e));
    let target_format = if srgb_blend { OFFSCREEN_FORMAT.remove_srgb_suffix() } else { OFFSCREEN_FORMAT };
    let mut renderer = Renderer::new(&gpu.device, target_format);
    renderer.set_srgb_blend(srgb_blend);
    let (wp, hp) = ((size.0 * scale).round() as u32, (size.1 * scale).round() as u32);
    let target = Offscreen::with_format(&gpu.device, wp, hp, target_format);
    let mut readback = crate::record::Readback::new(&gpu.device, &target, crate::record::DEPTH);
    let alpha = transparent.then(|| crate::surface_alpha::AlphaSurface::new(&gpu.device, target_format, (wp, hp)));
    let mut overlay = (args.keys.is_some() || args.pointer).then(|| {
        let mut ov = crate::keycaps::Overlay::new(size, scale, args.keys, args.keys_theme, args.pointer);
        ov.set_srgb_blend(srgb_blend);
        let mut ui = Renderer::new(&gpu.device, target_format);
        ui.set_srgb_blend(srgb_blend);
        (ov, ui)
    });
    let init_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let mut trace = args.trace.as_ref().map(|p| {
        let mut trace = Trace::open(p, size, fps, scale);
        trace.all = args.trace_all;
        trace
    });
    rt.trace_channels = trace.as_ref().map(|_| HashMap::new());
    rt.scene.store_mut().audit_retargets(trace.is_some());
    let mut strip: Vec<Vec<u8>> = Vec::new();
    let crops_px: Vec<[u32; 4]> = args.crops.iter().map(|c| c.map(|v| (v * scale).round() as u32)).collect();
    let gap = (4.0 * scale) as u32;
    let wall = Instant::now();
    let mut cpu_ms = Vec::new();
    let mut anim_ms = Vec::new();
    let mut rest_ms = Vec::new();
    let mut next_ev = 0;
    let mut last_down = (0.0f32, 0.0f32);
    // Events at t = 0 find keys and hit elements in a laid-out first frame, as input reaches a
    // real window only once it has drawn. That frame mounts the initial state with any queued
    // updates; frame 0 then draws the events' effects.
    let mut take_requests = |rt: &mut Runtime<S>| {
        shortcuts.drain();
        let requests = shared.take();
        menus.drain();
        for f in requests.updates {
            rt.update_if(f);
            menus.drain();
        }
        if let Some(p) = requests.position { rt.set_window_origin(desktop.to_points(p)); }
        requests.place
    };
    let mut frames_written = 0usize;
    let has_video = writer.has_video();
    // Finished readbacks, in frame order.
    let mut deliver = |shot: Shot, mut px: Vec<u8>| {
        if args.chrome_overlay
            && let Some(lights) = chrome.traffic_light_rects()
        {
            paint_traffic_lights(&mut px, wp, hp, scale, &lights);
        }
        if shot.dump {
            if args.strip.is_some() {
                strip.push(tile(&px, wp, hp, &crops_px, gap));
            }
            if let Some(dir) = &args.out {
                writer.png(dir.join(format!("frame_{:04}.png", shot.k - start_frame)), px.clone()).unwrap_or_else(|e| die(&e));
            }
        }
        if shot.snap {
            writer.png(args.snap.clone().unwrap(), px.clone()).unwrap_or_else(|e| die(&e));
        }
        if shot.video {
            frames_written += 1;
            writer.frame(px).unwrap_or_else(|e| die(&e));
        }
    };
    let mut early_place = None;
    // `Tx::resize_window` moves the viewport on the virtual clock; the canvas keeps its size.
    let mut frame_anim = None;
    // As on macOS, a frame change asked for while nothing moves waits a frame to start.
    let mut idle = true;
    if script.iter().find(|e| !matches!(e.kind, Kind::Mark(_))).is_some_and(|e| e.t <= 1e-9) {
        early_place = take_requests(&mut rt);
        rt.frame(0.0);
        rt.open_popup_headless();
    }

    #[cfg(feature = "frame-profile")]
    let mut profile = Vec::with_capacity(last_frame + 1);
    for k in 0..=last_frame {
        #[cfg(feature = "frame-profile")]
        let (alloc0, commits0, cpu0) = {
            crate::frame_profile::take();
            (procwatch::alloc::thread_allocations(), rt.stats.commits, crate::frame_profile::cpu_ms())
        };
        let real_t = k as f64 * dt;
        let frame_start = Instant::now();
        while next_ev < script.len() && script[next_ev].t <= real_t + 1e-9 {
            let e = &script[next_ev];
            next_ev += 1;
            let (x, y) = match &e.pos {
                Pos::Xy(x, y) => (*x, *y),
                Pos::Key(key) => rt.find(key).unwrap_or_else(|| die(&format!("script: no element with key {key:?}"))),
                Pos::Rel(dx, dy) => match e.kind {
                    Kind::Wheel(..) | Kind::Fling(..) => rt.pointer().unwrap_or((0.0, 0.0)),
                    _ => (last_down.0 + dx, last_down.1 + dy),
                },
            };
            match &e.kind {
                Kind::Mark(_) => {}
                Kind::Focus(focused) => rt.window_focus(*focused),
                // The one window always shows here.
                Kind::Reopen => if let Some(f) = on_reopen { rt.update(|s, tx| { f(s, true, tx); }); },
                Kind::Shortcut(shortcut) => shared.shortcuts.fire(shortcut).unwrap_or_else(|e| die(&format!("script shortcut: {e}"))),
                Kind::Menu(item) => crate::native_menu::fire(&shared, item).unwrap_or_else(|e| die(&format!("script menu: {e}"))),
                Kind::PopupMenu(picked) => rt.resolve_popup(picked.as_deref()).unwrap_or_else(|e| die(&format!("script pop-up menu: {e}"))),
                Kind::Move => rt.pointer_move(x, y),
                Kind::Down => {
                    last_down = (x, y);
                    rt.pointer_down(x, y);
                }
                Kind::Up => rt.pointer_up(x, y),
                Kind::SecondaryDown => { last_down = (x, y); rt.secondary_down(x, y); }
                Kind::SecondaryUp => rt.secondary_up(x, y),
                Kind::Key(k, false) => rt.key(k.clone()),
                Kind::Key(k, true) => rt.repeat_key(k.clone()),
                Kind::Physical(event) => rt.physical_input(*event),
                Kind::Paste(s) => rt.paste(s.clone()),
                Kind::Wheel(dx, dy) => wheel(&mut rt, (x, y), *dx, *dy),
                Kind::Fling(vx, vy) => fling(&mut rt, (x, y), *vx, *vy),
                Kind::Files(step) => file_input(&mut rt, step, (x, y)),
            }
            if let Some((ov, _)) = overlay.as_mut() {
                match &e.kind {
                    Kind::Key(key, _) => ov.key(e.t, key),
                    Kind::Shortcut(chord) => ov.chord(e.t, chord),
                    &Kind::Physical(crate::PhysicalInput::Key { key, mods, pressed: true, .. }) => {
                        if let Ok(chord) = crate::Shortcut::from_physical_key(key, mods) { ov.chord(e.t, &chord) }
                    }
                    Kind::Down => ov.press(e.t, x, y),
                    _ => {}
                }
            }
        }
        let requested_place = take_requests(&mut rt);
        take_window_frame(&mut rt, &mut frame_anim, real_t, limits);
        if !idle { step_window_frame(&mut rt, &mut frame_anim, real_t, scale); }
        rt.window_resting = frame_anim.is_none() && rt.window_frame.is_none();
        let step = if k == 0 { 0.0 } else { dt };
        let mut animating = rt.frame(step);
        rt.open_popup_headless();
        // As on macOS, a frame change this commit asked for starts with the springs it
        // started, which have shown a step unless they held for the window.
        if rt.window_frame.is_some() {
            let shown = if rt.fit_held { 0.0 } else { step };
            take_window_frame(&mut rt, &mut frame_anim, real_t - shown, limits);
        }
        idle = !animating && frame_anim.is_none();
        // Updates, poll and input handlers place too; the latest request this frame wins.
        if let Some(place) = shared.take_place().or(requested_place).or(early_place.take()) {
            let (display, frame) = desktop.place(&place, size);
            rt.set_window_origin((frame[0], frame[1]));
            let record = json!({ "t": real_t, "display": display, "frame": frame });
            println!("rune-placed {record}");
            placed = Some(record);
        }
        let update_ms = frame_start.elapsed().as_secs_f64() * 1000.0;
        renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
        renderer.prepare_surfaces(&gpu.device, &gpu.queue, &rt.surfaces, (wp, hp));
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        let view = alpha.as_ref().map_or(&target.view, |a| &a.view);
        if let Some(c) = comp.as_mut() {
            let mut f = Frame {
                device: &gpu.device,
                queue: &gpu.queue,
                encoder: &mut enc,
                target: view,
                format: target_format,
                size: (wp, hp),
                dt: step,
                update_ms,
                rt: &mut rt,
                ui: &mut renderer,
            };
            c.render(&mut f);
            animating |= c.animating();
        } else {
            renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (wp, hp));
            renderer.draw(&mut enc, view, rt.clear_color());
        }
        if let Some((ov, ui)) = overlay.as_mut() {
            let ort = ov.frame(real_t, step, rt.backdrop_dark().unwrap_or(dark));
            if !ort.instances.is_empty() {
                ui.prepare(&gpu.device, &gpu.queue, &mut ort.text.atlas, &ort.instances, (wp, hp));
                ui.draw_over(&mut enc, view);
            }
        }
        if let Some(alpha) = &alpha { alpha.draw(&mut enc, &target.view); }
        let in_range = k >= start_frame && k < start_frame + frames;
        let shot = Shot {
            k,
            dump: in_range && (args.out.is_some() || args.strip.is_some()) && (k - start_frame).is_multiple_of(args.every),
            video: in_range && has_video,
            snap: k == snap_frame && args.snap.is_some(),
        };
        let first_pixels = args.first_pixels && k == 0;
        let want_pixels = shot.dump || shot.video || shot.snap || first_pixels;
        if want_pixels {
            for (shot, px) in readback.make_room(&gpu.device) { deliver(shot, px); }
            readback.copy(&mut enc, &target, shot);
        }
        renderer.submit(&gpu.queue, enc);
        if want_pixels { readback.submitted(); }
        let ms = frame_start.elapsed().as_secs_f64() * 1000.0;
        #[cfg(feature = "frame-profile")]
        profile.push((crate::frame_profile::take(), update_ms, ms - update_ms,
            procwatch::alloc::thread_allocations() - alloc0, rt.stats.commits - commits0, rt.stats.shaped, animating,
            cpu0.map(|start| crate::frame_profile::cpu_ms().unwrap() - start)));
        cpu_ms.push(ms);
        if k > 0 {
            if animating { anim_ms.push(ms) } else { rest_ms.push(ms) }
        }

        if comp.is_some() {
            // Pace like a display link so a compositor's async readbacks (GPU timers) land between frames.
            gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        }
        if let Some(tr) = trace.as_mut() {
            tr.frame(&mut rt, real_t * 1000.0, ms);
        }
        if first_pixels {
            let wait = Instant::now();
            let done = readback.finish(&gpu.device);
            let marker = json!({
                "headless_ms": launched.elapsed().as_secs_f64() * 1000.0,
                "state_ms": state_ms, "runtime_ms": runtime_ms, "gpu_init_ms": init_ms,
                "frame_cpu_ms": ms, "readback_ms": wait.elapsed().as_secs_f64() * 1000.0,
                "pixel_bytes": done.last().map_or(0, |(_, px)| px.len()),
            });
            println!("rune-first-pixels {marker}");
            std::io::stdout().flush().unwrap();
            for (shot, px) in done { deliver(shot, px); }
        } else if want_pixels {
            for (shot, px) in readback.ready(&gpu.device) { deliver(shot, px); }
        }
        if k == snap_frame
            && args.chrome_overlay
            && let Some(lights) = chrome.traffic_light_rects()
        {
            report_traffic_light_overlaps(&rt, &lights);
        }
        if k == snap_frame
            && let Some(p) = &args.tree
        {
            let mut tree = tree_json(&rt, real_t);
            if let Some(placed) = &placed { tree["placed"] = placed.clone(); }
            let text = serde_json::to_string_pretty(&tree).unwrap();
            std::fs::write(p, text).unwrap_or_else(|e| die(&format!("{}: {e}", p.display())));
        }
    }
    for (shot, px) in readback.finish(&gpu.device) { deliver(shot, px); }
    drop(deliver);
    let written = frames_written;
    if let Some(video) = writer.finish().unwrap_or_else(|e| die(&e)) {
        eprintln!("rune: wrote {} ({written} frames at {fps} fps)", video.display());
    }
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();

    if let Some(p) = &args.strip { write_strip(p, &strip, (wp, hp), &crops_px, gap, args.cols); }

    if frames > 0 {
        let wall = wall.elapsed().as_secs_f64();
        eprintln!("rune: {} frames in {wall:.2} s wall ({:.1} frames/s)", last_frame + 1, (last_frame + 1) as f64 / wall);
    }
    let q = |v: &mut Vec<f64>, p: f64| {
        v.sort_by(f64::total_cmp);
        if v.is_empty() { 0.0 } else { v[((v.len() - 1) as f64 * p).round() as usize] }
    };
    let mut sorted = cpu_ms.clone();
    #[cfg(feature = "frame-profile")]
    {
        const P: usize = crate::frame_profile::PHASES;
        let rows = &profile[usize::from(profile.len() > 1)..];
        let n = rows.len() as f64;
        let avg = |f: fn(&([f64; 2 * P], f64, f64, u64, u64, u32, bool, Option<f64>)) -> f64| rows.iter().map(f).sum::<f64>() / n;
        eprintln!("rune-frame-profile {}", json!({
            "view_ms": avg(|r| r.0[0]), "commit_inclusive_ms": avg(|r| r.0[1]),
            "layout_inclusive_ms": avg(|r| r.0[2]), "shape_ms": avg(|r| r.0[3]),
            "paint_ms": avg(|r| r.0[4]), "apply_ms": avg(|r| r.0[5]), "update_ms": avg(|r| r.1), "encode_submit_ms": avg(|r| r.2),
            "allocations": avg(|r| r.3 as f64), "commits": avg(|r| r.4 as f64),
            "shaped": avg(|r| r.5 as f64), "animating_fraction": avg(|r| u8::from(r.6) as f64),
            "thread_cpu_ms": rows[0].7.map(|_| avg(|r| r.7.unwrap())),
            "view_cpu_ms": rows[0].7.map(|_| avg(|r| r.0[P])),
            "commit_cpu_ms": rows[0].7.map(|_| avg(|r| r.0[P + 1])),
            "layout_cpu_ms": rows[0].7.map(|_| avg(|r| r.0[P + 2])),
            "shape_cpu_ms": rows[0].7.map(|_| avg(|r| r.0[P + 3])),
            "paint_cpu_ms": rows[0].7.map(|_| avg(|r| r.0[P + 4])),
            "apply_cpu_ms": rows[0].7.map(|_| avg(|r| r.0[P + 5])),
        }));
    }
    let steady: Vec<f64> = cpu_ms.iter().skip(1).copied().collect();
    let mean = if steady.is_empty() { cpu_ms[0] } else { steady.iter().sum::<f64>() / steady.len() as f64 };
    eprintln!(
        "rune: {} frames at {fps} fps, gpu init {init_ms:.1} ms, first frame {:.2} ms, frame cpu mean {mean:.3} ms p50 {:.3} p95 {:.3} max {:.3}, {} instances",
        cpu_ms.len(),
        cpu_ms[0],
        q(&mut sorted, 0.5),
        q(&mut sorted, 0.95),
        q(&mut sorted, 1.0),
        rt.instances.len()
    );
    let (a50, a95) = (q(&mut anim_ms, 0.5), q(&mut anim_ms, 0.95));
    let (r50, r95) = (q(&mut rest_ms, 0.5), q(&mut rest_ms, 0.95));
    eprintln!(
        "rune: animating frames {} (cpu p50 {a50:.3} p95 {a95:.3} ms), at-rest frames {} (the window draws none; forced cpu p50 {r50:.3} p95 {r95:.3} ms), layout nodes {}, glyphs {}",
        anim_ms.len(),
        rest_ms.len(),
        rt.stats.layout_nodes,
        rt.stats.glyphs
    );
    if let Some(s) = comp.as_mut().and_then(|c| c.summary(&gpu.device)) {
        eprintln!("{s}");
    }
}

/// Tiles `--strip`'s frames, `cols` to a row, into one PNG.
pub(crate) fn write_strip(p: &std::path::Path, strip: &[Vec<u8>], (wp, hp): (u32, u32), crops_px: &[[u32; 4]], gap: u32, cols: usize) {
    if strip.is_empty() { return; }
    let (cw, ch) = tile_size(wp, hp, crops_px, gap);
    let cols = strip.len().min(cols) as u32;
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

/// Takes the size handlers asked for a headless window, inside the limits the app set; its
/// viewport moves there on the virtual clock from `start`. Centring has nothing to centre on.
fn take_window_frame<S>(rt: &mut Runtime<S>, anim: &mut Option<crate::window_frame::FrameAnim>, start: f64,
    (min, max): ((f32, f32), Option<(f32, f32)>)) {
    use crate::window_frame::{FrameAnim, clamp_size};
    rt.fit_held = false;
    let Some(request) = rt.window_frame.take() else { return };
    let Some(size) = request.size else { return };
    let (w, h) = clamp_size(size, min, max);
    let from = [0.0, 0.0, rt.viewport.0, rt.viewport.1];
    *anim = Some(FrameAnim::retarget(anim.as_ref(), request.spring, start, from, [0.0, 0.0, w, h]));
}

/// Moves a headless window's viewport one frame along, before the frame lays out.
fn step_window_frame<S>(rt: &mut Runtime<S>, anim: &mut Option<crate::window_frame::FrameAnim>, now: f64, scale: f32) {
    let Some(moving) = *anim else { return };
    let [_, _, w, h] = crate::window_frame::snap(moving.sample(now).0, scale);
    rt.step_viewport((w, h), scale);
    if moving.done(now) { *anim = None; }
}

/// Which frames a headless run renders and dumps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Timeline {
    pub fps: f64,
    pub dt: f64,
    pub start_frame: usize,
    /// Frames dumped from `start_frame` (0 when nothing is dumped).
    pub frames: usize,
    pub snap_frame: usize,
    pub last_frame: usize,
}

impl Timeline {
    pub(crate) fn of(args: &Args, script: &[Ev]) -> Result<Timeline, String> {
        let mark = |name: &str| script.iter().find_map(|e| matches!(&e.kind, Kind::Mark(n) if n == name).then_some(e.t));
        let resolve = |spec: &String| {
            // Exact names win over offsets, including names such as "step-1" or "end+0.5".
            if let Some(t) = mark(spec) { return Ok(t); }
            let names: Vec<_> = script.iter().filter_map(|e| match &e.kind { Kind::Mark(n) => Some(n.as_str()), _ => None }).collect();
            let known = if names.is_empty() { "none".into() } else { names.join(", ") };
            // Only a complete numeric suffix is an offset. Starting at the first valid sign
            // keeps a scientific exponent's sign inside the number (end-1e-3).
            for (i, _) in spec.char_indices().skip(1).filter(|&(_, c)| c == '+' || c == '-') {
                if let Ok(offset) = spec[i..].parse::<f64>() && offset.is_finite() {
                    let name = &spec[..i];
                    return mark(name).map(|t| (t + offset).max(0.0))
                        .ok_or_else(|| format!("no mark {name:?} in the script (marks: {known})"));
                }
            }
            if spec.contains(['+', '-']) {
                Err(format!("invalid mark time {spec:?}: expected an exact mark name or NAME+SECONDS/NAME-SECONDS (marks: {known})"))
            } else {
                Err(format!("no mark {spec:?} in the script (marks: {known})"))
            }
        };
        let fps = args.fps.max(1.0);
        let dt = 1.0 / fps;
        let start = args.start_mark.as_ref().map_or(Ok(args.start), resolve)?;
        let at = args.at_mark.as_ref().map(resolve).transpose()?.or(args.at);
        let start_frame = (start * fps).round() as usize;
        let script_end = script.last().map_or(0.0, |e| e.t);
        // Output without a length runs to the `end` mark, or one second after the script.
        let until = |t: f64| ((t * fps).round() as usize).saturating_sub(start_frame) + 1;
        let auto = || until(mark("end").unwrap_or(script_end + 1.0));
        let frames = args.frames.unwrap_or_else(|| match (args.duration, args.video.is_some() || args.out.is_some()) {
            (Some(Length::Seconds(d)), _) => (d * fps).round().max(1.0) as usize,
            (Some(Length::Auto), _) | (None, true) => auto(),
            (None, false) => 0,
        });
        let snap_t = at.unwrap_or(if frames > 0 {
            (start_frame + frames - 1) as f64 * dt
        } else if script.is_empty() {
            0.0
        } else {
            script_end + 1.0
        });
        let snap_frame = (snap_t * fps).round() as usize;
        let last_frame = snap_frame.max(start_frame + frames.max(1) - 1);
        Ok(Timeline { fps, dt, start_frame, frames, snap_frame, last_frame })
    }
}

/// A frame whose pixels are being read back, and where they go.
pub(crate) struct Shot {
    pub k: usize,
    pub dump: bool,
    pub video: bool,
    pub snap: bool,
}

/// Antialiased discs where macOS draws close, minimise and zoom, plus a hairline box 4 pt
/// around them (the clearance an app should keep).
fn paint_traffic_lights(px: &mut [u8], w: u32, h: u32, scale: f32, lights: &[[f32; 4]; 3]) {
    const COLORS: [[f32; 3]; 3] = [[255.0, 95.0, 87.0], [254.0, 188.0, 46.0], [40.0, 200.0, 64.0]];
    let mut blend = |x: i64, y: i64, c: [f32; 3], a: f32| {
        if x < 0 || y < 0 || x >= i64::from(w) || y >= i64::from(h) || a <= 0.0 {
            return;
        }
        let i = ((y as u32 * w + x as u32) * 4) as usize;
        for k in 0..3 {
            px[i + k] = (f32::from(px[i + k]) * (1.0 - a) + c[k] * a).round() as u8;
        }
    };
    for (r, c) in lights.iter().zip(COLORS) {
        let (cx, cy, rad) = ((r[0] + r[2] / 2.0) * scale, (r[1] + r[3] / 2.0) * scale, r[2] / 2.0 * scale);
        for y in (cy - rad - 1.0) as i64..=(cy + rad + 1.0) as i64 {
            for x in (cx - rad - 1.0) as i64..=(cx + rad + 1.0) as i64 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                blend(x, y, c, (rad - d + 0.5).clamp(0.0, 1.0));
            }
        }
    }
    let m = 4.0;
    let (x0, y0) = ((lights[0][0] - m) * scale, (lights[0][1] - m) * scale);
    let (x1, y1) = ((lights[2][0] + lights[2][2] + m) * scale, (lights[2][1] + lights[2][3] + m) * scale);
    let cyan = [0.0, 200.0, 255.0];
    for x in x0 as i64..=x1 as i64 {
        blend(x, y0 as i64, cyan, 0.8);
        blend(x, y1 as i64, cyan, 0.8);
    }
    for y in y0 as i64..=y1 as i64 {
        blend(x0 as i64, y, cyan, 0.8);
        blend(x1 as i64, y, cyan, 0.8);
    }
}

/// Lists visible text, icons and controls whose drawn rect comes within 4 pt of a traffic light.
fn report_traffic_light_overlaps<S>(rt: &Runtime<S>, lights: &[[f32; 4]; 3]) {
    let m = 4.0;
    let zone = [lights[0][0] - m, lights[0][1] - m, lights[2][0] + lights[2][2] + m, lights[2][1] + lights[2][3] + m];
    let mut n = 0;
    for e in rt.elements() {
        let drawn = e.text.is_some() || matches!(e.role, "icon" | "button" | "textbox");
        let v = e.visible;
        let (x1, y1) = (v[0] + v[2], v[1] + v[3]);
        if drawn && e.opacity > 0.01 && v[2] > 0.0 && v[3] > 0.0 && v[0] < zone[2] && x1 > zone[0] && v[1] < zone[3] && y1 > zone[1] {
            eprintln!("rune: chrome: {} overlaps the traffic lights at {:?}", e.path, e.visible.map(r2));
            n += 1;
        }
    }
    eprintln!("rune: chrome: traffic lights at {:?}, {n} overlapping elements", lights[0].map(r2));
}

pub(crate) fn tile_size(w: u32, h: u32, crops: &[[u32; 4]], gap: u32) -> (u32, u32) {
    if crops.is_empty() {
        return (w, h);
    }
    let cw = crops.iter().map(|c| c[2]).max().unwrap_or(w);
    let ch = crops.iter().map(|c| c[3]).sum::<u32>() + gap * (crops.len() as u32 - 1);
    (cw, ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_annotations_from_hidden_poll_survive_idle_catch_up_once() {
        let mut rt = Runtime::new(false, |_, _: &mut Cx<bool>| crate::el(), (100.0, 100.0), Env::default());
        let (trace, buffer) = rune_motion::MotionTrace::in_memory(&rune_motion::TraceHeader {
            launch: "hidden".into(), width: 100.0, height: 100.0, hz: 60.0, scale: 1.0,
        });
        rt.scene.set_trace(Some(trace));
        rt.trace_channels = Some(Default::default());
        rt.poll = Some(|pending, _, tx| {
            if !std::mem::take(pending) { return false; }
            tx.trace_event("hidden:ready");
            tx.trace_channel("shell.width", "position", 40.0);
            true
        });
        rt.frame(0.0);
        rt.state = true;
        let commits = rt.stats.commits;
        rt.poll_hidden();
        assert_eq!(rt.stats.commits, commits, "hidden polling must not lay out or render");
        rt.frame_split(1.0, 0.0);
        rt.frame(0.02);
        assert_eq!(rt.events, ["hidden:ready"]);
        assert_eq!(rt.trace_channels.as_ref().unwrap()["shell.width"], ("position", 40.0));
        rt.scene.take_trace().unwrap().flush().unwrap();
        let lines: Vec<Value> = buffer.contents().lines().map(|s| serde_json::from_str(s).unwrap()).collect();
        assert_eq!(lines.iter().filter(|line| line["ev"] == json!(["hidden:ready"])).count(), 1);
    }

    #[test]
    fn trace_annotations_reach_both_writers_once() {
        fn view(changed: &bool, _: &mut Cx<bool>) -> El {
            let (class, host) = if *changed { ("other", "other-host") } else { ("gpui", "pill") };
            crate::el().key("root").trace_namespace(class, host)
                .child(crate::el().key("generation-7").trace_name("chip:Area").size(30.0, 20.0))
                .child(crate::el().key("fixed").trace_name("fixed").trace_namespace("gpui", "pill"))
        }
        let mut rt = Runtime::new(false, view, (100.0, 100.0), Env { scale: 1.0, system_dark: true });
        let (trace, buffer) = rune_motion::MotionTrace::in_memory(&rune_motion::TraceHeader {
            launch: "pill".into(), width: 100.0, height: 100.0, hz: 60.0, scale: 1.0,
        });
        rt.scene.set_trace(Some(trace));
        rt.trace_channels = Some(Default::default());
        let path = std::env::temp_dir().join(format!("rune-annotations-{}.jsonl", std::process::id()));
        {
            let mut trace = Trace::open(&path, rt.viewport, 60.0, 1.0);
            rt.update(|_, tx| {
                assert!(tx.tracing());
                tx.trace_event(format_args!("lab:{}", "Start"));
                tx.trace_channel("shell.width", "position", 30.0);
            });
            // A second transaction before the frame must retain the first event.
            rt.update(|_, tx| {
                tx.trace_event("lab:Ready");
                tx.trace_channel("shell.width", "position", 40.0);
            });
            rt.frame(0.0);
            trace.frame(&mut rt, 0.0, 0.0);
            rt.frame(0.02);
            trace.frame(&mut rt, 20.0, 0.0);
            rt.update(|s, _| *s = true);
            rt.frame(0.02);
            trace.frame(&mut rt, 40.0, 0.0);
        }
        rt.scene.take_trace().unwrap().flush().unwrap();
        let tooling = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        for text in [tooling, buffer.contents()] {
            let lines: Vec<Value> = text.lines().map(|s| serde_json::from_str(s).unwrap()).collect();
            let chip = lines[1]["m"].as_array().unwrap().iter().find(|m| m["nm"] == "chip:Area").unwrap();
            assert_eq!(chip["cls"], "gpui");
            assert_eq!(chip["host"], "pill");
            assert_eq!(lines[1]["ev"], json!(["lab:Start", "lab:Ready"]));
            let channel = lines[1]["cm"].as_array().unwrap().iter().find(|m| m["name"] == "shell.width").unwrap();
            assert_eq!(channel["kind"], "position");
            assert!(lines[1]["c"].as_array().unwrap().iter().any(|c| c[0] == channel["id"] && c[1].as_f64() == Some(40.0)));
            for field in ["cm", "c", "ev"] { assert!(lines[2][field].is_null(), "duplicate {field}"); }
            let updated = lines[3]["m"].as_array().unwrap();
            let renamed = updated.iter().find(|m| m["nm"] == "chip:Area").unwrap();
            assert_eq!(renamed["id"], chip["id"]);
            assert_eq!(renamed["cls"], "other");
            assert_eq!(renamed["host"], "other-host");
            assert!(!updated.iter().any(|m| m["nm"] == "fixed"), "descendant override must survive parent namespace changes");
        }
    }

    #[test]
    fn native_traces_add_target_and_present_times_without_changing_other_fields() {
        struct Presents(std::sync::Mutex<Vec<crate::present_time::Presented>>);
        impl crate::present_time::Source for Presents {
            fn take(&self, out: &mut Vec<crate::present_time::Presented>) { out.append(&mut self.0.lock().unwrap()); }
        }
        let view = |_: &bool, _: &mut Cx<bool>| crate::el().key("box").size(20.0, 20.0);
        let run = |name: &str, timed: bool| {
            let mut rt = Runtime::new(false, view, (100.0, 100.0), Env { scale: 1.0, system_dark: true });
            let path = std::env::temp_dir().join(format!("rune-present-{name}-{}.jsonl", std::process::id()));
            let start = Instant::now();
            let source = std::sync::Arc::new(Presents(Default::default()));
            {
                let mut trace = Trace::open_clock(&path, rt.viewport, 120.0, 1.0, "wall");
                if timed { trace.record_presents(source.clone(), start); }
                for k in 0..3u64 {
                    rt.frame(1.0 / 120.0);
                    let timing = FrameTiming { target_ms: timed.then_some(k as f64 * 8.333), drawable: timed.then_some(k + 1), continuous: k > 0 };
                    trace.frame_timed(&mut rt, k as f64 * 8.0, 0.0, timing);
                    let at = start + std::time::Duration::from_micros(20_000 + k * 8_333);
                    source.0.lock().unwrap().push((k + 1, Some(at)));
                }
            }
            let text = std::fs::read_to_string(&path).unwrap();
            std::fs::remove_file(path).unwrap();
            text
        };
        let plain = run("plain", false);
        let timed = run("timed", true);
        let strip = |text: &str| -> Vec<Value> {
            text.lines().map(|l| {
                let mut v: Value = serde_json::from_str(l).unwrap();
                for key in ["target_ms", "present_ms", "present_dt_ms"] { v.as_object_mut().unwrap().remove(key); }
                if let Some(q) = v.get_mut("quality") { q["cpu_ms"] = 0.into(); }
                v
            }).collect()
        };
        assert_eq!(strip(&plain), strip(&timed), "present timing only adds fields");
        assert!(!plain.contains("present_ms") && !plain.contains("target_ms"), "untimed frames keep the old format");
        let frames: Vec<Value> = timed.lines().skip(1).map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(frames.len(), 3, "every held line is written when the trace ends");
        assert_eq!(frames[1]["target_ms"], 8.333);
        assert_eq!(frames[1]["present_ms"], 28.333);
        assert_eq!(frames[1]["present_dt_ms"], 8.333);
        let summary = crate::present_time::Summary::of_trace(&timed).unwrap();
        assert_eq!((summary.frames, summary.presented, summary.bunched, summary.missed), (3, 3, 0, 0));
        assert!(crate::present_time::Summary::of_trace(&plain).is_none());
    }

    #[test]
    fn marks_are_inert_events_that_times_can_name() {
        let script = parse_script(json!([
            {"t": 0.5, "type": "tap", "x": 1, "y": 1},
            {"t": 3.2, "type": "mark", "name": "end"},
            {"t": 1.0, "type": "mark", "name": "end-card"},
        ]));
        let marks: Vec<_> = script.iter().filter_map(|e| match &e.kind { Kind::Mark(n) => Some((n.as_str(), e.t)), _ => None }).collect();
        assert_eq!(marks, [("end-card", 1.0), ("end", 3.2)]);
        assert!(matches!(script[4].public().event, ScriptEvent::Mark(ref n) if n == "end"));

        assert_eq!(mark_time("mark:end"), Some("end".into()));
        assert_eq!(mark_time("mark:end+0.5"), Some("end+0.5".into()));
        assert_eq!(mark_time("1.5"), None);

        let args = |v: &[&str]| Args::parse_from(v.iter().map(|s| (*s).into()).collect());
        let timeline = |v: &[&str]| Timeline::of(&args(v), &script);
        let snap = timeline(&["--snap", "a.png", "--at", "mark:end+0.5"]).unwrap();
        assert_eq!((snap.snap_frame, snap.frames), (222, 0));
        let video = timeline(&["--video", "a.mp4", "--start", "mark:end-card"]).unwrap();
        assert_eq!((video.start_frame, video.frames, video.last_frame), (60, 133, 192), "auto runs to the end mark");
        let auto = timeline(&["--frames", "1", "--duration", "auto"]).unwrap();
        assert_eq!(auto.frames, 1, "--frames wins over --duration");
        let seconds = timeline(&["--video", "a.mp4", "--duration", "2", "--fps", "30"]).unwrap();
        assert_eq!((seconds.frames, seconds.last_frame), (60, 59));
        let err = timeline(&["--snap", "a.png", "--at", "mark:intro"]).unwrap_err();
        assert_eq!(err, "no mark \"intro\" in the script (marks: end-card, end)");
        let unmarked = Timeline::of(&args(&["--video", "a.mp4"]), &parse_script(json!([{"t": 0.5, "type": "tap", "x": 1, "y": 1}]))).unwrap();
        assert_eq!(unmarked.frames, 96, "without an end mark, one second after the last event (the tap's release at 0.58 s)");
    }

    /// Headless runs publish the process's `headless_run`, so tests that render one take turns.
    static HEADLESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn mark_names_win_over_offsets_and_offsets_must_be_complete() {
        let script = parse_script(json!([
            {"t": 2.0, "type": "mark", "name": "step-1"},
            {"t": 4.0, "type": "mark", "name": "end-card"},
            {"t": 6.0, "type": "mark", "name": "end+0.5"},
            {"t": 8.0, "type": "mark", "name": "step"},
            {"t": 10.0, "type": "mark", "name": "end"},
        ]));
        let resolve = |flag: &str, value: &str| {
            let args = Args::parse_from(["--snap", "a.png", "--fps", "1000", flag, value].map(String::from).into());
            Timeline::of(&args, &script)
        };
        for (value, frame) in [("mark:step-1", 2000), ("mark:step-1+0.25", 2250),
            ("mark:step-1-0.5", 1500), ("mark:end+0.5", 6000), ("mark:end-card-0.25", 3750),
            ("mark:end-1e-3", 9999), ("mark:end+1e-3", 10001)] {
            assert_eq!(resolve("--at", value).unwrap().snap_frame, frame, "{value}");
            assert_eq!(resolve("--start", value).unwrap().start_frame, frame, "{value}");
        }
        for value in ["mark:end+0.5s", "mark:end-", "mark:end+NaN", "mark:end+inf", "mark:end+1e999"] {
            let error = resolve("--at", value).unwrap_err();
            assert!(error.contains("invalid mark time") && error.contains("marks: step-1, end-card, end+0.5, step, end"), "{error}");
        }
        let args = Args::parse_from(["--at", "mark:end", "--at", "1", "--start", "mark:step", "--start", "0"].map(String::from).into());
        let timeline = Timeline::of(&args, &script).unwrap();
        assert_eq!((timeline.snap_frame, timeline.start_frame), (60, 0), "the last flag wins");
    }

    #[test]
    fn duration_and_out_select_headless_png_output_unless_native() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        let args = |v: &[&str]| Args::parse_from(v.iter().map(|s| (*s).into()).collect());
        for flags in [vec!["--out", "frames"], vec!["--duration", "auto"], vec!["--duration", "auto", "--out", "frames"]] {
            assert!(args(&flags).headless());
            let mut native = flags;
            native.push("--native");
            assert!(!args(&native).headless());
        }
        let dir = std::env::temp_dir().join(format!("rune-duration-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("script.json");
        std::fs::write(&script, r#"[{"t": 0.1, "type": "mark", "name": "end"}]"#).unwrap();
        let spec = || AppSpec::new("duration", (8.0, 8.0), || (), |_, _| crate::el());
        for automatic in [false, true] {
            let out = dir.join(if automatic { "explicit" } else { "default" });
            let mut flags = vec!["--script", script.to_str().unwrap(), "--out", out.to_str().unwrap(), "--fps", "30", "--scale", "1"];
            if automatic { flags.extend(["--duration", "auto"]); }
            let args = args(&flags);
            assert!(args.headless());
            headless(crate::App::new(spec()), args);
            let mut files: Vec<_> = std::fs::read_dir(out).unwrap().map(|e| e.unwrap().file_name()).collect();
            files.sort();
            assert_eq!(files, ["frame_0000.png", "frame_0001.png", "frame_0002.png", "frame_0003.png"]);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn single_window_snapshots_honor_transparency_and_physical_size() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("rune-window-options-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let render = |name: &str, options: crate::WindowOptions, size: Option<(f32, f32)>, legacy: bool| {
            let spec = AppSpec::new("options", (20.0, 12.0), || (), |_, _| crate::el().w_full().h_full()
                .child(crate::el().absolute(2.0, 2.0).size(8.0, 8.0).fill(crate::oklcha(0.6, 0.2, 30.0, 0.5))));
            let app = crate::App::new(spec).window_options(options);
            let snap = dir.join(format!("{name}.png"));
            let args = Args { snap: Some(snap.clone()), scale: Some(2.0), size, ..Default::default() };
            if legacy { headless_configured(app, args, false); } else { headless(app, args); }
            (std::fs::read(&snap).unwrap(), image::open(snap).unwrap().into_rgba8())
        };
        let plain = render("plain", crate::WindowOptions::default(), None, false);
        let legacy = render("legacy", crate::WindowOptions::default(), None, true);
        assert_eq!(plain.0, legacy.0, "default windows retain byte-identical PNG output");
        assert_eq!(plain.1.dimensions(), (40, 24));
        assert!(plain.1.pixels().all(|p| p[3] == 255));
        let options = crate::WindowOptions { transparent: true, physical_size: Some((80, 60)), ..Default::default() };
        let transparent = render("transparent", options.clone(), None, false).1;
        assert_eq!(transparent.dimensions(), (80, 60));
        assert_eq!(transparent.get_pixel(0, 0)[3], 0);
        assert!((127..=128).contains(&transparent.get_pixel(8, 8)[3]));
        let overridden = render("override", options, Some((16.0, 10.0)), false).1;
        assert_eq!(overridden.dimensions(), (32, 20), "--size overrides the physical-size option");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn window_resizes_move_the_headless_viewport_on_the_virtual_clock() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("rune-window-resize-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("script.json");
        std::fs::write(&script, r#"[{"t": 0.1, "type": "tap", "key": "grow"}]"#).unwrap();
        let viewport = |name: &str, instant: bool, at: &str| {
            let spec = AppSpec::new("resize", (300.0, 400.0), || false, |_, _| crate::el().key("root").w_full().h_full()
                .child(crate::el().key("grow").size(50.0, 50.0).on_press_tx(|instant: &mut bool, tx| {
                    if *instant { tx.resize_window_instant((300.0, 600.0)) } else { tx.resize_window((300.0, 600.0)) }
                })));
            let tree = dir.join(format!("{name}.json"));
            let args = ["--script", script.to_str().unwrap(), "--scale", "1", "--fps", "60", "--at", at, "--tree", tree.to_str().unwrap()];
            headless(crate::App::new(spec).init(move || instant), Args::parse_from(args.map(String::from).into()));
            let tree: Value = serde_json::from_str(&std::fs::read_to_string(tree).unwrap()).unwrap();
            let size = tree["viewport"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
            (size[0], size[1])
        };
        assert_eq!(viewport("before", false, "0.1"), (300.0, 400.0));
        // The press lands with the release at 0.18 s, on frame 11; as on macOS, frame 12 moves.
        assert_eq!(viewport("released", false, "0.1834"), (300.0, 400.0));
        let (_, first) = viewport("first", false, "0.2");
        assert!(first > 400.0, "moves the frame after the press: {first}");
        let (w, h) = viewport("moving", false, "0.3");
        assert!(w == 300.0 && h > 400.0 && h < 600.0, "mid-spring at 0.3 s: {h}");
        assert_eq!(viewport("moving-again", false, "0.3"), (w, h), "the virtual clock is deterministic");
        assert_eq!(viewport("landed", false, "2"), (300.0, 600.0));
        assert_eq!(viewport("instant", true, "0.3"), (300.0, 600.0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fit_content_height_follows_the_content() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("rune-fit-content-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("script.json");
        std::fs::write(&script, r#"[{"t": 0.1, "type": "tap", "key": "more"}]"#).unwrap();
        let viewport = |name: &str, at: &str| {
            let spec = AppSpec::new("fit", (300.0, 400.0), || 1usize, |rows, _| crate::col().key("root").w_full()
                .child(crate::el().key("more").size(50.0, 50.0).on_press(|rows: &mut usize| *rows += 1))
                .children((0..*rows).map(|i| crate::el().key(format!("row{i}")).size(100.0, 40.0))));
            let tree = dir.join(format!("{name}.json"));
            let args = ["--script", script.to_str().unwrap(), "--scale", "2", "--fps", "60", "--at", at, "--tree", tree.to_str().unwrap()];
            headless(crate::App::new(spec).fit_content_height(), Args::parse_from(args.map(String::from).into()));
            let tree: Value = serde_json::from_str(&std::fs::read_to_string(tree).unwrap()).unwrap();
            let size = tree["viewport"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
            (size[0], size[1])
        };
        assert_eq!(viewport("open", "0"), (300.0, 90.0), "opens at the content's height");
        let (_, h) = viewport("moving", "0.3");
        assert!(h > 90.0 && h < 130.0, "follows on the change's spring: {h}");
        assert_eq!(viewport("landed", "2"), (300.0, 130.0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn apps_read_the_headless_run_from_init() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("rune-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("script.json");
        std::fs::write(&script, r#"[{"t": 0.5, "type": "tap", "x": 4, "y": 5}, {"t": 0.25, "type": "type", "text": "hi", "every": 0.1}]"#).unwrap();
        fn init() -> usize {
            let run = headless_run().expect("published before init");
            assert_eq!((run.size(), run.scale(), run.fps()), ((37.0, 23.0), 1.0, 30.0));
            run.events().len()
        }
        let spec = AppSpec::new("run", (37.0, 23.0), init, |_, _| crate::el());
        let tree = dir.join("tree.json");
        let args = ["--script", script.to_str().unwrap(), "--scale", "1", "--fps", "30", "--tree", tree.to_str().unwrap()];
        headless(crate::App::new(spec), Args::parse_from(args.map(String::from).into()));
        let run = headless_run().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(run.script(), Some(script.as_path()));
        let summary: Vec<String> = run.events().iter().map(|e| match &e.event {
            ScriptEvent::Key { key: KeyEvent { key: Key::Text(c), .. }, repeat: false } => format!("{} key {c}", e.t),
            ScriptEvent::Pointer { action, at } => format!("{} {action:?} {at:?}", e.t),
            other => panic!("unexpected {other:?}"),
        }).collect();
        assert_eq!(summary, ["0.25 key h", "0.35 key i", "0.5 Move At(4.0, 5.0)", "0.5 Down At(4.0, 5.0)", "0.58 Up FromDown(0.0, 0.0)"]);
        assert_eq!((run.script_end(), run.frames(), run.start()), (0.58, 0, 0.0));
        // A tree is taken one second after the script, at the nearest frame.
        assert!((run.end() - 47.0 / 30.0).abs() < 1e-9, "{}", run.end());
    }

    #[test]
    fn video_streams_every_frame_into_ffmpeg() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        if std::process::Command::new("ffprobe").arg("-version").output().is_err() {
            eprintln!("skipped: ffmpeg and ffprobe aren't installed");
            return;
        }
        let dir = std::env::temp_dir().join(format!("rune-video-{}", std::process::id()));
        for (extension, codec, width) in [("mp4", "h264", 32), ("mov", "prores", 31), ("webm", "vp9", 32)] {
            let path = dir.join(format!("tiny.{extension}"));
            let out = dir.join(format!("{extension}-frames"));
            let snap = dir.join(format!("{extension}-last.png"));
            let spec = AppSpec::new("video", (31.0, 20.0), || (), |_, _| crate::el().w_full().h_full()
                .child(crate::el().absolute(0.0, 0.0).size(10.0, 10.0).fill(crate::oklch(0.6, 0.2, 30.0))));
            let args = ["--video", path.to_str().unwrap(), "--frames", "12", "--scale", "1", "--fps", "30",
                "--out", out.to_str().unwrap(), "--every", "3", "--snap", snap.to_str().unwrap(), "--at", "0.3"];
            let app = crate::App::new(spec).window_options(crate::WindowOptions {
                transparent: extension == "mov", ..Default::default()
            });
            headless(app, Args::parse_from(args.map(String::from).into()));
            let probe = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-count_frames", "-select_streams", "v:0",
                    "-show_entries", "stream=codec_name,width,height,nb_read_frames,r_frame_rate", "-of", "json"])
                .arg(&path).output().unwrap();
            assert!(probe.status.success(), "{}", String::from_utf8_lossy(&probe.stderr));
            let probe: Value = serde_json::from_slice(&probe.stdout).unwrap();
            let stream = &probe["streams"][0];
            assert_eq!(stream["codec_name"], codec);
            assert_eq!(stream["width"], width, "420 formats pad odd sizes; ProRes does not");
            assert_eq!(stream["height"], 20);
            assert_eq!(stream["r_frame_rate"], "30/1");
            assert_eq!(stream["nb_read_frames"], "12", "--every must not sample video");
            if extension == "mov" {
                let decoded = std::process::Command::new("ffmpeg")
                    .args(["-v", "error", "-i"]).arg(&path)
                    .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"]).output().unwrap();
                assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
                assert_eq!(decoded.stdout.len(), 31 * 20 * 4);
                assert_eq!(decoded.stdout[(15 * 31 + 20) * 4 + 3], 0, "transparent background survives ProRes encoding");
                assert_eq!(decoded.stdout[(5 * 31 + 5) * 4 + 3], 255);
            }
            let mut files: Vec<_> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name()).collect();
            files.sort();
            assert_eq!(files, ["frame_0000.png", "frame_0003.png", "frame_0006.png", "frame_0009.png"]);
            assert_eq!(std::fs::read(snap).unwrap(), std::fs::read(out.join("frame_0009.png")).unwrap());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn desktop_composites_each_window_at_its_frame_with_a_shadow() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("rune-desktop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let backdrop = dir.join("backdrop.png");
        ::image::RgbaImage::from_pixel(240, 160, ::image::Rgba([128, 128, 128, 255])).save(&backdrop).unwrap();
        let script = dir.join("script.json");
        // A click on the back window gives it the keyboard and brings it to the front.
        std::fs::write(&script, r#"[{"t": 0.1, "type": "tap", "x": 45, "y": 35}]"#).unwrap();
        let fill = || AppSpec::new("fill", (1.0, 1.0), || crate::Color::TRANSPARENT, |c, _| crate::el().w_full().h_full().fill(*c));
        let red = crate::Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
        let blue = crate::Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 };
        let render = |name: &str| {
            let snap = dir.join(name);
            let args = ["--desktop", backdrop.to_str().unwrap(), "--script", script.to_str().unwrap(),
                "--scale", "2", "--at", "0.5", "--snap", snap.to_str().unwrap()];
            crate::run_windows_with_args(crate::RunOptions::default(), args.map(String::from), move |displays| {
                assert_eq!(displays[0].size, (240, 160), "one display the backdrop's size");
                [crate::WindowSpec::new("back", fill()).state(red).options(crate::WindowOptions {
                    position: Some((40, 40)), physical_size: Some((100, 60)), ..Default::default()
                }), crate::WindowSpec::new("panel", fill()).state(blue).configured_options(crate::WindowOptions {
                    borderless: true, position: Some((160, 100)), physical_size: Some((40, 40)), ..Default::default()
                }.macos(crate::MacWindowOptions::default().shadow(false)))]
            });
            std::fs::read(snap).unwrap()
        };
        let bytes = render("a.png");
        assert_eq!(bytes, render("b.png"), "byte-identical across runs");
        let png = ::image::load_from_memory(&bytes).unwrap().into_rgba8();
        let px = |x: u32, y: u32| png.get_pixel(x, y).0;
        assert_eq!(png.dimensions(), (240, 160));
        // The back window covers points (20, 20) to (70, 50); the panel (80, 50) to (100, 70).
        let (red, blue) = (|p: [u8; 4]| p[0] > 245 && p[1] < 10 && p[2] < 10, |p: [u8; 4]| p[2] > 245 && p[0] < 10 && p[1] < 10);
        assert!(red(px(90, 70)) && red(px(40, 70)) && red(px(139, 70)) && red(px(90, 40)) && red(px(90, 99)), "the window's pixels land at its frame");
        assert!(!red(px(39, 70)) && !red(px(140, 70)) && !red(px(90, 100)), "and stop at its edges");
        assert!(blue(px(160, 100)) && blue(px(199, 139)) && blue(px(180, 120)));
        assert!(!red(px(40, 40)), "a titled window's corners are rounded");
        let below = px(90, 106);
        assert!(below[0] < 112 && below[0] == below[2], "a soft shadow under the window: {below:?}");
        assert_eq!(px(180, 146), [128, 128, 128, 255], "no shadow when the window asks for none");
        assert_eq!(px(4, 156), [128, 128, 128, 255], "the backdrop elsewhere");
        let run = headless_run().unwrap();
        assert_eq!(run.desktop(), Some(backdrop.as_path()));
        let frames = run.windows_at(0.5);
        let back = frames.iter().find(|f| f.key == "back").unwrap();
        assert_eq!((back.frame, back.visible, back.focused), ([20.0, 20.0, 50.0, 30.0], true, true));
        assert!(run.window_frames().iter().any(|f| f.key == "panel" && f.focused && f.t == 0.0), "the topmost window starts with the keyboard");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A `--desktop` window whose key handler does `act` to another window.
    struct Pair { name: &'static str, color: crate::Color, other: Option<crate::AppHandle<Pair>>, act: fn(&crate::AppHandle<Pair>, &mut crate::Tx) }
    /// The windows `Pair`'s key handler ran in, in order.
    static KEYED: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    const RED: crate::Color = crate::Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
    const GREEN: crate::Color = crate::Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 };
    const BLUE: crate::Color = crate::Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 };

    fn pair(name: &'static str, color: crate::Color, other: Option<crate::AppHandle<Pair>>, act: fn(&crate::AppHandle<Pair>, &mut crate::Tx), options: crate::WindowOptions) -> (crate::WindowSpec<Pair>, crate::AppHandle<Pair>) {
        let spec = AppSpec {
            on_key: Some(|p: &mut Pair, _: &crate::KeyEvent, tx: &mut crate::Tx| {
                KEYED.lock().unwrap().push(p.name);
                if let Some(other) = &p.other { (p.act)(other, tx); }
                true
            }),
            ..AppSpec::new("pair", (1.0, 1.0), || Pair { name: "", color: crate::Color::TRANSPARENT, other: None, act: |_, _| {} },
                |p, _| crate::el().w_full().h_full().fill(p.color))
        };
        let app = crate::App::new(spec).init(move || Pair { name, color, other, act });
        let handle = app.handle();
        (crate::WindowSpec::from_app(name, app).options(options), handle)
    }

    /// Runs `windows` over a grey 120x80-point backdrop at scale 2: the snap at `at`, and the run.
    fn desktop_case(name: &str, script: &str, at: f64, extra: &[&str], windows: impl FnOnce() -> Vec<crate::AnyWindowSpec> + 'static) -> (::image::RgbaImage, &'static HeadlessRun) {
        let dir = std::env::temp_dir().join(format!("rune-desktop-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let backdrop = dir.join("backdrop.png");
        ::image::RgbaImage::from_pixel(240, 160, ::image::Rgba([128, 128, 128, 255])).save(&backdrop).unwrap();
        let (script_path, snap) = (dir.join("script.json"), dir.join("snap.png"));
        std::fs::write(&script_path, script).unwrap();
        KEYED.lock().unwrap().clear();
        let at = at.to_string();
        let mut args = vec!["--desktop", backdrop.to_str().unwrap(), "--script", script_path.to_str().unwrap(),
            "--scale", "2", "--fps", "10", "--at", &at, "--snap", snap.to_str().unwrap()];
        args.extend_from_slice(extra);
        crate::run_windows_with_args(crate::RunOptions::default(), args.into_iter().map(String::from).collect::<Vec<_>>(), move |_| windows());
        let png = ::image::open(&snap).unwrap().into_rgba8();
        std::fs::remove_dir_all(&dir).unwrap();
        (png, headless_run().unwrap())
    }

    /// Window pixels at (20, 20) to (120, 80) of the output, and at (160, 40) to (220, 100).
    fn left() -> crate::WindowOptions { crate::WindowOptions { position: Some((20, 20)), physical_size: Some((100, 60)), ..Default::default() } }
    fn right() -> crate::WindowOptions { crate::WindowOptions { position: Some((160, 40)), physical_size: Some((60, 60)), ..Default::default() } }
    fn focus_of(run: &HeadlessRun, t: f64, key: &str) -> (bool, bool) {
        let f = run.windows_at(t).into_iter().find(|f| f.key == key).unwrap();
        (f.visible, f.focused)
    }

    #[test]
    fn desktop_frame_shows_what_a_handler_did_to_another_window_that_frame() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        // The launcher's Return updates and focuses the notes window and closes the launcher;
        // the frame of that key shows the new note in the focused window.
        let (png, run) = desktop_case("stale", r#"[{"t": 0.1, "type": "key", "name": "Enter"}]"#, 0.1, &[], || {
            let (notes, handle) = pair("notes", RED, None, |_, _| {}, left());
            let (launcher, _) = pair("launcher", BLUE, Some(handle), |notes, tx| {
                notes.update(|p, tx| { p.color = GREEN; tx.instant(); });
                notes.focus();
                tx.close_window();
            }, right());
            vec![notes.boxed(), launcher.boxed()]
        });
        assert_eq!(*KEYED.lock().unwrap(), ["launcher"]);
        let p = png.get_pixel(70, 50).0;
        assert!(p[1] > 245 && p[0] < 10, "the note changed in this frame: {p:?}");
        assert_eq!((focus_of(&run, 0.1, "notes"), focus_of(&run, 0.1, "launcher")), ((true, true), (false, false)));
    }

    #[test]
    fn desktop_window_closing_itself_on_blur_closes_in_that_frame() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        // Return in `a` focuses `b`, which blurs `a`, whose on_focus closes it.
        let script = r#"[{"t": 0.1, "type": "tap", "x": 30, "y": 25}, {"t": 0.2, "type": "key", "name": "Enter"}]"#;
        let (png, run) = desktop_case("blur", script, 0.2, &[], || {
            let (b, handle) = pair("b", BLUE, None, |_, _| {}, right());
            let (a, _) = pair("a", RED, Some(handle), |b, _| b.focus(), left());
            vec![a.on_focus(|_, focused, tx| if !focused { tx.close_window() }).boxed(), b.boxed()]
        });
        assert_eq!(focus_of(&run, 0.1, "a"), (true, true));
        assert_eq!((focus_of(&run, 0.2, "a"), focus_of(&run, 0.2, "b")), ((false, false), (true, true)));
        assert_eq!(png.get_pixel(40, 50).0, [128, 128, 128, 255], "a is gone from the frame");

        // The last shown window closes; losing the keyboard, it opens `c`, which shows at once.
        let (png, run) = desktop_case("blur-open", r#"[{"t": 0.1, "type": "key", "name": "Enter"}]"#, 0.1, &[], || {
            let (c, handle) = pair("c", BLUE, None, |_, _| {}, crate::WindowOptions { visible: false, ..right() });
            let (a, _) = pair("a", RED, Some(handle), |_, tx| tx.close_window(), left());
            let a = a.on_focus(|p, focused, _| if !focused && let Some(c) = &p.other { c.open() });
            vec![c.boxed(), a.boxed()]
        });
        assert_eq!((focus_of(&run, 0.1, "a"), focus_of(&run, 0.1, "c")), ((false, false), (true, true)));
        let p = png.get_pixel(190, 70).0;
        assert!(p[2] > 245 && p[0] < 10, "c draws in that frame: {p:?}");
    }

    #[test]
    fn desktop_click_on_the_backdrop_deactivates_the_app_until_it_refocuses() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        // As on macOS, a click on the desktop activates another app: no window has focus and
        // keys go nowhere until the app is active again.
        let script = r#"[{"t": 0.1, "type": "tap", "x": 110, "y": 75}, {"t": 0.2, "type": "key", "name": "Enter"},
            {"t": 0.3, "focus": true}, {"t": 0.4, "type": "key", "name": "Enter"}]"#;
        let dir = std::env::temp_dir().join(format!("rune-desktop-tree-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tree = dir.join("tree.json");
        let (_, run) = desktop_case("inactive", script, 0.2, &["--tree", tree.to_str().unwrap()], || {
            vec![pair("a", RED, None, |_, _| {}, left()).0.boxed()]
        });
        let tree: Value = serde_json::from_str(&std::fs::read_to_string(&tree).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(tree["windows"][0]["focused"], json!(false), "--tree reports no focus while inactive");
        assert_eq!(focus_of(&run, 0.0, "a"), (true, true));
        assert_eq!(focus_of(&run, 0.2, "a"), (true, false));
        assert!(KEYED.lock().unwrap().is_empty(), "keys while inactive go to another app");

        let (_, run) = desktop_case("inactive", script, 0.5, &[], || vec![pair("a", RED, None, |_, _| {}, left()).0.boxed()]);
        assert_eq!(focus_of(&run, 0.3, "a"), (true, true));
        assert_eq!(*KEYED.lock().unwrap(), ["a"], "only the key after refocusing lands");
    }

    #[test]
    fn desktop_click_through_window_never_takes_the_keyboard() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        // `a` opens and focuses a click-through HUD; `a` keeps the keyboard.
        let script = r#"[{"t": 0.1, "type": "key", "name": "Enter"}, {"t": 0.2, "type": "key", "name": "Enter"}]"#;
        let (_, run) = desktop_case("hud", script, 0.3, &[], || {
            let hud = crate::WindowOptions { visible: false, click_through: true, borderless: true, ..right() };
            let (hud, handle) = pair("hud", BLUE, None, |_, _| {}, hud);
            let (a, _) = pair("a", RED, Some(handle), |hud, _| { hud.open(); hud.focus(); }, left());
            vec![a.boxed(), hud.boxed()]
        });
        assert_eq!((focus_of(&run, 0.1, "a"), focus_of(&run, 0.1, "hud")), ((true, true), (true, false)));
        assert_eq!(*KEYED.lock().unwrap(), ["a", "a"]);
    }

    /// The reopen hooks that ran: window, `has_visible_windows`.
    static REOPENED: std::sync::Mutex<Vec<(&'static str, bool)>> = std::sync::Mutex::new(Vec::new());

    /// Reopen hooks for `closer`: each records the reopen.
    fn record(p: &mut Pair, visible: bool, _: &mut crate::Tx) -> bool { REOPENED.lock().unwrap().push((p.name, visible)); false }
    fn handle(p: &mut Pair, visible: bool, tx: &mut crate::Tx) -> bool { record(p, visible, tx); true }
    /// Opens the other window and leaves the rest to the default.
    fn open_other(p: &mut Pair, visible: bool, tx: &mut crate::Tx) -> bool {
        if let Some(other) = &p.other { other.open(); }
        record(p, visible, tx)
    }

    /// A `keep_running` window that closes on any key, with `hook` as its `on_reopen`.
    fn closer_with(name: &'static str, color: crate::Color, other: Option<crate::AppHandle<Pair>>,
        hook: fn(&mut Pair, bool, &mut crate::Tx) -> bool, options: crate::ConfiguredWindowOptions) -> (crate::AnyWindowSpec, crate::AppHandle<Pair>) {
        let spec = AppSpec {
            on_key: Some(|_: &mut Pair, _: &crate::KeyEvent, tx: &mut crate::Tx| { tx.close_window(); true }),
            ..AppSpec::new("closer", (1.0, 1.0), || Pair { name: "", color: crate::Color::TRANSPARENT, other: None, act: |_, _| {} },
                |p, _| crate::el().w_full().h_full().fill(p.color))
        };
        let app = crate::App::new(spec).init(move || Pair { name, color, other, act: |_, _| {} }).keep_running().on_reopen(hook);
        let handle = app.handle();
        (crate::WindowSpec::from_app(name, app).configured_options(options).boxed(), handle)
    }

    /// `closer_with` that records reopens, handling them when `handles`.
    fn closer(name: &'static str, color: crate::Color, handles: bool, options: crate::ConfiguredWindowOptions) -> crate::AnyWindowSpec {
        closer_with(name, color, None, if handles { handle } else { record }, options).0
    }

    /// Runs `windows` on a headless desktop with `script`, as `desktop_case` does.
    fn reopen_case(script: &str, accessory: bool, windows: impl FnOnce() -> Vec<crate::AnyWindowSpec> + 'static) -> &'static HeadlessRun {
        REOPENED.lock().unwrap().clear();
        let dir = std::env::temp_dir().join(format!("rune-reopen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let backdrop = dir.join("backdrop.png");
        ::image::RgbaImage::from_pixel(240, 160, ::image::Rgba([128, 128, 128, 255])).save(&backdrop).unwrap();
        let (script_path, tree) = (dir.join("script.json"), dir.join("tree.json"));
        std::fs::write(&script_path, script).unwrap();
        let args = ["--desktop", backdrop.to_str().unwrap(), "--script", script_path.to_str().unwrap(),
            "--scale", "2", "--fps", "10", "--tree", tree.to_str().unwrap()];
        let options = crate::RunOptions { accessory, ..Default::default() };
        crate::run_windows_with_args(options, args.map(String::from), move |_| windows());
        std::fs::remove_dir_all(&dir).unwrap();
        headless_run().unwrap()
    }

    #[test]
    fn desktop_reopen_shows_the_window_closed_last_with_the_keyboard() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        // Enter closes `b` (on top, with the keyboard), then `a`; reopening brings back `a`.
        let script = r#"[{"t": 0.1, "type": "key", "name": "Enter"}, {"t": 0.2, "type": "key", "name": "Enter"},
            {"t": 0.3, "reopen": true}]"#;
        let run = reopen_case(script, false, || vec![closer("a", RED, false, left().macos(Default::default())),
            closer("b", BLUE, false, right().macos(Default::default()))]);
        assert_eq!((focus_of(run, 0.2, "a"), focus_of(run, 0.2, "b")), ((false, false), (false, false)));
        assert_eq!((focus_of(run, 0.3, "a"), focus_of(run, 0.3, "b")), ((true, true), (false, false)));
        assert_eq!(*REOPENED.lock().unwrap(), [("a", false), ("b", false)], "every hook runs, before the default");
        assert!(run.events().iter().any(|e| matches!(e.event, ScriptEvent::Reopen)));
    }

    #[test]
    fn desktop_reopen_with_a_window_showing_changes_nothing() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        let script = r#"[{"t": 0.1, "type": "key", "name": "Enter"}, {"t": 0.2, "reopen": true}]"#;
        let run = reopen_case(script, false, || vec![closer("a", RED, false, left().macos(Default::default())),
            closer("b", BLUE, false, right().macos(Default::default()))]);
        assert_eq!((focus_of(run, 0.2, "a"), focus_of(run, 0.2, "b")), ((true, true), (false, false)));
        assert_eq!(*REOPENED.lock().unwrap(), [("a", true), ("b", true)]);
    }

    #[test]
    fn desktop_reopen_default_sees_what_the_hooks_did() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        // `a` closes, then `b`, so `b` would come back; but `b`'s hook opens `a` and leaves
        // the rest to the default, which then finds a window showing.
        let script = r#"[{"t": 0.1, "type": "tap", "x": 30, "y": 25}, {"t": 0.2, "type": "key", "name": "Enter"},
            {"t": 0.3, "type": "key", "name": "Enter"}, {"t": 0.4, "reopen": true}]"#;
        let run = reopen_case(script, false, || {
            let (a, handle) = closer_with("a", RED, None, record, left().macos(Default::default()));
            let (b, _) = closer_with("b", BLUE, Some(handle), open_other, right().macos(Default::default()));
            vec![a, b]
        });
        assert_eq!((focus_of(run, 0.3, "a"), focus_of(run, 0.3, "b")), ((false, false), (false, false)), "both closed");
        assert_eq!((focus_of(run, 0.4, "a"), focus_of(run, 0.4, "b")), ((true, true), (false, false)));
        assert_eq!(*REOPENED.lock().unwrap(), [("a", false), ("b", false)]);
    }

    #[test]
    fn desktop_reopen_hook_that_handles_it_skips_the_default() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        let script = r#"[{"t": 0.1, "type": "key", "name": "Enter"}, {"t": 0.2, "reopen": true}]"#;
        let run = reopen_case(script, false, || vec![closer("a", RED, true, left().macos(Default::default()))]);
        assert_eq!(focus_of(run, 0.2, "a"), (false, false));
        assert_eq!(*REOPENED.lock().unwrap(), [("a", false)]);
    }

    #[test]
    fn desktop_reopen_never_brings_back_a_launcher_or_an_accessory_app_window() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        // The launcher closes last, so a launcher-blind rule would pick it; `a` comes back.
        let launcher = || {
            let mut options = crate::WindowOptions::launcher_panel();
            let (position, physical_size) = (right().position, right().physical_size);
            options.options = crate::WindowOptions { visible: true, position, physical_size, ..options.options };
            closer("launcher", BLUE, false, options)
        };
        let script = r#"[{"t": 0.1, "type": "tap", "x": 30, "y": 25}, {"t": 0.2, "type": "key", "name": "Enter"},
            {"t": 0.3, "type": "tap", "x": 95, "y": 35}, {"t": 0.4, "type": "key", "name": "Enter"}, {"t": 0.5, "reopen": true}]"#;
        let run = reopen_case(script, false, move || vec![closer("a", RED, false, left().macos(Default::default())), launcher()]);
        assert_eq!((focus_of(run, 0.4, "a"), focus_of(run, 0.4, "launcher")), ((false, false), (false, false)), "both closed");
        assert_eq!((focus_of(run, 0.5, "a"), focus_of(run, 0.5, "launcher")), ((true, true), (false, false)));

        // A launcher alone stays hidden.
        let run = reopen_case(r#"[{"t": 0.1, "type": "tap", "x": 95, "y": 35}, {"t": 0.2, "type": "key", "name": "Enter"},
            {"t": 0.3, "reopen": true}]"#, false, move || vec![launcher()]);
        assert_eq!(focus_of(run, 0.3, "launcher"), (false, false));

        // An accessory app's hooks run, but nothing comes back.
        let script = r#"[{"t": 0.1, "type": "key", "name": "Enter"}, {"t": 0.2, "reopen": true}]"#;
        let run = reopen_case(script, true, || vec![closer("a", RED, false, left().macos(Default::default()))]);
        assert_eq!(focus_of(run, 0.2, "a"), (false, false));
        assert_eq!(*REOPENED.lock().unwrap(), [("a", false)]);
    }

    #[test]
    fn size_and_scale_must_be_positive() {
        assert_eq!(parse_size("400x300.5"), Ok((400.0, 300.5)));
        assert_eq!(parse_scale("1.5"), Ok(1.5));
        for bad in ["0x0", "400x0", "-1x10", "NaNx10", "infx10", "400", "ax3"] { assert!(parse_size(bad).is_err(), "{bad}"); }
        for bad in ["0", "-2", "NaN", "inf", "x"] { assert!(parse_scale(bad).is_err(), "{bad}"); }
    }

    #[test]
    fn keycaps_draw_over_frames_but_never_enter_the_tree() {
        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("rune-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("script.json");
        std::fs::write(&script, r#"[{"t": 0.1, "type": "type", "text": "hi"}, {"t": 0.3, "type": "tap", "x": 150, "y": 30}]"#).unwrap();
        let run = |name: &str, extra: &[&str]| {
            let spec = AppSpec::new("keys", (200.0, 120.0), || (), |_, _| crate::el().key("root").w_full().h_full().fill(crate::oklch(0.9, 0.0, 0.0)));
            let (snap, tree) = (dir.join(format!("{name}.png")), dir.join(format!("{name}.json")));
            let mut args = vec!["--script", script.to_str().unwrap(), "--scale", "1", "--at", "0.5",
                "--snap", snap.to_str().unwrap(), "--tree", tree.to_str().unwrap()];
            args.extend_from_slice(extra);
            headless(crate::App::new(spec), Args::parse_from(args.into_iter().map(String::from).collect()));
            (std::fs::read(snap).unwrap(), std::fs::read_to_string(tree).unwrap())
        };
        let plain = run("plain", &[]);
        let keys = run("keys", &["--keys", "top-left", "--pointer"]);
        let light = run("light", &["--keys", "top-left", "--pointer", "--keys-theme", "light"]);
        let dark = run("dark", &["--keys", "top-left", "--pointer", "--keys-theme", "dark"]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(plain.1, keys.1, "the overlay never enters the tree");
        assert_ne!(plain.0, keys.0, "but it draws over the frame");
        assert_eq!(keys.0, light.0, "auto caps follow the app's light background");
        assert_ne!(light.0, dark.0);

        use crate::keycaps::Corner;
        let args = |v: &[&str]| Args::parse_from(v.iter().map(|s| (*s).into()).collect());
        assert_eq!(args(&["--keys", "--snap", "a.png"]).keys, Some(Corner::BottomLeft), "the corner is optional");
        assert_eq!(args(&["--keys", "top"]).keys, Some(Corner::Top));
        assert_eq!(args(&["--snap", "a.png"]).keys, None);
    }

    #[test]
    fn script_chords_only_fire_global_handlers_even_when_the_window_is_key() {
        let script = parse_script(json!([{"t": 0.1, "type": "shortcut", "chord": "cmd+space"}, {"t": 0.2, "shortcut": "cmd+k"}]));
        assert!(matches!(&script[0].kind, Kind::Shortcut(c) if *c == "cmd+space".parse().unwrap()));
        assert!(matches!(&script[1].kind, Kind::Shortcut(c) if *c == "cmd+k".parse().unwrap()));

        let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
        static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let dir = std::env::temp_dir().join(format!("rune-chords-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("script.json");
        SEEN.lock().unwrap().clear();
        std::fs::write(&path, r#"[{"focus": true}, {"t": 0.1, "type": "shortcut", "chord": "cmd+k"},
            {"t": 0.2, "type": "shortcut", "chord": "cmd+space"}, {"t": 0.3, "shortcut": "cmd+k"},
            {"t": 0.4, "focus": false}, {"t": 0.5, "type": "shortcut", "chord": "cmd+space"},
            {"t": 0.6, "type": "key", "name": "Escape"}]"#).unwrap();
        let spec = AppSpec {
            on_key: Some(|_, ev, _| { SEEN.lock().unwrap().push(format!("on_key {:?}", ev.key)); true }),
            ..AppSpec::new("chords", (40.0, 30.0), || (), |_, _| crate::el())
        };
        let app = crate::App::new(spec)
            .global_shortcut("cmd+k", |_, _| SEEN.lock().unwrap().push("global cmd+k".into()))
            .global_shortcut("cmd+space", |_, _| SEEN.lock().unwrap().push("global cmd+space".into()));
        let tree = dir.join("tree.json");
        headless(app, Args::parse_from(["--script", path.to_str().unwrap(), "--tree", tree.to_str().unwrap()].map(String::from).into()));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(*SEEN.lock().unwrap(), ["global cmd+k", "global cmd+space", "global cmd+k", "global cmd+space", "on_key Escape"]);
    }

    #[test]
    fn native_trace_does_not_select_virtual_rendering() {
        let args = |v: &[&str]| Args::parse_from(v.iter().map(|s| (*s).into()).collect());
        assert!(args(&["--trace", "trace.jsonl", "--frames", "120"]).headless());
        assert!(!args(&["--native", "--trace", "trace.jsonl", "--frames", "120"]).headless());
        assert!(args(&["--native", "--snap", "snap.png"]).headless());
    }

    #[test]
    fn tree_and_trace_serialize_effects_and_remove_channels() {
        fn view(show: &bool, _: &mut Cx<bool>) -> El {
            crate::el().key("root").size(100.0, 100.0).children(show.then(||
                crate::el().key("pill").size(40.0, 40.0).opacity(0.5)
                    .live_scale(&crate::live::Live::new(0.8))
                    .transition(crate::Transition::blur(4.0))))
        }
        let mut rt = Runtime::new(true, view, (100.0, 100.0), Env { scale: 1.0, system_dark: true });
        rt.frame(0.0);
        let tree = tree_json(&rt, 0.0);
        let pill = tree["elements"].as_array().unwrap().iter().find(|e| e["key"] == "pill").unwrap();
        assert_eq!(pill["opacity"], 0.5);
        assert!((pill["scale"].as_f64().unwrap() - 0.8).abs() < 1e-6);
        assert_eq!(pill["blur"], 0.0);
        let path = std::env::temp_dir().join(format!("rune-effects-{}.jsonl", std::process::id()));
        {
            let mut trace = Trace::open(&path, rt.viewport, 60.0, 1.0);
            trace.frame(&mut rt, 0.0, 0.0);
            rt.state = false; rt.invalidate(); rt.frame(0.0); rt.frame(0.05);
            let tree = tree_json(&rt, 0.05);
            let pill = tree["elements"].as_array().unwrap().iter().find(|e| e["key"] == "pill").unwrap();
            assert_eq!(pill["exiting"], true);
            assert!(tree["elements"].as_array().unwrap().iter().filter(|e| e["key"] != "pill").all(|e| e.get("exiting").is_none()));
            trace.frame(&mut rt, 50.0, 0.0);
            rt.frame(3.0);
            trace.frame(&mut rt, 3050.0, 0.0);
        }
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        let lines: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        let blur = lines[1]["cm"].as_array().unwrap().iter()
            .find(|m| m["name"].as_str().unwrap().ends_with("pill.blur")).unwrap()["id"].as_u64().unwrap();
        let values = lines[2]["c"].as_array().unwrap();
        assert!(values.iter().any(|v| v[0] == blur && v[1].as_f64().unwrap() > 0.0));
        assert!(lines[3]["cg"].as_array().unwrap().contains(&json!(blur)));
        assert_eq!(lines[1]["e"][0].as_array().unwrap().len(), 8, "element row schema stays compatible");
    }

    #[test]
    fn trace_all_names_unkeyed_elements_and_reports_what_animated() {
        fn view(wide: &bool, _: &mut Cx<bool>) -> El {
            crate::el().key("root").size(100.0, 100.0)
                .children([crate::el().size(if *wide { 80.0 } else { 20.0 }, 20.0)])
        }
        assert!(Args::parse_from(["--trace-all"].map(String::from).into()).trace_all);
        let lines = |all: bool| {
            let mut rt = Runtime::new(false, view, (100.0, 100.0), Env { scale: 1.0, system_dark: true });
            rt.frame(0.0);
            let path = std::env::temp_dir().join(format!("rune-trace-all-{all}-{}.jsonl", std::process::id()));
            {
                let mut trace = Trace::open(&path, rt.viewport, 60.0, 1.0);
                trace.all = all;
                trace.frame(&mut rt, 0.0, 0.0);
                rt.state = true; rt.invalidate(); rt.frame(0.0); rt.frame(0.05);
                trace.frame(&mut rt, 50.0, 0.0);
            }
            let text = std::fs::read_to_string(&path).unwrap();
            std::fs::remove_file(path).unwrap();
            text.lines().map(|line| serde_json::from_str(line).unwrap()).collect::<Vec<Value>>()
        };
        let keyed = lines(false);
        assert_eq!(keyed[1]["m"].as_array().unwrap().len(), 1, "only the keyed root without --trace-all");
        let all = lines(true);
        let metas = all[1]["m"].as_array().unwrap();
        let root = metas.iter().find(|m| m["nm"] == "root").unwrap();
        let child = metas.iter().find(|m| m["nm"] != "root").expect("unkeyed child traced");
        assert_eq!(child["pv"], root["id"], "parent link to the root");
        assert_eq!(all[2]["quality"]["why"], json!(["tracks"]));
        let tk = all[2]["tk"].as_array().unwrap();
        assert!(tk.iter().any(|t| t[0] == child["id"] && t[1] == "frame" && t[2] == 1), "{tk:?}");
    }
}

/// The crops of one frame stacked vertically (the whole frame when there are none).
pub(crate) fn tile(px: &[u8], w: u32, h: u32, crops: &[[u32; 4]], gap: u32) -> Vec<u8> {
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

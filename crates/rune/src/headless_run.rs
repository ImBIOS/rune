//! What a headless run was asked to do, for the app to read (`tooling::headless_run`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::edit::KeyEvent;

/// The headless run this process is rendering: its script, timing and output. One per run,
/// shared by every window, so a multi-window run reads the same timeline from each.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct HeadlessRun {
    pub(crate) script: Option<PathBuf>,
    pub(crate) events: Vec<TimedEvent>,
    pub(crate) scale: f32,
    pub(crate) fps: f64,
    pub(crate) size: (f32, f32),
    pub(crate) start: f64,
    pub(crate) frames: usize,
    pub(crate) end: f64,
}

impl HeadlessRun {
    /// The `--script` file.
    pub fn script(&self) -> Option<&Path> { self.script.as_deref() }

    /// The script's events in time order, expanded as Rune feeds them: a `tap` is a move, a
    /// down and an up; `type` is one key per character at its `every` interval.
    pub fn events(&self) -> &[TimedEvent] { &self.events }

    /// The time of the script's last event or 0 without one.
    pub fn script_end(&self) -> f64 { self.events.last().map_or(0.0, |e| e.t) }

    /// Device pixels per point (`--scale`, default 2).
    pub fn scale(&self) -> f32 { self.scale }

    /// Frames per virtual second (`--fps`, default 60).
    pub fn fps(&self) -> f64 { self.fps }

    /// The rendered window's viewport in points (`--size`, or the window's own size).
    pub fn size(&self) -> (f32, f32) { self.size }

    /// Seconds of virtual time at the first dumped frame (`--start`).
    pub fn start(&self) -> f64 { self.start }

    /// Frames written to `--video` or `--out` (0 for a `--snap` or `--tree` run).
    pub fn frames(&self) -> usize { self.frames }

    /// Seconds of virtual time at the last frame the run renders.
    pub fn end(&self) -> f64 { self.end }
}

/// A script event at its time in seconds.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct TimedEvent {
    pub t: f64,
    pub event: ScriptEvent,
}

/// One input a script feeds the app.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum ScriptEvent {
    /// A logical key; `repeat` is a held key's auto-repeat.
    Key { key: KeyEvent, repeat: bool },
    Paste(String),
    /// A global shortcut chord.
    Shortcut(crate::Shortcut),
    /// A hardware key or modifier change (`{"physical": …}`).
    Physical(crate::PhysicalInput),
    Pointer { action: PointerAction, at: ScriptPoint },
    /// The window gaining (true) or losing focus.
    Focus(bool),
    /// A native menu item fired by name.
    Menu(String),
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerAction {
    Move,
    Down,
    Up,
    SecondaryDown,
    SecondaryUp,
    /// A wheel or trackpad delta in points.
    Scroll(f32),
    /// A trackpad release velocity in points per second.
    Fling(f32),
}

/// Where a pointer event lands, as the script wrote it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptPoint {
    /// Points from the window's top-left.
    At(f32, f32),
    /// The centre of the element with this key (or path suffix), found when the event runs.
    Element(String),
    /// An offset from the last pointer-down position (drags, the release of a tap).
    FromDown(f32, f32),
}

static RUN: AtomicPtr<HeadlessRun> = AtomicPtr::new(std::ptr::null_mut());

/// The headless run this process is rendering, or None in a window. Set before the app's
/// state is created, so `init`, `setup`, views and handlers can read it; a `run_windows`
/// factory runs too early to see it.
pub fn headless_run() -> Option<&'static HeadlessRun> {
    // SAFETY: only `publish` stores, and it stores leaked boxes that are never freed.
    unsafe { RUN.load(Ordering::Acquire).as_ref() }
}

/// Makes `run` the process's headless run. Each run leaks its small description so earlier
/// references stay valid; a process renders one run, or a few in tests.
pub(crate) fn publish(run: HeadlessRun) {
    RUN.store(Box::into_raw(Box::new(run)), Ordering::Release);
}

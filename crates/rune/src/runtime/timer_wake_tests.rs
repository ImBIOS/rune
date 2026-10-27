//! A shown window's due timer while it rests: `poll` runs alone, and a frame follows only
//! when that poll changed something or anything else wants frames.
use super::*;
use crate::{Cx, El, Env};
use std::time::Duration;

#[derive(Default)]
struct App {
    polls: u32,
    /// The poll that reports a change; none at 0.
    change_at: u32,
    /// Asks for the next frame (`wake_after(ZERO)`) instead of a quarter second.
    step: bool,
    width: f32,
}

fn view(s: &App, _: &mut Cx<App>) -> El {
    crate::col().children([crate::el().key("box").size(10.0 + s.width, 8.0)])
}

fn poll(s: &mut App, _: f64, tx: &mut Tx) -> bool {
    s.polls += 1;
    tx.wake_after(if s.step { Duration::ZERO } else { Duration::from_millis(250) });
    s.step || s.polls == s.change_at
}

fn runtime(app: App) -> Runtime<App> {
    let mut rt = Runtime::new(app, view, (200.0, 50.0), Env::default());
    rt.poll = Some(poll);
    rt.wall_start = Some(crate::time::Instant::now());
    rt.frame(0.0);
    rt.frame(crate::frame_clock::RESUME_STEP);
    assert!(!rt.animating);
    rt
}

/// As the window host: the wall clock reaches the next timer, a resting window polls first,
/// and it builds and presents a frame only when asked. Returns whether it drew.
fn timer_wake(rt: &mut Runtime<App>) -> bool {
    let due = rt.wake_deadline().expect("a timer is armed");
    let now = rt.wall_start.unwrap().elapsed().as_secs_f64();
    let ahead = (due - now).max(0.0) + 1e-4;
    rt.wall_start = rt.wall_start.map(|t| t - Duration::from_secs_f64(ahead));
    if rt.resting_wake() == Some(false) { return false; }
    rt.frame(crate::frame_clock::RESUME_STEP);
    true
}

#[test]
fn a_resting_window_s_timer_draws_only_for_a_poll_that_changed_something() {
    let mut rt = runtime(App::default());
    let (commits, polls) = (rt.stats.commits, rt.state.polls);
    rt.state.change_at = polls + 9;
    for _ in 0..8 { assert!(!timer_wake(&mut rt), "a poll that returns false builds and draws nothing"); }
    assert_eq!(rt.state.polls, polls + 8, "each timer still polls");
    assert_eq!(rt.stats.commits, commits, "and runs no view");
    let deadline = rt.wake_deadline().unwrap();
    assert!(deadline > rt.clock, "the poll's next timer is armed: {deadline} after {}", rt.clock);

    let frames = (0..6).filter(|_| timer_wake(&mut rt)).count();
    assert_eq!(frames, 1, "the poll that returns true gets exactly one frame");
    assert_eq!(rt.stats.commits, commits + 1);
    assert_eq!(rt.state.polls, polls + 14, "one poll a wake: the frame doesn't poll again");
}

#[test]
fn a_poll_stepping_every_frame_still_draws_every_frame() {
    let mut rt = runtime(App { step: true, ..App::default() });
    let polls = rt.state.polls;
    for _ in 0..10 { assert!(timer_wake(&mut rt), "`wake_after(ZERO)` steps on frames"); }
    assert_eq!(rt.state.polls - polls, 10, "the frames poll; the wakes don't poll first");
    // Steps reported as no change still take their frames, as before.
    rt.poll = Some(|s, _, tx| { s.polls += 1; tx.wake_after(Duration::ZERO); false });
    rt.frame(crate::frame_clock::RESUME_STEP);
    for _ in 0..5 { assert!(timer_wake(&mut rt)); }
    // A resting window's poll that starts stepping takes the next frame.
    let mut rt = runtime(App::default());
    assert!(!timer_wake(&mut rt));
    rt.poll = Some(|s, _, tx| { s.polls += 1; tx.wake_after(Duration::ZERO); false });
    let polls = rt.state.polls;
    assert!(timer_wake(&mut rt), "its first step draws");
    assert!(timer_wake(&mut rt), "and so does every one after");
    assert_eq!(rt.state.polls, polls + 2, "one step each");
}

#[test]
fn motion_in_flight_takes_the_timer_s_frame() {
    let mut rt = runtime(App::default());
    rt.update(|s, _| s.width = 120.0);
    rt.frame(crate::frame_clock::RESUME_STEP);
    assert!(rt.animating, "the box springs to its new width");
    let polls = rt.state.polls;
    assert_eq!(rt.resting_wake(), None);
    assert_eq!(rt.state.polls, polls, "the frame polls, not the wake");
}

use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::{AppSpec, Env, Key, KeyEvent, Mods, Runtime, TextEdit, TextStyle, text_input};

#[derive(Default)]
struct Real {
    count: u32,
    polls: u32,
    holds_escape: bool,
    field: TextEdit,
}

fn view(s: &Real, _: &mut Cx<Real>) -> El {
    col().w_full().h_full()
        .child(text(format!("count {}", s.count)).key("count"))
        .child(el().key("inc").size(40.0, 20.0).on_press(|s: &mut Real| s.count += 1))
        .child(text_input(&s.field, TextStyle::new(14.0, 400.0)).key("field").size(200.0, 24.0).on_edit(|s: &mut Real| &mut s.field))
}

static SCENES: &[(&str, fn(&mut Real))] = &[("big", |s| s.count = 100), ("holding", |s| s.holds_escape = true), ("Motion/Still", |_| {})];

#[derive(Default)]
struct Lab {
    ticks: u32,
    presses: u32,
}

fn lab(p: &Lab, _: &mut Cx<Lab>) -> El {
    col().w_full().h_full()
        .child(text(format!("ticks {} presses {}", p.ticks, p.presses)).key("lab"))
        .child(el().key("bump").size(40.0, 20.0).on_press(|p: &mut Lab| p.presses += 1))
}

/// A timer: one tick a second, by `wake_after` and the page's poll.
fn lab_poll(p: &mut Lab, t: f64, tx: &mut Tx) -> bool {
    tx.wake_after(Duration::from_secs(1));
    let due = t.floor() as u32;
    let changed = due != p.ticks;
    p.ticks = due;
    changed
}

fn activity(s: &Real, _: &mut Cx<Real>) -> El {
    text(format!("live {}", s.count)).key("activity")
}

/// A story whose replay runs timed steps, logging when each was due after the first.
#[derive(Default)]
struct Fade {
    t0: f64,
    log: Vec<(&'static str, f64)>,
}

fn fade(p: &Fade, _: &mut Cx<Fade>) -> El {
    text(format!("{:?}", p.log.iter().map(|(n, _)| *n).collect::<Vec<_>>())).key("fade")
}

fn fade_story() -> Story<Fade> {
    Story::new("Motion/Fade", Fade::default, fade)
        .trigger('r', "Fade in", |p, play| {
            p.t0 = play.now();
            p.log.push(("start", 0.0));
            play.after(0.5, |p, play| {
                p.log.push(("half", play.now() - p.t0));
                play.after(0.25, |p, play| p.log.push(("end", play.now() - p.t0)));
            });
        })
        .trigger('f', "Finish", |p, _| p.log.push(("finish", 0.0)))
}

fn runtime_in(open: Option<Option<String>>, windowed: bool) -> Runtime<Real> {
    let mut spec = AppSpec::new("playground", (800.0, 500.0), Real::default, view);
    spec.scenes = SCENES;
    spec.poll = Some(|s, _, _| { s.polls += 1; false });
    spec.on_key = Some(|s, ev, _| s.holds_escape && ev.key == Key::Escape);
    let playground = Playground::new()
        .enabled(true)
        .page("Live/Activity", activity)
        .story(Story::new("Lab", Lab::default, lab).poll(lab_poll))
        .story(fade_story())
        .trigger("big", 'd', "Double", |s, _| s.count *= 2);
    let mut rt = crate::tooling::runtime_for(&spec, (spec.init)(), spec.size, Env::new(false, 1.0), Arc::new(|| {}));
    attach(&mut rt, Some(playground), &spec, open, windowed);
    rt.frame(0.0);
    rt
}

fn runtime(open: Option<Option<String>>) -> Runtime<Real> {
    runtime_in(open, true)
}

fn press_with(rt: &mut Runtime<Real>, key: Key, mods: Mods) {
    rt.key(KeyEvent { key, mods });
    rt.frame(1.0 / 60.0);
}

fn press(rt: &mut Runtime<Real>, key: Key) {
    press_with(rt, key, Mods::default());
}

fn letter(rt: &mut Runtime<Real>, c: &str) {
    press(rt, Key::Text(c.into()));
}

fn selected(rt: &Runtime<Real>) -> usize {
    rt.playground.as_ref().map_or(usize::MAX, |p| p.selected)
}

fn focused(rt: &Runtime<Real>) -> Option<String> {
    rt.elements().into_iter().find(|e| e.focused).and_then(|e| e.key)
}

fn find(rt: &Runtime<Real>, key: &str) -> Option<crate::ElementInfo> {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key) && !e.exiting)
}

fn shows(rt: &Runtime<Real>, key: &str) -> String {
    find(rt, key).and_then(|e| e.text).unwrap_or_else(|| panic!("no {key}"))
}

fn tap(rt: &mut Runtime<Real>, key: &str) {
    let r = find(rt, key).unwrap_or_else(|| panic!("no {key}")).rect;
    let (x, y) = (r[0] + r[2] * 0.5, r[1] + r[3] * 0.5);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
}

fn escape(rt: &mut Runtime<Real>) {
    rt.key(KeyEvent { key: Key::Escape, mods: Mods::default() });
    rt.frame(1.0 / 60.0);
}

fn is_open(rt: &Runtime<Real>) -> bool {
    rt.playground.as_ref().is_some_and(|p| p.is_open())
}

#[test]
fn the_bar_shortens_the_app_and_opens_the_playground() {
    let mut rt = runtime(None);
    assert!(find(&rt, "playground-open").is_some());
    let bar = find(&rt, "playground-open").unwrap().rect;
    assert!(bar[1] >= 500.0 - BAR, "the bar sits along the bottom edge");
    tap(&mut rt, "inc");
    assert_eq!(rt.state.count, 1);
    tap(&mut rt, "playground-open");
    assert!(is_open(&rt));
    assert_eq!(shows(&rt, "count"), "count 100", "the first scene, from a fresh init");
}

#[test]
fn a_scene_gets_a_fresh_state_and_back_restores_the_real_one() {
    let mut rt = runtime(None);
    tap(&mut rt, "inc");
    rt.update(|_, tx| tx.open_playground());
    rt.frame(1.0 / 60.0);
    tap(&mut rt, "inc");
    assert_eq!(shows(&rt, "count"), "count 101");
    tap(&mut rt, "playground-trigger-d");
    rt.frame(1.0 / 60.0);
    assert_eq!(shows(&rt, "count"), "count 202", "triggers act on the stage");
    assert_eq!(rt.state.count, 1, "the real state is untouched");
    tap(&mut rt, "playground-back");
    assert!(!is_open(&rt));
    assert_eq!(shows(&rt, "count"), "count 1");
    assert!(find(&rt, "playground-trigger-d").is_none());
}

#[test]
fn the_stage_takes_escape_first() {
    let mut rt = runtime(Some(Some("holding".into())));
    assert!(is_open(&rt));
    escape(&mut rt);
    assert!(is_open(&rt), "the scene's on_key took Escape");
    tap(&mut rt, "playground-item-0");
    tap(&mut rt, "field");
    assert_eq!(rt.elements().into_iter().find(|e| e.focused).and_then(|e| e.key).as_deref(), Some("field"));
    escape(&mut rt);
    assert!(is_open(&rt), "the focused field took Escape");
    escape(&mut rt);
    assert!(!is_open(&rt), "unhandled, Escape goes back");
    assert!(!rt.state.holds_escape);
}

#[test]
fn a_page_keeps_its_own_state_and_timer_while_open() {
    let mut rt = runtime(Some(Some("lab".into())));
    tap(&mut rt, "bump");
    tap(&mut rt, "bump");
    for _ in 0..70 { rt.frame(1.0 / 60.0); }
    assert_eq!(shows(&rt, "lab"), "ticks 1 presses 2");
    tap(&mut rt, "playground-item-0");
    tap(&mut rt, "playground-item-4");
    assert_eq!(shows(&rt, "lab"), "ticks 1 presses 2", "kept across selections");
    tap(&mut rt, "playground-trigger-r");
    assert_eq!(rt.wake_deadline(), Some(rt.clock), "a new stage state asks for its first poll");
    rt.frame(1.0 / 60.0);
    assert_eq!(shows(&rt, "lab"), "ticks 1 presses 0", "restart makes it again, polled at once");
    tap(&mut rt, "bump");
    escape(&mut rt);
    rt.update(|_, tx| tx.open_playground());
    rt.frame(1.0 / 60.0);
    assert!(shows(&rt, "lab").ends_with("presses 0"), "closing drops it");
}

#[test]
fn a_live_page_draws_and_changes_the_real_state() {
    let mut rt = runtime(Some(Some("Activity".into())));
    assert_eq!(shows(&rt, "activity"), "live 0");
    rt.update(|s, _| s.count = 7);
    rt.frame(1.0 / 60.0);
    assert_eq!(shows(&rt, "activity"), "live 7");
}

#[test]
fn the_real_state_keeps_polling_while_open() {
    let mut rt = runtime(Some(None));
    let before = rt.state.polls;
    for _ in 0..5 { rt.frame(1.0 / 60.0); }
    assert!(rt.state.polls >= before + 5);
    let scene = rt.playground.as_ref().and_then(|p| p.scene.as_ref()).map(|s| s.state.polls);
    assert!(scene.is_some_and(|n| n >= 5), "the scene polls too");
}

#[test]
fn without_the_bar_the_app_lays_out_untouched() {
    let spec = AppSpec::new("playground", (800.0, 500.0), Real::default, view);
    let mut rt = crate::tooling::runtime_for(&spec, (spec.init)(), spec.size, Env::new(false, 1.0), Arc::new(|| {}));
    rt.frame(0.0);
    let plain: Vec<_> = rt.elements().into_iter().map(|e| (e.path, e.rect)).collect();
    let mut rt = crate::tooling::runtime_for(&spec, (spec.init)(), spec.size, Env::new(false, 1.0), Arc::new(|| {}));
    attach(&mut rt, Some(Playground::new().enabled(true).bar(false)), &spec, None, true);
    rt.frame(0.0);
    let barless: Vec<_> = rt.elements().into_iter().map(|e| (e.path, e.rect)).collect();
    assert_eq!(plain, barless);
}

#[test]
fn keys_walk_the_stories_in_their_groups_order() {
    let mut rt = runtime(Some(None));
    // Scenes: big, holding; Motion: Still (a scene), Fade; Live: Activity; Pages: Lab.
    for (i, group) in [(0, true), (1, false), (2, true), (5, false), (3, true), (4, true)] {
        assert_eq!(find(&rt, &format!("playground-group-{i}")).is_some(), group, "story {i}");
    }
    let walk = [(Key::Down, 1), (Key::Down, 2), (Key::Down, 5), (Key::Down, 3), (Key::Up, 5)];
    for (key, at) in walk {
        press(&mut rt, key);
        assert_eq!(selected(&rt), at);
    }
    letter(&mut rt, "j");
    assert_eq!(selected(&rt), 3);
    letter(&mut rt, "k");
    assert_eq!(selected(&rt), 5);
    let cmd = Mods { cmd: true, ..Mods::default() };
    press_with(&mut rt, Key::Down, cmd);
    assert_eq!(selected(&rt), 4, "⌘↓ goes to the last");
    press_with(&mut rt, Key::Up, cmd);
    assert_eq!(selected(&rt), 0, "⌘↑ to the first");
    press(&mut rt, Key::Up);
    assert_eq!(selected(&rt), 4, "and ↑ wraps");
}

#[test]
fn a_replay_runs_its_steps_on_the_logical_clock_the_same_every_time() {
    let mut logs = Vec::new();
    for (start, dt) in [(0.0, 1.0 / 60.0), (3.3, 1.0 / 24.0)] {
        let mut rt = runtime(Some(Some("fade".into())));
        rt.frame(start);
        letter(&mut rt, "f");
        rt.frame(dt);
        letter(&mut rt, "r");
        // A second R part way starts over rather than queueing another run.
        for _ in 0..4 { rt.frame(dt); }
        letter(&mut rt, "r");
        rt.frame(dt);
        assert_eq!(shows(&rt, "fade"), "[\"start\"]", "the replay made the state again");
        for _ in 0..(1.0 / dt) as usize { rt.frame(dt); }
        let log = rt.playground.as_ref().and_then(|p| p.own()).map(|(own, stage)| {
            own.state_ref(stage).downcast_ref::<Fade>().unwrap().log.iter().map(|(n, t)| (*n, (t * 1e6).round() as i64)).collect::<Vec<_>>()
        });
        logs.push(log.unwrap());
    }
    assert_eq!(logs[0], [("start", 0), ("half", 500_000), ("end", 750_000)]);
    assert_eq!(logs[0], logs[1], "whatever the frame rate or start time");
}

#[test]
fn right_goes_into_the_side_column_and_tab_comes_back() {
    let mut rt = runtime(Some(Some("Fade".into())));
    press(&mut rt, Key::Right);
    assert_eq!(focused(&rt).as_deref(), Some("playground-trigger-r"));
    press(&mut rt, Key::Down);
    assert_eq!(focused(&rt).as_deref(), Some("playground-trigger-f"));
    press(&mut rt, Key::Down);
    assert_eq!(focused(&rt).as_deref(), Some("playground-trigger-r"), "↑↓ wrap inside it");
    letter(&mut rt, "j");
    assert_eq!(selected(&rt), 5, "J doesn't leave the story from the side column");
    press(&mut rt, Key::Tab);
    assert_eq!(focused(&rt), None);
    press(&mut rt, Key::Down);
    assert_eq!(selected(&rt), 3);
    press(&mut rt, Key::Right);
    assert_eq!(focused(&rt), None, "a live page has no triggers to go into");
}

#[test]
fn the_shortcut_opens_and_closes_it_without_a_menu() {
    let mut rt = runtime_in(Some(None), false);
    let chord = Mods { cmd: true, shift: true, ctrl: cfg!(not(target_os = "macos")), ..Mods::default() };
    assert!(is_open(&rt));
    press_with(&mut rt, Key::Command('d'), chord);
    assert!(!is_open(&rt));
    press_with(&mut rt, Key::Command('d'), chord);
    assert!(is_open(&rt));
    let mut rt = runtime(Some(None));
    let menu = cfg!(target_os = "macos");
    press_with(&mut rt, Key::Command('d'), chord);
    assert_eq!(is_open(&rt), menu, "the View menu's item has it in a Mac window");
}

#[test]
fn leaving_a_story_part_way_drops_its_queued_steps() {
    let mut rt = runtime(Some(Some("Fade".into())));
    letter(&mut rt, "r");
    press(&mut rt, Key::Down);
    for _ in 0..60 { rt.frame(1.0 / 60.0); }
    press(&mut rt, Key::Up);
    for _ in 0..60 { rt.frame(1.0 / 60.0); }
    assert_eq!(shows(&rt, "fade"), "[\"start\"]", "no burst of overdue steps on return");
}

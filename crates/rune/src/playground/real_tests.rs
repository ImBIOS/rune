use super::*;

fn open(rt: &mut Runtime<Real>, name: &str) {
    let name = name.to_string();
    rt.update(move |_, tx| tx.playground.push(Request::Open(Some(name))));
    rt.frame(DT);
}

fn copy(real: &Real) -> Real {
    Real { count: real.count, ..Real::default() }
}

#[test]
fn fresh_scenes_start_from_the_real_state_on_each_choice_and_replay() {
    let mut rt = runtime_with(base().fresh(copy), None, true);
    rt.state.count = 7;
    open(&mut rt, "holding");
    assert_eq!(shows(&rt, "count"), "count 7", "from the real state, then the scene fn");
    open(&mut rt, "big");
    assert_eq!(shows(&rt, "count"), "count 100", "the scene fn still applies");
    rt.state.count = 9;
    open(&mut rt, "holding");
    assert_eq!(shows(&rt, "count"), "count 9");
    rt.state.count = 11;
    letter(&mut rt, "r");
    assert_eq!(shows(&rt, "count"), "count 11", "R starts from it again");
    assert_eq!(rt.state.count, 11, "and never writes to it");
}

#[test]
fn a_story_from_real_derives_its_state_when_it_shows_and_on_replay() {
    let story = Story::from_real("Lab/Derived", |r: &Real| Lab { ticks: r.count, presses: 0 }, lab);
    let mut rt = runtime_with(base().story(story), None, true);
    rt.state.count = 3;
    open(&mut rt, "Derived");
    assert_eq!(shows(&rt, "lab"), "ticks 3 presses 0");
    rt.state.count = 4;
    letter(&mut rt, "j");
    letter(&mut rt, "k");
    assert_eq!(shows(&rt, "lab"), "ticks 3 presses 0", "kept while the playground's open");
    letter(&mut rt, "r");
    assert_eq!(shows(&rt, "lab"), "ticks 4 presses 0", "derived again on R");
}

#[test]
#[should_panic(expected = "derives from another app state")]
fn a_story_from_another_apps_state_is_refused() {
    let _ = base().story(Story::from_real("Lab/Other", |l: &Lab| Lab { ticks: l.ticks, presses: 0 }, lab));
}

#[test]
fn keep_copies_the_stage_back_before_a_replay_a_switch_and_back() {
    let keep = base().fresh(copy).keep(|stage, real| real.count = stage.count);
    let mut rt = runtime_with(keep, None, true);
    rt.state.count = 5;
    open(&mut rt, "Still");
    tap(&mut rt, "inc");
    assert_eq!((shows(&rt, "count"), rt.state.count), ("count 6".into(), 5), "tuned on the stage only");
    letter(&mut rt, "r");
    assert_eq!((shows(&rt, "count"), rt.state.count), ("count 6".into(), 6), "kept, and the replay starts from it");
    tap(&mut rt, "inc");
    letter(&mut rt, "j");
    assert_eq!(rt.state.count, 7, "kept on a switch");
    letter(&mut rt, "k");
    assert_eq!(shows(&rt, "count"), "count 7", "which the next scene starts from");
    tap(&mut rt, "inc");
    escape(&mut rt);
    assert_eq!(rt.state.count, 8, "and on Back");
    assert_eq!(shows(&rt, "count"), "count 8");
}

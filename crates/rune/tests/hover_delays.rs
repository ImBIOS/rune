//! `hover_delays`: `on_hover_change` hears an enter only after the pointer rests inside, and a
//! leave only after it stays out; shorter passes report nothing, and a press cancels the wait.
use std::time::Duration;

use rune::{Cx, El, Env, Runtime, col, el};

fn view(_: &Vec<bool>, _: &mut Cx<Vec<bool>>) -> El {
    col().w_full().h_full().child(
        el().key("zone").w(16.0).h(100.0)
            .on_hover_change(|s: &mut Vec<bool>, inside, _| s.push(inside))
            .hover_delays(Duration::from_millis(40), Duration::from_millis(100)),
    )
}

fn app() -> Runtime<Vec<bool>> {
    let mut rt = Runtime::new(Vec::new(), view, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.pointer_move(100.0, 50.0);
    rt.frame(0.0);
    rt
}

#[test]
fn an_enter_waits_for_the_pointer_to_rest() {
    let mut rt = app();
    rt.pointer_move(8.0, 50.0);
    rt.frame(0.03);
    assert!(rt.state.is_empty(), "not yet");
    assert!(rt.wake_deadline().is_some(), "the wait asks for a frame");
    rt.frame(0.015);
    assert_eq!(rt.state, [true]);
    // Moving within it reports nothing more.
    rt.pointer_move(10.0, 60.0);
    rt.frame(0.1);
    assert_eq!(rt.state, [true]);
}

#[test]
fn a_pass_shorter_than_the_delay_reports_nothing() {
    let mut rt = app();
    rt.pointer_move(8.0, 50.0);
    rt.frame(0.02);
    rt.pointer_move(100.0, 50.0);
    rt.frame(0.1);
    assert!(rt.state.is_empty());
}

#[test]
fn a_leave_waits_out_its_grace_and_a_return_keeps_it() {
    let mut rt = app();
    rt.pointer_move(8.0, 50.0);
    rt.frame(0.05);
    assert_eq!(rt.state, [true]);
    rt.pointer_move(100.0, 50.0);
    rt.frame(0.05);
    rt.pointer_move(8.0, 50.0);
    rt.frame(0.2);
    assert_eq!(rt.state, [true], "back within the grace");
    rt.pointer_move(100.0, 50.0);
    rt.frame(0.09);
    assert_eq!(rt.state, [true]);
    rt.frame(0.02);
    assert_eq!(rt.state, [true, false]);
}

#[test]
fn a_press_cancels_the_wait_and_the_next_move_starts_it_again() {
    let mut rt = app();
    rt.pointer_move(8.0, 50.0);
    rt.frame(0.02);
    rt.pointer_down(8.0, 50.0);
    rt.frame(0.1);
    assert!(rt.state.is_empty());
    rt.pointer_up(8.0, 50.0);
    rt.pointer_move(9.0, 50.0);
    rt.frame(0.03);
    assert!(rt.state.is_empty());
    rt.frame(0.02);
    assert_eq!(rt.state, [true]);
}

//! `App::on_frame_encoded`: reports carry what the window said, and reporting decides nothing.
use std::cell::RefCell;
use std::time::Duration;

use super::*;
use crate::{App, AppSpec, WindowSpec};

type Log = Rc<RefCell<Vec<FrameEncoded>>>;

fn logging() -> (Log, OnFrameEncoded) {
    let log = Log::default();
    let sink = log.clone();
    (log, Box::new(move |f: &FrameEncoded| sink.borrow_mut().push(f.clone())))
}

#[test]
fn reports_number_each_frame_of_their_window() {
    let (log, hook) = logging();
    let mut reports = FrameReports::new(Some(hook), Some("main"));
    let target = Instant::now() + Duration::from_millis(8);
    let encoded = reports.stamp();
    assert!(encoded.is_some(), "a hook takes the encode time");
    reports.drawn(encoded, Some(target));
    reports.unchanged(Some(target));
    reports.skipped();
    let log = log.borrow();
    let seen: Vec<_> = log.iter().map(|f| (f.seq(), f.outcome(), f.window(), f.presents_at())).collect();
    assert_eq!(seen, vec![
        (0, FrameOutcome::Drawn, Some("main"), Some(target)),
        (1, FrameOutcome::Unchanged, Some("main"), Some(target)),
        (2, FrameOutcome::Skipped, Some("main"), None),
    ]);
    assert_eq!(log[0].at(), encoded.unwrap(), "a drawn frame reports when encoding finished");
    assert!(log[1].at() >= log[0].at() && log[2].at() >= log[1].at());
}

#[test]
fn without_a_hook_nothing_is_timed_or_counted() {
    let mut reports = FrameReports::new(None, Some("main"));
    assert_eq!(reports.stamp(), None);
    reports.drawn(None, None);
    reports.unchanged(None);
    reports.skipped();
    assert!(reports.0.is_none());
}

#[test]
fn a_window_spec_keeps_its_app_s_hook_until_it_sets_its_own() {
    fn spec() -> AppSpec<()> { AppSpec::new("test", (100.0, 100.0), || (), |_, _| crate::el()) }
    let calls = Rc::new(RefCell::new(Vec::new()));
    let (app_calls, window_calls) = (calls.clone(), calls.clone());
    let window = WindowSpec::from_app("main", App::new(spec()).on_frame_encoded(move |_| app_calls.borrow_mut().push("app")));
    let mut reports = FrameReports::new(window.app.on_frame_encoded, Some("main"));
    reports.skipped();
    let window = WindowSpec::from_app("main", App::new(spec()).on_frame_encoded(|_| unreachable!()))
        .on_frame_encoded(move |_| window_calls.borrow_mut().push("window"));
    let mut reports = FrameReports::new(window.app.on_frame_encoded, Some("main"));
    reports.skipped();
    assert!(WindowSpec::new("plain", spec()).app.on_frame_encoded.is_none());
    assert_eq!(*calls.borrow(), vec!["app", "window"]);
}

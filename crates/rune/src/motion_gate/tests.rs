use super::*;
use crate::{Cx, El, Env, Spring, col, el, oklch};

#[derive(Default)]
struct S {
    right: bool,
    shown: bool,
}

fn view(s: &S, _: &mut Cx<S>) -> El {
    let ink = oklch(0.6, 0.15, 25.0);
    let mover = el().key("mover").size(100.0, 40.0).fill(ink).motion(Spring::new(0.3, 0.0));
    let mover = if s.right { mover.ml(300.0) } else { mover };
    let mut page = col().w_full().h_full().child(mover);
    if s.shown {
        page = page.child(el().key("box").size(100.0, 40.0).fill(ink));
    }
    page
}

fn cell(change: fn(&mut S, &mut crate::Tx)) -> Recording {
    let mut r = Run::new(Runtime::new(S::default(), view, (600.0, 400.0), Env::new(true, 1.0)), 120.0);
    r.idle(0.5);
    r.step();
    r.act("change", |rt| rt.update(change));
    r.play(1.0);
    r.rec
}

fn kinds(rec: &Recording) -> Vec<&'static str> {
    check(rec).into_iter().map(|f| f.kind).collect()
}

#[test]
fn a_run_records_a_spring_as_clean_and_a_snap_as_a_jump() {
    let spring = cell(|s, _| s.right = true);
    assert_eq!(spring.frames, 122, "the settled start, the input's frame and a second");
    assert!(kinds(&spring).is_empty(), "{:?}", check(&spring));
    let snap = cell(|s, tx| {
        s.right = true;
        tx.instant();
    });
    assert_eq!(kinds(&snap), ["jump"], "{:?}", check(&snap));
}

#[test]
fn a_box_that_mounts_with_no_transition_pops_in_unless_allowed() {
    let rec = cell(|s, tx| {
        s.shown = true;
        tx.instant();
    });
    let findings = check(&rec);
    assert!(findings.iter().any(|f| f.kind == "pop-in" && f.element.contains("box")), "{findings:?}");
    let cells = [Cell { name: "show/wide".into(), frames: rec.frames, skipped: false, findings }];
    let open = report(&cells, &[], false, 0.0);
    assert!(open.failing > 0 && open.text.contains("pop-in") && open.text.contains("mounts at or above"));
    const ALLOW: &[Allow] = &[("pop-in", "box", "show/", "the box appears at once, by design")];
    let allowed = report(&cells, ALLOW, true, 0.0);
    assert_eq!((allowed.failing, allowed.allowed), (0, 1));
    assert!(allowed.text.contains("allowed") && allowed.text.contains("by design"));
    assert_eq!(report(&cells, &[("pop-in", "box", "narrow", "")], false, 0.0).failing, 1, "the cell must match too");
}

#[test]
fn run_keeps_the_filtered_cells_in_order_and_flags_one_that_recorded_nothing() {
    let jobs: Vec<(String, Job)> = vec![
        ("move/wide".into(), Box::new(|| cell(|s, _| s.right = true))),
        ("move/narrow".into(), Box::new(|| cell(|s, _| s.right = true))),
        ("empty/wide".into(), Box::new(|| Recording::new((10.0, 10.0), 120.0))),
        ("show/wide".into(), Box::new(|| panic!("filtered out"))),
    ];
    let cells = run(jobs, "m");
    let names: Vec<&str> = cells.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["move/wide", "move/narrow", "empty/wide"]);
    assert!(cells[2].skipped && !cells[0].skipped);
    assert_eq!(report(&cells, &[], false, 0.0).failing, 1);
}

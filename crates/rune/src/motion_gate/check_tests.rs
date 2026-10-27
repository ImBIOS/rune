use super::*;

fn rec(tracks: Vec<(&str, Vec<(usize, [f32; 4], f32)>)>, frames: usize) -> Recording {
    let mut r = Recording::new((1000.0, 1000.0), 120.0);
    r.frames = frames;
    for (path, fs) in tracks {
        let i = r.track(path, None);
        r.tracks[i].frames = fs.into_iter().map(|(k, rect, op)| (k, Snap { rect, vis: rect, op, scale: 1.0, flags: INK, phase: f32::NAN })).collect();
    }
    r
}

fn kinds(r: &Recording) -> Vec<&'static str> {
    check(r).into_iter().map(|f| f.kind).collect()
}

#[test]
fn a_spring_passes_and_a_snap_jumps() {
    let smooth: Vec<_> = (0..60).map(|k| { let p = 1.0 - (-(k as f32) / 8.0).exp(); (k, [0.0, 300.0 - 200.0 * p, 100.0, 40.0], 1.0) }).collect();
    assert!(kinds(&rec(vec![("a", smooth)], 60)).is_empty());
    let snap: Vec<_> = (0..60).map(|k| (k, [0.0, if k < 30 { 300.0 } else { 100.0 }, 100.0, 40.0], 1.0)).collect();
    assert_eq!(kinds(&rec(vec![("a", snap)], 60)), ["jump"]);
    // Gliding at 4 pt a frame, then lurching to 20.
    let mut y = 0.0;
    let lurch: Vec<_> = (0..60).map(|k| { if k > 0 && k < 40 { y += if k < 20 { 4.0 } else { 20.0 } } (k, [0.0, y, 100.0, 40.0], 1.0) }).collect();
    assert_eq!(kinds(&rec(vec![("a", lurch)], 60)), ["jerk"]);
}

#[test]
fn an_instant_toggle_is_reported_where_the_height_changed() {
    let group: Vec<_> = (0..60).map(|k| (k, [0.0, 100.0, 100.0, if k < 30 { 28.0 } else { 84.0 }], 1.0)).collect();
    let below: Vec<_> = (0..60).map(|k| (k, [0.0, if k < 30 { 128.0 } else { 184.0 }, 100.0, 40.0], 1.0)).collect();
    let mut r = rec(vec![("g", group), ("b", below)], 60);
    assert_eq!(kinds(&r), ["jump", "jump"], "with no input, the row below jumps on its own");
    r.inputs.push((30, "tap".into()));
    let f = check(&r);
    assert_eq!(f.iter().map(|f| (f.kind, f.element.as_str())).collect::<Vec<_>>(), [("jump", "g")]);
}

#[test]
fn pops_overshoot_reversal_and_flicker() {
    let pop: Vec<_> = (20..60).map(|k| (k, [0.0, 0.0, 100.0, 40.0], 1.0)).collect();
    assert_eq!(kinds(&rec(vec![("a", pop)], 60)), ["pop-in"]);
    let gone: Vec<_> = (0..20).map(|k| (k, [0.0, 0.0, 100.0, 40.0], 1.0)).collect();
    assert_eq!(kinds(&rec(vec![("a", gone)], 60)), ["pop-out"]);
    let over: Vec<_> = (0..60).map(|k| { let t = k as f32 / 8.0; (k, [0.0, 100.0 + 100.0 * (1.0 - (-t).exp() * (t * 1.5).cos()), 100.0, 40.0], 1.0) }).collect();
    let k = kinds(&rec(vec![("a", over)], 60));
    assert!(k.contains(&"overshoot"), "{k:?}");
    let dogleg: Vec<_> = (0..60).map(|k| { let t = k as f32 / 59.0; (k, [0.0, 100.0 - 40.0 * (t * 3.0).min(1.0) * (1.0 - t) * 4.0 + 200.0 * t * t, 100.0, 40.0], 1.0) }).collect();
    let k = kinds(&rec(vec![("a", dogleg)], 60));
    assert!(k.contains(&"reversal"), "{k:?}");
    let blink: Vec<_> = (0..60).map(|k| (k, [0.0, 0.0, 100.0, 40.0], if (20..24).contains(&k) { 0.3 } else { 1.0 })).collect();
    assert_eq!(kinds(&rec(vec![("a", blink.clone())], 60)), ["flicker"]);
    // A container whose contents are all hidden draws nothing as it dips.
    let mut r = rec(vec![("a", blink)], 60);
    for f in &mut r.tracks[0].frames { f.1.flags = 0; }
    assert!(kinds(&r).is_empty());
}

#[test]
fn words_that_swap_in_place_are_a_text_swap_but_a_ticking_count_is_not() {
    let still: Vec<(usize, [f32; 4], f32)> = (0..60).map(|k| (k, [0.0, 0.0, 100.0, 20.0], 1.0)).collect();
    let with = |words: &[(usize, &str)]| {
        let mut r = rec(vec![("a", still.clone())], 60);
        r.tracks[0].words = words.iter().map(|(k, w)| (*k, w.to_string())).collect();
        kinds(&r)
    };
    assert_eq!(with(&[(0, "Read backoff.ts"), (30, "Grep retryBudget")]), ["text-swap"]);
    assert!(with(&[(0, "9s"), (30, "10s")]).is_empty(), "a clock ticks");
    assert!(with(&[(0, "Looks"), (30, "Looks right")]).is_empty(), "streaming words grow");
}

#[test]
fn a_loop_that_restarts_jumps_and_a_finished_run_goes_quiet() {
    let still: Vec<(usize, [f32; 4], f32)> = (0..120).map(|k| (k, [0.0, 0.0, 100.0, 20.0], 1.0)).collect();
    let looping = |restart: Option<usize>| {
        let mut r = rec(vec![("a", still.clone())], 120);
        for (k, f) in r.tracks[0].frames.iter_mut() {
            let t = restart.filter(|&at| *k >= at).map_or(*k, |at| *k - at);
            f.phase = (t as f32 / 144.0).rem_euclid(1.0);
        }
        r
    };
    // Wrapping round from 1 to 0 is a usual step.
    assert!(kinds(&looping(None)).is_empty());
    assert_eq!(kinds(&looping(Some(60))), ["loop-jump"]);
    let mut r = rec(vec![("a", (0..300).map(|k| (k, [0.0, 0.0, 100.0, 20.0], 1.0)).collect())], 300);
    r.quiet_from = Some(100);
    r.awake = (0..300).map(|k| k < 180).collect();
    assert!(kinds(&r).is_empty(), "settling within a second is fine");
    r.awake[260] = true;
    assert_eq!(kinds(&r), ["wakes-after-end"]);
}

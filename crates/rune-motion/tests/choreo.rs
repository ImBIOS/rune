use rune_motion::prelude::*;
use rune_motion::{Choreography, PhaseAnimator, When, stagger};

#[test]
fn stagger_delays_ripple_from_the_origin() {
    let d: Vec<f32> = (0..5).map(|i| stagger(i, 5, 0.1, StaggerFrom::First)).collect();
    assert_eq!(d, vec![0.0, 0.1, 0.2, 0.3, 0.4]);
    let d: Vec<f32> = (0..5).map(|i| stagger(i, 5, 0.1, StaggerFrom::Last)).collect();
    assert_eq!(d, vec![0.4, 0.3, 0.2, 0.1, 0.0]);
    let d: Vec<f32> = (0..5).map(|i| stagger(i, 5, 0.1, StaggerFrom::Center)).collect();
    assert!((d[0] - 0.2).abs() < 1e-6 && d[2] == 0.0 && (d[4] - 0.2).abs() < 1e-6);
    let eased = Stagger::new(0.1).ease(Easing::EASE_IN);
    // Eased stagger keeps the ends and bunches the start.
    assert!(eased.delay(0, 5) == 0.0 && (eased.delay(4, 5) - 0.4).abs() < 1e-5);
    assert!(eased.delay(1, 5) < 0.1);
}

#[test]
fn sequence_starts_on_arrival_not_on_physical_rest() {
    let mut store = AnimationStore::new();
    let (a, b, c) = (NodeId(1), NodeId(2), NodeId(3));
    for n in [a, b, c] {
        store.set(n, prop::OPACITY, 0.0);
    }
    let mut ch = Choreography::new();
    let s1 = ch.step(Cue::now(), |s| {
        s.animate(a, prop::OPACITY, 1.0, motion::SETTLE);
    });
    let s2 = ch.step(Cue::arrived(s1).after(0.05), |s| {
        s.animate(b, prop::OPACITY, 1.0, motion::SETTLE);
    });
    let s3 = ch.step(Cue::settled(s2).not_before(1.5), |s| {
        s.animate(c, prop::OPACITY, 1.0, motion::SNAP);
    });
    let mut t = 0.0;
    while t < 4.0 {
        store.set_time(t);
        ch.tick(&mut store);
        store.tick();
        t += 1.0 / 120.0;
    }
    // Step 2 is scheduled exactly at s1's logical arrival (its 0.3 s duration) + 0.05.
    assert!((ch.start_time(s2).unwrap() - 0.35).abs() < 1e-6, "{:?}", ch.start_time(s2));
    // Step 3 waits for physical rest *and* the guard.
    assert!(ch.start_time(s3).unwrap() >= 1.5);
    assert!(ch.is_done(&store));
    for n in [a, b, c] {
        assert_eq!(store.get(n, prop::OPACITY), Some(1.0));
    }
}

#[test]
fn choreography_stagger_cascades_items() {
    let mut store = AnimationStore::new();
    let ids: Vec<NodeId> = (0..4).map(NodeId).collect();
    for &n in &ids {
        store.set(n, prop::OFFSET, Vec2::new(0.0, 12.0));
    }
    let mut ch = Choreography::new();
    let steps = ch.stagger(Cue::now(), Stagger::new(0.04), ids.clone(), |s, n| {
        s.animate(n, prop::OFFSET, Vec2::ZERO, motion::SETTLE);
    });
    store.set_time(0.0);
    ch.tick(&mut store);
    for (i, s) in steps.iter().enumerate() {
        assert!((ch.start_time(*s).unwrap() - 0.04 * i as f64).abs() < 1e-6);
    }
    store.tick_at(0.05);
    let y: Vec<f32> = ids.iter().map(|&n| store.get(n, prop::OFFSET).unwrap().y).collect();
    assert!(y[0] < y[1] && y[1] < y[2] && y[2] == 12.0 && y[3] == 12.0, "{y:?}");
}

#[test]
fn phase_animator_cycles_and_holds_under_reduced_motion() {
    let pulse = |store: &mut AnimationStore, reduced: bool| {
        store.set_reduced_motion(reduced);
        let n = NodeId(1);
        let mut ph = PhaseAnimator::new()
            .phase(|p| {
                p.animate(n, prop::SCALE, 1.0, motion::SETTLE);
            })
            .phase(|p| {
                p.animate(n, prop::SCALE, 1.04, motion::SETTLE);
            })
            .advance_on(When::Arrived);
        let mut phases = Vec::new();
        let mut t = 0.0;
        while t < 2.0 {
            store.set_time(t);
            ph.tick(store);
            store.tick();
            phases.push(ph.current());
            t += 1.0 / 120.0;
        }
        phases
    };
    let phases = pulse(&mut AnimationStore::new(), false);
    let switches = phases.windows(2).filter(|w| w[0] != w[1]).count();
    // 0.3 s per phase for 2 s ≈ 6 switches.
    assert!((5..=7).contains(&switches), "{switches}");
    let held = pulse(&mut AnimationStore::new(), true);
    assert!(held.iter().all(|&p| p == 0));
}

use proptest::prelude::*;
use rune_ref::{Frame, NO_CLIP, Scene, generate};

fn render(s: &Scene) -> Frame {
    s.validate().unwrap();
    let mut f = Frame::new(s.width, s.height, true);
    f.render(s);
    f
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128,
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct("tests/reference.proptest-regressions"))),
        ..ProptestConfig::default() })]
    #[test]
    fn metamorphic(records in prop::collection::vec(any::<[u8;32]>(), 0..8)) {
        let scene = generate::scene(&records);
        let original = render(&scene);
        let mut shifted = scene.clone();
        shifted.width += 4; shifted.height += 6;
        generate::translate(&mut shifted, 4.0, 6.0);
        let actual = render(&shifted);
        for y in 0..scene.height as usize {
            let a = &original.rgba[y * scene.width as usize * 4..(y+1) * scene.width as usize * 4];
            let start = ((y + 6) * shifted.width as usize + 4) * 4;
            prop_assert_eq!(a, &actual.rgba[start..start+a.len()]);
        }
        // Transparent primitives preserve both bytes and provenance.
        let mut transparent = scene.clone();
        for mut p in scene.primitives.clone() {
            p.fill = [0.0;4]; p.border = [0.0;4];
            if p.params[1] == 4.0 { p.uv = [0.0;4]; }
            transparent.primitives.push(p);
        }
        let actual = render(&transparent);
        prop_assert_eq!(&original.rgba, &actual.rgba);
        prop_assert_eq!(&original.owners, &actual.owners);

        // Full viewport clip equals no clip at all pixel centers, including edges.
        let mut full = scene.clone();
        for p in &mut full.primitives { p.clip = NO_CLIP; p.fade = [0.0;4]; }
        let unclipped = render(&full);
        for p in &mut full.primitives { p.clip = [0.0,0.0,64.0,64.0]; }
        prop_assert_eq!(unclipped.rgba, render(&full).rgba);

        // Two independently generated groups separated by hard clips cannot interact.
        let mut disjoint = scene.clone();
        for p in &mut disjoint.primitives { p.clip = [0.0,0.0,31.0,64.0]; p.fade = [0.0;4]; }
        let left_count = disjoint.primitives.len();
        for mut p in scene.primitives.clone() {
            p.rect[0] += 32.0;
            p.clip = [32.0,0.0,64.0,64.0]; p.fade = [0.0;4];
            disjoint.primitives.push(p);
        }
        let a = render(&disjoint);
        disjoint.primitives.rotate_left(left_count);
        prop_assert_eq!(a.rgba, render(&disjoint).rgba);
    }
}

#[test]
fn provenance_and_diff_name_the_source() {
    let mut s = generate::scene(&[[0; 32]]);
    let p = &mut s.primitives[0];
    p.rect = [2.0, 2.0, 10.0, 10.0];
    p.fill = [1.0; 4];
    let f = render(&s);
    assert_eq!(f.blame(&s, 5, 5).unwrap().source, "generated/0");
    assert!(f.blame(&s, 63, 63).is_none());
    let mut bad = f.rgba.clone();
    bad[(5 * 64 + 5) * 4] = 0;
    let diff = rune_ref::compare(&s, &f, &bad, 2);
    assert_eq!(diff.outlier_pixels, 1);
    assert_eq!(diff.first.unwrap().node, Some(1));
}

#[test]
fn canonical_hashes() {
    let expected: Vec<(u64, String)> =
        serde_json::from_str(include_str!("../goldens.json")).unwrap();
    assert!(!expected.is_empty(), "canonical hashes must be recorded");
    for (seed, hash) in expected {
        assert_eq!(
            render(&generate::seeded(seed, 8)).hash(),
            hash,
            "seed {seed}"
        );
    }
}

#[test]
fn dashed_rings_translate_and_stay_inside_their_ring() {
    for seed in 0..32 {
        let scene = generate::dashed(seed, 8);
        let original = render(&scene);
        let mut shifted = scene.clone();
        shifted.width += 4; shifted.height += 6;
        generate::translate(&mut shifted, 4.0, 6.0);
        let actual = render(&shifted);
        for y in 0..64 {
            assert_eq!(&original.rgba[y * 256..(y + 1) * 256], &actual.rgba[((y + 6) * 68 + 4) * 4..((y + 6) * 68 + 68) * 4]);
        }
        // Dashes only ever take coverage away from the solid ring.
        for p in &scene.primitives {
            let mut one = scene.clone();
            one.primitives = vec![p.clone()];
            let dashed = render(&one);
            one.primitives[0].params[1] = 4.0;
            one.primitives[0].uv = one.primitives[0].border;
            one.primitives[0].border = one.primitives[0].fill;
            one.primitives[0].fill = [0.0; 4];
            let solid = render(&one);
            for (n, (d, s)) in dashed.owners.as_ref().unwrap().iter().zip(solid.owners.as_ref().unwrap()).enumerate() {
                assert!(d.is_none() || s.is_some(), "seed {seed}: pixel {n} dashed outside the ring");
            }
        }
    }
}

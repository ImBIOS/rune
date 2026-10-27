use super::*;

const A: Knob = Knob::new("A", 0.0, 10.0, 5.0);
const B: Knob = Knob::new("B", -1.0, 1.0, 0.0).step(0.5);
const C: Knob = Knob::choice("C", &["x", "y", "z"], 0);
const G: Group = Group::new("G", "g", &[A, B, C]).presets(&[Preset { name: "Wide", values: &[("A", 9.0)] }]);

#[test]
fn random_stays_in_range_and_on_steps() {
    let mut k = Knobs::default();
    for _ in 0..200 {
        k.random(&G);
        for d in G.knobs {
            let v = k.values[d.name];
            assert!((d.min..=d.max).contains(&v), "{} = {v}", d.name);
        }
        assert_eq!((k.values["B"] * 2.0).fract(), 0.0);
        assert_eq!(k.values["C"].fract(), 0.0);
    }
    let before = k.values.clone();
    k.random(&G);
    k.undo();
    assert_eq!(k.values, before);
}

#[test]
fn presets_reset_and_undo() {
    let mut k = Knobs::default();
    k.apply(&G, "Wide");
    assert_eq!(k.values.get("A"), Some(&9.0));
    assert_eq!(k.active(&G), "Wide");
    k.set(&A, 2.0);
    assert!(k.drifted(&G, "Wide"));
    k.reset(&G);
    assert_eq!(k.get(&A), 9.0, "reset goes back to the active preset first");
    k.reset(&G);
    assert!(k.values.is_empty());
    assert_eq!(k.active(&G), "Default");
    k.undo();
    assert_eq!(k.get(&A), 9.0);
    k.set(&A, 3.0);
    let name = k.save(&G);
    k.apply(&G, DEFAULT);
    k.apply(&G, &name);
    assert_eq!(k.get(&A), 3.0);
    assert_eq!(k.code(&G), "A = 3\nB = 0\nC = 0 (x)");
}

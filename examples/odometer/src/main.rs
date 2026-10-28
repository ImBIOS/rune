//! A count and a right-aligned run time as `RollingText` odometers. "Up" and "Down" step the
//! number: changed digits roll up as it rises and down as it falls, and "9s" → "10s" grows the
//! time to the left with every digit in its column. `cargo run -p odometer`; the motion audit
//! records it with `scripts/motion-audit/scenarios/odometer.json`.

use rune::{AppSpec, Cx, El, TextStyle, col, row};
use rune_kit::{Button, Kit, RollingText};

struct State {
    n: i64,
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let k = Kit::neutral(true);
    let big = TextStyle::new(28.0, 600.0).mono();
    let count = RollingText::new("count", s.n.to_string()).value(s.n as f64).font(big).build(&k);
    let time = RollingText::new("secs", format!("{}s", s.n)).value(s.n as f64).font(k.ty.mono).trailing().build(&k);
    col()
        .w_full()
        .h_full()
        .padding(24.0)
        .gap(16.0)
        .fill(k.c.bg_base)
        .child(row().gap(8.0).child(Button::new("Up").on_press(|s: &mut State| s.n += 1).build(&k).key("up")).child(Button::new("Down").on_press(|s: &mut State| s.n -= 1).build(&k).key("down")))
        .child(count)
        .child(row().w(160.0).justify_end().motion(RollingText::SPRING).child(time))
}

fn main() {
    rune::run(AppSpec::new("Odometer", (320.0, 200.0), || State { n: 9 }, view));
}

//! A card tuned live with `rune-knobs`: ⌘⇧K slides the panel in beside the page, and its sliders
//! and choices change the card as you drag. `cargo run -p knobs`; `-- --snap knobs.png --scene
//! open` renders it headless with the panel open.

use rune::tok::{self, type_};
use rune::{AppSpec, Cx, El, KeyEvent, Tx, col, text};
use rune_knobs::{Group, Knob, Knobs, Preset, Style};

const RADIUS: Knob = Knob::new("Card radius", 0.0, 32.0, 14.0).about("How round the card's corners are");
const PAD: Knob = Knob::new("Card padding", 8.0, 48.0, 24.0).step(1.0);
const TINT: Knob = Knob::choice("Card tint", &["Plain", "Accent", "Raised"], 0);
const GROUPS: &[Group] = &[Group::new("Card", "card", &[RADIUS, PAD, TINT]).presets(&[Preset { name: "Round", values: &[("Card radius", 28.0)] }])];

#[derive(Default)]
struct State {
    knobs: Knobs,
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let t = tok::theme(true);
    let k = &s.knobs;
    let fill = match k.get(&TINT) as usize {
        1 => tok::accent(0, true),
        2 => t.control_off,
        _ => t.knob,
    };
    let card = col()
        .key("card")
        .w(240.0)
        .padding(k.get(&PAD))
        .gap(6.0)
        .radius(k.get(&RADIUS))
        .fill(fill)
        .child(text("Tune me").font(type_::TITLE).color(t.bg))
        .child(text("⌘⇧K opens the knobs").color(t.bg));
    let page = col().key("page").grow(1.0).h_full().center().fill(t.bg).child(card);
    rune_knobs::side(page, k, GROUPS, Style::dark(), |s: &mut State| &mut s.knobs)
}

fn on_key(s: &mut State, ev: &KeyEvent, tx: &mut Tx) -> bool {
    let taken = rune_knobs::key(&mut s.knobs, ev);
    if taken {
        tx.motion(s.knobs.slide());
    }
    taken
}

fn main() {
    rune::run(AppSpec::<State> {
        scenes: &[("open", |s| s.knobs.show())],
        on_key: Some(on_key),
        ..AppSpec::new("Knobs", (820.0, 520.0), State::default, view)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Key, Mods, Runtime};

    #[test]
    fn cmd_shift_k_opens_the_panel_and_a_knob_changes_the_card() {
        let mut rt = Runtime::new(State::default(), view, (820.0, 520.0), Env::new(true, 1.0));
        rt.on_key = Some(on_key);
        rt.frame(0.0);
        let ev = KeyEvent { key: Key::Command('k'), mods: Mods { cmd: true, shift: true, ..Mods::default() } };
        rt.key(ev);
        for _ in 0..40 {
            rt.frame(1.0 / 60.0);
        }
        assert!(rt.state.knobs.open && rt.find("knobs-top").is_some());
        rt.state.knobs.set(&RADIUS, 30.0);
        rt.invalidate();
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.knobs.get(&RADIUS), 30.0);
    }
}

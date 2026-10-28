//! A card tuned live with `rune-knobs`: ⌘⇧K slides the panel in beside the page, and its sliders
//! and choices change the card as you drag. `cargo run -p knobs`; `-- --snap knobs.png --scene
//! open` renders it headless with the panel open, and `-- --playground "Knobs"` opens the
//! playground, whose side column hosts the same knobs as rows.

use rune::tok::{self, type_};
use rune::{App, AppSpec, Cx, El, KeyEvent, Playground, Story, Tx, col, text};
use rune_knobs::{Group, Knob, Knobs, Preset, Style};

const RADIUS: Knob = Knob::new("Card radius", 0.0, 32.0, 14.0).about("How round the card's corners are");
const PAD: Knob = Knob::new("Card padding", 8.0, 48.0, 24.0).step(1.0);
const TINT: Knob = Knob::choice("Card tint", &["Plain", "Accent", "Raised"], 0);
const SHADOW: Knob = Knob::choice("Card shadow", &["Off", "On"], 0);
const GROUPS: &[Group] = &[Group::new("Card", "card", &[RADIUS, PAD, TINT, SHADOW]).presets(&[Preset { name: "Round", values: &[("Card radius", 28.0)] }])];

#[derive(Default)]
struct State {
    knobs: Knobs,
}

fn page(s: &State, cx: &mut Cx<State>) -> El {
    let t = tok::theme(cx.system_dark());
    let ink = if cx.system_dark() { t.bg } else { t.text };
    let k = &s.knobs;
    let fill = match k.get(&TINT) as usize {
        1 => tok::accent(0, true),
        2 => t.control_off,
        _ => t.knob,
    };
    let card = col()
        .key("card")
        .shadow(if k.get(&SHADOW) > 0.5 { 10.0 } else { 0.0 }, 24.0, t.bg.alpha(0.35))
        .w(240.0)
        .padding(k.get(&PAD))
        .gap(6.0)
        .radius(k.get(&RADIUS))
        .fill(fill)
        .child(text("Tune me").font(type_::TITLE).color(ink))
        .child(text("⌘⇧K opens the knobs").color(ink));
    col().key("page").grow(1.0).h_full().center().fill(t.bg).child(card)
}

fn view(s: &State, cx: &mut Cx<State>) -> El {
    let page = page(s, cx);
    rune_knobs::side(page, &s.knobs, GROUPS, style(cx), |s: &mut State| &mut s.knobs)
}

fn style(cx: &Cx<State>) -> Style {
    if cx.system_dark() { Style::dark() } else { Style::light() }
}

/// The playground's side column: the same knobs as rows, filling its width.
fn story_side(s: &State, cx: &mut Cx<State>) -> El {
    rune_knobs::inline(&s.knobs, GROUPS, style(cx), |s: &mut State| &mut s.knobs, false)
}

fn on_key(s: &mut State, ev: &KeyEvent, tx: &mut Tx) -> bool {
    let taken = rune_knobs::key(&mut s.knobs, ev);
    if taken {
        tx.motion(s.knobs.slide());
    }
    taken
}

fn main() {
    let spec = AppSpec::<State> {
        scenes: &[("open", |s| s.knobs.show())],
        on_key: Some(on_key),
        ..AppSpec::new("Knobs", (820.0, 520.0), State::default, view)
    };
    App::new(spec).playground(Playground::<State>::new().story(Story::new("Card/Knobs", State::default, page).side(story_side))).run();
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

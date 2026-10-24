#![allow(deprecated)]

use rune::{Cx, El, Env, Runtime, col};
use rune_kit::*;

const SIZES: [ControlSize; 5] =
    [ControlSize::Mini, ControlSize::Small, ControlSize::Regular, ControlSize::Large, ControlSize::ExtraLarge];

fn view(_: &(), cx: &mut Cx<()>) -> El {
    let k = Kit::neutral(cx.system_dark());
    let mut c = col().items_start();
    for (i, s) in SIZES.into_iter().enumerate() {
        c = c.child(Button::new("Button").control_size(s).build(&k).key(format!("button-{i}")));
        c = c.child(IconButton::new(k.glyph.copy).control_size(s).build(&k).key(format!("icon-{i}")));
    }
    c.child(Button::new("Button").build(&k).key("default"))
        .child(IconButton::new(k.glyph.copy).build(&k).key("icon-default"))
        .children([ButtonSize::Small, ButtonSize::Normal, ButtonSize::Large].into_iter().enumerate().map(|(i, s)| {
            col().child(Button::new("Button").size(s).build(&k).key(format!("legacy-{i}")))
                .child(IconButton::new(k.glyph.copy).size(s).build(&k).key(format!("legacy-icon-{i}")))
        }))
}

#[test]
fn control_sizes_match_appkit_push_buttons_and_old_sizes_keep_their_metrics() {
    let mut rt = Runtime::new((), view, (400.0, 900.0), Env { system_dark: false, scale: 2.0 });
    rt.frame(0.0);
    let els = rt.elements();
    let size = |key: &str| {
        let e = els.iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{key}"));
        (e.rect[2], e.rect[3])
    };
    for (i, h) in [16.0, 20.0, 24.0, 28.0, 36.0].into_iter().enumerate() {
        assert_eq!(size(&format!("button-{i}")).1, h, "push button {i}");
        assert_eq!(size(&format!("icon-{i}")), (h, h), "icon button {i}");
    }
    assert_eq!(size("default").1, 24.0, "a button is Regular unless sized");
    assert_eq!(size("icon-default"), (24.0, 24.0));
    for (i, (h, side)) in [(24.0, 20.0), (28.0, 24.0), (32.0, 28.0)].into_iter().enumerate() {
        assert_eq!(size(&format!("legacy-{i}")).1, h, "ButtonSize keeps its height");
        assert_eq!(size(&format!("legacy-icon-{i}")), (side, side));
    }
    // Wider padding as the size grows, as AppKit's: 8, 10, 12, 14 and 18 points a side.
    let widths: Vec<f32> = (0..5).map(|i| size(&format!("button-{i}")).0).collect();
    assert!(widths.windows(2).all(|w| w[0] < w[1]), "{widths:?}");
}

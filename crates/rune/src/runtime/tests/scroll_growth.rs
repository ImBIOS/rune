use super::*;


#[test]
fn growth_rows_grow_and_sticky_scroll_lands_at_bottom() {
    use crate::el::{GrowthSpec, ScrollSpec};
    fn view(n: &usize, _cx: &mut Cx<usize>) -> El {
        let mut stack = col().key("stack").growth(GrowthSpec::default());
        for i in 0..*n {
            stack = stack.child(el().key(format!("r{i}")).h(100.0).w_full());
        }
        col().key("root").child(col().key("scroll").h(300.0).w_full().scroll(ScrollSpec { stick_bottom: true }).child(stack))
    }
    let mut rt = Runtime::new(10usize, view, (200.0, 300.0), Env::new(true, 2.0));
    rt.frame(0.0);
    // First frame: at the bottom, nothing animating (guarantee 8).
    assert!(!rt.animating);
    let last = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r9")).unwrap();
    assert!((last.rect[1] + last.rect[3] - 300.0).abs() < 0.05, "{:?}", last.rect);
    // A new row grows from 0 and the end stays pinned.
    rt.state = 11;
    rt.dirty = true;
    rt.frame(1.0 / 60.0);
    assert!(rt.animating);
    let r10 = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r10")).unwrap();
    assert!(r10.rect[3] < 100.0 && r10.rect[3] > 0.0);
    for _ in 0..120 {
        rt.frame(1.0 / 60.0);
    }
    assert!(!rt.animating);
    let r10 = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r10")).unwrap();
    assert!((r10.rect[1] + r10.rect[3] - 300.0).abs() < 0.05, "{:?}", r10.rect);
}

#[test]
fn animate_layout_springs_a_growth_row_s_contents() {
    use crate::el::GrowthSpec;
    fn view(s: &(f32, bool), _cx: &mut Cx<(f32, bool)>) -> El {
        let mut trigger = crate::row().key("trigger").h(28.0)
            .child(el().key("label").size(s.0, 16.0).width_leads())
            .child(el().key("chevron").size(14.0, 14.0));
        if s.1 { trigger = trigger.animate_layout(); }
        col().key("stack").w_full().growth(GrowthSpec::default())
            .child(col().key("r0").w_full().child(trigger))
    }
    let chevron_x = |rt: &Runtime<(f32, bool)>| rt.elements().into_iter()
        .find(|e| e.key.as_deref() == Some("chevron")).unwrap().rect[0];
    let label_w = |rt: &Runtime<(f32, bool)>| rt.elements().into_iter()
        .find(|e| e.key.as_deref() == Some("label")).unwrap().rect[2];
    for animate in [false, true] {
        let mut rt = Runtime::new((53.0, animate), view, (300.0, 100.0), Env::new(true, 2.0));
        rt.frame(0.0);
        assert_eq!(chevron_x(&rt), 53.0);
        rt.update(|s, _| s.0 = 88.0);
        rt.frame(1.0 / 120.0);
        let (x, w) = (chevron_x(&rt), label_w(&rt));
        if animate {
            assert!(x > 53.0 && x < 60.0, "springs from where it was: {x}");
            assert!(w > x, "width_leads still leads inside a growth row: {w} vs {x}");
            for _ in 0..120 { rt.frame(1.0 / 120.0); }
            assert_eq!(chevron_x(&rt), 88.0);
        } else {
            assert_eq!(x, 88.0, "a growth row's contents snap by default");
        }
    }
}

#[test]
fn animate_layout_in_a_growth_row_leaves_vertical_to_growth() {
    use crate::el::GrowthSpec;
    // (label width, space above the header, a second row)
    fn view(s: &(f32, f32, bool), _cx: &mut Cx<(f32, f32, bool)>) -> El {
        let head = |k: &str, w: f32| crate::row().key(k).h(28.0).animate_layout()
            .child(el().key(format!("{k}-names")).size(w, 16.0))
            .child(el().key(format!("{k}-chevron")).size(14.0, 14.0));
        let mut stack = col().key("stack").w_full().growth(GrowthSpec::default())
            .child(col().key("r0").w_full().child(el().key("above").w_full().h(s.1)).child(head("h0", s.0)));
        if s.2 { stack = stack.child(col().key("r1").w_full().child(head("h1", 40.0))); }
        stack
    }
    let find = |rt: &Runtime<(f32, f32, bool)>, key: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap();
    let mut rt = Runtime::new((31.5, 0.0, false), view, (300.0, 200.0), Env::new(true, 2.0));
    rt.frame(0.0);
    rt.update(|s, _| *s = (92.0, 20.0, true));
    rt.frame(1.0 / 120.0);
    let (chevron, names) = (find(&rt, "h0-chevron").rect, find(&rt, "h0-names").rect);
    assert!(chevron[0] > 31.5 && chevron[0] < 40.0, "x springs: {chevron:?}");
    assert!(names[2] > 31.5 && names[2] < 40.0, "width springs: {names:?}");
    assert_eq!(chevron[1], 20.0, "y lands at once; the row's growth carries it");
    assert_eq!(find(&rt, "h1-names").opacity, 1.0, "a new row's contents ride its growth");
}

#[test]
fn layout_out_of_sight_before_and_after_moves_at_once() {
    fn view(wide: &bool, _cx: &mut Cx<bool>) -> El {
        let rows = (0..40).map(|i| el().key(format!("r{i}")).w(if *wide { 200.0 } else { 100.0 }).h(40.0).fill(Color::WHITE));
        col().w_full().h_full()
            .child(col().key("scroll").w_full().h(100.0)
                .scroll(ScrollSpec { stick_bottom: false }).child(col().children(rows)))
            .child(el().key("crosser").absolute(if *wide { -80.0 } else { 380.0 }, 20.0).size(20.0, 20.0).fill(Color::WHITE))
            .child(col().key("drawer").w_full().h(if *wide { 100.0 } else { 0.0 }).clip_soft(0.0)
                .child(el().key("inside-closing").absolute(10.0, if *wide { 180.0 } else { 20.0 }).size(40.0, 20.0).fill(Color::WHITE)))
    }
    let mut rt = Runtime::new(true, view, (300.0, 300.0), Env::new(true, 2.0));
    rt.frame(0.0);
    rt.frame(1.0 / 120.0);
    rt.update(|s, _| *s = false);
    rt.frame(1.0 / 120.0);
    let rect = |rt: &Runtime<bool>, key: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().rect;
    assert!(rect(&rt, "r0")[2] > 190.0, "a row in sight springs: {}", rect(&rt, "r0")[2]);
    assert_eq!(rect(&rt, "r30")[2], 100.0, "a row out of sight before and after lands at once");
    assert!(rect(&rt, "crosser")[0] < 0.0, "an element sweeping across the window animates: {:?}", rect(&rt, "crosser"));
    assert!(rect(&rt, "inside-closing")[1] > 150.0, "an element entering a still-closing clip animates: {:?}", rect(&rt, "inside-closing"));
}

#[test]
fn animate_layout_lets_a_growth_row_s_new_contents_fade_in() {
    use crate::el::GrowthSpec;
    fn view(s: &(bool, bool), _cx: &mut Cx<(bool, bool)>) -> El {
        let trigger = crate::row().key("trigger").h(28.0).animate_layout()
            .child(el().key(if s.0 { "after" } else { "before" }).size(if s.0 { 88.0 } else { 53.0 }, 16.0));
        let mut stack = col().key("stack").w_full().growth(GrowthSpec::default());
        if s.1 { stack = stack.virtualize(40.0); }
        col().w_full().h_full().child(col().key("scroll").w_full().h(100.0).scroll(ScrollSpec { stick_bottom: true })
            .child(stack.child(col().key("r0").w_full().child(col().key("part").child(trigger)))))
    }
    let opacity = |rt: &Runtime<(bool, bool)>, key: &str| rt.elements().into_iter()
        .find(|e| e.key.as_deref() == Some(key)).map(|e| e.opacity);
    for virtualized in [false, true] {
        let mut rt = Runtime::new((false, virtualized), view, (300.0, 100.0), Env::new(true, 2.0));
        rt.frame(0.0);
        for _ in 0..30 { rt.frame(1.0 / 120.0); }
        rt.update(|s, _| s.0 = true);
        rt.frame(1.0 / 120.0);
        let (before, after) = (opacity(&rt, "before").unwrap_or(0.0), opacity(&rt, "after").unwrap());
        assert!(before > 0.5 && after < 0.5, "virtualized {virtualized}: crossfades, {before} out and {after} in");
    }
}

#[test]
fn explicit_transitions_play_inside_a_growth_row() {
    use crate::el::GrowthSpec;
    fn view(s: &(bool, bool), _cx: &mut Cx<(bool, bool)>) -> El {
        let mut trigger = crate::row().key("trigger").h(28.0).child(el().key("label").size(53.0, 16.0));
        if s.0 {
            trigger = trigger.child(el().key("badge").size(16.0, 16.0)
                .transition(Transition::offset(0.0, 8.0).combined(Transition::opacity())));
        }
        if s.1 { trigger = trigger.animate_layout(); }
        col().key("stack").w_full().growth(GrowthSpec::default())
            .child(col().key("r0").w_full().child(trigger))
    }
    let badge = |rt: &Runtime<(bool, bool)>| rt.elements().into_iter()
        .find(|e| e.key.as_deref() == Some("badge")).map(|e| (e.opacity, e.exiting));
    for animate in [false, true] {
        let mut rt = Runtime::new((false, animate), view, (300.0, 100.0), Env::new(true, 2.0));
        rt.frame(0.0);
        for _ in 0..30 { rt.frame(1.0 / 120.0); }
        rt.update(|s, _| s.0 = true);
        rt.frame(1.0 / 120.0);
        let (op, _) = badge(&rt).unwrap();
        assert!(op < 0.5, "animate_layout {animate}: it enters on its transition: {op}");
        for _ in 0..120 { rt.frame(1.0 / 120.0); }
        rt.update(|s, _| s.0 = false);
        rt.frame(1.0 / 120.0);
        let (op, exiting) = badge(&rt).expect("it plays its exit");
        assert!(exiting && op > 0.5, "animate_layout {animate}: it exits on its transition: {op}");
    }
}

/// A long text in a scroll view walks only the lines its clip shows: the glyphs of lines
/// far below aren't rasterized until scrolled to, while a tall span whose line box sits
/// more than two base ems below the clip still paints the ascenders that reach inside.
#[test]
fn a_long_scrolled_text_paints_only_its_visible_lines() {
    use crate::{text, text_input, Span, TextEdit, TextStyle};
    struct Note { edit: TextEdit }
    fn view(s: &Note, _: &mut Cx<Note>) -> El {
        col().w_full().h_full().child(col().key("viewport").w_full().h(200.0).scroll(ScrollSpec { stick_bottom: false })
            .child(text_input(&s.edit, TextStyle::new(14.0, 400.0).lh(20.0)).key("note").w_full().on_edit(|s: &mut Note| &mut s.edit)))
    }
    let mut content = "plain lowercase words only\n".repeat(500);
    content += "QUIZ JOKE VEX";
    let mut rt = Runtime::new(Note { edit: TextEdit::new(content) }, view, (300.0, 300.0), Env::new(true, 2.0));
    rt.frame(0.0);
    let top = rt.text.atlas.glyph_entries();
    let glyphs = rt.stats.glyphs;
    assert!(glyphs > 0 && glyphs < 400, "about ten visible lines painted: {glyphs}");
    rt.wheel(150.0, 100.0, -1e5);
    rt.frame(1.0 / 60.0);
    assert!(rt.text.atlas.glyph_entries() > top, "the capitals on the last line rasterize only once shown");

    // Line 6 sits at y = 72..84 pt, 22 pt (> 2 * 10 pt) below the 50 pt clip, but its 120 pt
    // span's ascender reaches up into the clip and must still paint.
    fn span_view(_: &(), _: &mut Cx<()>) -> El {
        let s = "a\n".repeat(6) + "T";
        col().w_full().h_full().child(col().w_full().h(50.0).scroll(ScrollSpec { stick_bottom: false })
            .child(text(&s).font(TextStyle::new(10.0, 400.0).lh(12.0)).spans(vec![Span::new(12, 13).size(120.0)])))
    }
    let mut rt = Runtime::new((), span_view, (200.0, 200.0), Env::new(true, 2.0));
    rt.frame(0.0);
    assert_eq!(rt.stats.glyphs, 5, "4 visible 'a' lines plus the overhanging 'T' glyph");
    assert_eq!(rt.text.atlas.glyph_entries(), 2, "'a' and overhanging 'T' both rasterized");
}

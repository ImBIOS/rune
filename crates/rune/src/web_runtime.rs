//! Browser-only access to the focused edit, kept beside Runtime so the shell sees no nodes.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
use super::*;

/// The last context sent to the DOM. Secure text never enters this cache.
#[derive(Default)]
pub(crate) struct Mirror {
    pub key: String,
    pub text: String,
    /// UTF-8 byte offset in the model; DOM offsets are UTF-16 relative to this window.
    pub base: usize,
    pub anchor: u32,
    pub focus: u32,
    pub secure: bool,
}

/// Read at most 512 UTF-16 units on each side, without scanning the document prefix.
fn context_window(edit: &crate::TextEdit) -> (usize, &str, u32, u32) {
    let (a, b) = (edit.anchor.min(edit.focus), edit.anchor.max(edit.focus));
    let mut start = a;
    let mut units = 0;
    for (i, ch) in edit.text[..a].char_indices().rev() {
        if units + ch.len_utf16() > 512 { break; }
        units += ch.len_utf16();
        start = i;
    }
    let mut end = b;
    let mut units = 0;
    for ch in edit.text[b..].chars() {
        if units + ch.len_utf16() > 512 { break; }
        units += ch.len_utf16();
        end += ch.len_utf8();
    }
    let anchor = edit.text[start..edit.anchor].encode_utf16().count() as u32;
    let focus = edit.text[start..edit.focus].encode_utf16().count() as u32;
    (start, &edit.text[start..end], anchor, focus)
}

impl Mirror {
    fn update(&mut self, key: &str, edit: Option<&crate::TextEdit>) -> bool {
        let secure = edit.is_some_and(|e| e.is_secure());
        let (base, text, anchor, focus) = edit.filter(|_| !secure).map(context_window).unwrap_or((0, "", 0, 0));
        if self.key == key && self.base == base && self.text == text && self.anchor == anchor
            && self.focus == focus && self.secure == secure { return false; }
        self.key.clear(); self.key.push_str(key);
        self.text.clear(); self.text.push_str(text);
        self.base = base; self.anchor = anchor; self.focus = focus; self.secure = secure;
        true
    }
}

pub(super) struct TouchScroll {
    id: NodeId,
    origin: f64,
    dragging: bool,
    /// The pan moves the viewport's horizontal axis (`El::scroll_x`).
    horizontal: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn utf16_replacement_preserves_callbacks_and_rejects_stale_context() {
        fn view(state: &(crate::TextEdit, usize), _: &mut Cx<(crate::TextEdit, usize)>) -> El {
            crate::text_input(&state.0, crate::tok::type_::BODY).key("input")
                .on_edit(|s: &mut (crate::TextEdit, usize)| &mut s.0)
                .on_input_change(|s: &mut (crate::TextEdit, usize), _| s.1 += 1)
        }
        let mut rt = Runtime::new((crate::TextEdit::new("a🦀c"), 0), view, (300.0, 100.0), Env::default());
        rt.frame(0.0);
        let key = rt.nodes.iter().find(|n| n.key.as_deref() == Some("input")).unwrap().path.clone();
        rt.set_focus(Some(key.clone()), true);
        let event = json!({"kind":"edit", "key":key, "base":0, "expected":"a🦀c",
            "start":1, "end":3, "text":"é", "anchor":2, "focus":2});
        rt.web_edit(&event);
        rt.frame(0.0);
        assert_eq!((&*rt.state.0.text, rt.state.1, rt.state.0.focus), ("aéc", 1, 3));
        rt.web_edit(&event);
        rt.frame(0.0);
        assert_eq!((&*rt.state.0.text, rt.state.1), ("aéc", 1));
        assert_eq!(byte_at("a🦀c", 2), None);
    }

    #[test]
    fn a_touch_pan_moves_the_axis_its_first_direction_picks() {
        fn view(_: &(), _: &mut Cx<()>) -> El {
            let tiles = (0..10).fold(crate::row(), |r, i| r.child(crate::el().key(format!("tile{i}")).w(100.0).h(100.0)));
            let rows = (0..20).fold(crate::col().w_full().child(crate::col().key("strip").w_full().h(100.0).scroll_x().child(tiles)),
                |c, i| c.child(crate::el().key(format!("row{i}")).w_full().h(40.0)));
            crate::col().w_full().h_full().child(crate::col().key("page").w_full().h(300.0)
                .scroll(crate::ScrollSpec { stick_bottom: false }).child(rows))
        }
        let mut rt = Runtime::new((), view, (300.0, 300.0), Env::default());
        rt.frame(0.0);
        let id = |rt: &Runtime<()>, key: &str| rt.nodes.iter().find(|n| n.key.as_deref() == Some(key)).unwrap().id;
        let (page, strip) = (id(&rt, "page"), id(&rt, "strip"));

        let vertical = rt.web_touch_start(150.0, 50.0);
        assert_eq!(vertical, Some(page));
        assert_eq!(rt.web_touch_pan(150.0, 50.0, true, vertical), (Some(strip), true));
        rt.web_touch_move(strip, -50.0);
        assert_eq!((rt.scrolls[&strip].x.offset, rt.scrolls[&page].offset), (50.0, 0.0));
        rt.web_touch_end(strip, 0.0);

        // A vertical pan over the strip passes through to the page.
        let vertical = rt.web_touch_start(150.0, 50.0);
        assert_eq!(rt.web_touch_pan(150.0, 50.0, false, vertical), (Some(page), false));
        rt.web_touch_move(page, -30.0);
        assert_eq!((rt.scrolls[&strip].x.offset, rt.scrolls[&page].offset), (50.0, 30.0));
    }

    #[test]
    fn edge_return_is_frame_rate_independent() {
        let one = rebound(-50.0, -400.0, 0.0, 0.5);
        let mut many = (-50.0, -400.0);
        for _ in 0..60 { many = rebound(many.0, many.1, 0.0, 0.5 / 60.0); }
        assert!((one.0 - many.0).abs() < 0.001);
        assert!((one.1 - many.1).abs() < 0.01);
        assert!(one.0 < 0.0 && one.0 > -1.0);
    }

    #[test]
    fn context_is_bounded_at_unicode_boundaries_and_keeps_the_selection() {
        let mut edit = crate::TextEdit::new("é🦀a".repeat(800));
        let boundaries: Vec<_> = edit.text.char_indices().map(|(i, _)| i).chain([edit.text.len()]).collect();
        for &a in boundaries.iter().step_by(157) {
            for &f in boundaries.iter().step_by(271) {
                edit.anchor = a; edit.focus = f;
                let (base, text, anchor, focus) = context_window(&edit);
                assert_eq!(base + byte_at(text, anchor as usize).unwrap(), a);
                assert_eq!(base + byte_at(text, focus as usize).unwrap(), f);
                let before = edit.text[base..a.min(f)].encode_utf16().count();
                let after = edit.text[a.max(f)..base + text.len()].encode_utf16().count();
                assert!(before <= 512 && after <= 512);
                if base > 0 { assert!(before + edit.text[..base].chars().next_back().unwrap().len_utf16() > 512); }
                if base + text.len() < edit.text.len() {
                    assert!(after + edit.text[base + text.len()..].chars().next().unwrap().len_utf16() > 512);
                }
            }
        }
    }

    #[test]
    fn mirror_skips_unchanged_frames_and_never_caches_secure_text() {
        let mut mirror = Mirror::default();
        let mut edit = crate::TextEdit::new("🦀".repeat(10_000));
        assert!(mirror.update("field", Some(&edit)));
        assert_eq!(mirror.text.encode_utf16().count(), 512);
        let allocation = mirror.text.as_ptr();
        for _ in 0..120 { assert!(!mirror.update("field", Some(&edit))); }
        assert_eq!(mirror.text.as_ptr(), allocation);
        edit.anchor -= 4;
        assert!(mirror.update("field", Some(&edit)), "selection-only changes sync");
        edit.text.push('x');
        assert!(mirror.update("field", Some(&edit)), "edits within the window sync");
        assert!(mirror.update("other", Some(&edit)), "same contents in a different field sync");
        edit.set_secure(true);
        assert!(mirror.update("other", Some(&edit)));
        assert!(mirror.secure && mirror.text.is_empty());
        assert_eq!((mirror.base, mirror.anchor, mirror.focus), (0, 0, 0));
        edit.insert("secret");
        assert!(!mirror.update("other", Some(&edit)), "secret edits don't mirror to the DOM");
        assert!(mirror.update("", None));
        assert!(!mirror.update("", None));
    }

    #[test]
    fn recomposition_replaces_its_range_without_changing_committed_text() {
        fn view(edit: &crate::TextEdit, _: &mut Cx<crate::TextEdit>) -> El {
            crate::text_input(edit, crate::tok::type_::BODY).key("input").on_edit(|s: &mut crate::TextEdit| s)
        }
        let mut rt = Runtime::new(crate::TextEdit::new("🦀 hello "), view, (300.0, 100.0), Env::default());
        rt.frame(0.0);
        let key = rt.nodes.iter().find(|n| n.key.as_deref() == Some("input")).unwrap().path.clone();
        rt.set_focus(Some(key.clone()), true);
        let mut event = json!({"kind":"preedit", "key":key, "base":5, "expected":"hello ",
            "start":0, "end":6, "text":"hello", "anchor":5, "focus":5});
        rt.web_edit(&event);
        assert_eq!(rt.state.text, "🦀 hello ");
        assert_eq!(rt.state.display().0, "🦀 hello", "preedit replaces the word, not appends");
        event["text"] = json!("help");
        event["anchor"] = json!(4); event["focus"] = json!(4);
        rt.web_edit(&event);
        assert_eq!(rt.state.display().0, "🦀 help");
        event["kind"] = json!("edit");
        rt.web_edit(&event);
        assert_eq!(rt.state.text, "🦀 help");
        assert!(rt.state.preedit.is_none());
        assert_eq!(rt.state.focus, rt.state.text.len());
        rt.web_edit(&event);
        assert_eq!(rt.state.text, "🦀 help", "stale context rejected");
        event["base"] = json!(1);
        rt.web_edit(&event);
        assert_eq!(rt.state.text, "🦀 help", "base must be a UTF-8 boundary");
    }

    #[test]
    fn replacement_offsets_are_local_utf16_and_cannot_escape_the_snapshot() {
        fn view(edit: &crate::TextEdit, _: &mut Cx<crate::TextEdit>) -> El {
            crate::text_input(edit, crate::tok::type_::BODY).key("input").on_edit(|s: &mut crate::TextEdit| s)
        }
        let mut rt = Runtime::new(crate::TextEdit::new("é🦀 tail"), view, (300.0, 100.0), Env::default());
        rt.frame(0.0);
        let key = rt.nodes.iter().find(|n| n.key.as_deref() == Some("input")).unwrap().path.clone();
        rt.set_focus(Some(key.clone()), true);
        let mut event = json!({"kind":"edit", "key":key, "base":2, "expected":"🦀 ",
            "start":1, "end":2, "text":"x", "anchor":1, "focus":1});
        rt.web_edit(&event);
        assert_eq!(rt.state.text, "é🦀 tail", "mid-surrogate offsets rejected");
        event["start"] = json!(0); event["end"] = json!(4);
        rt.web_edit(&event);
        assert_eq!(rt.state.text, "é🦀 tail", "replacement must stay inside expected context");
        event["end"] = json!(2);
        rt.web_edit(&event);
        assert_eq!(rt.state.text, "éx tail");
        assert_eq!(rt.state.focus, 3);
    }
}

/// A critically damped edge return, integrated exactly for variable frame times.
fn rebound(offset: f64, velocity: f32, target: f64, dt: f32) -> (f64, f32) {
    let dt = f64::from(dt);
    let displacement = offset - target;
    let b = f64::from(velocity) + 14.0 * displacement;
    let decay = (-14.0 * dt).exp();
    (target + (displacement + b * dt) * decay, ((f64::from(velocity) - 14.0 * b * dt) * decay) as f32)
}

/// Reject offsets in the middle of a surrogate pair, rather than corrupting the Rust string.
fn byte_at(text: &str, utf16: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units == utf16 { return Some(byte); }
        units += ch.len_utf16();
    }
    (units == utf16).then_some(text.len())
}

impl<S: 'static> Runtime<S> {
    /// A touch went down: it stops the momentum of the viewports under it and holds the
    /// vertical one until the pan picks its axis (`web_touch_pan`).
    pub(crate) fn web_touch_start(&mut self, x: f32, y: f32) -> Option<NodeId> {
        if let Some(st) = self.scroll_at_axis(x, y, true).and_then(|id| self.scrolls.get_mut(&id)) { st.x.v = 0.0; }
        let id = self.scroll_at(x, y)?;
        let st = self.scrolls.get_mut(&id)?;
        st.v = 0.0;
        st.glide = None;
        self.web_scroll = Some(TouchScroll { id, origin: st.offset, dragging: true, horizontal: false });
        Some(id)
    }

    /// The touch that went down at (x, y) started panning, mostly horizontally or not: a
    /// horizontal pan takes the innermost horizontal viewport there, and any other pan stays
    /// with the vertical one. Returns the viewport and whether it moves horizontally.
    pub(crate) fn web_touch_pan(&mut self, x: f32, y: f32, horizontal: bool, vertical: Option<NodeId>) -> (Option<NodeId>, bool) {
        let Some(id) = self.scroll_at_axis(x, y, true).filter(|_| horizontal) else { return (vertical, false) };
        let Some(st) = self.scrolls.get_mut(&id) else { return (vertical, false) };
        self.web_scroll = Some(TouchScroll { id, origin: st.x.offset, dragging: true, horizontal: true });
        (Some(id), true)
    }

    /// The pan moved `d` points along its axis from where the touch went down.
    pub(crate) fn web_touch_move(&mut self, id: NodeId, d: f32) {
        let Some(touch) = &self.web_scroll else { return };
        if touch.id != id { return; }
        let (origin, horizontal) = (touch.origin, touch.horizontal);
        let Some(st) = self.scrolls.get_mut(&id) else { return };
        let (offset, _, max) = st.axis(horizontal);
        let to = origin - f64::from(d);
        let edge = to.clamp(0.0, max);
        let over = to - edge;
        *offset = edge + over * 0.55 / (1.0 + over.abs() * 0.55 / 300.0);
        if !horizontal { st.pinned = false; }
        st.direct = true;
        self.scroll_cancels_press(id);
    }

    pub(crate) fn web_touch_end(&mut self, id: NodeId, velocity: f32) {
        let Some(touch) = &mut self.web_scroll else { return };
        if touch.id != id { return; }
        touch.dragging = false;
        let horizontal = touch.horizontal;
        if let Some(st) = self.scrolls.get_mut(&id) { *st.axis(horizontal).1 = -velocity.clamp(-8000.0, 8000.0); }
    }

    /// Own the touched viewport's panned axis through momentum and its edge return. Other
    /// viewports use the native wheel path, including native clamping and stick-to-bottom.
    pub(super) fn web_step_scroll(&mut self, id: NodeId, overflow: f64, dt: f32, horizontal: bool) -> Option<bool> {
        let touch = self.web_scroll.as_ref().filter(|t| t.id == id && t.horizontal == horizontal)?;
        let dragging = touch.dragging;
        let st = self.scrolls.get_mut(&id)?;
        if horizontal { st.x.max = overflow.max(0.0); } else { st.max = overflow.max(0.0); }
        let v = *st.axis(horizontal).1;
        st.scrolling = dragging || v.abs() > 1.0;
        if dragging { return Some(false); }
        st.direct = false;
        let (offset, v, max) = st.axis(horizontal);
        let edge = offset.clamp(0.0, max);
        if *offset != edge {
            (*offset, *v) = rebound(*offset, *v, edge, dt);
            if (*offset - edge).abs() < 0.1 && v.abs() < 1.0 { *offset = edge; *v = 0.0; }
        } else {
            let decay = (-SCROLL_DECAY * dt).exp();
            *offset += f64::from(*v * (1.0 - decay) / SCROLL_DECAY);
            *v *= decay;
        }
        let moving = v.abs() > 1.0 || *offset < 0.0 || *offset > max;
        if !moving { self.web_scroll = None; }
        Some(moving)
    }

    pub(crate) fn web_context(&self, mirror: &mut Mirror) -> bool {
        match self.focused_input().filter(|i| i.access.is_some()) {
            Some(input) => mirror.update(self.focused.as_deref().unwrap_or_default(), Some(&input.edit)),
            None => mirror.update("", None),
        }
    }

    pub(crate) fn web_secure(&mut self, key: &str, text: String) {
        let event = crate::KeyEvent { key: crate::Key::Text(text), mods: crate::Mods::default() };
        if self.focused.as_deref() == Some(key) && self.has_secure_focus() {
            self.key(event);
        }
    }

    pub(crate) fn web_edit(&mut self, event: &serde_json::Value) {
        let Some(key) = event["key"].as_str() else { return };
        if self.focused.as_deref() != Some(key) || self.has_secure_focus() { return; }
        let number = |key: &str| event[key].as_u64().and_then(|n| usize::try_from(n).ok());
        let Some(base) = number("base") else { return };
        let Some(expected) = event["expected"].as_str() else { return };
        let mut tx = self.transaction();
        let changed = self.with_focused(|edit| {
            // Browser edits belong to the snapshot from which its context was mirrored.
            if !edit.text.get(base..).is_some_and(|s| s.starts_with(expected)) { return EditOutcome::default(); }
            if event["kind"] == "edit" || event["kind"] == "preedit" {
                let Some((start, end)) = number("start").zip(number("end")) else { return EditOutcome::default() };
                let Some((a, b)) = byte_at(expected, start).zip(byte_at(expected, end)) else { return EditOutcome::default() };
                if a > b { return EditOutcome::default(); }
                edit.ime(&Ime::Preedit(String::new(), None));
                edit.anchor = base + a; edit.focus = base + b;
                let text = event["text"].as_str().unwrap_or_default();
                if event["kind"] == "preedit" {
                    edit.ime(&Ime::Preedit(text.to_owned(), Some((text.len(), text.len()))));
                    return EditOutcome::default();
                }
                edit.insert(text);
            }
            if let Some((anchor, focus)) = number("anchor").zip(number("focus"))
                && let Some((a, f)) = byte_at(&edit.text[base..], anchor).zip(byte_at(&edit.text[base..], focus)) {
                edit.anchor = base + a; edit.focus = base + f;
            }
            EditOutcome::default()
        }, &mut tx);
        if changed { self.dirty = true; }
        merge(&mut self.pending, tx);
    }
}

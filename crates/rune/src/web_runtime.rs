//! Browser-only access to the focused edit, kept beside Runtime so the shell sees no nodes.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
use super::*;

pub(super) struct TouchScroll {
    id: NodeId,
    origin: f64,
    dragging: bool,
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
    fn edge_return_is_frame_rate_independent() {
        let one = rebound(-50.0, -400.0, 0.0, 0.5);
        let mut many = (-50.0, -400.0);
        for _ in 0..60 { many = rebound(many.0, many.1, 0.0, 0.5 / 60.0); }
        assert!((one.0 - many.0).abs() < 0.001);
        assert!((one.1 - many.1).abs() < 0.01);
        assert!(one.0 < 0.0 && one.0 > -1.0);
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
    pub(crate) fn web_touch_start(&mut self, x: f32, y: f32) -> Option<NodeId> {
        let id = self.scroll_at(x, y)?;
        let st = self.scrolls.get_mut(&id)?;
        st.v = 0.0;
        st.glide = None;
        self.web_scroll = Some(TouchScroll { id, origin: st.offset, dragging: true });
        Some(id)
    }

    pub(crate) fn web_touch_move(&mut self, id: NodeId, dy: f32) {
        let Some(touch) = &self.web_scroll else { return };
        if touch.id != id { return; }
        let Some(st) = self.scrolls.get_mut(&id) else { return };
        let offset = touch.origin - f64::from(dy);
        let edge = offset.clamp(0.0, st.max);
        let d = offset - edge;
        st.offset = edge + d * 0.55 / (1.0 + d.abs() * 0.55 / 300.0);
        st.pinned = false;
        st.direct = true;
        self.scroll_cancels_press(id);
    }

    pub(crate) fn web_touch_end(&mut self, id: NodeId, velocity: f32) {
        let Some(touch) = &mut self.web_scroll else { return };
        if touch.id != id { return; }
        touch.dragging = false;
        if let Some(st) = self.scrolls.get_mut(&id) { st.v = -velocity.clamp(-8000.0, 8000.0); }
    }

    /// Own the touched viewport through momentum and its edge return. Other viewports use
    /// the native wheel path, including native clamping and stick-to-bottom behavior.
    pub(super) fn web_step_scroll(&mut self, id: NodeId, overflow: f64, dt: f32) -> Option<bool> {
        let touch = self.web_scroll.as_ref().filter(|t| t.id == id)?;
        let st = self.scrolls.get_mut(&id)?;
        st.max = overflow.max(0.0);
        st.scrolling = touch.dragging || st.v.abs() > 1.0;
        if touch.dragging { return Some(false); }
        st.direct = false;
        let edge = st.offset.clamp(0.0, st.max);
        if st.offset != edge {
            (st.offset, st.v) = rebound(st.offset, st.v, edge, dt);
            if (st.offset - edge).abs() < 0.1 && st.v.abs() < 1.0 { st.offset = edge; st.v = 0.0; }
        } else {
            let decay = (-SCROLL_DECAY * dt).exp();
            st.offset += f64::from(st.v * (1.0 - decay) / SCROLL_DECAY);
            st.v *= decay;
        }
        let moving = st.v.abs() > 1.0 || st.offset < 0.0 || st.offset > st.max;
        if !moving { self.web_scroll = None; }
        Some(moving)
    }

    pub(crate) fn web_context(&self) -> Option<(String, String, u32, u32)> {
        let input = self.focused_input()?;
        if input.access.is_none() || input.edit.is_secure() { return None; }
        let edit = &input.edit;
        Some((self.focused.clone()?, edit.text.clone(), edit.text[..edit.anchor].encode_utf16().count() as u32,
            edit.text[..edit.focus].encode_utf16().count() as u32))
    }

    pub(crate) fn web_edit(&mut self, event: &serde_json::Value) {
        let Some(key) = event["key"].as_str() else { return };
        if self.focused.as_deref() != Some(key) { return; }
        let number = |key: &str| event[key].as_u64().and_then(|n| usize::try_from(n).ok());
        let Some((base, anchor, focus)) = number("base").zip(number("anchor")).zip(number("focus")).map(|((b, a), f)| (b, a, f)) else { return };
        let Some(expected) = event["expected"].as_str() else { return };
        let mut tx = self.transaction();
        let changed = self.with_focused(|edit| {
            let Some(base_byte) = byte_at(&edit.text, base) else { return EditOutcome::default() };
            // Browser edits belong to the snapshot from which its context was mirrored.
            if !edit.text[base_byte..].starts_with(expected) { return EditOutcome::default(); }
            if event["kind"] == "edit" {
                let Some((start, end)) = number("start").zip(number("end")) else { return EditOutcome::default() };
                let Some((a, b)) = byte_at(&edit.text, start).zip(byte_at(&edit.text, end)) else { return EditOutcome::default() };
                if a > b { return EditOutcome::default(); }
                edit.ime(&Ime::Preedit(String::new(), None));
                edit.anchor = a; edit.focus = b;
                edit.insert(event["text"].as_str().unwrap_or_default());
            }
            if let Some((a, f)) = byte_at(&edit.text, anchor).zip(byte_at(&edit.text, focus)) {
                edit.anchor = a; edit.focus = f;
            }
            EditOutcome::default()
        }, &mut tx);
        if changed { self.dirty = true; }
        merge(&mut self.pending, tx);
    }
}

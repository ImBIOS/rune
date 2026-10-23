//! The focused text field's caret in window points, for views (`Cx::caret`) and tools
//! (`ElementInfo::caret`). It comes from layout, not the drawn frame, so a view that reads it
//! is re-measured in the same commit when an edit moves it.
use super::*;

impl<S: 'static> Runtime<S> {
    /// Node `i`'s caret line `[x, y, w, h]` in window points at its laid-out place, after
    /// scrolling, if it is a focused text field.
    pub(super) fn caret_rect(&self, nodes: &[Node], i: usize) -> Option<[f32; 4]> {
        let n = &nodes[i];
        let (inp, spec, layout) = (n.input.as_ref()?, n.text.as_ref()?, n.text_layout.as_ref()?);
        if !inp.edit.focused { return None; }
        let s = self.env.scale;
        let mut y = n.target[1];
        let mut up = n.parent;
        while let Some(p) = up {
            if let Some(st) = self.scrolls.get(&nodes[p].id) { y -= st.offset as f32; }
            up = nodes[p].parent;
        }
        let tw = layout.full_width().ceil() / s;
        let (x0, x1) = (n.target[0] + n.inset[0], n.target[2] - n.inset[2]);
        let mut x = match spec.align {
            _ if line_alignment(spec) != parley::Alignment::Start => x0,
            Align::Start => x0,
            Align::Center => x0 + (x1 - x0 - tw) * 0.5,
            Align::End => x1 - tw,
        };
        if inp.single_line {
            let prev = self.input_scroll.get(&n.path).copied().unwrap_or(0.0);
            x -= single_line_offset(prev, inp, &spec.content, layout, s, (x1 - x0) * s) / s;
        }
        let cur = Cursor::from_byte_index(layout, caret_index(&inp.edit, &spec.content), Affinity::Downstream);
        let bb = cur.geometry(layout, 1.5 * s);
        let h = if bb.y1 > bb.y0 { (bb.y1 - bb.y0) as f32 / s } else { spec.style.line_px() };
        Some([x + bb.x0 as f32 / s, y + n.inset[1] + bb.y0 as f32 / s, 1.5, h])
    }

    /// After an edit, scrolls the focused field's nearest scroll viewport just far enough to
    /// show the caret line, as a browser does for a textarea.
    pub(super) fn follow_caret(&mut self) {
        let Some(i) = self.nodes.iter().position(|n| self.focused.as_deref() == Some(n.path.as_str())) else { return };
        let Some(r) = self.caret_rect(&self.nodes, i) else { return };
        let mut up = self.nodes[i].parent;
        let v = loop {
            let Some(p) = up else { return };
            if self.nodes[p].scroll.is_some() { break p; }
            up = self.nodes[p].parent;
        };
        let mut top = self.nodes[v].target[1];
        let mut up = self.nodes[v].parent;
        while let Some(p) = up {
            if let Some(st) = self.scrolls.get(&self.nodes[p].id) { top -= st.offset as f32; }
            up = self.nodes[p].parent;
        }
        let view_h = f64::from(self.nodes[v].target[3] - self.nodes[v].target[1]);
        let (y, h) = (f64::from(r[1] - top), f64::from(r[3]));
        let stick = self.nodes[v].scroll.is_some_and(|s| s.stick_bottom);
        let Some(st) = self.scrolls.get_mut(&self.nodes[v].id) else { return };
        let delta = if y < 0.0 { y } else if y + h > view_h { y + h - view_h } else { return };
        st.offset = (st.offset + delta).clamp(0.0, st.max);
        st.pinned = stick && st.offset >= st.max - 0.5;
        st.v = 0.0;
        st.glide = None;
        st.direct = true;
    }

    /// Refreshes the caret the next view sees from `nodes` (the current frame's when `None`).
    /// Returns whether it moved.
    pub(super) fn caret_moved(&mut self, nodes: Option<&[Node]>) -> bool {
        let nodes = nodes.unwrap_or(&self.nodes);
        let next = nodes.iter().position(|n| n.input.as_ref().is_some_and(|inp| inp.edit.focused))
            .and_then(|i| Some((i, self.caret_rect(nodes, i)?)));
        let same = match (&self.caret, next) {
            (Some((path, rect)), Some((i, r))) => *path == nodes[i].path && *rect == r,
            (None, None) => true,
            _ => false,
        };
        if same { return false; }
        self.caret = next.map(|(i, r)| (nodes[i].path.clone(), r));
        true
    }
}

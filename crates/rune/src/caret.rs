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
        let y = scroll_offsets(&self.scrolls, nodes, i).fold(n.target[1], |y, offset| y - offset);
        let tw = layout.full_width().ceil() / s;
        let (x0, x1) = (n.target[0] + n.inset[0], n.target[2] - n.inset[2]);
        let mut x = text_x(spec, x0, x1, tw);
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
        let Some(i) = self.focused_index() else { return };
        let Some(r) = self.caret_rect(&self.nodes, i) else { return };
        let Some(v) = std::iter::successors(self.nodes[i].parent, |&p| self.nodes[p].parent)
            .find(|&p| self.nodes[p].scroll.is_some()) else { return };
        let top = scroll_offsets(&self.scrolls, &self.nodes, v).fold(self.nodes[v].target[1], |y, offset| y - offset);
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

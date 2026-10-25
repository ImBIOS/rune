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
        let [sx, sy] = scroll_offsets(&self.scrolls, nodes, i).fold([0.0; 2], |a, o| [a[0] + o[0], a[1] + o[1]]);
        let y = n.target[1] - sy;
        let tw = layout.full_width().ceil() / s;
        let (x0, x1) = (n.target[0] + n.inset[0] - sx, n.target[2] - n.inset[2] - sx);
        let mut x = text_x(spec, x0, x1, tw);
        if inp.single_line {
            let prev = self.input_scroll.get(&n.path).copied().unwrap_or(0.0);
            x -= single_line_offset(prev, inp, &spec.content, layout, s, (x1 - x0) * s) / s;
        }
        let cur = Cursor::from_byte_index(layout, caret_index(&inp.edit, &spec.content), Affinity::Downstream);
        let bb = crate::text::cursor_geometry(&cur, layout, 1.5 * s);
        let h = if bb.y1 > bb.y0 { (bb.y1 - bb.y0) as f32 / s } else { spec.style.line_px() };
        Some([x + bb.x0 as f32 / s, y + n.inset[1] + bb.y0 as f32 / s, 1.5, h])
    }

    /// After an edit, scrolls the focused field's nearest scroll viewport on each axis just far
    /// enough to show the caret line, as a browser does for a textarea.
    pub(super) fn follow_caret(&mut self) {
        let Some(i) = self.focused_index() else { return };
        let Some(r) = self.caret_rect(&self.nodes, i) else { return };
        for horizontal in [false, true] {
            let Some(v) = std::iter::successors(self.nodes[i].parent, |&p| self.nodes[p].parent)
                .find(|&p| self.nodes[p].scroll.is_some() && self.nodes[p].scroll_axes.has(horizontal)) else { continue };
            let a = usize::from(!horizontal);
            let start = scroll_offsets(&self.scrolls, &self.nodes, v).fold(self.nodes[v].target[a], |p, o| p - o[a]);
            let view = f64::from(self.nodes[v].target[a + 2] - self.nodes[v].target[a]);
            let (at, len) = (f64::from(r[a] - start), f64::from(r[a + 2]));
            let delta = if at < 0.0 { at } else if at + len > view { at + len - view } else { continue };
            let stick = self.nodes[v].scroll.is_some_and(|s| s.stick_bottom);
            let Some(st) = self.scrolls.get_mut(&self.nodes[v].id) else { continue };
            if horizontal {
                st.x.offset = (st.x.offset + delta).clamp(0.0, st.x.max);
                st.x.v = 0.0;
                st.x.glide = None;
            } else {
                st.offset = (st.offset + delta).clamp(0.0, st.max);
                st.pinned = stick && st.offset >= st.max - 0.5;
                st.v = 0.0;
                st.glide = None;
            }
            st.direct = true;
        }
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

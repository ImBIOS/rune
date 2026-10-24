//! Pressable spans in text (`El::link`). Each link's line boxes come from the shaped layout,
//! so only its glyphs take the pointer. The text element holds focus for its links and
//! `LinkState::focus` says which one; links are reported to tools as their own elements.
use super::*;
use crate::text::LinkPaint;

#[derive(Default)]
pub(super) struct LinkState {
    /// The link a pointer press began on: (text path, link index).
    press: Option<(String, usize)>,
    /// The focused text element's focused link.
    focus: Option<usize>,
}

/// Where node `n`'s text starts inside its border box `rect` (points, x0 y0 x1 y1).
fn origin(n: &Node, spec: &TextSpec, layout: &Layout<Ink>, rect: [f32; 4], s: f32) -> [f32; 2] {
    let x = text_x(spec, rect[0] + n.inset[0], rect[2] - n.inset[2], layout.full_width().ceil() / s);
    [x, rect[1] + n.inset[1]]
}

/// Line boxes of bytes `start..end` in layout device px, with each box's line index.
fn boxes(layout: &Layout<Ink>, len: usize, start: u32, end: u32) -> Vec<(parley::BoundingBox, usize)> {
    let (a, b) = ((start as usize).min(len), (end as usize).min(len));
    if a >= b { return Vec::new(); }
    Selection::new(Cursor::from_byte_index(layout, a, Affinity::Downstream), Cursor::from_byte_index(layout, b, Affinity::Upstream))
        .geometry(layout)
}

/// The link paints to draw this frame, by byte range.
pub(super) struct LinkFx(Vec<(u32, u32, LinkPaint)>);

impl LinkFx {
    pub(super) fn is_empty(&self) -> bool { self.0.is_empty() }

    pub(super) fn glyph_color(&self, byte: u32) -> Option<Color> {
        self.0.iter().find(|(a, b, p)| byte >= *a && byte < *b && p.color.is_some()).and_then(|(_, _, p)| p.color)
    }

    /// Backgrounds and underlines (device px, layout top-left at `origin`).
    pub(super) fn decos(&self, layout: &Layout<Ink>, len: usize, s: f32, origin: [f32; 2], out: &mut Vec<Deco>) {
        for &(a, b, p) in &self.0 {
            for (bb, line) in boxes(layout, len, a, b) {
                let (x0, w) = (origin[0] + bb.x0 as f32, (bb.x1 - bb.x0) as f32);
                if let Some(c) = p.bg {
                    let pad = 2.0 * s;
                    let (y0, y1) = ((origin[1] + bb.y0 as f32).round(), (origin[1] + bb.y1 as f32).round());
                    out.push(Deco { rect: [x0 - pad, y0, w + 2.0 * pad, y1 - y0], color: c, radius: 4.0 * s });
                }
                if let (Some(c), Some(l)) = (p.underline, layout.get(line)) {
                    let y = (origin[1] + l.metrics().baseline + 2.0 * s).round();
                    out.push(Deco { rect: [x0, y, w, s.max(1.0)], color: c, radius: 0.0 });
                }
            }
        }
    }
}

impl<S: 'static> Runtime<S> {
    /// Node `i`'s border box as drawn this frame (points, x0 y0 x1 y1).
    fn drawn_box(&self, i: usize) -> [f32; 4] {
        self.drawn.get(i).filter(|d| d[2] > 0.0 || d[3] > 0.0)
            .map_or(self.nodes[i].target, |d| [d[0], d[1], d[0] + d[2], d[1] + d[3]])
    }

    /// Node `i`'s link line boxes in window points (x0, y0, x1, y1) for border box `rect`.
    pub(super) fn link_boxes(&self, i: usize, rect: [f32; 4]) -> Vec<(usize, [f32; 4])> {
        let n = &self.nodes[i];
        let (Some(spec), Some(layout)) = (&n.text, &n.text_layout) else { return Vec::new() };
        let s = self.env.scale;
        let [ox, oy] = origin(n, spec, layout, rect, s);
        let mut out = Vec::new();
        for (k, l) in spec.links.iter().enumerate() {
            for (bb, _) in boxes(layout, spec.content.len(), l.start, l.end) {
                out.push((k, [ox + bb.x0 as f32 / s, oy + bb.y0 as f32 / s, ox + bb.x1 as f32 / s, oy + bb.y1 as f32 / s]));
            }
        }
        out
    }

    /// The link of node `i` under window point (x, y).
    pub(super) fn link_at(&self, i: usize, x: f32, y: f32) -> Option<usize> {
        if self.nodes[i].text.as_ref().is_none_or(|t| t.links.is_empty()) { return None; }
        self.link_boxes(i, self.drawn_box(i)).into_iter()
            .find(|(_, r)| contains(*r, x, y)).map(|(k, _)| k)
    }

    /// Hit rects for node `i`: only its links' boxes, clipped like `clipped`. False when it
    /// has no links (it takes its whole box).
    pub(super) fn push_link_hits(&mut self, i: usize, rect: [f32; 4], clipped: [f32; 4]) -> bool {
        if self.nodes[i].text.as_ref().is_none_or(|t| t.links.is_empty()) { return false; }
        let id = self.nodes[i].id;
        for (_, r) in self.link_boxes(i, rect) {
            let c = [r[0].max(clipped[0]), r[1].max(clipped[1]), r[2].min(clipped[2]), r[3].min(clipped[3])];
            if c[2] > c[0] && c[3] > c[1] { self.hits.push((id, c)); }
        }
        true
    }

    /// Which of node `i`'s links are hovered, pressed and keyboard-focused.
    fn link_states(&self, i: usize) -> (Option<usize>, Option<usize>, Option<usize>) {
        let path = &self.nodes[i].path;
        let hovered = self.pointer.filter(|_| self.hovered.as_deref() == Some(path))
            .and_then(|(x, y)| self.link_at(i, x, y));
        let held = self.capture.as_ref().is_some_and(|(p, _)| p == path) && self.capture_inside;
        let pressed = self.links.press.as_ref().filter(|(p, k)| held && p == path && hovered == Some(*k)).map(|(_, k)| *k);
        let focused = self.links.focus.filter(|_| self.keyboard_focus && self.focused.as_deref() == Some(path));
        (hovered, pressed, focused)
    }

    /// The paints node `i`'s links show this frame.
    pub(super) fn link_fx(&self, i: usize) -> LinkFx {
        let Some(spec) = self.nodes[i].text.as_ref().filter(|t| !t.links.is_empty()) else { return LinkFx(Vec::new()) };
        let (hovered, pressed, focused) = self.link_states(i);
        let mut fx = Vec::new();
        for (k, l) in spec.links.iter().enumerate() {
            let paint = if pressed == Some(k) {
                l.pressed.unwrap_or(l.hover)
            } else if hovered == Some(k) {
                l.hover
            } else if focused == Some(k) {
                let p = l.focused.unwrap_or(l.hover);
                if p.is_empty() {
                    let ink = spec.spans.iter().find(|sp| sp.start <= l.start && sp.end > l.start).and_then(|sp| sp.color);
                    LinkPaint::new().underline(ink.unwrap_or(spec.color))
                } else { p }
            } else { continue };
            fx.push((l.start, l.end, paint));
        }
        LinkFx(fx)
    }

    /// Pointer down on node `i`: remember the link it began on.
    pub(super) fn link_down(&mut self, i: usize, x: f32, y: f32) -> bool {
        let link = self.link_at(i, x, y);
        self.links.press = link.map(|k| (self.nodes[i].path.clone(), k));
        link.is_some()
    }

    /// Pointer up: a press that began and ends on the same link activates it.
    pub(super) fn link_up(&mut self, path: &str, x: f32, y: f32) {
        let Some((p, k)) = self.links.press.take() else { return };
        let Some(i) = self.nodes.iter().position(|n| n.path == p) else { return };
        if p == path && self.capture_inside && self.link_at(i, x, y) == Some(k) {
            self.queue.push(Action::Link(p, k));
        }
    }

    /// Runs the `on_link` handler of node `path` for link `k`.
    pub(super) fn press_link(&mut self, path: &str, k: usize, tx: &mut Tx) -> bool {
        let Some((f, id)) = self.node(path).and_then(|n| {
            let t = n.text.as_ref()?;
            Some((t.on_link.clone()?, t.links.get(k)?.id.clone()))
        }) else { return false };
        self.events.push(format!("link {path} {id}"));
        (f.0)(handler_state(&mut self.state), &id, tx);
        true
    }

    /// Centre of the first drawn line box of the link whose id is `id`, in points.
    pub(super) fn find_link(&self, id: &str) -> Option<(f32, f32)> {
        (0..self.nodes.len()).filter(|&i| !self.subtree_exiting(i)).find_map(|i| {
            let k = self.nodes[i].text.as_ref()?.links.iter().position(|l| l.id == id)?;
            let (_, r) = self.link_boxes(i, self.drawn_box(i)).into_iter().find(|(j, _)| *j == k)?;
            Some(((r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5))
        })
    }

    fn link_count(&self, i: usize) -> usize {
        self.nodes[i].text.as_ref().map_or(0, |t| t.links.len())
    }

    /// Tab inside the focused text's links: moves to its next (or previous) link and returns
    /// true, or false at its last (first) link so Tab leaves it.
    pub(super) fn link_tab(&mut self, backward: bool) -> bool {
        let Some(i) = self.focused_index() else { return false };
        let count = self.link_count(i);
        let Some(k) = self.links.focus.filter(|_| count > 0) else { return false };
        let next = if backward { k.checked_sub(1) } else { Some(k + 1).filter(|&j| j < count) };
        if next.is_some() { self.links.focus = next; }
        next.is_some()
    }

    /// Tab focused node `i`: start at its first link, or its last going backward.
    pub(super) fn enter_links(&mut self, i: usize, backward: bool) {
        let count = self.link_count(i);
        self.links.focus = (count > 0).then(|| if backward { count - 1 } else { 0 });
    }

    pub(super) fn blur_links(&mut self) {
        self.links.focus = None;
    }

    /// Enter or Space on a focused link presses it.
    pub(super) fn link_key(&mut self, i: usize, tx: &mut Tx) -> bool {
        let Some(k) = self.links.focus.filter(|&k| k < self.link_count(i)) else { return false };
        let path = self.nodes[i].path.clone();
        self.press_link(&path, k, tx);
        true
    }

    /// Reports each link as its own element after its text, which hands it its focus.
    pub(super) fn link_elements(&self, elements: &mut Vec<ElementInfo>) {
        for i in 0..self.nodes.len() {
            let n = &self.nodes[i];
            let Some(spec) = n.text.as_ref().filter(|t| !t.links.is_empty()) else { continue };
            let Some(at) = elements.iter().position(|e| e.path == n.path) else { continue };
            let text = elements[at].clone();
            elements[at].focusable = false;
            elements[at].focused = false;
            elements[at].focus_visible = false;
            let (hovered, pressed, _) = self.link_states(i);
            let focused = self.links.focus.filter(|_| self.focused.as_deref() == Some(&n.path));
            let boxes = self.link_boxes(i, self.drawn_box(i));
            let targets = self.link_boxes(i, n.target);
            let union = |k: usize, bs: &[(usize, [f32; 4])]| bs.iter().filter(|(j, _)| *j == k).fold(None, |u: Option<[f32; 4]>, (_, r)| {
                Some(u.map_or(*r, |u| [u[0].min(r[0]), u[1].min(r[1]), u[2].max(r[2]), u[3].max(r[3])]))
            });
            for (k, l) in spec.links.iter().enumerate() {
                let Some(r) = union(k, &boxes) else { continue };
                let t = union(k, &targets).unwrap_or(r);
                let v = text.visible;
                let (x0, y0) = (r[0].max(v[0]), r[1].max(v[1]));
                let (x1, y1) = (r[2].min(v[0] + v[2]), r[3].min(v[1] + v[3]));
                let words = spec.source_slice(l.start, l.end).map(str::to_owned);
                elements.push(ElementInfo {
                    path: format!("{}#link-{k}", n.path),
                    key: Some(l.id.clone()),
                    role: l.role,
                    label: l.label.clone().or_else(|| words.clone()),
                    text: words,
                    rect: [r[0], r[1], r[2] - r[0], r[3] - r[1]],
                    target: [t[0], t[1], t[2] - t[0], t[3] - t[1]],
                    visible: [x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0)],
                    focusable: text.focusable,
                    focused: focused == Some(k),
                    focus_visible: self.keyboard_focus && focused == Some(k),
                    hover: if hovered == Some(k) { 1.0 } else { 0.0 },
                    press: if pressed == Some(k) { 1.0 } else { 0.0 },
                    parent: Some(i),
                    caret: None,
                    ..text.clone()
                });
            }
        }
    }
}

//! Tooltips (`El::help`), as SwiftUI's `.help`. Help elements are hit-tested on their own, so
//! they take no hover or press from anything. The pointer resting on one for `help_delay`
//! shows its tooltip under the element; a press, scroll or key hides it until the pointer
//! leaves the element, as does the pointer leaving. The tooltip draws over everything and
//! follows its element while that moves under the pointer.
//!
//! A macOS window shows AppKit's own tooltips instead (`set_native_help`): the host registers
//! [`Runtime::help_regions`] with its view and AppKit owns the delay, look and hiding.
use super::*;

#[derive(Default)]
pub(super) struct HelpState {
    /// This frame's help elements: (node, visible rect in points, `hits.len()` before it).
    hits: Vec<(NodeId, [f32; 4], usize)>,
    /// The help element under the pointer and when (clock seconds) the pointer reached it.
    target: Option<(String, f64)>,
    /// A press, scroll or key hid its tooltip until the pointer leaves it.
    dismissed: bool,
    /// The deadline already asked for, so each rest schedules one wake.
    scheduled: Option<f64>,
    /// The tooltip showing this frame: its text and box in points.
    tip: Option<(String, [f32; 4])>,
    /// The host shows system tooltips from `help_regions`.
    native: bool,
    regions: Vec<([f32; 4], String)>,
}

const PAD: [f32; 2] = [7.0, 4.0];
const GAP: f32 = 6.0;
const MAX_W: f32 = 300.0;
const MARGIN: f32 = 4.0;

impl<S: 'static> Runtime<S> {
    /// The host shows the system's own tooltips for [`Runtime::help_regions`], so Rune draws none.
    #[cfg(target_os = "macos")]
    pub(crate) fn set_native_help(&mut self, on: bool) { self.help.native = on; }

    /// Each visible help element's rect (window points, x0 y0 x1 y1) and text, in paint order,
    /// for a host that shows system tooltips.
    pub fn help_regions(&self) -> &[([f32; 4], String)] { &self.help.regions }

    /// The tooltip Rune shows now: its text and box (window points, x y w h).
    pub fn help_tooltip(&self) -> Option<(&str, [f32; 4])> {
        self.help.tip.as_ref().map(|(t, r)| (t.as_str(), *r))
    }

    /// During `build`: node `i` has help and draws visible over `rect` (points, x0 y0 x1 y1),
    /// after the first `seq` hits.
    pub(super) fn push_help_hit(&mut self, i: usize, rect: [f32; 4], seq: usize) {
        if rect[2] > rect[0] && rect[3] > rect[1] {
            self.help.hits.push((self.nodes[i].id, rect, seq));
        }
    }

    pub(super) fn clear_help_hits(&mut self) { self.help.hits.clear(); }

    /// A press, scroll or key: hide the tooltip until the pointer leaves its element.
    pub(super) fn dismiss_help(&mut self) {
        if self.help.target.is_some() {
            self.help.dismissed = true;
            self.help.tip = None;
        }
    }

    /// The help element under (x, y): the topmost one, unless something interactive drawn over
    /// it (not inside it) is there too.
    fn help_at(&self, x: f32, y: f32) -> Option<usize> {
        let &(id, _, seq) = self.help.hits.iter().rev().find(|(_, r, _)| contains(*r, x, y))?;
        let h = *self.node_index.get(&id)?;
        if self.subtree_exiting(h) || !self.allowed(h) { return None; }
        let over = self.hits.iter().enumerate().rev().find(|(_, (_, r))| contains(*r, x, y));
        if let Some((k, (top, _))) = over
            && k > seq
            && let Some(&t) = self.node_index.get(top)
            && !std::iter::successors(Some(t), |&p| self.nodes[p].parent).any(|p| p == h)
        {
            return None;
        }
        Some(h)
    }

    /// After `build`'s hits: track the rest, show or hide the tooltip and draw it.
    pub(super) fn update_help(&mut self) {
        self.help.regions.clear();
        if self.help.native {
            let regions = self.help.hits.iter().filter_map(|&(id, r, _)| {
                let i = *self.node_index.get(&id)?;
                Some((r, self.nodes[i].help.clone()?))
            }).collect();
            self.help.regions = regions;
            self.help.tip = None;
            return;
        }
        let pointer = self.pointer.filter(|&(x, y)| self.in_viewport(x, y) && self.capture.is_none() && self.secondary_capture.is_none());
        let under = pointer.and_then(|(x, y)| self.help_at(x, y));
        let path = under.map(|i| self.nodes[i].path.clone());
        if path.as_deref() != self.help.target.as_ref().map(|(p, _)| p.as_str()) {
            self.help.target = path.map(|p| (p, self.clock));
            self.help.dismissed = false;
            self.help.scheduled = None;
        }
        self.help.tip = None;
        let (Some(i), Some((_, since))) = (under, self.help.target.clone()) else { return };
        if self.help.dismissed { return; }
        let due = since + self.help_delay;
        if self.clock + 1e-6 < due {
            if self.help.scheduled != Some(due) {
                self.help.scheduled = Some(due);
                self.schedule_wakes([std::time::Duration::from_secs_f64(due - self.clock)]);
            }
            return;
        }
        let Some(text) = self.nodes[i].help.clone() else { return };
        self.draw_tooltip(i, text);
    }

    /// Draws `text` in a tooltip under node `i` where it was drawn this frame (above it when
    /// there's no room below), kept inside the window.
    fn draw_tooltip(&mut self, i: usize, text: String) {
        let s = self.env.scale;
        let dark = self.backdrop_dark().unwrap_or(self.env.system_dark);
        let grey = |v: f32| Color { r: v, g: v, b: v, a: 1.0 };
        let (fill, ink, edge) = if dark {
            (grey(0.17), grey(0.93), Color::WHITE.alpha(0.14))
        } else {
            (grey(0.98), grey(0.11), Color::BLACK.alpha(0.16))
        };
        let style = crate::TextStyle::new(12.0, 400.0).lh(16.0);
        let layout = self.text.layout_rich(&text, &[], style, s, Some(MAX_W * s));
        let (tw, th) = ((layout.full_width() / s).ceil(), (layout.height() / s).ceil());
        let (w, h) = (tw + 2.0 * PAD[0], th + 2.0 * PAD[1]);
        let e = self.drawn_box(i);
        let (vw, vh) = self.viewport;
        let mut y = e[3] + GAP;
        if y + h > vh - MARGIN && e[1] - GAP - h >= MARGIN { y = e[1] - GAP - h; }
        let x = ((e[0] + e[2] - w) * 0.5).clamp(MARGIN, (vw - w - MARGIN).max(MARGIN));
        let y = y.clamp(MARGIN, (vh - h - MARGIN).max(MARGIN));
        let r = [x * s, y * s, w * s, h * s].map(|v| v.round());
        let srgb = self.srgb_blend;
        self.instances.push(Instance {
            rect: r,
            radii: [5.0 * s; 4],
            fill: premul(fill, 1.0, srgb),
            border: premul(edge, 1.0, srgb),
            params: [s.max(1.0), KIND_RECT, 0.0, 1.0],
            uv: [0.0; 4],
            clip: NO_CLIP,
            fade: crate::render::pack_fade([0.0; 4], NO_CLIP, NO_CLIP),
        });
        let origin = [r[0] + PAD[0] * s, r[1] + PAD[1] * s];
        self.glyph_scratch.clear();
        self.text.glyphs_for_layout_scaled(&layout, s, origin, 1.0, false, &mut self.glyph_scratch);
        let gamma = glyph_gamma(ink, s, srgb);
        for q in &self.glyph_scratch {
            self.instances.extend(glyph_or_image(q, &self.text, &mut self.images, premul(ink, 1.0, srgb), gamma, NO_CLIP, [0.0; 4]));
        }
        self.help.tip = Some((text, [x, y, w, h]));
    }
}

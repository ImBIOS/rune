//! Hover and press state: which elements the pointer is over, `on_hover_change` and its
//! delays, and the hover and press tracks hover styles and press scales animate.
use super::*;

/// Hover under a pointer that hasn't moved while layout carries content under it: a row
/// expanding and the rows below sliding, a growth stack, a flight, a glide the app asked for.
/// As on macOS, where tracking areas enter and exit as the mouse moves and re-sync once layout
/// settles, hover doesn't light each row that passes: it waits for the motion to settle or the
/// pointer to move. What leaves the tree or turns inert lets go at once, and presses are never
/// held.
#[derive(Default)]
pub(super) struct HoverHold {
    /// The pointer moved or a hand scrolled while layout moved: hover follows the pointer until
    /// the motion settles.
    pub(super) released: bool,
    /// The last pass kept a hover change that moving layout would have made.
    pub(super) held: bool,
    /// Hover-style changes `update_interaction` found: node, hovered, its animation, and
    /// whether it lands now whatever the hold (a leave from an element turned inert).
    flips: Vec<(NodeId, bool, Anim, bool)>,
}

impl<S: 'static> Runtime<S> {
    pub(super) fn refresh_hover(&mut self, x: f32, y: f32) {
        if self.hover_hold.released && !self.layout_moving() { self.hover_hold.released = false; }
        self.hover_hold.held = false;
        self.refresh_hover_within();
        let mut hovered = self.primary_target(x, y);
        if hovered.map(|i| self.nodes[i].path.as_str()) == self.hovered.as_deref() { return; }
        if self.hover_may_hold() {
            let was = self.hovered.as_deref().and_then(|p| self.hover_live(p));
            if self.hover_hold.held || was.is_some_and(|i| self.layout_carries(i)) || hovered.is_some_and(|i| self.layout_carries(i)) {
                self.hover_hold.held = true;
                // What's hovered stays; one that left the tree or turned inert lets go now.
                if was.is_some() || self.hovered.is_none() { return; }
                hovered = None;
            }
        }
        self.hovered = hovered.map(|i| self.nodes[i].path.clone());
        if let Some(path) = &self.hovered { self.queue.push(Action::Hover(path.clone())); }
        // A row never takes focus from a text field (a search field above its results).
        let row = hovered.and_then(|i| self.highlight_row(i)).filter(|_| self.focused_input().is_none());
        match row {
            Some(i) if self.focused.as_deref() != Some(&self.nodes[i].path) || !self.hover_focus =>
                self.queue.push(Action::HoverFocus(Some(self.nodes[i].path.clone()))),
            None if self.hover_focus => self.queue.push(Action::HoverFocus(None)),
            _ => {}
        }
    }

    /// Queue `on_hover_change` for elements the pointer entered or left.
    pub(super) fn refresh_hover_within(&mut self) {
        if self.hover_within.is_empty() && self.hover_shown.is_empty() && self.hover_pending.is_empty()
            && !self.hits.iter().any(|(id, _)| self.node_index.get(id).is_some_and(|&i| self.nodes[i].on_hover_change.is_some()))
        {
            return;
        }
        let mut inside: Vec<String> = Vec::new();
        let mut entering: Vec<usize> = Vec::new();
        if let Some((x, y)) = self.pointer {
            for &(id, r) in &self.hits {
                if let Some((i, n)) = self.node_index.get(&id).map(|&i| (i, &self.nodes[i]))
                    && n.on_hover_change.is_some() && self.hit_contains(id, r, x, y) && !inside.contains(&n.path)
                {
                    if !self.hover_within.contains(&n.path) { entering.push(i); }
                    inside.push(n.path.clone());
                }
            }
        }
        // A held press stays inside what it started in until it releases.
        if self.capture.is_some() {
            for path in &self.hover_within {
                if !inside.contains(path) && self.nodes.iter().enumerate().any(|(i, n)| n.path == *path && self.allowed(i)) {
                    inside.push(path.clone());
                }
            }
        }
        // Layout carrying either side under a still pointer: the pointer stays inside what it
        // was until that element leaves the tree or turns inert.
        let leaving = || self.hover_within.iter().filter(|p| !inside.contains(p));
        if self.hover_may_hold() && (!entering.is_empty() || leaving().next().is_some())
            && (self.hover_hold.held || entering.iter().any(|&i| self.layout_carries(i))
                || leaving().any(|p| self.hover_live(p).is_some_and(|i| self.layout_carries(i))))
        {
            self.hover_hold.held = true;
            inside = self.hover_within.iter().filter(|p| self.hover_live(p).is_some()).cloned().collect();
        }
        let delayed = |this: &Self, path: &str| this.node(path).map(|n| n.hover_delay).filter(|d| *d != [0.0; 2]);
        for path in &self.hover_within {
            if !inside.contains(path) && delayed(self, path).is_none() { self.queue.push(Action::HoverChange(path.clone(), false)); }
        }
        for path in &inside {
            if !self.hover_within.contains(path) && delayed(self, path).is_none() { self.queue.push(Action::HoverChange(path.clone(), true)); }
        }
        self.hover_within = inside;
        self.settle_hover_delays();
    }

    /// Node `n` leaves the tree with the pointer inside it: its `on_hover_change` still hears
    /// the leave, though the handler goes with the node before the action runs.
    pub(super) fn hover_pruned(&mut self, n: &Node) {
        let Some(f) = n.on_hover_change.clone().filter(|_| n.hover_delay == [0.0; 2]) else { return };
        let Some(at) = self.hover_within.iter().position(|p| *p == n.path) else { return };
        self.hover_within.remove(at);
        self.hover_gone.push((n.path.clone(), f));
        self.queue.push(Action::HoverChange(n.path.clone(), false));
    }

    /// Starts or drops the waits of `hover_delays` elements whose inside changed.
    pub(super) fn settle_hover_delays(&mut self) {
        let mut paths: Vec<String> = self.hover_shown.clone();
        paths.extend(self.hover_pending.iter().map(|p| p.0.clone()));
        paths.extend(self.hover_within.iter().filter(|p| self.node(p).is_some_and(|n| n.hover_delay != [0.0; 2])).cloned());
        paths.sort();
        paths.dedup();
        let now = self.wall_clock().unwrap_or(self.clock);
        for path in paths {
            let Some(delay) = self.node(&path).map(|n| n.hover_delay) else {
                self.hover_shown.retain(|p| *p != path);
                self.hover_pending.retain(|p| p.0 != path);
                continue;
            };
            let shown = self.hover_shown.contains(&path);
            // A held button, or a press until the pointer next moves, never starts the wait
            // to report an enter.
            let want = self.window_focused != Some(false) && self.hover_within.contains(&path)
                && (shown || self.capture.is_none() && !self.hover_paused);
            let pending = self.hover_pending.iter().position(|p| p.0 == path);
            if want == shown {
                if let Some(i) = pending { self.hover_pending.remove(i); }
                continue;
            }
            if pending.is_some_and(|i| self.hover_pending[i].1 == want) { continue; }
            if let Some(i) = pending { self.hover_pending.remove(i); }
            let d = delay[if want { 0 } else { 1 }] as f64;
            if d <= 0.0 {
                self.report_hover(path, want);
            } else {
                self.hover_pending.push((path, want, now + d));
                self.schedule_wakes_from(now, [std::time::Duration::from_secs_f64(d)]);
            }
        }
    }

    pub(super) fn report_hover(&mut self, path: String, inside: bool) {
        self.hover_shown.retain(|p| *p != path);
        if inside { self.hover_shown.push(path.clone()); }
        self.queue.push(Action::HoverChange(path, inside));
    }

    /// Reports the delayed hover changes due by this frame.
    pub(super) fn fire_hover_delays(&mut self) {
        if self.hover_pending.is_empty() { return; }
        let clock = self.clock + 1e-6;
        let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.hover_pending).into_iter().partition(|p| p.2 <= clock);
        self.hover_pending = rest;
        for (path, inside, _) in due { self.report_hover(path, inside); }
    }

    /// The pointer moved or a hand scrolled: hover follows the pointer through the layout
    /// moving now, until it settles.
    pub(super) fn release_hover_hold(&mut self) {
        if !self.hover_hold.released && self.layout_moving() { self.hover_hold.released = true; }
        self.hover_hold.held = false;
    }

    /// Hover may wait on moving layout: the pointer is in the window, holds no press, and
    /// hasn't moved since the motion started.
    fn hover_may_hold(&self) -> bool {
        self.pointer.is_some() && self.capture.is_none() && !self.hover_hold.released
    }

    /// Element `i` is carried by moving layout: a frame, offset or flight spring on it or an
    /// ancestor, a growth stack moving its rows, or a viewport gliding where the app sent it or
    /// letting rows down. A hand's scroll and live transforms aren't layout motion. Asked only
    /// of elements whose hover would change, so a frame at rest walks nothing.
    fn layout_carries(&self, i: usize) -> bool {
        let store = self.scene.store();
        let mut at = Some(i);
        while let Some(k) = at {
            let id = self.nodes[k].id;
            if store.is_animating(id, prop::FRAME) || store.is_animating(id, prop::OFFSET) || store.is_animating(id, prop::TRAVEL)
                || self.stacks.get(&id).is_some_and(|s| s.moving) || self.grows.get(&id).is_some_and(|g| g.moving())
                || self.scrolls.get(&id).is_some_and(|s| s.glide.is_some() || s.x.glide.is_some() || s.slack.moving())
            {
                return true;
            }
            at = self.nodes[k].parent;
        }
        false
    }

    /// Any layout moving at all: the running stacks, glides, flights and spring tracks, not the
    /// tree. Asked only when a hand moves and while hover follows it.
    fn layout_moving(&self) -> bool {
        let store = self.scene.store();
        let layout = [prop::FRAME.id, prop::OFFSET.id, prop::TRAVEL.id];
        !self.flying.is_empty()
            || self.stacks.values().any(|s| s.moving)
            || self.scrolls.values().any(|s| s.glide.is_some() || s.x.glide.is_some() || s.slack.moving())
            || store.touched_tracks().any(|(t, _, p)| layout.contains(&p) && store.is_active(t))
    }

    /// The element at `path`, while it's in the tree, not leaving it, and not inert.
    fn hover_live(&self, path: &str) -> Option<usize> {
        (0..self.nodes.len())
            .find(|&i| self.nodes[i].path == path && !self.subtree_exiting(i) && !self.subtree_unmounted(i))
            .filter(|&i| self.allowed(i))
    }

    pub(super) fn update_interaction(&mut self) -> bool {
        let mut changed = false;
        let may_hold = self.hover_may_hold();
        let mut wait = may_hold && self.hover_hold.held;
        let mut flips = std::mem::take(&mut self.hover_hold.flips);
        let captured = self.capture.as_ref().map(|(p, _)| p.as_str()).or(self.secondary_capture.as_deref());
        for i in 0..self.nodes.len() {
            let n = &self.nodes[i];
            let tracks_hover = n.vis.hover.a > 0.0 || n.vis.hover_group;
            if self.is_exiting(n.id) || (!tracks_hover && n.vis.press.a <= 0.0 && n.press_scale.is_none()) {
                continue;
            }
            let inside = self.pointer.is_some_and(|(x, y)| self.node_contains(i, x, y));
            // A group stays hovered while one of its descendants holds the press.
            let within = |p: &str| p == n.path || (p.len() > n.path.len() && p.starts_with(&n.path) && p.as_bytes()[n.path.len()] == b'/');
            let capture_inside = if self.secondary_capture.is_some() { inside } else { self.capture_inside };
            let held = captured == Some(n.path.as_str()) && capture_inside;
            let allowed = self.allowed(i);
            // A held press stays hovered while it's inside, though a press scale shrank the
            // drawn rect away from the pointer.
            let hovered = allowed && (inside || held) && tracks_hover && captured.is_none_or(within);
            let pressed = allowed && held && n.vis.press.a > 0.0;
            let press_scale = n.press_scale;
            let id = n.id;
            // Like a native sidebar or menu: the highlight lands with the pointer and
            // fades briefly as it leaves.
            let hover_anim = match n.vis.hover_motion {
                Some((enter, exit)) => if hovered { enter } else { exit },
                None if hovered => Anim::INSTANT,
                None => rune_motion::Curve::tween(0.1, rune_motion::Easing::EASE_OUT).into(),
            };
            if self.scene.store().target(id, P_HOVER).unwrap_or(0.0) != hovered as u8 as f32 {
                // A leave from an element turned inert lands now; the rest wait while layout
                // carries one of them under a still pointer.
                let now = !hovered && !allowed;
                wait |= may_hold && !now && !wait && self.layout_carries(i);
                flips.push((id, hovered, hover_anim, now));
            } else {
                self.scene.animate(id, P_HOVER, hovered as u8 as f32, hover_anim);
            }
            changed |= self.scene.store().target(id, P_PRESS).unwrap_or(0.0) != pressed as u8 as f32;
            self.scene.animate(id, P_PRESS, pressed as u8 as f32, if pressed { motion::INSTANT } else { motion::SNAPPY });
            if let Some(k) = press_scale {
                // Down follows the hand; the release springs back with a little life.
                let down = allowed && held && !self.reduced_motion;
                self.scene.animate(id, P_PRESS_SCALE, if down { k } else { 1.0 }, if down { motion::INSTANT } else { motion::POP });
            }
        }
        for &(id, hovered, anim, now) in &flips {
            if wait && !now { continue; }
            self.scene.animate(id, P_HOVER, hovered as u8 as f32, anim);
            changed = true;
        }
        flips.clear();
        self.hover_hold.flips = flips;
        self.hover_hold.held = wait;
        changed
    }
}

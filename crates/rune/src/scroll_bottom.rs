//! Where a scroll viewport stands against its bottom: `El::on_at_bottom` reports changes, and
//! `Tx::scroll_to_bottom` glides there (a chat's "jump to latest").
use super::*;

/// The glide's spring. It retargets each frame, so content growing under it extends the trip.
const GLIDE: Spring = motion::GLIDE;
/// A glide starts at most this many viewports above the bottom (a longer trip cuts first).
const GLIDE_REACH: f64 = 2.0;
/// Within this many points of the bottom a viewport counts as showing it.
const AT_BOTTOM: f64 = 1.0;

/// Steps a glide toward the current bottom. Returns whether it is still moving.
pub(super) fn glide(st: &mut ScrollState, v: f32, dt: f32) -> bool {
    // Sample the displacement, not the absolute offset: f32 loses sub-point precision far down.
    let (d, v) = GLIDE.sample((st.offset - st.max) as f32, v, 0.0, dt);
    if d.abs() < 0.5 && v.abs() < 20.0 {
        st.offset = st.max;
        st.glide = None;
        return false;
    }
    st.offset = st.max + f64::from(d);
    st.glide = Some(v);
    true
}

impl<S: 'static> Runtime<S> {
    /// The scroll viewport keyed (or pathed) `key`, or the nearest one containing it.
    fn scroll_viewport(&self, key: &str) -> Option<usize> {
        let mut i = self.nodes.iter().enumerate()
            .position(|(i, n)| (n.key.as_deref() == Some(key) || n.path == key) && !self.subtree_exiting(i))?;
        loop {
            if self.nodes[i].scroll.is_some() { return Some(i); }
            i = self.nodes[i].parent?;
        }
    }

    pub(super) fn scroll_to_bottom(&mut self, requests: Vec<(String, bool)>) {
        // This frame's commit may have changed the content: measure the extent first.
        self.step_scroll(0.0);
        let mut targets = Vec::new();
        for (key, animated) in requests {
            let Some(i) = self.scroll_viewport(&key) else { continue };
            let (id, sticky, view_h) = (self.nodes[i].id, self.nodes[i].scroll.is_some_and(|s| s.stick_bottom), f64::from(self.nodes[i].local.h));
            let Some(st) = self.scrolls.get_mut(&id) else { continue };
            targets.push((id, animated, sticky, view_h, st.offset, st.glide));
            // Stand at the bottom first so a virtual list mounts and measures its tail rows,
            // which can differ from the estimate: the trip below then ends at the true bottom.
            st.offset = st.max;
            st.pinned = true;
            st.glide = None;
            st.v = 0.0;
        }
        for _ in 0..3 {
            if !self.virtual_window_changed() { break; }
            self.commit(Tx::default());
            self.step_scroll(0.0);
            for (id, ..) in &targets {
                if let Some(st) = self.scrolls.get_mut(id) { st.offset = st.max; }
            }
        }
        for (id, animated, sticky, view_h, start, glide) in targets {
            let Some(st) = self.scrolls.get_mut(&id) else { continue };
            if animated && !self.reduced_motion && st.max - start > 0.5 {
                st.offset = start.max(st.max - GLIDE_REACH * view_h);
                st.glide = Some(glide.unwrap_or(0.0));
                st.pinned = false;
            } else {
                st.offset = st.max;
                st.pinned = sticky;
            }
        }
        if self.virtual_window_changed() {
            self.commit(Tx::default());
            self.step_scroll(0.0);
        }
    }

    /// Queues `on_at_bottom` for viewports whose at-bottom state changed this frame.
    pub(super) fn report_at_bottom(&mut self) {
        for i in 0..self.nodes.len() {
            if self.nodes[i].on_at_bottom.is_none() || self.subtree_exiting(i) { continue; }
            let n = &self.nodes[i];
            let Some(st) = self.scrolls.get_mut(&n.id) else { continue };
            let at = st.offset >= st.max - AT_BOTTOM;
            if st.at_bottom != Some(at) {
                st.at_bottom = Some(at);
                self.queue.push(Action::AtBottom(n.path.clone(), at));
            }
        }
    }

    /// Whether the scroll viewport keyed `key` (or the one containing it) shows its bottom,
    /// as of the last frame.
    pub fn scroll_at_bottom(&self, key: &str) -> Option<bool> {
        let i = self.scroll_viewport(key)?;
        self.scrolls.get(&self.nodes[i].id).map(|st| st.offset >= st.max - AT_BOTTOM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::el::{col, el};

    #[derive(Default)]
    struct Chat { rows: usize, reports: Vec<bool>, instant: bool, reduced: bool, virtualized: bool, loose: bool, tall_tail: bool }

    fn view(s: &Chat, cx: &mut Cx<Chat>) -> El {
        cx.reduced_motion(s.reduced);
        let mut stack = col().key("stack").w_full().growth(GrowthSpec::default());
        if s.virtualized { stack = stack.virtualize(60.0); }
        for i in 0..s.rows {
            let h = if s.tall_tail && i + 10 >= s.rows { 150.0 } else { 40.0 + (i % 3) as f32 * 10.0 };
            stack = stack.child(el().key(format!("r{i}")).w_full().h(h));
        }
        col().w_full().h_full()
            .child(col().key("scroll").w_full().grow(1.0).shrink().scroll(ScrollSpec { stick_bottom: !s.loose })
                .on_at_bottom(|s: &mut Chat, at| s.reports.push(at))
                .child(stack))
            .child(el().key("jump").w_full().h(40.0).on_press_tx(|s: &mut Chat, tx| {
                if s.instant { tx.scroll_to_bottom_instant("stack") } else { tx.scroll_to_bottom("scroll") }
            }))
    }

    fn chat(state: Chat) -> Runtime<Chat> {
        let mut rt = Runtime::new(state, view, (200.0, 340.0), Env::new(true, 2.0));
        rt.frame(0.0);
        rt.frame(1.0 / 120.0);
        rt
    }

    fn press(rt: &mut Runtime<Chat>, key: &str) {
        let (x, y) = rt.find(key).unwrap();
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
    }

    fn bottom_gap(rt: &Runtime<Chat>) -> f32 {
        let last = rt.elements().into_iter().find(|e| e.key.as_deref() == Some(&*format!("r{}", rt.state.rows - 1))).unwrap();
        300.0 - (last.rect[1] + last.rect[3])
    }

    #[test]
    fn at_bottom_reports_once_per_change_and_glides_back() {
        let mut rt = chat(Chat { rows: 40, ..Default::default() });
        assert_eq!(rt.state.reports, [true], "the first layout reports its state");
        assert_eq!(rt.scroll_at_bottom("scroll"), Some(true));
        for _ in 0..5 {
            rt.wheel(100.0, 100.0, 300.0);
            rt.frame(1.0 / 120.0);
        }
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.state.reports, [true, false], "scrolling further up does not report again");
        let commits = rt.stats.commits;
        for _ in 0..30 { rt.frame(1.0 / 120.0); }
        assert_eq!(rt.stats.commits, commits, "no per-frame callbacks or commits at rest");
        assert!(bottom_gap(&rt) < -1000.0, "reading far up: {}", bottom_gap(&rt));

        press(&mut rt, "jump");
        rt.frame(1.0 / 120.0);
        let start = bottom_gap(&rt);
        assert!((-600.0..0.0).contains(&start), "a long trip cuts to two viewports from the end: {start}");
        let mut previous = start;
        let mut frames = 0;
        while rt.state.reports.len() < 3 && frames < 240 {
            rt.frame(1.0 / 120.0);
            let gap = bottom_gap(&rt);
            assert!(gap >= previous - 0.01, "the glide only moves toward the bottom: {previous} -> {gap}");
            previous = gap;
            frames += 1;
        }
        assert!(frames > 10, "the glide is animated ({frames} frames)");
        assert_eq!(rt.state.reports, [true, false, true]);
        for _ in 0..60 { rt.frame(1.0 / 120.0); }
        assert!(bottom_gap(&rt).abs() < 0.05, "{}", bottom_gap(&rt));
        assert!(!rt.animating);
        rt.state.rows += 1;
        rt.invalidate();
        for _ in 0..120 { rt.frame(1.0 / 120.0); }
        assert!(bottom_gap(&rt).abs() < 0.05, "a sticky viewport follows the end again after landing");
        assert_eq!(rt.state.reports, [true, false, true]);
        assert!(!rt.animating);
    }

    #[test]
    fn wheel_interrupts_a_glide() {
        let mut rt = chat(Chat { rows: 40, ..Default::default() });
        rt.wheel(100.0, 100.0, 400.0);
        rt.frame(1.0 / 120.0);
        press(&mut rt, "jump");
        for _ in 0..4 { rt.frame(1.0 / 120.0); }
        rt.wheel(100.0, 100.0, 50.0);
        rt.frame(1.0 / 120.0);
        let held = bottom_gap(&rt);
        for _ in 0..60 { rt.frame(1.0 / 120.0); }
        assert_eq!(bottom_gap(&rt), held, "the hand takes over from the glide");
        assert_eq!(rt.scroll_at_bottom("r3"), Some(false), "a row finds its viewport");
    }

    #[test]
    fn instant_and_reduced_motion_land_in_the_same_frame() {
        for state in [Chat { rows: 40, instant: true, ..Default::default() }, Chat { rows: 40, reduced: true, ..Default::default() }] {
            let mut rt = chat(state);
            rt.wheel(100.0, 100.0, 800.0);
            rt.frame(1.0 / 120.0);
            press(&mut rt, "jump");
            rt.frame(1.0 / 120.0);
            assert!(bottom_gap(&rt).abs() < 0.05, "{}", bottom_gap(&rt));
            rt.frame(1.0 / 120.0);
            assert_eq!(rt.state.reports, [true, false, true]);
        }
    }

    #[test]
    fn glides_to_the_exact_bottom_of_a_virtual_list() {
        let mut rt = chat(Chat { rows: 600, virtualized: true, ..Default::default() });
        for _ in 0..40 {
            rt.wheel(100.0, 100.0, 2000.0);
            rt.frame(1.0 / 120.0);
        }
        assert!(rt.nodes.iter().all(|n| n.key.as_deref() != Some("r599")), "the tail is unmounted while reading the top");
        press(&mut rt, "jump");
        for _ in 0..240 { rt.frame(1.0 / 120.0); }
        assert!(bottom_gap(&rt).abs() < 0.05, "{}", bottom_gap(&rt));
        assert_eq!(rt.state.reports.last(), Some(&true));
        assert!(!rt.animating);
    }

    fn max(rt: &Runtime<Chat>) -> f64 {
        let i = rt.scroll_viewport("scroll").unwrap();
        rt.scrolls[&rt.nodes[i].id].max
    }

    /// Reads far up a virtual list whose unmounted tail rows are much taller than the estimate.
    fn far_from_a_tall_tail(state: Chat) -> Runtime<Chat> {
        let mut rt = chat(Chat { rows: 600, virtualized: true, tall_tail: true, ..state });
        for _ in 0..40 {
            rt.wheel(100.0, 100.0, 2000.0);
            rt.frame(1.0 / 120.0);
        }
        for _ in 0..30 { rt.frame(1.0 / 120.0); }
        assert!(rt.nodes.iter().all(|n| n.key.as_deref() != Some("r599")), "the tail is unmounted");
        rt
    }

    #[test]
    fn instant_lands_on_an_unmeasured_virtual_tail() {
        for loose in [true, false] {
            let mut rt = far_from_a_tall_tail(Chat { instant: true, loose, ..Default::default() });
            press(&mut rt, "jump");
            rt.frame(1.0 / 120.0);
            assert!(bottom_gap(&rt).abs() < 0.05, "loose {loose}: {}", bottom_gap(&rt));
            for _ in 0..10 { rt.frame(1.0 / 120.0); }
            assert!(bottom_gap(&rt).abs() < 0.05, "loose {loose}: stays at the bottom: {}", bottom_gap(&rt));
        }
    }

    #[test]
    fn a_glide_measures_the_virtual_tail_before_it_starts() {
        for loose in [true, false] {
            let mut rt = far_from_a_tall_tail(Chat { loose, ..Default::default() });
            press(&mut rt, "jump");
            rt.frame(1.0 / 120.0);
            let trip = max(&rt);
            assert!(bottom_gap(&rt) >= -2.0 * 300.0 - 0.5, "loose {loose}: starts two viewports from the true bottom: {}", bottom_gap(&rt));
            let mut frames = 0;
            while rt.animating && frames < 240 {
                rt.frame(1.0 / 120.0);
                assert_eq!(max(&rt), trip, "loose {loose}: the bottom holds still mid-glide (frame {frames})");
                frames += 1;
            }
            assert!(frames > 10, "loose {loose}: animated ({frames} frames)");
            assert!(bottom_gap(&rt).abs() < 0.05, "loose {loose}: {}", bottom_gap(&rt));
        }
    }
}

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
            if self.nodes[i].scroll.is_some() && self.nodes[i].scroll_axes.y() { return Some(i); }
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
            self.commit(&Tx::default());
            self.step_scroll(0.0);
            for (id, ..) in &targets {
                if let Some(st) = self.scrolls.get_mut(id) { st.offset = st.max; }
            }
        }
        for (id, animated, sticky, view_h, start, glide) in targets {
            let Some(st) = self.scrolls.get_mut(&id) else { continue };
            // A settled frame (a window showing or uncovering) lands at the end.
            if animated && !self.reduced_motion && !self.settled && st.max - start > 0.5 {
                st.offset = start.max(st.max - GLIDE_REACH * view_h);
                st.glide = Some(glide.unwrap_or(0.0));
                st.pinned = false;
            } else {
                st.offset = st.max;
                st.pinned = sticky;
            }
        }
        if self.virtual_window_changed() {
            self.commit(&Tx::default());
            self.step_scroll(0.0);
        }
    }

    /// `Tx::scroll_page`: each viewport moves by its requested pages.
    pub(super) fn scroll_pages(&mut self, requests: Vec<(String, i32)>) {
        // This frame's commit may have changed the content: measure the extent first.
        self.step_scroll(0.0);
        for (key, pages) in requests {
            let Some(i) = self.scroll_viewport(&key) else { continue };
            self.page_to(i, |offset, _, page| offset + page * f64::from(pages));
        }
        if self.virtual_window_changed() {
            self.commit(&Tx::default());
            self.step_scroll(0.0);
        }
    }

    /// The offset to draw stuck-to-bottom viewport `i` at, whose presented bottom edge is
    /// `bottom`: layout's offset keeps the content's target bottom on the viewport's target
    /// bottom, but while either is still animating (the viewport resizing under a banner)
    /// that would jump the rows and slide them back. This keeps the presented edges together.
    ///
    /// Rows that sat on the bottom hold it as the content crosses between overflowing and
    /// fitting: while they still end on the bottom (`justify_end`), or until the content's top
    /// would carry them above it, after which they ride the content's top. The offset goes
    /// negative while fitting rows sit low in a box that has not shrunk yet.
    pub(super) fn stuck_offset(&self, i: usize, bottom: f32) -> Option<f64> {
        let n = &self.nodes[i];
        let st = self.scrolls.get(&n.id).filter(|st| st.pinned)?;
        if !n.scroll.is_some_and(|s| s.stick_bottom) { return None; }
        let c = self.scroll_content(i)?;
        let lag = self.stuck_lag(i, c, bottom)?;
        // How far the rows end above the bottom in the layout (negative past it), beside the
        // stack's own bottom padding and border.
        let pad = self.stacks.get(&self.nodes[c].id).map_or(0.0, |s| (s.pad_bottom - self.nodes[c].inset[3]).max(0.0));
        let short = f64::from(n.target[3] - (self.nodes[c].target[3] - pad));
        if st.held && (short <= 0.5 || lag - short >= -0.5) {
            Some(self.scroll_overflow(i) + lag)
        } else if st.max > 0.0 {
            Some((st.max + lag).max(0.0))
        } else {
            None
        }
    }

    /// The offset that draws viewport `i`'s content bottom on its presented bottom `bottom`.
    pub(super) fn bottom_offset(&self, i: usize, bottom: f32) -> Option<f64> {
        let c = self.scroll_content(i)?;
        Some(self.scroll_overflow(i) + self.stuck_lag(i, c, bottom)?)
    }

    /// Scroll viewport `i`'s content: its first child.
    pub(super) fn scroll_content(&self, i: usize) -> Option<usize> {
        (i + 1..self.nodes.len()).find(|&j| self.nodes[j].parent == Some(i))
    }

    /// How much further content `c` of viewport `i` has to travel to its layout than the
    /// viewport's presented bottom `bottom` has.
    fn stuck_lag(&self, i: usize, c: usize, bottom: f32) -> Option<f64> {
        let top = self.scene.presented(self.nodes[c].id)?.rect.edges()[1];
        Some(f64::from((top - self.nodes[c].target[1]) - (bottom - self.nodes[i].target[3])))
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

    // One state type for every test here: each type monomorphises a whole `Runtime<S>`.
    #[derive(Default)]
    struct Chat { rows: usize, reports: Vec<bool>, instant: bool, reduced: bool, virtualized: bool, loose: bool, tall_tail: bool, composer: Option<crate::TextEdit> }

    fn view(s: &Chat, cx: &mut Cx<Chat>) -> El {
        cx.reduced_motion(s.reduced);
        let mut stack = col().key("stack").w_full().growth(GrowthSpec::default());
        if s.virtualized { stack = stack.virtualize(60.0); }
        for i in 0..s.rows {
            let h = if s.tall_tail && i + 10 >= s.rows { 150.0 } else { 40.0 + (i % 3) as f32 * 10.0 };
            stack = stack.child(el().key(format!("r{i}")).w_full().h(h));
        }
        let root = col().w_full().h_full()
            .child(col().key("scroll").w_full().grow(1.0).shrink().scroll(ScrollSpec { stick_bottom: !s.loose })
                .on_at_bottom(|s: &mut Chat, at| s.reports.push(at))
                .child(stack))
            .child(el().key("jump").w_full().h(40.0).on_press_tx(|s: &mut Chat, tx| {
                if s.instant { tx.scroll_to_bottom_instant("stack") } else { tx.scroll_to_bottom("scroll") }
            }));
        // A multi-line composer below the list, in its own small scroll area.
        let Some(edit) = &s.composer else { return root };
        let field = crate::text_input(edit, crate::TextStyle::new(14.0, 400.0)).key("composer").w_full()
            .on_edit(|s: &mut Chat| s.composer.as_mut().unwrap());
        root.child(col().key("composer-scroll").w_full().h(60.0).scroll(ScrollSpec { stick_bottom: false }).child(field))
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

    /// A scroll to the bottom asked for while the window is covered (or hidden) waits for the
    /// frame that uncovers it, which settles: that frame shows the bottom, with no glide after.
    /// (Settling already pins a sticky viewport; this one isn't.)
    #[test]
    fn a_scroll_asked_while_covered_lands_when_it_uncovers() {
        let mut rt = chat(Chat { rows: 40, loose: true, ..Default::default() });
        for _ in 0..5 {
            rt.wheel(100.0, 100.0, 300.0);
            rt.frame(1.0 / 120.0);
        }
        for _ in 0..60 { rt.frame(1.0 / 120.0); }
        assert!(bottom_gap(&rt) < -1000.0, "reading far up: {}", bottom_gap(&rt));
        rt.set_live_visible(false);
        rt.update(|_, tx| tx.scroll_to_bottom("scroll"));
        rt.hidden_wake();
        assert!(bottom_gap(&rt) < -1000.0, "nothing moves while covered");
        rt.set_live_visible(true);
        rt.settle_all();
        rt.frame(1.0 / 120.0);
        assert!(bottom_gap(&rt).abs() < 0.05, "the uncovering frame shows the bottom: {}", bottom_gap(&rt));
        assert!(!rt.animating, "with no glide left to play");
    }

    #[test]
    fn an_update_that_changes_nothing_still_scrolls() {
        let mut rt = chat(Chat { rows: 40, instant: true, ..Default::default() });
        for _ in 0..5 {
            rt.wheel(100.0, 100.0, 300.0);
            rt.frame(1.0 / 120.0);
        }
        for _ in 0..60 { rt.frame(1.0 / 120.0); }
        assert_eq!(rt.scroll_at_bottom("scroll"), Some(false));
        assert!(rt.update_if(|_, tx| { tx.scroll_to_bottom_instant("stack"); false }), "a scroll takes a frame");
        assert!(rt.stale());
        for _ in 0..3 { rt.frame(1.0 / 120.0); }
        assert_eq!(rt.scroll_at_bottom("scroll"), Some(true));
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

    /// A hidden host's poll that scrolls changes no state, yet the last frame no longer shows
    /// the window: a launcher's cached reveal must refresh. A poll that only schedules a wake
    /// leaves it fresh.
    #[test]
    fn a_hidden_poll_that_scrolls_leaves_the_last_frame_out_of_date() {
        let mut rt = chat(Chat { rows: 40, ..Default::default() });
        rt.wheel(100.0, 100.0, 800.0);
        for _ in 0..30 { rt.frame(1.0 / 120.0); }
        let up = bottom_gap(&rt);
        assert!(up < -100.0, "{up}");
        rt.poll = Some(|s: &mut Chat, _, tx: &mut Tx| {
            tx.wake_after(std::time::Duration::from_secs(1));
            if std::mem::take(&mut s.instant) { tx.scroll_to_bottom_instant("stack"); }
            false
        });
        assert!(!rt.poll_hidden(), "a wake alone draws nothing new");
        assert!(!rt.needs_commit());
        rt.state.instant = true;
        assert!(rt.poll_hidden(), "a scroll asked for while hidden schedules a hidden refresh");
        assert!(rt.needs_commit(), "and the cached frame isn't fresh");
        assert_eq!(bottom_gap(&rt), up, "nothing scrolled while hidden");
        rt.frame(0.0);
        assert!(bottom_gap(&rt).abs() < 0.05, "{}", bottom_gap(&rt));
        assert!(!rt.needs_commit());
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

    fn scroll_state<'a>(rt: &'a Runtime<Chat>, key: &str) -> &'a ScrollState {
        let i = rt.scroll_viewport(key).unwrap();
        &rt.scrolls[&rt.nodes[i].id]
    }

    fn page_height(rt: &Runtime<Chat>, key: &str) -> f64 {
        f64::from(rt.nodes[rt.scroll_viewport(key).unwrap()].local.h * 0.9)
    }

    fn key(rt: &mut Runtime<Chat>, key: crate::Key) {
        rt.key(KeyEvent { key, mods: Default::default() });
        rt.frame(1.0 / 120.0);
    }

    fn with_composer(text: &str) -> Runtime<Chat> {
        let mut edit = crate::TextEdit::new(text);
        edit.focused = true;
        chat(Chat { rows: 40, composer: Some(edit), ..Default::default() })
    }

    #[test]
    fn page_keys_from_a_short_composer_page_the_message_list_beside_it() {
        let mut rt = with_composer("hello");
        assert_eq!(scroll_state(&rt, "composer-scroll").max, 0.0, "the draft fits its scroll area");
        let (bottom, page) = (max(&rt), page_height(&rt, "scroll"));
        assert!(bottom > 3.0 * page);
        assert_eq!(scroll_state(&rt, "scroll").offset, bottom);
        key(&mut rt, crate::Key::PageUp);
        assert_eq!(scroll_state(&rt, "scroll").offset, bottom - page, "the list pages, not the composer");
        assert!(!scroll_state(&rt, "scroll").pinned);
        // The pointer over the composer doesn't claim the keys either.
        let (x, y) = rt.find("composer").unwrap();
        rt.pointer_move(x, y);
        key(&mut rt, crate::Key::PageUp);
        assert_eq!(scroll_state(&rt, "scroll").offset, bottom - 2.0 * page);
        key(&mut rt, crate::Key::PageDown);
        key(&mut rt, crate::Key::PageDown);
        assert_eq!(scroll_state(&rt, "scroll").offset, bottom);
        assert!(scroll_state(&rt, "scroll").pinned, "back on the end, the list follows it again");
        let composer = rt.state.composer.as_ref().unwrap();
        assert!(composer.focused);
        assert_eq!(composer.text, "hello");
    }

    #[test]
    fn an_overflowing_composer_keeps_the_page_keys_even_at_its_edge() {
        let mut rt = with_composer(&vec!["line"; 12].join("\n"));
        let list = max(&rt);
        let composer = scroll_state(&rt, "composer-scroll").max;
        assert!(composer > 0.0, "the draft overflows: {composer}");
        for _ in 0..10 { key(&mut rt, crate::Key::PageUp); }
        assert_eq!(scroll_state(&rt, "composer-scroll").offset, 0.0, "the composer paged to its top");
        assert_eq!(scroll_state(&rt, "scroll").offset, list, "and the list never moved, as an NSTextView keeps them");
    }

    #[test]
    fn tx_scroll_page_pages_a_viewport_and_repins_its_end() {
        let mut rt = chat(Chat { rows: 40, ..Default::default() });
        let (bottom, page) = (max(&rt), page_height(&rt, "scroll"));
        assert!(rt.update_if(|_, tx| { tx.scroll_page("scroll", -2); false }), "a page takes a frame");
        rt.frame(1.0 / 120.0);
        assert_eq!(scroll_state(&rt, "scroll").offset, bottom - 2.0 * page);
        assert_eq!(rt.scroll_at_bottom("scroll"), Some(false));
        let row = rt.elements().into_iter().find(|e| e.rect[1] > 0.0 && e.rect[1] < 200.0 && e.key.as_deref().is_some_and(|k| k.starts_with('r'))).unwrap().key.unwrap();
        rt.update_if(move |_, tx| { tx.scroll_page(row, 1); false });
        rt.frame(1.0 / 120.0);
        assert_eq!(scroll_state(&rt, "scroll").offset, bottom - page, "a row's key pages the viewport it's in");
        rt.update_if(|_, tx| { tx.scroll_page("scroll", 5); false });
        rt.frame(1.0 / 120.0);
        assert_eq!(scroll_state(&rt, "scroll").offset, bottom, "clamped at the end");
        assert!(scroll_state(&rt, "scroll").pinned);
        rt.state.rows += 5;
        rt.invalidate();
        for _ in 0..120 { rt.frame(1.0 / 120.0); }
        assert!(bottom_gap(&rt).abs() < 0.05, "a stick_bottom list follows new rows again: {}", bottom_gap(&rt));
        assert!(!rt.animating);
    }

    #[test]
    fn tx_scroll_page_finds_the_list_of_a_key_inside_a_virtual_row() {
        let mut rt = chat(Chat { rows: 600, virtualized: true, ..Default::default() });
        let (bottom, page) = (max(&rt), page_height(&rt, "scroll"));
        assert!(rt.nodes.iter().all(|n| n.key.as_deref() != Some("r0")), "the head is unmounted");
        rt.update_if(|_, tx| { tx.scroll_page("r599", -3); false });
        rt.frame(1.0 / 120.0);
        // Rows mounting on the way measure against their estimate, which moves the extent a little.
        let st = scroll_state(&rt, "scroll");
        let pages = (st.max - st.offset) / page;
        assert!((2.5..3.5).contains(&pages), "three pages above the end: {pages} (bottom was {bottom})");
        assert!(!scroll_state(&rt, "scroll").pinned);
        let shown = rt.elements().into_iter().filter(|e| e.key.as_deref().is_some_and(|k| k.starts_with('r')) && e.rect[1] + e.rect[3] > 0.0 && e.rect[1] < 300.0).count();
        assert!(shown >= 5, "the window mounted the rows it pages to ({shown})");
    }
}


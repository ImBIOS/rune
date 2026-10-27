//! Pointer selection in `text_input`: single-click caret placement, click-and-drag character
//! selection, double-click word selection (and word-by-word drag), triple-click line/paragraph
//! selection (and line-by-line drag), and Shift-click selection extension.
//!
//! Word and paragraph boundaries and drag extension share `selectable`'s `Unit`, `unit_at` and
//! `extend`.
use std::collections::VecDeque;

use super::selectable::{Unit, extend};
use super::*;

#[derive(Clone, Copy, Debug)]
enum Step {
    Down { clicks: u8, shift: bool },
    Drag,
}

#[derive(Default)]
pub(super) struct InputClick {
    /// The last non-Shift press on an input: `(path, time, window_x, window_y, clicks)`.
    last: Option<(String, f64, f32, f32, u8)>,
    /// What the press that started the current selection selects by, and the unit range it
    /// began on, which a drag keeps whole.
    unit: Unit,
    origin: (usize, usize),
    /// The input path whose held press is currently dragging a selection.
    dragging: Option<String>,
    /// Queued press/drag steps paired with `Action::Focus` entries in `Runtime::queue`.
    steps: VecDeque<(String, Step)>,
}

impl InputClick {
    pub(super) fn last_path(&self) -> Option<&str> {
        self.last.as_ref().map(|(p, ..)| p.as_str())
    }

    pub(super) fn clear(&mut self) {
        self.last = None;
        self.dragging = None;
    }

    pub(super) fn clear_last(&mut self) {
        self.last = None;
    }

    pub(super) fn stop_drag(&mut self) {
        self.dragging = None;
    }
}

fn hit_index(e: &crate::edit::TextEdit, l: &Layout<Ink>, x: f32, y: f32, s: f32, single: bool) -> usize {
    let h = (l.height() - 1e-3).max(0.0);
    let ys = if single || x < 0.0 { (y * s).clamp(0.0, h) } else { (y * s).max(0.0) };
    e.model_index(crate::text::cursor_from_point(l, x * s, ys).index())
}

impl<S: 'static> Runtime<S> {
    pub(super) fn input_down(&mut self, ji: usize, x: f32, y: f32) {
        let now = self.now_clock();
        let shift = self.held_mods.shift;
        // Its text drew scaled or rotated with its box: map the press back to the rest layout.
        let [lx, ly] = self.to_local(ji, x, y);
        let jn = &self.nodes[ji];
        let path = jn.path.clone();
        let (ix, iy) = (lx - jn.inset[0], ly - jn.inset[1]);
        let r = jn.target;
        let clicks = if shift {
            self.input_click.last = None;
            1
        } else {
            let n = match &self.input_click.last {
                Some((p, t, px, py, n))
                    if *p == path
                        && now - t <= self.double_click_interval
                        && (x - px).abs() <= CLICK_SLOP
                        && (y - py).abs() <= CLICK_SLOP =>
                {
                    (n + 1).min(3)
                }
                _ => 1,
            };
            self.input_click.last = Some((path.clone(), now, x, y, n));
            n
        };
        self.input_click.dragging = Some(path.clone());
        self.input_click.steps.push_back((path.clone(), Step::Down { clicks, shift }));
        self.queue.push(Action::Focus(path.clone(), ix, iy));
        let d = Drag {
            phase: Phase::Down,
            x: lx,
            y: ly,
            w: r[2] - r[0],
            h: r[3] - r[1],
            start_x: lx,
            start_y: ly,
            dx: 0.0,
            dy: 0.0,
            data: 0.0,
            cancelled: false,
        };
        self.capture = Some((path, d));
        self.capture_button = crate::PointerButton::Primary;
        self.capture_inside = true;
    }

    pub(super) fn input_drag(&mut self, x: f32, y: f32, drag_move: bool) {
        if !drag_move {
            return;
        }
        let Some(path) = self.input_click.dragging.clone() else { return };
        if (x - self.press_at.0).abs() > CLICK_SLOP || (y - self.press_at.1).abs() > CLICK_SLOP {
            self.input_click.last = None;
        }
        let Some(ji) = self.nodes.iter().enumerate().position(|(i, n)| n.path == path && !self.subtree_exiting(i)) else {
            return;
        };
        let [lx, ly] = self.to_local(ji, x, y);
        let jn = &self.nodes[ji];
        let (ix, iy) = (lx - jn.inset[0], ly - jn.inset[1]);
        self.input_click.steps.push_back((path.clone(), Step::Drag));
        self.queue.push(Action::Focus(path, ix, iy));
    }

    pub(super) fn focus_input(&mut self, path: String, x: f32, y: f32, one: &mut Tx) -> bool {
        let step = if self.input_click.steps.front().is_some_and(|(p, _)| *p == path) {
            self.input_click.steps.pop_front().unwrap().1
        } else {
            Step::Down { clicks: 1, shift: false }
        };
        let Some(access) = self.node(&path).and_then(|n| n.input.as_ref()?.access.clone()) else { return false };
        let single = self.node(&path).and_then(|n| n.input.as_ref()).is_some_and(|i| i.single_line);
        let layout = self.node(&path).and_then(|n| n.text_layout.clone());
        let s = self.env.scale;
        let scroll = self.input_scroll.get(&path).copied().unwrap_or(0.0);
        match step {
            Step::Down { clicks, shift } => {
                self.set_focus(Some(path), false);
                let e = access(handler_state(&mut self.state));
                e.set_single_line(single);
                e.break_history_group();
                e.clear_pending();
                if e.preedit.is_some() {
                    self.input_click.clear();
                    one.instant = true;
                    return true;
                }
                if let Some(l) = layout {
                    let i = hit_index(e, &l, x + scroll, y, s, single);
                    if shift {
                        let anchor = e.anchor;
                        self.input_click.unit = Unit::Char;
                        self.input_click.origin = (anchor, anchor);
                        e.set_selection(anchor, i);
                    } else {
                        let unit = match clicks {
                            1 => Unit::Char,
                            2 => Unit::Word,
                            _ => Unit::Paragraph,
                        };
                        let origin = e.unit_at(i, unit);
                        self.input_click.unit = unit;
                        self.input_click.origin = origin;
                        e.set_selection(origin.0, origin.1);
                    }
                }
                one.instant = true;
                true
            }
            Step::Drag => {
                if self.focused.as_deref() != Some(path.as_str()) {
                    return false;
                }
                let e = access(handler_state(&mut self.state));
                e.set_single_line(single);
                if e.preedit.is_some() {
                    self.input_click.clear();
                    return false;
                }
                let Some(l) = layout else { return false };
                let i = hit_index(e, &l, x + scroll, y, s, single);
                let at = e.unit_at(i, self.input_click.unit);
                let (anchor, focus) = extend(self.input_click.origin, at);
                let before = (e.anchor, e.focus);
                e.set_selection(anchor, focus);
                if (e.anchor, e.focus) == before {
                    return false;
                }
                e.break_history_group();
                one.instant = true;
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cx, El, Env, Ime, Mods, PhysicalInput, Runtime, TextEdit, TextStyle, col, text_input};

    struct Form {
        single: TextEdit,
        multi: TextEdit,
        other: TextEdit,
        secret: TextEdit,
        secret_multi: TextEdit,
    }

    fn style() -> TextStyle {
        TextStyle::new(14.0, 400.0).lh(20.0)
    }

    fn view(s: &Form, _: &mut Cx<Form>) -> El {
        let st = style();
        col()
            .w_full()
            .h_full()
            .padding(8.0)
            .gap(8.0)
            .child(
                text_input(&s.single, st)
                    .single_line()
                    .key("single")
                    .size(280.0, 24.0)
                    .on_edit(|s: &mut Form| &mut s.single),
            )
            .child(
                text_input(&s.multi, st)
                    .key("multi")
                    .size(280.0, 80.0)
                    .on_edit(|s: &mut Form| &mut s.multi),
            )
            .child(
                text_input(&s.other, st)
                    .single_line()
                    .key("other")
                    .size(280.0, 24.0)
                    .on_edit(|s: &mut Form| &mut s.other),
            )
            .child(
                text_input(&s.secret, st)
                    .single_line()
                    .secure()
                    .key("secret")
                    .size(280.0, 24.0)
                    .on_edit(|s: &mut Form| &mut s.secret),
            )
            .child(
                text_input(&s.secret_multi, st)
                    .secure()
                    .key("secret-multi")
                    .size(280.0, 48.0)
                    .on_edit(|s: &mut Form| &mut s.secret_multi),
            )
    }

    fn runtime() -> Runtime<Form> {
        let state = Form {
            single: TextEdit::new("make the title smaller"),
            multi: TextEdit::new("first line here\nsecond line there\nthird line end"),
            other: TextEdit::new("other words"),
            secret: TextEdit::new("correct horse battery"),
            secret_multi: TextEdit::new("line one secret\nline two secret"),
        };
        let mut rt = Runtime::new(state, view, (320.0, 260.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt
    }

    fn rect_of(rt: &Runtime<Form>, key: &str) -> [f32; 4] {
        rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().rect
    }

    /// Window point `(x, y)` at the center of byte `byte` in input `key`'s layout.
    fn point_at_byte(rt: &Runtime<Form>, key: &str, byte: usize) -> (f32, f32) {
        let n = rt.nodes.iter().find(|n| n.key.as_deref() == Some(key)).unwrap();
        let l = n.text_layout.as_ref().unwrap();
        let s = rt.env.scale;
        let cur = Cursor::from_byte_index(l, byte, Affinity::Downstream);
        let bb = crate::text::cursor_geometry(&cur, l, 1.0);
        let r = rect_of(rt, key);
        (r[0] + n.inset[0] + bb.x0 as f32 / s + 2.0, r[1] + n.inset[1] + ((bb.y0 + bb.y1) * 0.5) as f32 / s)
    }

    fn click(rt: &mut Runtime<Form>, (x, y): (f32, f32)) {
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
    }

    #[test]
    fn double_and_triple_click_select_word_and_line_in_single_line_input() {
        let mut rt = runtime();
        let r = rect_of(&rt, "single");
        let (x, y) = (r[0] + 12.0, r[1] + r[3] * 0.5);

        // Unfocused first press focuses the input and places the caret inside "make".
        assert!(!rt.state.single.focused);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        assert!(rt.state.single.focused);
        assert!(!rt.keyboard_focus, "pointer focus does not turn on keyboard focus ring");
        assert_eq!(rt.state.single.anchor, rt.state.single.focus);
        assert!(rt.state.single.focus > 0 && rt.state.single.focus < 4);

        // Second press within double_click_interval and CLICK_SLOP selects "make" (0..4).
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        assert_eq!((rt.state.single.anchor, rt.state.single.focus), (0, 4));
        assert_eq!(rt.state.single.selected(), "make");

        // Third press selects the whole single-line text; fourth stays at three.
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        assert_eq!((rt.state.single.anchor, rt.state.single.focus), (0, rt.state.single.text.len()));
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        assert_eq!((rt.state.single.anchor, rt.state.single.focus), (0, rt.state.single.text.len()));
    }

    #[test]
    fn triple_click_in_multiline_input_selects_hard_line_paragraph() {
        let mut rt = runtime();
        let second = rt.state.multi.text.find("second").unwrap();
        let third = rt.state.multi.text.find("third").unwrap();
        let pt = point_at_byte(&rt, "multi", second + 2);

        click(&mut rt, pt);
        click(&mut rt, pt);
        assert_eq!((rt.state.multi.anchor, rt.state.multi.focus), (second, second + 6));
        assert_eq!(rt.state.multi.selected(), "second");

        click(&mut rt, pt);
        assert_eq!((rt.state.multi.anchor, rt.state.multi.focus), (second, third));
        assert_eq!(rt.state.multi.selected(), "second line there\n");

        // Fourth click stays at three.
        click(&mut rt, pt);
        assert_eq!((rt.state.multi.anchor, rt.state.multi.focus), (second, third));
    }

    #[test]
    fn click_drag_double_drag_and_triple_drag_extend_by_unit_in_both_directions() {
        let mut rt = runtime();
        let p_the = point_at_byte(&rt, "single", 6); // inside "the" (5..8)
        let p_smaller = point_at_byte(&rt, "single", 17); // inside "smaller" (15..22)
        let p_make = point_at_byte(&rt, "single", 1); // inside "make" (0..4)

        // Single-click drag selects by characters.
        rt.pointer_down(p_the.0, p_the.1);
        rt.frame(1.0 / 60.0);
        let start = rt.state.single.anchor;
        rt.pointer_move(p_smaller.0, p_smaller.1);
        rt.pointer_up(p_smaller.0, p_smaller.1);
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.single.anchor, start);
        assert!(rt.state.single.focus > 15 && rt.state.single.focus < 22);

        // Wait past double_click_interval, then double-click "the" (5..8) and drag right into "smaller" (15..22).
        rt.frame(1.0);
        click(&mut rt, p_the);
        rt.pointer_down(p_the.0, p_the.1);
        rt.frame(1.0 / 60.0);
        assert_eq!((rt.state.single.anchor, rt.state.single.focus), (5, 8));
        rt.pointer_move(p_smaller.0, p_smaller.1);
        rt.frame(1.0 / 60.0);
        assert_eq!((rt.state.single.anchor, rt.state.single.focus), (5, 22));
        assert_eq!(rt.state.single.selected(), "the title smaller");

        // Dragging back left into "make" (0..4) flips anchor to the far end of "the" (8) and focus to 0.
        rt.pointer_move(p_make.0, p_make.1);
        rt.pointer_up(p_make.0, p_make.1);
        rt.frame(1.0 / 60.0);
        assert_eq!((rt.state.single.anchor, rt.state.single.focus), (8, 0));
        assert_eq!(rt.state.single.selected(), "make the");

        // Triple-click on line 2 of multi ("second line there\n") and drag down to line 3, then up to line 1.
        rt.frame(1.0);
        let second = rt.state.multi.text.find("second").unwrap();
        let third = rt.state.multi.text.find("third").unwrap();
        let len = rt.state.multi.text.len();
        let l1 = point_at_byte(&rt, "multi", 2);
        let l2 = point_at_byte(&rt, "multi", second + 2);
        let l3 = point_at_byte(&rt, "multi", third + 2);

        click(&mut rt, l2);
        click(&mut rt, l2);
        rt.pointer_down(l2.0, l2.1);
        rt.frame(1.0 / 60.0);
        assert_eq!((rt.state.multi.anchor, rt.state.multi.focus), (second, third));
        rt.pointer_move(l3.0, l3.1);
        rt.frame(1.0 / 60.0);
        assert_eq!((rt.state.multi.anchor, rt.state.multi.focus), (second, len));
        rt.pointer_move(l1.0, l1.1);
        rt.pointer_up(l1.0, l1.1);
        rt.frame(1.0 / 60.0);
        assert_eq!((rt.state.multi.anchor, rt.state.multi.focus), (third, 0));
    }

    #[test]
    fn shift_click_extends_from_anchor_and_does_not_start_multiclick() {
        let mut rt = runtime();
        let p_make = point_at_byte(&rt, "single", 2);
        let p_title = point_at_byte(&rt, "single", 11);

        click(&mut rt, p_make);
        let anchor = rt.state.single.anchor;
        assert!(anchor > 0 && anchor < 4);

        // Shift+click at the same point right after a click extends (empty at anchor) instead of double-clicking.
        rt.physical_input(PhysicalInput::Modifiers(Mods { shift: true, ..Mods::default() }));
        click(&mut rt, p_make);
        assert_eq!((rt.state.single.anchor, rt.state.single.focus), (anchor, anchor));

        // Shift+click further right extends from anchor to the clicked byte.
        click(&mut rt, p_title);
        rt.physical_input(PhysicalInput::Modifiers(Mods::default()));
        assert_eq!(rt.state.single.anchor, anchor);
        assert!(rt.state.single.focus >= 9 && rt.state.single.focus <= 14);

        // Immediately clicking at p_title without Shift is a single click, not a double-click.
        click(&mut rt, p_title);
        assert_eq!(rt.state.single.anchor, rt.state.single.focus);
    }

    #[test]
    fn secure_inputs_select_all_on_double_and_triple_click_and_stay_pristine() {
        let mut rt = runtime();
        assert!(rt.state.secret.is_pristine());
        assert!(rt.state.secret_multi.is_pristine());

        let r1 = rect_of(&rt, "secret");
        let pt1 = (r1[0] + 12.0, r1[1] + r1[3] * 0.5);
        click(&mut rt, pt1);
        assert!(rt.state.secret.is_pristine());
        assert_eq!(rt.state.secret.anchor, rt.state.secret.focus);
        click(&mut rt, pt1);
        assert_eq!((rt.state.secret.anchor, rt.state.secret.focus), (0, rt.state.secret.text.len()));
        assert!(rt.state.secret.is_pristine(), "double-click selection must not mark a secure input edited");
        click(&mut rt, pt1);
        assert_eq!((rt.state.secret.anchor, rt.state.secret.focus), (0, rt.state.secret.text.len()));
        assert!(rt.state.secret.is_pristine());

        let r2 = rect_of(&rt, "secret-multi");
        let pt2 = (r2[0] + 12.0, r2[1] + 12.0);
        click(&mut rt, pt2);
        assert_eq!((rt.state.secret_multi.anchor, rt.state.secret_multi.focus), (rt.state.secret_multi.text.len(), rt.state.secret_multi.text.len()));
        click(&mut rt, pt2);
        assert_eq!((rt.state.secret_multi.anchor, rt.state.secret_multi.focus), (0, rt.state.secret_multi.text.len()));
        assert!(rt.state.secret_multi.is_pristine());
        click(&mut rt, pt2);
        assert_eq!((rt.state.secret_multi.anchor, rt.state.secret_multi.focus), (0, rt.state.secret_multi.text.len()));
        assert!(rt.state.secret_multi.is_pristine());
    }

    #[test]
    fn timing_slop_and_target_changes_reset_multiclick_count_and_preedit_ignores_it() {
        let mut rt = runtime();
        let r = rect_of(&rt, "single");
        let pt = (r[0] + 12.0, r[1] + r[3] * 0.5);

        // 1. Waiting longer than double_click_interval resets count to 1.
        click(&mut rt, pt);
        let slow = rt.double_click_interval + 0.05;
        rt.frame(slow);
        click(&mut rt, pt);
        assert_eq!(rt.state.single.anchor, rt.state.single.focus, "press after double_click_interval is a single click");

        // 2. Moving outside CLICK_SLOP resets count to 1.
        rt.frame(1.0);
        click(&mut rt, pt);
        click(&mut rt, (pt.0 + CLICK_SLOP + 1.0, pt.1));
        assert_eq!(rt.state.single.anchor, rt.state.single.focus, "press outside CLICK_SLOP is a single click");

        // 3. Pressing another input in between resets count to 1.
        rt.frame(1.0);
        let r_other = rect_of(&rt, "other");
        let pt_other = (r_other[0] + 12.0, r_other[1] + r_other[3] * 0.5);
        click(&mut rt, pt);
        click(&mut rt, pt_other);
        click(&mut rt, pt);
        assert_eq!(rt.state.single.anchor, rt.state.single.focus, "pressing another input resets the click count");

        // 4. Active IME composition ignores multi-click and keeps preedit intact.
        rt.frame(1.0);
        rt.ime(Ime::Preedit("かん".into(), Some((0, 6))));
        rt.frame(1.0 / 60.0);
        let before = (rt.state.single.anchor, rt.state.single.focus);
        click(&mut rt, pt);
        click(&mut rt, pt);
        assert!(rt.state.single.preedit.is_some());
        assert_eq!((rt.state.single.anchor, rt.state.single.focus), before);
    }
}

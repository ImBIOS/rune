//! The open playground's keys.

use super::*;

impl<S: 'static> Live<S> {
    /// What `ev` does in the open playground. `in_side`: the keyboard is in the side column.
    pub(crate) fn nav(&self, ev: &KeyEvent, in_side: bool) -> Nav {
        let m = ev.mods;
        // Control on macOS, the Windows key elsewhere: never the playground's.
        let other = m.alt || (cfg!(target_os = "macos") && m.ctrl) || (cfg!(not(target_os = "macos")) && m.logo);
        if !self.open || other { return Nav::Pass; }
        let letter = match &ev.key {
            Key::Text(t) if !m.cmd => t.chars().next().filter(|_| t.chars().count() == 1).map(|c| c.to_ascii_lowercase()),
            _ => None,
        };
        if in_side {
            match ev.key {
                Key::Tab | Key::Escape | Key::Left => return Nav::OutOfSide,
                Key::Up if !m.cmd => return Nav::SideStep(-1),
                Key::Down if !m.cmd => return Nav::SideStep(1),
                _ => {}
            }
        }
        if let Some(request) = letter.and_then(|c| self.variant_key(c)) { return Nav::Request(request); }
        // In the side column, Space presses the focused control.
        if let Some(c) = letter && !(in_side && c == ' ') && let Some(request) = self.trigger_request(c) {
            return Nav::Request(request);
        }
        if in_side { return Nav::Pass; }
        let rows = self.rows();
        let Some(&last) = rows.iter().rev().find(|r| matches!(r, Row::Story(_))) else { return Nav::Pass };
        let at = rows.iter().position(|r| match (*r, self.header) {
            (Row::Header(g, _), Some(h)) => g == h,
            (Row::Story(i), None) => i == self.selected,
            _ => false,
        });
        let n = rows.len();
        let to = match (&ev.key, letter, self.header) {
            (Key::Up, _, _) if m.cmd => rows.iter().copied().find(|r| matches!(r, Row::Story(_))).unwrap_or(last),
            (Key::Down, _, _) if m.cmd => last,
            (_, _, _) if m.cmd => return Nav::Pass,
            (Key::Up, _, _) => rows[at.map_or(0, |a| (a + n - 1) % n)],
            (Key::Down, _, _) => rows[at.map_or(0, |a| (a + 1) % n)],
            // J and K skip the headers.
            (_, Some(c @ ('j' | 'k')), _) => {
                let step = if c == 'j' { 1 } else { n - 1 };
                let from = at.unwrap_or(0);
                (1..=n).map(|k| rows[(from + k * step) % n]).find(|r| matches!(r, Row::Story(_))).unwrap_or(last)
            }
            (Key::Left, _, Some(g)) => return Nav::Request(Request::Fold(g, Some(true))),
            (Key::Right, _, Some(g)) if !m.shift => return Nav::Request(Request::Fold(g, Some(false))),
            (Key::Enter, _, Some(g)) => return Nav::Request(Request::Fold(g, None)),
            (Key::Right, _, None) | (Key::Tab, _, _) if !m.shift => return Nav::IntoSide,
            _ => return Nav::Pass,
        };
        Nav::Request(match to {
            Row::Header(g, _) => Request::Header(g),
            Row::Story(i) => Request::Select(i),
        })
    }

    /// What the open story's trigger on `c` asks for, as its letter does: R replays.
    pub(super) fn trigger_request(&self, c: char) -> Option<Request> {
        self.triggers().iter().any(|(k, _)| *k == c).then_some(if c == 'r' { Request::Replay } else { Request::Trigger(c) })
    }

    /// The list's rows: each group's header, then its stories unless it's folded.
    pub(super) fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut current = None;
        for &i in &self.order {
            let group = self.group(i);
            if current != Some(group) {
                current = Some(group);
                rows.push(Row::Header(group, i));
            }
            if !self.folded.contains(&group) { rows.push(Row::Story(i)); }
        }
        rows
    }
}

/// A row of the story list. A header carries its first story, which keys it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Row {
    Header(&'static str, usize),
    Story(usize),
}

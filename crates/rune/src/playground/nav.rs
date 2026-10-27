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
                Key::Tab | Key::Escape => return Nav::OutOfSide,
                Key::Up if !m.cmd => return Nav::SideStep(-1),
                Key::Down if !m.cmd => return Nav::SideStep(1),
                _ => {}
            }
        }
        // In the side column, Space presses the focused control.
        if let Some(c) = letter && !(in_side && c == ' ') && self.triggers().iter().any(|(k, _)| *k == c) {
            return Nav::Request(if c == 'r' { Request::Replay } else { Request::Trigger(c) });
        }
        if in_side { return Nav::Pass; }
        let at = self.order.iter().position(|&i| i == self.selected).unwrap_or(0);
        let n = self.order.len();
        if n == 0 { return Nav::Pass; }
        let to = match (&ev.key, letter) {
            (Key::Up, _) if m.cmd => 0,
            (Key::Down, _) if m.cmd => n - 1,
            (_, _) if m.cmd => return Nav::Pass,
            (Key::Up, _) | (_, Some('k')) => (at + n - 1) % n,
            (Key::Down, _) | (_, Some('j')) => (at + 1) % n,
            (Key::Right, _) | (Key::Tab, _) if !m.shift => return Nav::IntoSide,
            _ => return Nav::Pass,
        };
        Nav::Request(Request::Select(self.order[to]))
    }
}

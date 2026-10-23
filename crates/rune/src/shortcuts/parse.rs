use super::ShortcutError;
use std::{fmt, str::FromStr};

pub(super) const CTRL: u8 = 1;
pub(super) const ALT: u8 = 2;
pub(super) const SHIFT: u8 = 4;
pub(super) const SUPER: u8 = 8;

/// A validated, layout-independent accelerator. cmd/command/logo/meta are aliases for super;
/// option is alt. Letters name physical US-layout keys on macOS/Windows, keysyms on X11.
/// Display uses ctrl+alt+shift+super order, lowercase keys and normalized aliases.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub(super) mods: u8,
    pub(super) key: String,
}

impl FromStr for Shortcut {
    type Err = ShortcutError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let invalid = || ShortcutError::InvalidAccelerator(input.into());
        let mut mods = 0;
        let mut key = None;
        for part in input.split('+') {
            let part = part.trim().to_ascii_lowercase();
            let modifier = match part.as_str() {
                "ctrl" | "control" => CTRL,
                "alt" | "option" => ALT,
                "shift" => SHIFT,
                "cmd" | "command" | "super" | "meta" | "logo" => SUPER,
                _ => 0,
            };
            if modifier != 0 {
                if key.is_some() || mods & modifier != 0 { return Err(invalid()); }
                mods |= modifier;
            } else {
                if key.is_some() { return Err(invalid()); }
                let name = match part.as_str() {
                    "esc" => "escape", "return" => "enter", "spacebar" => "space",
                    "arrowleft" => "left", "arrowright" => "right", "arrowup" => "up", "arrowdown" => "down",
                    "del" => "delete", "ins" => "insert", "pgup" => "pageup", "pgdn" => "pagedown",
                    "-" => "minus", "=" => "equal", "," => "comma", "." => "period", "/" => "slash",
                    ";" => "semicolon", "'" => "quote", "[" => "bracketleft", "]" => "bracketright",
                    "\\" => "backslash", "`" => "backquote",
                    name => name,
                };
                let character = name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric();
                let function = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
                    .is_some_and(|n| (1..=24).contains(&n) && name == format!("f{n}"));
                if !character && !function && !matches!(name, "escape" | "enter" | "space" | "tab" | "backspace" |
                    "delete" | "insert" | "home" | "end" | "pageup" | "pagedown" | "left" | "right" | "up" | "down" |
                    "minus" | "equal" | "comma" | "period" | "slash" | "semicolon" | "quote" | "bracketleft" |
                    "bracketright" | "backslash" | "backquote") { return Err(invalid()); }
                key = Some(name.to_owned());
            }
        }
        Ok(Self { mods, key: key.ok_or_else(invalid)? })
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (bit, name) in [(CTRL, "ctrl"), (ALT, "alt"), (SHIFT, "shift"), (SUPER, "super")] {
            if self.mods & bit != 0 { write!(f, "{name}+")?; }
        }
        f.write_str(&self.key)
    }
}

#[cfg(target_os = "linux")]
impl Shortcut {
    pub(super) fn keysym_name(&self) -> String {
        match self.key.as_str() {
            "escape" => "Escape", "enter" => "Return", "tab" => "Tab", "backspace" => "BackSpace",
            "delete" => "Delete", "insert" => "Insert", "home" => "Home", "end" => "End", "pageup" => "Prior",
            "pagedown" => "Next", "left" => "Left", "right" => "Right", "up" => "Up", "down" => "Down",
            "quote" => "apostrophe", "backquote" => "grave",
            k if k.starts_with('f') && k.len() > 1 => return k.to_uppercase(),
            k => k,
        }.into()
    }

    pub(super) fn portal_trigger(&self) -> String {
        let mut s = String::new();
        for (bit, name) in [(CTRL, "CTRL"), (ALT, "ALT"), (SHIFT, "SHIFT"), (SUPER, "LOGO")] {
            if self.mods & bit != 0 { s.push_str(name); s.push('+'); }
        }
        s.push_str(&self.keysym_name());
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_aliases_round_trip() {
        for (input, expected) in [
            ("CMD+Shift+Space", "shift+super+space"),
            ("OPTION+Control+Return", "ctrl+alt+enter"),
            ("super+SHIFT+ctrl+ALT+F24", "ctrl+alt+shift+super+f24"),
            (" ctrl + A ", "ctrl+a"), ("esc", "escape"), ("cmd+/", "super+slash"),
        ] {
            let shortcut: Shortcut = input.parse().unwrap();
            assert_eq!(shortcut.to_string(), expected);
            assert_eq!(shortcut, expected.parse().unwrap());
        }
    }
    #[test]
    fn reject_ambiguous_and_incomplete_accelerators() {
        for input in ["", "cmd", "cmd+", "cmd++a", "cmd+super+a", "a+b", "a+shift", "ctrl+unknown", "F0", "f25", "f01", "é"] {
            assert!(matches!(input.parse::<Shortcut>(), Err(ShortcutError::InvalidAccelerator(_))), "{input}");
        }
    }
}

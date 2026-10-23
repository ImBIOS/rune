//! Stable physical key names. Display text is a separate, layout-dependent operation.
use super::{Shortcut, ShortcutError, parse::{ALT, CTRL, SHIFT, SUPER}};
pub use winit::keyboard::KeyCode as PhysicalKeyCode;

const KEYS: &[(PhysicalKeyCode, &str)] = {
    use PhysicalKeyCode::*;
    &[
        (KeyA,"a"),(KeyB,"b"),(KeyC,"c"),(KeyD,"d"),(KeyE,"e"),(KeyF,"f"),(KeyG,"g"),(KeyH,"h"),
        (KeyI,"i"),(KeyJ,"j"),(KeyK,"k"),(KeyL,"l"),(KeyM,"m"),(KeyN,"n"),(KeyO,"o"),(KeyP,"p"),
        (KeyQ,"q"),(KeyR,"r"),(KeyS,"s"),(KeyT,"t"),(KeyU,"u"),(KeyV,"v"),(KeyW,"w"),(KeyX,"x"),(KeyY,"y"),(KeyZ,"z"),
        (Digit0,"0"),(Digit1,"1"),(Digit2,"2"),(Digit3,"3"),(Digit4,"4"),(Digit5,"5"),(Digit6,"6"),(Digit7,"7"),(Digit8,"8"),(Digit9,"9"),
        (F1,"f1"),(F2,"f2"),(F3,"f3"),(F4,"f4"),(F5,"f5"),(F6,"f6"),(F7,"f7"),(F8,"f8"),(F9,"f9"),(F10,"f10"),
        (F11,"f11"),(F12,"f12"),(F13,"f13"),(F14,"f14"),(F15,"f15"),(F16,"f16"),(F17,"f17"),(F18,"f18"),(F19,"f19"),(F20,"f20"),
        (F21,"f21"),(F22,"f22"),(F23,"f23"),(F24,"f24"),
        (Escape,"escape"),(Enter,"enter"),(Space,"space"),(Tab,"tab"),(Backspace,"backspace"),(Delete,"delete"),(Insert,"insert"),
        (Home,"home"),(End,"end"),(PageUp,"pageup"),(PageDown,"pagedown"),(ArrowLeft,"left"),(ArrowRight,"right"),(ArrowUp,"up"),(ArrowDown,"down"),
        (Minus,"minus"),(Equal,"equal"),(Comma,"comma"),(Period,"period"),(Slash,"slash"),(Semicolon,"semicolon"),(Quote,"quote"),
        (BracketLeft,"bracketleft"),(BracketRight,"bracketright"),(Backslash,"backslash"),(Backquote,"backquote"),
    ]
};

pub(super) fn named(name: &str) -> Option<PhysicalKeyCode> { KEYS.iter().find(|(_, n)| *n == name).map(|(key, _)| *key) }
pub(super) fn name(key: PhysicalKeyCode) -> Option<&'static str> { KEYS.iter().find(|(k, _)| *k == key).map(|(_, name)| *name) }

impl Shortcut {
    /// Build from the hardware key reported by the window, never its typed/logical character.
    /// Modifiers alone do not form a registerable shortcut.
    pub fn from_physical_key(key: PhysicalKeyCode, mods: crate::Mods) -> Result<Self, ShortcutError> {
        name(key).ok_or_else(|| ShortcutError::InvalidAccelerator(format!("{key:?}")))?;
        let mut bits = 0;
        if mods.ctrl || (!cfg!(target_os = "macos") && mods.cmd) { bits |= CTRL; }
        if mods.alt { bits |= ALT; }
        if mods.shift { bits |= SHIFT; }
        if mods.logo || (cfg!(target_os = "macos") && mods.cmd) { bits |= SUPER; }
        Ok(Self { key, mods: bits })
    }

    pub fn physical_key(&self) -> PhysicalKeyCode { self.key }

    pub fn modifiers(&self) -> crate::Mods {
        crate::Mods { ctrl: self.mods & CTRL != 0, alt: self.mods & ALT != 0, shift: self.mods & SHIFT != 0,
            logo: self.mods & SUPER != 0, cmd: self.mods & if cfg!(target_os = "macos") { SUPER } else { CTRL } != 0 }
    }

    pub(super) fn key_name(&self) -> &'static str { name(self.key).expect("validated physical shortcut key") }

    /// Human-facing keycaps. macOS translates the physical key using the current keyboard
    /// layout on the UI thread; elsewhere (or when layout data is unavailable) these
    /// are US labels. Persist Display/FromStr, not these labels.
    pub fn keycaps(&self) -> Vec<String> {
        let mut labels = modifier_keycaps(self.modifiers());
        labels.push(self.key_label());
        labels
    }

    fn key_label(&self) -> String {
        let name = self.key_name();
        if let Some(label) = named_label(name) { return label.into(); }
        #[cfg(target_os = "macos")]
        if let Some(label) = super::macos::keycode(name).and_then(super::layout_macos::label) { return label; }
        us_label(name).into()
    }
}

/// Keycaps for a chord being held, before a non-modifier key has been pressed.
pub fn modifier_keycaps(mods: crate::Mods) -> Vec<String> {
    let mac = cfg!(target_os = "macos");
    let mut labels = Vec::new();
    for (held, label) in [(mods.ctrl || (!mac && mods.cmd), if mac { "⌃" } else { "Ctrl" }),
        (mods.alt, if mac { "⌥" } else { "Alt" }), (mods.shift, if mac { "⇧" } else { "Shift" }),
        (mods.logo || (mac && mods.cmd), if mac { "⌘" } else { "Super" })] {
        if held { labels.push(label.into()); }
    }
    labels
}

fn named_label(name: &str) -> Option<&str> {
    Some(match name {
        "escape" => "Esc", "enter" => "Return", "space" => "Space", "tab" => "Tab", "backspace" => "⌫",
        "delete" => "Delete", "insert" => "Insert", "home" => "Home", "end" => "End", "pageup" => "Page Up", "pagedown" => "Page Down",
        "left" => "←", "right" => "→", "up" => "↑", "down" => "↓",
        n if n.starts_with('f') && n.len() > 1 => n,
        _ => return None,
    })
}
fn us_label(name: &str) -> &str {
    match name { "minus" => "-", "equal" => "=", "comma" => ",", "period" => ".", "slash" => "/", "semicolon" => ";", "quote" => "'",
        "bracketleft" => "[", "bracketright" => "]", "backslash" => "\\", "backquote" => "`", name => name }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_storage_roundtrips_every_supported_key() {
        for &(key, _) in KEYS {
            let shortcut = Shortcut::from_physical_key(key, crate::Mods { ctrl: true, shift: true, ..Default::default() }).unwrap();
            assert_eq!(shortcut.to_string().parse::<Shortcut>().unwrap().physical_key(), key);
            assert_eq!(shortcut.physical_key(), key);
        }
        assert!(Shortcut::from_physical_key(PhysicalKeyCode::ShiftLeft, crate::Mods::default()).is_err());
    }
}

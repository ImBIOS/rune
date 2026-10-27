//! Key names in headless scripts and `--keys`.
use crate::edit::Key;

pub(crate) fn key_named(name: &str) -> Option<Key> {
    Some(match name {
        "Enter" => Key::Enter,
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "Left" => Key::Left,
        "Right" => Key::Right,
        "Up" => Key::Up,
        "Down" => Key::Down,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "Escape" => Key::Escape,
        "Tab" => Key::Tab,
        "ContextMenu" => Key::ContextMenu,
        f if f.len() > 1 && f.starts_with('F') && f[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => {
            Key::Function(f[1..].parse().unwrap())
        }
        c if c.chars().count() == 1 => Key::Text(c.to_owned()),
        _ => return None,
    })
}

/// A script key's `Tx::key_base`: its `"base"`, else a one-character name typed without Shift
/// or Option (which change the character the key types), lowercased.
pub(crate) fn script_key_base(e: &serde_json::Value, name: &str, mods: crate::Mods) -> Option<char> {
    let one = |s: &str| { let mut c = s.chars(); c.next().filter(|_| c.next().is_none()) };
    match e["base"].as_str() {
        Some(base) => one(base),
        None => one(name).filter(|_| !mods.shift && !mods.alt).map(|c| c.to_ascii_lowercase()),
    }
}

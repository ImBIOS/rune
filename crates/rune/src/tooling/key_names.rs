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
        f if f.len() > 1 && f.starts_with('F') && f[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => {
            Key::Function(f[1..].parse().unwrap())
        }
        c if c.chars().count() == 1 => Key::Text(c.to_owned()),
        _ => return None,
    })
}

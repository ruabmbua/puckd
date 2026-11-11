//! Key name lookup ("KEY_ENTER", "enter", "BTN_LEFT", "ctrl+c") for config bindings.

#[path = "keys_table.rs"]
mod table;

use crate::uinput::ev::{BTN_EXTRA, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE};

pub fn code(name: &str) -> Option<u16> {
    let up = name.trim().to_ascii_uppercase();
    let alias = match up.as_str() {
        "CTRL" => "KEY_LEFTCTRL",
        "SHIFT" => "KEY_LEFTSHIFT",
        "ALT" => "KEY_LEFTALT",
        "SUPER" | "META" | "WIN" => "KEY_LEFTMETA",
        "ESCAPE" => "KEY_ESC",
        "RETURN" => "KEY_ENTER",
        "DEL" => "KEY_DELETE",
        "PGUP" => "KEY_PAGEUP",
        "PGDN" => "KEY_PAGEDOWN",
        "MOUSE_LEFT" => "BTN_LEFT",
        "MOUSE_RIGHT" => "BTN_RIGHT",
        "MOUSE_MIDDLE" => "BTN_MIDDLE",
        "MOUSE_BACK" => "BTN_SIDE",
        "MOUSE_FORWARD" => "BTN_EXTRA",
        _ => "",
    };
    let full = if !alias.is_empty() {
        alias.to_string()
    } else if up.starts_with("KEY_") || up.starts_with("BTN_") {
        up
    } else {
        format!("KEY_{up}")
    };
    table::KEYS.iter().find(|(n, _)| *n == full).map(|&(_, c)| c)
}

/// Parse "ctrl+shift+t" into key codes. Empty / "none" yields an empty list.
pub fn parse_combo(s: &str) -> Result<Vec<u16>, String> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    s.split('+').map(|p| code(p).ok_or_else(|| format!("unknown key name '{p}'"))).collect()
}

pub fn is_mouse_button(c: u16) -> bool {
    (0x110..=0x117).contains(&c)
}

/// Mouse buttons the virtual mouse exposes.
pub const MOUSE_BUTTONS: &[u16] = &[BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA, 0x115, 0x116, 0x117];

/// Every keyboard key code (KEY_*, excluding BTN_* ranges) for the virtual keyboard.
pub fn keyboard_keys() -> Vec<u16> {
    let mut v: Vec<u16> = table::KEYS
        .iter()
        .filter(|(n, c)| n.starts_with("KEY_") && *c > 0 && !(0x100..0x160).contains(c))
        .map(|&(_, c)| c)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(code("KEY_ENTER"), Some(28));
        assert_eq!(code("enter"), Some(28));
        assert_eq!(code("esc"), Some(1));
        assert_eq!(code("btn_left"), Some(0x110));
        assert_eq!(parse_combo("ctrl+c").unwrap(), vec![29, 46]);
        assert!(parse_combo("nope").is_err());
        assert!(parse_combo("none").unwrap().is_empty());
    }
}

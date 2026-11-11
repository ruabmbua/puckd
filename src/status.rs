//! `$XDG_RUNTIME_DIR/puckd/<slot>.json` status files (for waybar & co).

use std::path::PathBuf;

use crate::config::Mode;
use crate::hid::HidInfo;
use crate::proto::charge_state_name;

pub fn dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).map(|d| PathBuf::from(d).join("puckd"))
}

fn file_for(info: &HidInfo) -> Option<PathBuf> {
    Some(dir()?.join(format!("{}.json", info.slot_label())))
}

pub struct SlotStatus<'a> {
    pub connected: bool,
    pub mode: Mode,
    pub battery: Option<(u8, u8)>,
    pub backed_off: bool,
    pub name: &'a str,
    pub serial: Option<&'a str>,
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if c.is_control() => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

pub fn write(info: &HidInfo, st: &SlotStatus) {
    let SlotStatus { connected, mode, battery, backed_off, name, serial } = *st;
    let Some(path) = file_for(info) else { return };
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let (level, charge) = match battery {
        Some((c, l)) => (l.to_string(), format!("\"{}\"", charge_state_name(c))),
        None => ("null".into(), "null".into()),
    };
    let mode = match mode {
        Mode::Desktop => "desktop",
        Mode::Gamepad => "gamepad",
    };
    let json = format!(
        "{{\"slot\":\"{}\",\"hidraw\":\"{}\",\"name\":{},\"serial\":{},\"connected\":{connected},\"mode\":\"{mode}\",\"backed_off\":{backed_off},\"battery\":{level},\"charge_state\":{charge}}}\n",
        info.slot_label(),
        info.node,
        json_str(name),
        serial.map(json_str).unwrap_or_else(|| "null".into()),
    );
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

pub fn remove(info: &HidInfo) {
    if let Some(p) = file_for(info) {
        let _ = std::fs::remove_file(p);
    }
}

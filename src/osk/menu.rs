//! Entries of the overlay's Controller pages (settings, button mapping, presets): built from
//! the config, applied at runtime, and saved back to the config file (keeping its comments).

use std::path::Path;

use crate::config::{Backoff, Config, Mode, PresetSwitch, Profile, Toggle};
use crate::osk::{Item, Status, Value};
use crate::presets::{Presets, State};
use crate::proto::charge_state_name;

const PROFILES: &[(Profile, &str, &str)] = &[
    (Profile::Xbox360, "xbox360", "Xbox 360"),
    (Profile::XboxElite, "xbox-elite", "Xbox Elite 2 (paddles)"),
    (Profile::Extended, "extended", "Extended (Steam Deck layout)"),
];
const MOTION: &[(Toggle, &str)] = &[(Toggle::Auto, "auto"), (Toggle::On, "on"), (Toggle::Off, "off")];
const BACKOFF: &[(Backoff, &str)] = &[(Backoff::Any, "any"), (Backoff::Steam, "steam"), (Backoff::Never, "never")];
const PRESET_SWITCH: &[(PresetSwitch, &str)] =
    &[(PresetSwitch::Ask, "ask"), (PresetSwitch::Auto, "automatic"), (PresetSwitch::Off, "off")];

fn choice<T: PartialEq>(cur: T, opts: &[(T, &str)]) -> Value {
    Value::Choice {
        idx: opts.iter().position(|(v, _)| *v == cur).unwrap_or(0),
        options: opts.iter().map(|(_, n)| n.to_string()).collect(),
    }
}

fn num(v: f32, min: f32, max: f32, step: f32, percent: bool) -> Value {
    Value::Number { v, min, max, step, percent }
}

fn item(id: &str, label: &str, value: Value) -> Item {
    Item { id: id.into(), label: label.into(), value }
}

/// Buttons that can be remapped, with their display names.
pub const MAPPABLE: &[(&str, &str)] = &[
    ("a", "A"),
    ("b", "B"),
    ("x", "X"),
    ("y", "Y"),
    ("lb", "LB"),
    ("rb", "RB"),
    ("l3", "L3 (left stick click)"),
    ("r3", "R3 (right stick click)"),
    ("view", "View (left center)"),
    ("menu", "Menu ≡ (right center)"),
    ("steam", "Steam"),
    ("l4", "L4 (left upper paddle)"),
    ("l5", "L5 (left lower paddle)"),
    ("r4", "R4 (right upper paddle)"),
    ("r5", "R5 (right lower paddle)"),
    ("dpad_up", "D-pad up"),
    ("dpad_down", "D-pad down"),
    ("dpad_left", "D-pad left"),
    ("dpad_right", "D-pad right"),
    ("lpad_click", "Left pad click"),
    ("rpad_click", "Right pad click"),
    ("lt_click", "LT full pull"),
    ("rt_click", "RT full pull"),
];

/// Short text for a button inside a round glyph.
pub fn button_glyph(name: &str) -> String {
    match name {
        "menu" => "≡".into(),
        "view" => "V".into(),
        "steam" => "S".into(),
        "qam" => "⋯".into(),
        n => n.to_uppercase(),
    }
}

/// How a button is named in text.
pub fn button_label(name: &str) -> String {
    match name {
        "menu" => "≡ Menu".into(),
        "view" => "View".into(),
        n => short(n).to_string(),
    }
}

fn short(name: &str) -> &str {
    MAPPABLE.iter().find(|(n, _)| *n == name).map(|(_, l)| l.split(" (").next().unwrap_or(l)).unwrap_or(name)
}

fn profile_label(p: Profile) -> &'static str {
    PROFILES.iter().find(|x| x.0 == p).map(|x| x.2).unwrap_or("?")
}

/// Button mapping page: title and entries.
pub fn mapping(cfg: &Config, presets: &Presets) -> (String, Vec<Item>) {
    let g = &cfg.gamepad;
    let title = if presets.active.is_none() {
        format!("Preset {} · changing a button here starts a new preset", presets.describe(g))
    } else {
        format!("Preset {} · {}", presets.describe(g), profile_label(g.profile))
    };
    let mut options = vec!["itself".to_string(), "nothing".to_string()];
    options.extend(MAPPABLE.iter().map(|(n, _)| short(n).to_string()));
    let mut items: Vec<Item> = MAPPABLE
        .iter()
        .map(|(name, label)| {
            let idx = match g.remap.get(*name).map(String::as_str) {
                None => 0,
                Some(t) if t.eq_ignore_ascii_case("none") => 1,
                Some(t) => MAPPABLE.iter().position(|(n, _)| n.eq_ignore_ascii_case(t)).map_or(0, |i| i + 2),
            };
            item(&format!("remap.{name}"), label, Value::Choice { idx, options: options.clone() })
        })
        .collect();
    items.push(item("remap.clear", "Reset all mappings", Value::Button));
    (title, items)
}

fn header(label: &str) -> Item {
    item("", label, Value::Header)
}

/// Presets page: title and entries. `app` is the focused application (app_id, title).
pub fn presets(cfg: &Config, p: &Presets, app: Option<(&str, &str)>) -> (String, Vec<Item>) {
    let g = &cfg.gamepad;
    let state = p.state(g);
    let title = format!("Active preset: {}", p.describe(g));
    let name = p.active_name().to_string();
    let mut items = Vec::new();
    match state {
        State::Clean => {
            items.push(item("preset.new", "Save a copy as new preset…", Value::Text(p.suggest(app))));
            if p.active.is_some() {
                items.push(item(
                    "preset.rename",
                    &format!("Rename \u{201c}{name}\u{201d}…"),
                    Value::Text(name.clone()),
                ));
                items.push(item("preset.delete", &format!("Delete \u{201c}{name}\u{201d}"), Value::Button));
            }
        }
        State::Modified | State::NewFromDefault => {
            if state == State::Modified {
                let (id, label) = if p.active.is_some() {
                    ("preset.save", format!("Save changes to \u{201c}{name}\u{201d}"))
                } else {
                    ("preset.save_default", "Save changes as Default".to_string())
                };
                items.push(item(id, &label, Value::Button));
            }
            items.push(item("preset.new", "Save as new preset…", Value::Text(p.suggest(app))));
            items.push(item("preset.discard", "Discard changes", Value::Button));
        }
    }
    items.push(header("Load"));
    let tags = |n: Option<&str>| {
        let mut t = Vec::new();
        // The emulated controller belongs to the preset (mappings depend on it).
        if let Some(gp) = p.get(n) {
            t.push(profile_label(gp.profile).to_string());
        }
        if p.active.as_deref() == n {
            t.push(if state == State::Clean { "active".to_string() } else { "active, modified".to_string() });
        }
        if let Some(n) = n {
            let apps: Vec<String> = p
                .apps
                .iter()
                .filter(|(_, x)| x.as_str() == n)
                .map(|(a, _)| crate::presets::app_display(a, ""))
                .collect();
            if !apps.is_empty() {
                t.push(apps.join(", "));
            }
        }
        t.join(" · ")
    };
    items.push(item("preset.load_default", "Default", Value::Link(tags(None))));
    for n in p.named.keys() {
        items.push(item(&format!("preset.load.{n}"), n, Value::Link(tags(Some(n)))));
    }
    items.push(header("Apps"));
    match &p.active {
        Some(n) => {
            // The focused app first (on if linked to this preset), then every other app
            // already linked to it.
            if let Some((a, t)) = app {
                let on = p.apps.get(a) == Some(n);
                let who = crate::presets::app_display(a, t);
                items.push(item(&format!("preset.app.{a}"), &format!("Use for {who} (focused)"), Value::Toggle(on)));
            }
            for (a, x) in &p.apps {
                if x == n && app.is_none_or(|(f, _)| f != a) {
                    let who = crate::presets::app_display(a, "");
                    items.push(item(&format!("preset.app.{a}"), &format!("Use for {who}"), Value::Toggle(true)));
                }
            }
        }
        None => items.push(header("Default is used for every app without a preset")),
    }
    items.push(item(
        "general.preset_switch",
        "When an app with a preset gets focus",
        choice(cfg.general.preset_switch, PRESET_SWITCH),
    ));
    let back = p.manual.as_deref().unwrap_or(crate::presets::DEFAULT_NAME);
    items.push(item("", &format!("Leaving that app returns to \u{201c}{back}\u{201d}"), Value::Header));
    (title, items)
}

pub fn items(cfg: &Config, mode: Mode, p: &Presets, serial: Option<&str>) -> Vec<Item> {
    let d = &cfg.desktop;
    let profiles: Vec<(Profile, &str)> = PROFILES.iter().map(|&(p, _, l)| (p, l)).collect();
    let remapped = cfg.gamepad.remap.len();
    vec![
        header("Preset (how games see the controller)"),
        item("page.presets", "Preset", Value::Link(p.describe(&cfg.gamepad))),
        item(
            "page.mapping",
            "Button mapping",
            Value::Link(if remapped == 0 { "unchanged".into() } else { format!("{remapped} changed") }),
        ),
        item("gamepad.profile", "Emulated controller", choice(cfg.gamepad.profile, &profiles)),
        item("gamepad.motion", "Motion sensor device", choice(cfg.gamepad.motion, MOTION)),
        item("gamepad.rumble_strength", "Rumble strength", num(cfg.gamepad.rumble_strength, 0.0, 2.0, 0.1, true)),
        item("gamepad.rumble_swap", "Swap rumble motors", Value::Toggle(cfg.gamepad.rumble_swap)),
        item(
            "gamepad.trackpad_mouse",
            "Trackpads as mouse in gamepad mode (Xbox profiles)",
            Value::Toggle(cfg.gamepad.trackpad_mouse),
        ),
        header("Settings (shared by all presets)"),
        match serial {
            Some(s) => item(
                "controller.name",
                &format!("Controller name ({s})"),
                Value::Text(cfg.names.get(s).cloned().unwrap_or_default()),
            ),
            None => header("Controller name: this controller did not report a serial number"),
        },
        item(
            "mode",
            &format!("Mode (or hold {} to switch)", button_label(&cfg.general.switch_button)),
            choice(mode, &[(Mode::Desktop, "Desktop"), (Mode::Gamepad, "Gamepad")]),
        ),
        item("desktop.pointer_speed", "Pointer speed", num(d.pointer_speed, 0.1, 4.0, 0.1, false)),
        item("desktop.pointer_accel", "Pointer acceleration", num(d.pointer_accel, 0.0, 3.0, 0.1, false)),
        item("desktop.inertia", "Pointer inertia", Value::Toggle(d.inertia)),
        item("desktop.scroll_speed", "Scroll speed", num(d.scroll_speed, 0.1, 4.0, 0.1, false)),
        item("desktop.natural_scroll", "Natural scrolling", Value::Toggle(d.natural_scroll)),
        item("desktop.gyro_mouse", "Gyro mouse (hold grip)", Value::Toggle(d.gyro_mouse)),
        item("haptics.enabled", "Haptics", Value::Toggle(cfg.haptics.enabled)),
        item("haptics.pointer_ticks", "Pointer ticks", Value::Toggle(cfg.haptics.pointer_ticks)),
        item("general.backoff", "Step aside for", choice(cfg.general.backoff, BACKOFF)),
        item("defaults", "Restore default settings (not the preset)", Value::Button),
        item("save", "Save settings", Value::Button),
        header("Tools"),
        item("page.input", "Input test", Value::Link(String::new())),
        item("power_off", "Turn off controller", Value::Button),
    ]
}

pub struct StatusCtx<'a> {
    pub cfg: &'a Config,
    pub presets: &'a Presets,
    /// Focused application: (app_id, window title).
    pub app: Option<(&'a str, &'a str)>,
    pub slot: &'a str,
    /// The controller's serial (names are keyed by it) and display name.
    pub serial: Option<&'a str>,
    pub name: &'a str,
    pub mode: Mode,
    pub battery: Option<(u8, u8)>,
}

pub fn status(c: &StatusCtx) -> Status {
    let mode_s = match c.mode {
        Mode::Desktop => "Desktop",
        Mode::Gamepad => "Gamepad",
    };
    let bat = c.battery.map(|(cs, l)| format!(" · battery {l}% ({})", charge_state_name(cs))).unwrap_or_default();
    let (mapping_title, mapping) = mapping(c.cfg, c.presets);
    let (presets_title, presets) = presets(c.cfg, c.presets, c.app);
    let _ = c.slot;
    Status {
        title: c.name.to_string(),
        detail: format!("{} · {mode_s} mode{bat}", c.presets.describe(&c.cfg.gamepad)),
        items: items(c.cfg, c.mode, c.presets, c.serial),
        mapping_title,
        mapping,
        presets_title,
        presets,
    }
}

/// What the daemon has to do after a change.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Config changed: re-apply it to all slots.
    Config,
    /// Switch the owner slot to this mode.
    Mode(Mode),
    /// Write the config file.
    Save,
    /// Preset actions (the daemon owns the presets).
    PresetNew(String),
    PresetSave,
    PresetSaveDefault,
    PresetDiscard,
    PresetRename(String),
    PresetDelete,
    /// None = Default.
    PresetLoad(Option<String>),
    /// Turn off the controller that owns the overlay.
    PowerOff,
    /// Name the controller that owns the overlay ("" removes the name).
    ControllerName(String),
    /// (app, on): link the app to the active preset, or remove the link.
    PresetApp(String, bool),
    None,
}

fn idx(v: &Value) -> usize {
    match v {
        Value::Choice { idx, .. } => *idx,
        _ => 0,
    }
}
fn f(v: &Value) -> f32 {
    match v {
        Value::Number { v, .. } => *v,
        _ => 0.0,
    }
}
fn text(v: &Value) -> String {
    match v {
        Value::Text(t) => t.clone(),
        _ => String::new(),
    }
}
fn b(v: &Value) -> bool {
    matches!(v, Value::Toggle(true))
}

pub fn apply(cfg: &mut Config, id: &str, v: &Value) -> Effect {
    match id {
        "mode" => return Effect::Mode(if idx(v) == 1 { Mode::Gamepad } else { Mode::Desktop }),
        "save" => return Effect::Save,
        "preset.new" => return Effect::PresetNew(text(v)),
        "preset.save" => return Effect::PresetSave,
        "controller.name" => return Effect::ControllerName(text(v)),
        "power_off" => return Effect::PowerOff,
        "preset.save_default" => return Effect::PresetSaveDefault,
        "preset.discard" => return Effect::PresetDiscard,
        "preset.rename" => return Effect::PresetRename(text(v)),
        "preset.delete" => return Effect::PresetDelete,
        id if id.starts_with("preset.app.") => return Effect::PresetApp(id["preset.app.".len()..].to_string(), b(v)),
        "preset.load_default" => return Effect::PresetLoad(None),
        id if id.starts_with("preset.load.") => {
            return Effect::PresetLoad(Some(id["preset.load.".len()..].to_string()));
        }
        // Settings only: the preset part ([gamepad]) stays.
        "defaults" => {
            let gp = cfg.gamepad.clone();
            *cfg = Config::default();
            cfg.gamepad = gp;
        }
        "gamepad.profile" => cfg.gamepad.profile = PROFILES[idx(v).min(PROFILES.len() - 1)].0,
        "gamepad.motion" => cfg.gamepad.motion = MOTION[idx(v).min(MOTION.len() - 1)].0,
        "gamepad.rumble_strength" => cfg.gamepad.rumble_strength = f(v),
        "gamepad.rumble_swap" => cfg.gamepad.rumble_swap = b(v),
        "gamepad.trackpad_mouse" => cfg.gamepad.trackpad_mouse = b(v),
        "desktop.pointer_speed" => cfg.desktop.pointer_speed = f(v),
        "desktop.pointer_accel" => cfg.desktop.pointer_accel = f(v),
        "desktop.inertia" => cfg.desktop.inertia = b(v),
        "desktop.scroll_speed" => cfg.desktop.scroll_speed = f(v),
        "desktop.natural_scroll" => cfg.desktop.natural_scroll = b(v),
        "desktop.gyro_mouse" => cfg.desktop.gyro_mouse = b(v),
        "haptics.enabled" => cfg.haptics.enabled = b(v),
        "haptics.pointer_ticks" => cfg.haptics.pointer_ticks = b(v),
        "general.backoff" => cfg.general.backoff = BACKOFF[idx(v).min(BACKOFF.len() - 1)].0,
        "general.preset_switch" => cfg.general.preset_switch = PRESET_SWITCH[idx(v).min(PRESET_SWITCH.len() - 1)].0,
        "remap.clear" => cfg.gamepad.remap.clear(),
        id if id.starts_with("remap.") => {
            let src = &id["remap.".len()..];
            let remap = &mut cfg.gamepad.remap;
            match idx(v) {
                0 => {
                    remap.remove(src);
                }
                1 => {
                    remap.insert(src.to_string(), "none".into());
                }
                i => match MAPPABLE.get(i - 2) {
                    Some((t, _)) if *t == src => {
                        remap.remove(src);
                    }
                    Some((t, _)) => {
                        remap.insert(src.to_string(), t.to_string());
                    }
                    None => return Effect::None,
                },
            }
        }
        _ => return Effect::None,
    }
    Effect::Config
}

fn round1(x: f32) -> f64 {
    (x as f64 * 10.0).round() / 10.0
}

fn open_doc(path: &Path) -> Result<toml_edit::DocumentMut, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    text.parse().map_err(|e| format!("{}: {e}", path.display()))
}

fn write_doc(doc: &toml_edit::DocumentMut, path: &Path) -> Result<(), String> {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn set(doc: &mut toml_edit::DocumentMut, section: &str, key: &str, v: toml_edit::Item) {
    if !doc.contains_table(section) {
        doc.insert(section, toml_edit::Item::Table(toml_edit::Table::new()));
    }
    doc[section][key] = v;
}

/// Write the shared settings (not the preset part) into the config file, keeping
/// everything else (comments, other keys) as it is.
pub fn save(cfg: &Config, path: &Path) -> Result<(), String> {
    use toml_edit::value;
    let mut doc = open_doc(path)?;
    let backoff = BACKOFF.iter().find(|(m, _)| *m == cfg.general.backoff).map(|m| m.1).unwrap_or("any");
    let pswitch = match cfg.general.preset_switch {
        PresetSwitch::Ask => "ask",
        PresetSwitch::Auto => "auto",
        PresetSwitch::Off => "off",
    };
    let d = &mut doc;
    set(d, "desktop", "pointer_speed", value(round1(cfg.desktop.pointer_speed)));
    set(d, "desktop", "pointer_accel", value(round1(cfg.desktop.pointer_accel)));
    set(d, "desktop", "inertia", value(cfg.desktop.inertia));
    set(d, "desktop", "scroll_speed", value(round1(cfg.desktop.scroll_speed)));
    set(d, "desktop", "natural_scroll", value(cfg.desktop.natural_scroll));
    set(d, "desktop", "gyro_mouse", value(cfg.desktop.gyro_mouse));
    set(d, "haptics", "enabled", value(cfg.haptics.enabled));
    set(d, "haptics", "pointer_ticks", value(cfg.haptics.pointer_ticks));
    set(d, "general", "backoff", value(backoff));
    set(d, "general", "preset_switch", value(pswitch));
    write_doc(&doc, path)?;
    save_names(cfg, path)
}

/// Write the controller names ([names]) into the config file.
pub fn save_names(cfg: &Config, path: &Path) -> Result<(), String> {
    let mut doc = open_doc(path)?;
    let mut t = toml_edit::Table::new();
    for (serial, name) in &cfg.names {
        t[serial.as_str()] = toml_edit::value(name.as_str());
    }
    if cfg.names.is_empty() {
        doc.remove("names");
    } else {
        doc["names"] = toml_edit::Item::Table(t);
    }
    write_doc(&doc, path)
}

/// Write the Default preset ([gamepad]) into the config file.
pub fn save_default_preset(gp: &crate::config::Gamepad, path: &Path) -> Result<(), String> {
    use toml_edit::value;
    let mut doc = open_doc(path)?;
    let profile = PROFILES.iter().find(|(p, _, _)| *p == gp.profile).map(|p| p.1).unwrap_or("xbox360");
    let motion = MOTION.iter().find(|(m, _)| *m == gp.motion).map(|m| m.1).unwrap_or("auto");
    let d = &mut doc;
    set(d, "gamepad", "profile", value(profile));
    set(d, "gamepad", "motion", value(motion));
    set(d, "gamepad", "rumble_strength", value(round1(gp.rumble_strength)));
    set(d, "gamepad", "rumble_swap", value(gp.rumble_swap));
    set(d, "gamepad", "trackpad_mouse", value(gp.trackpad_mouse));
    write_doc(&doc, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Presets {
        Presets::load(Path::new("/nonexistent-puckd-test"), Config::default().gamepad)
    }

    #[test]
    fn apply_and_save_roundtrip() {
        let mut cfg = Config::default();
        let its = items(&cfg, Mode::Desktop, &store(), Some("FXA1"));
        assert!(its.iter().any(|i| i.id == "controller.name"));
        let mut prof = its.iter().find(|i| i.id == "gamepad.profile").unwrap().value.clone();
        prof.step(1);
        assert_eq!(apply(&mut cfg, "gamepad.profile", &prof), Effect::Config);
        assert_eq!(cfg.gamepad.profile, Profile::XboxElite);
        let mut sp = its.iter().find(|i| i.id == "desktop.pointer_speed").unwrap().value.clone();
        sp.step(-1);
        apply(&mut cfg, "desktop.pointer_speed", &sp);
        assert!((cfg.desktop.pointer_speed - 0.9).abs() < 1e-4);
        let mut l4 = mapping(&cfg, &store()).1.into_iter().find(|i| i.id == "remap.l4").unwrap().value;
        l4.step(2); // itself -> nothing -> A
        apply(&mut cfg, "remap.l4", &l4);
        assert_eq!(cfg.gamepad.remap.get("l4").map(String::as_str), Some("a"));

        let dir = std::env::temp_dir().join(format!("puckd-test-{}", std::process::id()));
        let path = dir.join("puckd.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "# my comment\n[desktop]\nsmoothing = 0.3 # keep\n").unwrap();
        save(&cfg, &path).unwrap();
        save_default_preset(&cfg.gamepad, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my comment") && text.contains("# keep"));
        let back = Config::load(&path).unwrap();
        assert_eq!(back.gamepad.profile, Profile::XboxElite);
        assert!((back.desktop.pointer_speed - 0.9).abs() < 1e-4);
        assert!((back.desktop.smoothing - 0.3).abs() < 1e-4);
        // The Default preset never gets a mapping written.
        assert!(back.gamepad.remap.is_empty());
        std::fs::remove_dir_all(dir).unwrap();

        // Restoring default settings keeps the preset part.
        apply(&mut cfg, "defaults", &Value::Button);
        assert_eq!(cfg.desktop, Config::default().desktop);
        assert_eq!(cfg.gamepad.profile, Profile::XboxElite);
    }

    #[test]
    fn preset_entries() {
        let mut cfg = Config::default();
        assert_eq!(apply(&mut cfg, "preset.load.Racing", &Value::Button), Effect::PresetLoad(Some("Racing".into())));
        assert_eq!(apply(&mut cfg, "preset.new", &Value::Text("X".into())), Effect::PresetNew("X".into()));
        let p = store();
        let (title, items) = presets(&cfg, &p, Some(("org.mozilla.firefox", "Firefox")));
        assert!(title.contains("Default"));
        assert_eq!(items[0].id, "preset.new");
        assert_eq!(items[0].value, Value::Text("firefox".into()));
        cfg.gamepad.remap.insert("a".into(), "b".into());
        let (title, items) = presets(&cfg, &p, None);
        assert!(title.contains("new preset"));
        assert!(!items.iter().any(|i| i.id == "preset.save_default"));
    }

    #[test]
    fn apps_list() {
        let dir = std::env::temp_dir().join(format!("puckd-apps-{}", std::process::id()));
        let cfg = Config::default();
        let mut p = Presets::load(&dir, cfg.gamepad.clone());
        p.save("Racing", &cfg.gamepad).unwrap();
        p.set_app("title:Some Game", "Racing", true).unwrap();
        let apps = |p: &Presets, focused| {
            presets(&cfg, p, focused)
                .1
                .into_iter()
                .filter(|i| i.id.starts_with("preset.app."))
                .map(|i| (i.id, i.value))
                .collect::<Vec<_>>()
        };
        // Focused app first (off: not linked yet), then the linked one.
        let l = apps(&p, Some(("firefox", "Firefox")));
        assert_eq!(l[0], ("preset.app.firefox".to_string(), Value::Toggle(false)));
        assert_eq!(l[1], ("preset.app.title:Some Game".to_string(), Value::Toggle(true)));
        // Focused + linked: listed once, on.
        let l = apps(&p, Some(("title:Some Game", "Some Game")));
        assert_eq!(l, vec![("preset.app.title:Some Game".to_string(), Value::Toggle(true))]);
        // Turning a non-focused app off removes it from the list.
        let mut cfg2 = cfg.clone();
        let e = apply(&mut cfg2, "preset.app.title:Some Game", &Value::Toggle(false));
        assert_eq!(e, Effect::PresetApp("title:Some Game".into(), false));
        p.set_app("title:Some Game", "Racing", false).unwrap();
        assert_eq!(apps(&p, Some(("firefox", "Firefox"))).len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

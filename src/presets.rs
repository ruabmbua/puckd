//! Presets: how the controller appears to games (emulated profile, motion device, rumble,
//! button mapping), i.e. the `[gamepad]` part of the config. Everything else is a setting
//! shared by all presets.
//!
//! - "Default" always exists: the `[gamepad]` section of puckd.toml. Its button mapping is
//!   never changed from the overlay; remapping while on Default makes a new, unsaved preset.
//! - Named presets live in `$XDG_CONFIG_HOME/puckd/presets/<name>.toml`.
//! - App associations (`"<wayland app_id>" = "<preset>"`) live in `.../puckd/app-presets.toml`.
//!   Focusing such an app offers (or applies) its preset; leaving it again goes back to the
//!   preset that was last chosen by hand.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use log::warn;

use crate::config::{Config, Gamepad};

pub const DEFAULT_NAME: &str = "Default";

/// How the current gamepad settings relate to the active preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Clean,
    /// Changed, and the changes can be saved into the active preset.
    Modified,
    /// Default with a changed button mapping: only "save as new preset" applies.
    NewFromDefault,
}

pub struct Presets {
    dir: PathBuf,
    pub named: BTreeMap<String, Gamepad>,
    /// app_id -> preset name
    pub apps: BTreeMap<String, String>,
    /// The Default preset (puckd.toml's [gamepad]).
    pub default: Gamepad,
    /// Active preset; None = Default.
    pub active: Option<String>,
    /// Last preset chosen by hand (what leaving an app's preset returns to).
    pub manual: Option<String>,
    /// App whose focus activated the current preset.
    pub auto_app: Option<String>,
    /// Per-app answers to the "switch preset?" popup, for this session.
    pub decided: HashMap<String, bool>,
}

/// How an app key shows up in the menu: windows without an app_id are keyed by title.
pub fn app_display(app: &str, title: &str) -> String {
    if let Some(t) = app.strip_prefix("title:") {
        return t.to_string();
    }
    if title.is_empty() || title == app { app.to_string() } else { format!("{title} ({app})") }
}

pub fn default_dir() -> PathBuf {
    crate::config::default_path().parent().map(|p| p.join("puckd")).unwrap_or_else(|| PathBuf::from("puckd"))
}

fn clean_name(n: &str) -> String {
    let s: String = n.chars().filter(|c| !matches!(c, '/' | '\\' | '\0') && !c.is_control()).collect();
    let s = s.trim().trim_start_matches('.').trim();
    s.chars().take(48).collect()
}

/// A preset file: just the gamepad section (older files held a whole config).
fn load_preset(p: &Path) -> Result<Gamepad, String> {
    let text = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
    if let Ok(g) = toml::from_str::<Gamepad>(&text) {
        return Ok(g);
    }
    toml::from_str::<Config>(&text).map(|c| c.gamepad).map_err(|e| e.to_string())
}

impl Presets {
    pub fn load(dir: &Path, default: Gamepad) -> Self {
        let mut named = BTreeMap::new();
        if let Ok(rd) = std::fs::read_dir(dir.join("presets")) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("toml") {
                    continue;
                }
                let Some(name) = p.file_stem().and_then(|x| x.to_str()).map(str::to_string) else { continue };
                match load_preset(&p) {
                    Ok(g) => {
                        named.insert(name, g);
                    }
                    Err(e) => warn!("preset {}: {e}", p.display()),
                }
            }
        }
        let apps = std::fs::read_to_string(dir.join("app-presets.toml"))
            .ok()
            .and_then(|t| toml::from_str::<BTreeMap<String, String>>(&t).ok())
            .unwrap_or_default();
        Self {
            dir: dir.to_path_buf(),
            named,
            apps,
            default,
            active: None,
            manual: None,
            auto_app: None,
            decided: HashMap::new(),
        }
    }

    pub fn active_name(&self) -> &str {
        self.active.as_deref().unwrap_or(DEFAULT_NAME)
    }

    /// The saved content of the active preset.
    pub fn saved(&self) -> &Gamepad {
        self.active.as_ref().and_then(|n| self.named.get(n)).unwrap_or(&self.default)
    }

    pub fn get(&self, name: Option<&str>) -> Option<&Gamepad> {
        match name {
            None => Some(&self.default),
            Some(n) => self.named.get(n),
        }
    }

    pub fn state(&self, cur: &Gamepad) -> State {
        let saved = self.saved();
        if cur == saved {
            State::Clean
        } else if self.active.is_none() && cur.remap != saved.remap {
            State::NewFromDefault
        } else {
            State::Modified
        }
    }

    /// One-line description, e.g. "Racing", "Racing (modified)", "new preset (unsaved)".
    pub fn describe(&self, cur: &Gamepad) -> String {
        match self.state(cur) {
            State::Clean => self.active_name().to_string(),
            State::Modified => format!("{} (modified)", self.active_name()),
            State::NewFromDefault => "new preset (unsaved)".into(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join("presets").join(format!("{name}.toml"))
    }

    /// A free name based on `want`.
    pub fn unique_name(&self, want: &str) -> String {
        let base = clean_name(want);
        let base = if base.is_empty() { "Preset".to_string() } else { base };
        if base != DEFAULT_NAME && !self.named.contains_key(&base) {
            return base;
        }
        (2..).map(|n| format!("{base} {n}")).find(|n| !self.named.contains_key(n)).unwrap()
    }

    /// Name suggestion for a new preset: the focused app (Steam games by window title).
    pub fn suggest(&self, app: Option<(&str, &str)>) -> String {
        let want = match app {
            Some((id, title)) if (id.starts_with("steam_app_") || id.starts_with("title:")) && !title.is_empty() => {
                title.to_string()
            }
            Some((id, _)) if !id.is_empty() => id.rsplit('.').next().unwrap_or(id).to_string(),
            _ => "Preset".to_string(),
        };
        self.unique_name(&want)
    }

    /// Save `gp` as a named preset (new or existing) and make it active.
    pub fn save(&mut self, name: &str, gp: &Gamepad) -> Result<String, String> {
        let name = clean_name(name);
        if name.is_empty() || name == DEFAULT_NAME {
            return Err(format!("invalid preset name '{name}'"));
        }
        let text = toml::to_string(gp).map_err(|e| e.to_string())?;
        let p = self.path(&name);
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        let header = "# puckd preset: how the controller appears to games ([gamepad] settings).\n";
        std::fs::write(&p, format!("{header}{text}")).map_err(|e| format!("{}: {e}", p.display()))?;
        self.named.insert(name.clone(), gp.clone());
        self.active = Some(name.clone());
        self.manual = Some(name.clone());
        self.auto_app = None;
        Ok(name)
    }

    pub fn rename(&mut self, old: &str, new: &str) -> Result<String, String> {
        let Some(gp) = self.named.get(old).cloned() else { return Err(format!("no preset '{old}'")) };
        let new = clean_name(new);
        if new == old {
            return Ok(new);
        }
        if self.named.contains_key(&new) {
            return Err(format!("a preset named '{new}' exists already"));
        }
        let (active, manual, auto_app) = (self.active.clone(), self.manual.clone(), self.auto_app.clone());
        let new = self.save(&new, &gp)?;
        let _ = std::fs::remove_file(self.path(old));
        self.named.remove(old);
        let fix = |x: Option<String>| x.map(|v| if v == old { new.clone() } else { v });
        self.active = fix(active);
        self.manual = fix(manual);
        self.auto_app = auto_app;
        let mut changed = false;
        for p in self.apps.values_mut() {
            if p == old {
                *p = new.clone();
                changed = true;
            }
        }
        if changed {
            self.save_apps()?;
        }
        Ok(new)
    }

    pub fn delete(&mut self, name: &str) -> Result<(), String> {
        if self.named.remove(name).is_none() {
            return Ok(());
        }
        let _ = std::fs::remove_file(self.path(name));
        for slot in [&mut self.active, &mut self.manual] {
            if slot.as_deref() == Some(name) {
                *slot = None;
            }
        }
        let before = self.apps.len();
        self.apps.retain(|_, p| p != name);
        if self.apps.len() != before {
            self.save_apps()?;
        }
        Ok(())
    }

    /// Make a preset active. `manual` = chosen by hand (not because of a focused app).
    pub fn activate(&mut self, name: Option<&str>, manual: bool) -> Option<Gamepad> {
        let gp = self.get(name)?.clone();
        self.active = name.map(str::to_string);
        if manual {
            self.manual = self.active.clone();
            self.auto_app = None;
        }
        Some(gp)
    }

    /// Link `app` to `preset` (on) or remove its link to `preset` (off).
    pub fn set_app(&mut self, app: &str, preset: &str, on: bool) -> Result<(), String> {
        if on {
            self.apps.insert(app.to_string(), preset.to_string());
        } else if self.apps.get(app).map(String::as_str) == Some(preset) {
            self.apps.remove(app);
        } else {
            return Ok(());
        }
        self.decided.remove(app);
        self.save_apps()
    }

    fn save_apps(&self) -> Result<(), String> {
        let p = self.dir.join("app-presets.toml");
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let text = toml::to_string(&self.apps).map_err(|e| e.to_string())?;
        std::fs::write(&p, format!("# Wayland app_id = preset name (managed by puckd)\n{text}"))
            .map_err(|e| format!("{}: {e}", p.display()))
    }

    /// Preset associated with an app, if it exists.
    pub fn for_app(&self, app: &str) -> Option<&str> {
        self.apps.get(app).filter(|n| self.named.contains_key(*n)).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_and_files() {
        let dir = std::env::temp_dir().join(format!("puckd-presets-{}", std::process::id()));
        let mut p = Presets::load(&dir, Gamepad::default());
        let mut gp = Gamepad::default();
        assert_eq!(p.state(&gp), State::Clean);
        gp.rumble_strength = 0.5;
        assert_eq!(p.state(&gp), State::Modified);
        gp.remap.insert("l4".into(), "a".into());
        assert_eq!(p.state(&gp), State::NewFromDefault);

        assert_eq!(p.suggest(Some(("org.mozilla.firefox", "Mozilla Firefox"))), "firefox");
        assert_eq!(p.suggest(Some(("steam_app_42", "Elden Ring"))), "Elden Ring");
        let n = p.save("Elden Ring", &gp).unwrap();
        assert_eq!(p.state(&gp), State::Clean);
        assert_eq!(p.suggest(Some(("steam_app_42", "Elden Ring"))), "Elden Ring 2");
        p.set_app("steam_app_42", &n, true).unwrap();
        let n2 = p.rename(&n, "ER").unwrap();
        assert_eq!(p.for_app("steam_app_42"), Some("ER"));

        let p2 = Presets::load(&dir, Gamepad::default());
        assert_eq!(p2.named.get(&n2), Some(&gp));
        assert_eq!(p2.for_app("steam_app_42"), Some("ER"));
        let mut p2 = p2;
        p2.delete(&n2).unwrap();
        assert!(p2.for_app("steam_app_42").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

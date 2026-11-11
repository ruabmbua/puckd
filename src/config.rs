//! `~/.config/puckd.toml`. Every field is optional; see `puckd.example.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Desktop,
    Gamepad,
}

impl Mode {
    pub fn toggled(self) -> Self {
        match self {
            Mode::Desktop => Mode::Gamepad,
            Mode::Gamepad => Mode::Desktop,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backoff {
    /// Back off whenever any other process has the controller's hidraw node open.
    Any,
    /// Back off only for Steam.
    Steam,
    /// Never back off.
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    /// "Microsoft X-Box 360 pad" (045e:028e), xpad layout. Works everywhere.
    Xbox360,
    /// "Microsoft X-Box One Elite 2 pad" (045e:0b00), xpad layout plus 4 paddles.
    XboxElite,
    /// Everything: paddles, QAM, trigger clicks, pad clicks, capacitive touches,
    /// analog trackpads and a motion sensor device. Uses SDL's automatic evdev mapping.
    Extended,
}

/// What to do when an app with an associated preset gets focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetSwitch {
    /// Show a popup: A switches, B keeps the current settings.
    Ask,
    /// Switch right away.
    Auto,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Toggle {
    Auto,
    On,
    Off,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub start_mode: Mode,
    /// Button that switches modes when held (e.g. "menu" = right ≡, "view" = left ⧉, "steam", "qam").
    pub switch_button: String,
    pub switch_hold_ms: u64,
    /// Button that opens/closes the on-screen keyboard + settings overlay in any mode
    /// ("none" to disable; desktop mode can also bind "osk" to a button).
    pub osk_button: String,
    pub backoff: Backoff,
    pub backoff_check_ms: u64,
    /// When a window whose app has a preset gets focus: "ask", "auto" or "off".
    pub preset_switch: PresetSwitch,
    /// How often lizard mode is turned off again (firmware watchdog is ~3 s).
    pub keepalive_ms: u64,
    /// Show a banner when a controller connects or disconnects.
    pub notify: bool,
    /// With exclusive access (`puckd broker`): give Steam and games a pass-through clone of
    /// the controller, which they may take over. false: they never see the controller.
    pub passthrough: bool,
    /// Held together for a second: hand the controller between puckd and Steam.
    pub handover_buttons: String,
}

impl Default for General {
    fn default() -> Self {
        Self {
            start_mode: Mode::Desktop,
            switch_button: "menu".into(),
            switch_hold_ms: 600,
            osk_button: "qam".into(),
            backoff: Backoff::Any,
            backoff_check_ms: 2000,
            preset_switch: PresetSwitch::Ask,
            keepalive_ms: 2000,
            notify: true,
            passthrough: true,
            handover_buttons: "view+menu".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Gamepad {
    pub profile: Profile,
    /// Separate "<name> Motion Sensors" device. auto = only with the extended profile.
    pub motion: Toggle,
    /// Multiplier for game rumble (0 disables).
    pub rumble_strength: f32,
    /// Swap the strong/weak motors.
    pub rumble_swap: bool,
    /// Physical button -> physical button it should act as (e.g. `l4 = "a"`), or "none".
    pub remap: BTreeMap<String, String>,
    /// In gamepad mode, let the trackpads move the mouse and scroll (as in desktop mode)
    /// when the emulated controller has no trackpads of its own (the Xbox profiles).
    pub trackpad_mouse: bool,
    /// Override the device name (keeps the profile's ids).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Default for Gamepad {
    fn default() -> Self {
        Self {
            profile: Profile::Xbox360,
            motion: Toggle::Auto,
            rumble_strength: 1.0,
            rumble_swap: false,
            remap: BTreeMap::new(),
            trackpad_mouse: true,
            name: None,
        }
    }
}

impl Gamepad {
    pub fn motion_enabled(&self) -> bool {
        match self.motion {
            Toggle::Auto => self.profile == Profile::Extended,
            Toggle::On => true,
            Toggle::Off => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Desktop {
    /// Right trackpad pointer speed multiplier.
    pub pointer_speed: f32,
    /// Pointer acceleration strength (0 = linear).
    pub pointer_accel: f32,
    /// Position smoothing 0..1 (higher = smoother, more latency).
    pub smoothing: f32,
    /// Keep gliding after a flick.
    pub inertia: bool,
    /// Inertia decay per second (higher stops faster).
    pub inertia_friction: f32,
    /// Left trackpad scroll speed multiplier.
    pub scroll_speed: f32,
    /// Extra scroll speed for fast swipes on the left trackpad (0 = linear).
    pub scroll_accel: f32,
    /// Trackpad output is held back this long so the thumb rolling off the pad when
    /// lifting doesn't move the pointer (0 = off, lowest latency).
    pub liftoff_delay_ms: u64,
    pub natural_scroll: bool,
    pub horizontal_scroll: bool,
    /// Right stick pointer speed in px/s at full deflection (0 = off).
    pub stick_pointer_speed: f32,
    /// Left stick scroll speed in wheel notches/s at full deflection (0 = off).
    pub stick_scroll_speed: f32,
    pub stick_deadzone: f32,
    /// Analog trigger pull (0..1) that counts as pressed for `lt`/`rt` bindings.
    pub trigger_threshold: f32,
    /// Freeze the pointer this long around trackpad clicks (stops click jitter).
    pub click_suppress_ms: u64,
    /// Gyro moves the pointer while a grip is touched.
    pub gyro_mouse: bool,
    /// Pixels per degree of rotation.
    pub gyro_speed: f32,
    /// Button bindings: protocol button name (or "lt"/"rt") -> key combo, mouse button,
    /// "mode" (switch modes) or "none". Merged over the defaults.
    pub buttons: BTreeMap<String, String>,
}

impl Default for Desktop {
    fn default() -> Self {
        Self {
            pointer_speed: 1.0,
            pointer_accel: 1.0,
            smoothing: 0.4,
            inertia: false,
            inertia_friction: 6.0,
            scroll_speed: 1.0,
            scroll_accel: 1.0,
            liftoff_delay_ms: 12,
            natural_scroll: false,
            horizontal_scroll: true,
            stick_pointer_speed: 1200.0,
            stick_scroll_speed: 5.0,
            stick_deadzone: 0.15,
            trigger_threshold: 0.45,
            click_suppress_ms: 150,
            gyro_mouse: false,
            gyro_speed: 12.0,
            buttons: BTreeMap::new(),
        }
    }
}

pub const DEFAULT_DESKTOP_BUTTONS: &[(&str, &str)] = &[
    ("rpad_click", "BTN_LEFT"),
    ("rt", "BTN_LEFT"),
    ("lt", "BTN_RIGHT"),
    ("lpad_click", "BTN_MIDDLE"),
    ("a", "KEY_ENTER"),
    ("b", "KEY_ESC"),
    ("x", "KEY_BACKSPACE"),
    ("y", "KEY_SPACE"),
    ("dpad_up", "KEY_UP"),
    ("dpad_down", "KEY_DOWN"),
    ("dpad_left", "KEY_LEFT"),
    ("dpad_right", "KEY_RIGHT"),
    ("lb", "BTN_SIDE"),
    ("rb", "BTN_EXTRA"),
    ("view", "KEY_TAB"),
    ("l3", "KEY_LEFTMETA"),
    ("r3", "BTN_MIDDLE"),
];

impl Desktop {
    /// Defaults merged with user bindings.
    pub fn bindings(&self) -> BTreeMap<String, String> {
        let mut m: BTreeMap<String, String> =
            DEFAULT_DESKTOP_BUTTONS.iter().map(|&(k, v)| (k.to_string(), v.to_string())).collect();
        for (k, v) in &self.buttons {
            m.insert(k.to_ascii_lowercase(), v.clone());
        }
        m
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Haptics {
    pub enabled: bool,
    /// Click when switching modes.
    pub mode_switch: bool,
    /// Tick while the pointer moves on the right pad.
    pub pointer_ticks: bool,
    /// Pointer travel (px) per tick.
    pub tick_distance: f32,
    /// Tick per scroll wheel notch on the left pad.
    pub scroll_ticks: bool,
    /// Click on trackpad clicks.
    pub pad_clicks: bool,
    /// Gain in dB for ticks/clicks (negative = softer).
    pub gain_db: i8,
}

impl Default for Haptics {
    fn default() -> Self {
        Self {
            enabled: true,
            mode_switch: true,
            pointer_ticks: true,
            tick_distance: 60.0,
            scroll_ticks: true,
            pad_clicks: true,
            gain_db: 0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub gamepad: Gamepad,
    pub desktop: Desktop,
    pub haptics: Haptics,
    /// Controller names by the controller's serial number (shown in the overlay, logs and
    /// status files), e.g. `FXA00000000AA = "Couch"`.
    pub names: BTreeMap<String, String>,
}

pub fn default_path() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(x).join("puckd.toml");
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config/puckd.toml")
}

impl Config {
    /// Load the config. A missing file yields the defaults.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let cfg: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), String> {
        use crate::proto::btn;
        if btn::from_name(&self.general.switch_button).is_none() {
            return Err(format!("general.switch_button: unknown button '{}'", self.general.switch_button));
        }
        let ob = &self.general.osk_button;
        if !ob.eq_ignore_ascii_case("none") && btn::from_name(ob).is_none() {
            return Err(format!("general.osk_button: unknown button '{ob}'"));
        }
        for (k, v) in &self.gamepad.remap {
            if btn::from_name(k).is_none() {
                return Err(format!("gamepad.remap: unknown button '{k}'"));
            }
            if !v.eq_ignore_ascii_case("none") && btn::from_name(v).is_none() {
                return Err(format!("gamepad.remap.{k}: unknown button '{v}'"));
            }
        }
        for (k, v) in self.desktop.bindings() {
            if k != "lt" && k != "rt" && btn::from_name(&k).is_none() {
                return Err(format!("desktop.buttons: unknown button '{k}'"));
            }
            if !matches!(v.to_ascii_lowercase().as_str(), "mode" | "osk") {
                crate::keys::parse_combo(&v).map_err(|e| format!("desktop.buttons.{k}: {e}"))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let text = include_str!("../puckd.example.toml");
        let cfg: Config = toml::from_str(text).unwrap();
        cfg.validate().unwrap();
    }

    #[test]
    fn defaults_validate() {
        Config::default().validate().unwrap();
    }
}

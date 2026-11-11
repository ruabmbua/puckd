//! Physical key grid (evdev keycodes, ISO-style 60%) and labels from the active xkb keymap,
//! so the keys show what the compositor's layout will actually type (QWERTZ, umlauts, ...).

use xkbcommon::xkb;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModKey {
    Shift,
    Ctrl,
    Alt,
    Super,
    AltGr,
}

impl ModKey {
    pub const ALL: [ModKey; 5] = [ModKey::Shift, ModKey::Ctrl, ModKey::Alt, ModKey::Super, ModKey::AltGr];

    pub fn code(self) -> u16 {
        match self {
            ModKey::Shift => 42,
            ModKey::Ctrl => 29,
            ModKey::Alt => 56,
            ModKey::Super => 125,
            ModKey::AltGr => 100,
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Label comes from the keymap.
    Char,
    /// Fixed label (Tab, Enter, arrows...).
    Named(&'static str),
    Mod(ModKey, &'static str),
    Caps,
    /// Switches to the settings tab.
    Settings,
}

#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub code: u16,
    pub kind: Kind,
    /// Position and width in key units; `row` is 0-based.
    pub x: f32,
    pub w: f32,
    pub row: usize,
}

impl Key {
    pub fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
}

pub const COLS: f32 = 15.0;
pub const ROWS: usize = 5;

pub struct Layout {
    pub keys: Vec<Key>,
}

impl Layout {
    pub fn new() -> Self {
        use Kind::*;
        let mut keys = Vec::new();
        let mut row = |r: usize, items: &[(u16, Kind, f32)], start: f32| {
            let mut x = start;
            for &(code, kind, w) in items {
                if code != 0 || !matches!(kind, Named("")) {
                    keys.push(Key { code, kind, x, w, row: r });
                }
                x += w;
            }
        };
        let gap = (0, Named(""), 0.25);
        let chars = |codes: std::ops::RangeInclusive<u16>| codes.map(|c| (c, Char, 1.0)).collect::<Vec<_>>();

        let mut r0 = vec![(1, Named("Esc"), 1.0), (41, Char, 1.0)];
        r0.extend(chars(2..=13));
        r0.push((14, Named("Bksp"), 1.0));
        row(0, &r0, 0.0);

        let mut r1 = vec![(15, Named("Tab"), 1.5)];
        r1.extend(chars(16..=27));
        r1.push((43, Char, 1.5));
        row(1, &r1, 0.0);

        let mut r2 = vec![(58, Caps, 1.75)];
        r2.extend(chars(30..=40));
        r2.push((28, Named("Enter"), 2.25));
        row(2, &r2, 0.0);

        let mut r3 = vec![(42, Mod(ModKey::Shift, "Shift"), 1.75), (86, Char, 1.0)];
        r3.extend(chars(44..=53));
        r3.push(gap);
        r3.push((103, Named("↑"), 1.0));
        r3.push((111, Named("Del"), 1.0));
        row(3, &r3, 0.0);

        let r4 = vec![
            (29, Mod(ModKey::Ctrl, "Ctrl"), 1.25),
            (125, Mod(ModKey::Super, "Super"), 1.25),
            (56, Mod(ModKey::Alt, "Alt"), 1.25),
            (57, Named(""), 5.75),
            (100, Mod(ModKey::AltGr, "AltGr"), 1.25),
            (0, Settings, 1.25),
            (105, Named("←"), 1.0),
            (108, Named("↓"), 1.0),
            (106, Named("→"), 1.0),
        ];
        row(4, &r4, 0.0);
        Self { keys }
    }

    /// Key under a point (key units).
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        let r = y.floor();
        if r < 0.0 || r >= ROWS as f32 {
            return None;
        }
        self.keys.iter().position(|k| k.row == r as usize && x >= k.x && x < k.x + k.w)
    }

    /// Nearest key under a point, clamping into the grid (for trackpad cursors).
    pub fn nearest(&self, x: f32, y: f32) -> usize {
        let r = (y.floor().max(0.0) as usize).min(ROWS - 1);
        let mut best = 0;
        let mut bd = f32::MAX;
        for (i, k) in self.keys.iter().enumerate() {
            if k.row != r {
                continue;
            }
            let d = if x < k.x {
                k.x - x
            } else if x > k.x + k.w {
                x - k.x - k.w
            } else {
                0.0
            };
            if d < bd {
                bd = d;
                best = i;
            }
        }
        best
    }

    /// Key focus movement with the d-pad / stick.
    pub fn neighbor(&self, from: usize, dx: i32, dy: i32) -> usize {
        let k = self.keys[from];
        if dy != 0 {
            let r = k.row as i32 + dy;
            if r < 0 || r >= ROWS as i32 {
                return from;
            }
            return self.nearest(k.cx(), r as f32 + 0.5);
        }
        let row: Vec<usize> = (0..self.keys.len()).filter(|&i| self.keys[i].row == k.row).collect();
        let pos = row.iter().position(|&i| i == from).unwrap_or(0) as i32;
        let n = row.len() as i32;
        row[((pos + dx).rem_euclid(n)) as usize]
    }
}

/// Labels for `Kind::Char` keys, from the compositor's keymap (or the xkb default).
pub struct Labels {
    keymap: Option<xkb::Keymap>,
    group: u32,
}

/// Modifier state that affects labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LabelMods {
    pub shift: bool,
    pub altgr: bool,
    pub caps: bool,
}

impl Labels {
    pub fn fallback() -> Self {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(&ctx, "", "", "", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS);
        Self { keymap, group: 0 }
    }

    pub fn from_string(text: &str) -> Option<Self> {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_string(
            &ctx,
            text.to_string(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )?;
        Some(Self { keymap: Some(keymap), group: 0 })
    }

    pub fn set_group(&mut self, g: u32) -> bool {
        let changed = self.group != g;
        self.group = g;
        changed
    }

    /// Labels for every key of `layout` under `mods` (empty for non-char keys).
    pub fn compute(&self, layout: &Layout, mods: LabelMods) -> Vec<String> {
        let Some(km) = &self.keymap else {
            return layout.keys.iter().map(|_| String::new()).collect();
        };
        let mut st = xkb::State::new(km);
        let bit = |name: &str| {
            let i = km.mod_get_index(name);
            if i == xkb::MOD_INVALID { 0 } else { 1u32 << i }
        };
        let mut dep = 0;
        if mods.shift {
            dep |= bit(xkb::MOD_NAME_SHIFT);
        }
        if mods.altgr {
            dep |= bit(xkb::MOD_NAME_ISO_LEVEL3_SHIFT);
        }
        let locked = if mods.caps { bit(xkb::MOD_NAME_CAPS) } else { 0 };
        st.update_mask(dep, 0, locked, 0, 0, self.group);
        layout
            .keys
            .iter()
            .map(|k| {
                if k.kind != Kind::Char {
                    return String::new();
                }
                let kc = xkb::Keycode::new(k.code as u32 + 8);
                let s = st.key_get_utf8(kc);
                if !s.is_empty() && !s.chars().any(char::is_control) {
                    return s;
                }
                dead_key_label(&xkb::keysym_get_name(st.key_get_one_sym(kc)))
            })
            .collect()
    }
}

fn dead_key_label(name: &str) -> String {
    let s = match name {
        "dead_circumflex" => "^",
        "dead_acute" => "´",
        "dead_grave" => "`",
        "dead_tilde" => "~",
        "dead_diaeresis" => "¨",
        "dead_cedilla" => "¸",
        "dead_abovering" => "°",
        "dead_caron" => "ˇ",
        "dead_macron" => "¯",
        "dead_breve" => "˘",
        "dead_doubleacute" => "˝",
        "dead_ogonek" => "˛",
        "dead_abovedot" => "˙",
        _ => "",
    };
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_fit() {
        let l = Layout::new();
        for r in 0..ROWS {
            let end = l.keys.iter().filter(|k| k.row == r).map(|k| k.x + k.w).fold(0.0, f32::max);
            assert!((end - COLS).abs() < 1e-4, "row {r} ends at {end}");
        }
        // q is below 1-ish, navigation moves between rows
        let q = l.keys.iter().position(|k| k.code == 16).unwrap();
        let up = l.neighbor(q, 0, -1);
        assert_eq!(l.keys[up].row, 0);
        assert_eq!(l.hit(0.5, 0.5).map(|i| l.keys[i].code), Some(1));
    }

    #[test]
    fn us_labels() {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let Some(km) = xkb::Keymap::new_from_names(&ctx, "evdev", "pc105", "de", "", None, 0) else { return };
        let labels = Labels { keymap: Some(km), group: 0 };
        let l = Layout::new();
        let v = labels.compute(&l, LabelMods::default());
        let at = |code| v[l.keys.iter().position(|k| k.code == code).unwrap()].clone();
        assert_eq!(at(21), "z"); // QWERTZ
        assert_eq!(at(39), "ö");
        assert_eq!(at(41), "^");
        let s = labels.compute(&l, LabelMods { altgr: true, ..Default::default() });
        assert_eq!(s[l.keys.iter().position(|k| k.code == 16).unwrap()], "@");
    }
}

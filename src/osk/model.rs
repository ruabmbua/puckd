//! Overlay state and controller handling, independent of Wayland (unit-testable).
//!
//! Keyboard tab:
//! - left/right trackpad: a cursor over the left/right part of the keyboard;
//!   pad click or trigger types the key under that cursor
//! - d-pad / left stick: move the focus; A types the focused key
//! - B backspace, X space, Y shift, Menu (short press) enter, LB/RB switch tabs
//!
//! Controller tab and its sub-pages (presets, button mapping): d-pad/stick up/down selects,
//! left/right changes, A activates, B goes back (sub-page -> Controller -> Keyboard).
//! LB/RB switch between Keyboard and Controller.
//!
//! Button mapping page: A on an entry waits for the button it should act as (5 s timeout),
//! X opens a list to pick from, Y resets the entry to "itself".
//!
//! Input test page: shows the raw controller state; every button is shown instead of acting,
//! so LB + RB together leave it (QAM, owned by the daemon, closes the overlay everywhere).

use std::time::{Duration, Instant};

use crate::osk::layout::{COLS, Kind, LabelMods, Labels, Layout, ModKey, ROWS};
use crate::osk::{Event, Item, Status, Tab, Value};
use crate::proto::{Side, State, btn};

const NAV_DELAY: Duration = Duration::from_millis(350);
pub const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
const NAV_REPEAT: Duration = Duration::from_millis(90);
const KEY_BACKSPACE: u16 = 14;
const KEY_SPACE: u16 = 57;
const KEY_ENTER: u16 = 28;
const KEY_CAPSLOCK: u16 = 58;
const KEY_ESC: u16 = 1;
const KEY_DELETE: u16 = 111;

/// Horizontal range (key units) each trackpad covers.
const LEFT_PAD_SPAN: (f32, f32) = (0.0, 8.5);
const RIGHT_PAD_SPAN: (f32, f32) = (6.5, COLS);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Latch {
    #[default]
    Off,
    /// Applies to the next key only.
    Latched,
    Locked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Src {
    A,
    LeftPad,
    RightPad,
    B,
    X,
    Enter,
    Pointer,
}

/// Typing a name (e.g. a new preset's) with the overlay keyboard: keys go into this text
/// instead of to the focused application.
#[derive(Debug, Clone, PartialEq)]
pub struct NameEntry {
    /// Settings entry the name is for (sent back as `Event::Set` with `Value::Text`).
    pub id: String,
    pub label: String,
    pub text: String,
    /// Page to return to.
    pub back: Tab,
}

/// An edit to the name being typed (from the overlay keys or a real keyboard).
#[derive(Debug, Clone, PartialEq)]
pub enum EntryInput {
    Text(String),
    Backspace,
    Clear,
    Confirm,
    Cancel,
}

/// Editing one entry of the button mapping page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MapEdit {
    /// Waiting for the user to press the target button.
    Capture { entry: usize, until: Instant },
    /// Picking the target from the list of options.
    List { entry: usize, focus: usize },
}

#[derive(Default)]
struct Nav {
    dir: (i32, i32),
    next: Option<Instant>,
}

pub struct Model {
    pub layout: Layout,
    labels: Labels,
    pub label_text: Vec<String>,
    label_mods: LabelMods,
    pub tab: Tab,
    pub focus: usize,
    /// Trackpad cursors in key units (left, right).
    pub cursor: [Option<(f32, f32)>; 2],
    pub hover: [Option<usize>; 2],
    pub mods: [Latch; 5],
    pub caps: bool,
    /// Keys held per source: (source, key index or None, codes sent down).
    held: Vec<(Src, Option<usize>, Vec<u16>)>,
    pub status: Status,
    pub item_focus: usize,
    pub map_focus: usize,
    pub preset_focus: usize,
    pub map_edit: Option<MapEdit>,
    pub entry: Option<NameEntry>,
    /// Latest raw controller state (input test tab).
    pub raw: State,
    prev: u32,
    lt_on: bool,
    rt_on: bool,
    nav: Nav,
    pub dirty: bool,
}

/// Nearest entry at or after `i` (in direction `dir`, falling back to the other one)
/// that is not a heading.
fn selectable(items: &[Item], i: usize, dir: i32) -> usize {
    let n = items.len() as i32;
    if n == 0 {
        return 0;
    }
    let ok = |j: i32| j >= 0 && j < n && !matches!(items[j as usize].value, Value::Header);
    let d = if dir < 0 { -1 } else { 1 };
    let mut j = (i as i32).clamp(0, n - 1);
    while j >= 0 && j < n {
        if ok(j) {
            return j as usize;
        }
        j += d;
    }
    let mut j = (i as i32).clamp(0, n - 1);
    while j >= 0 && j < n {
        if ok(j) {
            return j as usize;
        }
        j -= d;
    }
    i.min(n as usize - 1)
}

fn side_of(src: Src) -> Side {
    match src {
        Src::LeftPad => Side::Left,
        Src::RightPad => Side::Right,
        _ => Side::Both,
    }
}

impl Model {
    pub fn new(labels: Labels) -> Self {
        let layout = Layout::new();
        let focus = layout.keys.iter().position(|k| k.code == 35).unwrap_or(0); // 'h'
        let mut m = Self {
            layout,
            labels,
            label_text: Vec::new(),
            label_mods: LabelMods::default(),
            tab: Tab::Keyboard,
            focus,
            cursor: [None, None],
            hover: [None, None],
            mods: [Latch::Off; 5],
            caps: false,
            held: Vec::new(),
            status: Status::default(),
            item_focus: 0,
            map_focus: 0,
            preset_focus: 0,
            map_edit: None,
            entry: None,
            raw: State::default(),
            prev: 0,
            lt_on: false,
            rt_on: false,
            nav: Nav::default(),
            dirty: true,
        };
        m.refresh_labels(true);
        m
    }

    pub fn set_labels(&mut self, labels: Labels) {
        self.labels = labels;
        self.refresh_labels(true);
    }

    pub fn set_group(&mut self, g: u32) {
        if self.labels.set_group(g) {
            self.refresh_labels(true);
        }
    }

    fn refresh_labels(&mut self, force: bool) {
        let m = LabelMods {
            shift: self.mods[ModKey::Shift.index()] != Latch::Off,
            altgr: self.mods[ModKey::AltGr.index()] != Latch::Off,
            caps: self.caps,
        };
        if force || m != self.label_mods || self.label_text.is_empty() {
            self.label_mods = m;
            self.label_text = self.labels.compute(&self.layout, m);
            self.dirty = true;
        }
    }

    pub fn set_status(&mut self, s: Status) {
        if s != self.status {
            self.status = s;
            self.item_focus = self.item_focus.min(self.status.items.len().saturating_sub(1));
            self.map_focus = self.map_focus.min(self.status.mapping.len().saturating_sub(1));
            self.preset_focus = self.preset_focus.min(self.status.presets.len().saturating_sub(1));
            self.item_focus = selectable(&self.status.items, self.item_focus, 1);
            self.map_focus = selectable(&self.status.mapping, self.map_focus, 1);
            self.preset_focus = selectable(&self.status.presets, self.preset_focus, 1);
            self.dirty = true;
        }
    }

    /// Fresh start when the overlay is shown.
    /// Show the overlay; `None` keeps the page and selection from last time, so you can
    /// go back and forth between a game and e.g. the button mapping page.
    pub fn show(&mut self, tab: Option<Tab>, now: Instant) {
        if let Some(t) = tab {
            self.tab = t;
        }
        self.map_edit = None;
        self.cursor = [None, None];
        self.hover = [None, None];
        // Ignore buttons that are already down (e.g. the one that opened us).
        self.prev = u32::MAX;
        self.nav = Nav { dir: (0, 0), next: Some(now) };
        self.dirty = true;
    }

    pub fn is_down(&self, idx: usize) -> bool {
        self.held.iter().any(|(_, k, _)| *k == Some(idx))
    }

    // ---- key presses ----

    /// A key while typing a name: edit the text instead of sending the key.
    fn entry_key(&mut self, src: Src, key: Option<usize>, code: u16, out: &mut Vec<Event>) {
        let input = match code {
            KEY_BACKSPACE => EntryInput::Backspace,
            KEY_DELETE => EntryInput::Clear,
            KEY_SPACE => EntryInput::Text(" ".into()),
            KEY_ENTER => EntryInput::Confirm,
            KEY_ESC => EntryInput::Cancel,
            _ => match key {
                Some(k) if self.layout.keys[k].kind == Kind::Char => {
                    EntryInput::Text(self.label_text.get(k).cloned().unwrap_or_default())
                }
                _ => EntryInput::Text(String::new()),
            },
        };
        self.entry_input(input, out);
        out.push(Event::Haptic { click: true, side: side_of(src) });
        // One-shot modifiers are used up by the key.
        for l in &mut self.mods {
            if *l == Latch::Latched {
                *l = Latch::Off;
            }
        }
        self.refresh_labels(false);
        self.dirty = true;
    }

    /// Apply an edit to the name being typed (no-op when not typing a name).
    pub fn entry_input(&mut self, input: EntryInput, out: &mut Vec<Event>) {
        let Some(e) = &mut self.entry else { return };
        match input {
            EntryInput::Text(t) => {
                for c in t.chars().filter(|c| !c.is_control()) {
                    if e.text.chars().count() < 48 {
                        e.text.push(c);
                    }
                }
            }
            EntryInput::Backspace => {
                e.text.pop();
            }
            EntryInput::Clear => e.text.clear(),
            EntryInput::Confirm => {
                let e = self.entry.take().unwrap();
                out.push(Event::Set { id: e.id, value: Value::Text(e.text) });
                self.set_tab(e.back, out);
            }
            EntryInput::Cancel => {
                let back = self.entry.take().unwrap().back;
                self.set_tab(back, out);
            }
        }
        self.dirty = true;
    }

    fn press_codes(&mut self, src: Src, key: Option<usize>, code: u16, out: &mut Vec<Event>) {
        if self.entry.is_some() {
            self.entry_key(src, key, code, out);
            return;
        }
        let mut codes: Vec<u16> =
            ModKey::ALL.iter().filter(|m| self.mods[m.index()] != Latch::Off).map(|m| m.code()).collect();
        codes.push(code);
        for &c in &codes {
            out.push(Event::Key { code: c, down: true });
        }
        self.held.push((src, key, codes));
        out.push(Event::Haptic { click: true, side: side_of(src) });
        self.dirty = true;
    }

    fn press_key(&mut self, src: Src, idx: usize, out: &mut Vec<Event>) {
        let k = self.layout.keys[idx];
        match k.kind {
            Kind::Mod(m, _) => {
                let l = &mut self.mods[m.index()];
                *l = match *l {
                    Latch::Off => Latch::Latched,
                    Latch::Latched => Latch::Locked,
                    Latch::Locked => Latch::Off,
                };
                out.push(Event::Haptic { click: true, side: side_of(src) });
                self.refresh_labels(false);
                self.dirty = true;
            }
            Kind::Caps => {
                if self.entry.is_none() {
                    out.push(Event::Key { code: KEY_CAPSLOCK, down: true });
                    out.push(Event::Key { code: KEY_CAPSLOCK, down: false });
                }
                out.push(Event::Haptic { click: true, side: side_of(src) });
                self.caps = !self.caps;
                self.refresh_labels(false);
            }
            Kind::Settings => self.set_tab(Tab::Settings, out),
            Kind::Char | Kind::Named(_) => self.press_codes(src, Some(idx), k.code, out),
        }
    }

    fn release(&mut self, src: Src, out: &mut Vec<Event>) {
        let Some(pos) = self.held.iter().position(|(s, _, _)| *s == src) else { return };
        let (_, _, codes) = self.held.remove(pos);
        for &c in codes.iter().rev() {
            out.push(Event::Key { code: c, down: false });
        }
        // One-shot modifiers are used up by the key.
        let mut changed = false;
        for l in &mut self.mods {
            if *l == Latch::Latched {
                *l = Latch::Off;
                changed = true;
            }
        }
        if changed {
            self.refresh_labels(false);
        }
        self.dirty = true;
    }

    /// Release every held key (on hide or tab switch).
    pub fn release_all(&mut self, out: &mut Vec<Event>) {
        while let Some(src) = self.held.first().map(|h| h.0) {
            self.release(src, out);
        }
    }

    pub fn hide(&mut self, out: &mut Vec<Event>) {
        self.release_all(out);
        self.map_edit = None;
        // An unfinished name is dropped; reopening shows the page it was started from.
        if let Some(e) = self.entry.take() {
            self.tab = e.back;
        }
        self.mods = [Latch::Off; 5];
        self.refresh_labels(false);
    }

    // ---- pointer (mouse / touch on the overlay) ----

    pub fn pointer_key(&mut self, idx: Option<usize>, out: &mut Vec<Event>) {
        if let Some(i) = idx {
            self.press_key(Src::Pointer, i, out);
        }
    }

    pub fn pointer_release(&mut self, out: &mut Vec<Event>) {
        self.release(Src::Pointer, out);
    }

    pub fn pointer_item(&mut self, i: usize, out: &mut Vec<Event>) {
        if let Some(MapEdit::List { entry, .. }) = self.map_edit {
            self.set_mapping(entry, i, out);
            self.map_edit = None;
            return;
        }
        if i < self.list().0.len() && !matches!(self.list().0[i].value, Value::Header) {
            *self.list_mut().1 = i;
            if self.tab == Tab::Mapping && self.is_remap_entry(i) {
                self.open_map_list(i);
            } else {
                self.activate_item(1, out);
            }
        }
    }

    // ---- button mapping editor ----

    fn is_remap_entry(&self, i: usize) -> bool {
        self.status.mapping.get(i).is_some_and(|it| it.id.starts_with("remap.") && it.id != "remap.clear")
    }

    /// Options of a mapping entry ("itself", "nothing", buttons...) and the current one.
    pub fn map_options(&self, entry: usize) -> (&[String], usize) {
        match self.status.mapping.get(entry).map(|it| &it.value) {
            Some(Value::Choice { idx, options }) => (options, *idx),
            _ => (&[], 0),
        }
    }

    fn open_map_list(&mut self, entry: usize) {
        let (_, cur) = self.map_options(entry);
        self.map_edit = Some(MapEdit::List { entry, focus: cur });
        self.dirty = true;
    }

    fn set_mapping(&mut self, entry: usize, choice: usize, out: &mut Vec<Event>) {
        let Some(it) = self.status.mapping.get_mut(entry) else { return };
        if let Value::Choice { idx, options } = &mut it.value
            && choice < options.len()
        {
            *idx = choice;
            out.push(Event::Set { id: it.id.clone(), value: it.value.clone() });
            out.push(Event::Haptic { click: true, side: Side::Both });
        }
        self.dirty = true;
    }

    /// Choice index for a physical button bit: entries follow the option order
    /// ("itself", "nothing", then the buttons in entry order).
    fn choice_for_bit(&self, bit: u32) -> Option<usize> {
        self.status
            .mapping
            .iter()
            .position(|it| it.id.strip_prefix("remap.").and_then(btn::from_name) == Some(bit))
            .map(|i| i + 2)
    }

    /// Handle input while editing a mapping entry. Uses raw buttons (every button can be a target).
    fn map_edit_input(
        &mut self,
        edit: MapEdit,
        raw_pressed: u32,
        step: Option<(i32, i32)>,
        now: Instant,
        out: &mut Vec<Event>,
    ) {
        match edit {
            MapEdit::Capture { entry, until } => {
                if now >= until {
                    self.map_edit = None;
                    self.dirty = true;
                    return;
                }
                // Lowest set bit that names a mappable button.
                let mut bits = raw_pressed;
                while bits != 0 {
                    let bit = bits.isolate_lowest_one();
                    bits &= !bit;
                    if let Some(c) = self.choice_for_bit(bit) {
                        self.set_mapping(entry, c, out);
                        self.map_edit = None;
                        return;
                    }
                }
                self.dirty = true; // countdown
            }
            MapEdit::List { entry, focus } => {
                let n = self.map_options(entry).0.len();
                if raw_pressed & btn::A != 0 {
                    self.set_mapping(entry, focus, out);
                    self.map_edit = None;
                } else if raw_pressed & btn::B != 0 {
                    self.map_edit = None;
                    self.dirty = true;
                } else if let Some((dx, dy)) = step
                    && n > 0
                {
                    let f = crate::osk::picker::step(focus, dx, dy).min(n - 1);
                    self.map_edit = Some(MapEdit::List { entry, focus: f });
                    self.dirty = true;
                    out.push(Event::Haptic { click: false, side: Side::Both });
                }
            }
        }
    }

    /// The entries and focus of the current list tab.
    pub fn list(&self) -> (&[Item], usize) {
        match self.tab {
            Tab::Mapping => (&self.status.mapping, self.map_focus),
            Tab::Presets => (&self.status.presets, self.preset_focus),
            _ => (&self.status.items, self.item_focus),
        }
    }

    fn list_mut(&mut self) -> (&mut Vec<Item>, &mut usize) {
        match self.tab {
            Tab::Mapping => (&mut self.status.mapping, &mut self.map_focus),
            Tab::Presets => (&mut self.status.presets, &mut self.preset_focus),
            _ => (&mut self.status.items, &mut self.item_focus),
        }
    }

    pub fn set_tab(&mut self, tab: Tab, out: &mut Vec<Event>) {
        self.map_edit = None;
        self.entry = None;
        if self.tab != tab {
            self.release_all(out);
            self.tab = tab;
            self.dirty = true;
        }
    }

    // ---- settings ----

    fn activate_item(&mut self, dir: i32, out: &mut Vec<Event>) {
        let (items, focus) = self.list_mut();
        let f = *focus;
        let Some(it) = items.get_mut(f) else { return };
        // Links to sub-pages are handled here, not by the daemon.
        let page = match it.id.as_str() {
            "page.presets" => Some(Tab::Presets),
            "page.mapping" => Some(Tab::Mapping),
            "page.input" => Some(Tab::Input),
            _ => None,
        };
        if let Some(p) = page {
            if dir > 0 {
                self.set_tab(p, out);
                out.push(Event::Haptic { click: true, side: Side::Both });
            }
            return;
        }
        if let Value::Text(prefill) = &it.value {
            if dir > 0 {
                let e = NameEntry { id: it.id.clone(), label: it.label.clone(), text: prefill.clone(), back: self.tab };
                self.set_tab(Tab::Keyboard, out);
                self.entry = Some(e);
                out.push(Event::Haptic { click: true, side: Side::Both });
            }
            return;
        }
        let changed = match it.value {
            Value::Button | Value::Link(_) => dir > 0,
            Value::Text(_) => false,
            Value::Header => false,
            _ => it.value.step(dir),
        };
        if changed {
            out.push(Event::Set { id: it.id.clone(), value: it.value.clone() });
            out.push(Event::Haptic { click: true, side: Side::Both });
            self.dirty = true;
        }
    }

    // ---- controller input ----

    fn nav_dir(st: &State) -> (i32, i32) {
        let b = st.buttons;
        let mut dx = (b & btn::DPAD_RIGHT != 0) as i32 - (b & btn::DPAD_LEFT != 0) as i32;
        let mut dy = (b & btn::DPAD_DOWN != 0) as i32 - (b & btn::DPAD_UP != 0) as i32;
        if dx == 0 && dy == 0 {
            let (x, y) = (st.lx as f32 / 32767.0, -(st.ly as f32) / 32767.0);
            if x.abs().max(y.abs()) > 0.6 {
                if x.abs() > y.abs() {
                    dx = x.signum() as i32;
                } else {
                    dy = y.signum() as i32;
                }
            }
        }
        (dx, dy)
    }

    /// Returns the direction to move now (with auto-repeat), or None.
    fn nav_step(&mut self, dir: (i32, i32), now: Instant) -> Option<(i32, i32)> {
        if dir == (0, 0) {
            self.nav = Nav::default();
            return None;
        }
        if dir != self.nav.dir {
            self.nav.dir = dir;
            self.nav.next = Some(now + NAV_DELAY);
            return Some(dir);
        }
        match self.nav.next {
            Some(t) if now >= t => {
                self.nav.next = Some(now + NAV_REPEAT);
                Some(dir)
            }
            _ => None,
        }
    }

    fn pad_cursor(&mut self, side: usize, touching: bool, x: i16, y: i16, out: &mut Vec<Event>) {
        if !touching {
            if self.cursor[side].take().is_some() {
                self.hover[side] = None;
                self.dirty = true;
            }
            return;
        }
        let (a, b) = if side == 0 { LEFT_PAD_SPAN } else { RIGHT_PAD_SPAN };
        let fx = (x as f32 / 65536.0 + 0.5).clamp(0.0, 0.9999);
        let fy = (0.5 - y as f32 / 65536.0).clamp(0.0, 0.9999);
        let p = (a + fx * (b - a), fy * ROWS as f32);
        if self.cursor[side] != Some(p) {
            self.cursor[side] = Some(p);
            self.dirty = true;
        }
        let h = self.layout.nearest(p.0, p.1);
        if self.hover[side] != Some(h) {
            self.hover[side] = Some(h);
            out.push(Event::Haptic { click: false, side: if side == 0 { Side::Left } else { Side::Right } });
        }
    }

    pub fn input(&mut self, st: &State, raw: &State, now: Instant) -> Vec<Event> {
        let mut out = Vec::new();
        // First frame after showing: nothing counts as newly pressed.
        let raw_prev = if self.prev == u32::MAX { raw.buttons } else { self.raw.buttons };
        if *raw != self.raw {
            self.raw = *raw;
            if self.tab == Tab::Input {
                self.dirty = true;
            }
        }
        if self.tab == Tab::Input {
            // Everything is shown, nothing acts; LB+RB together leave.
            let both = btn::LB | btn::RB;
            self.prev = st.buttons;
            if raw.buttons & both == both && raw_prev & both != both {
                self.set_tab(Tab::Settings, &mut out);
            }
            return out;
        }
        if self.tab == Tab::Mapping
            && let Some(edit) = self.map_edit
        {
            let dir = Self::nav_dir(raw);
            let step = self.nav_step(dir, now);
            self.prev = st.buttons;
            let raw_pressed = raw.buttons & !raw_prev;
            self.map_edit_input(edit, raw_pressed, step, now, &mut out);
            return out;
        }
        let b = st.buttons;
        let first = self.prev == u32::MAX;
        let prev = if first { b } else { self.prev };
        let pressed = b & !prev;
        let released = prev & !b;
        self.prev = b;

        let thr_on = 0.5 * 32767.0;
        let thr_off = 0.4 * 32767.0;
        let lt = st.lt as f32;
        let rt = st.rt as f32;
        let lt_was = self.lt_on;
        let rt_was = self.rt_on;
        self.lt_on = if lt_was { lt > thr_off } else { lt > thr_on && !first };
        self.rt_on = if rt_was { rt > thr_off } else { rt > thr_on && !first };

        if pressed & (btn::LB | btn::RB) != 0 {
            let t = if self.tab == Tab::Keyboard { Tab::Settings } else { Tab::Keyboard };
            self.set_tab(t, &mut out);
            return out;
        }
        let dir = Self::nav_dir(st);
        let step = self.nav_step(dir, now);

        match self.tab {
            Tab::Keyboard => {
                self.pad_cursor(0, b & btn::LPAD_TOUCH != 0, st.lpad_x, st.lpad_y, &mut out);
                self.pad_cursor(1, b & btn::RPAD_TOUCH != 0, st.rpad_x, st.rpad_y, &mut out);

                let lp_now = b & btn::LPAD_CLICK != 0 || self.lt_on;
                let lp_was = prev & btn::LPAD_CLICK != 0 || lt_was;
                let rp_now = b & btn::RPAD_CLICK != 0 || self.rt_on;
                let rp_was = prev & btn::RPAD_CLICK != 0 || rt_was;
                for (now_d, was, side, src) in [(lp_now, lp_was, 0, Src::LeftPad), (rp_now, rp_was, 1, Src::RightPad)] {
                    if now_d && !was {
                        if let Some(h) = self.hover[side] {
                            self.press_key(src, h, &mut out);
                        }
                    } else if !now_d && was {
                        self.release(src, &mut out);
                    }
                }

                let simple = [
                    (btn::A, Src::A, None),
                    (btn::B, Src::B, Some(KEY_BACKSPACE)),
                    (btn::X, Src::X, Some(KEY_SPACE)),
                    (btn::MENU, Src::Enter, Some(KEY_ENTER)),
                ];
                for (bit, src, code) in simple {
                    if pressed & bit != 0 {
                        match code {
                            Some(c) => self.press_codes(src, None, c, &mut out),
                            None => {
                                let f = self.focus;
                                self.press_key(src, f, &mut out);
                            }
                        }
                    } else if released & bit != 0 {
                        self.release(src, &mut out);
                    }
                }
                if pressed & btn::Y != 0 {
                    let i = ModKey::Shift.index();
                    self.mods[i] = if self.mods[i] == Latch::Off { Latch::Latched } else { Latch::Off };
                    self.refresh_labels(false);
                    out.push(Event::Haptic { click: false, side: Side::Both });
                }
                if let Some((dx, dy)) = step {
                    let n = self.layout.neighbor(self.focus, dx, dy);
                    if n != self.focus {
                        self.focus = n;
                        self.dirty = true;
                        out.push(Event::Haptic { click: false, side: Side::Both });
                    }
                }
            }
            Tab::Settings | Tab::Mapping | Tab::Presets | Tab::Input => {
                if self.cursor.iter().any(Option::is_some) {
                    self.cursor = [None, None];
                    self.hover = [None, None];
                    self.dirty = true;
                }
                let n = self.list().0.len();
                if let Some((dx, dy)) = step
                    && n > 0
                {
                    let mapping = self.tab == Tab::Mapping;
                    let (items, focus) = self.list_mut();
                    if dy != 0 {
                        let want = (*focus as i32 + dy).clamp(0, n as i32 - 1) as usize;
                        let f = selectable(items, want, dy);
                        // At the top/bottom a heading may be all that's left: stay.
                        *focus = if matches!(items[f].value, Value::Header) { *focus } else { f };
                        self.dirty = true;
                        out.push(Event::Haptic { click: false, side: Side::Both });
                    } else if dx != 0 && !mapping && !matches!(items[*focus].value, Value::Button | Value::Link(_)) {
                        self.activate_item(dx, &mut out);
                    }
                }
                let focus = self.list().1;
                if self.tab == Tab::Mapping && self.is_remap_entry(focus) {
                    if pressed & btn::A != 0 {
                        self.map_edit = Some(MapEdit::Capture { entry: focus, until: now + CAPTURE_TIMEOUT });
                        self.dirty = true;
                        out.push(Event::Haptic { click: true, side: Side::Both });
                    } else if pressed & btn::X != 0 {
                        self.open_map_list(focus);
                    } else if pressed & btn::Y != 0 {
                        self.set_mapping(focus, 0, &mut out);
                    }
                } else if pressed & btn::A != 0 {
                    self.activate_item(1, &mut out);
                }
                if pressed & btn::B != 0 {
                    let back = if self.tab.is_sub() { Tab::Settings } else { Tab::Keyboard };
                    self.set_tab(back, &mut out);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osk::Item;

    fn st(buttons: u32) -> State {
        State { buttons, ..Default::default() }
    }

    fn keys(ev: &[Event]) -> Vec<(u16, bool)> {
        ev.iter()
            .filter_map(|e| match e {
                Event::Key { code, down } => Some((*code, *down)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_types_focus_with_latched_shift() {
        let mut m = Model::new(Labels::fallback());
        let t = Instant::now();
        m.show(Some(Tab::Keyboard), t);
        m.input(&st(0), &State::default(), t);
        m.input(&st(btn::Y), &State::default(), t); // shift latch
        m.input(&st(0), &State::default(), t);
        let down = m.input(&st(btn::A), &State::default(), t);
        assert_eq!(keys(&down), vec![(42, true), (35, true)]);
        let up = m.input(&st(0), &State::default(), t);
        assert_eq!(keys(&up), vec![(35, false), (42, false)]);
        // latch consumed
        let again = m.input(&st(btn::A), &State::default(), t);
        assert_eq!(keys(&again), vec![(35, true)]);
    }

    #[test]
    fn opening_button_is_ignored_and_dpad_moves() {
        let mut m = Model::new(Labels::fallback());
        let t = Instant::now();
        m.show(Some(Tab::Keyboard), t);
        assert!(keys(&m.input(&st(btn::A), &State::default(), t)).is_empty()); // held while opening
        m.input(&st(0), &State::default(), t);
        let f = m.focus;
        m.input(&st(btn::DPAD_RIGHT), &State::default(), t);
        assert_ne!(m.focus, f);
        m.input(&st(btn::DPAD_RIGHT), &State::default(), t + Duration::from_millis(100));
        let f2 = m.focus;
        m.input(&st(btn::DPAD_RIGHT), &State::default(), t + Duration::from_millis(400)); // repeat
        assert_ne!(m.focus, f2);
    }

    #[test]
    fn right_pad_click_types_hovered() {
        let mut m = Model::new(Labels::fallback());
        let t = Instant::now();
        m.show(Some(Tab::Keyboard), t);
        m.input(&st(0), &State::default(), t);
        let mut s = st(btn::RPAD_TOUCH);
        s.rpad_x = 32767;
        s.rpad_y = 32767; // top right -> Backspace (row 0, last key)
        m.input(&s, &State::default(), t);
        s.buttons |= btn::RPAD_CLICK;
        let ev = m.input(&s, &State::default(), t);
        assert_eq!(keys(&ev), vec![(14, true)]);
    }

    #[test]
    fn settings_change_emits_set() {
        let mut m = Model::new(Labels::fallback());
        let t = Instant::now();
        m.set_status(Status {
            items: vec![Item { id: "x".into(), label: "X".into(), value: Value::Toggle(false) }],
            ..Default::default()
        });
        m.show(Some(Tab::Settings), t);
        m.input(&st(0), &State::default(), t);
        let ev = m.input(&st(btn::A), &State::default(), t);
        assert!(ev.contains(&Event::Set { id: "x".into(), value: Value::Toggle(true) }));
    }

    #[test]
    fn controller_pages_navigation() {
        let mut m = Model::new(Labels::fallback());
        let t = Instant::now();
        m.set_status(Status {
            items: vec![
                Item { id: "page.presets".into(), label: "Presets".into(), value: Value::Link(String::new()) },
                Item { id: "page.input".into(), label: "Input".into(), value: Value::Link(String::new()) },
            ],
            ..Default::default()
        });
        m.show(Some(Tab::Keyboard), t);
        let raw = State::default();
        m.input(&st(0), &raw, t);
        m.input(&st(btn::RB), &raw, t);
        assert_eq!(m.tab, Tab::Settings);
        m.input(&st(0), &raw, t);
        m.input(&st(btn::A), &raw, t);
        assert_eq!(m.tab, Tab::Presets);
        m.input(&st(0), &raw, t);
        m.input(&st(btn::B), &raw, t);
        assert_eq!(m.tab, Tab::Settings);
        // Input test: buttons don't act, LB+RB together go back.
        m.input(&st(0), &raw, t);
        m.input(&st(btn::DPAD_DOWN), &raw, t);
        m.input(&st(0), &raw, t);
        m.input(&st(btn::A), &raw, t);
        assert_eq!(m.tab, Tab::Input);
        let b = st(btn::B);
        m.input(&b, &b, t);
        assert_eq!(m.tab, Tab::Input);
        let both = st(btn::LB | btn::RB);
        m.input(&both, &both, t);
        assert_eq!(m.tab, Tab::Settings);
    }

    fn mapping_status() -> Status {
        let cfg = crate::config::Config::default();
        let (mapping_title, mapping) = crate::osk::menu::mapping(
            &cfg,
            &crate::presets::Presets::load(std::path::Path::new("/nonexistent-puckd-test"), cfg.gamepad.clone()),
        );
        Status { mapping_title, mapping, ..Default::default() }
    }

    #[test]
    fn mapping_capture_list_and_reopen() {
        let mut m = Model::new(Labels::fallback());
        let t = Instant::now();
        m.set_status(mapping_status());
        m.show(Some(Tab::Mapping), t);
        let idle = st(0);
        m.input(&idle, &idle, t);
        // Select L4 (entry 11) and capture: pressing Y maps it to Y.
        m.map_focus = 11;
        let a = st(btn::A);
        m.input(&a, &a, t);
        assert!(matches!(m.map_edit, Some(MapEdit::Capture { entry: 11, .. })));
        m.input(&idle, &idle, t); // releasing A must not count
        assert!(m.map_edit.is_some());
        let y = st(btn::Y);
        let ev = m.input(&y, &y, t);
        assert!(m.map_edit.is_none());
        let set = ev.iter().find_map(|e| match e {
            Event::Set { id, value: Value::Choice { idx, options } } => Some((id.clone(), options[*idx].clone())),
            _ => None,
        });
        assert_eq!(set, Some(("remap.l4".to_string(), "Y".to_string())));

        // Capture times out.
        m.input(&idle, &idle, t);
        m.input(&a, &a, t);
        m.input(&idle, &idle, t + CAPTURE_TIMEOUT + Duration::from_millis(1));
        assert!(m.map_edit.is_none());

        // List: X opens it at the current choice, down + A picks the next option.
        let x = st(btn::X);
        m.input(&x, &x, t);
        let Some(MapEdit::List { focus, .. }) = m.map_edit else { panic!() };
        m.input(&idle, &idle, t);
        let d = st(btn::DPAD_DOWN);
        m.input(&d, &d, t);
        m.input(&idle, &idle, t);
        let ev = m.input(&a, &a, t);
        let want = crate::osk::picker::step(focus, 0, 1);
        assert!(ev.iter().any(|e| matches!(e, Event::Set { value: Value::Choice { idx, .. }, .. } if *idx == want)));

        // Closing and reopening keeps page and selection.
        let mut out = Vec::new();
        m.hide(&mut out);
        m.show(None, t);
        assert_eq!(m.tab, Tab::Mapping);
        assert_eq!(m.map_focus, 11);
    }

    #[test]
    fn name_entry_and_headings() {
        let mut m = Model::new(Labels::fallback());
        let t = Instant::now();
        let it = |id: &str, v: Value| Item { id: id.into(), label: id.into(), value: v };
        m.set_status(Status {
            presets: vec![
                it("", Value::Header),
                it("preset.new", Value::Text("fire".into())),
                it("", Value::Header),
                it("preset.load_default", Value::Link(String::new())),
            ],
            ..Default::default()
        });
        assert_eq!(m.preset_focus, 1); // heading skipped
        m.show(Some(Tab::Presets), t);
        let idle = st(0);
        m.input(&idle, &idle, t);
        let d = st(btn::DPAD_DOWN);
        m.input(&d, &d, t);
        assert_eq!(m.preset_focus, 3);
        m.input(&idle, &idle, t + Duration::from_millis(500));
        let u = st(btn::DPAD_UP);
        m.input(&u, &u, t + Duration::from_millis(600));
        assert_eq!(m.preset_focus, 1);
        m.input(&idle, &idle, t + Duration::from_millis(700));

        // A opens the name entry on the keyboard, pre-filled.
        let a = st(btn::A);
        m.input(&a, &a, t);
        assert_eq!(m.tab, Tab::Keyboard);
        assert_eq!(m.entry.as_ref().map(|e| e.text.as_str()), Some("fire"));
        // Typing 'f' via the focused key (focus starts on 'h'; move left twice: g, f).
        m.input(&idle, &idle, t);
        let l = st(btn::DPAD_LEFT);
        m.input(&l, &l, t);
        m.input(&idle, &idle, t);
        m.input(&l, &l, t);
        m.input(&idle, &idle, t);
        let ev = m.input(&a, &a, t);
        assert!(keys(&ev).is_empty(), "keys must not reach the app while typing a name");
        m.input(&idle, &idle, t);
        let b = st(btn::B);
        m.input(&b, &b, t); // backspace
        m.input(&idle, &idle, t);
        m.input(&a, &a, t);
        m.input(&idle, &idle, t);
        assert_eq!(m.entry.as_ref().unwrap().text, "firef");
        // Menu (≡) = Enter: confirm and go back.
        let menu = st(btn::MENU);
        let ev = m.input(&menu, &menu, t);
        assert!(ev.contains(&Event::Set { id: "preset.new".into(), value: Value::Text("firef".into()) }));
        assert_eq!(m.tab, Tab::Presets);
        assert!(m.entry.is_none());
    }
}

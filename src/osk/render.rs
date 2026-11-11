//! Software rendering of the overlay (tiny-skia shapes, fontdue text) and its geometry.

use std::collections::HashMap;

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::osk::Tab;
use crate::osk::layout::{Kind, ROWS};
use crate::osk::model::{CAPTURE_TIMEOUT, Latch, MapEdit, Model};
use crate::proto::btn;

/// Logical overlay size.
pub const WIDTH: u32 = 960;
pub const HEIGHT: u32 = 380;

const PAD: f32 = 14.0;
const HEADER: f32 = 42.0;
const FOOTER: f32 = 28.0;
const GAP: f32 = 6.0;
const ITEM_H: f32 = 34.0;
const LIST_TITLE: f32 = 26.0;
const TAB_W: f32 = 118.0;

/// Layout math in logical pixels.
pub struct Geo {
    kb_x: f32,
    kb_y: f32,
    unit: f32,
    row_h: f32,
    list_y: f32,
    list_h: f32,
}

impl Geo {
    pub fn new() -> Self {
        let (w, h) = (WIDTH as f32, HEIGHT as f32);
        let kb_x = PAD;
        let kb_y = PAD + HEADER;
        let unit = (w - 2.0 * PAD) / crate::osk::layout::COLS;
        let row_h = (h - kb_y - FOOTER - PAD) / ROWS as f32;
        Self { kb_x, kb_y, unit, row_h, list_y: kb_y, list_h: h - kb_y - FOOTER - PAD }
    }

    fn key_rect(&self, x: f32, w: f32, row: usize) -> (f32, f32, f32, f32) {
        (
            self.kb_x + x * self.unit + GAP / 2.0,
            self.kb_y + row as f32 * self.row_h + GAP / 2.0,
            w * self.unit - GAP,
            self.row_h - GAP,
        )
    }

    /// Key-unit coordinates of a logical point.
    pub fn to_units(&self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.kb_x) / self.unit, (y - self.kb_y) / self.row_h)
    }

    fn to_px(&self, ux: f32, uy: f32) -> (f32, f32) {
        (self.kb_x + ux * self.unit, self.kb_y + uy * self.row_h)
    }

    /// Top of the list on list tabs (the Mapping tab has a heading line first).
    fn list_top(&self, tab: Tab) -> f32 {
        if matches!(tab, Tab::Mapping | Tab::Presets) { self.list_y + LIST_TITLE } else { self.list_y }
    }

    fn visible_items(&self, tab: Tab) -> usize {
        ((self.list_h - (self.list_top(tab) - self.list_y)) / ITEM_H).floor().max(1.0) as usize
    }

    /// First visible entry so that `focus` stays on screen.
    pub fn first_item(&self, focus: usize, n: usize, tab: Tab) -> usize {
        let vis = self.visible_items(tab);
        if n <= vis { 0 } else { focus.saturating_sub(vis / 2).min(n - vis) }
    }

    pub fn item_at(&self, y: f32, focus: usize, n: usize, tab: Tab) -> Option<usize> {
        let top = self.list_top(tab);
        if y < top {
            return None;
        }
        let i = self.first_item(focus, n, tab) + ((y - top) / ITEM_H) as usize;
        (i < n).then_some(i)
    }

    /// Tab header hit test: Some(tab) if the point is on a tab label.
    pub fn tab_at(&self, x: f32, y: f32) -> Option<Tab> {
        if !(PAD..PAD + HEADER).contains(&y) || x < PAD {
            return None;
        }
        [Tab::Keyboard, Tab::Settings].get(((x - PAD) / TAB_W) as usize).copied()
    }
}

pub struct Fonts {
    fonts: Vec<fontdue::Font>,
    cache: HashMap<(char, u32), (usize, fontdue::Metrics, Vec<u8>)>,
}

fn fc_match(pattern: &str) -> Option<String> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}", pattern]).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    (!s.is_empty()).then_some(s)
}

impl Fonts {
    pub fn load() -> Result<Self, String> {
        let mut paths: Vec<String> = Vec::new();
        for p in ["sans-serif:weight=medium", "DejaVu Sans", "Noto Sans Symbols2", "Noto Sans Symbols"] {
            if let Some(f) = fc_match(p)
                && !paths.contains(&f)
            {
                paths.push(f);
            }
        }
        for p in [
            "/usr/share/fonts/noto/NotoSans-Medium.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ] {
            if std::path::Path::new(p).exists() && !paths.iter().any(|x| x == p) {
                paths.push(p.to_string());
            }
        }
        let fonts: Vec<fontdue::Font> = paths
            .iter()
            .filter_map(|p| std::fs::read(p).ok())
            .filter_map(|d| fontdue::Font::from_bytes(d, fontdue::FontSettings::default()).ok())
            .collect();
        if fonts.is_empty() {
            return Err("no usable font found (install fontconfig and a sans font)".into());
        }
        Ok(Self { fonts, cache: HashMap::new() })
    }

    fn glyph(&mut self, c: char, px: f32) -> &(usize, fontdue::Metrics, Vec<u8>) {
        let key = (c, (px * 4.0) as u32);
        let fonts = &self.fonts;
        self.cache.entry(key).or_insert_with(|| {
            let fi = fonts.iter().position(|f| f.lookup_glyph_index(c) != 0).unwrap_or(0);
            let (m, bmp) = fonts[fi].rasterize(c, px);
            (fi, m, bmp)
        })
    }

    fn ascent(&self, px: f32) -> f32 {
        self.fonts[0].horizontal_line_metrics(px).map(|m| m.ascent).unwrap_or(px * 0.8)
    }

    /// Height of capital letters (for optical vertical centering).
    fn cap_height(&self, px: f32) -> f32 {
        let m = self.fonts[0].metrics('H', px);
        if m.height > 0 { m.height as f32 } else { px * 0.7 }
    }

    /// Top y (as `draw` expects it) that centers capitals on `cy` (device pixels).
    fn centered_top(&self, cy: f32, px: f32) -> f32 {
        cy - self.ascent(px) + self.cap_height(px) / 2.0
    }

    pub fn width(&mut self, text: &str, px: f32) -> f32 {
        text.chars().map(|c| self.glyph(c, px).1.advance_width).sum()
    }

    /// The end of `text` that fits in `max` device pixels, "…" in front if cut.
    pub fn ellipsize_start(&mut self, text: &str, px: f32, max: f32) -> String {
        if self.width(text, px) <= max {
            return text.to_string();
        }
        let ell = self.width("…", px);
        let mut w = 0.0;
        let mut keep: Vec<char> = Vec::new();
        for c in text.chars().rev() {
            let cw = self.glyph(c, px).1.advance_width;
            if w + cw + ell > max {
                break;
            }
            w += cw;
            keep.push(c);
        }
        format!("…{}", keep.into_iter().rev().collect::<String>())
    }

    /// `text` shortened with "…" so it fits in `max` device pixels.
    pub fn ellipsize(&mut self, text: &str, px: f32, max: f32) -> String {
        if self.width(text, px) <= max {
            return text.to_string();
        }
        let ell = self.width("…", px);
        let mut out = String::new();
        let mut w = 0.0;
        for c in text.chars() {
            let cw = self.glyph(c, px).1.advance_width;
            if w + cw + ell > max {
                break;
            }
            w += cw;
            out.push(c);
        }
        format!("{}…", out.trim_end())
    }

    /// Draw text with its baseline-box top-left at (x, y), in device pixels.
    fn draw(&mut self, pm: &mut Pixmap, text: &str, x: f32, y: f32, px: f32, color: [u8; 4]) {
        let base = y + self.ascent(px);
        let mut pen = x;
        let (w, h) = (pm.width() as i32, pm.height() as i32);
        for c in text.chars() {
            let (_, m, bmp) = self.glyph(c, px).clone();
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (base - m.height as f32 - m.ymin as f32).round() as i32;
            let data = pm.data_mut();
            for row in 0..m.height as i32 {
                let py = gy + row;
                if py < 0 || py >= h {
                    continue;
                }
                for col in 0..m.width as i32 {
                    let pxx = gx + col;
                    if pxx < 0 || pxx >= w {
                        continue;
                    }
                    let cov = bmp[(row * m.width as i32 + col) as usize] as u32 * color[3] as u32 / 255;
                    if cov == 0 {
                        continue;
                    }
                    let i = ((py * w + pxx) * 4) as usize;
                    let inv = 255 - cov;
                    for ch in 0..3 {
                        data[i + ch] = ((color[ch] as u32 * cov + data[i + ch] as u32 * inv) / 255) as u8;
                    }
                    data[i + 3] = (cov + data[i + 3] as u32 * inv / 255) as u8;
                }
            }
            pen += m.advance_width;
        }
    }
}

fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

fn paint(c: [u8; 4]) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(Color::from_rgba8(c[0], c[1], c[2], c[3]));
    p.anti_alias = true;
    p
}

const BG: [u8; 4] = [18, 20, 26, 250];
const KEY: [u8; 4] = [44, 48, 60, 255];
const KEY_MOD: [u8; 4] = [36, 40, 50, 255];
const KEY_DOWN: [u8; 4] = [110, 120, 150, 255];
const LATCHED: [u8; 4] = [60, 90, 150, 255];
const LOCKED: [u8; 4] = [80, 130, 210, 255];
const TEXT: [u8; 4] = [230, 233, 240, 255];
const DIM: [u8; 4] = [150, 156, 170, 255];
const FOCUS: [u8; 4] = [245, 245, 250, 255];
const LEFT_C: [u8; 4] = [80, 200, 255, 255];
const RIGHT_C: [u8; 4] = [255, 160, 70, 255];
const GLYPH: [u8; 4] = [6, 7, 9, 255];
const GLYPH_RIM: [u8; 4] = [90, 96, 112, 255];

/// Render the whole overlay at `scale` device pixels per logical pixel.
pub fn draw(model: &Model, fonts: &mut Fonts, scale: f32) -> Option<Pixmap> {
    let g = Geo::new();
    let (wp, hp) = ((WIDTH as f32 * scale) as u32, (HEIGHT as f32 * scale) as u32);
    let mut pm = Pixmap::new(wp, hp)?;
    let t = Transform::from_scale(scale, scale);
    let s = scale;

    if let Some(p) = rrect(0.0, 0.0, WIDTH as f32, HEIGHT as f32, 16.0) {
        pm.fill_path(&p, &paint(BG), FillRule::Winding, t, None);
    }

    if let Some(e) = &model.entry {
        // Typing a name: a text field replaces the tab bar.
        let lpx = 15.0 * s;
        let label = format!("{}:", e.label.trim_end_matches('…'));
        let lw = fonts.width(&label, lpx) / s;
        fonts.draw(&mut pm, &label, (PAD + 8.0) * s, (PAD + 10.0) * s, lpx, DIM);
        let fx = PAD + 20.0 + lw;
        let fw = WIDTH as f32 - PAD - 8.0 - fx;
        if let Some(p) = rrect(fx, PAD + 2.0, fw, 32.0, 8.0) {
            pm.fill_path(&p, &paint(KEY), FillRule::Winding, t, None);
            let st = Stroke { width: 2.0, ..Default::default() };
            pm.stroke_path(&p, &paint(LOCKED), &st, t, None);
        }
        let tpx = 17.0 * s;
        let text = fonts.ellipsize_start(&format!("{}▏", e.text), tpx, (fw - 24.0) * s);
        fonts.draw(&mut pm, &text, (fx + 12.0) * s, (PAD + 8.0) * s, tpx, TEXT);
    } else {
        // Header: tabs + status.
        let tab_px = 17.0 * s;
        let current = if model.tab.is_sub() { Tab::Settings } else { model.tab };
        for (i, tab) in [Tab::Keyboard, Tab::Settings].iter().enumerate() {
            let name = tab.title();
            let x = PAD + 6.0 + i as f32 * TAB_W;
            let active = current == *tab;
            fonts.draw(&mut pm, name, x * s, (PAD + 6.0) * s, tab_px, if active { TEXT } else { DIM });
            if active {
                let w = fonts.width(name, tab_px) / s;
                if let Some(p) = rrect(x, PAD + 32.0, w, 3.0, 1.5) {
                    pm.fill_path(&p, &paint(LOCKED), FillRule::Winding, t, None);
                }
            }
        }
        let info = format!("{}  ·  {}", model.status.title, model.status.detail);
        let info_px = 14.0 * s;
        // Right of the two tabs.
        let info = fonts.ellipsize(&info, info_px, (WIDTH as f32 - 2.0 * PAD - 2.0 * TAB_W - 24.0) * s);
        let iw = fonts.width(&info, info_px);
        fonts.draw(&mut pm, &info, (WIDTH as f32 - PAD - 6.0) * s - iw, (PAD + 9.0) * s, info_px, DIM);
    }

    match model.tab {
        Tab::Keyboard => draw_keyboard(model, fonts, &mut pm, &g, s, t),
        Tab::Settings | Tab::Mapping | Tab::Presets => draw_list(model, fonts, &mut pm, &g, s, t),
        Tab::Input => draw_input(model, fonts, &mut pm, s, t),
    }

    let hint = match model.tab {
        Tab::Keyboard if model.entry.is_some() => {
            "Type here or on your keyboard   B ⌫   X space   Y shift   ≡ / Enter: save   Esc: cancel   ⋯ close"
        }
        Tab::Keyboard => {
            "Pads + click/trigger: type   D-pad + A: type   B ⌫   X space   Y shift   ≡ enter   LB/RB: tabs   ⋯ close"
        }
        Tab::Settings => "D-pad: select / change   A: change / open   B: keyboard   LB/RB: tabs   ⋯ close",
        Tab::Mapping => match model.map_edit {
            Some(MapEdit::Capture { .. }) => "Press any button   wait 5 s: cancel   ⋯ close",
            Some(MapEdit::List { .. }) => "D-pad: select   A: choose   B: cancel",
            None => "A: map by pressing a button   X: choose from a list   Y: reset   B: back   ⋯ close",
        },
        Tab::Presets => "D-pad: select / change   A: change / do   B: back   LB/RB: tabs   ⋯ close",
        Tab::Input => "Everything you press is shown here   LB + RB together: back   ⋯ close (reserved)",
    };
    let hpx = 12.5 * s;
    let hw = fonts.width(hint, hpx);
    fonts.draw(&mut pm, hint, (wp as f32 - hw) / 2.0, (HEIGHT as f32 - PAD - FOOTER + 8.0) * s, hpx, DIM);
    Some(pm)
}

fn draw_keyboard(model: &Model, fonts: &mut Fonts, pm: &mut Pixmap, g: &Geo, s: f32, t: Transform) {
    for (i, k) in model.layout.keys.iter().enumerate() {
        let (x, y, w, h) = g.key_rect(k.x, k.w, k.row);
        let latch = match k.kind {
            Kind::Mod(m, _) => model.mods[m.index()],
            Kind::Caps if model.caps => Latch::Locked,
            _ => Latch::Off,
        };
        let fill = if model.is_down(i) {
            KEY_DOWN
        } else {
            match latch {
                Latch::Latched => LATCHED,
                Latch::Locked => LOCKED,
                Latch::Off if matches!(k.kind, Kind::Char) || k.code == 57 => KEY,
                Latch::Off => KEY_MOD,
            }
        };
        if let Some(p) = rrect(x, y, w, h, 7.0) {
            pm.fill_path(&p, &paint(fill), FillRule::Winding, t, None);
            let mut outline = |c: [u8; 4], width: f32, inset: f32| {
                if let Some(p) = rrect(x + inset, y + inset, w - 2.0 * inset, h - 2.0 * inset, 7.0 - inset) {
                    let st = Stroke { width, ..Default::default() };
                    pm.stroke_path(&p, &paint(c), &st, t, None);
                }
            };
            if model.focus == i {
                outline(FOCUS, 2.0, 1.0);
            }
            if model.hover[0] == Some(i) {
                outline(LEFT_C, 3.0, 3.5);
            }
            if model.hover[1] == Some(i) {
                outline(RIGHT_C, 3.0, 3.5);
            }
        }
        let label: &str = match k.kind {
            Kind::Char => model.label_text.get(i).map(String::as_str).unwrap_or(""),
            Kind::Named(n) | Kind::Mod(_, n) => n,
            Kind::Caps => "Caps",
            Kind::Settings => "⚙",
        };
        if label.is_empty() {
            continue;
        }
        let px = if label.chars().count() > 1 && matches!(k.kind, Kind::Char) {
            15.0
        } else if matches!(k.kind, Kind::Char) || label.chars().count() == 1 {
            21.0
        } else {
            14.0
        } * s;
        let tw = fonts.width(label, px);
        let top = fonts.centered_top((y + h / 2.0) * s, px);
        fonts.draw(pm, label, (x + w / 2.0) * s - tw / 2.0, top, px, TEXT);
    }
    // Trackpad cursors.
    for (side, c) in [(0, LEFT_C), (1, RIGHT_C)] {
        if let Some((ux, uy)) = model.cursor[side] {
            let (cx, cy) = g.to_px(ux, uy);
            let mut pb = PathBuilder::new();
            pb.push_circle(cx, cy, 5.0);
            if let Some(p) = pb.finish() {
                pm.fill_path(&p, &paint(c), FillRule::Winding, t, None);
            }
        }
    }
}

fn draw_list(model: &Model, fonts: &mut Fonts, pm: &mut Pixmap, g: &Geo, s: f32, t: Transform) {
    let (items, focus) = model.list();
    let heading = match model.tab {
        Tab::Mapping => Some(&model.status.mapping_title),
        Tab::Presets => Some(&model.status.presets_title),
        _ => None,
    };
    if let Some(h) = heading {
        let text = format!("{} › {}   ·   {h}", Tab::Settings.title(), model.tab.title());
        fonts.draw(pm, &text, (PAD + 18.0) * s, (g.list_y + 2.0) * s, 14.0 * s, DIM);
    }
    if let (Tab::Mapping, Some(edit)) = (model.tab, model.map_edit) {
        draw_map_edit(model, edit, fonts, pm, g, s, t);
        return;
    }
    let top = g.list_top(model.tab);
    let first = g.first_item(focus, items.len(), model.tab);
    let vis = g.visible_items(model.tab);
    let px = 16.0 * s;
    for (row, i) in (first..items.len().min(first + vis)).enumerate() {
        let it = &items[i];
        let y = top + row as f32 * ITEM_H;
        let x = PAD + 4.0;
        let w = WIDTH as f32 - 2.0 * PAD - 8.0;
        if matches!(it.value, crate::osk::Value::Header) {
            // Section heading: small, dim, with a rule.
            let hpx = 12.5 * s;
            let label = fonts.ellipsize(&it.label, hpx, (w - 40.0) * s);
            fonts.draw(pm, &label, (x + 14.0) * s, (y + 14.0) * s, hpx, DIM);
            let hw = fonts.width(&label, hpx) / s;
            if let Some(p) = rrect(x + 26.0 + hw, y + 22.0, (w - 40.0 - hw).max(0.0), 1.0, 0.0) {
                pm.fill_path(&p, &paint(KEY), FillRule::Winding, t, None);
            }
            continue;
        }
        let focused = i == focus;
        if focused && let Some(p) = rrect(x, y + 2.0, w, ITEM_H - 4.0, 8.0) {
            pm.fill_path(&p, &paint(KEY), FillRule::Winding, t, None);
        }
        let ty = (y + (ITEM_H - 16.0) / 2.0 - 2.0) * s;
        let v = match it.value {
            crate::osk::Value::Button => {
                if focused {
                    "press A".to_string()
                } else {
                    String::new()
                }
            }
            crate::osk::Value::Link(ref info) => format!("{info}  ›"),
            crate::osk::Value::Text(ref t) if !t.is_empty() && !it.id.starts_with("preset.") => format!("{t}  ›"),
            crate::osk::Value::Text(_) => "type a name  ›".to_string(),
            ref v if focused && model.tab != Tab::Mapping => format!("‹  {}  ›", v.display()),
            ref v => v.display(),
        };
        // Values get at most half the row; labels take the rest (both cut with "…").
        let v = fonts.ellipsize(&v, px, (w * 0.5) * s);
        let vw = fonts.width(&v, px);
        let label = fonts.ellipsize(&it.label, px, (w - 28.0 - 24.0) * s - vw);
        fonts.draw(pm, &label, (x + 14.0) * s, ty, px, if focused { TEXT } else { DIM });
        let col = if focused {
            LOCKED
        } else if v == "itself" {
            DIM
        } else {
            TEXT
        };
        fonts.draw(pm, &v, (x + w - 14.0) * s - vw, ty, px, col);
    }
}

/// Small drawing helpers in logical coordinates (positional args keep the layout code compact).
#[allow(clippy::too_many_arguments)]
struct Canvas<'a> {
    pm: &'a mut Pixmap,
    fonts: &'a mut Fonts,
    s: f32,
    t: Transform,
}

#[allow(clippy::too_many_arguments)]
impl Canvas<'_> {
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, fill: [u8; 4]) {
        if let Some(p) = rrect(x, y, w, h, r) {
            self.pm.fill_path(&p, &paint(fill), FillRule::Winding, self.t, None);
        }
    }

    fn rect_outline(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: [u8; 4], width: f32) {
        if let Some(p) = rrect(x, y, w, h, r) {
            let st = Stroke { width, ..Default::default() };
            self.pm.stroke_path(&p, &paint(c), &st, self.t, None);
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, fill: Option<[u8; 4]>, stroke: Option<([u8; 4], f32)>) {
        let mut pb = PathBuilder::new();
        pb.push_circle(cx, cy, r);
        let Some(p) = pb.finish() else { return };
        if let Some(f) = fill {
            self.pm.fill_path(&p, &paint(f), FillRule::Winding, self.t, None);
        }
        if let Some((c, w)) = stroke {
            let st = Stroke { width: w, ..Default::default() };
            self.pm.stroke_path(&p, &paint(c), &st, self.t, None);
        }
    }

    /// Text centered on (cx, cy).
    fn label(&mut self, text: &str, cx: f32, cy: f32, size: f32, c: [u8; 4]) {
        let px = size * self.s;
        let w = self.fonts.width(text, px);
        let top = self.fonts.centered_top(cy * self.s, px);
        self.fonts.draw(self.pm, text, cx * self.s - w / 2.0, top, px, c);
    }

    /// A controller button glyph, Steam style: dark circle, thin rim, white letter.
    /// Used wherever a button is named (popups, hints), so they all look alike.
    fn glyph(&mut self, letter: &str, cx: f32, cy: f32, r: f32) {
        self.circle(cx, cy, r, Some(GLYPH), Some((GLYPH_RIM, 1.5)));
        self.label(letter, cx, cy, r * 1.05, TEXT);
    }

    fn text(&mut self, text: &str, x: f32, y: f32, size: f32, c: [u8; 4]) {
        self.fonts.draw(self.pm, text, x * self.s, y * self.s, size * self.s, c);
    }

    /// A labelled button: filled when pressed.
    fn button(&mut self, name: &str, x: f32, y: f32, w: f32, h: f32, on: bool) {
        self.rect(x, y, w, h, 6.0, if on { LOCKED } else { KEY });
        self.label(name, x + w / 2.0, y + h / 2.0, 13.0, TEXT);
    }

    /// Horizontal bar for 0..1 (or -1..1 when `signed`).
    fn bar(&mut self, name: &str, x: f32, y: f32, w: f32, v: f32, signed: bool, outline: bool) {
        self.rect(x, y, w, 14.0, 4.0, KEY_MOD);
        let v = v.clamp(-1.0, 1.0);
        if signed {
            let mid = x + w / 2.0;
            let len = v.abs() * w / 2.0;
            let x0 = if v < 0.0 { mid - len } else { mid };
            if len >= 1.0 {
                self.rect(x0, y, len, 14.0, 3.0, LOCKED);
            }
            self.rect(mid - 0.5, y, 1.0, 14.0, 0.0, DIM);
        } else if v * w >= 1.0 {
            self.rect(x, y, v * w, 14.0, 4.0, LOCKED);
        }
        if outline {
            self.rect_outline(x, y, w, 14.0, 4.0, RIGHT_C, 2.0);
        }
        self.text(name, x + 4.0, y - 1.0, 11.0, TEXT);
    }

    fn stick(&mut self, cx: f32, cy: f32, x: i16, y: i16, click: bool, touch: bool) {
        let r = 40.0;
        self.circle(cx, cy, r, Some(KEY_MOD), touch.then_some((LEFT_C, 2.5)));
        let (dx, dy) = (x as f32 / 32768.0 * r, -(y as f32) / 32768.0 * r);
        self.circle(cx + dx, cy + dy, 11.0, Some(if click { LOCKED } else { TEXT }), None);
    }

    fn pad(&mut self, x: f32, y: f32, size: f32, px: i16, py: i16, pressure: u16, touch: bool, click: bool) {
        self.rect(x, y, size, size, 14.0, if touch { KEY } else { KEY_MOD });
        if click {
            self.rect_outline(x, y, size, size, 14.0, LOCKED, 3.0);
        }
        if touch {
            let cx = x + (px as f32 / 65536.0 + 0.5) * size;
            let cy = y + (0.5 - py as f32 / 65536.0) * size;
            let pr = 6.0 + pressure as f32 / 32768.0 * 18.0;
            self.circle(cx, cy, pr, None, Some((RIGHT_C, 2.0)));
            self.circle(cx, cy, 4.0, Some(TEXT), None);
        }
    }
}

/// Live view of the raw controller state (gamepad tester).
fn draw_input(model: &Model, fonts: &mut Fonts, pm: &mut Pixmap, s: f32, t: Transform) {
    let st = &model.raw;
    let on = |b: u32| st.buttons & b != 0;
    let mut c = Canvas { pm, fonts, s, t };
    let trig = |v: i16| v.max(0) as f32 / 32767.0;

    // Shoulders: trigger bars (outlined on full-pull click), bumpers, paddles.
    c.bar("LT", 30.0, 66.0, 160.0, trig(st.lt), false, on(btn::LT_CLICK));
    c.bar("RT", 770.0, 66.0, 160.0, trig(st.rt), false, on(btn::RT_CLICK));
    c.button("LB", 200.0, 62.0, 70.0, 22.0, on(btn::LB));
    c.button("RB", 690.0, 62.0, 70.0, 22.0, on(btn::RB));
    c.button("L4", 30.0, 90.0, 76.0, 20.0, on(btn::L4));
    c.button("L5", 114.0, 90.0, 76.0, 20.0, on(btn::L5));
    c.button("R4", 770.0, 90.0, 76.0, 20.0, on(btn::R4));
    c.button("R5", 854.0, 90.0, 76.0, 20.0, on(btn::R5));

    // Left side: stick, d-pad, trackpad.
    c.stick(110.0, 168.0, st.lx, st.ly, on(btn::L3), on(btn::LSTICK_TOUCH));
    let (dx, dy, d) = (110.0, 278.0, 24.0);
    c.button("↑", dx - d / 2.0, dy - 1.5 * d, d, d, on(btn::DPAD_UP));
    c.button("↓", dx - d / 2.0, dy + 0.5 * d, d, d, on(btn::DPAD_DOWN));
    c.button("←", dx - 1.5 * d, dy - d / 2.0, d, d, on(btn::DPAD_LEFT));
    c.button("→", dx + 0.5 * d, dy - d / 2.0, d, d, on(btn::DPAD_RIGHT));
    c.pad(210.0, 120.0, 150.0, st.lpad_x, st.lpad_y, st.lpad_pressure, on(btn::LPAD_TOUCH), on(btn::LPAD_CLICK));

    // Right side: trackpad, face buttons, stick.
    c.pad(600.0, 120.0, 150.0, st.rpad_x, st.rpad_y, st.rpad_pressure, on(btn::RPAD_TOUCH), on(btn::RPAD_CLICK));
    let (fx, fy, r) = (850.0, 168.0, 15.0);
    for (name, bit, ox, oy) in
        [("Y", btn::Y, 0.0, -1.0), ("A", btn::A, 0.0, 1.0), ("X", btn::X, -1.0, 0.0), ("B", btn::B, 1.0, 0.0)]
    {
        let (cx, cy) = (fx + ox * 30.0, fy + oy * 30.0);
        if on(bit) {
            c.circle(cx, cy, r, Some(LOCKED), None);
            c.label(name, cx, cy, r * 1.05, TEXT);
        } else {
            c.glyph(name, cx, cy, r);
        }
    }
    c.stick(850.0, 278.0, st.rx, st.ry, on(btn::R3), on(btn::RSTICK_TOUCH));

    // Center: system buttons, grips, motion.
    c.button("View", 392.0, 118.0, 80.0, 24.0, on(btn::VIEW));
    c.button("Menu ≡", 488.0, 118.0, 80.0, 24.0, on(btn::MENU));
    c.button("Steam", 392.0, 150.0, 80.0, 24.0, on(btn::STEAM));
    // QAM always opens/closes this overlay, so it can't be shown pressed here.
    c.rect(488.0, 150.0, 80.0, 24.0, 6.0, KEY_MOD);
    c.label("QAM: overlay", 528.0, 162.0, 11.0, DIM);
    c.button("L grip", 392.0, 182.0, 80.0, 22.0, on(btn::LGRIP_TOUCH));
    c.button("R grip", 488.0, 182.0, 80.0, 22.0, on(btn::RGRIP_TOUCH));
    let g = |v: i16| v as f32 / 32768.0;
    for (i, (name, v)) in [
        ("accel x", g(st.accel[0])),
        ("accel y", g(st.accel[1])),
        ("accel z", g(st.accel[2])),
        ("gyro x", g(st.gyro[0]) * 8.0),
        ("gyro y", g(st.gyro[1]) * 8.0),
        ("gyro z", g(st.gyro[2]) * 8.0),
    ]
    .into_iter()
    .enumerate()
    {
        let (x, y) = (392.0 + (i / 3) as f32 * 96.0, 216.0 + (i % 3) as f32 * 20.0);
        c.bar(name, x, y, 80.0, v, true, false);
    }
    c.text(
        &format!("report {:#04x}  seq {:3}  buttons {:#010x}", st.report_id, st.seq, st.buttons),
        392.0,
        284.0,
        11.0,
        DIM,
    );
}

/// Size of the "switch preset?" popup (logical pixels).
pub const PROMPT_W: u32 = 900;
pub const PROMPT_H: u32 = 128;

/// `preset` and `app` are shortened as needed so the question always fits on one line.
pub fn draw_prompt(preset: &str, app: &str, fonts: &mut Fonts, scale: f32) -> Option<Pixmap> {
    let (w, h) = (PROMPT_W as f32, PROMPT_H as f32);
    let mut pm = Pixmap::new((w * scale) as u32, (h * scale) as u32)?;
    let t = Transform::from_scale(scale, scale);
    if let Some(p) = rrect(0.0, 0.0, w, h, 18.0) {
        pm.fill_path(&p, &paint(BG), FillRule::Winding, t, None);
        let st = Stroke { width: 2.0, ..Default::default() };
        pm.stroke_path(&p, &paint(LATCHED), &st, t, None);
    }
    let mut c = Canvas { pm: &mut pm, fonts, s: scale, t };
    let qpx = 23.0 * scale;
    let max = (w - 48.0) * scale;
    let fixed = c.fonts.width("Allow switching to preset \u{201c}\u{201d} for ?", qpx);
    // Give the preset name up to a third, the app the rest.
    let preset = c.fonts.ellipsize(preset, qpx, (max - fixed) / 3.0);
    let pw = c.fonts.width(&preset, qpx);
    let app = c.fonts.ellipsize(app, qpx, max - fixed - pw);
    let text = format!("Allow switching to preset \u{201c}{preset}\u{201d} for {app}?");
    c.label(&text, w / 2.0, 38.0, 23.0, TEXT);

    // Two big button hints: (A) Switch   (B) Keep current settings
    let choices = [("A", "Always for this session"), ("X", "Once"), ("B", "No")];
    let (r, gap, label_px) = (17.0, 44.0, 20.0);
    let widths: Vec<f32> =
        choices.iter().map(|(_, l)| 2.0 * r + 12.0 + c.fonts.width(l, label_px * scale) / scale).collect();
    let total: f32 = widths.iter().sum::<f32>() + gap * (choices.len() - 1) as f32;
    let mut x = (w - total) / 2.0;
    let cy = 88.0;
    for ((letter, label), cw) in choices.iter().zip(&widths) {
        c.glyph(letter, x + r, cy, r);
        let lw = c.fonts.width(label, label_px * scale) / scale;
        c.label(label, x + 2.0 * r + 12.0 + lw / 2.0, cy, label_px, TEXT);
        x += cw + gap;
    }
    Some(pm)
}

/// Button mapping editor: waiting for a button press, or a grid of targets.
fn draw_map_edit(model: &Model, edit: MapEdit, fonts: &mut Fonts, pm: &mut Pixmap, g: &Geo, s: f32, t: Transform) {
    let entry = match edit {
        MapEdit::Capture { entry, .. } | MapEdit::List { entry, .. } => entry,
    };
    let name = model.status.mapping.get(entry).map(|i| i.label.as_str()).unwrap_or("?");
    let (options, cur) = model.map_options(entry);
    let mut c = Canvas { pm, fonts, s, t };
    let (x0, y0) = (PAD + 4.0, g.list_y);
    let w = WIDTH as f32 - 2.0 * PAD - 8.0;
    let h = g.list_h - 4.0;
    c.rect(x0, y0, w, h, 12.0, KEY_MOD);
    match edit {
        MapEdit::Capture { until, .. } => {
            let left = until.saturating_duration_since(std::time::Instant::now()).as_secs_f32();
            let cx = x0 + w / 2.0;
            c.label(&format!("Press the button that {name} should act as"), cx, y0 + h * 0.32, 22.0, TEXT);
            let now_is = options.get(cur).map(String::as_str).unwrap_or("?");
            c.label(&format!("currently: {now_is}"), cx, y0 + h * 0.32 + 34.0, 15.0, DIM);
            let bw = 360.0;
            let frac = left / CAPTURE_TIMEOUT.as_secs_f32();
            c.rect(cx - bw / 2.0, y0 + h * 0.66, bw, 6.0, 3.0, KEY);
            if frac > 0.0 {
                c.rect(cx - bw / 2.0, y0 + h * 0.66, bw * frac, 6.0, 3.0, LOCKED);
            }
            c.label("any button works, including A, B and the d-pad", cx, y0 + h * 0.66 + 24.0, 13.0, DIM);
        }
        MapEdit::List { focus, .. } => {
            // Header: "L4  →  currently A".
            let short = name.split(" (").next().unwrap_or(name);
            let cur_label = match cur {
                0 => "itself".to_string(),
                1 => "nothing".to_string(),
                _ => options.get(cur).cloned().unwrap_or_default(),
            };
            c.text(&format!("{short}  →  "), x0 + 22.0, y0 + 12.0, 18.0, TEXT);
            let hw = c.fonts.width(&format!("{short}  →  "), 18.0 * s) / s;
            c.text(&format!("currently {cur_label}"), x0 + 22.0 + hw, y0 + 14.0, 15.0, DIM);

            let cols = crate::osk::picker::columns();
            for (ci, col) in cols.iter().enumerate() {
                for (ri, g) in col.iter().enumerate() {
                    let (gx, gy) = picker_group_origin(y0, ci, ri);
                    c.text(g.title, gx, gy, 11.5, DIM);
                    for (k, &(o, label)) in g.chips.iter().enumerate() {
                        let x = gx + k as f32 * (CHIP_W + CHIP_GAP);
                        let y = gy + 18.0;
                        let fill = if o == focus { LOCKED } else { KEY };
                        c.rect(x, y, CHIP_W, CHIP_H, CHIP_H / 2.0, fill);
                        if o == cur {
                            c.rect_outline(
                                x + 1.0,
                                y + 1.0,
                                CHIP_W - 2.0,
                                CHIP_H - 2.0,
                                CHIP_H / 2.0 - 1.0,
                                RIGHT_C,
                                2.0,
                            );
                        }
                        let tc = TEXT;
                        let text = if o == cur { format!("{label} ✓") } else { label.to_string() };
                        c.label(&text, x + CHIP_W / 2.0, y + CHIP_H / 2.0, 14.0, tc);
                    }
                }
            }
        }
    }
}

const CHIP_W: f32 = 96.0;
const CHIP_H: f32 = 28.0;
const CHIP_GAP: f32 = 10.0;
const GROUP_H: f32 = 54.0;

/// Top-left of a picker group (title line), given the editor panel top.
fn picker_group_origin(panel_y: f32, col: usize, row: usize) -> (f32, f32) {
    let x0 = PAD + 4.0;
    let w = WIDTH as f32 - 2.0 * PAD - 8.0;
    let col_w = (w - 44.0) / 2.0;
    (x0 + 22.0 + col as f32 * (col_w + 22.0), panel_y + 46.0 + row as f32 * GROUP_H)
}

impl Geo {
    /// Option under a point in the mapping target picker.
    pub fn picker_at(&self, x: f32, y: f32) -> Option<usize> {
        let cols = crate::osk::picker::columns();
        for (ci, col) in cols.iter().enumerate() {
            for (ri, g) in col.iter().enumerate() {
                let (gx, gy) = picker_group_origin(self.list_y, ci, ri);
                for (k, &(o, _)) in g.chips.iter().enumerate() {
                    let cx = gx + k as f32 * (CHIP_W + CHIP_GAP);
                    let cy = gy + 18.0;
                    if (cx..cx + CHIP_W).contains(&x) && (cy..cy + CHIP_H).contains(&y) {
                        return Some(o);
                    }
                }
            }
        }
        None
    }
}

/// Size of the mode-switch banner (logical pixels).
pub const TOAST_W: u32 = 460;
pub const TOAST_H: u32 = 92;

pub fn draw_toast(t: &crate::osk::Toast, fonts: &mut Fonts, scale: f32) -> Option<Pixmap> {
    let (w, h) = (TOAST_W as f32, TOAST_H as f32);
    let mut pm = Pixmap::new((w * scale) as u32, (h * scale) as u32)?;
    let tf = Transform::from_scale(scale, scale);
    if let Some(p) = rrect(0.0, 0.0, w, h, 18.0) {
        pm.fill_path(&p, &paint(BG), FillRule::Winding, tf, None);
        let st = Stroke { width: 2.0, ..Default::default() };
        pm.stroke_path(&p, &paint(LATCHED), &st, tf, None);
    }
    let mut c = Canvas { pm: &mut pm, fonts, s: scale, t: tf };
    let title = c.fonts.ellipsize(&t.title, 24.0 * scale, (w - 40.0) * scale);
    let px = 15.0;
    let (who, glyph, next) = match &t.line {
        crate::osk::ToastLine::Text(s) if s.is_empty() => {
            c.label(&title, w / 2.0, h / 2.0, 24.0, TEXT);
            return Some(pm);
        }
        crate::osk::ToastLine::Text(s) => {
            c.label(&title, w / 2.0, 30.0, 24.0, TEXT);
            let s = c.fonts.ellipsize(s, px * scale, (w - 40.0) * scale);
            c.label(&s, w / 2.0, 66.0, px, DIM);
            return Some(pm);
        }
        crate::osk::ToastLine::Hold { who, glyph, next } => (who, glyph, next),
    };
    c.label(&title, w / 2.0, 30.0, 24.0, TEXT);
    // "<who>  ·  Hold (glyph) for <next>", centered as a whole.
    let pre = if who.is_empty() { "Hold".to_string() } else { format!("{who}  ·  Hold") };
    let post = format!("for {next}");
    let r = 12.0;
    let pre = c.fonts.ellipsize(&pre, px * scale, (w * 0.45) * scale);
    let pw = c.fonts.width(&pre, px * scale) / scale;
    let qw = c.fonts.width(&post, px * scale) / scale;
    let total = pw + 8.0 + 2.0 * r + 8.0 + qw;
    let x = (w - total) / 2.0;
    let cy = 66.0;
    c.label(&pre, x + pw / 2.0, cy, px, DIM);
    c.glyph(glyph, x + pw + 8.0 + r, cy, r);
    c.label(&post, x + pw + 16.0 + 2.0 * r + qw / 2.0, cy, px, DIM);
    Some(pm)
}

//! Desktop mode: a shared virtual mouse + keyboard, driven per controller.

use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use log::warn;

use crate::config;
use crate::keys;
use crate::proto::{Side, State, btn};
use crate::uinput::{DeviceSpec, VirtualDevice, ev::*};

/// The shared virtual mouse and keyboard. Keys are reference counted so two controllers
/// holding the same key don't release each other's press.
pub struct DesktopDevices {
    /// None in a recorder (replay without real devices).
    mouse: Option<VirtualDevice>,
    kbd: Option<VirtualDevice>,
    held: HashMap<u16, u32>,
    /// When set, every emitted pointer/scroll/key event is also recorded (mouse-study).
    pub trace: Option<Vec<(Instant, &'static str, i32, i32)>>,
}

impl DesktopDevices {
    pub fn create() -> io::Result<Self> {
        let mouse = VirtualDevice::create(&DeviceSpec {
            name: "puckd Mouse".into(),
            bustype: BUS_VIRTUAL,
            vendor: 0x1209,
            product: 0x5bc1,
            version: 1,
            phys: Some("puckd/desktop".into()),
            keys: keys::MOUSE_BUTTONS.to_vec(),
            rels: vec![REL_X, REL_Y, REL_WHEEL, REL_HWHEEL, REL_WHEEL_HI_RES, REL_HWHEEL_HI_RES],
            props: vec![INPUT_PROP_POINTER],
            ..Default::default()
        })?;
        let kbd = VirtualDevice::create(&DeviceSpec {
            name: "puckd Keyboard".into(),
            bustype: BUS_VIRTUAL,
            vendor: 0x1209,
            product: 0x5bc2,
            version: 1,
            phys: Some("puckd/desktop".into()),
            keys: keys::keyboard_keys(),
            ..Default::default()
        })?;
        Ok(Self { mouse: Some(mouse), kbd: Some(kbd), held: HashMap::new(), trace: None })
    }

    /// No real devices: only records what would be emitted (for `puckd replay`).
    pub fn recorder() -> Self {
        Self { mouse: None, kbd: None, held: HashMap::new(), trace: Some(Vec::new()) }
    }

    pub fn key(&mut self, code: u16, down: bool) {
        let n = self.held.entry(code).or_insert(0);
        let before = *n;
        if down {
            *n += 1;
        } else {
            *n = n.saturating_sub(1);
        }
        let after = *n;
        if (before == 0) != (after == 0) {
            self.record("key", code as i32, (after > 0) as i32);
            let dev = if keys::is_mouse_button(code) { &mut self.mouse } else { &mut self.kbd };
            if let Some(d) = dev {
                d.key(code, after > 0);
            }
        }
    }

    fn record(&mut self, kind: &'static str, a: i32, b: i32) {
        if let Some(t) = &mut self.trace {
            t.push((Instant::now(), kind, a, b));
        }
    }

    pub fn motion(&mut self, dx: i32, dy: i32) {
        self.record("move", dx, dy);
        if let Some(m) = &mut self.mouse {
            m.rel(REL_X, dx);
            m.rel(REL_Y, dy);
        }
    }

    /// Hi-res wheel units (120 per notch); `notches` are the legacy wheel clicks crossed.
    pub fn wheel(&mut self, v_hires: i32, v_notches: i32, h_hires: i32, h_notches: i32) {
        self.record("wheel", v_hires, h_hires);
        if let Some(m) = &mut self.mouse {
            m.rel(REL_WHEEL_HI_RES, v_hires);
            m.rel(REL_WHEEL, v_notches);
            m.rel(REL_HWHEEL_HI_RES, h_hires);
            m.rel(REL_HWHEEL, h_notches);
        }
    }

    pub fn sync(&mut self) {
        // Keyboard first, so modifier+click combos arrive in order.
        if let Some(k) = &mut self.kbd
            && let Err(e) = k.sync()
        {
            warn!("keyboard: {e}");
        }
        if let Some(m) = &mut self.mouse
            && let Err(e) = m.sync()
        {
            warn!("mouse: {e}");
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Action {
    Keys(Vec<u16>),
    Mode,
    Osk,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Source {
    Button(u32),
    LeftTrigger,
    RightTrigger,
}

/// Haptic feedback requested by desktop handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feedback {
    PointerTick,
    ScrollTick,
    /// A click felt on this side (trackpad click, or a mouse button pressed with a trigger).
    Click(Side),
    /// Letting go of a click (lighter).
    Release(Side),
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub toggle_mode: bool,
    pub toggle_osk: bool,
    pub feedback: Vec<Feedback>,
}

/// Which side of the controller an input is on (for haptics).
fn source_side(src: Source) -> Side {
    const LEFT: u32 = btn::LB
        | btn::L3
        | btn::L4
        | btn::L5
        | btn::LPAD_CLICK
        | btn::LT_CLICK
        | btn::VIEW
        | btn::DPAD_UP
        | btn::DPAD_DOWN
        | btn::DPAD_LEFT
        | btn::DPAD_RIGHT;
    match src {
        Source::LeftTrigger => Side::Left,
        Source::RightTrigger => Side::Right,
        Source::Button(b) if b & LEFT != 0 => Side::Left,
        Source::Button(_) => Side::Right,
    }
}

/// Wheel accumulator: hi-res units -> legacy notches.
#[derive(Debug, Default, Clone, Copy)]
struct Wheel {
    frac: f32,
    hires_acc: i32,
}

impl Wheel {
    /// Add hi-res units; returns (hires, notches) to emit.
    fn add(&mut self, units: f32) -> (i32, i32) {
        self.frac += units;
        let whole = self.frac.trunc() as i32;
        self.frac -= whole as f32;
        let before = self.hires_acc / 120;
        self.hires_acc += whole;
        let notches = self.hires_acc / 120 - before;
        // Keep the accumulator small but preserve the remainder.
        if self.hires_acc.abs() > 120 * 1000 {
            self.hires_acc %= 120;
        }
        (whole, notches)
    }
}

/// One-euro filter (Casiez et al.): smooths a lot when slow (kills jitter), little when
/// fast (keeps latency low).
#[derive(Debug, Default, Clone, Copy)]
struct OneEuro {
    x: f32,
    dx: f32,
    init: bool,
}

impl OneEuro {
    fn alpha(cutoff: f32, dt: f32) -> f32 {
        let tau = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
        1.0 / (1.0 + tau / dt)
    }

    fn filter(&mut self, v: f32, dt: f32, min_cutoff: f32, beta: f32) -> f32 {
        if !self.init {
            *self = Self { x: v, dx: 0.0, init: true };
            return v;
        }
        let raw_dx = (v - self.x) / dt;
        self.dx += Self::alpha(1.0, dt) * (raw_dx - self.dx);
        let cutoff = min_cutoff + beta * self.dx.abs();
        self.x += Self::alpha(cutoff, dt) * (v - self.x);
        self.x
    }
}

/// Per-trackpad tracking: filtered position, and output held back a few samples so the
/// thumb rolling off the pad at lift-off (or pressing down for a click) can be dropped.
#[derive(Debug, Default, Clone)]
struct PadTrack {
    touching: bool,
    since: Option<Instant>,
    fx: OneEuro,
    fy: OneEuro,
    pos: (f32, f32),
    pending: std::collections::VecDeque<(f32, f32)>,
}

impl PadTrack {
    fn start(raw: (f32, f32), now: Instant) -> Self {
        let mut p = Self { touching: true, since: Some(now), pos: raw, ..Default::default() };
        p.fx.filter(raw.0, 0.004, 1.0, 0.0);
        p.fy.filter(raw.1, 0.004, 1.0, 0.0);
        p
    }

    /// Filtered movement (pad units) and its speed (units/s) for this sample.
    fn step(&mut self, raw: (f32, f32), dt: f32, smoothing: f32) -> ((f32, f32), f32) {
        // smoothing 0 → light filtering (12 Hz floor), 0.95 → heavy (1.5 Hz floor).
        let min_cutoff = 1.5 + (1.0 - smoothing.clamp(0.0, 0.95)) * 10.5;
        let beta = 0.0004;
        let np = (self.fx.filter(raw.0, dt, min_cutoff, beta), self.fy.filter(raw.1, dt, min_cutoff, beta));
        let d = (np.0 - self.pos.0, np.1 - self.pos.1);
        self.pos = np;
        (d, (d.0 * d.0 + d.1 * d.1).sqrt() / dt)
    }

    /// Queue an output delta; returns the one that is old enough to emit.
    fn delay(&mut self, d: (f32, f32), frames: usize) -> Option<(f32, f32)> {
        self.pending.push_back(d);
        if self.pending.len() > frames { self.pending.pop_front() } else { None }
    }
}

/// Distance from the click point (pad units, ~4.5% of the pad) before a held click starts
/// dragging. A thumb holding a click in place wanders up to ~2600 (measured).
const DRAG_START: f32 = 3000.0;

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Slow wander of a resting thumb is ignored; real (even slow, precise) moves pass.
fn rest_scale(speed: f32) -> f32 {
    smoothstep(400.0, 2000.0, speed)
}

/// Pressing down (towards a click) stops the motion: the thumb rolls while pressing.
/// Normal moving stays below ~2200 pressure; clicks register around 3000–8000.
fn press_scale(pressure: u16) -> f32 {
    1.0 - smoothstep(1600.0, 3000.0, pressure as f32)
}

pub struct DesktopState {
    bindings: Vec<(Source, Action)>,
    active: Vec<bool>,
    lt_on: bool,
    rt_on: bool,
    last: Option<Instant>,
    rpad: PadTrack,
    lpad: PadTrack,
    sub: (f32, f32),
    vel: (f32, f32),
    gliding: bool,
    tick_acc: f32,
    freeze_until: Option<Instant>,
    vwheel: Wheel,
    hwheel: Wheel,
    last_buttons: u32,
    drag_origin: Option<(f32, f32)>,
    dragging: bool,
}

impl DesktopState {
    pub fn new(cfg: &config::Desktop) -> Self {
        let mut bindings = Vec::new();
        for (k, v) in cfg.bindings() {
            let src = match k.as_str() {
                "lt" => Source::LeftTrigger,
                "rt" => Source::RightTrigger,
                n => match btn::from_name(n) {
                    Some(b) => Source::Button(b),
                    None => continue,
                },
            };
            let act = match v.to_ascii_lowercase().as_str() {
                "mode" => Action::Mode,
                "osk" => Action::Osk,
                _ => match keys::parse_combo(&v) {
                    Ok(c) if !c.is_empty() => Action::Keys(c),
                    _ => continue,
                },
            };
            bindings.push((src, act));
        }
        let n = bindings.len();
        Self {
            bindings,
            active: vec![false; n],
            lt_on: false,
            rt_on: false,
            last: None,
            rpad: PadTrack::default(),
            lpad: PadTrack::default(),
            sub: (0.0, 0.0),
            vel: (0.0, 0.0),
            gliding: false,
            tick_acc: 0.0,
            freeze_until: None,
            vwheel: Wheel::default(),
            hwheel: Wheel::default(),
            last_buttons: 0,
            drag_origin: None,
            dragging: false,
        }
    }

    /// Release everything this controller holds and stop motion.
    pub fn release_all(&mut self, dev: &mut DesktopDevices) {
        for (i, (_, act)) in self.bindings.iter().enumerate() {
            if self.active[i] {
                if let Action::Keys(codes) = act {
                    for &c in codes.iter().rev() {
                        dev.key(c, false);
                    }
                }
                self.active[i] = false;
            }
        }
        dev.sync();
        self.gliding = false;
        self.vel = (0.0, 0.0);
        self.rpad = PadTrack::default();
        self.lpad = PadTrack::default();
        self.last = None;
        self.lt_on = false;
        self.rt_on = false;
        self.last_buttons = 0;
    }

    pub fn is_gliding(&self) -> bool {
        self.gliding
    }

    /// `tick_distance` <= 0 disables haptic ticks for this motion.
    fn emit_pointer(&mut self, dev: &mut DesktopDevices, dx: f32, dy: f32, out: &mut Outcome, tick_distance: f32) {
        self.sub.0 += dx;
        self.sub.1 += dy;
        let (ix, iy) = (self.sub.0.trunc(), self.sub.1.trunc());
        self.sub.0 -= ix;
        self.sub.1 -= iy;
        if ix != 0.0 || iy != 0.0 {
            dev.motion(ix as i32, iy as i32);
        }
        if tick_distance > 0.0 {
            self.tick_acc += (dx * dx + dy * dy).sqrt();
            if self.tick_acc >= tick_distance {
                self.tick_acc %= tick_distance;
                out.feedback.push(Feedback::PointerTick);
            }
        }
    }

    fn emit_scroll(&mut self, dev: &mut DesktopDevices, v_units: f32, h_units: f32, out: &mut Outcome, ticks: bool) {
        let (vh, vn) = self.vwheel.add(v_units);
        let (hh, hn) = self.hwheel.add(h_units);
        if vh != 0 || hh != 0 {
            dev.wheel(vh, vn, hh, hn);
        }
        if ticks && (vn != 0 || hn != 0) {
            out.feedback.push(Feedback::ScrollTick);
        }
    }

    /// Inertia step without a new report (called from the main loop timer).
    pub fn glide(&mut self, now: Instant, cfg: &config::Desktop, dev: &mut DesktopDevices) -> Outcome {
        let mut out = Outcome::default();
        if !self.gliding {
            return out;
        }
        let dt = self.last.map(|l| now.saturating_duration_since(l).as_secs_f32()).unwrap_or(0.0).min(0.05);
        self.last = Some(now);
        self.step_glide(dt, cfg, dev, &mut out);
        dev.sync();
        out
    }

    fn step_glide(&mut self, dt: f32, cfg: &config::Desktop, dev: &mut DesktopDevices, out: &mut Outcome) {
        let decay = (-cfg.inertia_friction.max(0.1) * dt).exp();
        self.vel.0 *= decay;
        self.vel.1 *= decay;
        let speed = (self.vel.0 * self.vel.0 + self.vel.1 * self.vel.1).sqrt();
        if speed < 40.0 {
            self.gliding = false;
            self.vel = (0.0, 0.0);
            return;
        }
        let (dx, dy) = (self.vel.0 * dt, self.vel.1 * dt);
        self.emit_pointer(dev, dx, dy, out, 0.0);
    }

    /// `only`: in gamepad mode, just the trackpads drive the mouse; button bindings are
    /// limited to these bits (pad clicks), sticks/triggers/gyro are left to the gamepad.
    pub fn update(
        &mut self,
        st: &State,
        now: Instant,
        cfg: &config::Desktop,
        hcfg: &config::Haptics,
        dev: &mut DesktopDevices,
        only: Option<u32>,
    ) -> Outcome {
        let mut out = Outcome::default();
        let dt = self.last.map(|l| now.saturating_duration_since(l).as_secs_f32()).unwrap_or(0.004).clamp(0.0005, 0.05);
        self.last = Some(now);
        let b = st.buttons;
        let pointer_tick = if hcfg.pointer_ticks { hcfg.tick_distance } else { 0.0 };

        // --- buttons ---
        let thr = cfg.trigger_threshold.clamp(0.05, 0.95) * 32767.0;
        let hyst = 0.05 * 32767.0;
        let lt = st.lt as f32;
        let rt = st.rt as f32;
        self.lt_on = if self.lt_on { lt > thr - hyst } else { lt > thr };
        self.rt_on = if self.rt_on { rt > thr - hyst } else { rt > thr };
        for i in 0..self.bindings.len() {
            let pressed = match (self.bindings[i].0, only) {
                (Source::Button(bit), Some(m)) if bit & m == 0 => false,
                (Source::LeftTrigger | Source::RightTrigger, Some(_)) => false,
                (Source::Button(bit), _) => b & bit != 0,
                (Source::LeftTrigger, None) => self.lt_on,
                (Source::RightTrigger, None) => self.rt_on,
            };
            if pressed == self.active[i] {
                continue;
            }
            self.active[i] = pressed;
            match &self.bindings[i].1 {
                Action::Keys(codes) => {
                    if pressed {
                        for &c in codes {
                            dev.key(c, true);
                        }
                        // Mouse buttons from triggers/buttons click like a mouse button would.
                        // (Trackpad clicks get their click below.)
                        let src = self.bindings[i].0;
                        let pad = matches!(src, Source::Button(b) if b & (btn::RPAD_CLICK | btn::LPAD_CLICK) != 0);
                        if hcfg.pad_clicks && !pad && codes.iter().any(|&c| keys::is_mouse_button(c)) {
                            out.feedback.push(Feedback::Click(source_side(src)));
                        }
                    } else {
                        for &c in codes.iter().rev() {
                            dev.key(c, false);
                        }
                    }
                }
                Action::Mode => out.toggle_mode |= pressed,
                Action::Osk => out.toggle_osk |= pressed,
            }
        }

        // Trackpad click haptics + jitter suppression.
        let clicks = btn::RPAD_CLICK | btn::LPAD_CLICK;
        let changed = (b ^ self.last_buttons) & clicks;
        if changed != 0 {
            self.freeze_until = Some(now + Duration::from_millis(cfg.click_suppress_ms));
            if hcfg.pad_clicks {
                // Click in: a click; let go: a lighter tick (so a drag's start and end are felt).
                for (bit, side) in [(btn::RPAD_CLICK, Side::Right), (btn::LPAD_CLICK, Side::Left)] {
                    if changed & bit != 0 {
                        out.feedback.push(if b & bit != 0 { Feedback::Click(side) } else { Feedback::Release(side) });
                    }
                }
            }
        }
        self.last_buttons = b;
        let frozen = self.freeze_until.is_some_and(|t| now < t);

        // Output is held back this many samples (4 ms each) so lift-off roll can be dropped.
        let delay = (cfg.liftoff_delay_ms / 4).min(10) as usize;
        if changed & btn::RPAD_CLICK != 0 {
            self.drag_origin = None;
            self.dragging = false;
        }
        if changed != 0 {
            // Motion just before a click is the press, not a move.
            self.rpad.pending.clear();
            self.lpad.pending.clear();
        }

        // --- right trackpad: pointer ---
        let touching = b & btn::RPAD_TOUCH != 0;
        let raw = (st.rpad_x as f32, -(st.rpad_y as f32));
        if touching {
            self.gliding = false;
            if !self.rpad.touching {
                // Touch-down: no motion, start tracking from here.
                self.rpad = PadTrack::start(raw, now);
                self.vel = (0.0, 0.0);
                self.sub = (0.0, 0.0);
            } else {
                let (d, speed) = self.rpad.step(raw, dt, cfg.smoothing);
                // While the pad is held clicked, pressure is high on purpose: no pressure fade,
                // but the pointer only starts dragging once the thumb has clearly moved, so a
                // click held in place doesn't wobble.
                let held = b & btn::RPAD_CLICK != 0;
                let press = if held { 1.0 } else { press_scale(st.rpad_pressure) };
                let mut k = if frozen { 0.0 } else { rest_scale(speed) * press };
                if held && !self.dragging {
                    // Distance from where the click went in (jitter alone stays below this).
                    let o = *self.drag_origin.get_or_insert(self.rpad.pos);
                    let (ox, oy) = (self.rpad.pos.0 - o.0, self.rpad.pos.1 - o.1);
                    if (ox * ox + oy * oy).sqrt() > DRAG_START {
                        self.dragging = true;
                    } else {
                        k = 0.0;
                    }
                }
                let gain = 0.010 * cfg.pointer_speed;
                let accel = 1.0 + cfg.pointer_accel.max(0.0) * ((speed - 30_000.0) / 150_000.0).clamp(0.0, 2.0);
                let px = (d.0 * gain * accel * k, d.1 * gain * accel * k);
                if let Some((dx, dy)) = self.rpad.delay(px, delay) {
                    let inst = (dx / dt, dy / dt);
                    self.vel = (0.7 * self.vel.0 + 0.3 * inst.0, 0.7 * self.vel.1 + 0.3 * inst.1);
                    self.emit_pointer(dev, dx, dy, &mut out, pointer_tick);
                }
            }
        } else if self.rpad.touching {
            // Lift-off: the held-back samples are the thumb rolling off; drop them.
            self.rpad.touching = false;
            self.rpad.pending.clear();
            let speed = (self.vel.0 * self.vel.0 + self.vel.1 * self.vel.1).sqrt();
            let short = self.rpad.since.is_some_and(|t| now.duration_since(t) < Duration::from_millis(350));
            // Inertia only for a real flick: a short, fast touch.
            self.gliding = cfg.inertia && short && speed > 1500.0 && !frozen;
            if !self.gliding {
                self.vel = (0.0, 0.0);
            }
        } else if self.gliding {
            self.step_glide(dt, cfg, dev, &mut out);
        }

        // --- left trackpad: scroll ---
        let ltouch = b & btn::LPAD_TOUCH != 0;
        let lraw = (st.lpad_x as f32, st.lpad_y as f32);
        if ltouch {
            if !self.lpad.touching {
                self.lpad = PadTrack::start(lraw, now);
            } else {
                let (d, speed) = self.lpad.step(lraw, dt, cfg.smoothing);
                let press = if b & btn::LPAD_CLICK != 0 { 1.0 } else { press_scale(st.lpad_pressure) };
                let k = if frozen { 0.0 } else { rest_scale(speed) * press };
                // 120 hi-res units (one notch) per 10000 pad units (~6.5 notches per full swipe),
                // more for fast swipes.
                let accel = 1.0 + cfg.scroll_accel.max(0.0) * ((speed - 40_000.0) / 80_000.0).clamp(0.0, 4.0);
                let s = 120.0 / 10000.0 * cfg.scroll_speed * accel * k * if cfg.natural_scroll { -1.0 } else { 1.0 };
                let h = if cfg.horizontal_scroll { d.0 * s } else { 0.0 };
                if let Some((v, h)) = self.lpad.delay((d.1 * s, h), delay) {
                    self.emit_scroll(dev, v, h, &mut out, hcfg.scroll_ticks);
                }
            }
        } else if self.lpad.touching {
            self.lpad.touching = false;
            self.lpad.pending.clear();
        }

        if only.is_some() {
            dev.sync();
            return out;
        }

        // --- sticks ---
        let dz = cfg.stick_deadzone.clamp(0.0, 0.9);
        let curve = |x: i16, y: i16| -> (f32, f32) {
            let (fx, fy) = (x as f32 / 32767.0, y as f32 / 32767.0);
            let m = (fx * fx + fy * fy).sqrt().min(1.0);
            if m <= dz {
                return (0.0, 0.0);
            }
            let n = (m - dz) / (1.0 - dz);
            let s = n * n / m;
            (fx * s, fy * s)
        };
        if cfg.stick_pointer_speed > 0.0 {
            let (x, y) = curve(st.rx, st.ry);
            if x != 0.0 || y != 0.0 {
                let v = cfg.stick_pointer_speed * dt;
                self.emit_pointer(dev, x * v, -y * v, &mut out, 0.0);
            }
        }
        if cfg.stick_scroll_speed > 0.0 {
            let (x, y) = curve(st.lx, st.ly);
            if x != 0.0 || y != 0.0 {
                let v = cfg.stick_scroll_speed * 120.0 * dt * if cfg.natural_scroll { -1.0 } else { 1.0 };
                let h = if cfg.horizontal_scroll { x * v } else { 0.0 };
                self.emit_scroll(dev, y * v, h, &mut out, false);
            }
        }

        // --- gyro mouse while a grip is held ---
        if cfg.gyro_mouse && b & (btn::LGRIP_TOUCH | btn::RGRIP_TOUCH) != 0 {
            // raw gyro: 16.384 units per °/s. Yaw = raw Z, pitch = raw X (SDL convention).
            let yaw = st.gyro[2] as f32 / 16.384;
            let pitch = st.gyro[0] as f32 / 16.384;
            let k = cfg.gyro_speed * dt;
            self.emit_pointer(dev, -yaw * k, -pitch * k, &mut out, 0.0);
        }

        dev.sync();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_notches() {
        let mut w = Wheel::default();
        assert_eq!(w.add(60.0), (60, 0));
        assert_eq!(w.add(60.0), (60, 1));
        assert_eq!(w.add(-130.0), (-130, -1));
        assert_eq!(w.add(0.5), (0, 0));
        assert_eq!(w.add(0.6), (1, 0));
    }

    fn moved(dev: &mut DesktopDevices) -> i32 {
        dev.trace.as_mut().unwrap().drain(..).filter(|e| e.1 == "move").map(|e| e.2.abs() + e.3.abs()).sum()
    }

    #[test]
    fn liftoff_roll_and_rest_are_ignored() {
        let cfg = config::Desktop::default();
        let h = config::Haptics { enabled: false, ..Default::default() };
        let mut dev = DesktopDevices::recorder();
        let mut d = DesktopState::new(&cfg);
        let t0 = Instant::now();
        let mut t = t0;
        let mut feed = |d: &mut DesktopState, dev: &mut DesktopDevices, x: i16, touch: bool| {
            t += Duration::from_millis(4);
            let st = State { buttons: if touch { btn::RPAD_TOUCH } else { 0 }, rpad_x: x, ..Default::default() };
            d.update(&st, t, &cfg, &h, dev, None);
        };
        // Resting thumb with a little wobble: nothing moves.
        for i in 0..200 {
            feed(&mut d, &mut dev, (i % 3) as i16 * 15, true);
        }
        assert_eq!(moved(&mut dev), 0);
        // Steady move, then a big roll-off right before lifting.
        let mut x = 0i16;
        for _ in 0..60 {
            x += 150;
            feed(&mut d, &mut dev, x, true);
        }
        let steady = moved(&mut dev);
        assert!(steady > 50, "steady move produced {steady}");
        for _ in 0..3 {
            x += 1500;
            feed(&mut d, &mut dev, x, true);
        }
        feed(&mut d, &mut dev, x, false);
        for _ in 0..50 {
            feed(&mut d, &mut dev, x, false);
        }
        // The roll (3 samples of 1500) is far more than the steady pace; it must not show up.
        assert!(moved(&mut dev) < steady / 10);
    }

    #[test]
    fn held_click_drags_only_after_real_movement() {
        let cfg = config::Desktop::default();
        let h = config::Haptics { enabled: false, ..Default::default() };
        let mut dev = DesktopDevices::recorder();
        let mut d = DesktopState::new(&cfg);
        let mut t = Instant::now();
        let mut feed = |d: &mut DesktopState, dev: &mut DesktopDevices, x: i16, click: bool, p: u16| {
            t += Duration::from_millis(4);
            let mut b = btn::RPAD_TOUCH;
            if click {
                b |= btn::RPAD_CLICK;
            }
            let st = State { buttons: b, rpad_x: x, rpad_pressure: p, ..Default::default() };
            d.update(&st, t, &cfg, &h, dev, None);
        };
        for _ in 0..20 {
            feed(&mut d, &mut dev, 0, false, 500);
        }
        // Click and hold in place with ±1000 units of wobble: no movement.
        for i in 0..100 {
            feed(&mut d, &mut dev, if i % 2 == 0 { 1000 } else { -1000 }, true, 20000);
        }
        assert_eq!(moved(&mut dev), 0);
        // Same held click, now a real drag: the pointer follows.
        let mut x = 0i16;
        for _ in 0..80 {
            x += 200;
            feed(&mut d, &mut dev, x, true, 20000);
        }
        let dragged = moved(&mut dev);
        assert!(dragged > 50, "drag produced {dragged}");
        // The held button is the left mouse button throughout.
        let keys: Vec<_> = dev.trace.as_ref().unwrap().iter().filter(|e| e.1 == "key").collect();
        assert!(keys.is_empty() || keys.iter().all(|e| e.2 == BTN_LEFT as i32));
    }
}

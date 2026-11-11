//! Desktop mode: a shared virtual mouse + keyboard, driven per controller.

use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use log::warn;

use crate::config;
use crate::keys;
use crate::proto::{State, btn};
use crate::uinput::{DeviceSpec, VirtualDevice, ev::*};

/// The shared virtual mouse and keyboard. Keys are reference counted so two controllers
/// holding the same key don't release each other's press.
pub struct DesktopDevices {
    mouse: VirtualDevice,
    kbd: VirtualDevice,
    held: HashMap<u16, u32>,
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
        Ok(Self { mouse, kbd, held: HashMap::new() })
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
            let dev = if keys::is_mouse_button(code) { &mut self.mouse } else { &mut self.kbd };
            dev.key(code, after > 0);
        }
    }

    pub fn motion(&mut self, dx: i32, dy: i32) {
        self.mouse.rel(REL_X, dx);
        self.mouse.rel(REL_Y, dy);
    }

    /// Hi-res wheel units (120 per notch); `notches` are the legacy wheel clicks crossed.
    pub fn wheel(&mut self, v_hires: i32, v_notches: i32, h_hires: i32, h_notches: i32) {
        self.mouse.rel(REL_WHEEL_HI_RES, v_hires);
        self.mouse.rel(REL_WHEEL, v_notches);
        self.mouse.rel(REL_HWHEEL_HI_RES, h_hires);
        self.mouse.rel(REL_HWHEEL, h_notches);
    }

    pub fn sync(&mut self) {
        // Keyboard first, so modifier+click combos arrive in order.
        if let Err(e) = self.kbd.sync() {
            warn!("keyboard: {e}");
        }
        if let Err(e) = self.mouse.sync() {
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
    Click,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub toggle_mode: bool,
    pub toggle_osk: bool,
    pub feedback: Vec<Feedback>,
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

#[derive(Debug, Default, Clone, Copy)]
struct PadTrack {
    touching: bool,
    pos: (f32, f32),
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
            if hcfg.pad_clicks && b & changed != 0 {
                out.feedback.push(Feedback::Click);
            }
        }
        self.last_buttons = b;
        let frozen = self.freeze_until.is_some_and(|t| now < t);

        // --- right trackpad: pointer ---
        let alpha = 1.0 - cfg.smoothing.clamp(0.0, 0.95);
        let touching = b & btn::RPAD_TOUCH != 0;
        let raw = (st.rpad_x as f32, -(st.rpad_y as f32));
        if touching {
            self.gliding = false;
            if !self.rpad.touching {
                // Touch-down: no motion, start tracking from here.
                self.rpad = PadTrack { touching: true, pos: raw };
                self.vel = (0.0, 0.0);
                self.sub = (0.0, 0.0);
            } else {
                let np = (
                    self.rpad.pos.0 + alpha * (raw.0 - self.rpad.pos.0),
                    self.rpad.pos.1 + alpha * (raw.1 - self.rpad.pos.1),
                );
                let d = (np.0 - self.rpad.pos.0, np.1 - self.rpad.pos.1);
                self.rpad.pos = np;
                if !frozen {
                    let dist = (d.0 * d.0 + d.1 * d.1).sqrt();
                    let speed = dist / dt; // pad units per second
                    let gain = 0.010 * cfg.pointer_speed;
                    let accel = 1.0 + cfg.pointer_accel.max(0.0) * ((speed - 30_000.0) / 150_000.0).clamp(0.0, 2.0);
                    let (dx, dy) = (d.0 * gain * accel, d.1 * gain * accel);
                    let inst = (dx / dt, dy / dt);
                    self.vel = (0.6 * self.vel.0 + 0.4 * inst.0, 0.6 * self.vel.1 + 0.4 * inst.1);
                    self.emit_pointer(dev, dx, dy, &mut out, pointer_tick);
                } else {
                    self.vel = (0.0, 0.0);
                }
            }
        } else if self.rpad.touching {
            self.rpad.touching = false;
            let speed = (self.vel.0 * self.vel.0 + self.vel.1 * self.vel.1).sqrt();
            self.gliding = cfg.inertia && speed > 600.0 && !frozen;
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
                self.lpad = PadTrack { touching: true, pos: lraw };
            } else {
                let np = (
                    self.lpad.pos.0 + alpha * (lraw.0 - self.lpad.pos.0),
                    self.lpad.pos.1 + alpha * (lraw.1 - self.lpad.pos.1),
                );
                let d = (np.0 - self.lpad.pos.0, np.1 - self.lpad.pos.1);
                self.lpad.pos = np;
                if !frozen {
                    // 120 hi-res units (one notch) per 10000 pad units (~6.5 notches per full swipe).
                    let k = 120.0 / 10000.0 * cfg.scroll_speed * if cfg.natural_scroll { -1.0 } else { 1.0 };
                    let h = if cfg.horizontal_scroll { d.0 * k } else { 0.0 };
                    self.emit_scroll(dev, d.1 * k, h, &mut out, hcfg.scroll_ticks);
                }
            }
        } else {
            self.lpad.touching = false;
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
}

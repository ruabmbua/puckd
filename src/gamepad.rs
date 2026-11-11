//! Virtual gamepad (per controller): the emulated pad, an optional motion sensor device,
//! and force feedback.

use std::io;
use std::time::Instant;

use log::{debug, warn};

use crate::config::{self, Profile};
use crate::ff::{self, FfEngine};
use crate::hid::HidInfo;
use crate::proto::{State, btn};
use crate::uinput::{AbsSpec, DeviceSpec, VirtualDevice, ev::*};

/// What a profile looks like to the outside world.
struct Identity {
    name: &'static str,
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

const XBOX360: Identity =
    Identity { name: "Microsoft X-Box 360 pad", bustype: BUS_USB, vendor: 0x045e, product: 0x028e, version: 0x0114 };
/// Version 0x0407 matches the USB Elite 2 entry in the community SDL_GameControllerDB
/// (used by gilrs and by Steam's SDL mappings); that entry expects one extra button (b11)
/// before the four paddles (b12..b15), which is where QAM goes.
const XBOX_ELITE: Identity = Identity {
    name: "Microsoft X-Box One Elite 2 pad",
    bustype: BUS_USB,
    vendor: 0x045e,
    product: 0x0b00,
    version: 0x0407,
};
/// Looks like the Steam Deck's built-in controller as the kernel's hid-steam driver presents
/// it (28de:1205, version 0x0111). Both SDL's built-in mapping database and the community
/// one (gilrs, Steam) know this exact layout, paddles and QAM included, so every consumer
/// agrees on it. The layout below must stay identical to hid-steam's; the extras (touches,
/// pad pressure) come after it so they don't shift the button/axis indices.
const EXTENDED: Identity =
    Identity { name: "Steam Deck (puckd)", bustype: BUS_USB, vendor: 0x28de, product: 0x1205, version: 0x0111 };

/// Trackpad resolution (units/mm), same constant hid-steam uses.
const PAD_RES: i32 = 1638;

fn identity(p: Profile) -> &'static Identity {
    match p {
        Profile::Xbox360 => &XBOX360,
        Profile::XboxElite => &XBOX_ELITE,
        Profile::Extended => &EXTENDED,
    }
}

fn pad_spec(p: Profile, name: &str, phys: &str) -> DeviceSpec {
    let id = identity(p);
    let stick = |c| AbsSpec::new(c, -32768, 32767).ff(16, 128);
    let mut s = DeviceSpec {
        name: name.to_string(),
        bustype: id.bustype,
        vendor: id.vendor,
        product: id.product,
        version: id.version,
        phys: Some(phys.to_string()),
        ff: ff::SUPPORTED.to_vec(),
        ff_effects_max: ff::MAX_EFFECTS,
        ..Default::default()
    };
    let xpad_keys =
        [BTN_SOUTH, BTN_EAST, BTN_X, BTN_Y, BTN_TL, BTN_TR, BTN_SELECT, BTN_START, BTN_MODE, BTN_THUMBL, BTN_THUMBR];
    match p {
        Profile::Xbox360 | Profile::XboxElite => {
            let tmax = if p == Profile::Xbox360 { 255 } else { 1023 };
            s.keys = xpad_keys.to_vec();
            if p == Profile::XboxElite {
                // HAPPY1 = QAM (the database's b11), HAPPY5..8 = paddles (b12..b15).
                s.keys.push(BTN_TRIGGER_HAPPY1);
                s.keys.extend((0..4).map(|i| BTN_TRIGGER_HAPPY1 + 4 + i));
            }
            s.abs = vec![
                stick(ABS_X),
                stick(ABS_Y),
                stick(ABS_RX),
                stick(ABS_RY),
                AbsSpec::new(ABS_Z, 0, tmax),
                AbsSpec::new(ABS_RZ, 0, tmax),
                AbsSpec::new(ABS_HAT0X, -1, 1),
                AbsSpec::new(ABS_HAT0Y, -1, 1),
            ];
        }
        Profile::Extended => {
            s.keys = xpad_keys.to_vec();
            s.keys.extend([
                BTN_TL2,
                BTN_TR2,
                BTN_DPAD_UP,
                BTN_DPAD_DOWN,
                BTN_DPAD_LEFT,
                BTN_DPAD_RIGHT,
                BTN_GRIPL,
                BTN_GRIPR,
                BTN_GRIPL2,
                BTN_GRIPR2,
                BTN_BASE,
                BTN_THUMB,
                BTN_THUMB2,
            ]);
            s.keys.extend((0..6).map(|i| BTN_TRIGGER_HAPPY1 + i));
            // hid-steam (Deck) axes: no ABS_Z/RZ; triggers on HAT2Y (left) / HAT2X (right).
            // Resolutions make SDL treat the HAT axes as analog axes, not d-pad hats.
            let pad = |c| AbsSpec::new(c, -32768, 32767).res(PAD_RES);
            let trig = |c| AbsSpec::new(c, 0, 32767).res(1);
            s.abs = vec![
                stick(ABS_X),
                stick(ABS_Y),
                stick(ABS_RX),
                stick(ABS_RY),
                trig(ABS_HAT2X),
                trig(ABS_HAT2Y),
                pad(ABS_HAT0X),
                pad(ABS_HAT0Y),
                pad(ABS_HAT1X),
                pad(ABS_HAT1Y),
                AbsSpec::new(ABS_HAT3X, 0, 32767).res(1),
                AbsSpec::new(ABS_HAT3Y, 0, 32767).res(1),
            ];
        }
    }
    s
}

/// Accel: 16384 units per g. Gyro: 16 units per °/s (raw is 16.384, rescaled on output).
const ACCEL_RES: i32 = 16384;
const GYRO_RES: i32 = 16;

fn motion_spec(p: Profile, name: &str, phys: &str) -> DeviceSpec {
    let id = identity(p);
    let a = |c| AbsSpec::new(c, -32768, 32767).ff(16, 0).res(ACCEL_RES);
    let g = |c| AbsSpec::new(c, -32768, 32767).res(GYRO_RES);
    DeviceSpec {
        name: format!("{name} Motion Sensors"),
        bustype: id.bustype,
        vendor: id.vendor,
        product: id.product,
        version: id.version,
        phys: Some(phys.to_string()),
        abs: vec![a(ABS_X), a(ABS_Y), a(ABS_Z), g(ABS_RX), g(ABS_RY), g(ABS_RZ)],
        msc: vec![MSC_TIMESTAMP],
        props: vec![INPUT_PROP_ACCELEROMETER],
        ..Default::default()
    }
}

pub struct Gamepad {
    dev: VirtualDevice,
    motion: Option<VirtualDevice>,
    profile: Profile,
    remap: Vec<(u32, u32)>,
    pub ff: FfEngine,
    imu_last: Option<u32>,
    imu_ts_us: u64,
    rumble_strength: f32,
    rumble_swap: bool,
}

fn remap_table(m: &std::collections::BTreeMap<String, String>) -> Vec<(u32, u32)> {
    m.iter().filter_map(|(k, v)| Some((btn::from_name(k)?, btn::from_name(v).unwrap_or(0)))).collect()
}

fn neg(v: i16) -> i32 {
    (-(v as i32)).clamp(-32768, 32767)
}

impl Gamepad {
    pub fn create(cfg: &config::Gamepad, info: &HidInfo) -> io::Result<Self> {
        let phys = info.stable_id();
        let name = cfg.name.clone().unwrap_or_else(|| identity(cfg.profile).name.to_string());
        let dev = VirtualDevice::create(&pad_spec(cfg.profile, &name, &phys))?;
        let motion = if cfg.motion_enabled() {
            match VirtualDevice::create(&motion_spec(cfg.profile, &name, &phys)) {
                Ok(m) => Some(m),
                Err(e) => {
                    warn!("motion sensor device: {e}");
                    None
                }
            }
        } else {
            None
        };
        let remap = remap_table(&cfg.remap);
        debug!("created gamepad '{}' ({}) for slot {}", name, dev.sysname, info.slot_label());
        Ok(Self {
            dev,
            motion,
            profile: cfg.profile,
            remap,
            ff: FfEngine::default(),
            imu_last: None,
            imu_ts_us: 0,
            rumble_strength: cfg.rumble_strength.max(0.0),
            rumble_swap: cfg.rumble_swap,
        })
    }

    pub fn fd(&self) -> std::os::fd::RawFd {
        self.dev.fd()
    }

    pub fn sysname(&self) -> &str {
        &self.dev.sysname
    }

    fn remapped(&self, buttons: u32) -> u32 {
        if self.remap.is_empty() {
            return buttons;
        }
        let mask: u32 = self.remap.iter().map(|&(s, _)| s).fold(0, |a, b| a | b);
        let mut out = buttons & !mask;
        for &(src, dst) in &self.remap {
            if buttons & src != 0 {
                out |= dst;
            }
        }
        out
    }

    /// Push one controller state to the virtual pad (and motion device).
    pub fn update(&mut self, st: &State) -> io::Result<()> {
        let b = self.remapped(st.buttons);
        let on = |bit: u32| b & bit != 0;
        let d = &mut self.dev;

        d.key(BTN_SOUTH, on(btn::A));
        d.key(BTN_EAST, on(btn::B));
        d.key(BTN_X, on(btn::X));
        d.key(BTN_Y, on(btn::Y));
        d.key(BTN_TL, on(btn::LB));
        d.key(BTN_TR, on(btn::RB));
        // Left center button (view) = Back, right center button (menu) = Start.
        d.key(BTN_SELECT, on(btn::VIEW));
        d.key(BTN_START, on(btn::MENU));
        d.key(BTN_MODE, on(btn::STEAM));
        d.key(BTN_THUMBL, on(btn::L3));
        d.key(BTN_THUMBR, on(btn::R3));

        d.abs(ABS_X, st.lx as i32);
        d.abs(ABS_Y, neg(st.ly));
        d.abs(ABS_RX, st.rx as i32);
        d.abs(ABS_RY, neg(st.ry));
        let (lt, rt) = (st.lt.max(0) as i32, st.rt.max(0) as i32);

        let hx = on(btn::DPAD_RIGHT) as i32 - on(btn::DPAD_LEFT) as i32;
        let hy = on(btn::DPAD_DOWN) as i32 - on(btn::DPAD_UP) as i32;

        match self.profile {
            Profile::Xbox360 | Profile::XboxElite => {
                let shift = if self.profile == Profile::Xbox360 { 7 } else { 5 };
                d.abs(ABS_Z, lt >> shift);
                d.abs(ABS_RZ, rt >> shift);
                d.abs(ABS_HAT0X, hx);
                d.abs(ABS_HAT0Y, hy);
                if self.profile == Profile::XboxElite {
                    d.key(BTN_TRIGGER_HAPPY1, on(btn::QAM));
                    // xpad Elite order: P1 (upper right), P2 (lower right), P3 (upper left), P4 (lower left)
                    d.key(BTN_TRIGGER_HAPPY1 + 4, on(btn::R4));
                    d.key(BTN_TRIGGER_HAPPY1 + 5, on(btn::R5));
                    d.key(BTN_TRIGGER_HAPPY1 + 6, on(btn::L4));
                    d.key(BTN_TRIGGER_HAPPY1 + 7, on(btn::L5));
                }
            }
            Profile::Extended => {
                d.abs(ABS_HAT2Y, lt);
                d.abs(ABS_HAT2X, rt);
                d.key(BTN_TL2, on(btn::LT_CLICK));
                d.key(BTN_TR2, on(btn::RT_CLICK));
                d.key(BTN_DPAD_UP, on(btn::DPAD_UP));
                d.key(BTN_DPAD_DOWN, on(btn::DPAD_DOWN));
                d.key(BTN_DPAD_LEFT, on(btn::DPAD_LEFT));
                d.key(BTN_DPAD_RIGHT, on(btn::DPAD_RIGHT));
                d.key(BTN_GRIPL, on(btn::L4));
                d.key(BTN_GRIPR, on(btn::R4));
                d.key(BTN_GRIPL2, on(btn::L5));
                d.key(BTN_GRIPR2, on(btn::R5));
                d.key(BTN_BASE, on(btn::QAM));
                d.key(BTN_THUMB, on(btn::LPAD_CLICK));
                d.key(BTN_THUMB2, on(btn::RPAD_CLICK));
                let touches = [
                    btn::LSTICK_TOUCH,
                    btn::RSTICK_TOUCH,
                    btn::LPAD_TOUCH,
                    btn::RPAD_TOUCH,
                    btn::LGRIP_TOUCH,
                    btn::RGRIP_TOUCH,
                ];
                for (i, t) in touches.iter().enumerate() {
                    d.key(BTN_TRIGGER_HAPPY1 + i as u16, on(*t));
                }
                // Trackpads: report the last touched position while touched, center otherwise.
                let (lx, ly) = if on(btn::LPAD_TOUCH) { (st.lpad_x as i32, neg(st.lpad_y)) } else { (0, 0) };
                let (rx, ry) = if on(btn::RPAD_TOUCH) { (st.rpad_x as i32, neg(st.rpad_y)) } else { (0, 0) };
                d.abs(ABS_HAT0X, lx);
                d.abs(ABS_HAT0Y, ly);
                d.abs(ABS_HAT1X, rx);
                d.abs(ABS_HAT1Y, ry);
                d.abs(ABS_HAT3X, (st.lpad_pressure as i32).min(32767));
                d.abs(ABS_HAT3Y, (st.rpad_pressure as i32).min(32767));
            }
        }
        d.sync()?;

        if let Some(m) = &mut self.motion
            && st.imu_tick_us != 0
            && self.imu_last != Some(st.imu_tick)
        {
            if let Some(last) = self.imu_last {
                let modulus = if st.imu_tick_bits >= 32 { u64::from(u32::MAX) + 1 } else { 1u64 << st.imu_tick_bits };
                let delta = (st.imu_tick as u64 + modulus - last as u64) % modulus;
                self.imu_ts_us += delta * st.imu_tick_us as u64;
            }
            self.imu_last = Some(st.imu_tick);
            // Same axis convention as SDL's Triton driver: (X, Z, -Y).
            let gyro = |v: i16| (v as i32 * 1000 / 1024).clamp(-32768, 32767);
            m.push_raw(EV_MSC, MSC_TIMESTAMP, self.imu_ts_us as u32 as i32);
            m.abs(ABS_X, st.accel[0] as i32);
            m.abs(ABS_Y, st.accel[2] as i32);
            m.abs(ABS_Z, neg(st.accel[1]));
            m.abs(ABS_RX, gyro(st.gyro[0]));
            m.abs(ABS_RY, gyro(st.gyro[2]));
            m.abs(ABS_RZ, -gyro(st.gyro[1]));
            m.sync()?;
        }
        Ok(())
    }

    /// Release every button and center every axis.
    pub fn neutral(&mut self) -> io::Result<()> {
        let st = State::default();
        let motion = self.motion.take();
        let r = self.update(&st);
        self.motion = motion;
        r
    }

    /// Process requests from clients (FF uploads/erases, play/stop, gain).
    pub fn handle_client(&mut self, now: Instant) -> io::Result<()> {
        for e in self.dev.read_events()? {
            match (e.kind, e.code) {
                (EV_UINPUT, UI_FF_UPLOAD) => {
                    let mut up = self.dev.begin_ff_upload(e.value as u32)?;
                    up.retval = self.ff.upload(&up.effect, now);
                    debug!(
                        "ff upload id={} type={:#x} len={}ms -> {}",
                        up.effect.id, up.effect.kind, up.effect.replay_length, up.retval
                    );
                    self.dev.end_ff_upload(up)?;
                }
                (EV_UINPUT, UI_FF_ERASE) => {
                    let mut er = self.dev.begin_ff_erase(e.value as u32)?;
                    self.ff.erase(er.effect_id as i16);
                    er.retval = 0;
                    self.dev.end_ff_erase(er)?;
                }
                (EV_FF, code) => self.ff.handle_ff_event(code, e.value, now),
                _ => {}
            }
        }
        Ok(())
    }

    pub fn set_remap(&mut self, m: &std::collections::BTreeMap<String, String>) {
        self.remap = remap_table(m);
    }

    pub fn set_rumble(&mut self, strength: f32, swap: bool) {
        self.rumble_strength = strength.max(0.0);
        self.rumble_swap = swap;
    }

    /// Current rumble motor levels (left/strong, right/weak) after strength and swap.
    pub fn rumble(&mut self, now: Instant) -> (u16, u16) {
        let (s, w) = self.ff.mix(now);
        let k = self.rumble_strength;
        let sc = |v: u16| ((v as f32 * k).round() as u32).min(0xffff) as u16;
        if self.rumble_swap { (sc(w), sc(s)) } else { (sc(s), sc(w)) }
    }
}

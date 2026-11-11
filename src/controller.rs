//! One controller slot (a dongle interface or a direct USB/BLE controller).

use std::io;
use std::time::{Duration, Instant};

use log::{debug, info, warn};

use crate::config::{Config, Mode, Profile};
use crate::desktop::{DesktopDevices, DesktopState, Feedback};
use crate::gamepad::Gamepad;
use crate::hid::{HidDevice, HidInfo};
use crate::osk::{self, Overlay};
use crate::passthrough::{self, Chord};
use crate::proto::{self, HapticCmd, Input, Side, State, btn};
use crate::status;

const RUMBLE_RESEND: Duration = Duration::from_millis(40);
const TAP_HOLD: Duration = Duration::from_millis(60);
const MIN_TICK_GAP: Duration = Duration::from_millis(25);
const GLIDE_STEP: Duration = Duration::from_millis(8);
/// A disconnect this soon after we asked the controller to turn off is the expected one.
const POWER_OFF_GRACE: Duration = Duration::from_secs(10);

pub enum SlotError {
    /// The hidraw node is gone (unplugged); drop the slot.
    Gone(io::Error),
}

#[derive(Default)]
struct SwitchButton {
    down_since: Option<Instant>,
    fired: bool,
    tap_until: Option<Instant>,
}

pub struct Slot {
    pub info: HidInfo,
    hid: Option<HidDevice>,
    connected: bool,
    pub mode: Mode,
    pad: Option<Gamepad>,
    desk: DesktopState,
    switch_bit: u32,
    switch: SwitchButton,
    last_keepalive: Option<Instant>,
    rumble_sent: (u16, u16),
    last_rumble: Option<Instant>,
    /// Last tick per pad (index = Side value), so both pads can tick independently.
    last_tick: [Option<Instant>; 4],
    battery: Option<(u8, u8)>,
    backed_off: bool,
    last_state: Option<State>,
    /// Raw buttons of the previous report (edge detection for the overlay button).
    prev_buttons: u32,
    /// Buttons held when the preset popup was answered: ignored until released, so the
    /// answering press doesn't also reach the overlay or a game.
    suppress: u32,
    /// Set when the mode changed; the daemon shows a banner and clears it.
    pub mode_switched: bool,
    /// Connect / disconnect banner for the daemon to show.
    pub toast: Option<osk::Toast>,
    /// The connected controller's own serial number (names are keyed by it).
    serial: Option<String>,
    /// Display name (from `[names]`, else "Controller <slot>").
    name: String,
    /// Gamepad settings the current pad device was built with.
    pad_key: String,
    /// A "connected" banner was shown (and no "disconnected" since). Survives a
    /// back-off, so Steam taking the controller over and handing it back stays quiet.
    announced: bool,
    /// When we asked the controller to turn off (its disconnect is then expected).
    powered_off: Option<Instant>,
    /// Pass-through clone for Steam and games (exclusive mode only).
    clone: Option<passthrough::Clone>,
    /// Who uses the pass-through clone; None: nobody.
    client: Option<Client>,
    /// The user kept the controller in puckd with the hand-over chord: no clone.
    held: bool,
    /// ... from this process (Steam): when it exits, the clone comes back for the next one.
    /// None: until the chord again.
    held_from: Option<u32>,
    chord: Chord,
}

/// A program using the pass-through clone.
struct Client {
    name: String,
    pid: u32,
    /// Steam: Steam Input does what puckd does (modes, menus, mouse), so puckd steps aside
    /// completely until the hand-over chord. Other programs (SDL games) share: they get
    /// the native controller in gamepad mode, while puckd keeps its buttons, desktop mode
    /// and overlay.
    takes_over: bool,
}

/// The hand-over chord's buttons from the config ("view+menu").
pub fn chord_bits(cfg: &Config) -> u32 {
    cfg.general.handover_buttons.split('+').filter_map(|n| btn::from_name(n.trim())).fold(0, |a, b| a | b)
}

/// "View + ≡ Menu"
fn chord_label(cfg: &Config) -> String {
    let names: Vec<String> =
        cfg.general.handover_buttons.split('+').map(|n| osk::menu::button_label(n.trim())).collect();
    names.join(" + ")
}

/// The parts of the gamepad config that need a new virtual device when they change.
fn pad_key(cfg: &Config) -> String {
    let g = &cfg.gamepad;
    format!("{:?}|{:?}|{:?}", g.profile, g.motion, g.name)
}

impl Slot {
    pub fn open(info: HidInfo, cfg: &Config) -> io::Result<Self> {
        let hid = HidDevice::open(&info)?;
        Ok(Self {
            info,
            hid: Some(hid),
            connected: false,
            mode: cfg.general.start_mode,
            pad: None,
            desk: DesktopState::new(&cfg.desktop),
            switch_bit: btn::from_name(&cfg.general.switch_button).unwrap_or(btn::MENU),
            switch: SwitchButton::default(),
            last_keepalive: None,
            rumble_sent: (0, 0),
            last_rumble: None,
            last_tick: [None; 4],
            battery: None,
            backed_off: false,
            last_state: None,
            prev_buttons: 0,
            suppress: 0,
            serial: None,
            name: String::new(),
            mode_switched: false,
            toast: None,
            pad_key: pad_key(cfg),
            announced: false,
            powered_off: None,
            clone: None,
            client: None,
            held: false,
            held_from: None,
            chord: Chord::new(chord_bits(cfg)),
        })
    }

    pub fn label(&self) -> String {
        format!("slot {} ({})", self.info.slot_label(), self.info.node)
    }

    pub fn hid_fd(&self) -> Option<i32> {
        self.hid.as_ref().map(|h| h.fd())
    }

    pub fn pad_fd(&self) -> Option<i32> {
        self.pad.as_ref().map(|p| p.fd())
    }

    fn write_status(&self) {
        status::write(
            &self.info,
            &status::SlotStatus {
                connected: self.connected,
                mode: self.mode,
                battery: self.battery,
                backed_off: self.backed_off || self.steam_has_it(),
                name: &self.display_name(),
                serial: self.serial.as_deref(),
            },
        );
    }

    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    /// The name given to this controller, or "Controller <slot>".
    pub fn display_name(&self) -> String {
        if self.name.is_empty() { format!("Controller {}", self.info.slot_label()) } else { self.name.clone() }
    }

    /// Re-read the controller's name from the config.
    pub fn update_name(&mut self, cfg: &Config) {
        let n = self.serial.as_ref().and_then(|s| cfg.names.get(s)).cloned().unwrap_or_default();
        if n != self.name {
            self.name = n;
            self.write_status();
        }
    }

    /// Apply a changed config: rebuild bindings, and recreate the pad device only if its
    /// identity changed (games see that as an unplug/replug).
    pub fn apply_config(&mut self, cfg: &Config, desk: &mut DesktopDevices) {
        self.update_name(cfg);
        self.desk.release_all(desk);
        self.desk = DesktopState::new(&cfg.desktop);
        self.switch_bit = btn::from_name(&cfg.general.switch_button).unwrap_or(btn::MENU);
        self.switch = SwitchButton::default();
        let key = pad_key(cfg);
        if key != self.pad_key {
            self.pad_key = key;
            if self.pad.take().is_some() && self.connected {
                self.create_pad(cfg);
            }
        } else if let Some(p) = &mut self.pad {
            let g = &cfg.gamepad;
            p.set_rumble(g.rumble_strength, g.rumble_swap);
            p.set_remap(&g.remap);
        }
        self.last_keepalive = None;
        self.chord = Chord::new(chord_bits(cfg));
        self.sync_clone(cfg);
    }

    /// Ask the controller to turn itself off.
    pub fn power_off(&mut self) -> io::Result<()> {
        info!("{}: turning the controller off", self.label());
        let Some(hid) = &mut self.hid else { return Err(io::Error::other("not connected")) };
        hid.set_feature(&proto::power_off_report())?;
        self.powered_off = Some(Instant::now());
        Ok(())
    }

    /// Connected and driven by us (not backed off, not taken over by Steam).
    pub fn is_active(&self) -> bool {
        self.connected && self.hid.is_some() && !self.steam_has_it()
    }

    /// The game sharing the native controller with puckd, if any (not Steam).
    pub fn native_client(&self) -> Option<&str> {
        self.client.as_ref().filter(|c| !c.takes_over).map(|c| c.name.as_str())
    }

    /// Something worth watching for: a controller connected, or puckd stepped aside or
    /// lent the controller to Steam or a game. Otherwise the daemon can sleep.
    pub fn busy(&self) -> bool {
        self.connected || self.backed_off || self.client.is_some()
    }

    /// Steam took the controller over (puckd stays out of the way).
    pub fn steam_has_it(&self) -> bool {
        self.client.as_ref().is_some_and(|c| c.takes_over)
    }

    /// puckd has the hardware to itself (through `puckd broker`), so it can offer the
    /// pass-through clone.
    fn exclusive(&self) -> bool {
        self.hid.as_ref().is_some_and(|h| h.exclusive)
    }

    pub fn clone_fd(&self) -> Option<i32> {
        self.clone.as_ref().map(|c| c.fd())
    }

    /// Create or remove the pass-through clone as the state asks for.
    fn sync_clone(&mut self, cfg: &Config) {
        let want = self.connected && self.exclusive() && cfg.general.passthrough && !self.held;
        match (want, self.clone.is_some()) {
            (true, false) => match self.hid.as_ref().map(|h| passthrough::Clone::create(h, self.serial.as_deref())) {
                Some(Ok(c)) => {
                    info!("{}: pass-through device {} for Steam and games", self.label(), c.phys);
                    self.clone = Some(c);
                }
                Some(Err(e)) => warn!("{}: cannot create the pass-through device: {e}", self.label()),
                None => {}
            },
            (false, true) => {
                self.clone = None;
                if self.client.is_some() {
                    self.release(cfg);
                }
            }
            _ => {}
        }
    }

    /// A program started using the clone. Steam takes over; others share.
    fn attach(&mut self, who: passthrough::Holder, cfg: &Config, desk: &mut DesktopDevices) {
        let passthrough::Holder { name, steam: takes_over, pid } = who;
        // Its native controller replaces our pad (else it sees two).
        if let Some(p) = &mut self.pad {
            let _ = p.neutral();
        }
        self.pad = None;
        self.rumble_sent = (0, 0);
        if takes_over {
            info!("{}: {name} took over the controller", self.label());
            self.desk.release_all(desk);
            self.switch = SwitchButton::default();
            self.toast = Some(osk::Toast {
                title: format!("{name} has {}", self.which()),
                line: osk::ToastLine::Text(format!("Hold {} for puckd", chord_label(cfg))),
                secs: osk::Toast::LONG,
            });
        } else {
            info!("{}: {name} uses the controller natively (shared with puckd)", self.label());
            self.toast = Some(osk::Toast {
                title: format!("{name} uses {}", self.which()),
                line: osk::ToastLine::Text("Native controller in gamepad mode".into()),
                secs: osk::Toast::NORMAL,
            });
        }
        self.client = Some(Client { name, pid, takes_over });
        self.write_status();
    }

    /// The client is gone: undo what it may have changed, puckd drives the controller again.
    fn release(&mut self, cfg: &Config) {
        self.client = None;
        // Steam changes settings (trackpad and IMU modes, ...) and clears the lizard-mode
        // mappings: back to the firmware defaults, then puckd's own settings right away.
        if let Some(hid) = &mut self.hid
            && self.connected
        {
            for r in [proto::load_default_settings_report(), proto::default_digital_mappings_report()] {
                if let Err(e) = hid.set_feature(&r) {
                    debug!("{}: resetting settings ({:#04x}): {e}", self.info.node, r[1]);
                }
            }
        }
        self.last_keepalive = None;
        self.create_pad(cfg);
        self.write_status();
    }

    /// "the controller", or its name when it has one.
    fn which(&self) -> String {
        if self.name.is_empty() { "the controller".into() } else { self.name.clone() }
    }

    /// The hand-over chord: puckd ↔ Steam (and games).
    fn handover(&mut self, cfg: &Config) {
        if self.held {
            self.held = false;
            self.held_from = None;
            self.sync_clone(cfg);
            info!("{}: controller offered to Steam and games again", self.label());
            self.toast = Some(osk::Toast {
                title: "Steam and games may take it".into(),
                line: osk::ToastLine::Text(format!("Hold {} to keep it in puckd", chord_label(cfg))),
                secs: osk::Toast::LONG,
            });
        } else {
            // Cut off from the current user of the clone only: when that process exits, the
            // clone comes back (else a restarted Steam would never get the controller).
            let from = self.client.as_ref().map(|c| (c.pid, c.name.clone()));
            self.held = true;
            self.held_from = from.as_ref().map(|f| f.0);
            self.sync_clone(cfg);
            info!(
                "{}: controller kept in puckd{}",
                self.label(),
                match &from {
                    Some((pid, name)) => format!(" while {name} (pid {pid}) runs"),
                    None => String::new(),
                }
            );
            let line = match from {
                Some((_, name)) => format!("Until {name} restarts, or hold {}", chord_label(cfg)),
                None => format!("Hold {} to hand it to Steam", chord_label(cfg)),
            };
            self.toast = Some(osk::Toast {
                title: format!("puckd has {}", self.which()),
                line: osk::ToastLine::Text(line),
                secs: osk::Toast::LONG,
            });
        }
    }

    /// Requests from the clone's users (Steam, games).
    pub fn on_clone_readable(&mut self, cfg: &Config, desk: &mut DesktopDevices) {
        let (Some(c), Some(h)) = (&mut self.clone, &mut self.hid) else { return };
        if let Err(e) = c.serve(h) {
            warn!("{}: pass-through device: {e}", self.info.node);
            self.clone = None;
            return;
        }
        if c.take_activity() && self.client.is_none() {
            let who =
                c.holder().unwrap_or_else(|| passthrough::Holder { name: "A program".into(), steam: false, pid: 0 });
            self.attach(who, cfg, desk);
        }
    }

    /// Periodic: notice when the clone's user let go, and keep the clone in sync.
    pub fn check_passthrough(&mut self, cfg: &Config, desk: &mut DesktopDevices) {
        // The program the controller was taken back from has exited: offer it again.
        if let Some(pid) = self.held_from
            && !std::path::Path::new(&format!("/proc/{pid}")).exists()
        {
            info!("{}: pid {pid} exited, offering the controller to Steam and games again", self.label());
            self.held = false;
            self.held_from = None;
        }
        if let (Some(client), Some(c)) = (&self.client, &self.clone) {
            match c.holder() {
                None => {
                    let (name, took) = (client.name.clone(), client.takes_over);
                    info!("{}: {name} let go of the controller", self.label());
                    if took {
                        self.toast = Some(osk::Toast {
                            title: format!("puckd has {}", self.which()),
                            line: osk::ToastLine::Text(format!("{name} let go")),
                            secs: osk::Toast::NORMAL,
                        });
                    }
                    self.release(cfg);
                }
                // Steam joined a game that shares the controller: Steam takes over.
                Some(h) if h.steam && !client.takes_over => self.attach(h, cfg, desk),
                Some(_) => {}
            }
        }
        self.sync_clone(cfg);
    }

    /// What the overlay header shows about this controller.
    pub fn battery(&self) -> Option<(u8, u8)> {
        self.battery
    }

    pub fn set_mode(&mut self, mode: Mode, cfg: &Config, desk: &mut DesktopDevices) {
        if self.mode != mode {
            self.toggle_mode(cfg, desk, Instant::now());
        }
    }

    /// Haptic feedback requested by the overlay.
    pub fn overlay_haptic(&mut self, click: bool, side: Side, cfg: &Config) {
        if !cfg.haptics.enabled {
            return;
        }
        let now = Instant::now();
        let i = side as usize;
        if !click && self.last_tick[i].is_some_and(|t| now.duration_since(t) < MIN_TICK_GAP) {
            return;
        }
        self.last_tick[i] = Some(now);
        let cmd = if click { HapticCmd::Click } else { HapticCmd::Tick };
        self.haptic(&proto::haptic_command_report(side, cmd, cfg.haptics.gain_db));
    }

    fn toggle_osk(&mut self, osk: &mut Overlay, cfg: &Config, desk: &mut DesktopDevices) {
        if osk.is_open_for(&self.info.node) {
            osk.close();
            return;
        }
        // Inputs go to the overlay now: release what the current mode holds.
        match self.mode {
            Mode::Gamepad => {
                if let Some(p) = &mut self.pad {
                    let _ = p.neutral();
                }
            }
            Mode::Desktop => self.desk.release_all(desk),
        }
        let _ = cfg;
        osk.open(&self.info.node, None);
    }

    fn create_pad(&mut self, cfg: &Config) {
        let g = &cfg.gamepad;
        match Gamepad::create(g, &self.info) {
            Ok(p) => {
                info!("{}: gamepad {} ({:?})", self.label(), p.sysname(), g.profile);
                self.pad = Some(p);
            }
            Err(e) => warn!("{}: cannot create gamepad: {e}", self.label()),
        }
    }

    fn send_settings(&mut self, cfg: &Config) {
        let Some(hid) = &mut self.hid else { return };
        let mut settings = vec![(proto::SETTING_LIZARD_MODE, proto::LIZARD_MODE_OFF)];
        if cfg.gamepad.motion_enabled() || cfg.desktop.gyro_mouse {
            settings.push((proto::SETTING_IMU_MODE, proto::IMU_MODE_RAW_ACCEL_GYRO));
        }
        if let Err(e) = hid.set_feature(&proto::settings_report(&settings)) {
            debug!("{}: keepalive failed: {e}", self.info.node);
        }
    }

    fn send_lizard_off(&mut self) {
        let Some(hid) = &mut self.hid else { return };
        if let Err(e) =
            hid.set_feature(&proto::settings_report(&[(proto::SETTING_LIZARD_MODE, proto::LIZARD_MODE_OFF)]))
        {
            debug!("{}: keepalive failed: {e}", self.info.node);
        }
    }

    /// Give lizard mode back to the controller (on exit).
    pub fn restore_lizard(&mut self) {
        if let (Some(hid), true, false, None) = (&mut self.hid, self.connected, self.backed_off, &self.client) {
            let _ = hid.write_output(&proto::rumble_report(0, 0));
            let _ = hid.set_feature(&proto::settings_report(&[(proto::SETTING_LIZARD_MODE, proto::LIZARD_MODE_ON)]));
        }
    }

    fn haptic(&mut self, report: &[u8]) {
        if let Some(hid) = &mut self.hid
            && let Err(e) = hid.write_output(report)
        {
            debug!("{}: haptic write failed: {e}", self.info.node);
        }
    }

    fn feedback(&mut self, f: Feedback, cfg: &Config, now: Instant) {
        let h = &cfg.haptics;
        if !h.enabled {
            return;
        }
        let (side, cmd) = match f {
            Feedback::PointerTick => (Side::Right, HapticCmd::Tick),
            Feedback::ScrollTick => (Side::Left, HapticCmd::Tick),
            Feedback::Click(side) => (side, HapticCmd::Click),
            Feedback::Release(side) => (side, HapticCmd::Tick),
        };
        let i = side as usize;
        if matches!(cmd, HapticCmd::Tick) && self.last_tick[i].is_some_and(|t| now.duration_since(t) < MIN_TICK_GAP) {
            return;
        }
        self.last_tick[i] = Some(now);
        let r = proto::haptic_command_report(side, cmd, h.gain_db);
        self.haptic(&r);
    }

    fn connect(&mut self, cfg: &Config, now: Instant) {
        if self.connected {
            return;
        }
        self.connected = true;
        self.serial = self.hid.as_mut().and_then(|h| h.unit_serial());
        self.name = self.serial.as_ref().and_then(|s| cfg.names.get(s)).cloned().unwrap_or_default();
        info!(
            "{}: controller {} connected ({:?} mode)",
            self.label(),
            match (&self.serial, self.name.is_empty()) {
                (Some(s), true) => s.clone(),
                (Some(s), false) => format!("\"{}\" ({s})", self.name),
                (None, _) => "(serial unknown)".into(),
            },
            self.mode
        );
        self.send_settings(cfg);
        self.last_keepalive = Some(now);
        self.create_pad(cfg);
        self.write_status();
        self.sync_clone(cfg);
        self.powered_off = None;
        if !self.announced {
            self.announced = true;
            if cfg.general.notify {
                let mode = match self.mode {
                    Mode::Desktop => "Desktop mode",
                    Mode::Gamepad => "Gamepad mode",
                };
                self.toast = Some(osk::Toast {
                    title: format!("{} connected", self.display_name()),
                    line: osk::ToastLine::Text(mode.into()),
                    secs: osk::Toast::NORMAL,
                });
            }
        }
    }

    /// The controller went away (turned off, out of range, unplugged): show a banner, unless
    /// we turned it off ourselves.
    fn lost(&mut self, cfg: &Config, desk: &mut DesktopDevices) {
        let name = self.display_name();
        self.disconnect(desk);
        if !std::mem::take(&mut self.announced) {
            return;
        }
        let expected = self.powered_off.take().is_some_and(|t| t.elapsed() < POWER_OFF_GRACE);
        if cfg.general.notify && !expected {
            let line = match self.battery {
                Some((_, level)) => format!("Battery {level} %"),
                None => String::new(),
            };
            self.toast = Some(osk::Toast {
                title: format!("{name} disconnected"),
                line: osk::ToastLine::Text(line),
                secs: osk::Toast::NORMAL,
            });
        }
    }

    fn disconnect(&mut self, desk: &mut DesktopDevices) {
        if !self.connected {
            return;
        }
        self.connected = false;
        info!("{}: controller disconnected", self.label());
        self.pad = None;
        self.clone = None;
        self.client = None;
        self.desk.release_all(desk);
        self.switch = SwitchButton::default();
        self.rumble_sent = (0, 0);
        self.last_state = None;
        self.serial = None;
        self.name.clear();
        self.write_status();
    }

    /// Stop driving the controller because another program (Steam) has it open.
    pub fn back_off(&mut self, who: &str, desk: &mut DesktopDevices) {
        if self.backed_off {
            return;
        }
        info!("{}: {who} opened the controller, backing off", self.label());
        self.disconnect(desk);
        self.hid = None;
        self.backed_off = true;
        self.write_status();
    }

    /// Release outputs and close the device (slot removed).
    pub fn close(&mut self, cfg: &Config, desk: &mut DesktopDevices) {
        self.lost(cfg, desk);
        self.hid = None;
    }

    pub fn reclaim(&mut self) {
        if !self.backed_off {
            return;
        }
        match HidDevice::open(&self.info) {
            Ok(h) => {
                info!("{}: controller is free again, reclaiming", self.label());
                self.hid = Some(h);
                self.backed_off = false;
                self.last_keepalive = None;
                self.write_status();
            }
            Err(e) => debug!("{}: reclaim failed: {e}", self.label()),
        }
    }

    fn toggle_mode(&mut self, cfg: &Config, desk: &mut DesktopDevices, now: Instant) {
        match self.mode {
            Mode::Gamepad => {
                if let Some(p) = &mut self.pad {
                    let _ = p.neutral();
                }
            }
            Mode::Desktop => {}
        }
        // Gamepad mode may use the mouse too (trackpads), so release it either way.
        self.desk.release_all(desk);
        self.mode = self.mode.toggled();
        self.mode_switched = true;
        info!("{}: {:?} mode", self.label(), self.mode);
        if cfg.haptics.enabled && cfg.haptics.mode_switch {
            self.haptic(&proto::haptic_command_report(Side::Both, HapticCmd::Click, cfg.haptics.gain_db));
            self.last_tick = [Some(now); 4];
        }
        self.write_status();
    }

    /// Long-press detection for the mode switch button. Returns the buttons to forward.
    fn filter_switch(&mut self, buttons: u32, now: Instant, cfg: &Config, desk: &mut DesktopDevices) -> u32 {
        let bit = self.switch_bit;
        let hold = Duration::from_millis(cfg.general.switch_hold_ms);
        if buttons & bit != 0 {
            match self.switch.down_since {
                None => {
                    self.switch.down_since = Some(now);
                    self.switch.fired = false;
                }
                Some(t) if !self.switch.fired && now.duration_since(t) >= hold => {
                    self.switch.fired = true;
                    self.toggle_mode(cfg, desk, now);
                }
                _ => {}
            }
        } else if self.switch.down_since.take().is_some() && !self.switch.fired {
            // Short press: pass it through as a tap.
            self.switch.tap_until = Some(now + TAP_HOLD);
        }
        let tapping = self.switch.tap_until.is_some_and(|t| now < t);
        if !tapping {
            self.switch.tap_until = None;
        }
        (buttons & !bit) | if tapping { bit } else { 0 }
    }

    /// Send a state where it belongs (overlay, desktop mode, gamepad). Returns the buttons
    /// the game gets in gamepad mode, None when the game should see a neutral controller.
    fn route(
        &mut self,
        st: &State,
        now: Instant,
        cfg: &Config,
        desk: &mut DesktopDevices,
        osk: &mut Overlay,
    ) -> Option<u32> {
        let mut s = *st;
        s.buttons = self.filter_switch(st.buttons, now, cfg, desk);
        let osk_bit = btn::from_name(&cfg.general.osk_button).unwrap_or(0);
        let pressed = st.buttons & !self.prev_buttons;
        self.prev_buttons = st.buttons;
        s.buttons &= !osk_bit;
        if pressed & osk_bit != 0 {
            self.toggle_osk(osk, cfg, desk);
        }
        // A pending "switch preset?" popup comes first: A (this session) / X (once) / B (no),
        // from any controller. An open overlay gets no input until it is answered; games only
        // lose A, X and B.
        self.suppress &= st.buttons;
        let prompt_keys = btn::A | btn::X | btn::B;
        if osk.prompt_active() {
            if pressed & prompt_keys != 0 {
                let a = if pressed & btn::A != 0 {
                    osk::Answer::Session
                } else if pressed & btn::X != 0 {
                    osk::Answer::Once
                } else {
                    osk::Answer::No
                };
                osk.answer(a);
                self.suppress = st.buttons;
            }
            if osk.is_open_for(&self.info.node) {
                // Keep the overlay's view neutral (nothing held) while it waits.
                osk.input(&State::default(), &State::default());
                self.last_state = Some(*st);
                return None;
            }
            s.buttons &= !prompt_keys;
        }
        s.buttons &= !self.suppress;
        if osk.is_open_for(&self.info.node) {
            let mut raw = *st;
            raw.buttons &= !self.suppress;
            osk.input(&s, &raw);
            self.last_state = Some(*st);
            return None;
        }
        let to_game = match self.mode {
            Mode::Gamepad => {
                if let Some(p) = &mut self.pad
                    && let Err(e) = p.update(&s)
                {
                    warn!("{}: gamepad write failed: {e}", self.label());
                }
                // Xbox profiles have no trackpads: let them drive the mouse instead (not when
                // a game has the native controller, trackpads included).
                if cfg.gamepad.trackpad_mouse && cfg.gamepad.profile != Profile::Extended && self.client.is_none() {
                    let mut clicks = btn::RPAD_CLICK | btn::LPAD_CLICK;
                    for k in cfg.gamepad.remap.keys() {
                        clicks &= !btn::from_name(k).unwrap_or(0);
                    }
                    let out = self.desk.update(&s, now, &cfg.desktop, &cfg.haptics, desk, Some(clicks));
                    for f in out.feedback {
                        self.feedback(f, cfg, now);
                    }
                }
                Some(s.buttons)
            }
            Mode::Desktop => {
                let out = self.desk.update(&s, now, &cfg.desktop, &cfg.haptics, desk, None);
                for f in out.feedback {
                    self.feedback(f, cfg, now);
                }
                if out.toggle_mode {
                    self.toggle_mode(cfg, desk, now);
                }
                if out.toggle_osk {
                    self.toggle_osk(osk, cfg, desk);
                }
                None
            }
        };
        self.last_state = Some(*st);
        to_game
    }

    pub fn on_hid_readable(
        &mut self,
        now: Instant,
        cfg: &Config,
        desk: &mut DesktopDevices,
        osk: &mut Overlay,
    ) -> Result<(), SlotError> {
        let mut buf = [0u8; 64];
        loop {
            let Some(hid) = &mut self.hid else { return Ok(()) };
            let n = match hid.read_report(&mut buf) {
                Ok(Some(n)) => n,
                Ok(None) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(SlotError::Gone(e)),
            };
            let parsed = proto::parse(&buf[..n]);
            if let Some(Input::State(mut st)) = parsed {
                self.connect(cfg, now);
                // The hand-over chord is hidden from everyone else.
                let (buttons, fire) = if self.exclusive() && cfg.general.passthrough {
                    self.chord.apply(st.buttons, now)
                } else {
                    (st.buttons, false)
                };
                if fire {
                    self.handover(cfg);
                }
                if self.steam_has_it() {
                    if let Some(c) = &mut self.clone {
                        c.forward(&buf[..n], Some(buttons));
                    }
                    continue;
                }
                // puckd drives: the clone (a game sharing it) gets what a game should get.
                st.buttons = buttons;
                let to_game = self.route(&st, now, cfg, desk, osk);
                if let Some(c) = &mut self.clone {
                    match to_game {
                        Some(b) => c.forward(&buf[..n], Some(b)),
                        None => c.forward_neutral(&buf[..n]),
                    }
                }
                continue;
            }
            if let (Some(c), Some(id)) = (&mut self.clone, buf.first())
                && !matches!(*id, proto::ID_WIRELESS | proto::ID_WIRELESS_X)
            {
                c.forward(&buf[..n], None);
            }
            match parsed {
                Some(Input::State(_)) => {}
                Some(Input::Wireless(proto::WIRELESS_CONNECT)) => self.connect(cfg, now),
                Some(Input::Wireless(proto::WIRELESS_DISCONNECT)) => self.lost(cfg, desk),
                Some(Input::Battery { charge_state, level }) => {
                    if self.battery != Some((charge_state, level)) {
                        info!("{}: battery {level}% ({})", self.label(), proto::charge_state_name(charge_state));
                        self.battery = Some((charge_state, level));
                        self.write_status();
                    }
                }
                Some(Input::Wireless(other)) => debug!("{}: wireless status {other}", self.label()),
                Some(Input::Other(id)) => debug!("{}: report {id:#04x} ({n} bytes)", self.label()),
                None => debug!("{}: short report ({n} bytes)", self.label()),
            }
        }
    }

    pub fn on_pad_readable(&mut self, now: Instant) {
        if let Some(p) = &mut self.pad
            && let Err(e) = p.handle_client(now)
        {
            warn!("{}: gamepad client I/O: {e}", self.label());
        }
        self.update_rumble(now, true);
    }

    fn update_rumble(&mut self, now: Instant, changed_only: bool) {
        if self.hid.is_none() || !self.connected {
            return;
        }
        let level = match &mut self.pad {
            Some(p) => p.rumble(now),
            None => (0, 0),
        };
        let due = self.last_rumble.is_none_or(|t| now.duration_since(t) >= RUMBLE_RESEND);
        let send = level != self.rumble_sent || (level != (0, 0) && due && !changed_only);
        if send {
            log::trace!("{}: rumble {:#06x} {:#06x}", self.info.node, level.0, level.1);
            self.haptic(&proto::rumble_report(level.0, level.1));
            self.rumble_sent = level;
            self.last_rumble = Some(now);
        }
    }

    /// Timers: keepalive, rumble refresh, long-press, taps, inertia.
    pub fn tick(&mut self, now: Instant, cfg: &Config, desk: &mut DesktopDevices, osk: &mut Overlay) {
        if self.hid.is_none() || !self.connected {
            return;
        }
        let ka = Duration::from_millis(cfg.general.keepalive_ms.clamp(500, 2900));
        let keepalive_due = self.last_keepalive.is_none_or(|t| now.duration_since(t) >= ka);
        if self.steam_has_it() {
            // Steam drives everything else, but lizard mode stays off even when Steam stops
            // sending its keepalive while it shuts down (the firmware brings lizard mode
            // back after ~3 s without one, before we notice that Steam let go).
            if keepalive_due {
                self.send_lizard_off();
                self.last_keepalive = Some(now);
            }
            return;
        }
        if keepalive_due {
            self.send_settings(cfg);
            self.last_keepalive = Some(now);
        }
        self.update_rumble(now, false);

        // Let long-press and tap release fire even if reports pause.
        if let Some(st) = self.last_state {
            let hold_due = self.switch.down_since.is_some_and(|t| {
                !self.switch.fired && now.duration_since(t) >= Duration::from_millis(cfg.general.switch_hold_ms)
            });
            let tap_due = self.switch.tap_until.is_some_and(|t| now >= t);
            if hold_due || tap_due {
                self.route(&st, now, cfg, desk, osk);
            }
        }
        if self.mode == Mode::Desktop && self.desk.is_gliding() {
            let out = self.desk.glide(now, &cfg.desktop, desk);
            for f in out.feedback {
                self.feedback(f, cfg, now);
            }
        }
    }

    pub fn next_deadline(&self, now: Instant, cfg: &Config) -> Option<Instant> {
        if self.hid.is_none() || !self.connected {
            return None;
        }
        let mut d: Vec<Instant> = Vec::new();
        let ka = Duration::from_millis(cfg.general.keepalive_ms.clamp(500, 2900));
        d.push(self.last_keepalive.map_or(now, |t| t + ka));
        if self.steam_has_it() {
            return d.into_iter().min();
        }
        if self.rumble_sent != (0, 0) {
            d.push(self.last_rumble.map_or(now, |t| t + RUMBLE_RESEND));
        }
        if let Some(p) = &self.pad
            && let Some(t) = p.ff.next_change(now)
        {
            d.push(t);
        }
        if let Some(t) = self.switch.down_since
            && !self.switch.fired
        {
            d.push(t + Duration::from_millis(cfg.general.switch_hold_ms));
        }
        if let Some(t) = self.switch.tap_until {
            d.push(t);
        }
        if self.mode == Mode::Desktop && self.desk.is_gliding() {
            d.push(now + GLIDE_STEP);
        }
        d.into_iter().min()
    }
}

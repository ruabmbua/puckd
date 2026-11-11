//! The main poll loop: discovery/hotplug, dispatch, timers, back-off, reload.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};

use crate::backoff;
use crate::config::PresetSwitch;
use crate::config::{Backoff, Config};
use crate::controller::{Slot, SlotError};
use crate::desktop::DesktopDevices;
use crate::hid;
use crate::osk::{self, Overlay, menu};
use crate::presets::{self, Presets};
use crate::status;
use crate::sys;

const RESCAN: Duration = Duration::from_secs(2);
const MAX_SLEEP: Duration = Duration::from_millis(1000);

#[derive(Clone, Copy)]
enum Target {
    Hid,
    Pad,
    Clone,
    Osk,
}

pub struct Daemon {
    cfg: Config,
    cfg_path: PathBuf,
    slots: BTreeMap<String, Slot>,
    desk: DesktopDevices,
    last_scan: Option<Instant>,
    last_backoff: Option<Instant>,
    osk: Overlay,
    presets: Presets,
    /// Focused window (app_id, title), from the overlay thread.
    focused: Option<(String, String)>,
}

impl Daemon {
    pub fn new(cfg: Config, cfg_path: PathBuf) -> std::io::Result<Self> {
        let desk = DesktopDevices::create()?;
        let mut osk = Overlay::new()?;
        // Started right away: it also tracks the focused app for per-app presets.
        osk.keep_running();
        let presets = Presets::load(&presets::default_dir(), cfg.gamepad.clone());
        Ok(Self {
            cfg,
            cfg_path,
            slots: BTreeMap::new(),
            desk,
            last_scan: None,
            last_backoff: None,
            osk,
            presets,
            focused: None,
        })
    }

    fn rescan(&mut self) {
        let found = hid::scan();
        // Removed nodes.
        let gone: Vec<String> = self.slots.keys().filter(|k| !found.iter().any(|f| &f.node == *k)).cloned().collect();
        for k in gone {
            self.drop_slot(&k, "device removed");
        }
        for info in found {
            if self.slots.contains_key(&info.node) {
                continue;
            }
            match Slot::open(info.clone(), &self.cfg) {
                Ok(s) => {
                    info!("found {} [{:04x}:{:04x} {}]", s.label(), info.vid, info.pid, info.uniq);
                    self.slots.insert(info.node.clone(), s);
                }
                Err(e) => {
                    // Typically a permission problem; retry on the next scan.
                    debug!("cannot open {}: {e}", info.dev.display());
                    if e.kind() == std::io::ErrorKind::PermissionDenied {
                        warn!("{}: permission denied (install the udev rule, see README)", info.dev.display());
                    }
                }
            }
        }
    }

    fn drop_slot(&mut self, node: &str, why: &str) {
        if let Some(mut s) = self.slots.remove(node) {
            info!("{}: {why}", s.label());
            s.close(&self.cfg, &mut self.desk);
            status::remove(&s.info);
            if let Some(t) = s.toast.take() {
                self.osk.toast(t);
            }
        }
    }

    fn check_backoff(&mut self) {
        for s in self.slots.values_mut() {
            s.check_passthrough(&self.cfg, &mut self.desk);
        }
        let mode = self.cfg.general.backoff;
        if mode == Backoff::Never {
            for s in self.slots.values_mut() {
                s.reclaim();
            }
            return;
        }
        let devs: Vec<PathBuf> = self.slots.values().map(|s| s.info.dev.clone()).collect();
        let openers = backoff::openers(&devs);
        for s in self.slots.values_mut() {
            let others: Vec<_> = openers
                .get(&s.info.dev)
                .map(|v| v.iter().filter(|o| mode == Backoff::Any || o.is_steam()).cloned().collect())
                .unwrap_or_default();
            if let Some(o) = others.first() {
                s.back_off(&format!("{} (pid {})", o.comm, o.pid), &mut self.desk);
            } else {
                s.reclaim();
            }
        }
    }

    fn handle_osk_events(&mut self) {
        let owner = self.osk.owner().map(str::to_string);
        let mut keys = false;
        for e in self.osk.drain() {
            match e {
                osk::Event::Key { code, down } => {
                    self.desk.key(code, down);
                    keys = true;
                }
                osk::Event::Haptic { click, side } => {
                    if let Some(s) = owner.as_ref().and_then(|o| self.slots.get_mut(o)) {
                        s.overlay_haptic(click, side, &self.cfg);
                    }
                }
                osk::Event::Focus { app, title } => self.on_focus(app, title),
                osk::Event::Set { id, value } => match menu::apply(&mut self.cfg, &id, &value) {
                    menu::Effect::Config => {
                        debug!("overlay: {id} = {}", value.display());
                        self.config_changed();
                    }
                    menu::Effect::PresetNew(name) => {
                        let name = if name.trim().is_empty() { self.presets.unique_name("Preset") } else { name };
                        match self.presets.save(&name, &self.cfg.gamepad) {
                            Ok(n) => info!("saved preset \"{n}\""),
                            Err(e) => error!("saving preset failed: {e}"),
                        }
                    }
                    menu::Effect::PresetSave => {
                        if let Some(n) = self.presets.active.clone()
                            && let Err(e) = self.presets.save(&n, &self.cfg.gamepad)
                        {
                            error!("saving preset failed: {e}");
                        }
                    }
                    menu::Effect::PresetSaveDefault => {
                        let mut gp = self.cfg.gamepad.clone();
                        gp.remap = self.presets.default.remap.clone();
                        match menu::save_default_preset(&gp, &self.cfg_path) {
                            Ok(()) => self.presets.default = gp,
                            Err(e) => error!("saving the Default preset failed: {e}"),
                        }
                    }
                    menu::Effect::PresetDiscard => {
                        self.cfg.gamepad = self.presets.saved().clone();
                        self.config_changed();
                    }
                    menu::Effect::PresetRename(new) => {
                        if let Some(old) = self.presets.active.clone()
                            && let Err(e) = self.presets.rename(&old, &new)
                        {
                            error!("renaming preset failed: {e}");
                        }
                    }
                    menu::Effect::PresetLoad(n) => self.load_preset(n.as_deref(), true),
                    menu::Effect::PresetDelete => {
                        if let Some(n) = self.presets.active.clone() {
                            match self.presets.delete(&n) {
                                Ok(()) => info!("deleted preset \"{n}\" (keeping its settings as unsaved changes)"),
                                Err(e) => error!("deleting preset failed: {e}"),
                            }
                        }
                    }
                    menu::Effect::PowerOff => {
                        self.osk.close();
                        if let Some(s) = owner.as_ref().and_then(|o| self.slots.get_mut(o))
                            && let Err(e) = s.power_off()
                        {
                            error!("turning the controller off failed: {e}");
                        }
                    }
                    menu::Effect::ControllerName(name) => {
                        let serial =
                            owner.as_ref().and_then(|o| self.slots.get(o)).and_then(|s| s.serial()).map(str::to_string);
                        if let Some(serial) = serial {
                            let name = name.trim().to_string();
                            if name.is_empty() {
                                self.cfg.names.remove(&serial);
                            } else {
                                self.cfg.names.insert(serial.clone(), name.clone());
                            }
                            info!("controller {serial} is now called \"{name}\"");
                            if let Err(e) = menu::save_names(&self.cfg, &self.cfg_path) {
                                error!("saving the controller name failed: {e}");
                            }
                            for s in self.slots.values_mut() {
                                s.update_name(&self.cfg);
                            }
                        }
                    }
                    menu::Effect::PresetApp(app, on) => {
                        if let Some(n) = self.presets.active.clone()
                            && let Err(e) = self.presets.set_app(&app, &n, on)
                        {
                            error!("saving app presets failed: {e}");
                        }
                    }
                    menu::Effect::Mode(m) => {
                        if let Some(s) = owner.as_ref().and_then(|o| self.slots.get_mut(o)) {
                            s.set_mode(m, &self.cfg, &mut self.desk);
                        }
                    }
                    menu::Effect::Save => match menu::save(&self.cfg, &self.cfg_path) {
                        Ok(()) => info!("saved settings to {}", self.cfg_path.display()),
                        Err(e) => error!("saving settings failed: {e}"),
                    },
                    menu::Effect::None => {}
                },
                osk::Event::Closed => info!("overlay closed by the compositor"),
                osk::Event::Failed(e) => warn!("on-screen keyboard unavailable: {e}"),
            }
        }
        if keys {
            self.desk.sync();
        }
    }

    /// Banner after a mode switch, naming the hold-to-switch button.
    fn mode_banners(&mut self) {
        let several = self.slots.values().filter(|s| s.is_active()).count() > 1;
        let glyph = menu::button_glyph(&self.cfg.general.switch_button);
        for s in self.slots.values_mut() {
            // The open overlay already shows the mode in its header.
            if !std::mem::take(&mut s.mode_switched) || self.osk.owner().is_some() {
                continue;
            }
            let (title, next) = match s.mode {
                crate::config::Mode::Gamepad => ("Gamepad mode", "desktop mode"),
                crate::config::Mode::Desktop => ("Desktop mode", "gamepad mode"),
            };
            let named = !s.display_name().starts_with("Controller ");
            let who = if several || named { s.display_name() } else { String::new() };
            self.osk.toast(osk::Toast {
                title: title.into(),
                line: osk::ToastLine::Hold { who, glyph: glyph.clone(), next: next.into() },
                secs: osk::Toast::SHORT,
            });
        }
    }

    /// Re-apply the config to every slot after a change.
    fn config_changed(&mut self) {
        for s in self.slots.values_mut() {
            s.apply_config(&self.cfg, &mut self.desk);
        }
    }

    /// Load a preset (None = Default) into the gamepad part of the config.
    fn load_preset(&mut self, name: Option<&str>, manual: bool) {
        let Some(gp) = self.presets.activate(name, manual) else {
            warn!("no preset \"{}\"", name.unwrap_or(presets::DEFAULT_NAME));
            return;
        };
        info!("preset \"{}\"{}", self.presets.active_name(), if manual { "" } else { " (for the focused app)" });
        self.cfg.gamepad = gp;
        self.config_changed();
    }

    /// Another window got focus: offer (or apply) its preset, or go back from an app's preset.
    fn on_focus(&mut self, app: String, title: String) {
        debug!("focused app: {app} ({title})");
        // Same app, new window title: only the shown name changes.
        let same = self.focused.as_ref().is_some_and(|(a, _)| *a == app);
        self.focused = Some((app.clone(), title));
        if same {
            return;
        }
        let mode = self.cfg.general.preset_switch;
        let Some(p) = self.presets.for_app(&app).map(str::to_string) else {
            self.osk.dismiss();
            // Leaving an app whose preset was switched to: back to the hand-picked one,
            // unless there are unsaved changes.
            if self.presets.auto_app.take().is_some() && mode != PresetSwitch::Off {
                if self.presets.state(&self.cfg.gamepad) == presets::State::Clean {
                    let back = self.presets.manual.clone();
                    self.load_preset(back.as_deref(), true);
                } else {
                    info!("keeping the modified preset (unsaved changes)");
                }
            }
            return;
        };
        if self.presets.active.as_deref() == Some(p.as_str()) {
            return;
        }
        let decided = self.presets.decided.get(&app).copied();
        match (mode, decided) {
            (PresetSwitch::Off, _) | (_, Some(false)) => {}
            (PresetSwitch::Auto, _) | (_, Some(true)) => self.switch_for_app(&p, &app),
            (PresetSwitch::Ask, None) => {
                let shown = self.focused.as_ref().map(|(a, t)| presets::app_display(a, t)).unwrap_or_default();
                self.osk.ask(&p, &app, &shown)
            }
        }
    }

    fn switch_for_app(&mut self, preset: &str, app: &str) {
        if self.presets.auto_app.is_none() {
            // Remember what to return to (the current preset, if it was chosen by hand).
            self.presets.manual = self.presets.active.clone();
        }
        self.load_preset(Some(preset), false);
        self.presets.auto_app = Some(app.to_string());
    }

    /// Keep the overlay's header and pages in sync with its owner slot.
    fn sync_osk_status(&mut self) {
        let Some(owner) = self.osk.owner().map(str::to_string) else { return };
        let Some(slot) = self.slots.get(&owner) else {
            self.osk.close();
            return;
        };
        let st = menu::status(&menu::StatusCtx {
            cfg: &self.cfg,
            presets: &self.presets,
            app: self.focused.as_ref().map(|(a, t)| (a.as_str(), t.as_str())),
            slot: &slot.info.slot_label(),
            serial: slot.serial(),
            name: &slot.display_name(),
            mode: slot.mode,
            battery: slot.battery(),
            native: slot.native_client(),
        });
        self.osk.status(&owner, st);
    }

    fn reload(&mut self) {
        match Config::load(&self.cfg_path) {
            Ok(cfg) => {
                info!("reloaded {}", self.cfg_path.display());
                self.presets.default = cfg.gamepad.clone();
                let gp = self.presets.saved().clone();
                self.cfg = cfg;
                self.cfg.gamepad = gp;
                self.config_changed();
            }
            Err(e) => error!("config reload failed, keeping the old config: {e}"),
        }
    }

    pub fn run(&mut self) {
        info!(
            "puckd {} running ({:?} profile, starting in {:?} mode)",
            env!("CARGO_PKG_VERSION"),
            self.cfg.gamepad.profile,
            self.cfg.general.start_mode
        );
        let backoff_every = Duration::from_millis(self.cfg.general.backoff_check_ms.max(250));
        while !sys::quit_requested() {
            if sys::take_reload() {
                self.reload();
            }
            let now = Instant::now();
            if self.last_scan.is_none_or(|t| now.duration_since(t) >= RESCAN) {
                self.rescan();
                self.last_scan = Some(now);
            }
            if self.last_backoff.is_none_or(|t| now.duration_since(t) >= backoff_every) {
                self.check_backoff();
                self.last_backoff = Some(now);
            }

            let mut fds = vec![self.osk.wake_fd()];
            let mut targets: Vec<(String, Target)> = vec![(String::new(), Target::Osk)];
            for (k, s) in &self.slots {
                if let Some(fd) = s.hid_fd() {
                    fds.push(fd);
                    targets.push((k.clone(), Target::Hid));
                }
                if let Some(fd) = s.pad_fd() {
                    fds.push(fd);
                    targets.push((k.clone(), Target::Pad));
                }
                if let Some(fd) = s.clone_fd() {
                    fds.push(fd);
                    targets.push((k.clone(), Target::Clone));
                }
            }
            let mut wake = now + MAX_SLEEP;
            if let Some(t) = self.osk.prompt_deadline() {
                wake = wake.min(t);
            }
            for t in
                [self.last_scan.map(|t| t + RESCAN), self.last_backoff.map(|t| t + backoff_every)].into_iter().flatten()
            {
                wake = wake.min(t);
            }
            for s in self.slots.values() {
                if let Some(t) = s.next_deadline(now, &self.cfg) {
                    wake = wake.min(t);
                }
            }
            let timeout = wake.saturating_duration_since(Instant::now());
            let revents = match sys::poll(&fds, Some(timeout)) {
                Ok(r) => r,
                Err(e) => {
                    error!("poll: {e}");
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };

            let now = Instant::now();
            let mut dead = Vec::new();
            for ((key, target), rev) in targets.iter().zip(revents) {
                if rev == 0 {
                    continue;
                }
                if let Target::Osk = target {
                    self.handle_osk_events();
                    continue;
                }
                let Some(slot) = self.slots.get_mut(key) else { continue };
                match target {
                    Target::Osk => {}
                    Target::Hid => {
                        if rev & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 && rev & libc::POLLIN == 0 {
                            dead.push(key.clone());
                            continue;
                        }
                        if let Err(SlotError::Gone(e)) =
                            slot.on_hid_readable(now, &self.cfg, &mut self.desk, &mut self.osk)
                        {
                            debug!("{}: read error: {e}", slot.label());
                            dead.push(key.clone());
                        }
                    }
                    Target::Pad => slot.on_pad_readable(now),
                    Target::Clone => slot.on_clone_readable(&self.cfg, &mut self.desk),
                }
            }
            for k in dead {
                self.drop_slot(&k, "device gone");
            }
            for s in self.slots.values_mut() {
                s.tick(now, &self.cfg, &mut self.desk, &mut self.osk);
            }
            // Owner gone, disconnected, backed off or taken over by Steam: close the overlay
            // (and whatever it was asking, e.g. a name being typed).
            if let Some(o) = self.osk.owner()
                && self.slots.get(o).is_none_or(|s| !s.is_active())
            {
                self.osk.close();
            }
            // A question no controller can answer any more (all taken over by Steam): drop it,
            // nothing is done. Banners time out by themselves.
            if self.osk.prompt_active()
                && self.slots.values().any(|s| s.steam_has_it())
                && !self.slots.values().any(|s| s.is_active())
            {
                info!("no controller left to answer the popup, dismissing it");
                self.osk.dismiss();
            }
            self.mode_banners();
            for s in self.slots.values_mut() {
                if let Some(t) = s.toast.take() {
                    self.osk.toast(t);
                }
            }
            self.osk.tick();
            for (p, app, answer) in self.osk.take_answers() {
                match answer {
                    osk::Answer::Session => {
                        self.presets.decided.insert(app.clone(), true);
                    }
                    osk::Answer::No => {
                        self.presets.decided.insert(app.clone(), false);
                    }
                    osk::Answer::Once => {}
                }
                let still_focused = self.focused.as_ref().is_some_and(|(a, _)| *a == app);
                if answer != osk::Answer::No && still_focused {
                    self.switch_for_app(&p, &app);
                }
            }
            self.osk.keep_running();
            self.sync_osk_status();
        }
        info!("shutting down");
        for s in self.slots.values_mut() {
            s.restore_lizard();
            status::remove(&s.info);
        }
    }
}

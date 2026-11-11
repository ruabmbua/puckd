//! On-screen keyboard + settings menu: a wlr-layer-shell overlay driven by the controller.
//!
//! The Wayland side runs on its own thread (`ui`). The daemon talks to it through a calloop
//! channel (commands) and gets events back over an mpsc channel plus a wake-up pipe it polls.
//! Keys are typed through the daemon's uinput keyboard, so they reach whatever window has
//! focus; the overlay itself never takes keyboard focus.

pub mod layout;
pub mod menu;
pub mod model;
pub mod picker;
mod render;
mod ui;

use std::collections::HashSet;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::{debug, warn};
use smithay_client_toolkit::reexports::calloop::channel as cchan;

use crate::proto::{Side, State};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Keyboard,
    /// Controller settings (with links to the sub-pages below).
    Settings,
    Mapping,
    Presets,
    Input,
}

impl Tab {
    /// Sub-pages of the Controller tab.
    pub fn is_sub(self) -> bool {
        matches!(self, Tab::Mapping | Tab::Presets | Tab::Input)
    }

    pub fn title(self) -> &'static str {
        match self {
            Tab::Keyboard => "Keyboard",
            Tab::Settings => "Controller",
            Tab::Mapping => "Button mapping",
            Tab::Presets => "Presets",
            Tab::Input => "Input test",
        }
    }
}

/// A value of a settings menu entry.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Choice {
        idx: usize,
        options: Vec<String>,
    },
    Number {
        v: f32,
        min: f32,
        max: f32,
        step: f32,
        percent: bool,
    },
    Toggle(bool),
    Button,
    /// Opens a page or performs an action; the text is shown as info.
    Link(String),
    /// Asks for a name with the on-screen keyboard (pre-filled with this text); the
    /// entered name comes back as `Event::Set` with `Value::Text`.
    Text(String),
    /// Section heading (not selectable).
    Header,
}

impl Value {
    /// Step left (-1) or right (+1). Returns false if nothing changed.
    pub fn step(&mut self, dir: i32) -> bool {
        match self {
            Value::Choice { idx, options } => {
                if options.is_empty() {
                    return false;
                }
                let n = options.len() as i32;
                *idx = ((*idx as i32 + dir).rem_euclid(n)) as usize;
                true
            }
            Value::Number { v, min, max, step, .. } => {
                let nv = ((*v + *step * dir as f32) / *step).round() * *step;
                let nv = nv.clamp(*min, *max);
                let changed = (nv - *v).abs() > f32::EPSILON;
                *v = nv;
                changed
            }
            Value::Toggle(b) => {
                *b = !*b;
                true
            }
            Value::Button | Value::Link(_) | Value::Text(_) | Value::Header => false,
        }
    }

    pub fn display(&self) -> String {
        match self {
            Value::Choice { idx, options } => options.get(*idx).cloned().unwrap_or_default(),
            Value::Number { v, percent: true, .. } => format!("{:.0}%", v * 100.0),
            Value::Number { v, .. } => format!("{v:.1}"),
            Value::Toggle(true) => "on".into(),
            Value::Toggle(false) => "off".into(),
            Value::Button => String::new(),
            Value::Link(s) => s.clone(),
            Value::Text(_) | Value::Header => String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub value: Value,
}

/// What the overlay shows besides the keys: header text and the settings entries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub title: String,
    pub detail: String,
    /// "Controller" tab entries.
    pub items: Vec<Item>,
    /// "Mapping" tab: what is being edited, and the entries.
    pub mapping_title: String,
    pub mapping: Vec<Item>,
    /// "Presets" page.
    pub presets_title: String,
    pub presets: Vec<Item>,
}

/// A short banner: big title, then "<who> · Hold (glyph) for <next>".
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub title: String,
    pub who: String,
    pub glyph: String,
    pub next: String,
}

pub enum Cmd {
    /// Show the overlay on this page, or (None) where it was when it was last closed.
    Show(Option<Tab>),
    Hide,
    /// (buttons as the overlay should act on them, raw controller state for the input test)
    Input(State, State),
    Status(Status),
    /// Show (Some) or hide (None) the small "switch preset?" popup.
    /// (preset, app name)
    Prompt(Option<(String, String)>),
    /// Short banner after a mode switch.
    Toast(Toast),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Key {
        code: u16,
        down: bool,
    },
    Haptic {
        click: bool,
        side: Side,
    },
    Set {
        id: String,
        value: Value,
    },
    /// The overlay went away on its own (compositor closed it).
    Closed,
    /// Another application got keyboard focus: Wayland app_id and window title.
    Focus {
        app: String,
        title: String,
    },
    /// The UI thread could not run (no Wayland, no layer shell...).
    Failed(String),
}

/// Sends events to the daemon and pokes its wake-up pipe.
#[derive(Clone)]
pub struct EventSink {
    tx: mpsc::Sender<Event>,
    wake: Arc<OwnedFd>,
}

impl EventSink {
    pub fn send(&self, e: Event) {
        if self.tx.send(e).is_ok() {
            let b = [1u8];
            unsafe { libc::write(self.wake.as_raw_fd(), b.as_ptr().cast(), 1) };
        }
    }
}

/// Daemon-side handle of the overlay.
pub struct Overlay {
    cmd: Option<cchan::Sender<Cmd>>,
    thread: Option<JoinHandle<()>>,
    rx: mpsc::Receiver<Event>,
    tx: mpsc::Sender<Event>,
    wake_r: OwnedFd,
    wake_w: Arc<OwnedFd>,
    /// hidraw node of the controller that drives the overlay while it is open.
    owner: Option<String>,
    /// Keys the overlay currently holds down (released if it goes away).
    held: HashSet<u16>,
    /// Pending "switch to preset?" question: (preset, app, expires).
    prompt: Option<(String, String, Instant)>,
    /// Answers to the prompt for the daemon: (preset, app, answer).
    answered: Vec<(String, String, Answer)>,
    last_start: Option<Instant>,
    /// Last status sent (to avoid resending identical ones every frame).
    sent: Option<Status>,
}

/// Answer to the "switch preset?" popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Switch, and keep switching for this app until puckd restarts (A).
    Session,
    /// Switch now, ask again next time (X).
    Once,
    /// Don't switch, and don't ask again this session (B).
    No,
}

const PROMPT_TIMEOUT: Duration = Duration::from_secs(10);
const RESTART_EVERY: Duration = Duration::from_secs(15);

impl Overlay {
    pub fn new() -> io::Result<Self> {
        let mut fds = [0 as RawFd; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let (wake_r, wake_w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let (tx, rx) = mpsc::channel();
        Ok(Self {
            cmd: None,
            thread: None,
            rx,
            tx,
            wake_r,
            wake_w: Arc::new(wake_w),
            owner: None,
            held: HashSet::new(),
            prompt: None,
            answered: Vec::new(),
            last_start: None,
            sent: None,
        })
    }

    pub fn wake_fd(&self) -> RawFd {
        self.wake_r.as_raw_fd()
    }

    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    pub fn is_open_for(&self, node: &str) -> bool {
        self.owner.as_deref() == Some(node)
    }

    fn send(&mut self, c: Cmd) -> bool {
        match &self.cmd {
            Some(tx) => {
                if tx.send(c).is_err() {
                    self.cmd = None;
                    false
                } else {
                    true
                }
            }
            None => false,
        }
    }

    /// Keep the UI thread running (it also tracks the focused app); retries are throttled.
    pub fn keep_running(&mut self) {
        let dead = self.cmd.is_none() || self.thread.as_ref().is_some_and(|t| t.is_finished());
        if dead && self.last_start.is_none_or(|t| t.elapsed() >= RESTART_EVERY) {
            self.ensure_thread();
        }
    }

    fn ensure_thread(&mut self) {
        self.last_start = Some(Instant::now());
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            self.cmd = None;
        }
        if self.cmd.is_some() {
            return;
        }
        let (ctx, crx) = cchan::channel();
        let sink = EventSink { tx: self.tx.clone(), wake: self.wake_w.clone() };
        match std::thread::Builder::new().name("puckd-osk".into()).spawn(move || ui::run(crx, sink)) {
            Ok(h) => {
                self.cmd = Some(ctx);
                self.thread = Some(h);
            }
            Err(e) => warn!("cannot start the overlay thread: {e}"),
        }
    }

    /// Open the overlay for `node`. The daemon sends its status right after (`status`).
    pub fn open(&mut self, node: &str, tab: Option<Tab>) {
        self.ensure_thread();
        self.owner = Some(node.to_string());
        self.sent = None;
        if !self.send(Cmd::Show(tab)) {
            self.owner = None;
        }
    }

    /// Ask whether to switch to `preset` (A = yes, B = no; times out).
    /// `app` is the app key the answer is for, `shown` its readable name.
    pub fn ask(&mut self, preset: &str, app: &str, shown: &str) {
        self.keep_running();
        self.prompt = Some((preset.to_string(), app.to_string(), Instant::now() + PROMPT_TIMEOUT));
        self.send(Cmd::Prompt(Some((preset.to_string(), shown.to_string()))));
    }

    pub fn prompt_active(&self) -> bool {
        self.prompt.is_some()
    }

    /// Answer the pending question (picked up by the daemon through `take_answers`).
    pub fn answer(&mut self, a: Answer) {
        if let Some((p, app, _)) = self.prompt.take() {
            self.send(Cmd::Prompt(None));
            self.answered.push((p, app, a));
        }
    }

    /// Show a short banner (e.g. after a mode switch).
    pub fn toast(&mut self, t: Toast) {
        self.keep_running();
        self.send(Cmd::Toast(t));
    }

    /// Drop a pending question without recording an answer (focus moved on).
    pub fn dismiss(&mut self) {
        if self.prompt.take().is_some() {
            self.send(Cmd::Prompt(None));
        }
    }

    pub fn take_answers(&mut self) -> Vec<(String, String, Answer)> {
        std::mem::take(&mut self.answered)
    }

    /// Expire an unanswered question.
    pub fn tick(&mut self) {
        // Unanswered: no decision is remembered, so it asks again next time.
        if self.prompt.as_ref().is_some_and(|(_, _, t)| Instant::now() >= *t) {
            self.dismiss();
        }
    }

    pub fn prompt_deadline(&self) -> Option<Instant> {
        self.prompt.as_ref().map(|(_, _, t)| *t)
    }

    pub fn close(&mut self) {
        self.owner = None;
        self.send(Cmd::Hide);
    }

    pub fn input(&mut self, st: &State, raw: &State) {
        self.send(Cmd::Input(*st, *raw));
    }

    pub fn status(&mut self, node: &str, status: Status) {
        if self.is_open_for(node) && self.sent.as_ref() != Some(&status) {
            self.sent = Some(status.clone());
            self.send(Cmd::Status(status));
        }
    }

    /// Collect pending events. Tracks held keys so they can be released on failure.
    pub fn drain(&mut self) -> Vec<Event> {
        let mut buf = [0u8; 64];
        while unsafe { libc::read(self.wake_r.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
        let mut out: Vec<Event> = self.rx.try_iter().collect();
        for e in &out {
            match e {
                Event::Key { code, down: true } => {
                    self.held.insert(*code);
                }
                Event::Key { code, down: false } => {
                    self.held.remove(code);
                }
                Event::Closed | Event::Failed(_) => self.owner = None,
                _ => {}
            }
        }
        // Whenever the overlay is gone, nothing may stay pressed.
        if self.owner.is_none() && !self.held.is_empty() {
            debug!("overlay closed with {} keys held, releasing", self.held.len());
            out.extend(self.held.drain().map(|code| Event::Key { code, down: false }));
        }
        out
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // Dropping the sender ends the UI thread's loop.
        self.cmd = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

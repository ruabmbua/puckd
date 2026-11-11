//! `puckd osk-preview --script FILE`: drive the overlay with a scripted, virtual controller.
//! Used to record the README showreel and to check overlay flows by eye. Nothing reaches the
//! desktop: no uinput devices are created; typed keys are only printed.
//!
//! Script: one command per line, `#` comments. Times in ms, pad/stick values in -1..1.
//!
//! ```text
//! show [keyboard|settings|presets|mapping|input]   open the overlay (default: last page)
//! hide                                               close it
//! wait MS                                            keep sending the current state
//! press BTN...  / release BTN... | all               buttons (protocol names: a, lb, menu, ...)
//! tap BTN... [MS]                                    press, hold MS (default 120), release
//! rpad X Y | lpad X Y | rpad off | lpad off          touch a trackpad at a position
//! slide rpad|lpad X1 Y1 X2 Y2 MS                     move a touching thumb over time
//! stick l|r X Y                                      stick position
//! trigger lt|rt V                                    trigger pull 0..1
//! toast TITLE | WHO | NEXT                           mode-switch banner (also sets the mode)
//! mode desktop|gamepad                              mode shown in the header
//! ask PRESET | APP                                   "switch preset?" popup
//! answer session|once|no                             answer it
//! caption TEXT                                       printed as `CAPTION <ms> TEXT` (empty: clear)
//! ```

use std::path::Path;
use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::config::{Config, Mode};
use crate::osk::{self, Overlay, Tab, menu};
use crate::presets::Presets;
use crate::proto::{State, btn};
use crate::sys;

const FRAME: Duration = Duration::from_millis(8);

struct Demo {
    ov: Overlay,
    cfg: Config,
    presets: Presets,
    mode: Mode,
    st: State,
    start: Instant,
}

fn bits(names: &[&str]) -> Result<u32, String> {
    names.iter().try_fold(0, |acc, n| btn::from_name(n).map(|b| acc | b).ok_or_else(|| format!("unknown button '{n}'")))
}

fn axis(v: &str) -> Result<i16, String> {
    let f: f32 = v.parse().map_err(|_| format!("not a number: {v}"))?;
    Ok((f.clamp(-1.0, 1.0) * 32767.0) as i16)
}

impl Demo {
    fn status(&mut self) {
        let st = menu::status(&menu::StatusCtx {
            cfg: &self.cfg,
            presets: &self.presets,
            app: Some(("steam_app_42", "Example Game")),
            slot: "1",
            serial: Some("FXA00000000AA"),
            name: "Couch",
            mode: self.mode,
            battery: Some((1, 88)),
        });
        self.ov.status("demo", st);
    }

    /// Send the current state for `ms`, applying what the overlay asks for.
    fn run_for(&mut self, ms: u64) {
        let end = Instant::now() + Duration::from_millis(ms);
        loop {
            self.ov.input(&self.st, &self.st);
            for e in self.ov.drain() {
                match e {
                    osk::Event::Key { code, down } => println!("KEY {code} {}", if down { "down" } else { "up" }),
                    osk::Event::Set { id, value } => match menu::apply(&mut self.cfg, &id, &value) {
                        menu::Effect::Mode(m) => self.mode = m,
                        menu::Effect::PresetLoad(n) => {
                            if let Some(gp) = self.presets.activate(n.as_deref(), true) {
                                self.cfg.gamepad = gp;
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            self.status();
            if Instant::now() >= end || sys::quit_requested() {
                break;
            }
            sleep(FRAME);
        }
    }

    fn exec(&mut self, line: &str) -> Result<(), String> {
        let (cmd, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        let args: Vec<&str> = rest.split_whitespace().collect();
        let pipe: Vec<&str> = rest.split('|').map(str::trim).collect();
        match cmd {
            "show" => {
                let tab = match args.first().copied() {
                    Some("keyboard") => Some(Tab::Keyboard),
                    Some("settings") => Some(Tab::Settings),
                    Some("presets") => Some(Tab::Presets),
                    Some("mapping") => Some(Tab::Mapping),
                    Some("input") => Some(Tab::Input),
                    _ => None,
                };
                self.ov.open("demo", tab);
                self.status();
            }
            "hide" => self.ov.close(),
            "wait" => self.run_for(args.first().and_then(|v| v.parse().ok()).unwrap_or(500)),
            "press" => self.st.buttons |= bits(&args)?,
            "release" if args.first() == Some(&"all") => self.st.buttons = 0,
            "release" => self.st.buttons &= !bits(&args)?,
            "tap" => {
                let hold = args.last().and_then(|v| v.parse::<u64>().ok());
                let names: Vec<&str> = args.iter().copied().filter(|a| a.parse::<u64>().is_err()).collect();
                let b = bits(&names)?;
                self.st.buttons |= b;
                self.run_for(hold.unwrap_or(120));
                self.st.buttons &= !b;
                self.run_for(60);
            }
            "rpad" | "lpad" => {
                let (touch, x, y) = if cmd == "rpad" {
                    (btn::RPAD_TOUCH, &mut self.st.rpad_x, &mut self.st.rpad_y)
                } else {
                    (btn::LPAD_TOUCH, &mut self.st.lpad_x, &mut self.st.lpad_y)
                };
                if args.first() == Some(&"off") {
                    self.st.buttons &= !touch;
                } else {
                    *x = axis(args.first().ok_or("x?")?)?;
                    *y = axis(args.get(1).ok_or("y?")?)?;
                    self.st.buttons |= touch;
                }
            }
            "slide" => {
                let [pad, x1, y1, x2, y2, ms] = args[..] else { return Err("slide PAD X1 Y1 X2 Y2 MS".into()) };
                let (a, b, c, d) = (axis(x1)? as f32, axis(y1)? as f32, axis(x2)? as f32, axis(y2)? as f32);
                let ms: u64 = ms.parse().map_err(|_| "MS?")?;
                let steps = (ms / 8).max(1);
                let touch = if pad == "rpad" { btn::RPAD_TOUCH } else { btn::LPAD_TOUCH };
                self.st.buttons |= touch;
                for i in 0..=steps {
                    let t = i as f32 / steps as f32;
                    let (x, y) = ((a + (c - a) * t) as i16, (b + (d - b) * t) as i16);
                    if pad == "rpad" {
                        (self.st.rpad_x, self.st.rpad_y) = (x, y);
                    } else {
                        (self.st.lpad_x, self.st.lpad_y) = (x, y);
                    }
                    self.run_for(0);
                    sleep(FRAME);
                }
            }
            "stick" => {
                let (x, y) = (axis(args.get(1).ok_or("x?")?)?, axis(args.get(2).ok_or("y?")?)?);
                if args.first() == Some(&"l") {
                    (self.st.lx, self.st.ly) = (x, y);
                } else {
                    (self.st.rx, self.st.ry) = (x, y);
                }
            }
            "trigger" => {
                let v =
                    (args.get(1).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0).clamp(0.0, 1.0) * 32767.0) as i16;
                if args.first() == Some(&"lt") {
                    self.st.lt = v;
                } else {
                    self.st.rt = v;
                }
            }
            "toast" => {
                let title = pipe.first().unwrap_or(&"").to_string();
                if title.starts_with("Gamepad") {
                    self.mode = Mode::Gamepad;
                } else if title.starts_with("Desktop") {
                    self.mode = Mode::Desktop;
                }
                self.ov.toast(osk::Toast {
                    title,
                    line: osk::ToastLine::Hold {
                        who: pipe.get(1).unwrap_or(&"").to_string(),
                        glyph: "≡".into(),
                        next: pipe.get(2).unwrap_or(&"").to_string(),
                    },
                    secs: osk::Toast::SHORT,
                })
            }
            "mode" => self.mode = if args.first() == Some(&"gamepad") { Mode::Gamepad } else { Mode::Desktop },
            "ask" => self.ov.ask(pipe.first().unwrap_or(&""), "steam_app_42", pipe.get(1).unwrap_or(&"")),
            "answer" => self.ov.answer(match args.first().copied() {
                Some("session") => osk::Answer::Session,
                Some("once") => osk::Answer::Once,
                _ => osk::Answer::No,
            }),
            "caption" => println!("CAPTION {} {rest}", self.start.elapsed().as_millis()),
            other => return Err(format!("unknown command '{other}'")),
        }
        Ok(())
    }
}

pub fn run(path: &Path) -> ExitCode {
    let script = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let ov = match Overlay::new() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    sys::install_signal_handlers();
    let cfg = Config::default();
    let presets = Presets::load(&crate::presets::default_dir(), cfg.gamepad.clone());
    let mut d = Demo { ov, cfg, presets, mode: Mode::Desktop, st: State::default(), start: Instant::now() };
    println!("START");
    for (n, line) in script.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Err(e) = d.exec(line) {
            eprintln!("{}:{}: {e}", path.display(), n + 1);
            return ExitCode::FAILURE;
        }
        if sys::quit_requested() {
            break;
        }
    }
    d.ov.close();
    d.run_for(200);
    println!("END {}", d.start.elapsed().as_millis());
    ExitCode::SUCCESS
}

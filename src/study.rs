//! `puckd mouse-study`: guided recording session for tuning the trackpad mouse.
//!
//! Walks the user through short tasks while driving the real pointer with the current
//! settings, and records (all with microsecond timestamps since the start):
//! - `raw.jsonl`: every controller state report (buttons, pads, sticks, triggers)
//! - `out.jsonl`: every pointer move / wheel / button event puckd emitted
//! - `cursor.jsonl`: the actual cursor position (Hyprland: `hyprctl cursorpos`, ~30 Hz)
//! - `tasks.jsonl`: task names, start/end times, the user's rating and comment
//! - `config.toml`, `monitors.json`: the settings and screen layout used

use std::fs::{self, File};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::desktop::{DesktopDevices, DesktopState, Feedback};
use crate::proto::{self, HapticCmd, Input, Side, State};
use crate::sys;

struct Task {
    name: &'static str,
    text: &'static str,
    secs: u64,
}

const TASKS: &[Task] = &[
    Task {
        name: "rest",
        text: "Put your thumb on the RIGHT trackpad and keep it as still as you can.\n\
               (Measures sensor noise: the cursor should not move.)",
        secs: 5,
    },
    Task {
        name: "slow_sweep",
        text: "Move the cursor SLOWLY from the left edge of the screen to the right edge,\n\
               then back. Lift and re-place your thumb whenever you run out of pad.",
        secs: 12,
    },
    Task {
        name: "corners",
        text: "Move the cursor into each screen corner in turn: top-left, top-right,\n\
               bottom-right, bottom-left. Go at a normal, comfortable speed.",
        secs: 15,
    },
    Task {
        name: "flicks",
        text: "Flick the cursor quickly across the screen and back, about 5 times.\n\
               (Short, fast swipes; inertia may let it glide.)",
        secs: 10,
    },
    Task {
        name: "precise",
        text: "Precision: put the cursor on single characters of this terminal text,\n\
               moving one character at a time along a line, then one line down, etc.",
        secs: 15,
    },
    Task {
        name: "circles",
        text: "Draw a few slow circles with the cursor, about the size of your palm on screen.",
        secs: 10,
    },
    Task {
        name: "clicks",
        text: "Without moving, CLICK the right trackpad 5 times (press it down firmly),\n\
               keeping your thumb where it is. (Measures whether clicking moves the cursor.)",
        secs: 8,
    },
    Task {
        name: "point_and_click",
        text: "Point at things and click them like you normally would: e.g. a few window\n\
               titles or buttons on screen (clicking is fine, it's a real mouse click).",
        secs: 15,
    },
    Task {
        name: "scroll_slow",
        text: "Hover over this terminal (or a long web page) and SCROLL slowly with the\n\
               LEFT trackpad: drag up and down a few lines at a time.",
        secs: 10,
    },
    Task {
        name: "scroll_fast",
        text: "Now scroll FAST with the left trackpad: big quick swipes, several pages.",
        secs: 8,
    },
];

fn other_puckd_running() -> bool {
    let me = std::process::id();
    fs::read_dir("/proc").into_iter().flatten().flatten().any(|e| {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { return false };
        pid != me && fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "puckd")
    })
}

fn us(base: Instant, t: Instant) -> u128 {
    t.saturating_duration_since(base).as_micros()
}

fn prompt_line(text: &str) -> String {
    print!("{text}");
    let _ = io::stdout().flush();
    let mut s = String::new();
    let _ = io::stdin().lock().read_line(&mut s);
    s.trim().to_string()
}

fn json_str(s: &str) -> String {
    format!("{s:?}") // Rust debug quoting is valid JSON for ordinary text
}

pub fn run(cfg: Config, args: &[String]) -> ExitCode {
    if other_puckd_running() && !args.iter().any(|a| a == "--force") {
        eprintln!(
            "Another puckd is running. Stop it first (e.g. `systemctl --user stop puckd`, or Ctrl-C\n\
             your terminal running it), then start the study again. (--force to ignore.)"
        );
        return ExitCode::FAILURE;
    }
    let Some(mut hid) = crate::tools::active_slot(None) else {
        eprintln!("no connected controller found (is it on?)");
        return ExitCode::FAILURE;
    };
    let mut desk = match DesktopDevices::create() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot create the virtual mouse: {e}");
            return ExitCode::FAILURE;
        }
    };
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dir = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(format!("puckd-study-{stamp}"));
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("{}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    let open = |n: &str| File::create(dir.join(n)).map(io::BufWriter::new);
    let (Ok(mut raw), Ok(mut outf), Ok(mut cursorf), Ok(mut tasksf)) =
        (open("raw.jsonl"), open("out.jsonl"), open("cursor.jsonl"), open("tasks.jsonl"))
    else {
        eprintln!("cannot write to {}", dir.display());
        return ExitCode::FAILURE;
    };
    let _ = fs::write(dir.join("config.toml"), toml::to_string(&cfg).unwrap_or_default());
    if let Ok(o) = std::process::Command::new("hyprctl").args(["monitors", "-j"]).output() {
        let _ = fs::write(dir.join("monitors.json"), o.stdout);
    }

    // Cursor sampler (Hyprland only).
    let base = Instant::now();
    let (ctx, crx) = mpsc::channel::<(Instant, i32, i32)>();
    let hypr = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some();
    if hypr {
        std::thread::spawn(move || {
            loop {
                let Ok(o) = std::process::Command::new("hyprctl").arg("cursorpos").output() else { return };
                let t = Instant::now();
                let s = String::from_utf8_lossy(&o.stdout);
                let mut it = s.trim().split(',').map(|v| v.trim().parse::<i32>());
                if let (Some(Ok(x)), Some(Ok(y))) = (it.next(), it.next())
                    && ctx.send((t, x, y)).is_err()
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        });
    } else {
        println!("(not Hyprland: the actual cursor position won't be recorded)");
    }

    println!(
        "\npuckd mouse study: {} tasks, about 2 minutes. Your current settings are used\n\
         (pointer speed {}, acceleration {}, smoothing {}, inertia {}, scroll speed {}).\n\
         Data goes to {}\n",
        TASKS.len(),
        cfg.desktop.pointer_speed,
        cfg.desktop.pointer_accel,
        cfg.desktop.smoothing,
        cfg.desktop.inertia,
        cfg.desktop.scroll_speed,
        dir.display()
    );
    sys::install_signal_handlers();
    let mut state = DesktopState::new(&cfg.desktop);
    desk.trace = Some(Vec::new());
    let mut buf = [0u8; 64];
    let mut last_keepalive: Option<Instant> = None;
    let mut last_tick: Option<Instant> = None;
    let lizard = proto::settings_report(&[(proto::SETTING_LIZARD_MODE, proto::LIZARD_MODE_OFF)]);

    for (i, task) in TASKS.iter().enumerate() {
        println!("── Task {}/{}: {} ──\n{}\n", i + 1, TASKS.len(), task.name, task.text);
        prompt_line(&format!("Press Enter to start (you get {} s)… ", task.secs));
        let t0 = Instant::now();
        let end = t0 + Duration::from_secs(task.secs);
        let mut shown = u64::MAX;
        while Instant::now() < end && !sys::quit_requested() {
            let left = end.saturating_duration_since(Instant::now()).as_secs();
            if left != shown {
                print!("\r  recording… {left:2} s left ");
                let _ = io::stdout().flush();
                shown = left;
            }
            if last_keepalive.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
                let _ = hid.set_feature(&lizard);
                last_keepalive = Some(Instant::now());
            }
            let Ok(rev) = sys::poll(&[hid.fd()], Some(Duration::from_millis(20))) else { break };
            if rev[0] == 0 {
                continue;
            }
            while let Ok(Some(n)) = hid.read_report(&mut buf) {
                let now = Instant::now();
                let Some(Input::State(st)) = proto::parse(&buf[..n]) else { continue };
                write_raw(&mut raw, us(base, now), &st);
                let out = state.update(&st, now, &cfg.desktop, &cfg.haptics, &mut desk, None);
                for f in out.feedback {
                    haptic(&mut hid, f, &cfg, now, &mut last_tick);
                }
            }
            flush_trace(&mut desk, &mut outf, base);
            for (t, x, y) in crx.try_iter() {
                let _ = writeln!(cursorf, "{{\"t\":{},\"x\":{x},\"y\":{y}}}", us(base, t));
            }
        }
        state.release_all(&mut desk);
        flush_trace(&mut desk, &mut outf, base);
        let t1 = Instant::now();
        println!("\r  done.                      ");
        let rating = prompt_line("  How did that feel? 1 (bad) … 5 (great), Enter to skip: ");
        let comment = prompt_line("  Anything specific (too fast/slow, jumpy, drift, …)? ");
        let _ = writeln!(
            tasksf,
            "{{\"task\":{},\"start\":{},\"end\":{},\"rating\":{},\"comment\":{}}}",
            json_str(task.name),
            us(base, t0),
            us(base, t1),
            rating.parse::<u8>().map(|r| r.to_string()).unwrap_or("null".into()),
            json_str(&comment)
        );
        let _ = tasksf.flush();
        println!();
        if sys::quit_requested() {
            break;
        }
    }
    for w in [&mut raw, &mut outf, &mut cursorf, &mut tasksf] {
        let _ = w.flush();
    }
    let _ = hid.set_feature(&proto::settings_report(&[(proto::SETTING_LIZARD_MODE, proto::LIZARD_MODE_ON)]));
    println!("Thanks! Recorded to {}\nTell Claude this path so it can analyse the data.", dir.display());
    ExitCode::SUCCESS
}

fn write_raw(w: &mut impl Write, t: u128, s: &State) {
    let _ = writeln!(
        w,
        "{{\"t\":{t},\"seq\":{},\"b\":{},\"lt\":{},\"rt\":{},\"lsx\":{},\"lsy\":{},\"rsx\":{},\"rsy\":{},\
         \"lx\":{},\"ly\":{},\"lp\":{},\"rx\":{},\"ry\":{},\"rp\":{}}}",
        s.seq,
        s.buttons,
        s.lt,
        s.rt,
        s.lx,
        s.ly,
        s.rx,
        s.ry,
        s.lpad_x,
        s.lpad_y,
        s.lpad_pressure,
        s.rpad_x,
        s.rpad_y,
        s.rpad_pressure
    );
}

fn flush_trace(desk: &mut DesktopDevices, w: &mut impl Write, base: Instant) {
    if let Some(tr) = &mut desk.trace {
        for (t, kind, a, b) in tr.drain(..) {
            let _ = writeln!(w, "{{\"t\":{},\"k\":\"{kind}\",\"a\":{a},\"b\":{b}}}", us(base, t));
        }
    }
}

fn haptic(hid: &mut crate::hid::HidDevice, f: Feedback, cfg: &Config, now: Instant, last: &mut Option<Instant>) {
    let h = &cfg.haptics;
    if !h.enabled {
        return;
    }
    if matches!(f, Feedback::PointerTick | Feedback::ScrollTick)
        && last.is_some_and(|t| now.duration_since(t) < Duration::from_millis(25))
    {
        return;
    }
    *last = Some(now);
    let r = match f {
        Feedback::PointerTick => proto::haptic_command_report(Side::Right, HapticCmd::Tick, h.gain_db),
        Feedback::ScrollTick => proto::haptic_command_report(Side::Left, HapticCmd::Tick, h.gain_db),
        Feedback::Click(side) => proto::haptic_command_report(side, HapticCmd::Click, h.gain_db),
        Feedback::Release(side) => proto::haptic_command_report(side, HapticCmd::Tick, h.gain_db),
    };
    let _ = hid.write_output(&r);
}

/// Pull `"key":number` out of a flat JSON line.
fn field(line: &str, key: &str) -> Option<i64> {
    let pat = format!("\"{key}\":");
    let i = line.find(&pat)? + pat.len();
    let rest = &line[i..];
    let end = rest.find(|c: char| c != '-' && !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// `puckd replay DIR [-c FILE]`: run a recorded study through the current pointer code
/// (no real devices) and print per-task numbers, to compare tuning changes.
pub fn replay(cfg: Config, args: &[String]) -> ExitCode {
    let Some(dir) = args.iter().find(|a| !a.starts_with('-')).map(PathBuf::from) else {
        eprintln!("usage: puckd replay <study dir>");
        return ExitCode::from(2);
    };
    let (Ok(raw), Ok(tasks)) = (fs::read_to_string(dir.join("raw.jsonl")), fs::read_to_string(dir.join("tasks.jsonl")))
    else {
        eprintln!("{}: no raw.jsonl / tasks.jsonl", dir.display());
        return ExitCode::FAILURE;
    };
    let base = Instant::now();
    let mut desk = DesktopDevices::recorder();
    let mut state = DesktopState::new(&cfg.desktop);
    let mut samples: Vec<(u128, State)> = Vec::new();
    for l in raw.lines() {
        let f = |k| field(l, k).unwrap_or(0);
        let st = State {
            report_id: 0x42,
            seq: f("seq") as u8,
            buttons: f("b") as u32,
            lt: f("lt") as i16,
            rt: f("rt") as i16,
            lx: f("lsx") as i16,
            ly: f("lsy") as i16,
            rx: f("rsx") as i16,
            ry: f("rsy") as i16,
            lpad_x: f("lx") as i16,
            lpad_y: f("ly") as i16,
            lpad_pressure: f("lp") as u16,
            rpad_x: f("rx") as i16,
            rpad_y: f("ry") as i16,
            rpad_pressure: f("rp") as u16,
            ..Default::default()
        };
        samples.push((f("t") as u128, st));
    }
    // Replay with the recorded timing.
    let mut out: Vec<(u128, &'static str, i32, i32)> = Vec::new();
    for (t, st) in &samples {
        let now = base + Duration::from_micros(*t as u64);
        state.update(st, now, &cfg.desktop, &cfg.haptics, &mut desk, None);
        if let Some(tr) = &mut desk.trace {
            out.extend(tr.drain(..).map(|(_, k, a, b)| (*t, k, a, b)));
        }
    }
    if let Some(i) = args.iter().position(|a| a == "--dump") {
        let path = args.get(i + 1).map(PathBuf::from).unwrap_or_else(|| dir.join("replay-out.jsonl"));
        let mut f = io::BufWriter::new(File::create(&path).expect("dump file"));
        for (t, k, a, b) in &out {
            let _ = writeln!(f, "{{\"t\":{t},\"k\":\"{k}\",\"a\":{a},\"b\":{b}}}");
        }
        println!("dumped {} events to {}", out.len(), path.display());
    }
    const RT: u32 = proto::btn::RPAD_TOUCH;
    const RC: u32 = proto::btn::RPAD_CLICK;
    println!(
        "{:<16} {:>9} {:>11} {:>12} {:>12} {:>9}",
        "task", "moved px", "rest drift", "after lifts", "near clicks", "scroll"
    );
    for l in tasks.lines() {
        let name = l.split("\"task\":\"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("?");
        let (s0, s1) = (field(l, "start").unwrap_or(0) as u128, field(l, "end").unwrap_or(0) as u128);
        let mv = |a: u128, b: u128| -> i64 {
            out.iter().filter(|o| o.1 == "move" && o.0 >= a && o.0 <= b).map(|o| (o.2.abs() + o.3.abs()) as i64).sum()
        };
        let moved = mv(s0, s1);
        let scroll: i64 =
            out.iter().filter(|o| o.1 == "wheel" && o.0 >= s0 && o.0 <= s1).map(|o| o.2.abs() as i64).sum::<i64>()
                / 120;
        let win: Vec<&(u128, State)> = samples.iter().filter(|(t, _)| *t >= s0 && *t <= s1).collect();
        let mut lifts = 0;
        let mut clicks = 0;
        for w in win.windows(2) {
            let (a, b) = (&w[0].1, &w[1].1);
            if a.buttons & RT != 0 && b.buttons & RT == 0 {
                lifts += mv(w[1].0, w[1].0 + 400_000);
            }
            if (a.buttons ^ b.buttons) & RC != 0 {
                clicks += mv(w[1].0.saturating_sub(150_000), w[1].0 + 150_000);
            }
        }
        let rest = if name == "rest" { moved.to_string() } else { "-".into() };
        println!("{name:<16} {moved:>9} {rest:>11} {lifts:>12} {clicks:>12} {scroll:>9}");
    }
    ExitCode::SUCCESS
}

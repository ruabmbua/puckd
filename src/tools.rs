//! Diagnostic subcommands: list, dump, haptic-test.

use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::hid::{self, HidDevice, HidInfo};
use crate::proto::{self, HapticCmd, Input, Side, State, btn};
use crate::sys;

fn pick(filter: Option<&str>) -> Vec<HidInfo> {
    hid::scan().into_iter().filter(|i| filter.is_none_or(|f| i.slot_label() == f || i.node == f)).collect()
}

pub fn list() -> ExitCode {
    let found = hid::scan();
    if found.is_empty() {
        println!("no Steam Controller slots found");
        return ExitCode::FAILURE;
    }
    for i in found {
        let access = match HidDevice::open(&i) {
            Ok(_) => "rw ok".to_string(),
            Err(e) => format!("cannot open: {e}"),
        };
        println!(
            "slot {:<4} {:<14} {:04x}:{:04x} iface {:<4} uniq {:<16} {access}",
            i.slot_label(),
            i.dev.display(),
            i.vid,
            i.pid,
            i.iface.map(|n| n.to_string()).unwrap_or("-".into()),
            i.uniq
        );
    }
    ExitCode::SUCCESS
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn analog_line(s: &State, imu: bool) -> String {
    let mut l = format!(
        "LS({:6},{:6}) RS({:6},{:6}) LT{:6} RT{:6} LP({:6},{:6},{:5}) RP({:6},{:6},{:5})",
        s.lx, s.ly, s.rx, s.ry, s.lt, s.rt, s.lpad_x, s.lpad_y, s.lpad_pressure, s.rpad_x, s.rpad_y, s.rpad_pressure
    );
    if imu {
        l += &format!(
            " acc({:6},{:6},{:6}) gyro({:6},{:6},{:6}) t{}",
            s.accel[0], s.accel[1], s.accel[2], s.gyro[0], s.gyro[1], s.gyro[2], s.imu_tick
        );
    }
    l
}

fn moved(a: &State, b: &State) -> bool {
    let d = |x: i16, y: i16| (x as i32 - y as i32).abs() > 600;
    d(a.lx, b.lx)
        || d(a.ly, b.ly)
        || d(a.rx, b.rx)
        || d(a.ry, b.ry)
        || d(a.lt, b.lt)
        || d(a.rt, b.rt)
        || d(a.lpad_x, b.lpad_x)
        || d(a.lpad_y, b.lpad_y)
        || d(a.rpad_x, b.rpad_x)
        || d(a.rpad_y, b.rpad_y)
}

pub fn dump(args: &[String]) -> ExitCode {
    let raw = args.iter().any(|a| a == "--raw");
    // Lizard mode (the controller's own mouse/keyboard) is off unless asked to keep it,
    // so the dump doesn't move the pointer or type.
    let lizard_off = !args.iter().any(|a| a == "--keep-lizard");
    let imu = args.iter().any(|a| a == "--imu");
    let filter = args.iter().find(|a| !a.starts_with('-')).map(String::as_str);
    let mut devs: Vec<HidDevice> = Vec::new();
    for i in pick(filter) {
        match HidDevice::open(&i) {
            Ok(d) => {
                println!("# slot {} = {}", i.slot_label(), i.dev.display());
                devs.push(d);
            }
            Err(e) => eprintln!("{}: {e}", i.dev.display()),
        }
    }
    if devs.is_empty() {
        eprintln!("no controller slots to dump");
        return ExitCode::FAILURE;
    }
    println!("# press buttons / move sticks; Ctrl-C to stop{}", if lizard_off { " (lizard mode off)" } else { "" });
    let mut last: Vec<Option<State>> = vec![None; devs.len()];
    let mut last_keepalive: Option<Instant> = None;
    let mut buf = [0u8; 64];
    while !sys::quit_requested() {
        if lizard_off && last_keepalive.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
            for d in &mut devs {
                let _ = d.set_feature(&proto::settings_report(&[(proto::SETTING_LIZARD_MODE, proto::LIZARD_MODE_OFF)]));
            }
            last_keepalive = Some(Instant::now());
        }
        let fds: Vec<i32> = devs.iter().map(|d| d.fd()).collect();
        let Ok(rev) = sys::poll(&fds, Some(Duration::from_millis(250))) else { break };
        for (i, r) in rev.iter().enumerate() {
            if *r == 0 {
                continue;
            }
            let label = devs[i].info.slot_label();
            while let Ok(Some(n)) = devs[i].read_report(&mut buf) {
                let b = &buf[..n];
                if raw {
                    println!("[{label}] {}", hex(b));
                    continue;
                }
                match proto::parse(b) {
                    Some(Input::State(s)) => {
                        let prev = last[i];
                        let pb = prev.map(|p| p.buttons).unwrap_or(u32::MAX);
                        if s.buttons != pb {
                            println!(
                                "[{label}] {:#04x} buttons {:#010x}: {}",
                                s.report_id,
                                s.buttons,
                                btn::names(s.buttons).join(" ")
                            );
                        }
                        if prev.is_none_or(|p| moved(&p, &s)) {
                            println!("[{label}] {}", analog_line(&s, imu));
                            last[i] = Some(s);
                        } else if let Some(p) = &mut last[i] {
                            p.buttons = s.buttons;
                        }
                    }
                    Some(Input::Battery { charge_state, level }) => {
                        println!("[{label}] battery {level}% {} ({})", proto::charge_state_name(charge_state), hex(b))
                    }
                    Some(Input::Wireless(w)) => println!(
                        "[{label}] wireless {}",
                        match w {
                            proto::WIRELESS_CONNECT => "connect",
                            proto::WIRELESS_DISCONNECT => "disconnect",
                            _ => "?",
                        }
                    ),
                    _ => println!("[{label}] other: {}", hex(b)),
                }
            }
        }
    }
    if lizard_off {
        for d in &mut devs {
            let _ = d.set_feature(&proto::settings_report(&[(proto::SETTING_LIZARD_MODE, proto::LIZARD_MODE_ON)]));
        }
    }
    ExitCode::SUCCESS
}

/// Find the first slot that is sending state reports.
pub(crate) fn active_slot(filter: Option<&str>) -> Option<HidDevice> {
    let mut devs: Vec<HidDevice> = pick(filter).iter().filter_map(|i| HidDevice::open(i).ok()).collect();
    let end = Instant::now() + Duration::from_secs(2);
    let mut buf = [0u8; 64];
    while Instant::now() < end {
        let fds: Vec<i32> = devs.iter().map(|d| d.fd()).collect();
        let rev = sys::poll(&fds, Some(Duration::from_millis(100))).ok()?;
        for (i, r) in rev.iter().enumerate() {
            if *r != 0
                && let Ok(Some(n)) = devs[i].read_report(&mut buf)
                && matches!(proto::parse(&buf[..n]), Some(Input::State(_)))
            {
                return Some(devs.swap_remove(i));
            }
        }
    }
    None
}

pub fn haptic_test(args: &[String]) -> ExitCode {
    let filter = args.iter().find(|a| !a.starts_with('-')).map(String::as_str);
    let Some(mut d) = active_slot(filter) else {
        eprintln!("no connected controller found (is it on?)");
        return ExitCode::FAILURE;
    };
    println!("using slot {} ({})", d.info.slot_label(), d.info.dev.display());
    let _ = d.set_feature(&proto::settings_report(&[(proto::SETTING_LIZARD_MODE, proto::LIZARD_MODE_OFF)]));

    let rumble = |d: &mut HidDevice, l: u16, r: u16, ms: u64| {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            if let Err(e) = d.write_output(&proto::rumble_report(l, r)) {
                println!("   write failed: {e}");
                return;
            }
            sleep(Duration::from_millis(40));
        }
        let _ = d.write_output(&proto::rumble_report(0, 0));
    };
    let out = |d: &mut HidDevice, r: &[u8]| {
        if let Err(e) = d.write_output(r) {
            println!("   write failed: {e}");
        }
    };
    let step = |n: u32, what: &str| {
        println!("{n:2}. {what}");
        sleep(Duration::from_millis(300));
    };
    let pause = || sleep(Duration::from_millis(1200));

    step(1, "rumble report 0x80: LEFT (strong / low-frequency) motor at 50% for 0.8 s");
    rumble(&mut d, 0x8000, 0, 800);
    pause();
    step(2, "rumble report 0x80: RIGHT (weak / high-frequency) motor at 50% for 0.8 s");
    rumble(&mut d, 0, 0x8000, 800);
    pause();
    step(3, "rumble report 0x80: both motors at 100% for 0.8 s");
    rumble(&mut d, 0xffff, 0xffff, 800);
    pause();
    let sides = [(Side::Left, "left"), (Side::Right, "right"), (Side::Both, "both")];
    let mut n = 4;
    for (cmd, cname) in [(HapticCmd::Tick, "tick"), (HapticCmd::Click, "click")] {
        for (side, sname) in sides {
            step(n, &format!("haptic command 0x82: {cname} on side {} ({sname})", side as u8));
            out(&mut d, &proto::haptic_command_report(side, cmd, 0));
            pause();
            n += 1;
        }
    }
    step(n, "haptic pulse 0x81: right side, 10 pulses of 3 ms on / 60 ms off");
    out(&mut d, &proto::haptic_pulse_report(Side::Right, 3000, 60000, 10));
    pause();
    n += 1;
    step(n, "haptic command 0x82: tick, side right, gain -12 dB (softer)");
    out(&mut d, &proto::haptic_command_report(Side::Right, HapticCmd::Tick, -12));
    pause();
    let _ = d.set_feature(&proto::settings_report(&[(proto::SETTING_LIZARD_MODE, proto::LIZARD_MODE_ON)]));
    println!("done. Tell which steps you felt (and where) if something looks off.");
    ExitCode::SUCCESS
}

/// Show the overlay without a controller (for checking how it looks). Simulated trackpad
/// touches show both cursors; events (typed keys) are printed, not typed.
pub fn osk_preview(args: &[String]) -> ExitCode {
    use crate::osk::{Overlay, Tab, menu};
    if let Some(i) = args.iter().position(|a| a == "--script") {
        return match args.get(i + 1) {
            Some(path) => crate::demo::run(std::path::Path::new(path)),
            None => {
                eprintln!("--script needs a file");
                ExitCode::from(2)
            }
        };
    }
    let tab = match args
        .iter()
        .find(|a| a.parse::<u64>().is_err() && !matches!(a.as_str(), "prompt" | "capture" | "list" | "name" | "toast"))
        .map(String::as_str)
    {
        Some("settings") => Tab::Settings,
        Some("mapping") => Tab::Mapping,
        Some("presets") => Tab::Presets,
        Some("input") => Tab::Input,
        _ => Tab::Keyboard,
    };
    let secs: u64 = args.iter().find_map(|a| a.parse().ok()).unwrap_or(5);
    let mut ov = match Overlay::new() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let cfg = crate::config::Config::default();
    if args.iter().any(|a| a == "toast") {
        ov.toast(crate::osk::Toast {
            title: "Gamepad mode".into(),
            line: crate::osk::ToastLine::Hold { who: "Couch".into(), glyph: "≡".into(), next: "desktop mode".into() },
            secs: crate::osk::Toast::SHORT,
        });
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end && !sys::quit_requested() {
            let _ = sys::poll(&[ov.wake_fd()], Some(Duration::from_millis(100)));
            ov.drain();
        }
        return ExitCode::SUCCESS;
    }
    let preview_app = std::env::var("PUCKD_PREVIEW_APP").unwrap_or_else(|_| "Elden Ring".into());
    let store = crate::presets::Presets::load(&crate::presets::default_dir(), cfg.gamepad.clone());
    ov.open("preview", Some(tab));
    ov.status(
        "preview",
        menu::status(&menu::StatusCtx {
            cfg: &cfg,
            presets: &store,
            app: Some(("steam_app_42", &preview_app)),
            slot: "1",
            serial: Some("FXA00000000AA"),
            name: "Couch",
            mode: crate::config::Mode::Desktop,
            battery: Some((1, 88)),
            native: None,
        }),
    );
    if args.iter().any(|a| a == "prompt") {
        let shown = std::env::var("PUCKD_PREVIEW_APP").unwrap_or_else(|_| "Elden Ring".into());
        ov.ask("Racing", "steam_app_42", &shown);
    }
    let mut st = State {
        buttons: btn::LPAD_TOUCH | btn::RPAD_TOUCH | btn::A | btn::LB | btn::L4 | btn::LGRIP_TOUCH | btn::RSTICK_TOUCH,
        lt: 16000,
        rt: 32767,
        lx: -12000,
        ly: 20000,
        rx: 25000,
        report_id: 0x42,
        accel: [-600, 9900, 13500],
        gyro: [900, -300, 120],
        ..Default::default()
    };
    st.lpad_x = -8000;
    st.lpad_y = 4000;
    st.rpad_x = 12000;
    st.rpad_y = -6000;
    st.rpad_pressure = 14000;
    let end = Instant::now() + Duration::from_secs(secs);
    let mut first = true;
    while Instant::now() < end && !sys::quit_requested() {
        if first {
            ov.input(&State::default(), &State::default());
            // "capture" / "list": open the mapping editor on the first entry.
            match tab {
                // Only the two trackpad touches (LB/RB would switch tabs).
                Tab::Keyboard => {
                    st = State {
                        buttons: btn::LPAD_TOUCH | btn::RPAD_TOUCH,
                        lpad_x: st.lpad_x,
                        lpad_y: st.lpad_y,
                        rpad_x: st.rpad_x,
                        rpad_y: st.rpad_y,
                        ..Default::default()
                    }
                }
                Tab::Input => {}
                _ => st = State::default(),
            }
            for (arg, bit) in [("capture", btn::A), ("list", btn::X), ("name", btn::A)] {
                if args.iter().any(|a| a == arg) {
                    sleep(Duration::from_millis(50));
                    ov.input(&State::default(), &State::default());
                    sleep(Duration::from_millis(20));
                    let s = State { buttons: bit, ..Default::default() };
                    ov.input(&s, &s);
                    sleep(Duration::from_millis(20));
                    ov.input(&State::default(), &State::default());
                    st = State::default();
                }
            }
            sleep(Duration::from_millis(50));
            first = false;
        }
        ov.input(&st, &st);
        let _ = sys::poll(&[ov.wake_fd()], Some(Duration::from_millis(100)));
        for e in ov.drain() {
            match e {
                crate::osk::Event::Haptic { .. } => {}
                other => println!("{other:?}"),
            }
        }
    }
    ov.close();
    sleep(Duration::from_millis(100));
    ExitCode::SUCCESS
}

/// Create a profile's virtual pad without a controller; X and the L4 paddle blink and LT ramps,
/// to check how SDL/gilrs/browsers map it.
pub fn pad_preview(args: &[String]) -> ExitCode {
    let profile = match args.iter().find(|a| !a.starts_with('-') && a.parse::<u64>().is_err()).map(String::as_str) {
        Some("xbox-elite") => crate::config::Profile::XboxElite,
        Some("extended") => crate::config::Profile::Extended,
        _ => crate::config::Profile::Xbox360,
    };
    let secs: u64 = args.iter().find_map(|a| a.parse().ok()).unwrap_or(10);
    let cfg = crate::config::Gamepad { profile, ..Default::default() };
    let info = HidInfo {
        node: "preview".into(),
        dev: "/dev/null".into(),
        bus: 3,
        vid: 0x28de,
        pid: 0x1304,
        iface: Some(2),
        uniq: "PREVIEW".into(),
        phys: String::new(),
        name: String::new(),
    };
    let mut pad = match crate::gamepad::Gamepad::create(&cfg, &info) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{:?} pad {}: X and L4 blink, LT ramps, for {secs} s", profile, pad.sysname());
    let mut st = State { buttons: btn::X | btn::L4, ..Default::default() };
    let end = Instant::now() + Duration::from_secs(secs);
    let mut n = 0u32;
    while Instant::now() < end && !sys::quit_requested() {
        // LT ramps up and stays at full after 0.5 s (SDL ignores axes until they move).
        n += 1;
        st.lt = ((n % 25) * 1365).min(32767) as i16;
        st.buttons = if (n / 12).is_multiple_of(2) { btn::X | btn::L4 } else { 0 };
        let _ = pad.update(&st);
        let _ = pad.handle_client(Instant::now());
        sleep(Duration::from_millis(20));
    }
    ExitCode::SUCCESS
}

/// Turn a controller off (all connected ones, or the given slot).
pub fn power_off(args: &[String]) -> ExitCode {
    let filter = args.iter().find(|a| !a.starts_with('-')).map(String::as_str);
    let Some(mut d) = active_slot(filter) else {
        eprintln!("no connected controller found");
        return ExitCode::FAILURE;
    };
    match d.set_feature(&proto::power_off_report()) {
        Ok(()) => {
            println!("sent power-off to slot {}", d.info.slot_label());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Find out how the controller numbers its haptic sides: plays a tick and a click for each
/// raw side value; the user answers with the controller (LB left, RB right, both, A none).
pub fn haptic_sides(args: &[String]) -> ExitCode {
    use std::io::Write as _;
    let filter = args.iter().find(|a| !a.starts_with('-')).map(String::as_str);
    let Some(mut d) = active_slot(filter) else {
        eprintln!("no connected controller found (is it on?)");
        return ExitCode::FAILURE;
    };
    sys::install_signal_handlers();
    let lizard = proto::settings_report(&[(proto::SETTING_LIZARD_MODE, proto::LIZARD_MODE_OFF)]);
    let _ = d.set_feature(&lizard);
    let mut last_keepalive = Instant::now();
    let mut buf = [0u8; 64];
    // Read pending reports; returns the latest buttons seen.
    let mut poll_buttons = |d: &mut HidDevice, wait: Duration, last_keepalive: &mut Instant| -> Option<u32> {
        if last_keepalive.elapsed() >= Duration::from_secs(2) {
            let _ = d.set_feature(&lizard);
            *last_keepalive = Instant::now();
        }
        let _ = sys::poll(&[d.fd()], Some(wait));
        let mut b = None;
        while let Ok(Some(n)) = d.read_report(&mut buf) {
            if let Some(Input::State(st)) = proto::parse(&buf[..n]) {
                b = Some(st.buttons);
            }
        }
        b
    };
    println!("Hold the controller with both thumbs resting on the trackpads.");
    println!("After each effect, answer where you felt it:");
    println!("   LB = left pad    RB = right pad    LB + RB together = both    A = nothing\n");
    let tests: Vec<(u8, u8, &str)> = (0u8..=3).flat_map(|side| [(side, 1u8, "TICK"), (side, 2u8, "CLICK")]).collect();
    let mut results: Vec<(u8, &str, String)> = Vec::new();
    let answer_keys = btn::LB | btn::RB | btn::A;
    for (n, &(side, cmd, name)) in tests.iter().enumerate() {
        if sys::quit_requested() {
            break;
        }
        println!("── Test {}/{}: side value {side} · {name} ──", n + 1, tests.len());
        // Play the effect three times.
        for k in (1..=3).rev() {
            print!("\r   playing… {k} ");
            let _ = std::io::stdout().flush();
            let _ = d.write_output(&[proto::OUT_HAPTIC_COMMAND, side, cmd, 0]);
            let end = Instant::now() + Duration::from_millis(450);
            while Instant::now() < end {
                poll_buttons(&mut d, Duration::from_millis(20), &mut last_keepalive);
            }
        }
        // Wait for a chord of LB / RB / A: collect everything pressed until all are released.
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut chord = 0u32;
        let mut prev = 0u32;
        let mut answer = String::from("no answer");
        let mut shown = u64::MAX;
        while Instant::now() < deadline && !sys::quit_requested() {
            let left = deadline.saturating_duration_since(Instant::now()).as_secs() + 1;
            if left != shown {
                print!("\r   where did you feel it?  LB=left  RB=right  LB+RB=both  A=nothing   {left} s ");
                let _ = std::io::stdout().flush();
                shown = left;
            }
            let Some(b) = poll_buttons(&mut d, Duration::from_millis(20), &mut last_keepalive) else { continue };
            let b = b & answer_keys;
            chord |= b;
            if b == 0 && prev != 0 {
                answer = if chord & btn::A != 0 {
                    "nothing".into()
                } else if chord & btn::LB != 0 && chord & btn::RB != 0 {
                    "BOTH".into()
                } else if chord & btn::LB != 0 {
                    "LEFT".into()
                } else {
                    "RIGHT".into()
                };
                break;
            }
            prev = b;
        }
        println!("\r   → {answer:<70}");
        results.push((side, name, answer));
        sleep(Duration::from_millis(400));
    }
    let _ = d.set_feature(&proto::settings_report(&[(proto::SETTING_LIZARD_MODE, proto::LIZARD_MODE_ON)]));
    println!("\nSummary (side value → where it was felt):");
    println!("   {:<6} {:<10} {:<10}", "value", "TICK", "CLICK");
    for side in 0u8..=3 {
        let get =
            |name: &str| results.iter().find(|r| r.0 == side && r.1 == name).map(|r| r.2.clone()).unwrap_or_default();
        println!("   {:<6} {:<10} {:<10}", side, get("TICK"), get("CLICK"));
    }
    println!("\nPaste this table to Claude.");
    ExitCode::SUCCESS
}

//! puckd: userspace driver for the Steam Controller (2026) and its Puck dongle.

mod backoff;
mod config;
mod controller;
mod daemon;
mod demo;
mod desktop;
mod ff;
mod gamepad;
mod hid;
mod keys;
mod osk;
mod passthrough;
mod presets;
mod proto;
mod status;
mod study;
mod sys;
mod tools;
mod uinput;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
puckd - Steam Controller (2026) / Puck userspace driver

USAGE:
    puckd [run] [-c FILE] [-v|-vv]   run the daemon (default)
    puckd list                       list controller slots
    puckd dump [--raw] [--imu] [--keep-lizard] [SLOT]
                                     print decoded (or raw) reports. Lizard mode (the
                                     controller's built-in mouse/keyboard) is turned off
                                     while dumping unless --keep-lizard is given
    puckd haptic-test [SLOT]         play each rumble/haptic effect, with a description
    puckd power-off [SLOT]           turn a controller off
    puckd haptic-sides [SLOT]        play tick/click on each raw haptic side value (0..3)
    puckd mouse-study                guided recording session for tuning the trackpad mouse
    puckd osk-preview [settings|mapping|presets|input] [prompt|capture|list|name|toast] [SECS]
    puckd osk-preview --script FILE  drive the overlay with a scripted controller (docs/demo)
                                     show the on-screen keyboard overlay without a controller
    puckd pad-preview [PROFILE] [SECS]
                                     create a profile's virtual pad (X and L4 blink, LT ramps)
    puckd check-config [-c FILE]     validate the config file
    puckd uhid-clone [--bus usb|ble|virtual] [--pid HEX] [--log-input] all|hidrawN...
                                     experimental: clone hidraw nodes via uhid, forward both
                                     ways and log what Steam & co send (run as root)

OPTIONS:
    -c, --config FILE   config file (default: $XDG_CONFIG_HOME/puckd.toml)
    -v                  debug logging (-vv: trace); RUST_LOG also works
    -h, --help          this help
    -V, --version       version
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut cmd: Option<String> = None;
    let mut cfg_path: Option<PathBuf> = None;
    let mut verbose = 0;
    let mut rest: Vec<String> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("puckd {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "-c" | "--config" => match args.next() {
                Some(p) => cfg_path = Some(PathBuf::from(p)),
                None => {
                    eprintln!("--config needs a file");
                    return ExitCode::from(2);
                }
            },
            "-v" => verbose += 1,
            "-vv" => verbose += 2,
            _ if cmd.is_none() && !a.starts_with('-') => cmd = Some(a),
            _ => rest.push(a),
        }
    }

    let default_level = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_level))
        .format_timestamp_millis()
        .init();

    let cfg_path = cfg_path.unwrap_or_else(config::default_path);
    let load = || config::Config::load(&cfg_path);

    match cmd.as_deref().unwrap_or("run") {
        "run" => {
            let cfg = match load() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("config error: {e}");
                    return ExitCode::from(2);
                }
            };
            sys::install_signal_handlers();
            let mut d = match daemon::Daemon::new(cfg, cfg_path) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("cannot create virtual devices via /dev/uinput: {e}");
                    return ExitCode::FAILURE;
                }
            };
            d.run();
            ExitCode::SUCCESS
        }
        "list" => tools::list(),
        "dump" => {
            sys::install_signal_handlers();
            tools::dump(&rest)
        }
        "haptic-test" => tools::haptic_test(&rest),
        "power-off" => tools::power_off(&rest),
        "haptic-sides" => tools::haptic_sides(&rest),
        "replay" => match load() {
            Ok(c) => study::replay(c, &rest),
            Err(e) => {
                eprintln!("config error: {e}");
                ExitCode::from(2)
            }
        },
        "mouse-study" => match load() {
            Ok(c) => study::run(c, &rest),
            Err(e) => {
                eprintln!("config error: {e}");
                ExitCode::from(2)
            }
        },
        "pad-preview" => {
            sys::install_signal_handlers();
            tools::pad_preview(&rest)
        }
        "osk-preview" => {
            sys::install_signal_handlers();
            tools::osk_preview(&rest)
        }
        "uhid-clone" => {
            sys::install_signal_handlers();
            passthrough::run(&rest)
        }
        "check-config" => match load() {
            Ok(_) => {
                println!("{}: ok", cfg_path.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(2)
            }
        },
        other => {
            eprintln!("unknown command '{other}'\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

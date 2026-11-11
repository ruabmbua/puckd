//! Pass-through: a uhid clone of a connected controller for Steam and games.
//!
//! The clone presents itself as a *wired* Steam Controller (28de:1302), whatever the real
//! link is: SDL and Steam only accept dongle devices (1304/1305) on USB interfaces 2..5,
//! and a uhid device has no USB interface. The controller protocol is the same.
//!
//! puckd forwards the controller's reports to the clone and the clone users' output and
//! feature reports to the controller. Only the hand-over chord is held back.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use log::debug;

use crate::hid::{self, HidDevice, OWN_PHYS_PREFIX};
use crate::proto;
use crate::sys;
use crate::uhid::{self, Event};

/// Lizard-mode mouse and keyboard reports: never forwarded (the clone's own hid-generic
/// input devices would move the pointer a second time).
const LIZARD_REPORTS: [u8; 2] = [0x40, 0x41];

/// A pass-through clone of one controller.
pub struct Clone {
    dev: uhid::Device,
    pub phys: String,
    /// A client sent output or feature reports since we last looked.
    activity: bool,
}

impl Clone {
    pub fn create(hid: &HidDevice, serial: Option<&str>) -> io::Result<Self> {
        let descriptor = hid.report_descriptor()?;
        let phys = format!("{}/passthrough", hid.info.stable_id());
        debug_assert!(phys.starts_with(OWN_PHYS_PREFIX));
        // A dongle slot becomes a wired controller; wired and BLE controllers keep their
        // identity (BLE controllers are uhid devices already, which Steam accepts).
        let (bus, pid, name) = if hid.info.is_dongle() {
            (uhid::BUS_USB, hid::PID_TRITON_USB, "Valve Software Steam Controller")
        } else {
            (hid.info.bus, hid.info.pid, hid.info.name.as_str())
        };
        let dev = uhid::Device::create(
            uhid::open_file()?,
            &uhid::Params {
                name,
                phys: &phys,
                uniq: serial.unwrap_or(""),
                bus,
                vid: hid::VALVE_VID,
                pid,
                descriptor: &descriptor,
            },
        )?;
        Ok(Self { dev, phys, activity: false })
    }

    pub fn fd(&self) -> i32 {
        self.dev.fd()
    }

    /// The clone's hidraw node, once the kernel has made it.
    pub fn dev_path(&self) -> Option<PathBuf> {
        hid::node_by_phys(&self.phys).map(|n| Path::new("/dev").join(n))
    }

    /// Forward a report from the controller, with the state's buttons replaced by `buttons`
    /// (the chord hidden).
    pub fn forward(&mut self, report: &[u8], buttons: Option<u32>) {
        let Some(&id) = report.first() else { return };
        if LIZARD_REPORTS.contains(&id) {
            return;
        }
        let res = match buttons {
            Some(b)
                if matches!(id, proto::ID_STATE | proto::ID_STATE_BLE | proto::ID_STATE_TS) && report.len() >= 6 =>
            {
                let mut r = report.to_vec();
                r[2..6].copy_from_slice(&b.to_le_bytes());
                self.dev.input(&r)
            }
            _ => self.dev.input(report),
        };
        if let Err(e) = res {
            debug!("pass-through input: {e}");
        }
    }

    /// Forward a report with the controller at rest: no buttons, sticks, triggers, trackpad
    /// touches or motion (desktop mode, overlay open). Other reports go through as they are.
    pub fn forward_neutral(&mut self, report: &[u8]) {
        let Some(&id) = report.first() else { return };
        if !matches!(id, proto::ID_STATE | proto::ID_STATE_BLE | proto::ID_STATE_TS) || report.len() < 46 {
            return self.forward(report, None);
        }
        let mut r = report.to_vec();
        neutralize(&mut r);
        if let Err(e) = self.dev.input(&r) {
            debug!("pass-through input: {e}");
        }
    }

    /// Serve requests from the clone's users against the real controller.
    pub fn serve(&mut self, hid: &mut HidDevice) -> io::Result<()> {
        while let Some(ev) = self.dev.read_event()? {
            match ev {
                Event::Output { rtype, data } => {
                    self.activity = true;
                    let r = if rtype == uhid::FEATURE_REPORT {
                        hid.set_report(rtype, &data)
                    } else {
                        hid.write_output(&data)
                    };
                    if let Err(e) = r {
                        debug!("pass-through output {}: {e}", hex(&data));
                    }
                }
                Event::GetReport { id, rnum, rtype } => {
                    let mut buf = [0u8; proto::FEATURE_REPORT_LEN + 1];
                    buf[0] = rnum;
                    match hid.get_report(rtype, &mut buf) {
                        Ok(n) => self.dev.get_report_reply(id, Ok(&buf[..n.min(buf.len())]))?,
                        Err(e) => self.dev.get_report_reply(id, Err(e.raw_os_error().unwrap_or(libc::EIO)))?,
                    }
                }
                Event::SetReport { id, rtype, data, .. } => {
                    self.activity = true;
                    let errno = match hid.set_report(rtype, &data) {
                        Ok(()) => 0,
                        Err(e) => e.raw_os_error().unwrap_or(libc::EIO),
                    };
                    self.dev.set_report_reply(id, errno)?;
                }
                Event::Start | Event::Stop | Event::Open | Event::Close | Event::Other(_) => {}
            }
        }
        Ok(())
    }

    /// Whether a client sent anything since the last call.
    pub fn take_activity(&mut self) -> bool {
        std::mem::take(&mut self.activity)
    }

    /// Who holds the clone open: (name, is Steam), Steam preferred. None when nobody does.
    pub fn holder(&self) -> Option<(String, bool)> {
        let dev = self.dev_path()?;
        let openers = crate::backoff::openers(std::slice::from_ref(&dev));
        let list = openers.get(&dev)?;
        let o = list.iter().find(|o| o.is_steam()).or_else(|| list.first())?;
        Some(if o.is_steam() { ("Steam".into(), true) } else { (o.comm.clone(), false) })
    }
}

/// Zero the inputs of a state report (0x42/0x45/0x47), keeping sequence and timestamps.
fn neutralize(r: &mut [u8]) {
    // buttons, triggers, sticks
    r[2..18].fill(0);
    // trackpads (0x47 has a u16 trackpad timestamp first)
    let p = if r[0] == proto::ID_STATE_TS { 20 } else { 18 };
    r[p..p + 12].fill(0);
    // accelerometer, gyro
    r[34..46].fill(0);
}

const CHORD_HOLD: Duration = Duration::from_millis(1000);
/// How long a single chord button is held back, waiting for the other.
const CHORD_WINDOW: Duration = Duration::from_millis(80);
/// A held-back button released within the window is replayed as a press this long.
const CHORD_TAP: Duration = Duration::from_millis(60);

/// The hand-over chord (e.g. ⧉ + ≡ held for a second). Its buttons are hidden from
/// everyone else (puckd's own modes and the pass-through clone) while it is pressed.
#[derive(Default)]
pub struct Chord {
    bits: u32,
    held_since: Option<Instant>,
    fired: bool,
    /// One chord button went down alone: (when, buttons seen), held back for the window.
    pending: Option<(Instant, u32)>,
    /// Replay of a held-back press: (until, buttons).
    tap: Option<(Instant, u32)>,
}

impl Chord {
    pub fn new(bits: u32) -> Self {
        Self { bits, ..Default::default() }
    }

    /// Filter a state's buttons: returns (buttons for everyone else, chord fired now).
    pub fn apply(&mut self, buttons: u32, now: Instant) -> (u32, bool) {
        let bits = self.bits;
        if bits.count_ones() < 2 {
            return (buttons, false);
        }
        let down = buttons & bits;
        let mut hide = 0;
        let mut fire = false;
        if down == bits {
            let since = *self.held_since.get_or_insert(now);
            self.pending = None;
            hide = bits;
            if !self.fired && now.duration_since(since) >= CHORD_HOLD {
                self.fired = true;
                fire = true;
            }
        } else if self.held_since.is_some() {
            // Chord let go: keep hiding until every chord button is up.
            hide = down;
            if down == 0 {
                self.held_since = None;
                self.fired = false;
            }
        } else if down != 0 {
            let (t, seen) = self.pending.get_or_insert((now, 0));
            *seen |= down;
            if now.duration_since(*t) < CHORD_WINDOW {
                hide = down;
            }
        } else if let Some((t, seen)) = self.pending.take()
            && now.duration_since(t) < CHORD_WINDOW
        {
            // Released while still held back: replay it as a short press.
            self.tap = Some((now + CHORD_TAP, seen));
        }
        let mut out = buttons & !hide;
        if let Some((until, b)) = self.tap {
            if now < until {
                out |= b;
            } else {
                self.tap = None;
            }
        }
        (out, fire)
    }
}

fn hex(b: &[u8]) -> String {
    // Trailing zeros are noise in 64-byte reports.
    let end = b.iter().rposition(|&x| x != 0).map_or(1.min(b.len()), |p| p + 1);
    let mut s: String = b[..end].iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ");
    if end < b.len() {
        s.push_str(&format!(" (+{} zero)", b.len() - end));
    }
    s
}

fn errno_of(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(libc::EIO)
}

/// `puckd uhid-clone`: debug tool. Clones hidraw nodes as uhid devices with the same (or
/// overridden) identity and forwards everything both ways, logging what clients send.
/// Run as root with the daemon stopped.
pub fn run(args: &[String]) -> ExitCode {
    let mut bus: Option<u16> = None;
    let mut pid: Option<u16> = None;
    let mut log_input = false;
    let mut nodes: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bus" => {
                bus = match it.next().map(String::as_str) {
                    Some("usb") => Some(uhid::BUS_USB),
                    Some("ble" | "bt") => Some(uhid::BUS_BLUETOOTH),
                    Some("virtual") => Some(uhid::BUS_VIRTUAL),
                    _ => {
                        eprintln!("--bus usb|ble|virtual");
                        return ExitCode::from(2);
                    }
                }
            }
            "--pid" => match it.next().and_then(|p| u16::from_str_radix(p.trim_start_matches("0x"), 16).ok()) {
                Some(p) => pid = Some(p),
                None => {
                    eprintln!("--pid HEX");
                    return ExitCode::from(2);
                }
            },
            "--log-input" => log_input = true,
            "all" => {
                let mut v: Vec<String> = fs::read_dir("/sys/class/hidraw")
                    .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                    .unwrap_or_default();
                v.retain(|n| hid::is_controller_node(n));
                v.sort_by_key(|n| n.trim_start_matches("hidraw").parse::<u32>().unwrap_or(0));
                nodes.extend(v);
            }
            n => nodes.push(n.trim_start_matches("/dev/").to_string()),
        }
    }
    if nodes.is_empty() {
        eprintln!("usage: puckd uhid-clone [--bus usb|ble|virtual] [--pid HEX] [--log-input] all | hidrawN...");
        return ExitCode::from(2);
    }

    struct Tap {
        label: String,
        real: fs::File,
        dev: uhid::Device,
        inputs: u64,
    }
    let open = |n: &str| -> io::Result<Tap> {
        let sysd = Path::new("/sys/class/hidraw").join(n);
        let (id, uniq, phys, name) = hid::parse_uevent(&fs::read_to_string(sysd.join("device/uevent"))?);
        let (b, v, p) = id.ok_or_else(|| io::Error::other("no HID_ID"))?;
        let desc = fs::read(sysd.join("device/report_descriptor"))?;
        let real = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(Path::new("/dev").join(n))?;
        let phys = format!("{OWN_PHYS_PREFIX}uhid-clone/{phys}");
        let (cb, cp) = (bus.unwrap_or(b), pid.unwrap_or(p));
        let dev = uhid::Device::create(
            uhid::open_file()?,
            &uhid::Params { name: &name, phys: &phys, uniq: &uniq, bus: cb, vid: v, pid: cp, descriptor: &desc },
        )?;
        println!(
            "{n}: {v:04x}:{p:04x} bus {b:#04x} cloned as {v:04x}:{cp:04x} bus {cb:#04x} \"{name}\" ({} byte descriptor)",
            desc.len()
        );
        Ok(Tap { label: n.to_string(), real, dev, inputs: 0 })
    };
    let mut taps = Vec::new();
    for n in &nodes {
        match open(n) {
            Ok(t) => taps.push(t),
            Err(e) => {
                eprintln!("{n}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let t0 = Instant::now();
    let log = |label: &str, msg: &str| println!("{:>9.3} {label}: {msg}", t0.elapsed().as_secs_f64());
    println!("forwarding; Ctrl-C to stop (the clones disappear)");
    let mut last_stats = t0;
    while !sys::quit_requested() {
        let fds: Vec<i32> = taps.iter().flat_map(|t| [t.real.as_raw_fd(), t.dev.fd()]).collect();
        let Ok(rev) = sys::poll(&fds, Some(Duration::from_secs(1))) else { break };
        for (i, t) in taps.iter_mut().enumerate() {
            if rev[2 * i] != 0 {
                let mut buf = [0u8; uhid::DATA_MAX];
                loop {
                    let n = match t.real.read(&mut buf) {
                        Ok(n) => n,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            log(&t.label, &format!("real device: {e}"));
                            return ExitCode::FAILURE;
                        }
                    };
                    t.inputs += 1;
                    if log_input || !matches!(buf[0], 0x42 | 0x45 | 0x47) {
                        log(&t.label, &format!("in  {}", hex(&buf[..n])));
                    }
                    let _ = t.dev.input(&buf[..n]);
                }
            }
            if rev[2 * i + 1] == 0 {
                continue;
            }
            loop {
                let ev = match t.dev.read_event() {
                    Ok(Some(ev)) => ev,
                    Ok(None) => break,
                    Err(e) => {
                        log(&t.label, &format!("uhid: {e}"));
                        return ExitCode::FAILURE;
                    }
                };
                let fd = t.real.as_raw_fd();
                let ioc = |get: bool, rtype: u8, buf: &mut [u8]| -> io::Result<usize> {
                    let nr = match (get, rtype) {
                        (false, uhid::INPUT_REPORT) => 0x09,
                        (true, uhid::INPUT_REPORT) => 0x0a,
                        (false, uhid::OUTPUT_REPORT) => 0x0b,
                        (true, uhid::OUTPUT_REPORT) => 0x0c,
                        (false, _) => 0x06,
                        (true, _) => 0x07,
                    };
                    let req = sys::ioc(sys::IOC_WRITE | sys::IOC_READ, b'H', nr, buf.len());
                    unsafe { sys::ioctl(fd, req, buf.as_mut_ptr()) }.map(|n| n as usize)
                };
                let name = uhid::rtype_name;
                match ev {
                    Event::Output { rtype, data } => {
                        let note = t.real.write_all(&data).err().map(|e| format!("  -> error {e}")).unwrap_or_default();
                        log(&t.label, &format!("out {} {}{note}", name(rtype), hex(&data)));
                    }
                    Event::GetReport { id, rnum, rtype } => {
                        let mut buf = [0u8; proto::FEATURE_REPORT_LEN + 1];
                        buf[0] = rnum;
                        match ioc(true, rtype, &mut buf) {
                            Ok(n) => {
                                let n = n.min(buf.len());
                                log(&t.label, &format!("get {} {rnum:#04x} -> {}", name(rtype), hex(&buf[..n])));
                                let _ = t.dev.get_report_reply(id, Ok(&buf[..n]));
                            }
                            Err(e) => {
                                log(&t.label, &format!("get {} {rnum:#04x} -> error {e}", name(rtype)));
                                let _ = t.dev.get_report_reply(id, Err(errno_of(&e)));
                            }
                        }
                    }
                    Event::SetReport { id, rnum, rtype, mut data } => {
                        let r = ioc(false, rtype, &mut data);
                        let note = r.as_ref().err().map(|e| format!("  -> error {e}")).unwrap_or_default();
                        log(&t.label, &format!("set {} {rnum:#04x} {}{note}", name(rtype), hex(&data)));
                        let _ = t.dev.set_report_reply(id, r.err().map_or(0, |e| errno_of(&e)));
                    }
                    other => log(&t.label, &format!("{other:?}")),
                }
            }
        }
        if last_stats.elapsed() >= Duration::from_secs(10) {
            last_stats = Instant::now();
            let s: Vec<String> = taps.iter().map(|t| format!("{} {} reports", t.label, t.inputs)).collect();
            println!("{:>9.3} stats: {}", t0.elapsed().as_secs_f64(), s.join(", "));
        }
    }
    println!("stopping");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::btn;

    const C: u32 = btn::VIEW | btn::MENU;

    fn ms(t: Instant, n: u64) -> Instant {
        t + Duration::from_millis(n)
    }

    #[test]
    fn chord_fires_after_hold_and_stays_hidden() {
        let mut c = Chord::new(C);
        let t = Instant::now();
        assert_eq!(c.apply(C | btn::A, t), (btn::A, false));
        assert_eq!(c.apply(C, ms(t, 500)), (0, false));
        assert_eq!(c.apply(C, ms(t, 1001)), (0, true));
        assert_eq!(c.apply(C, ms(t, 1500)), (0, false));
        // Released one by one: no stray press.
        assert_eq!(c.apply(btn::MENU, ms(t, 1600)), (0, false));
        assert_eq!(c.apply(0, ms(t, 1700)), (0, false));
        // A new single press is held back again.
        assert_eq!(c.apply(btn::MENU, ms(t, 1800)).0, 0);
    }

    #[test]
    fn single_button_passes_after_window() {
        let mut c = Chord::new(C);
        let t = Instant::now();
        assert_eq!(c.apply(btn::MENU, t).0, 0);
        assert_eq!(c.apply(btn::MENU, ms(t, 50)).0, 0);
        assert_eq!(c.apply(btn::MENU, ms(t, 90)).0, btn::MENU);
        assert_eq!(c.apply(0, ms(t, 200)).0, 0);
    }

    #[test]
    fn quick_tap_is_replayed() {
        let mut c = Chord::new(C);
        let t = Instant::now();
        assert_eq!(c.apply(btn::VIEW, t).0, 0);
        assert_eq!(c.apply(0, ms(t, 30)).0, btn::VIEW);
        assert_eq!(c.apply(0, ms(t, 60)).0, btn::VIEW);
        assert_eq!(c.apply(0, ms(t, 100)).0, 0);
    }

    #[test]
    fn neutral_report_keeps_only_seq_and_timestamps() {
        let mut r = vec![0xffu8; 46];
        r[0] = proto::ID_STATE;
        neutralize(&mut r);
        assert_eq!(r[1], 0xff);
        assert!(r[2..30].iter().all(|&b| b == 0));
        assert!(r[30..34].iter().all(|&b| b == 0xff)); // IMU timestamp
        assert!(r[34..46].iter().all(|&b| b == 0));
        let st = match proto::parse(&r) {
            Some(proto::Input::State(s)) => s,
            _ => panic!(),
        };
        assert_eq!((st.buttons, st.lx, st.rpad_pressure, st.gyro), (0, 0, 0, [0; 3]));
    }

    #[test]
    fn one_button_chord_is_off() {
        let mut c = Chord::new(btn::MENU);
        assert_eq!(c.apply(btn::MENU, Instant::now()), (btn::MENU, false));
    }
}

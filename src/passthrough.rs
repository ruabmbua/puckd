//! `puckd uhid-clone`: spike for the pass-through mode. Clones Valve hidraw nodes as uhid
//! devices and forwards everything both ways, logging what the other side (Steam, SDL) sends.
//!
//! Run as root (for /dev/uhid and for the real nodes once the user's access is taken
//! away), with puckd's daemon stopped:
//!
//! ```sh
//! sudo setfacl -b /dev/hidraw11 ...             # Steam (your user) loses the real device
//! sudo puckd uhid-clone all                     # clones appear with your user's access
//! sudo udevadm trigger                          # afterwards: restore the real nodes' ACLs
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use crate::hid::{self, parse_report_descriptor};
use crate::sys;

// <linux/uhid.h>
const UHID_DESTROY: u32 = 1;
const UHID_START: u32 = 2;
const UHID_STOP: u32 = 3;
const UHID_OPEN: u32 = 4;
const UHID_CLOSE: u32 = 5;
const UHID_OUTPUT: u32 = 6;
const UHID_GET_REPORT: u32 = 9;
const UHID_GET_REPORT_REPLY: u32 = 10;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT: u32 = 13;
const UHID_SET_REPORT_REPLY: u32 = 14;

const UHID_FEATURE_REPORT: u8 = 0;
const UHID_OUTPUT_REPORT: u8 = 1;
const UHID_INPUT_REPORT: u8 = 2;

const DATA_MAX: usize = 4096;
/// sizeof(struct uhid_event): u32 type + the largest union member (uhid_create2_req).
const EVENT_SIZE: usize = 4 + 128 + 64 + 64 + 2 + 2 + 4 * 4 + DATA_MAX;

const BUS_USB: u16 = 0x03;
const BUS_BLUETOOTH: u16 = 0x05;
const BUS_VIRTUAL: u16 = 0x06;

// <linux/hidraw.h> report ioctls, by report type: (get, set).
const fn hidioc(nr: u32, len: usize) -> u64 {
    sys::ioc(sys::IOC_WRITE | sys::IOC_READ, b'H', nr, len)
}
fn get_nr(rtype: u8) -> u32 {
    match rtype {
        UHID_INPUT_REPORT => 0x0a,
        UHID_OUTPUT_REPORT => 0x0c,
        _ => 0x07,
    }
}
fn set_nr(rtype: u8) -> u32 {
    match rtype {
        UHID_INPUT_REPORT => 0x09,
        UHID_OUTPUT_REPORT => 0x0b,
        _ => 0x06,
    }
}

fn rtype_name(t: u8) -> &'static str {
    match t {
        UHID_FEATURE_REPORT => "feature",
        UHID_OUTPUT_REPORT => "output",
        UHID_INPUT_REPORT => "input",
        _ => "?",
    }
}

fn hex(b: &[u8]) -> String {
    // Trailing zeros are noise in 64-byte reports.
    let end = b.iter().rposition(|&x| x != 0).map_or(0, |p| p + 1).max(1.min(b.len()));
    let mut s: String = b[..end].iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ");
    if end < b.len() {
        s.push_str(&format!(" (+{} zero)", b.len() - end));
    }
    s
}

fn put_str(buf: &mut [u8], s: &str) {
    let n = s.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&s.as_bytes()[..n]);
}

struct Opts {
    bus: Option<u16>,
    pid: Option<u16>,
    log_input: bool,
}

struct Clone {
    real_path: PathBuf,
    real: File,
    uhid: File,
    label: String,
    /// Report sizes per (type, id), for sizing get-report buffers.
    feature_len: Vec<(u8, usize)>,
    opened: bool,
    inputs: u64,
}

impl Clone {
    fn create(node: &str, o: &Opts) -> io::Result<Self> {
        let sys = Path::new("/sys/class/hidraw").join(node);
        let uevent = fs::read_to_string(sys.join("device/uevent"))?;
        let (id, uniq, phys, name) = hid::parse_uevent(&uevent);
        let (bus, vid, pid) = id.ok_or_else(|| io::Error::other("no HID_ID"))?;
        let desc = fs::read(sys.join("device/report_descriptor"))?;
        let real_path = Path::new("/dev").join(node);
        let real = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&real_path)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", real_path.display())))?;
        let uhid = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open("/dev/uhid")
            .map_err(|e| io::Error::new(e.kind(), format!("/dev/uhid: {e} (run as root)")))?;

        let (cbus, cpid) = (o.bus.unwrap_or(bus), o.pid.unwrap_or(pid));
        let mut ev = vec![0u8; EVENT_SIZE];
        ev[0..4].copy_from_slice(&UHID_CREATE2.to_ne_bytes());
        let r = &mut ev[4..];
        put_str(&mut r[0..128], &name);
        put_str(&mut r[128..192], &phys);
        put_str(&mut r[192..256], &uniq);
        r[256..258].copy_from_slice(&(desc.len() as u16).to_ne_bytes());
        r[258..260].copy_from_slice(&cbus.to_ne_bytes());
        r[260..264].copy_from_slice(&(vid as u32).to_ne_bytes());
        r[264..268].copy_from_slice(&(cpid as u32).to_ne_bytes());
        // version, country: 0
        r[276..276 + desc.len()].copy_from_slice(&desc);
        let mut c = Self {
            real_path,
            real,
            uhid,
            label: node.to_string(),
            feature_len: parse_report_descriptor(&desc).feature,
            opened: false,
            inputs: 0,
        };
        c.uhid.write_all(&ev)?;
        println!(
            "{node}: cloned {:04x}:{:04x} bus {bus:#04x} as {:04x}:{:04x} bus {cbus:#04x}  name \"{name}\" phys \"{phys}\" uniq \"{uniq}\" ({} byte descriptor)",
            vid,
            pid,
            vid,
            cpid,
            desc.len()
        );
        Ok(c)
    }

    fn log(&self, t0: Instant, msg: &str) {
        println!("{:>9.3} {}: {msg}", t0.elapsed().as_secs_f64(), self.label);
    }

    /// Real device → clone.
    fn pump_input(&mut self, t0: Instant, o: &Opts) -> io::Result<()> {
        let mut buf = [0u8; DATA_MAX];
        loop {
            let n = match self.real.read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            };
            self.inputs += 1;
            // State reports stream constantly; log everything else (wireless status, battery...).
            let state = matches!(buf.first(), Some(0x42 | 0x45 | 0x47));
            if o.log_input || !state {
                self.log(t0, &format!("in  {}", hex(&buf[..n])));
            }
            let mut ev = vec![0u8; EVENT_SIZE];
            ev[0..4].copy_from_slice(&UHID_INPUT2.to_ne_bytes());
            ev[4..6].copy_from_slice(&(n as u16).to_ne_bytes());
            ev[6..6 + n].copy_from_slice(&buf[..n]);
            if let Err(e) = self.uhid.write_all(&ev) {
                self.log(t0, &format!("uhid input failed: {e}"));
            }
        }
    }

    fn report_len(&self, rtype: u8, rnum: u8) -> usize {
        let base = if rtype == UHID_FEATURE_REPORT {
            self.feature_len.iter().find(|&&(i, _)| i == rnum).map(|&(_, l)| l).unwrap_or(64)
        } else {
            64
        };
        base + 1
    }

    /// Clone → real device (requests from Steam & co).
    fn pump_uhid(&mut self, t0: Instant) -> io::Result<()> {
        let mut ev = vec![0u8; EVENT_SIZE];
        loop {
            match self.uhid.read(&mut ev) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            }
            let ty = u32::from_ne_bytes(ev[0..4].try_into().unwrap());
            let r = &ev[4..];
            match ty {
                UHID_START => self.log(t0, "start"),
                UHID_STOP => self.log(t0, "stop"),
                UHID_OPEN => {
                    self.opened = true;
                    self.log(t0, "opened (someone opened the clone)");
                }
                UHID_CLOSE => {
                    self.opened = false;
                    self.log(t0, "closed");
                }
                UHID_OUTPUT => {
                    let size = u16::from_ne_bytes(r[DATA_MAX..DATA_MAX + 2].try_into().unwrap()) as usize;
                    let rtype = r[DATA_MAX + 2];
                    let data = &r[..size.min(DATA_MAX)];
                    let res = self.real.write(data).map(|_| ());
                    self.log(t0, &format!("out {} {}{}", rtype_name(rtype), hex(data), err_note(&res)));
                }
                UHID_GET_REPORT => {
                    let id = u32::from_ne_bytes(r[0..4].try_into().unwrap());
                    let (rnum, rtype) = (r[4], r[5]);
                    let mut buf = vec![0u8; self.report_len(rtype, rnum)];
                    buf[0] = rnum;
                    let res = unsafe {
                        sys::ioctl(self.real.as_raw_fd(), hidioc(get_nr(rtype), buf.len()), buf.as_mut_ptr())
                    };
                    let mut rep = vec![0u8; EVENT_SIZE];
                    rep[0..4].copy_from_slice(&UHID_GET_REPORT_REPLY.to_ne_bytes());
                    rep[4..8].copy_from_slice(&id.to_ne_bytes());
                    match &res {
                        Ok(n) => {
                            let n = (*n as usize).min(buf.len());
                            rep[10..12].copy_from_slice(&(n as u16).to_ne_bytes());
                            rep[12..12 + n].copy_from_slice(&buf[..n]);
                            self.log(t0, &format!("get {} {rnum:#04x} -> {}", rtype_name(rtype), hex(&buf[..n])));
                        }
                        Err(e) => {
                            let errno = e.raw_os_error().unwrap_or(libc::EIO) as u16;
                            rep[8..10].copy_from_slice(&errno.to_ne_bytes());
                            self.log(t0, &format!("get {} {rnum:#04x} -> error {e}", rtype_name(rtype)));
                        }
                    }
                    self.uhid.write_all(&rep)?;
                }
                UHID_SET_REPORT => {
                    let id = u32::from_ne_bytes(r[0..4].try_into().unwrap());
                    let (rnum, rtype) = (r[4], r[5]);
                    let size = u16::from_ne_bytes(r[6..8].try_into().unwrap()) as usize;
                    let mut data = r[8..8 + size.min(DATA_MAX)].to_vec();
                    let res = unsafe {
                        sys::ioctl(self.real.as_raw_fd(), hidioc(set_nr(rtype), data.len()), data.as_mut_ptr())
                    }
                    .map(|_| ());
                    self.log(t0, &format!("set {} {rnum:#04x} {}{}", rtype_name(rtype), hex(&data), err_note(&res)));
                    let mut rep = vec![0u8; EVENT_SIZE];
                    rep[0..4].copy_from_slice(&UHID_SET_REPORT_REPLY.to_ne_bytes());
                    rep[4..8].copy_from_slice(&id.to_ne_bytes());
                    if let Err(e) = &res {
                        let errno = e.raw_os_error().unwrap_or(libc::EIO) as u16;
                        rep[8..10].copy_from_slice(&errno.to_ne_bytes());
                    }
                    self.uhid.write_all(&rep)?;
                }
                other => self.log(t0, &format!("uhid event {other}")),
            }
        }
    }
}

impl Drop for Clone {
    fn drop(&mut self) {
        let mut ev = vec![0u8; EVENT_SIZE];
        ev[0..4].copy_from_slice(&UHID_DESTROY.to_ne_bytes());
        let _ = self.uhid.write_all(&ev);
    }
}

fn err_note(r: &io::Result<()>) -> String {
    match r {
        Ok(()) => String::new(),
        Err(e) => format!("  -> error {e}"),
    }
}

/// Every Valve hidraw node of the supported devices (all dongle interfaces, not only slots).
fn valve_nodes() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/sys/class/hidraw") else { return out };
    for e in rd.flatten() {
        let node = e.file_name().to_string_lossy().into_owned();
        let Ok(uevent) = fs::read_to_string(e.path().join("device/uevent")) else { continue };
        if let (Some((_, hid::VALVE_VID, pid)), ..) = hid::parse_uevent(&uevent)
            && matches!(pid, hid::PID_TRITON_USB | hid::PID_TRITON_BLE | hid::PID_PROTEUS_DONGLE | hid::PID_NEREID_DONGLE)
            // Not our own clones from an earlier run.
            && !e.path().join("device").canonicalize().is_ok_and(|p| p.to_string_lossy().contains("/virtual/misc/uhid/"))
        {
            out.push(node);
        }
    }
    out.sort_by_key(|n| n.trim_start_matches("hidraw").parse::<u32>().unwrap_or(0));
    out
}

pub fn run(args: &[String]) -> ExitCode {
    let mut o = Opts { bus: None, pid: None, log_input: false };
    let mut nodes: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bus" => {
                o.bus = match it.next().map(String::as_str) {
                    Some("usb") => Some(BUS_USB),
                    Some("ble" | "bt") => Some(BUS_BLUETOOTH),
                    Some("virtual") => Some(BUS_VIRTUAL),
                    _ => {
                        eprintln!("--bus usb|ble|virtual");
                        return ExitCode::from(2);
                    }
                }
            }
            "--pid" => match it.next().and_then(|p| u16::from_str_radix(p.trim_start_matches("0x"), 16).ok()) {
                Some(p) => o.pid = Some(p),
                None => {
                    eprintln!("--pid HEX");
                    return ExitCode::from(2);
                }
            },
            "--log-input" => o.log_input = true,
            "all" => nodes.extend(valve_nodes()),
            n => nodes.push(n.trim_start_matches("/dev/").to_string()),
        }
    }
    if nodes.is_empty() {
        eprintln!("usage: puckd uhid-clone [--bus usb|ble|virtual] [--pid HEX] [--log-input] all | hidrawN...");
        return ExitCode::from(2);
    }
    let mut clones = Vec::new();
    for n in &nodes {
        match Clone::create(n, &o) {
            Ok(c) => clones.push(c),
            Err(e) => {
                eprintln!("{n}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let t0 = Instant::now();
    let mut last_stats = t0;
    println!("forwarding; Ctrl-C to stop (the clones disappear)");
    while !sys::quit_requested() {
        let fds: Vec<i32> = clones.iter().flat_map(|c| [c.real.as_raw_fd(), c.uhid.as_raw_fd()]).collect();
        let rev = match sys::poll(&fds, Some(Duration::from_secs(1))) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("poll: {e}");
                return ExitCode::FAILURE;
            }
        };
        for (i, c) in clones.iter_mut().enumerate() {
            if rev[2 * i] != 0
                && let Err(e) = c.pump_input(t0, &o)
            {
                c.log(t0, &format!("real device {}: {e}", c.real_path.display()));
                return ExitCode::FAILURE;
            }
            if rev[2 * i + 1] != 0
                && let Err(e) = c.pump_uhid(t0)
            {
                c.log(t0, &format!("uhid: {e}"));
                return ExitCode::FAILURE;
            }
        }
        if last_stats.elapsed() >= Duration::from_secs(10) {
            last_stats = Instant::now();
            let s: Vec<String> = clones
                .iter()
                .map(|c| format!("{} {} reports{}", c.label, c.inputs, if c.opened { " (open)" } else { "" }))
                .collect();
            println!("{:>9.3} stats: {}", t0.elapsed().as_secs_f64(), s.join(", "));
        }
    }
    println!("stopping");
    ExitCode::SUCCESS
}

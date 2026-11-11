//! hidraw discovery and I/O.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::sys;

pub const VALVE_VID: u16 = 0x28de;
pub const PID_TRITON_USB: u16 = 0x1302;
pub const PID_TRITON_BLE: u16 = 0x1303;
pub const PID_PROTEUS_DONGLE: u16 = 0x1304;
pub const PID_NEREID_DONGLE: u16 = 0x1305;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidInfo {
    /// e.g. "hidraw11"
    pub node: String,
    /// e.g. "/dev/hidraw11"
    pub dev: PathBuf,
    pub bus: u16,
    pub vid: u16,
    pub pid: u16,
    /// USB interface number, if known (dongle slots are interfaces 2..=5).
    pub iface: Option<u8>,
    pub uniq: String,
    pub phys: String,
    pub name: String,
}

impl HidInfo {
    pub fn is_dongle(&self) -> bool {
        matches!(self.pid, PID_PROTEUS_DONGLE | PID_NEREID_DONGLE)
    }

    /// Human slot label: "1".."4" for dongle slots, otherwise "usb"/"ble".
    pub fn slot_label(&self) -> String {
        match (self.is_dongle(), self.iface) {
            (true, Some(i)) => format!("{}", i.saturating_sub(1)),
            _ if self.pid == PID_TRITON_BLE => "ble".into(),
            _ => "usb".into(),
        }
    }

    /// Stable identifier for virtual devices of this slot (goes into `phys`).
    pub fn stable_id(&self) -> String {
        format!("puckd/{}/{}", if self.uniq.is_empty() { &self.node } else { &self.uniq }, self.slot_label())
    }
}

/// Report IDs and sizes (without the ID byte) found in a report descriptor.
#[derive(Debug, Default)]
pub struct ReportIds {
    pub input: Vec<(u8, usize)>,
    pub output: Vec<(u8, usize)>,
    pub feature: Vec<(u8, usize)>,
}

impl ReportIds {
    pub fn has_input(&self, id: u8) -> bool {
        self.input.iter().any(|&(i, _)| i == id)
    }
    pub fn has_output(&self, id: u8) -> bool {
        self.output.iter().any(|&(i, _)| i == id)
    }
}

/// Minimal HID report descriptor walk: collects report IDs and their byte sizes.
pub fn parse_report_descriptor(d: &[u8]) -> ReportIds {
    use std::collections::BTreeMap;
    let mut acc: BTreeMap<(u8, u8), usize> = BTreeMap::new();
    let (mut rid, mut size, mut count) = (0u8, 0usize, 0usize);
    let mut i = 0;
    while i < d.len() {
        let b = d[i];
        if b == 0xfe {
            // long item
            let len = *d.get(i + 1).unwrap_or(&0) as usize;
            i += 3 + len;
            continue;
        }
        let sz = [0, 1, 2, 4][(b & 3) as usize];
        let mut v: u32 = 0;
        for k in 0..sz {
            v |= (*d.get(i + 1 + k).unwrap_or(&0) as u32) << (8 * k);
        }
        i += 1 + sz;
        match b & 0xfc {
            0x84 => rid = v as u8,
            0x74 => size = v as usize,
            0x94 => count = v as usize,
            t @ (0x80 | 0x90 | 0xb0) => *acc.entry((t, rid)).or_default() += size * count,
            _ => {}
        }
    }
    let mut r = ReportIds::default();
    for ((t, id), bits) in acc {
        let e = (id, bits / 8);
        match t {
            0x80 => r.input.push(e),
            0x90 => r.output.push(e),
            _ => r.feature.push(e),
        }
    }
    r
}

pub fn parse_uevent(text: &str) -> (Option<(u16, u16, u16)>, String, String, String) {
    let (mut id, mut uniq, mut phys, mut name) = (None, String::new(), String::new(), String::new());
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("HID_ID=") {
            let p: Vec<_> = v.split(':').collect();
            if p.len() == 3 {
                let h = |s: &str| u32::from_str_radix(s, 16).ok().map(|x| x as u16);
                if let (Some(b), Some(vi), Some(pi)) = (h(p[0]), h(p[1]), h(p[2])) {
                    id = Some((b, vi, pi));
                }
            }
        } else if let Some(v) = line.strip_prefix("HID_UNIQ=") {
            uniq = v.to_string();
        } else if let Some(v) = line.strip_prefix("HID_PHYS=") {
            phys = v.to_string();
        } else if let Some(v) = line.strip_prefix("HID_NAME=") {
            name = v.to_string();
        }
    }
    (id, uniq, phys, name)
}

fn iface_from_phys(phys: &str) -> Option<u8> {
    phys.rsplit_once("/input").and_then(|(_, n)| n.parse().ok())
}

/// Find all hidraw nodes that carry Steam Controller (Triton) state reports.
pub fn scan() -> Vec<HidInfo> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/sys/class/hidraw") else { return out };
    for e in rd.flatten() {
        let node = e.file_name().to_string_lossy().to_string();
        let sys = e.path();
        let Ok(uevent) = fs::read_to_string(sys.join("device/uevent")) else { continue };
        let (Some((bus, vid, pid)), uniq, phys, name) = parse_uevent(&uevent) else { continue };
        if vid != VALVE_VID || !matches!(pid, PID_TRITON_USB | PID_TRITON_BLE | PID_PROTEUS_DONGLE | PID_NEREID_DONGLE)
        {
            continue;
        }
        let iface = iface_from_phys(&phys);
        let info = HidInfo { dev: Path::new("/dev").join(&node), node, bus, vid, pid, iface, uniq, phys, name };
        if info.is_dongle() && !matches!(iface, Some(2..=5)) {
            continue;
        }
        // Only nodes with Triton state input and the rumble output report.
        let Ok(desc) = fs::read(sys.join("device/report_descriptor")) else { continue };
        let ids = parse_report_descriptor(&desc);
        if !(ids.has_input(0x42) || ids.has_input(0x45) || ids.has_input(0x47)) || !ids.has_output(0x80) {
            continue;
        }
        out.push(info);
    }
    out.sort_by(|a, b| (a.phys.as_str(), a.node.as_str()).cmp(&(b.phys.as_str(), b.node.as_str())));
    out
}

pub struct HidDevice {
    file: File,
    pub info: HidInfo,
}

const fn hidiocsfeature(len: usize) -> u64 {
    sys::ioc(sys::IOC_WRITE | sys::IOC_READ, b'H', 0x06, len)
}
const fn hidiocgfeature(len: usize) -> u64 {
    sys::ioc(sys::IOC_WRITE | sys::IOC_READ, b'H', 0x07, len)
}

impl HidDevice {
    pub fn open(info: &HidInfo) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&info.dev)?;
        Ok(Self { file, info: info.clone() })
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Non-blocking read of one report. Ok(None) when nothing is pending.
    pub fn read_report(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        match self.file.read(buf) {
            Ok(n) => Ok(Some(n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Write an output report (first byte = report ID).
    pub fn write_output(&mut self, data: &[u8]) -> io::Result<()> {
        let n = self.file.write(data)?;
        if n != data.len() {
            return Err(io::Error::other(format!("short write {n}/{}", data.len())));
        }
        Ok(())
    }

    /// Send a feature report (first byte = report ID).
    pub fn set_feature(&mut self, data: &[u8]) -> io::Result<()> {
        let mut buf = data.to_vec();
        unsafe { sys::ioctl(self.fd(), hidiocsfeature(buf.len()), buf.as_mut_ptr()) }?;
        Ok(())
    }

    /// The controller's own serial number (ID_GET_STRING_ATTRIBUTE / unit serial), as the
    /// kernel's hid-steam reads it. Differs from the dongle's USB serial, so it identifies a
    /// controller across dongle slots. None when no controller answers.
    pub fn unit_serial(&mut self) -> Option<String> {
        let mut req = [0u8; 64];
        req[..4].copy_from_slice(&[1, 0xae, 0x15, 0x01]);
        self.set_feature(&req).ok()?;
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            let mut r = [0u8; 64];
            r[0] = 1;
            if self.get_feature(&mut r).is_ok() && r[1] == 0xae && r[3] == 0x01 {
                let len = (r[2] as usize).min(60);
                let s: String = r[4..4 + len.saturating_sub(1)]
                    .iter()
                    .take_while(|&&b| b != 0)
                    .map(|&b| b as char)
                    .filter(|c| c.is_ascii_alphanumeric())
                    .collect();
                return (!s.is_empty()).then_some(s);
            }
        }
        None
    }

    /// Get a feature report; `buf[0]` must hold the report ID.
    pub fn get_feature(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe { sys::ioctl(self.fd(), hidiocgfeature(buf.len()), buf.as_mut_ptr()) }?;
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phys_iface() {
        assert_eq!(iface_from_phys("usb-0000:0e:00.3-3.1/input2"), Some(2));
        assert_eq!(iface_from_phys("aa:bb:cc"), None);
    }

    #[test]
    fn uevent() {
        let (id, uniq, phys, _) = parse_uevent(
            "DRIVER=hid-generic\nHID_ID=0003:000028DE:00001304\nHID_NAME=Valve\nHID_PHYS=usb-x/input3\nHID_UNIQ=FXB1\n",
        );
        assert_eq!(id, Some((3, 0x28de, 0x1304)));
        assert_eq!(uniq, "FXB1");
        assert_eq!(phys, "usb-x/input3");
    }
}

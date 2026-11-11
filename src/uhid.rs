//! /dev/uhid: virtual HID devices whose reports we produce and whose requests we answer.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;

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

pub const FEATURE_REPORT: u8 = 0;
pub const OUTPUT_REPORT: u8 = 1;
pub const INPUT_REPORT: u8 = 2;

pub const BUS_USB: u16 = 0x03;
pub const BUS_BLUETOOTH: u16 = 0x05;
pub const BUS_VIRTUAL: u16 = 0x06;

pub const DATA_MAX: usize = 4096;
/// sizeof(struct uhid_event): u32 type + the largest union member (uhid_create2_req).
const EVENT_SIZE: usize = 4 + 128 + 64 + 64 + 2 + 2 + 4 * 4 + DATA_MAX;

pub fn rtype_name(t: u8) -> &'static str {
    match t {
        FEATURE_REPORT => "feature",
        OUTPUT_REPORT => "output",
        INPUT_REPORT => "input",
        _ => "?",
    }
}

pub struct Params<'a> {
    pub name: &'a str,
    pub phys: &'a str,
    pub uniq: &'a str,
    pub bus: u16,
    pub vid: u16,
    pub pid: u16,
    pub descriptor: &'a [u8],
}

#[derive(Debug)]
pub enum Event {
    Start,
    Stop,
    Open,
    Close,
    /// Output report written by a client (first byte = report ID if numbered).
    Output {
        rtype: u8,
        data: Vec<u8>,
    },
    /// Answer with [`Device::get_report_reply`].
    GetReport {
        id: u32,
        rnum: u8,
        rtype: u8,
    },
    /// Answer with [`Device::set_report_reply`].
    SetReport {
        id: u32,
        rnum: u8,
        rtype: u8,
        data: Vec<u8>,
    },
    Other(#[allow(dead_code)] u32),
}

/// Open /dev/uhid directly, or through `puckd broker` when it is root-only.
pub fn open_file() -> io::Result<File> {
    match OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC).open("/dev/uhid") {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            crate::broker::request("uhid").map_err(|b| io::Error::new(e.kind(), format!("/dev/uhid: {e}; broker: {b}")))
        }
        Err(e) => Err(e),
    }
}

/// A uhid device; dropping it removes the device.
pub struct Device {
    file: File,
}

fn put_str(buf: &mut [u8], s: &str) {
    let n = s.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&s.as_bytes()[..n]);
}

fn event(ty: u32) -> Vec<u8> {
    let mut ev = vec![0u8; EVENT_SIZE];
    ev[0..4].copy_from_slice(&ty.to_ne_bytes());
    ev
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_ne_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_ne_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl Device {
    /// Create a device on an open /dev/uhid file (non-blocking).
    pub fn create(file: File, p: &Params) -> io::Result<Self> {
        if p.descriptor.len() > DATA_MAX {
            return Err(io::Error::other("report descriptor too long"));
        }
        let mut ev = event(UHID_CREATE2);
        let r = &mut ev[4..];
        put_str(&mut r[0..128], p.name);
        put_str(&mut r[128..192], p.phys);
        put_str(&mut r[192..256], p.uniq);
        r[256..258].copy_from_slice(&(p.descriptor.len() as u16).to_ne_bytes());
        r[258..260].copy_from_slice(&p.bus.to_ne_bytes());
        r[260..264].copy_from_slice(&(p.vid as u32).to_ne_bytes());
        r[264..268].copy_from_slice(&(p.pid as u32).to_ne_bytes());
        // version (268), country (272): 0
        r[276..276 + p.descriptor.len()].copy_from_slice(p.descriptor);
        let mut d = Self { file };
        d.file.write_all(&ev)?;
        Ok(d)
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Send an input report (first byte = report ID if numbered).
    pub fn input(&mut self, data: &[u8]) -> io::Result<()> {
        let n = data.len().min(DATA_MAX);
        let mut ev = event(UHID_INPUT2);
        ev[4..6].copy_from_slice(&(n as u16).to_ne_bytes());
        ev[6..6 + n].copy_from_slice(&data[..n]);
        self.file.write_all(&ev)
    }

    /// Next pending event, None when there is none.
    pub fn read_event(&mut self) -> io::Result<Option<Event>> {
        let mut ev = vec![0u8; EVENT_SIZE];
        match self.file.read(&mut ev) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) => return Err(e),
        }
        let r = &ev[4..];
        Ok(Some(match u32_at(&ev, 0) {
            UHID_START => Event::Start,
            UHID_STOP => Event::Stop,
            UHID_OPEN => Event::Open,
            UHID_CLOSE => Event::Close,
            UHID_OUTPUT => {
                // struct uhid_output_req { data[4096]; u16 size; u8 rtype; }
                let size = (u16_at(r, DATA_MAX) as usize).min(DATA_MAX);
                Event::Output { rtype: r[DATA_MAX + 2], data: r[..size].to_vec() }
            }
            UHID_GET_REPORT => Event::GetReport { id: u32_at(r, 0), rnum: r[4], rtype: r[5] },
            UHID_SET_REPORT => {
                let size = (u16_at(r, 6) as usize).min(DATA_MAX);
                Event::SetReport { id: u32_at(r, 0), rnum: r[4], rtype: r[5], data: r[8..8 + size].to_vec() }
            }
            other => Event::Other(other),
        }))
    }

    /// Answer a GetReport: the report (with its ID byte) or an errno.
    pub fn get_report_reply(&mut self, id: u32, res: Result<&[u8], i32>) -> io::Result<()> {
        let mut ev = event(UHID_GET_REPORT_REPLY);
        ev[4..8].copy_from_slice(&id.to_ne_bytes());
        match res {
            Ok(data) => {
                let n = data.len().min(DATA_MAX);
                ev[10..12].copy_from_slice(&(n as u16).to_ne_bytes());
                ev[12..12 + n].copy_from_slice(&data[..n]);
            }
            Err(errno) => ev[8..10].copy_from_slice(&(errno as u16).to_ne_bytes()),
        }
        self.file.write_all(&ev)
    }

    /// Answer a SetReport: 0 or an errno.
    pub fn set_report_reply(&mut self, id: u32, errno: i32) -> io::Result<()> {
        let mut ev = event(UHID_SET_REPORT_REPLY);
        ev[4..8].copy_from_slice(&id.to_ne_bytes());
        ev[8..10].copy_from_slice(&(errno as u16).to_ne_bytes());
        self.file.write_all(&ev)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = self.file.write_all(&event(UHID_DESTROY));
    }
}

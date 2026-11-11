//! Thin raw uinput wrapper (device creation, event emission, force-feedback plumbing).

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;

use crate::sys::{self, IOC_NONE, IOC_READ, IOC_WRITE, ioc};

pub mod ev {
    pub const EV_SYN: u16 = 0x00;
    pub const EV_KEY: u16 = 0x01;
    pub const EV_REL: u16 = 0x02;
    pub const EV_ABS: u16 = 0x03;
    pub const EV_MSC: u16 = 0x04;
    pub const EV_FF: u16 = 0x15;
    pub const EV_UINPUT: u16 = 0x0101;
    pub const SYN_REPORT: u16 = 0;
    pub const UI_FF_UPLOAD: u16 = 1;
    pub const UI_FF_ERASE: u16 = 2;
    pub const MSC_TIMESTAMP: u16 = 0x05;

    pub const REL_X: u16 = 0x00;
    pub const REL_Y: u16 = 0x01;
    pub const REL_HWHEEL: u16 = 0x06;
    pub const REL_WHEEL: u16 = 0x08;
    pub const REL_WHEEL_HI_RES: u16 = 0x0b;
    pub const REL_HWHEEL_HI_RES: u16 = 0x0c;

    pub const ABS_X: u16 = 0x00;
    pub const ABS_Y: u16 = 0x01;
    pub const ABS_Z: u16 = 0x02;
    pub const ABS_RX: u16 = 0x03;
    pub const ABS_RY: u16 = 0x04;
    pub const ABS_RZ: u16 = 0x05;
    pub const ABS_HAT0X: u16 = 0x10;
    pub const ABS_HAT0Y: u16 = 0x11;
    pub const ABS_HAT1X: u16 = 0x12;
    pub const ABS_HAT1Y: u16 = 0x13;
    pub const ABS_HAT2X: u16 = 0x14;
    pub const ABS_HAT2Y: u16 = 0x15;
    pub const ABS_HAT3X: u16 = 0x16;
    pub const ABS_HAT3Y: u16 = 0x17;

    pub const BTN_LEFT: u16 = 0x110;
    pub const BTN_RIGHT: u16 = 0x111;
    pub const BTN_MIDDLE: u16 = 0x112;
    pub const BTN_SIDE: u16 = 0x113;
    pub const BTN_EXTRA: u16 = 0x114;
    pub const BTN_BASE: u16 = 0x126;
    pub const BTN_SOUTH: u16 = 0x130;
    pub const BTN_EAST: u16 = 0x131;
    /// Xbox "X" in xpad's layout (== BTN_NORTH).
    pub const BTN_X: u16 = 0x133;
    /// Xbox "Y" in xpad's layout (== BTN_WEST).
    pub const BTN_Y: u16 = 0x134;
    pub const BTN_TL: u16 = 0x136;
    pub const BTN_TR: u16 = 0x137;
    pub const BTN_TL2: u16 = 0x138;
    pub const BTN_TR2: u16 = 0x139;
    pub const BTN_SELECT: u16 = 0x13a;
    pub const BTN_START: u16 = 0x13b;
    pub const BTN_MODE: u16 = 0x13c;
    pub const BTN_THUMBL: u16 = 0x13d;
    pub const BTN_THUMBR: u16 = 0x13e;
    pub const BTN_THUMB: u16 = 0x121;
    pub const BTN_THUMB2: u16 = 0x122;
    pub const BTN_DPAD_UP: u16 = 0x220;
    pub const BTN_DPAD_DOWN: u16 = 0x221;
    pub const BTN_DPAD_LEFT: u16 = 0x222;
    pub const BTN_DPAD_RIGHT: u16 = 0x223;
    pub const BTN_GRIPL: u16 = 0x224;
    pub const BTN_GRIPR: u16 = 0x225;
    pub const BTN_GRIPL2: u16 = 0x226;
    pub const BTN_GRIPR2: u16 = 0x227;
    pub const BTN_TRIGGER_HAPPY1: u16 = 0x2c0;

    pub const FF_RUMBLE: u16 = 0x50;
    pub const FF_PERIODIC: u16 = 0x51;
    pub const FF_CONSTANT: u16 = 0x52;
    pub const FF_SQUARE: u16 = 0x58;
    pub const FF_TRIANGLE: u16 = 0x59;
    pub const FF_SINE: u16 = 0x5a;
    pub const FF_SAW_UP: u16 = 0x5b;
    pub const FF_SAW_DOWN: u16 = 0x5c;
    pub const FF_GAIN: u16 = 0x60;

    pub const INPUT_PROP_POINTER: u16 = 0x00;
    pub const INPUT_PROP_ACCELEROMETER: u16 = 0x06;

    pub const BUS_USB: u16 = 0x03;
    pub const BUS_VIRTUAL: u16 = 0x06;
}

use ev::*;

#[derive(Debug, Clone, Copy)]
pub struct AbsSpec {
    pub code: u16,
    pub min: i32,
    pub max: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub res: i32,
}

impl AbsSpec {
    pub const fn new(code: u16, min: i32, max: i32) -> Self {
        Self { code, min, max, fuzz: 0, flat: 0, res: 0 }
    }
    pub const fn ff(mut self, fuzz: i32, flat: i32) -> Self {
        self.fuzz = fuzz;
        self.flat = flat;
        self
    }
    pub const fn res(mut self, res: i32) -> Self {
        self.res = res;
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeviceSpec {
    pub name: String,
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub phys: Option<String>,
    pub keys: Vec<u16>,
    pub rels: Vec<u16>,
    pub abs: Vec<AbsSpec>,
    pub msc: Vec<u16>,
    pub ff: Vec<u16>,
    pub ff_effects_max: u32,
    pub props: Vec<u16>,
}

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct InputAbsinfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    absinfo: InputAbsinfo,
}

/// `struct ff_effect` (64-bit layout: the union is 8-byte aligned, 32 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FfEffect {
    pub kind: u16,
    pub id: i16,
    pub direction: u16,
    pub trigger_button: u16,
    pub trigger_interval: u16,
    pub replay_length: u16,
    pub replay_delay: u16,
    pub _pad: u16,
    pub u: [u64; 4],
}

impl FfEffect {
    fn bytes(&self) -> [u8; 32] {
        let mut b = [0u8; 32];
        for (i, w) in self.u.iter().enumerate() {
            b[i * 8..i * 8 + 8].copy_from_slice(&w.to_ne_bytes());
        }
        b
    }
    fn u16_at(&self, o: usize) -> u16 {
        let b = self.bytes();
        u16::from_ne_bytes([b[o], b[o + 1]])
    }
    /// FF_RUMBLE: (strong, weak)
    pub fn rumble(&self) -> (u16, u16) {
        (self.u16_at(0), self.u16_at(2))
    }
    /// FF_PERIODIC: (waveform, period ms, magnitude)
    pub fn periodic(&self) -> (u16, u16, i16) {
        (self.u16_at(0), self.u16_at(2), self.u16_at(4) as i16)
    }
    /// FF_CONSTANT: level
    pub fn constant(&self) -> i16 {
        self.u16_at(0) as i16
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct UinputFfUpload {
    pub request_id: u32,
    pub retval: i32,
    pub effect: FfEffect,
    pub old: FfEffect,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct UinputFfErase {
    pub request_id: u32,
    pub retval: i32,
    pub effect_id: u32,
}

const _: () = assert!(std::mem::size_of::<UinputSetup>() == 92);
const _: () = assert!(std::mem::size_of::<UinputAbsSetup>() == 28);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<FfEffect>() == 48);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<UinputFfUpload>() == 104);
const _: () = assert!(std::mem::size_of::<UinputFfErase>() == 12);

const UI_DEV_CREATE: u64 = ioc(IOC_NONE, b'U', 1, 0);
const UI_DEV_DESTROY: u64 = ioc(IOC_NONE, b'U', 2, 0);
const UI_DEV_SETUP: u64 = ioc(IOC_WRITE, b'U', 3, std::mem::size_of::<UinputSetup>());
const UI_ABS_SETUP: u64 = ioc(IOC_WRITE, b'U', 4, std::mem::size_of::<UinputAbsSetup>());
const UI_SET_EVBIT: u64 = ioc(IOC_WRITE, b'U', 100, 4);
const UI_SET_KEYBIT: u64 = ioc(IOC_WRITE, b'U', 101, 4);
const UI_SET_RELBIT: u64 = ioc(IOC_WRITE, b'U', 102, 4);
const UI_SET_ABSBIT: u64 = ioc(IOC_WRITE, b'U', 103, 4);
const UI_SET_MSCBIT: u64 = ioc(IOC_WRITE, b'U', 104, 4);
const UI_SET_FFBIT: u64 = ioc(IOC_WRITE, b'U', 107, 4);
const UI_SET_PHYS: u64 = ioc(IOC_WRITE, b'U', 108, std::mem::size_of::<*const u8>());
const UI_SET_PROPBIT: u64 = ioc(IOC_WRITE, b'U', 110, 4);
const UI_BEGIN_FF_UPLOAD: u64 = ioc(IOC_READ | IOC_WRITE, b'U', 200, std::mem::size_of::<UinputFfUpload>());
const UI_END_FF_UPLOAD: u64 = ioc(IOC_WRITE, b'U', 201, std::mem::size_of::<UinputFfUpload>());
const UI_BEGIN_FF_ERASE: u64 = ioc(IOC_READ | IOC_WRITE, b'U', 202, std::mem::size_of::<UinputFfErase>());
const UI_END_FF_ERASE: u64 = ioc(IOC_WRITE, b'U', 203, std::mem::size_of::<UinputFfErase>());
const fn ui_get_sysname(len: usize) -> u64 {
    ioc(IOC_READ, b'U', 44, len)
}

#[derive(Debug, Clone, Copy)]
pub struct RawEvent {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

pub struct VirtualDevice {
    file: File,
    pub sysname: String,
    pending: Vec<libc::input_event>,
    /// Last emitted value per (type, code) for EV_KEY / EV_ABS, to drop no-op frames.
    last: HashMap<(u16, u16), i32>,
}

impl VirtualDevice {
    pub fn create(spec: &DeviceSpec) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open("/dev/uinput")?;
        let fd = file.as_raw_fd();
        let set = |req: u64, v: u16| sys::ioctl_int(fd, req, v as libc::c_int);

        if !spec.keys.is_empty() {
            set(UI_SET_EVBIT, EV_KEY)?;
            for &k in &spec.keys {
                set(UI_SET_KEYBIT, k)?;
            }
        }
        if !spec.rels.is_empty() {
            set(UI_SET_EVBIT, EV_REL)?;
            for &r in &spec.rels {
                set(UI_SET_RELBIT, r)?;
            }
        }
        if !spec.abs.is_empty() {
            set(UI_SET_EVBIT, EV_ABS)?;
            for a in &spec.abs {
                set(UI_SET_ABSBIT, a.code)?;
                let mut s = UinputAbsSetup {
                    code: a.code,
                    absinfo: InputAbsinfo {
                        value: 0,
                        minimum: a.min,
                        maximum: a.max,
                        fuzz: a.fuzz,
                        flat: a.flat,
                        resolution: a.res,
                    },
                };
                unsafe { sys::ioctl(fd, UI_ABS_SETUP, &mut s) }?;
            }
        }
        if !spec.msc.is_empty() {
            set(UI_SET_EVBIT, EV_MSC)?;
            for &m in &spec.msc {
                set(UI_SET_MSCBIT, m)?;
            }
        }
        if !spec.ff.is_empty() {
            set(UI_SET_EVBIT, EV_FF)?;
            for &f in &spec.ff {
                set(UI_SET_FFBIT, f)?;
            }
        }
        for &p in &spec.props {
            set(UI_SET_PROPBIT, p)?;
        }
        if let Some(phys) = &spec.phys {
            let c = CString::new(phys.as_str()).map_err(io::Error::other)?;
            let r = unsafe { libc::ioctl(fd, UI_SET_PHYS as _, c.as_ptr()) };
            if r < 0 {
                return Err(io::Error::last_os_error());
            }
        }

        let mut setup = UinputSetup {
            id: InputId { bustype: spec.bustype, vendor: spec.vendor, product: spec.product, version: spec.version },
            name: [0; 80],
            ff_effects_max: if spec.ff.is_empty() { 0 } else { spec.ff_effects_max },
        };
        let n = spec.name.len().min(79);
        setup.name[..n].copy_from_slice(&spec.name.as_bytes()[..n]);
        unsafe { sys::ioctl(fd, UI_DEV_SETUP, &mut setup) }?;
        sys::ioctl_int(fd, UI_DEV_CREATE, 0)?;

        let mut buf = [0u8; 64];
        let sysname = match unsafe { sys::ioctl(fd, ui_get_sysname(buf.len()), buf.as_mut_ptr()) } {
            Ok(_) => String::from_utf8_lossy(buf.split(|&b| b == 0).next().unwrap_or(&[])).into_owned(),
            Err(_) => String::new(),
        };

        Ok(Self { file, sysname, pending: Vec::new(), last: HashMap::new() })
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Queue an event. EV_KEY/EV_ABS events that repeat the last value are dropped.
    pub fn push(&mut self, kind: u16, code: u16, value: i32) {
        if (kind == EV_KEY || kind == EV_ABS) && self.last.get(&(kind, code)) == Some(&value) {
            return;
        }
        if kind == EV_KEY || kind == EV_ABS {
            self.last.insert((kind, code), value);
        }
        if kind == EV_REL && value == 0 {
            return;
        }
        self.pending.push(raw_event(kind, code, value));
    }

    pub fn key(&mut self, code: u16, down: bool) {
        self.push(EV_KEY, code, down as i32);
    }

    pub fn abs(&mut self, code: u16, value: i32) {
        self.push(EV_ABS, code, value);
    }

    pub fn rel(&mut self, code: u16, value: i32) {
        self.push(EV_REL, code, value);
    }

    /// Queue an event even if the value did not change (for MSC_TIMESTAMP etc.).
    pub fn push_raw(&mut self, kind: u16, code: u16, value: i32) {
        self.pending.push(raw_event(kind, code, value));
    }

    /// Write queued events followed by SYN_REPORT. No-op if nothing is queued.
    pub fn sync(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.pending.push(raw_event(EV_SYN, SYN_REPORT, 0));
        let bytes = unsafe {
            std::slice::from_raw_parts(
                self.pending.as_ptr() as *const u8,
                self.pending.len() * std::mem::size_of::<libc::input_event>(),
            )
        };
        let r = self.file.write_all(bytes);
        self.pending.clear();
        r
    }

    /// Drain events sent to the device by clients (EV_FF play/gain, EV_UINPUT requests).
    pub fn read_events(&mut self) -> io::Result<Vec<RawEvent>> {
        let mut out = Vec::new();
        let sz = std::mem::size_of::<libc::input_event>();
        let mut buf = vec![0u8; sz * 32];
        loop {
            match self.file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for chunk in buf[..n].chunks_exact(sz) {
                        let e: libc::input_event = unsafe { std::ptr::read_unaligned(chunk.as_ptr() as *const _) };
                        out.push(RawEvent { kind: e.type_, code: e.code, value: e.value });
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    pub fn begin_ff_upload(&self, request_id: u32) -> io::Result<UinputFfUpload> {
        let mut up = UinputFfUpload { request_id, ..Default::default() };
        unsafe { sys::ioctl(self.fd(), UI_BEGIN_FF_UPLOAD, &mut up) }?;
        Ok(up)
    }

    pub fn end_ff_upload(&self, mut up: UinputFfUpload) -> io::Result<()> {
        unsafe { sys::ioctl(self.fd(), UI_END_FF_UPLOAD, &mut up) }?;
        Ok(())
    }

    pub fn begin_ff_erase(&self, request_id: u32) -> io::Result<UinputFfErase> {
        let mut er = UinputFfErase { request_id, ..Default::default() };
        unsafe { sys::ioctl(self.fd(), UI_BEGIN_FF_ERASE, &mut er) }?;
        Ok(er)
    }

    pub fn end_ff_erase(&self, mut er: UinputFfErase) -> io::Result<()> {
        unsafe { sys::ioctl(self.fd(), UI_END_FF_ERASE, &mut er) }?;
        Ok(())
    }
}

impl Drop for VirtualDevice {
    fn drop(&mut self) {
        let _ = sys::ioctl_int(self.fd(), UI_DEV_DESTROY, 0);
    }
}

fn raw_event(kind: u16, code: u16, value: i32) -> libc::input_event {
    libc::input_event { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, type_: kind, code, value }
}

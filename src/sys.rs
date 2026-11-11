//! Small libc helpers: ioctl request encoding, poll, signals.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const IOC_NONE: u32 = 0;
pub const IOC_WRITE: u32 = 1;
pub const IOC_READ: u32 = 2;

pub const fn ioc(dir: u32, ty: u8, nr: u32, size: usize) -> u64 {
    ((dir as u64) << 30) | ((size as u64) << 16) | ((ty as u64) << 8) | nr as u64
}

/// `ioctl` returning an io::Result.
///
/// # Safety
/// `arg` must be valid for the given request.
pub unsafe fn ioctl<T>(fd: RawFd, req: u64, arg: *mut T) -> io::Result<i32> {
    let r = unsafe { libc::ioctl(fd, req as _, arg) };
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
}

/// `ioctl` with an integer argument (UI_SET_*BIT and friends).
pub fn ioctl_int(fd: RawFd, req: u64, arg: libc::c_int) -> io::Result<i32> {
    let r = unsafe { libc::ioctl(fd, req as _, arg as libc::c_ulong) };
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
}

/// poll(2) over `fds`. Returns the revents per fd (0 = nothing). EINTR yields all zeros.
pub fn poll(fds: &[RawFd], timeout: Option<Duration>) -> io::Result<Vec<i16>> {
    let mut pfds: Vec<libc::pollfd> =
        fds.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
    let ms = match timeout {
        None => -1,
        Some(d) => d.as_millis().min(i32::MAX as u128) as i32,
    };
    let r = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, ms) };
    if r < 0 {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::Interrupted {
            return Ok(vec![0; fds.len()]);
        }
        return Err(e);
    }
    Ok(pfds.iter().map(|p| p.revents).collect())
}

/// inotify on /dev: tells when hidraw nodes appear, go away or change permissions, so
/// controllers are found without polling.
pub struct DevWatch {
    fd: OwnedFd,
}

impl DevWatch {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mask = libc::IN_CREATE | libc::IN_DELETE | libc::IN_ATTRIB;
        if unsafe { libc::inotify_add_watch(fd.as_raw_fd(), c"/dev".as_ptr(), mask) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd })
    }

    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Read all pending events; true if one was about a hidraw node.
    pub fn hidraw_changed(&self) -> bool {
        let mut hit = false;
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                return hit;
            }
            hit |= inotify_names(&buf[..n as usize]).any(|name| name.starts_with(b"hidraw"));
        }
    }
}

/// File names in a buffer of `struct inotify_event`s.
fn inotify_names(mut b: &[u8]) -> impl Iterator<Item = &[u8]> {
    const HDR: usize = 16; // wd, mask, cookie, len
    std::iter::from_fn(move || {
        if b.len() < HDR {
            return None;
        }
        let len = u32::from_ne_bytes([b[12], b[13], b[14], b[15]]) as usize;
        let name = b.get(HDR..HDR + len)?;
        let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())];
        b = &b[(HDR + len).min(b.len())..];
        Some(name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inotify_event_names() {
        let mut b = Vec::new();
        for name in [&b"hidraw3\0"[..], &b"tty1\0\0\0\0\0\0\0\0\0\0\0\0"[..]] {
            b.extend_from_slice(&1i32.to_ne_bytes());
            b.extend_from_slice(&libc::IN_CREATE.to_ne_bytes());
            b.extend_from_slice(&0u32.to_ne_bytes());
            b.extend_from_slice(&(name.len() as u32).to_ne_bytes());
            b.extend_from_slice(name);
        }
        let names: Vec<&[u8]> = inotify_names(&b).collect();
        assert_eq!(names, [&b"hidraw3"[..], &b"tty1"[..]]);
    }
}

pub static QUIT: AtomicBool = AtomicBool::new(false);
pub static RELOAD: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(sig: libc::c_int) {
    match sig {
        libc::SIGHUP => RELOAD.store(true, Ordering::SeqCst),
        _ => QUIT.store(true, Ordering::SeqCst),
    }
}

/// Install SIGINT/SIGTERM (quit) and SIGHUP (reload) handlers without SA_RESTART,
/// so a blocking poll returns early.
pub fn install_signal_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_signal as *const () as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = 0;
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

pub fn quit_requested() -> bool {
    QUIT.load(Ordering::SeqCst)
}

pub fn take_reload() -> bool {
    RELOAD.swap(false, Ordering::SeqCst)
}

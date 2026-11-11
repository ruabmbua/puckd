//! Small libc helpers: ioctl request encoding, poll, signals.

use std::io;
use std::os::fd::RawFd;
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

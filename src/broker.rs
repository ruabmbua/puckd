//! `puckd broker`: a small root service that opens the controller's hidraw nodes and
//! /dev/uhid for the user's puckd and passes the file descriptors over a unix socket.
//!
//! With `72-puckd-exclusive.rules` the logged-in user (and so Steam and games) can no
//! longer open the real hidraw nodes; only puckd gets them, through the broker. This is
//! not a security boundary: it keeps other programs from tripping over the hardware.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use log::{info, warn};
use serde::Deserialize;

use crate::hid;

pub const SOCKET: &str = "/run/puckd/broker.sock";
const MAX_MSG: usize = 128;

fn sockaddr(path: &str) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let mut a: libc::sockaddr_un = unsafe { zeroed() };
    a.sun_family = libc::AF_UNIX as _;
    let b = path.as_bytes();
    if b.len() >= a.sun_path.len() {
        return Err(io::Error::other("socket path too long"));
    }
    for (d, s) in a.sun_path.iter_mut().zip(b) {
        *d = *s as _;
    }
    Ok((a, (size_of::<libc::sa_family_t>() + b.len() + 1) as libc::socklen_t))
}

fn socket() -> io::Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn set_timeout(fd: RawFd, d: Duration) {
    let tv = libc::timeval { tv_sec: d.as_secs() as _, tv_usec: d.subsec_micros() as _ };
    for opt in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
        unsafe {
            libc::setsockopt(fd, libc::SOL_SOCKET, opt, &tv as *const _ as *const _, size_of::<libc::timeval>() as _)
        };
    }
}

/// Send `msg`, with `fd` attached when given.
fn send(sock: RawFd, msg: &[u8], fd: Option<RawFd>) -> io::Result<()> {
    let mut iov = libc::iovec { iov_base: msg.as_ptr() as *mut _, iov_len: msg.len() };
    let mut cbuf = [0u8; 64];
    let mut mh: libc::msghdr = unsafe { zeroed() };
    mh.msg_iov = &mut iov;
    mh.msg_iovlen = 1;
    if let Some(fd) = fd {
        let space = unsafe { libc::CMSG_SPACE(size_of::<RawFd>() as u32) } as usize;
        mh.msg_control = cbuf.as_mut_ptr() as *mut _;
        mh.msg_controllen = space as _;
        unsafe {
            let c = libc::CMSG_FIRSTHDR(&mh);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as u32) as _;
            std::ptr::write_unaligned(libc::CMSG_DATA(c) as *mut RawFd, fd);
        }
    }
    if unsafe { libc::sendmsg(sock, &mh, libc::MSG_NOSIGNAL) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Receive one message and the fd attached to it, if any.
fn recv(sock: RawFd) -> io::Result<(Vec<u8>, Option<OwnedFd>)> {
    let mut buf = [0u8; MAX_MSG];
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr() as *mut _, iov_len: buf.len() };
    let mut cbuf = [0u8; 64];
    let mut mh: libc::msghdr = unsafe { zeroed() };
    mh.msg_iov = &mut iov;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf.as_mut_ptr() as *mut _;
    mh.msg_controllen = cbuf.len() as _;
    let n = unsafe { libc::recvmsg(sock, &mut mh, libc::MSG_CMSG_CLOEXEC) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut fd = None;
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&mh);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let raw = std::ptr::read_unaligned(libc::CMSG_DATA(c) as *const RawFd);
                fd = Some(OwnedFd::from_raw_fd(raw));
            }
            c = libc::CMSG_NXTHDR(&mh, c);
        }
    }
    Ok((buf[..n as usize].to_vec(), fd))
}

/// Ask the broker for a device ("uhid" or "hidrawN"). The fd comes back non-blocking.
pub fn request(what: &str) -> io::Result<File> {
    if !Path::new(SOCKET).exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no broker running"));
    }
    let s = socket()?;
    let (addr, len) = sockaddr(SOCKET)?;
    if unsafe { libc::connect(s.as_raw_fd(), &addr as *const _ as *const _, len) } < 0 {
        return Err(io::Error::last_os_error());
    }
    set_timeout(s.as_raw_fd(), Duration::from_secs(3));
    send(s.as_raw_fd(), what.as_bytes(), None)?;
    let (msg, fd) = recv(s.as_raw_fd())?;
    match fd {
        Some(fd) if msg == b"ok" => Ok(File::from(fd)),
        _ => Err(io::Error::other(String::from_utf8_lossy(&msg).into_owned())),
    }
}

/// The connecting process's (pid, uid, gid).
fn peer(sock: RawFd) -> Option<(i32, u32, u32)> {
    let mut cred: libc::ucred = unsafe { zeroed() };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    let r =
        unsafe { libc::getsockopt(sock, libc::SOL_SOCKET, libc::SO_PEERCRED, &mut cred as *mut _ as *mut _, &mut len) };
    (r == 0).then_some((cred.pid, cred.uid, cred.gid))
}

/// `/etc/puckd/broker.toml`: who may get the controller.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Access {
    /// Members of this group may (supplementary or primary; "" = no group).
    pub group: String,
    /// These users may too (names or uids).
    pub users: Vec<String>,
}

impl Default for Access {
    fn default() -> Self {
        Self { group: "puckd".into(), users: Vec::new() }
    }
}

pub const CONFIG: &str = "/etc/puckd/broker.toml";

impl Access {
    fn load(path: &Path) -> Result<Self, String> {
        match fs::read_to_string(path) {
            Ok(t) => toml::from_str(&t).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Whether this peer may use the broker.
    fn allows(&self, pid: i32, uid: u32, gid: u32) -> bool {
        if uid == 0 || self.users.iter().any(|u| uid_of(u) == Some(uid)) {
            return true;
        }
        let Some(want) = (!self.group.is_empty()).then(|| gid_of(&self.group)).flatten() else { return false };
        // The process's own groups (set at login), or the group database (no new login
        // needed after `usermod -aG`).
        gid == want || process_groups(pid).contains(&want) || user_groups(uid).contains(&want)
    }
}

fn uid_of(name: &str) -> Option<u32> {
    if let Ok(n) = name.parse() {
        return Some(n);
    }
    let c = std::ffi::CString::new(name).ok()?;
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    (!pw.is_null()).then(|| unsafe { (*pw).pw_uid })
}

fn gid_of(name: &str) -> Option<u32> {
    if let Ok(n) = name.parse() {
        return Some(n);
    }
    let c = std::ffi::CString::new(name).ok()?;
    let gr = unsafe { libc::getgrnam(c.as_ptr()) };
    (!gr.is_null()).then(|| unsafe { (*gr).gr_gid })
}

fn process_groups(pid: i32) -> Vec<u32> {
    let Ok(st) = fs::read_to_string(format!("/proc/{pid}/status")) else { return Vec::new() };
    st.lines()
        .find_map(|l| l.strip_prefix("Groups:"))
        .map(|g| g.split_whitespace().filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_default()
}

fn user_groups(uid: u32) -> Vec<u32> {
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return Vec::new();
    }
    let (name, gid) = unsafe { ((*pw).pw_name, (*pw).pw_gid) };
    let mut groups = vec![0 as libc::gid_t; 256];
    let mut n = groups.len() as libc::c_int;
    if unsafe { libc::getgrouplist(name, gid, groups.as_mut_ptr(), &mut n) } < 0 {
        return Vec::new();
    }
    groups.truncate(n.max(0) as usize);
    groups
}

fn open_rw(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC).open(path)
}

/// Open what was asked for, if it is something the broker hands out.
fn serve(req: &str) -> Result<File, String> {
    if req == "uhid" {
        return open_rw(Path::new("/dev/uhid")).map_err(|e| format!("/dev/uhid: {e}"));
    }
    let node = req.strip_prefix("/dev/").unwrap_or(req);
    if !node.strip_prefix("hidraw").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
        return Err(format!("not a hidraw node: {req}"));
    }
    if !hid::is_controller_node(node) {
        return Err(format!("{node} is not a Steam Controller / Puck node"));
    }
    open_rw(&Path::new("/dev").join(node)).map_err(|e| format!("{node}: {e}"))
}

/// How long a socket-activated broker waits for the next request before exiting.
const IDLE_EXIT: Duration = Duration::from_secs(60);

/// The listening socket from systemd socket activation, if any.
fn activated_socket() -> Option<OwnedFd> {
    let pid: u32 = std::env::var("LISTEN_PID").ok()?.parse().ok()?;
    let n: u32 = std::env::var("LISTEN_FDS").ok()?.parse().ok()?;
    (pid == std::process::id() && n >= 1).then(|| unsafe { OwnedFd::from_raw_fd(3) })
}

fn listen() -> io::Result<OwnedFd> {
    let dir = Path::new(SOCKET).parent().unwrap();
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
    let _ = fs::remove_file(SOCKET);
    let l = socket()?;
    let (addr, len) = sockaddr(SOCKET)?;
    if unsafe { libc::bind(l.as_raw_fd(), &addr as *const _ as *const _, len) } < 0
        || unsafe { libc::listen(l.as_raw_fd(), 8) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    // Anyone may connect; each peer is checked against the access config.
    fs::set_permissions(SOCKET, fs::Permissions::from_mode(0o666))?;
    Ok(l)
}

pub fn run(args: &[String]) -> ExitCode {
    let mut cfg_path = std::path::PathBuf::from(CONFIG);
    let mut extra: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match (a.as_str(), it.clone().next()) {
            ("--config", Some(f)) => {
                cfg_path = f.into();
                it.next();
            }
            ("--allow", Some(u)) => {
                extra.push(u.clone());
                it.next();
            }
            _ => {
                eprintln!("usage: puckd broker [--config FILE] [--allow USER]...");
                return ExitCode::from(2);
            }
        }
    }
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("puckd broker must run as root");
        return ExitCode::FAILURE;
    }
    if let Err(e) = Access::load(&cfg_path) {
        eprintln!("{e}");
        return ExitCode::from(2);
    }
    let activated = activated_socket();
    let idle = activated.is_some().then_some(IDLE_EXIT);
    let listener = match activated.map(Ok).unwrap_or_else(listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{SOCKET}: {e}");
            return ExitCode::FAILURE;
        }
    };
    info!("broker on {SOCKET}{}", if idle.is_some() { " (socket-activated)" } else { "" });
    while !crate::sys::quit_requested() {
        match crate::sys::poll(&[listener.as_raw_fd()], idle) {
            Ok(r) if r[0] == 0 => {
                if idle.is_some() && !crate::sys::quit_requested() {
                    info!("idle, exiting (systemd starts the broker again on the next request)");
                    break;
                }
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("poll: {e}");
                return ExitCode::FAILURE;
            }
        }
        let c = unsafe {
            libc::accept4(listener.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_CLOEXEC)
        };
        if c < 0 {
            continue;
        }
        let c = unsafe { OwnedFd::from_raw_fd(c) };
        set_timeout(c.as_raw_fd(), Duration::from_secs(2));
        // Re-read per request: edits apply without a restart.
        let mut access = Access::load(&cfg_path).unwrap_or_else(|e| {
            warn!("{e}; using the defaults");
            Access::default()
        });
        access.users.extend(extra.iter().cloned());
        let Some((pid, uid, gid)) = peer(c.as_raw_fd()) else { continue };
        if !access.allows(pid, uid, gid) {
            let why = if access.group.is_empty() {
                "not allowed (see /etc/puckd/broker.toml)".to_string()
            } else {
                format!("not allowed: add the user to the \"{}\" group", access.group)
            };
            warn!("refusing uid {uid} (pid {pid}): {why}");
            let _ = send(c.as_raw_fd(), why.as_bytes(), None);
            continue;
        }
        let Ok((msg, _)) = recv(c.as_raw_fd()) else { continue };
        let req = String::from_utf8_lossy(&msg).into_owned();
        match serve(&req) {
            Ok(f) => {
                info!("uid {uid} gets {req}");
                let _ = send(c.as_raw_fd(), b"ok", Some(f.as_raw_fd()));
            }
            Err(e) => {
                warn!("uid {uid}: {e}");
                let _ = send(c.as_raw_fd(), e.as_bytes(), None);
            }
        }
    }
    if idle.is_none() {
        let _ = fs::remove_file(SOCKET);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, Write};

    #[test]
    fn access_config() {
        let a: Access = toml::from_str("users = [\"1234\"]").unwrap();
        assert_eq!(a.group, "puckd");
        assert!(a.allows(std::process::id() as i32, 1234, 1234));
        assert!(a.allows(1, 0, 0));
        let none: Access = toml::from_str("group = \"\"").unwrap();
        assert!(!none.allows(std::process::id() as i32, 4242, 4242));
    }

    #[test]
    fn passes_an_fd() {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, fds.as_mut_ptr()) }, 0);
        let (a, b) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let path = std::env::temp_dir().join(format!("puckd-broker-test-{}", std::process::id()));
        let mut f = File::options().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
        f.write_all(b"hello").unwrap();
        send(a.as_raw_fd(), b"ok", Some(f.as_raw_fd())).unwrap();
        let (msg, fd) = recv(b.as_raw_fd()).unwrap();
        assert_eq!(msg, b"ok");
        let mut g = File::from(fd.expect("fd attached"));
        g.rewind().unwrap();
        let mut s = String::new();
        g.read_to_string(&mut s).unwrap();
        assert_eq!(s, "hello");
        send(a.as_raw_fd(), b"no", None).unwrap();
        let (msg, fd) = recv(b.as_raw_fd()).unwrap();
        assert_eq!((msg.as_slice(), fd.is_none()), (&b"no"[..], true));
        let _ = fs::remove_file(path);
    }
}

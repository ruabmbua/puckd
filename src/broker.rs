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

fn peer_uid(sock: RawFd) -> Option<u32> {
    let mut cred: libc::ucred = unsafe { zeroed() };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    let r =
        unsafe { libc::getsockopt(sock, libc::SOL_SOCKET, libc::SO_PEERCRED, &mut cred as *mut _ as *mut _, &mut len) };
    (r == 0).then_some(cred.uid)
}

fn uid_of(name: &str) -> Option<u32> {
    if let Ok(n) = name.parse() {
        return Some(n);
    }
    let passwd = fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next() == Some(name)).then(|| f.nth(1).and_then(|u| u.parse().ok())).flatten()
    })
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

pub fn run(args: &[String]) -> ExitCode {
    let mut allowed: Vec<u32> = vec![0];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--allow" => match it.next().and_then(|n| uid_of(n)) {
                Some(u) => allowed.push(u),
                None => {
                    eprintln!("--allow USER: unknown user");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("usage: puckd broker --allow USER [--allow USER...]  (unknown '{other}')");
                return ExitCode::from(2);
            }
        }
    }
    if allowed.len() == 1 {
        eprintln!("usage: puckd broker --allow USER");
        return ExitCode::from(2);
    }
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("puckd broker must run as root");
        return ExitCode::FAILURE;
    }
    let dir = Path::new(SOCKET).parent().unwrap();
    let _ = fs::create_dir_all(dir);
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
    let _ = fs::remove_file(SOCKET);
    let listener = match socket() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("socket: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (addr, len) = sockaddr(SOCKET).unwrap();
    if unsafe { libc::bind(listener.as_raw_fd(), &addr as *const _ as *const _, len) } < 0
        || unsafe { libc::listen(listener.as_raw_fd(), 8) } < 0
    {
        eprintln!("{SOCKET}: {}", io::Error::last_os_error());
        return ExitCode::FAILURE;
    }
    // Anyone may connect; the peer's uid is checked per connection.
    let _ = fs::set_permissions(SOCKET, fs::Permissions::from_mode(0o666));
    info!("broker listening on {SOCKET} for uids {:?}", &allowed[1..]);
    loop {
        let c = unsafe {
            libc::accept4(listener.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_CLOEXEC)
        };
        if c < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                if crate::sys::quit_requested() {
                    break;
                }
                continue;
            }
            eprintln!("accept: {e}");
            return ExitCode::FAILURE;
        }
        let c = unsafe { OwnedFd::from_raw_fd(c) };
        set_timeout(c.as_raw_fd(), Duration::from_secs(2));
        let uid = peer_uid(c.as_raw_fd());
        if !uid.is_some_and(|u| allowed.contains(&u)) {
            warn!("refusing uid {uid:?}");
            let _ = send(c.as_raw_fd(), b"not allowed", None);
            continue;
        }
        let Ok((msg, _)) = recv(c.as_raw_fd()) else { continue };
        let req = String::from_utf8_lossy(&msg).into_owned();
        match serve(&req) {
            Ok(f) => {
                info!("uid {} gets {req}", uid.unwrap_or(0));
                let _ = send(c.as_raw_fd(), b"ok", Some(f.as_raw_fd()));
            }
            Err(e) => {
                warn!("uid {}: {e}", uid.unwrap_or(0));
                let _ = send(c.as_raw_fd(), e.as_bytes(), None);
            }
        }
    }
    let _ = fs::remove_file(SOCKET);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, Write};

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

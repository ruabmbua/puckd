//! Detect other processes (Steam, SDL games) that have a controller's hidraw node open.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Opener {
    pub pid: u32,
    pub comm: String,
}

impl Opener {
    pub fn is_steam(&self) -> bool {
        self.comm == "steam" || self.comm.starts_with("steam") && !self.comm.starts_with("steamwebhelper")
    }
}

/// For each of `devs`, the other processes holding it open. Only processes we may
/// inspect (our own user's) are seen, which covers Steam and games.
pub fn openers(devs: &[PathBuf]) -> HashMap<PathBuf, Vec<Opener>> {
    let mut out: HashMap<PathBuf, Vec<Opener>> = HashMap::new();
    if devs.is_empty() {
        return out;
    }
    let me = std::process::id();
    let Ok(procs) = fs::read_dir("/proc") else { return out };
    for p in procs.flatten() {
        let Some(pid) = p.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        if pid == me {
            continue;
        }
        let Ok(fds) = fs::read_dir(p.path().join("fd")) else { continue };
        let mut comm: Option<String> = None;
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else { continue };
            if !target.as_os_str().as_encoded_bytes().starts_with(b"/dev/hidraw") {
                continue;
            }
            if let Some(dev) = devs.iter().find(|d| d.as_path() == Path::new(&target)) {
                let c = comm.get_or_insert_with(|| {
                    fs::read_to_string(p.path().join("comm")).map(|s| s.trim().to_string()).unwrap_or_default()
                });
                let list = out.entry(dev.clone()).or_default();
                if !list.iter().any(|o| o.pid == pid) {
                    list.push(Opener { pid, comm: c.clone() });
                }
            }
        }
    }
    out
}

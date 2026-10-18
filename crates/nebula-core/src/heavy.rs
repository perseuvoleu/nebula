//! The mark a `heavy` wrapper sets in its own environment before it asks
//! for a lease, as `NEBULA_HEAVY_LEASE=<wrapper pid>:<bytes|default>:<daemon>`.
//! The wrapper stays the root of its command's process tree and never
//! changes its own environment again, so a restarted daemon can count it
//! from the process table, whatever the command does to its own.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeavyMark {
    pub wrapper: u32,
    /// None takes the daemon's configured command reservation.
    pub reserve_bytes: Option<u64>,
    /// `daemon_id` of the socket the wrapper asks, so a daemon counts only
    /// its own wrappers.
    pub daemon: u64,
}

impl HeavyMark {
    pub const ENV: &'static str = "NEBULA_HEAVY_LEASE";

    pub fn value(&self) -> String {
        let bytes = match self.reserve_bytes {
            Some(bytes) => bytes.to_string(),
            None => "default".into(),
        };
        format!("{}:{bytes}:{:016x}", self.wrapper, self.daemon)
    }

    pub fn from_value(value: &str) -> Option<Self> {
        let mut fields = value.split(':');
        let wrapper = fields.next()?.parse().ok()?;
        let reserve_bytes = match fields.next()? {
            "default" => None,
            bytes => Some(bytes.parse().ok().filter(|&b| b > 0)?),
        };
        let daemon = u64::from_str_radix(fields.next()?, 16).ok()?;
        fields.next().is_none().then_some(Self {
            wrapper,
            reserve_bytes,
            daemon,
        })
    }

    /// Read one `NAME=value` environment entry.
    pub fn from_env_entry(entry: &str) -> Option<Self> {
        Self::from_value(entry.strip_prefix(Self::ENV)?.strip_prefix('=')?)
    }
}

/// FNV-1a of the socket path: stable across builds, unlike std's hasher.
pub fn daemon_id(socket: &Path) -> u64 {
    socket
        .as_os_str()
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
}

pub struct Process {
    pub pid: u32,
    pub ppid: u32,
    pub mark: Option<HeavyMark>,
}

impl Process {
    /// Descendants inherit the wrapper's mark; only the wrapper's names it.
    pub fn is_wrapper(&self) -> bool {
        self.mark.is_some_and(|m| m.wrapper == self.pid)
    }
}

/// Every process this user can see. Blocking.
#[cfg(target_os = "linux")]
pub fn processes() -> Vec<Process> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|entry| {
        let pid: u32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let ppid = stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        let mark = std::fs::read(format!("/proc/{pid}/environ"))
            .ok()
            .and_then(|environ| {
                environ
                    .split(|&b| b == 0)
                    .find_map(|e| HeavyMark::from_env_entry(std::str::from_utf8(e).ok()?))
            });
        Some(Process { pid, ppid, mark })
    })
    .collect()
}

/// `ps -E` appends each process's environment to its command line.
#[cfg(not(target_os = "linux"))]
pub fn processes() -> Vec<Process> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-axwwE", "-o", "pid=,ppid=,command="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let ppid = words.next()?.parse().ok()?;
            let mark = words.find_map(HeavyMark::from_env_entry);
            Some(Process { pid, ppid, mark })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_reads_back_what_it_wrote() {
        for reserve_bytes in [Some(644_245_094), None] {
            let mark = HeavyMark {
                wrapper: 4242,
                reserve_bytes,
                daemon: daemon_id(Path::new("/tmp/nebula-1000/daemon.sock")),
            };
            let entry = format!("{}={}", HeavyMark::ENV, mark.value());
            assert_eq!(HeavyMark::from_env_entry(&entry), Some(mark));
        }
        for malformed in [
            "4242:1",
            "4242:0:00",
            "4242:x:00",
            "4242:1:00:extra",
            "x:1:00",
        ] {
            assert_eq!(HeavyMark::from_value(malformed), None, "{malformed}");
        }
    }
}

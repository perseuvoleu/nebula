//! Small memory probes shared by the daemon and the TUI client. All of them
//! shell out (macOS has no /proc) and only run on the metrics modal's slow
//! poll, never on a hot path.

/// Resident set size of one process, bytes.
pub fn process_rss_bytes(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    text.trim().parse::<u64>().ok().map(|kb| kb * 1024)
}

/// Physical memory installed on this machine, bytes.
pub fn system_total_bytes() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        String::from_utf8(out.stdout).ok()?.trim().parse().ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
}

/// One reading of how much room the machine has, in the signals each
/// platform actually exposes: Linux has `MemAvailable` and PSI, macOS has
/// only the kernel's pressure level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MachineMemory {
    Linux {
        available_bytes: u64,
        swap_free_bytes: u64,
        psi_some_avg10: f64,
    },
    Macos {
        total_bytes: u64,
        swap_free_bytes: u64,
        pressure: VmPressureLevel,
    },
}

/// `kern.memorystatus_vm_pressure_level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmPressureLevel {
    Normal,
    Warn,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    Unreadable(String),
    Malformed(&'static str),
}

/// Read this machine's memory signals. Blocking (file reads or `sysctl`).
pub fn machine_memory() -> Result<MachineMemory, ProbeError> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sysctl")
            .args([
                "-n",
                "kern.memorystatus_vm_pressure_level",
                "hw.memsize",
                "vm.swapusage",
            ])
            .output()
            .map_err(|e| ProbeError::Unreadable(format!("sysctl: {e}")))?;
        parse_macos_sysctl(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let read = |path: &str| {
            std::fs::read_to_string(path)
                .map_err(|e| ProbeError::Unreadable(format!("{path}: {e}")))
        };
        parse_linux(&read("/proc/meminfo")?, &read("/proc/pressure/memory")?)
    }
}

/// `meminfo` is `/proc/meminfo`, `psi` is `/proc/pressure/memory`.
pub fn parse_linux(meminfo: &str, psi: &str) -> Result<MachineMemory, ProbeError> {
    let kb = |field: &'static str| {
        meminfo
            .lines()
            .find_map(|l| l.strip_prefix(field)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
            .and_then(|kb| kb.checked_mul(1024))
            .ok_or(ProbeError::Malformed(field))
    };
    let psi_some_avg10 = psi
        .lines()
        .find_map(|l| l.strip_prefix("some "))
        .and_then(|rest| {
            rest.split_whitespace()
                .find_map(|kv| kv.strip_prefix("avg10="))
        })
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|percent| (0.0..=100.0).contains(percent))
        .ok_or(ProbeError::Malformed("psi some avg10"))?;
    Ok(MachineMemory::Linux {
        available_bytes: kb("MemAvailable")?,
        swap_free_bytes: kb("SwapFree")?,
        psi_some_avg10,
    })
}

/// Output of `sysctl -n kern.memorystatus_vm_pressure_level hw.memsize
/// vm.swapusage`, one value per line in that order.
pub fn parse_macos_sysctl(text: &str) -> Result<MachineMemory, ProbeError> {
    let mut lines = text.lines().map(str::trim);
    let pressure = match lines.next() {
        Some("1") => VmPressureLevel::Normal,
        Some("2") => VmPressureLevel::Warn,
        Some("4") => VmPressureLevel::Critical,
        _ => return Err(ProbeError::Malformed("vm_pressure_level")),
    };
    let total_bytes = lines
        .next()
        .and_then(|l| l.parse().ok())
        .ok_or(ProbeError::Malformed("hw.memsize"))?;
    let swap_free_bytes = lines
        .next()
        .and_then(|l| l.split("free = ").nth(1)?.split_whitespace().next())
        .and_then(parse_sysctl_size)
        .ok_or(ProbeError::Malformed("vm.swapusage"))?;
    Ok(MachineMemory::Macos {
        total_bytes,
        swap_free_bytes,
        pressure,
    })
}

fn parse_sysctl_size(s: &str) -> Option<u64> {
    let (num, scale) = [('K', 1u64 << 10), ('M', 1 << 20), ('G', 1 << 30)]
        .into_iter()
        .find_map(|(unit, scale)| Some((s.strip_suffix(unit)?, scale)))?;
    let bytes = num.parse::<f64>().ok()? * scale as f64;
    (0.0..u64::MAX as f64)
        .contains(&bytes)
        .then_some(bytes as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    const MEMINFO: &str = "\
MemTotal:       16382172 kB
MemFree:         4286236 kB
MemAvailable:    7479388 kB
Buffers:          166684 kB
SwapTotal:       6291452 kB
SwapFree:         528812 kB
";
    const PSI: &str = "\
some avg10=12.50 avg60=0.02 avg300=0.00 total=2044511005
full avg10=3.00 avg60=0.01 avg300=0.00 total=1604359218
";

    #[test]
    fn linux_reads_available_swap_and_psi() {
        assert_eq!(
            parse_linux(MEMINFO, PSI),
            Ok(MachineMemory::Linux {
                available_bytes: 7479388 * 1024,
                swap_free_bytes: 528812 * 1024,
                psi_some_avg10: 12.5,
            })
        );
    }

    #[test]
    fn linux_rejects_missing_or_malformed_fields() {
        let cases = [
            ("MemTotal: 1 kB\nSwapFree: 2 kB\n", PSI, "MemAvailable"),
            (
                "MemAvailable: lots kB\nSwapFree: 2 kB\n",
                PSI,
                "MemAvailable",
            ),
            ("MemAvailable: 1 kB\n", PSI, "SwapFree"),
            (MEMINFO, "", "psi some avg10"),
            (MEMINFO, "some avg10=high avg60=0.00\n", "psi some avg10"),
            (MEMINFO, "full avg10=0.00 avg60=0.00\n", "psi some avg10"),
            (MEMINFO, "some avg10=NaN avg60=0.00\n", "psi some avg10"),
            (MEMINFO, "some avg10=inf avg60=0.00\n", "psi some avg10"),
            (
                "MemAvailable: 18014398509481984 kB\nSwapFree: 0 kB\n",
                PSI,
                "MemAvailable",
            ),
        ];
        for (meminfo, psi, field) in cases {
            assert_eq!(
                parse_linux(meminfo, psi),
                Err(ProbeError::Malformed(field)),
                "meminfo={meminfo:?} psi={psi:?}"
            );
        }
    }

    #[test]
    fn macos_reads_pressure_level_memsize_and_swap_free() {
        let text =
            "1\n17179869184\ntotal = 20480.00M  used = 18800.00M  free = 1680.00M  (encrypted)\n";
        assert_eq!(
            parse_macos_sysctl(text),
            Ok(MachineMemory::Macos {
                total_bytes: 16 * GIB,
                swap_free_bytes: 1680 * 1024 * 1024,
                pressure: VmPressureLevel::Normal,
            })
        );
        let levels = [
            ("2", VmPressureLevel::Warn),
            ("4", VmPressureLevel::Critical),
        ];
        for (raw, level) in levels {
            let text = format!("{raw}\n17179869184\ntotal = 0.00M  used = 0.00M  free = 0.00M\n");
            assert!(
                matches!(parse_macos_sysctl(&text), Ok(MachineMemory::Macos { pressure, .. }) if pressure == level),
                "level {raw}"
            );
        }
    }

    #[test]
    fn macos_rejects_unknown_level_and_malformed_lines() {
        let swap = "total = 0.00M  used = 0.00M  free = 1.00M";
        let cases = [
            (format!("3\n17179869184\n{swap}\n"), "vm_pressure_level"),
            (format!("\n17179869184\n{swap}\n"), "vm_pressure_level"),
            (format!("1\nmany\n{swap}\n"), "hw.memsize"),
            ("1\n17179869184\n".to_string(), "vm.swapusage"),
            (
                "1\n17179869184\ntotal = 1.00M used = 0.00M\n".to_string(),
                "vm.swapusage",
            ),
            ("".to_string(), "vm_pressure_level"),
            ("1\n17179869184\nfree = infM\n".to_string(), "vm.swapusage"),
            ("1\n17179869184\nfree = NaNM\n".to_string(), "vm.swapusage"),
            ("1\n17179869184\nfree = 1é\n".to_string(), "vm.swapusage"),
        ];
        for (text, field) in cases {
            assert_eq!(
                parse_macos_sysctl(&text),
                Err(ProbeError::Malformed(field)),
                "{text:?}"
            );
        }
    }
}

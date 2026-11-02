//! The physical computer: what the recommendation is computed from and
//! what the guest's agent is told. Parsers are pure and tested on captured
//! output; a reading that fails is `None`, never a made-up zero.

use std::path::Path;
use std::process::Command;
use vercelify_core::host::{HostCapabilities, HostOs, HostStatus, Virtualization};

pub fn host_os() -> HostOs {
    if cfg!(target_os = "macos") {
        HostOs::Macos
    } else if cfg!(windows) {
        HostOs::Windows
    } else {
        HostOs::Linux
    }
}

fn output(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).output().ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

pub fn os_version() -> String {
    if cfg!(target_os = "macos") {
        output("sw_vers", &["-productVersion"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    } else {
        sysinfo::System::long_os_version().unwrap_or_default()
    }
}

/// `pmset -g batt`: (on battery, charge).
pub fn parse_pmset_batt(out: &str) -> (Option<bool>, Option<u8>) {
    let on_battery = if out.contains("'Battery Power'") {
        Some(true)
    } else if out.contains("'AC Power'") {
        Some(false)
    } else {
        None
    };
    let pct = out
        .lines()
        .find(|l| l.contains("InternalBattery"))
        .and_then(|l| l.split('\t').nth(1))
        .and_then(|r| r.split('%').next())
        .and_then(|p| p.trim().parse().ok());
    (on_battery, pct)
}

/// `pmset -g therm`: throttled below 80 % speed, or a warning recorded.
pub fn parse_pmset_therm(out: &str) -> bool {
    let limited = out
        .lines()
        .filter_map(|l| l.trim().strip_prefix("CPU_Speed_Limit"))
        .filter_map(|r| {
            r.trim_start_matches(|c: char| c.is_whitespace() || c == '=')
                .trim()
                .parse::<u32>()
                .ok()
        })
        .any(|v| v < 80);
    let warned = out
        .lines()
        .any(|l| l.contains("thermal warning level") && !l.contains("No thermal warning"));
    limited || warned
}

/// `diskutil info`: a spinning disk?
pub fn parse_diskutil_ssd(out: &str) -> Option<bool> {
    let l = out.lines().find(|l| l.trim_start().starts_with("Solid State:"))?;
    Some(!l.contains("Yes"))
}

fn power() -> (Option<bool>, Option<u8>) {
    #[cfg(target_os = "macos")]
    {
        output("pmset", &["-g", "batt"])
            .map(|o| parse_pmset_batt(&o))
            .unwrap_or((None, None))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut s: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        if unsafe { GetSystemPowerStatus(&mut s) } == 0 {
            return (None, None);
        }
        let on_battery = match s.ACLineStatus {
            0 => Some(true),
            1 => Some(false),
            _ => None,
        };
        // 128: no battery; 255: unknown.
        let pct = (s.BatteryFlag & 128 == 0 && s.BatteryLifePercent <= 100).then_some(s.BatteryLifePercent);
        (on_battery, pct)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        (None, None)
    }
}

fn thermal_pressure() -> bool {
    #[cfg(target_os = "macos")]
    {
        output("pmset", &["-g", "therm"]).is_some_and(|o| parse_pmset_therm(&o))
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Free bytes on the volume holding `path` (or its nearest existing parent).
pub fn disk_free(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        #[allow(clippy::unnecessary_cast)]
        Some(s.f_bavail as u64 * s.f_frsize as u64)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let w: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free: u64 = 0;
        if unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) } == 0 {
            return None;
        }
        Some(free)
    }
}

fn rotational(path: &Path) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        output("diskutil", &["info", &path.to_string_lossy()])
            .or_else(|| output("diskutil", &["info", "/"]))
            .and_then(|o| parse_diskutil_ssd(&o))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

/// This computer's address on its network: the interface the default
/// route uses (a UDP socket "connected" to a public address learns it
/// without sending anything).
pub fn lan_ipv4() -> Option<String> {
    let s = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect(("1.1.1.1", 53)).ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v) if !v.is_loopback() && !v.is_unspecified() => Some(v.to_string()),
        _ => None,
    }
}

/// A main server's mesh ports are free on this computer.
pub fn mesh_ports_free() -> Result<(), String> {
    use crate::provider::{MESH_CONTROL_PORT, MESH_WG_PORT};
    std::net::TcpListener::bind(("0.0.0.0", MESH_CONTROL_PORT)).map_err(|_| {
        format!("port {MESH_CONTROL_PORT} (TCP) is used by another program on this computer: a main server needs it")
    })?;
    std::net::UdpSocket::bind(("0.0.0.0", MESH_WG_PORT)).map_err(|_| {
        format!("port {MESH_WG_PORT} (UDP) is used by another program on this computer: a main server needs it")
    })?;
    Ok(())
}

pub fn arch() -> String {
    std::env::consts::ARCH.to_string()
}

/// Everything the recommendation needs, measured now.
pub fn capabilities(data_dir: &Path, virtualization: Virtualization, provider: Option<String>) -> HostCapabilities {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let total = sys.total_memory();
    let avail = sys.available_memory();
    let cpus = std::thread::available_parallelism().ok().map(|n| n.get() as u32);
    let (on_battery, _) = power();
    HostCapabilities {
        os: host_os(),
        os_version: os_version(),
        arch: arch(),
        logical_cpus: cpus,
        memory_total_bytes: (total > 0).then_some(total),
        memory_available_bytes: (avail > 0).then_some(avail),
        disk_free_bytes: disk_free(data_dir),
        disk_rotational: rotational(data_dir),
        on_battery,
        virtualization,
        provider,
        measured_at: chrono::Utc::now().timestamp(),
    }
}

/// What the guest's agent is told (every 20 s).
pub fn host_status(
    data_dir: &Path,
    going_down: Option<String>,
    grant: Option<&vercelify_core::host::EnvironmentGrant>,
) -> HostStatus {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let (on_battery, battery_percent) = power();
    HostStatus {
        measured_at: chrono::Utc::now().timestamp(),
        on_battery: on_battery.unwrap_or(false),
        battery_percent,
        thermal_pressure: thermal_pressure(),
        disk_free_bytes: disk_free(data_dir),
        going_down,
        memory_available_bytes: (sys.total_memory() > 0).then_some(sys.available_memory()),
        environment_memory_bytes: grant.map(|g| g.memory_bytes),
        provider_overhead_bytes: grant.map(|_| vercelify_core::host::PROVIDER_OVERHEAD),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pmset_battery() {
        let b = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=23461987)\t53%; discharging; 4:00 remaining present: true\n";
        assert_eq!(parse_pmset_batt(b), (Some(true), Some(53)));
        let ac =
            "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; 0:00 remaining present: true\n";
        assert_eq!(parse_pmset_batt(ac), (Some(false), Some(100)));
        assert_eq!(
            parse_pmset_batt("Now drawing from 'AC Power'\n"),
            (Some(false), None),
            "a Mac mini"
        );
        assert_eq!(parse_pmset_batt(""), (None, None));
    }

    #[test]
    fn pmset_thermal() {
        let calm =
            "Note: No thermal warning level has been recorded\nNote: No performance warning level has been recorded\n";
        assert!(!parse_pmset_therm(calm));
        assert!(parse_pmset_therm(
            "CPU_Scheduler_Limit \t= 100\nCPU_Available_CPUs \t= 8\nCPU_Speed_Limit \t= 62\n"
        ));
        assert!(!parse_pmset_therm("CPU_Speed_Limit \t= 100\n"));
        assert!(parse_pmset_therm(
            "Thermal warning level set to 2.\nthermal warning level: 2\n"
        ));
    }

    #[test]
    fn diskutil() {
        assert_eq!(parse_diskutil_ssd("   Solid State:               Yes\n"), Some(false));
        assert_eq!(parse_diskutil_ssd("   Solid State:               No\n"), Some(true));
        assert_eq!(parse_diskutil_ssd("nothing"), None);
    }

    #[test]
    fn measures_this_computer() {
        let d = tempfile::tempdir().unwrap();
        let c = capabilities(&d.path().join("not/yet"), Virtualization::Ready, None);
        assert!(c.memory_total_bytes.unwrap() > 0);
        assert!(
            c.disk_free_bytes.unwrap() > 0,
            "measured on the nearest existing parent"
        );
        assert!(c.logical_cpus.unwrap() >= 1);
    }
}

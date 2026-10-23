//! What kind of machine this is and how it is coping: board profile,
//! Raspberry Pi throttling, memory pressure and OOM kills. Mirrors
//! `host/lib/hardware.sh` (same profiles and thresholds).

use crate::model::{HardwareInfo, MediaKind, Pressure, Throttling};
use crate::sysfs::read_trim;
use std::path::{Path, PathBuf};

/// Where the readings come from; overridable for fixture tests.
#[derive(Debug, Clone)]
pub struct HwPaths {
    pub device_tree_model: PathBuf,
    pub throttled: PathBuf,
    pub cgroup_controllers: PathBuf,
    pub proc: PathBuf,
    pub power_supply: PathBuf,
    pub watchdog: PathBuf,
}

impl Default for HwPaths {
    fn default() -> Self {
        Self {
            device_tree_model: "/proc/device-tree/model".into(),
            throttled: "/sys/devices/platform/soc/soc:firmware/get_throttled".into(),
            cgroup_controllers: "/sys/fs/cgroup/cgroup.controllers".into(),
            proc: "/proc".into(),
            power_supply: "/sys/class/power_supply".into(),
            watchdog: "/dev/watchdog".into(),
        }
    }
}

/// Defaults that depend on the board (same values as hardware.sh).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProfileDefaults {
    pub pause_c: f32,
    pub resume_c: f32,
    pub shutdown_c: f32,
}

pub fn is_pi(profile: &str) -> bool {
    profile.starts_with("pi")
}

pub fn defaults(profile: &str) -> ProfileDefaults {
    if is_pi(profile) {
        ProfileDefaults {
            pause_c: 78.0,
            resume_c: 68.0,
            shutdown_c: 85.0,
        }
    } else {
        ProfileDefaults {
            pause_c: 92.0,
            resume_c: 80.0,
            shutdown_c: 98.0,
        }
    }
}

/// `pi5 | pi4 | pi | laptop | pc` from the device-tree model and batteries.
pub fn profile_from(model: &str, has_battery: bool) -> String {
    let p = if model.contains("Raspberry Pi 5") || model.contains("Raspberry Pi Compute Module 5") {
        "pi5"
    } else if model.contains("Raspberry Pi 4")
        || model.contains("Raspberry Pi Compute Module 4")
        || model.contains("Raspberry Pi 400")
    {
        "pi4"
    } else if model.contains("Raspberry Pi") {
        "pi"
    } else if has_battery {
        "laptop"
    } else {
        "pc"
    };
    p.into()
}

/// Decode the firmware's `get_throttled` bits ("0x50005" or "50005").
pub fn parse_throttled(raw: &str) -> Option<Throttling> {
    let hex = raw.trim().trim_start_matches("throttled=").trim_start_matches("0x");
    let bits = u32::from_str_radix(hex, 16).ok()?;
    Some(Throttling {
        raw: bits,
        under_voltage_now: bits & 0x1 != 0,
        throttled_now: bits & 0x4 != 0,
        soft_temp_limit_now: bits & 0x8 != 0,
        under_voltage_since_boot: bits & 0x10000 != 0,
        throttled_since_boot: bits & 0x40000 != 0,
    })
}

/// `some avg10` of a `/proc/pressure/*` file.
pub fn parse_psi_some_avg10(raw: &str) -> Option<f32> {
    raw.lines()
        .find(|l| l.starts_with("some "))?
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix("avg10="))?
        .parse()
        .ok()
}

/// `oom_kill` counter from `/proc/vmstat`.
pub fn parse_oom_kills(vmstat: &str) -> Option<u64> {
    vmstat
        .lines()
        .find_map(|l| l.strip_prefix("oom_kill "))
        .and_then(|v| v.trim().parse().ok())
}

fn has_battery(power_supply: &Path) -> bool {
    crate::sysfs::children(power_supply).iter().any(|d| {
        read_trim(d.join("type")).as_deref() == Some("Battery")
            && !read_trim(d.join("scope")).is_some_and(|s| s.eq_ignore_ascii_case("device"))
    })
}

pub fn read(paths: &HwPaths, root_media: MediaKind) -> HardwareInfo {
    let model = std::fs::read(&paths.device_tree_model)
        .map(|b| {
            String::from_utf8_lossy(&b)
                .trim_matches(char::from(0))
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let profile = profile_from(&model, has_battery(&paths.power_supply));
    let memory_cgroup = match std::fs::read_to_string(&paths.cgroup_controllers) {
        Ok(c) => c.split_whitespace().any(|w| w == "memory"),
        // cgroup v1 or not Linux: limits work the old way.
        Err(_) => true,
    };
    let psi = |name: &str| {
        std::fs::read_to_string(paths.proc.join("pressure").join(name))
            .ok()
            .and_then(|r| parse_psi_some_avg10(&r))
    };
    let pressure = match (psi("cpu"), psi("memory"), psi("io")) {
        (None, None, None) => None,
        (cpu, memory, io) => Some(Pressure {
            cpu_some_avg10: cpu,
            memory_some_avg10: memory,
            io_some_avg10: io,
        }),
    };
    HardwareInfo {
        throttling: is_pi(&profile)
            .then(|| read_trim(&paths.throttled).and_then(|r| parse_throttled(&r)))
            .flatten(),
        defaults_pause_c: defaults(&profile).pause_c,
        profile,
        model,
        root_media,
        memory_cgroup,
        pressure,
        oom_kills: std::fs::read_to_string(paths.proc.join("vmstat"))
            .ok()
            .and_then(|v| parse_oom_kills(&v)),
        watchdog: paths.watchdog.exists(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles() {
        assert_eq!(profile_from("Raspberry Pi 5 Model B Rev 1.0", false), "pi5");
        assert_eq!(profile_from("Raspberry Pi 4 Model B Rev 1.4", false), "pi4");
        assert_eq!(profile_from("Raspberry Pi 3 Model B Plus Rev 1.3", false), "pi");
        assert_eq!(profile_from("", true), "laptop");
        assert_eq!(profile_from("", false), "pc");
        assert_eq!(defaults("pi4").pause_c, 78.0);
        assert_eq!(defaults("laptop").shutdown_c, 98.0);
    }

    #[test]
    fn throttling_bits() {
        let t = parse_throttled("throttled=0x50005").unwrap();
        assert!(t.under_voltage_now && t.throttled_now && t.under_voltage_since_boot && t.throttled_since_boot);
        assert!(!t.soft_temp_limit_now);
        let ok = parse_throttled("0x0").unwrap();
        assert!(!ok.under_voltage_now && !ok.under_voltage_since_boot);
        assert!(parse_throttled("garbage").is_none());
    }

    #[test]
    fn psi_and_oom() {
        let psi = "some avg10=12.50 avg60=3.00 avg300=1.00 total=123\nfull avg10=1.00 avg60=0.00 avg300=0.00 total=9\n";
        assert_eq!(parse_psi_some_avg10(psi), Some(12.5));
        assert_eq!(parse_oom_kills("pgfault 1\noom_kill 3\n"), Some(3));
        assert_eq!(parse_oom_kills("pgfault 1\n"), None);
    }

    #[test]
    fn reads_a_pi_fixture() {
        let d = tempfile::tempdir().unwrap();
        let p = |s: &str| d.path().join(s);
        std::fs::write(p("model"), b"Raspberry Pi 4 Model B Rev 1.5\0").unwrap();
        std::fs::write(p("throttled"), "50000\n").unwrap();
        std::fs::write(p("controllers"), "cpuset cpu io pids\n").unwrap();
        std::fs::create_dir_all(p("proc/pressure")).unwrap();
        std::fs::write(p("proc/pressure/memory"), "some avg10=40.00 avg60=0 avg300=0 total=1\n").unwrap();
        std::fs::write(p("proc/vmstat"), "oom_kill 2\n").unwrap();
        let paths = HwPaths {
            device_tree_model: p("model"),
            throttled: p("throttled"),
            cgroup_controllers: p("controllers"),
            proc: p("proc"),
            power_supply: p("none"),
            watchdog: p("watchdog0"),
        };
        let hw = read(&paths, MediaKind::SdCard);
        assert_eq!(hw.profile, "pi4");
        assert_eq!(hw.model, "Raspberry Pi 4 Model B Rev 1.5");
        assert!(!hw.memory_cgroup, "memory controller missing");
        let t = hw.throttling.unwrap();
        assert!(t.under_voltage_since_boot && !t.under_voltage_now);
        assert_eq!(hw.pressure.unwrap().memory_some_avg10, Some(40.0));
        assert_eq!(hw.oom_kills, Some(2));
        assert_eq!(hw.root_media, MediaKind::SdCard);
        assert!(!hw.watchdog);
        assert_eq!(hw.defaults_pause_c, 78.0);
    }
}

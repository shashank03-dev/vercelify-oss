//! Live system collection (feature `collect`).

use crate::disks::{is_interesting_mount, media_kind, role_for_mount};
use crate::model::*;
use crate::{power, thermal};
use std::path::PathBuf;
use std::sync::Mutex;
use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, RefreshKind, System};

/// Paths the collector reads; overridable so tests and containers can point
/// at fixture trees.
#[derive(Debug, Clone)]
pub struct SysPaths {
    /// Usually `/sys/class`.
    pub sys_class: PathBuf,
    /// Usually `/sys/block`.
    pub sys_block: PathBuf,
    /// Mount point of the HDD tier.
    pub storage_mount: String,
    /// Board, throttling, pressure and OOM readings.
    pub hw: crate::hardware::HwPaths,
}

impl Default for SysPaths {
    fn default() -> Self {
        Self {
            sys_class: "/sys/class".into(),
            sys_block: "/sys/block".into(),
            storage_mount: crate::STORAGE_MOUNT.into(),
            hw: Default::default(),
        }
    }
}

/// Keeps a `System` around so CPU usage is measured between calls.
pub struct Collector {
    sys: Mutex<System>,
    pub paths: SysPaths,
    agent_version: String,
}

impl Collector {
    pub fn new(paths: SysPaths, agent_version: impl Into<String>) -> Self {
        let sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        );
        Self {
            sys: Mutex::new(sys),
            paths,
            agent_version: agent_version.into(),
        }
    }

    pub fn host(&self) -> HostInfo {
        HostInfo {
            hostname: System::host_name().unwrap_or_else(|| "unknown".into()),
            os: System::long_os_version().unwrap_or_else(|| std::env::consts::OS.into()),
            kernel: System::kernel_version().unwrap_or_default(),
            arch: std::env::consts::ARCH.into(),
            uptime_secs: System::uptime(),
            agent_version: self.agent_version.clone(),
        }
    }

    pub fn cpu_and_memory(&self) -> (CpuStats, MemoryStats) {
        let mut sys = self.sys.lock().unwrap_or_else(|e| e.into_inner());
        sys.refresh_cpu_all();
        sys.refresh_memory();
        let load = System::load_average();
        let cpu = CpuStats {
            model: sys
                .cpus()
                .first()
                .map(|c| c.brand().trim().to_string())
                .unwrap_or_default(),
            cores: sys.cpus().len() as u32,
            usage_pct: (sys.global_cpu_usage() * 10.0).round() / 10.0,
            load_avg: [load.one, load.five, load.fifteen],
        };
        let mem = MemoryStats {
            total_bytes: sys.total_memory(),
            used_bytes: sys.used_memory(),
            available_bytes: sys.available_memory(),
            swap_total_bytes: sys.total_swap(),
            swap_used_bytes: sys.used_swap(),
        };
        (cpu, mem)
    }

    pub fn disks(&self) -> Vec<DiskInfo> {
        let disks = Disks::new_with_refreshed_list();
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<DiskInfo> = disks
            .list()
            .iter()
            .filter_map(|d| {
                let device = d.name().to_string_lossy().to_string();
                let mount = d.mount_point().to_string_lossy().to_string();
                let fs = d.file_system().to_string_lossy().to_string();
                if !is_interesting_mount(&fs, &mount) || !seen.insert(mount.clone()) {
                    return None;
                }
                let mut media = media_kind(&self.paths.sys_block, &device);
                if media == MediaKind::Unknown {
                    media = match d.kind() {
                        sysinfo::DiskKind::SSD => MediaKind::Ssd,
                        sysinfo::DiskKind::HDD => MediaKind::Hdd,
                        _ => MediaKind::Unknown,
                    };
                }
                let total = d.total_space();
                let avail = d.available_space();
                Some(DiskInfo {
                    role: role_for_mount(&mount, &self.paths.storage_mount),
                    device,
                    mount_point: mount,
                    file_system: fs,
                    total_bytes: total,
                    available_bytes: avail,
                    used_bytes: total.saturating_sub(avail),
                    media,
                    removable: d.is_removable(),
                })
            })
            .collect();
        // System, then Storage, then the rest by mount point.
        out.sort_by_key(|d| {
            (
                match d.role {
                    DiskRole::System => 0,
                    DiskRole::Storage => 1,
                    DiskRole::Other => 2,
                },
                d.mount_point.clone(),
            )
        });
        out
    }

    pub fn power(&self) -> PowerStatus {
        power::read_power(self.paths.sys_class.join("power_supply"))
    }

    pub fn thermal(&self) -> ThermalStatus {
        thermal::read_thermal(&self.paths.sys_class)
    }

    pub fn snapshot(&self, power_guard: Option<PowerGuardState>) -> SystemSnapshot {
        let (cpu, memory) = self.cpu_and_memory();
        let disks = self.disks();
        let root_media = disks
            .iter()
            .find(|d| d.mount_point == "/")
            .map(|d| d.media)
            .unwrap_or(MediaKind::Unknown);
        SystemSnapshot {
            host: self.host(),
            cpu,
            memory,
            disks,
            power: self.power(),
            thermal: self.thermal(),
            power_guard,
            hardware: Some(crate::hardware::read(&self.paths.hw, root_media)),
        }
    }
}

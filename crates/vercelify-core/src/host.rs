//! The Vercelify environment on a Mac or Windows computer (cross-platform
//! hosting design): one Linux guest per physical host, run by the host
//! manager (`vercelify-host`), with the unchanged agent inside.
//!
//! This module is the shared contract: what the host manager measures, the
//! resources it recommends and the owner grants, the environment's state
//! machine, and what it tells the agent in the guest about the real
//! machine. Everything here is pure so it is tested without a hypervisor.
//! Units are explicit: bytes, and whole virtual CPUs.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const GIB: u64 = 1 << 30;
pub const MIB: u64 = 1 << 20;

/// Version of [`recommend`]'s rules; stored with every grant so a later
/// policy change never silently reinterprets an accepted one.
pub const POLICY_VERSION: u32 = 1;

/// How long a [`HostStatus`] stays trustworthy. The host manager writes it
/// every 20 s; three missed writes make physical capacity unknown. Battery and
/// thermal display may fall back to the guest, but admission waits for fresh data.
pub const HOST_STATUS_MAX_AGE_SECS: i64 = 60;

// ----- the real machine, as the guest sees it ---------------------------------

/// What the host manager tells the agent in the guest about the physical
/// computer. Inside a VM the agent's own battery, thermal and disk readings
/// describe virtual hardware, so placement's "on battery below 50 %" and
/// the thermal pause would never fire without this.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[serde(default)]
#[ts(export)]
pub struct HostStatus {
    /// Unix seconds when it was measured.
    pub measured_at: i64,
    pub on_battery: bool,
    pub battery_percent: Option<u8>,
    /// The computer is hot enough that heavy work should wait.
    pub thermal_pressure: bool,
    /// Free space on the physical disk that holds the environment's image.
    /// A sparse image's own free space is not this.
    #[ts(type = "number | null")]
    pub disk_free_bytes: Option<u64>,
    /// The computer is about to sleep or restart (an OS update), when known.
    pub going_down: Option<String>,
    /// Fresh physical availability, not the guest's free RAM.
    #[serde(default)]
    #[ts(optional)]
    pub memory_available_bytes: Option<u64>,
    /// The accepted complete environment grant.
    #[serde(default)]
    #[ts(optional)]
    pub environment_memory_bytes: Option<u64>,
    #[serde(default)]
    #[ts(optional)]
    pub provider_overhead_bytes: Option<u64>,
}

impl HostStatus {
    /// Fresh enough to believe at `now` (Unix seconds). A clock that went
    /// backwards (a future timestamp beyond a little skew) is not fresh.
    pub fn fresh(&self, now: i64) -> bool {
        let age = now - self.measured_at;
        (-5..=HOST_STATUS_MAX_AGE_SECS).contains(&age)
    }
}

// ----- measurements -------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum HostOs {
    Linux,
    Macos,
    Windows,
}

/// Whether this computer can run the environment's hypervisor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum Virtualization {
    Ready,
    /// An OS feature must be turned on (Windows Hypervisor Platform): needs
    /// the owner's approval on this computer and usually a restart.
    NeedsOsFeature {
        feature: String,
        restart: bool,
    },
    /// Virtualization is off in the firmware (BIOS/UEFI): only the owner,
    /// at this computer, can change it.
    NeedsFirmware,
    Unsupported {
        reason: String,
    },
    /// Not checked yet, or the check failed.
    Unknown,
}

/// The physical computer, measured by the host manager. `None` means the
/// measurement isn't available; it never stands for zero.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct HostCapabilities {
    pub os: HostOs,
    pub os_version: String,
    /// `x86_64` or `aarch64`.
    pub arch: String,
    pub logical_cpus: Option<u32>,
    #[ts(type = "number | null")]
    pub memory_total_bytes: Option<u64>,
    #[ts(type = "number | null")]
    pub memory_available_bytes: Option<u64>,
    /// Free space where the environment's disk would live.
    #[ts(type = "number | null")]
    pub disk_free_bytes: Option<u64>,
    /// A spinning disk (slow builds and databases).
    pub disk_rotational: Option<bool>,
    pub on_battery: Option<bool>,
    pub virtualization: Virtualization,
    /// Which provider would run the environment (`lima-vz`, `lima-qemu`,
    /// `qemu-whpx`), when one is available.
    pub provider: Option<String>,
    /// Unix seconds.
    pub measured_at: i64,
}

// ----- recommendation and grant ------------------------------------------------------

/// Resources given to the whole environment: the guest OS, Docker, the
/// proxy, builds, apps, databases, caches and logs inside it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export)]
pub struct EnvironmentGrant {
    #[ts(type = "number")]
    pub memory_bytes: u64,
    pub cpus: u32,
    /// The virtual disk's maximum size (sparse: it grows up to this).
    #[ts(type = "number")]
    pub disk_bytes: u64,
    pub policy_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct EnvironmentRecommendation {
    /// None when this computer can't host (see `blockers`).
    pub grant: Option<EnvironmentGrant>,
    /// The most the owner may choose, by the same rules.
    pub max: Option<EnvironmentGrant>,
    pub min: EnvironmentGrant,
    /// Reasons it can't host now, in plain words.
    pub blockers: Vec<String>,
    /// It can, but the owner should know.
    pub warnings: Vec<String>,
    /// The measurements this was computed from (`HostCapabilities.measured_at`).
    pub measured_at: i64,
    /// Memory the hypervisor itself uses beyond the guest's.
    #[ts(type = "number")]
    pub provider_overhead_bytes: u64,
}

/// The smallest environment that runs the engine, the proxy and a small app
/// with room for one build.
pub const MIN_MEMORY: u64 = 2 * GIB;
pub const MIN_CPUS: u32 = 1;
pub const MIN_DISK: u64 = 20 * GIB;
/// Below this the environment works but builds and databases compete with
/// each other; the owner is told this computer is better as a desktop.
pub const COMFORTABLE_MEMORY: u64 = 3 * GIB;
/// The most a recommendation offers by default (the owner may go higher up
/// to `max`).
const DEFAULT_DISK_CAP: u64 = 64 * GIB;
/// The hypervisor's own memory beyond the guest's (measured, then rounded
/// up; refined per provider by qualification).
pub const PROVIDER_OVERHEAD: u64 = 256 * MIB;

/// Memory left to the computer's own OS and its owner: never less than 4 GiB
/// on macOS/Windows (2 GiB on Linux), or a third of the total if larger.
fn os_reserve(os: HostOs, total: u64) -> u64 {
    let floor = if os == HostOs::Linux { 2 * GIB } else { 4 * GIB };
    floor.max(total / 3)
}

/// Disk space left free on the computer: 10 GiB, or 10 % if larger.
fn disk_reserve(free: u64) -> u64 {
    (10 * GIB).max(free / 10)
}

/// CPUs left to the computer: 2, or a quarter if more.
fn cpu_reserve(n: u32) -> u32 {
    2.max(n / 4)
}

fn round_down(v: u64, step: u64) -> u64 {
    v / step * step
}

/// Conservative resources for the environment on this computer (policy
/// [`POLICY_VERSION`]). Never fabricates: a missing measurement is a blocker.
pub fn recommend(c: &HostCapabilities) -> EnvironmentRecommendation {
    let min = EnvironmentGrant {
        memory_bytes: MIN_MEMORY,
        cpus: MIN_CPUS,
        disk_bytes: MIN_DISK,
        policy_version: POLICY_VERSION,
    };
    let mut blockers = vec![];
    let mut warnings = vec![];
    match &c.virtualization {
        Virtualization::Ready => {}
        Virtualization::NeedsOsFeature { feature, restart } => blockers.push(format!(
            "{feature} must be turned on on this computer{}",
            if *restart { " (it needs a restart)" } else { "" }
        )),
        Virtualization::NeedsFirmware => blockers.push(
            "virtualization is off in this computer's firmware (BIOS/UEFI): turn it on there, at the computer".into(),
        ),
        Virtualization::Unsupported { reason } => blockers.push(reason.clone()),
        Virtualization::Unknown => {
            blockers.push("couldn't check whether this computer can run virtual machines".into())
        }
    }
    let (Some(total), Some(cpus), Some(free)) = (c.memory_total_bytes, c.logical_cpus, c.disk_free_bytes) else {
        blockers.push("couldn't measure this computer's memory, processors or free disk space".into());
        return EnvironmentRecommendation {
            grant: None,
            max: None,
            min,
            blockers,
            warnings,
            measured_at: c.measured_at,
            provider_overhead_bytes: PROVIDER_OVERHEAD,
        };
    };

    let mem_max = round_down(
        total
            .saturating_sub(os_reserve(c.os, total))
            .saturating_sub(PROVIDER_OVERHEAD),
        256 * MIB,
    );
    // What's free right now bounds the default (not the maximum): starting
    // a VM into memory the owner is using makes the computer swap.
    let mem_now = c
        .memory_available_bytes
        .map(|a| round_down(a.saturating_sub(PROVIDER_OVERHEAD + GIB), 256 * MIB))
        .unwrap_or(mem_max);
    let mem = mem_max.min(mem_now.max(MIN_MEMORY));
    let cpu_max = cpus
        .saturating_sub(cpu_reserve(cpus))
        .max(if cpus >= 2 { 1 } else { 0 });
    let cpu = cpu_max;
    let disk_max = round_down(free.saturating_sub(disk_reserve(free)), GIB);
    let disk = disk_max.min(DEFAULT_DISK_CAP);

    if mem_max < MIN_MEMORY {
        blockers.push(format!(
            "this computer has {} of memory: after what it needs itself, less than the {} an environment needs is left",
            human_bytes(total),
            human_bytes(MIN_MEMORY)
        ));
    } else if mem < COMFORTABLE_MEMORY {
        warnings.push(
            "this computer has little memory to spare: apps will run, but builds will be slow and it is better used as a desktop that manages another server"
                .into(),
        );
    }
    if mem_now < MIN_MEMORY && mem_max >= MIN_MEMORY {
        warnings.push(
            "much of this computer's memory is in use right now: close some programs before starting the environment"
                .into(),
        );
    }
    if cpu_max < MIN_CPUS {
        blockers.push("this computer has too few processors to share".into());
    }
    if disk_max < MIN_DISK {
        blockers.push(format!(
            "{} free on this disk: the environment needs {} and the computer keeps {} for itself",
            human_bytes(free),
            human_bytes(MIN_DISK),
            human_bytes(disk_reserve(free))
        ));
    }
    if c.disk_rotational == Some(true) {
        warnings.push("this disk is a spinning hard drive: builds and databases will be slow".into());
    }
    if c.on_battery == Some(true) {
        warnings.push("this computer is on battery: new builds wait while the battery is low, and a sleeping computer serves nothing".into());
    }
    let ok = blockers.is_empty();
    let max = EnvironmentGrant {
        memory_bytes: mem_max,
        cpus: cpu_max,
        disk_bytes: disk_max,
        policy_version: POLICY_VERSION,
    };
    EnvironmentRecommendation {
        grant: ok.then_some(EnvironmentGrant {
            memory_bytes: mem,
            cpus: cpu,
            disk_bytes: disk,
            policy_version: POLICY_VERSION,
        }),
        max: ok.then_some(max),
        min,
        blockers,
        warnings,
        measured_at: c.measured_at,
        provider_overhead_bytes: PROVIDER_OVERHEAD,
    }
}

/// Check a grant the owner chose against fresh measurements. Errors are
/// plain sentences for the owner.
pub fn validate_grant(c: &HostCapabilities, g: &EnvironmentGrant) -> Result<(), Vec<String>> {
    let r = recommend(c);
    let Some(max) = r.max else { return Err(r.blockers) };
    let mut errs = vec![];
    if g.policy_version != POLICY_VERSION {
        errs.push("these settings came from an older version: review them again".into());
    }
    if g.memory_bytes < r.min.memory_bytes || g.memory_bytes > max.memory_bytes {
        errs.push(format!(
            "memory must be between {} and {}",
            human_bytes(r.min.memory_bytes),
            human_bytes(max.memory_bytes)
        ));
    }
    if !g.memory_bytes.is_multiple_of(MIB) {
        errs.push("memory must be whole megabytes".into());
    }
    if g.cpus < r.min.cpus || g.cpus > max.cpus {
        errs.push(format!(
            "processing power must be between {} and {} processors",
            r.min.cpus, max.cpus
        ));
    }
    if g.disk_bytes < r.min.disk_bytes || g.disk_bytes > max.disk_bytes {
        errs.push(format!(
            "the storage limit must be between {} and {}",
            human_bytes(r.min.disk_bytes),
            human_bytes(max.disk_bytes)
        ));
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

/// What changing a running environment's grant needs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum GrantChange {
    None,
    /// Memory or processors: the environment restarts (its apps stop briefly).
    Restart,
    /// The disk grows; applied at the next start.
    GrowDisk,
}

/// Compare grants. Shrinking the disk is refused: no verified way to move
/// the data exists yet.
pub fn grant_change(from: &EnvironmentGrant, to: &EnvironmentGrant) -> Result<GrantChange, String> {
    if to.disk_bytes < from.disk_bytes {
        return Err("the storage limit can't be made smaller".into());
    }
    if to.memory_bytes != from.memory_bytes || to.cpus != from.cpus {
        return Ok(GrantChange::Restart);
    }
    if to.disk_bytes > from.disk_bytes {
        return Ok(GrantChange::GrowDisk);
    }
    Ok(GrantChange::None)
}

pub fn human_bytes(b: u64) -> String {
    if b >= GIB {
        let g = b as f64 / GIB as f64;
        if (g - g.round()).abs() < 0.05 {
            format!("{} GB", g.round() as u64)
        } else {
            format!("{g:.1} GB")
        }
    } else {
        format!("{} MB", b / MIB)
    }
}

// ----- the environment and its operations -----------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EnvironmentState {
    NotCreated,
    Preparing,
    Starting,
    Verifying,
    Ready,
    /// Shutting down at the owner's request.
    Stopping,
    Stopped,
    Updating,
    Removing,
    /// A prerequisite is missing (see the reason).
    Blocked,
    /// The last operation failed (see the reason and whether retry is safe).
    Failed,
}

impl EnvironmentState {
    /// The lifecycle (design §9). Any state may fail; failed and blocked go
    /// back through `preparing` (retry) or `removing`.
    pub fn can_become(self, to: EnvironmentState) -> bool {
        use EnvironmentState::*;
        if to == Failed {
            return self != NotCreated;
        }
        matches!(
            (self, to),
            (NotCreated, Preparing)
                | (NotCreated, Blocked)
                | (Blocked, Preparing)
                | (Blocked, NotCreated)
                | (Preparing, Starting)
                | (Starting, Verifying)
                | (Verifying, Ready)
                | (Ready, Stopping)
                | (Stopping, Stopped)
                | (Ready, Updating)
                | (Ready, Starting)
                | (Stopped, Starting)
                | (Stopped, Updating)
                | (Updating, Starting)
                | (Failed, Preparing)
                | (Failed, Starting)
                | (Failed, Stopping)
                | (Ready | Stopped | Failed | Blocked, Removing)
                | (Removing, NotCreated)
        )
    }

    /// An operation is in flight: no other may start.
    pub fn busy(self) -> bool {
        use EnvironmentState::*;
        matches!(self, Preparing | Starting | Verifying | Stopping | Updating | Removing)
    }
}

/// The steps shown to the owner while the environment is made.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SetupStage {
    CheckingComputer,
    PreparingEnvironment,
    StartingHosting,
    CheckingConnection,
    DeployingApp,
}

impl SetupStage {
    pub fn label(self) -> &'static str {
        match self {
            SetupStage::CheckingComputer => "Checking computer",
            SetupStage::PreparingEnvironment => "Preparing environment",
            SetupStage::StartingHosting => "Starting hosting",
            SetupStage::CheckingConnection => "Checking connection",
            SetupStage::DeployingApp => "Deploying your app",
        }
    }
}

/// The one environment on this computer, as the host manager knows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ManagedEnvironment {
    /// Stable for the life of the environment; a new one after removal.
    pub id: String,
    pub state: EnvironmentState,
    /// Bumped by every accepted change; a change must name the revision it
    /// was made against.
    pub revision: u64,
    pub provider: String,
    pub grant: Option<EnvironmentGrant>,
    /// Digest of the guest image it was made from.
    pub image: String,
    /// Why it is blocked or failed, in plain words.
    pub reason: Option<String>,
    /// For `failed`: running the operation again is safe.
    pub retry_safe: bool,
    pub last_stage: Option<SetupStage>,
    /// The operation in flight, if any.
    pub operation: Option<String>,
    /// `main` (the first server: its mesh ports are open on this computer)
    /// or `worker` (joined another main server; nothing is open).
    #[serde(default)]
    pub role: Option<String>,
}

/// `POST /v1/host/environment/pairing`: a one-use code (15 minutes) that
/// pairs Desktop with a main-server environment through this computer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct EnvironmentPairing {
    pub code: String,
    /// The address in it: this computer on its network.
    pub address: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum OperationKind {
    Create,
    Start,
    Stop,
    Resize,
    Remove,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum OperationStatus {
    Running,
    Succeeded,
    Failed,
}

/// A durable operation (journaled before it acts, so a restart resumes or
/// reports it; a replayed request returns the same one).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct HostOperation {
    pub id: String,
    pub kind: OperationKind,
    /// The caller's idempotency key.
    pub key: String,
    pub status: OperationStatus,
    pub stage: Option<SetupStage>,
    pub error: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

/// `POST /v1/host/environment`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct CreateEnvironment {
    pub idempotency_key: String,
    pub grant: EnvironmentGrant,
    /// The recommendation's `measured_at` the owner accepted.
    pub accepted_measured_at: i64,
    /// An invite (`vfyj1.…`) from the main server: the environment joins it
    /// as a worker. None: it becomes the first (main) server.
    #[serde(default)]
    #[ts(optional)]
    pub join: Option<String>,
}

/// `POST /v1/host/environment/start`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct StartEnvironment {
    pub idempotency_key: String,
}

/// `PATCH /v1/host/environment`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ChangeEnvironment {
    pub idempotency_key: String,
    pub revision: u64,
    pub grant: EnvironmentGrant,
    /// The owner agreed the environment (and its apps) restart now.
    #[serde(default)]
    pub restart_ok: bool,
}

/// `POST /v1/host/environment/stop` and `DELETE /v1/host/environment`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ConfirmedChange {
    pub idempotency_key: String,
    pub revision: u64,
    /// The owner read what stops (or is deleted) and agreed.
    pub acknowledged: bool,
}

/// Desktop's view of the host manager on its own computer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct HostLocal {
    /// `vercelify-host` is set up here (its owner token exists).
    pub installed: bool,
    /// It answered.
    pub running: bool,
    pub environment: Option<ManagedEnvironment>,
    /// Why it didn't answer, in plain words.
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(os: HostOs, mem_gib: u64, avail_gib: u64, cpus: u32, free_gib: u64) -> HostCapabilities {
        HostCapabilities {
            os,
            os_version: "14.5".into(),
            arch: "aarch64".into(),
            logical_cpus: Some(cpus),
            memory_total_bytes: Some(mem_gib * GIB),
            memory_available_bytes: Some(avail_gib * GIB),
            disk_free_bytes: Some(free_gib * GIB),
            disk_rotational: Some(false),
            on_battery: Some(false),
            virtualization: Virtualization::Ready,
            provider: Some("lima-vz".into()),
            measured_at: 1_000,
        }
    }

    #[test]
    fn a_16_gb_mac_gets_a_comfortable_environment() {
        let r = recommend(&caps(HostOs::Macos, 16, 10, 8, 200));
        assert!(r.blockers.is_empty(), "{:?}", r.blockers);
        let g = r.grant.unwrap();
        // 16 - max(4, 5.33) - 0.25 = 10.4 → 10.25 GiB, under what's free
        // (10 - 1.25 = 8.75): 8.75.
        assert_eq!(g.memory_bytes, 8 * GIB + 768 * MIB);
        assert_eq!(g.cpus, 6);
        assert_eq!(g.disk_bytes, 64 * GIB, "capped by default");
        let max = r.max.unwrap();
        assert_eq!(max.disk_bytes, 180 * GIB);
        assert!(validate_grant(&caps(HostOs::Macos, 16, 10, 8, 200), &g).is_ok());
    }

    #[test]
    fn an_8_gb_windows_laptop_is_told_the_truth() {
        let r = recommend(&caps(HostOs::Windows, 8, 4, 4, 100));
        assert!(r.blockers.is_empty());
        let g = r.grant.unwrap();
        // 8 - 4 - 0.25 → 3.75 max; free now 4 - 1.25 = 2.75.
        assert_eq!(g.memory_bytes, 2 * GIB + 768 * MIB);
        assert_eq!(g.cpus, 2);
        assert!(
            r.warnings.iter().any(|w| w.contains("better used as a desktop")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn a_4_gb_machine_cannot_host() {
        let r = recommend(&caps(HostOs::Windows, 4, 3, 2, 100));
        assert!(r.grant.is_none() && r.max.is_none());
        assert!(r.blockers.iter().any(|b| b.contains("memory")), "{:?}", r.blockers);
    }

    #[test]
    fn missing_measurements_are_not_zeros() {
        let mut c = caps(HostOs::Macos, 16, 10, 8, 200);
        c.memory_total_bytes = None;
        let r = recommend(&c);
        assert!(r.grant.is_none());
        assert!(r.blockers.iter().any(|b| b.contains("couldn't measure")));
    }

    #[test]
    fn a_full_disk_blocks_and_virtualization_off_blocks() {
        let r = recommend(&caps(HostOs::Macos, 16, 10, 8, 25));
        assert!(r.grant.is_none(), "25 GB free - 10 reserved < 20");
        let mut c = caps(HostOs::Windows, 16, 10, 8, 200);
        c.virtualization = Virtualization::NeedsFirmware;
        assert!(recommend(&c).blockers.iter().any(|b| b.contains("firmware")));
        c.virtualization = Virtualization::NeedsOsFeature {
            feature: "Windows Hypervisor Platform".into(),
            restart: true,
        };
        assert!(recommend(&c).blockers[0].contains("restart"));
    }

    #[test]
    fn hard_drives_and_batteries_warn() {
        let mut c = caps(HostOs::Windows, 16, 10, 8, 200);
        c.disk_rotational = Some(true);
        c.on_battery = Some(true);
        let r = recommend(&c);
        assert!(r.grant.is_some());
        assert_eq!(r.warnings.len(), 2);
    }

    #[test]
    fn owner_choices_are_validated() {
        let c = caps(HostOs::Macos, 16, 10, 8, 200);
        let mut g = recommend(&c).grant.unwrap();
        g.memory_bytes = 15 * GIB;
        g.cpus = 8;
        g.disk_bytes = 10 * GIB;
        let errs = validate_grant(&c, &g).unwrap_err();
        assert_eq!(errs.len(), 3, "{errs:?}");
        let mut old = recommend(&c).grant.unwrap();
        old.policy_version = 0;
        assert!(validate_grant(&c, &old).is_err());
    }

    #[test]
    fn grant_changes() {
        let a = EnvironmentGrant {
            memory_bytes: 4 * GIB,
            cpus: 2,
            disk_bytes: 40 * GIB,
            policy_version: 1,
        };
        assert_eq!(grant_change(&a, &a), Ok(GrantChange::None));
        assert_eq!(
            grant_change(&a, &EnvironmentGrant { cpus: 3, ..a }),
            Ok(GrantChange::Restart)
        );
        assert_eq!(
            grant_change(
                &a,
                &EnvironmentGrant {
                    disk_bytes: 50 * GIB,
                    ..a
                }
            ),
            Ok(GrantChange::GrowDisk)
        );
        assert!(grant_change(
            &a,
            &EnvironmentGrant {
                disk_bytes: 30 * GIB,
                ..a
            }
        )
        .is_err());
    }

    #[test]
    fn lifecycle() {
        use EnvironmentState::*;
        for (a, b) in [
            (NotCreated, Preparing),
            (Preparing, Starting),
            (Starting, Verifying),
            (Verifying, Ready),
            (Ready, Stopping),
            (Stopping, Stopped),
            (Stopped, Starting),
            (Failed, Preparing),
            (Ready, Removing),
            (Removing, NotCreated),
        ] {
            assert!(a.can_become(b), "{a:?} -> {b:?}");
        }
        for (a, b) in [
            (NotCreated, Ready),
            (Preparing, Ready),
            (Removing, Ready),
            (NotCreated, Failed),
            (Preparing, Removing),
        ] {
            assert!(!a.can_become(b), "{a:?} -> {b:?}");
        }
        assert!(Preparing.busy() && Stopping.busy() && !Ready.busy() && !Failed.busy());
    }

    #[test]
    fn host_status_freshness() {
        let s = HostStatus {
            measured_at: 1_000,
            ..HostStatus::default()
        };
        assert!(s.fresh(1_000) && s.fresh(1_060));
        assert!(!s.fresh(1_061), "stale");
        assert!(!s.fresh(990), "from the future");
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_bytes(8 * GIB), "8 GB");
        assert_eq!(human_bytes(2 * GIB + 768 * MIB), "2.8 GB");
        assert_eq!(human_bytes(512 * MIB), "512 MB");
    }
}

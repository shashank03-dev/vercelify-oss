//! Core library shared by `vercelify-agent` (runs on the Linux host) and the
//! Tauri cockpit (runs on the Mac). Everything that parses host state lives
//! here so it can be unit-tested against fixture trees instead of real sysfs.

pub mod avfoundation;
pub mod disks;
pub mod hardware;
pub mod model;
pub mod pairing;
pub mod platform;
pub mod power;
pub mod stream;
pub mod sysfs;
pub mod tailscale;
pub mod thermal;
pub mod volumes;

#[cfg(feature = "collect")]
pub mod collect;

pub use model::*;

/// Default TCP port of the host agent.
pub const DEFAULT_AGENT_PORT: u16 = 7420;
/// Mount point of the HDD storage tier.
pub const STORAGE_MOUNT: &str = "/mnt/storage";
/// Root of HDD-tier bind-mounted volumes.
pub const HDD_VOLUME_ROOT: &str = "/mnt/storage/volumes";
/// Root of Docker named volumes (SSD tier).
pub const SSD_VOLUME_ROOT: &str = "/var/lib/docker/volumes";

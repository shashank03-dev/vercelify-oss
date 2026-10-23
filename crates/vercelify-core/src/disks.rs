//! Disk classification for the SSD/HDD storage tiers.

use crate::model::{DiskRole, MediaKind};
use crate::sysfs::read_parse;
use std::path::Path;

/// Pseudo / overlay filesystems that never represent real capacity.
const IGNORED_FS: &[&str] = &[
    "overlay",
    "tmpfs",
    "devtmpfs",
    "squashfs",
    "proc",
    "sysfs",
    "cgroup",
    "cgroup2",
    "nsfs",
    "fuse.lxcfs",
    "efivarfs",
    "autofs",
    "ramfs",
    "devpts",
    "tracefs",
    "debugfs",
];

/// Mount prefixes that are views of an already-listed disk.
const IGNORED_MOUNT_PREFIXES: &[&str] = &[
    "/var/lib/docker/",
    "/run/",
    "/snap/",
    "/proc",
    "/sys",
    "/dev",
    "/boot/efi",
];

pub fn is_interesting_mount(fs: &str, mount: &str) -> bool {
    if IGNORED_FS.contains(&fs) {
        return false;
    }
    !IGNORED_MOUNT_PREFIXES
        .iter()
        .any(|p| mount == p.trim_end_matches('/') || mount.starts_with(p))
}

pub fn role_for_mount(mount: &str, storage_mount: &str) -> DiskRole {
    if mount == "/" {
        DiskRole::System
    } else if mount == storage_mount || mount.starts_with(&format!("{storage_mount}/")) {
        DiskRole::Storage
    } else {
        DiskRole::Other
    }
}

/// Map a partition device (`/dev/sda1`, `/dev/nvme0n1p2`, `/dev/mmcblk0p2`)
/// to its parent block device name (`sda`, `nvme0n1`, `mmcblk0`).
pub fn parent_block_device(device: &str) -> Option<String> {
    let name = device.strip_prefix("/dev/")?;
    if name.starts_with("mapper/") || name.contains('/') {
        return None;
    }
    if name.starts_with("nvme") || name.starts_with("mmcblk") || name.starts_with("loop") {
        // nvme0n1p2 -> nvme0n1, mmcblk0p2 -> mmcblk0
        return Some(match name.rfind('p') {
            Some(i) if i > 0 && name[i + 1..].chars().all(|c| c.is_ascii_digit()) && i + 1 < name.len() => {
                name[..i].to_string()
            }
            _ => name.to_string(),
        });
    }
    Some(name.trim_end_matches(|c: char| c.is_ascii_digit()).to_string())
}

/// Determine SSD vs HDD via `/sys/block/<dev>/queue/rotational`.
pub fn media_kind(sys_block: impl AsRef<Path>, device: &str) -> MediaKind {
    let Some(parent) = parent_block_device(device) else {
        return MediaKind::Unknown;
    };
    if parent.starts_with("nvme") {
        return MediaKind::Ssd;
    }
    // SD cards and eMMC report rotational=0, but they are not SSDs.
    if parent.starts_with("mmcblk") {
        return MediaKind::SdCard;
    }
    match read_parse::<u8>(sys_block.as_ref().join(&parent).join("queue/rotational")) {
        Some(1) => MediaKind::Hdd,
        Some(0) => MediaKind::Ssd,
        _ => MediaKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_devices() {
        assert_eq!(parent_block_device("/dev/sda1").as_deref(), Some("sda"));
        assert_eq!(parent_block_device("/dev/sdb").as_deref(), Some("sdb"));
        assert_eq!(parent_block_device("/dev/nvme0n1p2").as_deref(), Some("nvme0n1"));
        assert_eq!(parent_block_device("/dev/nvme0n1").as_deref(), Some("nvme0n1"));
        assert_eq!(parent_block_device("/dev/mmcblk0p2").as_deref(), Some("mmcblk0"));
        assert_eq!(parent_block_device("/dev/mapper/root"), None);
        assert_eq!(parent_block_device("overlay"), None);
    }

    #[test]
    fn roles() {
        assert_eq!(role_for_mount("/", "/mnt/storage"), DiskRole::System);
        assert_eq!(role_for_mount("/mnt/storage", "/mnt/storage"), DiskRole::Storage);
        assert_eq!(role_for_mount("/mnt/storage2", "/mnt/storage"), DiskRole::Other);
        assert_eq!(role_for_mount("/home", "/mnt/storage"), DiskRole::Other);
    }

    #[test]
    fn filters_pseudo_mounts() {
        assert!(is_interesting_mount("ext4", "/"));
        assert!(is_interesting_mount("ext4", "/mnt/storage"));
        assert!(!is_interesting_mount("overlay", "/var/lib/docker/overlay2/x/merged"));
        assert!(!is_interesting_mount("ext4", "/var/lib/docker/volumes/x"));
        assert!(!is_interesting_mount("vfat", "/boot/efi"));
        assert!(!is_interesting_mount("tmpfs", "/run"));
    }

    #[test]
    fn media_from_rotational() {
        let dir = tempfile::tempdir().unwrap();
        for (dev, rot) in [("sda", "0"), ("sdb", "1")] {
            let q = dir.path().join(dev).join("queue");
            std::fs::create_dir_all(&q).unwrap();
            std::fs::write(q.join("rotational"), rot).unwrap();
        }
        assert_eq!(media_kind(dir.path(), "/dev/sda2"), MediaKind::Ssd);
        assert_eq!(media_kind(dir.path(), "/dev/sdb1"), MediaKind::Hdd);
        assert_eq!(media_kind(dir.path(), "/dev/sdc1"), MediaKind::Unknown);
        assert_eq!(media_kind(dir.path(), "/dev/nvme0n1p1"), MediaKind::Ssd);
        assert_eq!(media_kind(dir.path(), "/dev/mmcblk0p2"), MediaKind::SdCard);
    }
}

//! Volume creation on the SSD (named volume) or HDD (bind mount) tier.

use crate::config::Config;
use crate::docker;
use crate::error::AppError;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use vercelify_core::volumes::resolve;
use vercelify_core::{StorageTier, VolumeEntry, VolumeRequest, VolumeResult};

/// True if `path` is a mount point (different device than its parent).
pub fn is_mount_point(path: &Path) -> bool {
    let (Ok(m), Some(parent)) = (std::fs::metadata(path), path.parent()) else {
        return false;
    };
    let Ok(pm) = std::fs::metadata(parent) else {
        return false;
    };
    m.dev() != pm.dev() || m.ino() == pm.ino()
}

pub async fn create(cfg: &Config, req: VolumeRequest) -> Result<VolumeResult, AppError> {
    let (source, host_path) =
        resolve(req.tier, &req.service, req.name.as_deref()).map_err(|e| AppError::bad_request(e.to_string()))?;
    match req.tier {
        StorageTier::Ssd => {
            let created = docker::volume_create(&cfg.docker_bin, &source, &req.service).await?;
            Ok(VolumeResult {
                tier: req.tier,
                source,
                host_path,
                created,
            })
        }
        StorageTier::Hdd => {
            if cfg.require_storage_mount && !is_mount_point(Path::new(&cfg.storage_mount)) {
                return Err(AppError::conflict(format!(
                    "{} is not mounted — attach the HDD and run `sudo vercelify doctor`",
                    cfg.storage_mount
                )));
            }
            // The resolved path always starts with the default root; re-root it
            // onto the configured root (identical in production).
            let rel = host_path
                .strip_prefix(vercelify_core::HDD_VOLUME_ROOT)
                .unwrap_or(&host_path)
                .trim_start_matches('/');
            let path = cfg.hdd_volume_root.join(rel);
            let existed = path.is_dir();
            std::fs::create_dir_all(&path)
                .map_err(|e| AppError::internal(format!("creating {}: {e}", path.display())))?;
            if req.uid.is_some() || req.gid.is_some() {
                std::os::unix::fs::chown(&path, req.uid, req.gid)
                    .map_err(|e| AppError::internal(format!("chown {}: {e}", path.display())))?;
            }
            let p = path.to_string_lossy().into_owned();
            Ok(VolumeResult {
                tier: req.tier,
                source: p.clone(),
                host_path: p,
                created: !existed,
            })
        }
    }
}

pub async fn list(cfg: &Config) -> Result<Vec<VolumeEntry>, AppError> {
    let mut out = Vec::new();
    if let Ok(services) = std::fs::read_dir(&cfg.hdd_volume_root) {
        let mut services: Vec<_> = services.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).collect();
        services.sort_by_key(|e| e.file_name());
        for s in services {
            out.push(VolumeEntry {
                tier: StorageTier::Hdd,
                name: s.file_name().to_string_lossy().into_owned(),
                host_path: s.path().to_string_lossy().into_owned(),
                size_bytes: None,
            });
        }
    }
    // Docker may be unavailable; HDD listing is still useful.
    if let Ok(names) = docker::volume_names(&cfg.docker_bin).await {
        for n in names {
            out.push(VolumeEntry {
                tier: StorageTier::Ssd,
                host_path: format!("{}/{n}/_data", vercelify_core::SSD_VOLUME_ROOT),
                name: n,
                size_bytes: None,
            });
        }
    }
    Ok(out)
}

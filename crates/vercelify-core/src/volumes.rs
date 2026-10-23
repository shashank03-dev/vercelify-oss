//! Storage-tier volume naming rules. The cockpit mirrors these rules in
//! `cockpit/src/lib/compose.ts`; both are tested against
//! `tests/fixtures/volume-vectors.json`.

use crate::model::StorageTier;
use crate::{HDD_VOLUME_ROOT, SSD_VOLUME_ROOT};
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum VolumeError {
    #[error("service name must be 1-63 chars of a-z, 0-9, '-', '_' or '.', starting with a letter or digit")]
    InvalidService,
    #[error("volume name must be relative path segments of a-z, A-Z, 0-9, '-', '_' or '.' (no '..')")]
    InvalidName,
}

pub fn validate_service(service: &str) -> Result<(), VolumeError> {
    let ok = (1..=63).contains(&service.len())
        && service
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && service
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(VolumeError::InvalidService)
    }
}

/// Normalise an optional sub-directory; returns segments joined by `/`.
pub fn normalize_name(name: Option<&str>) -> Result<Option<String>, VolumeError> {
    let Some(raw) = name.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let segs: Vec<&str> = raw.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() || raw.len() > 200 {
        return Err(VolumeError::InvalidName);
    }
    for s in &segs {
        if *s == "."
            || *s == ".."
            || !s
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(VolumeError::InvalidName);
        }
    }
    Ok(Some(segs.join("/")))
}

/// Docker named volume used for the SSD tier: `<service>-<name with / as ->`.
pub fn named_volume(service: &str, name: Option<&str>) -> String {
    match name {
        Some(n) => format!("{service}-{}", n.replace('/', "-")),
        None => format!("{service}-data"),
    }
}

/// Returns `(source, host_path)` for a volume request.
///
/// * SSD: source is a named volume; host path is Docker's `_data` dir.
/// * HDD: source and host path are the same absolute bind-mount path.
pub fn resolve(tier: StorageTier, service: &str, name: Option<&str>) -> Result<(String, String), VolumeError> {
    validate_service(service)?;
    let name = normalize_name(name)?;
    Ok(match tier {
        StorageTier::Ssd => {
            let vol = named_volume(service, name.as_deref());
            let path = format!("{SSD_VOLUME_ROOT}/{vol}/_data");
            (vol, path)
        }
        StorageTier::Hdd => {
            let path = match name {
                Some(n) => format!("{HDD_VOLUME_ROOT}/{service}/{n}"),
                None => format!("{HDD_VOLUME_ROOT}/{service}"),
            };
            (path.clone(), path)
        }
    })
}

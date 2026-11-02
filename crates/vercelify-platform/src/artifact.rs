//! Bounded Docker-save archive validation. No archive entry is extracted on the
//! host. Docker receives one manifest, config and its layers, without an OCI
//! index that could supply alternate tags. Files stay in private temporary storage.
use crate::{Error, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

pub const MAX_ARCHIVE_BYTES: u64 = 3 * 1024 * 1024 * 1024;
const MAX_METADATA: u64 = 1024 * 1024;
fn bad() -> Error {
    Error::Invalid("invalid image archive (one job tag, regular files, complete SHA256 content required)".into())
}
fn hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn digest(s: &str) -> bool {
    s.strip_prefix("sha256:").is_some_and(hex)
}
fn octal(raw: &[u8]) -> Result<u64> {
    let text = std::str::from_utf8(raw).map_err(|_| bad())?.trim_matches(['\0', ' ']);
    u64::from_str_radix(if text.is_empty() { "0" } else { text }, 8).map_err(|_| bad())
}
#[derive(Clone)]
struct Entry {
    header: [u8; 512],
    offset: u64,
    size: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Manifest {
    config: String,
    repo_tags: Vec<String>,
    layers: Vec<String>,
}
fn metadata(file: &mut File, entry: &Entry) -> Result<Vec<u8>> {
    if entry.size > MAX_METADATA {
        return Err(bad());
    }
    file.seek(SeekFrom::Start(entry.offset))?;
    let mut bytes = vec![0; entry.size as usize];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn allowed_name(name: &str) -> bool {
    matches!(
        name,
        "manifest.json" | "index.json" | "oci-layout" | "blobs/" | "blobs/sha256/"
    ) || name.strip_prefix("blobs/sha256/").is_some_and(hex)
        || name.strip_suffix(".json").is_some_and(hex)
        || name
            .split_once('/')
            .is_some_and(|(id, file)| hex(id) && matches!(file, "" | "layer.tar" | "VERSION" | "json"))
}

pub struct ImageIdentity {
    config: serde_json::Value,
    diff_ids: Vec<String>,
}

/// Docker 29's image Id can name an OCI index. Verify actual loaded runtime
/// configuration and rootfs identities instead of conflating it with config SHA.
pub fn matches_loaded(identity: &ImageIdentity, loaded: &serde_json::Value, platform: &str) -> bool {
    let Some((os, arch)) = platform.split_once('/') else {
        return false;
    };
    let layers = loaded["RootFS"]["Layers"].as_array().map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect::<Vec<_>>()
    });
    if loaded["Os"] != os || loaded["Architecture"] != arch || layers.as_ref() != Some(&identity.diff_ids) {
        return false;
    }
    let empty = |v: &serde_json::Value| {
        v.is_null()
            || v.as_array().is_some_and(Vec::is_empty)
            || v.as_object().is_some_and(serde_json::Map::is_empty)
            || v.as_str() == Some("")
    };
    [
        "Cmd",
        "Entrypoint",
        "Env",
        "WorkingDir",
        "User",
        "ExposedPorts",
        "Labels",
        "StopSignal",
        "Healthcheck",
        "Shell",
        "OnBuild",
        "Volumes",
    ]
    .iter()
    .all(|key| {
        let actual = &loaded["Config"][key];
        let wanted = &identity.config[key];
        actual == wanted || (empty(actual) && empty(wanted))
    })
}

/// Produce a legacy Docker-save tar with only this job's image. Bounded metadata
/// and a 64 KiB copy/hash buffer keep memory independent of layer size.
pub fn sanitize(input: &Path, output: &Path, tag: &str, expected: &str, platform: &str) -> Result<ImageIdentity> {
    if !digest(expected) {
        return Err(bad());
    }
    let mut file = File::open(input)?;
    let total = file.metadata()?.len();
    if total > MAX_ARCHIVE_BYTES || total % 512 != 0 {
        return Err(bad());
    }
    let mut entries = BTreeMap::new();
    loop {
        let mut header = [0; 512];
        if file.read_exact(&mut header).is_err() {
            return Err(bad());
        }
        if header.iter().all(|b| *b == 0) {
            let mut rest = [0; 512];
            file.read_exact(&mut rest).map_err(|_| bad())?;
            if rest.iter().any(|b| *b != 0) {
                return Err(bad());
            }
            break;
        }
        let checksum = octal(&header[148..156])?;
        let computed: u64 = header
            .iter()
            .enumerate()
            .map(|(i, b)| if (148..156).contains(&i) { 32 } else { u64::from(*b) })
            .sum();
        if checksum != computed || header[345..500].iter().any(|b| *b != 0) {
            return Err(bad());
        }
        let length = header[..100].iter().position(|b| *b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&header[..length]).map_err(|_| bad())?.to_string();
        if !allowed_name(&name) || !matches!(header[156], 0 | b'0' | b'5') || entries.len() > 512 {
            return Err(bad());
        }
        let size = octal(&header[124..136])?;
        if size > MAX_ARCHIVE_BYTES || (header[156] == b'5' && size != 0) {
            return Err(bad());
        }
        let offset = file.stream_position()?;
        let padded = size.div_ceil(512) * 512;
        if offset.checked_add(padded).is_none_or(|end| end > total) {
            return Err(bad());
        }
        if entries.insert(name, Entry { header, offset, size }).is_some() {
            return Err(bad());
        }
        file.seek(SeekFrom::Current(padded as i64))?;
    }
    // Reject non-zero trailing data; it could otherwise be interpreted differently.
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if buffer[..n].iter().any(|b| *b != 0) {
            return Err(bad());
        }
    }
    let manifest_entry = entries.get("manifest.json").ok_or_else(bad)?;
    let manifests: Vec<Manifest> = serde_json::from_slice(&metadata(&mut file, manifest_entry)?).map_err(|_| bad())?;
    if manifests.len() != 1 {
        return Err(bad());
    }
    let manifest = &manifests[0];
    if manifest.repo_tags != [tag] || manifest.layers.len() > 256 {
        return Err(bad());
    }
    let config_entry = entries.get(&manifest.config).ok_or_else(bad)?;
    let config = metadata(&mut file, config_entry)?;
    if format!("sha256:{:x}", Sha256::digest(&config)) != expected {
        return Err(bad());
    }
    let config: serde_json::Value = serde_json::from_slice(&config).map_err(|_| bad())?;
    let (os, arch) = platform.split_once('/').ok_or_else(bad)?;
    if config["os"].as_str() != Some(os) || config["architecture"].as_str() != Some(arch) {
        return Err(Error::Invalid(
            "uploaded image does not match the job's Linux architecture".into(),
        ));
    }
    // Referenced layers must be distinct, ordinary files, with valid content IDs.
    let mut names = vec!["manifest.json".to_string(), manifest.config.clone()];
    names.extend(manifest.layers.clone());
    let unique: std::collections::BTreeSet<_> = names.iter().collect();
    if unique.len() != names.len() {
        return Err(bad());
    }
    let mut out = File::create(output)?;
    for name in names {
        let entry = entries.get(&name).ok_or_else(bad)?;
        if !matches!(entry.header[156], 0 | b'0') {
            return Err(bad());
        }
        file.seek(SeekFrom::Start(entry.offset))?;
        out.write_all(&entry.header)?;
        let mut remain = entry.size;
        let mut hash = Sha256::new();
        while remain > 0 {
            let size = remain.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..size])?;
            hash.update(&buffer[..size]);
            out.write_all(&buffer[..size])?;
            remain -= size as u64;
        }
        if let Some(id) = name.strip_prefix("blobs/sha256/") {
            if format!("{:x}", hash.finalize()) != id {
                return Err(bad());
            }
        }
        let padding = (512 - entry.size % 512) % 512;
        out.write_all(&[0; 512][..padding as usize])?;
    }
    out.write_all(&[0; 1024])?;
    out.sync_all()?;
    let diff_ids = config["rootfs"]["diff_ids"]
        .as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|v| v.as_str().filter(|s| digest(s)).map(String::from).ok_or_else(bad))
        .collect::<Result<Vec<_>>>()?;
    if diff_ids.len() != manifest.layers.len() {
        return Err(bad());
    }
    Ok(ImageIdentity {
        config: config["config"].clone(),
        diff_ids,
    })
}

/// An unguessable mode-0600 file, removed on all return/drop paths.
pub fn private_upload(dir: &Path) -> Result<tempfile::NamedTempFile> {
    Ok(tempfile::Builder::new().prefix("handoff-").tempfile_in(dir)?)
}

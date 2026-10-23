//! Authorising the main host's Coolify SSH key on a worker, so one Coolify
//! can build and run apps on every old laptop in the pool.

use crate::error::AppError;
use std::io::Write;
use std::path::Path;

const KEY_TYPES: &[&str] = &[
    "ssh-ed25519",
    "ssh-rsa",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
];

/// `type base64 [comment]` on a single line.
pub fn valid_public_key(key: &str) -> bool {
    let mut parts = key.split_whitespace();
    let (Some(kind), Some(blob)) = (parts.next(), parts.next()) else {
        return false;
    };
    key.len() <= 16 * 1024
        && !key.contains(['\n', '\r'])
        && KEY_TYPES.contains(&kind)
        && blob.len() >= 32
        && blob
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
        && parts.all(|c| c.chars().all(|ch| ch.is_ascii_graphic()))
}

/// The `type blob` identity of a key, ignoring options and comments.
fn identity(line: &str) -> Option<(String, String)> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let i = words.iter().position(|w| KEY_TYPES.contains(w))?;
    Some((words[i].to_string(), words.get(i + 1)?.to_string()))
}

/// Append `key` to `file` unless it is already there. Returns true if added.
pub fn authorize(file: &Path, key: &str) -> Result<bool, AppError> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let key = key.trim();
    if !valid_public_key(key) {
        return Err(AppError::bad_request("not an OpenSSH public key"));
    }
    let want = identity(key);
    let existing = std::fs::read_to_string(file).unwrap_or_default();
    if existing.lines().any(|l| identity(l) == want) {
        return Ok(false);
    }
    if let Some(dir) = file.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| AppError::internal(format!("creating {}: {e}", dir.display())))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(file)
        .map_err(|e| AppError::internal(format!("opening {}: {e}", file.display())))?;
    let sep = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    f.write_all(format!("{sep}{key}\n").as_bytes())
        .map_err(|e| AppError::internal(e.to_string()))?;
    let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHkLyFOm2v3pq6tYp2kQ0Q8Xc1sR2mJ3dZy7vC9p0Abc coolify";

    #[test]
    fn validates() {
        assert!(valid_public_key(KEY));
        assert!(valid_public_key("ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQDx7q"));
        assert!(!valid_public_key("ssh-ed25519"));
        assert!(!valid_public_key(
            "command=\"rm -rf /\" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHkLyFOm2v3pq6tYp2kQ0Q8"
        ));
        assert!(!valid_public_key("ssh-dss AAAAB3NzaC1kc3MAAACBAP"));
        assert!(!valid_public_key(&format!("{KEY}\nssh-ed25519 AAAA")));
    }

    #[test]
    fn appends_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".ssh/authorized_keys");
        assert!(authorize(&file, KEY).unwrap());
        assert!(!authorize(&file, KEY).unwrap(), "idempotent");
        // same key with another comment/options is the same key
        let other_comment = KEY.replace("coolify", "root@main");
        assert!(!authorize(&file, &other_comment).unwrap());
        let raw = std::fs::read_to_string(&file).unwrap();
        assert_eq!(raw.lines().count(), 1);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(authorize(&file, "junk").is_err());
    }

    #[test]
    fn keeps_existing_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("authorized_keys");
        std::fs::write(&file, "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQDx7qExisting me@laptop").unwrap();
        assert!(authorize(&file, KEY).unwrap());
        let raw = std::fs::read_to_string(&file).unwrap();
        assert_eq!(raw.lines().count(), 2);
        assert!(raw.starts_with("ssh-rsa"));
    }
}

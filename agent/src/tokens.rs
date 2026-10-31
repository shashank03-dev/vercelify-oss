//! Agent tokens per paired device, and the audit log.
//!
//! The host keeps its own token (`agent.token`, used by `sudo vercelify`).
//! Every pairing mints a separate token for that device, stored here only as
//! a SHA-256 hash, so one Mac can be revoked without re-pairing the others.
//! Every change made through the API is appended to the audit log.

use crate::config::Config;
use crate::watch::write_private;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Instant, SystemTime};
use subtle::ConstantTimeEq;
use vercelify_core::platform::Role;
use vercelify_core::{AuditEntry, DeviceToken};

/// Who made a request: `admin` (the host's token) or a device token id, and
/// what that token may do.
#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    pub id: String,
    pub role: Role,
}

impl Actor {
    pub fn admin() -> Self {
        Self {
            id: "admin".into(),
            role: Role::Owner,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct StoredToken {
    id: String,
    name: String,
    /// Hex SHA-256 of the secret.
    hash: String,
    created_at: String,
    #[serde(default)]
    last_used_at: Option<String>,
    /// Tokens from before roles existed are owners (they could do everything).
    #[serde(default)]
    role: Role,
    /// A token minted inside a pairing code stops working if it's never used
    /// by then, so a code left in a log or a scrollback goes stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unused_expires_at: Option<String>,
}

impl StoredToken {
    /// Never used and past its first-use deadline.
    fn stale(&self, used: bool, now: chrono::DateTime<chrono::Utc>) -> bool {
        !used
            && self.last_used_at.is_none()
            && self
                .unused_expires_at
                .as_deref()
                .and_then(|e| chrono::DateTime::parse_from_rfc3339(e).ok())
                .is_some_and(|e| e < now)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TokenFile {
    tokens: Vec<StoredToken>,
}

pub fn sha256_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("OS RNG available");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

// CLI pair/revoke and HTTP requests are separate processes/threads. Hold a
// shared advisory lock across the whole read-modify-write transaction.
fn lock_file(cfg: &Config) -> std::io::Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(parent) = cfg.tokens_file.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(cfg.tokens_file.with_extension("lock"))?;
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(file);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn read_file(cfg: &Config) -> std::io::Result<TokenFile> {
    match std::fs::read(&cfg.tokens_file) {
        Ok(data) => serde_json::from_slice(&data).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TokenFile::default()),
        Err(e) => Err(e),
    }
}

fn write_file(cfg: &Config, f: &TokenFile) -> std::io::Result<()> {
    write_private(&cfg.tokens_file, &serde_json::to_vec_pretty(f).expect("serializable"))
}

/// Mint an owner token for a device. Returns (record, secret). (Pairing
/// codes use `create_for_pairing`.)
#[cfg(test)]
pub fn create(cfg: &Config, name: &str) -> std::io::Result<(DeviceToken, String)> {
    create_with_role(cfg, name, Role::Owner)
}

pub fn create_with_role(cfg: &Config, name: &str, role: Role) -> std::io::Result<(DeviceToken, String)> {
    mint(cfg, name, role, None)
}

/// An owner token for a pairing code: it must be used within `unused_for`.
pub fn create_for_pairing(
    cfg: &Config,
    name: &str,
    unused_for: std::time::Duration,
) -> std::io::Result<(DeviceToken, String)> {
    // Stored to the second: rounded up, never shorter than asked.
    let by = chrono::Utc::now()
        + chrono::Duration::from_std(unused_for).unwrap_or(chrono::Duration::days(1))
        + chrono::Duration::seconds(1);
    mint(
        cfg,
        name,
        Role::Owner,
        Some(by.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    )
}

fn mint(
    cfg: &Config,
    name: &str,
    role: Role,
    unused_expires_at: Option<String>,
) -> std::io::Result<(DeviceToken, String)> {
    let secret = format!("vfyd_{}", random_hex(32));
    let name: String = name.chars().filter(|c| !c.is_control()).take(80).collect();
    let t = StoredToken {
        id: random_hex(4),
        name: if name.trim().is_empty() { "device".into() } else { name },
        hash: sha256_hex(&secret),
        created_at: now(),
        last_used_at: None,
        role,
        unused_expires_at,
    };
    let _lock = lock_file(cfg)?;
    let mut f = read_file(cfg)?;
    // Codes nobody claimed in time: their tokens go.
    let now = chrono::Utc::now();
    f.tokens.retain(|t| !t.stale(false, now));
    f.tokens.push(t.clone());
    write_file(cfg, &f)?;
    Ok((public(&t, None), secret))
}

fn public(t: &StoredToken, current: Option<&str>) -> DeviceToken {
    DeviceToken {
        id: t.id.clone(),
        name: t.name.clone(),
        created_at: t.created_at.clone(),
        last_used_at: t.last_used_at.clone(),
        current: current == Some(t.id.as_str()),
        role: t.role,
    }
}

pub fn list(cfg: &Config) -> Vec<StoredTokenView> {
    read_file(cfg)
        .unwrap_or_default()
        .tokens
        .into_iter()
        .map(StoredTokenView)
        .collect()
}

/// Opaque view for the CLI.
pub struct StoredTokenView(StoredToken);

impl StoredTokenView {
    pub fn line(&self) -> String {
        format!(
            "{}  {:<28} created {}  last used {}",
            self.0.id,
            self.0.name,
            self.0.created_at,
            self.0.last_used_at.as_deref().unwrap_or("never")
        )
    }
}

pub fn revoke(cfg: &Config, id: &str) -> std::io::Result<bool> {
    let _lock = lock_file(cfg)?;
    let mut f = read_file(cfg)?;
    let before = f.tokens.len();
    f.tokens.retain(|t| t.id != id);
    if f.tokens.len() == before {
        return Ok(false);
    }
    write_file(cfg, &f)?;
    Ok(true)
}

/// Verifies tokens; caches the token file and notes when each was used.
pub struct Tokens {
    admin: String,
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    /// (modified, size) of the token file when it was read.
    mtime: Option<(SystemTime, u64)>,
    tokens: Vec<StoredToken>,
    used: HashMap<String, String>,
    flushed: Option<Instant>,
}

impl Tokens {
    pub fn new(admin: String) -> Self {
        Self {
            admin,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// Who presented `secret`, if anyone.
    pub fn verify(&self, cfg: &Config, secret: &str) -> Option<Actor> {
        if !secret.is_empty() && bool::from(secret.as_bytes().ct_eq(self.admin.as_bytes())) {
            return Some(Actor::admin());
        }
        if !secret.starts_with("vfyd_") {
            return None;
        }
        let hash = sha256_hex(secret);
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let stamp = |cfg: &Config| {
            std::fs::metadata(&cfg.tokens_file)
                .ok()
                .and_then(|m| Some((m.modified().ok()?, m.len())))
        };
        let mtime = stamp(cfg);
        if mtime != c.mtime {
            c.tokens = read_file(cfg).unwrap_or_default().tokens;
            c.mtime = mtime;
        }
        let (id, role, first_use) = c
            .tokens
            .iter()
            .find(|t| bool::from(t.hash.as_bytes().ct_eq(hash.as_bytes())))
            .filter(|t| !t.stale(c.used.contains_key(&t.id), chrono::Utc::now()))
            .map(|t| {
                let first = t.last_used_at.is_none() && !c.used.contains_key(&t.id);
                (t.id.clone(), t.role, first)
            })?;
        c.used.insert(id.clone(), now());
        // Note last use on disk at most every 10 minutes (the first use at
        // once: it lifts a pairing token's deadline).
        if first_use || c.flushed.is_none_or(|t| t.elapsed().as_secs() > 600) {
            if let Ok(_lock) = lock_file(cfg) {
                if let Ok(mut f) = read_file(cfg) {
                    for t in &mut f.tokens {
                        if let Some(u) = c.used.get(&t.id) {
                            t.last_used_at = Some(u.clone());
                        }
                    }
                    if write_file(cfg, &f).is_ok() {
                        c.mtime = stamp(cfg);
                        c.tokens = f.tokens;
                    }
                }
            }
            c.flushed = Some(Instant::now());
        }
        Some(Actor { id, role })
    }

    pub fn list(&self, cfg: &Config, current: &Actor) -> Vec<DeviceToken> {
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        read_file(cfg)
            .unwrap_or_default()
            .tokens
            .iter()
            .map(|t| {
                let mut d = public(t, Some(&current.id));
                if let Some(u) = c.used.get(&t.id) {
                    d.last_used_at = Some(u.clone());
                }
                d
            })
            .collect()
    }
}

/// Append one line to the audit log (JSON lines; rotated at 2 MB).
pub fn audit(cfg: &Config, actor: &str, method: &str, path: &str, status: u16) {
    let entry = AuditEntry {
        at: now(),
        actor: actor.into(),
        method: method.into(),
        // Query strings can carry nothing secret today, but keep paths only.
        path: path.split('?').next().unwrap_or(path).to_string(),
        status,
    };
    let path = &cfg.audit_log;
    if std::fs::metadata(path).is_ok_and(|m| m.len() > 2 * 1024 * 1024) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    use std::os::unix::fs::OpenOptionsExt;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
    {
        let _ = writeln!(f, "{}", serde_json::to_string(&entry).unwrap_or_default());
    }
}

/// The newest `limit` audit entries, newest first.
pub fn audit_tail(cfg: &Config, limit: usize) -> Vec<AuditEntry> {
    let raw = std::fs::read_to_string(&cfg.audit_log).unwrap_or_default();
    raw.lines()
        .rev()
        .filter_map(|l| serde_json::from_str(l).ok())
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &std::path::Path) -> Config {
        Config {
            tokens_file: dir.join("tokens.json"),
            audit_log: dir.join("audit.log"),
            ..Config::default()
        }
    }

    #[test]
    fn concurrent_pairings_preserve_every_device() {
        let d = tempfile::tempdir().unwrap();
        let cfg = std::sync::Arc::new(cfg(d.path()));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(24));
        let handles: Vec<_> = (0..24)
            .map(|n| {
                let cfg = cfg.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    create(&cfg, &format!("device-{n}"))
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(results.iter().all(Result::is_ok), "concurrent writes failed");
        assert_eq!(
            list(&cfg).len(),
            24,
            "a successful pairing must not discard another device"
        );
    }

    #[test]
    fn corrupt_registry_is_not_overwritten_by_pairing_or_revocation() {
        let d = tempfile::tempdir().unwrap();
        let cfg = cfg(d.path());
        std::fs::write(&cfg.tokens_file, b"{ damaged registry").unwrap();
        assert!(create(&cfg, "new device").is_err());
        assert!(revoke(&cfg, "old-device").is_err());
        assert_eq!(std::fs::read(&cfg.tokens_file).unwrap(), b"{ damaged registry");
    }

    #[test]
    fn device_tokens() {
        let d = tempfile::tempdir().unwrap();
        let cfg = cfg(d.path());
        let tokens = Tokens::new("admin-token-0123456789".into());
        assert_eq!(tokens.verify(&cfg, "admin-token-0123456789"), Some(Actor::admin()));
        assert_eq!(tokens.verify(&cfg, "vfyd_nope"), None);
        assert_eq!(tokens.verify(&cfg, ""), None);

        let (t, secret) = create(&cfg, "MacBook").unwrap();
        assert!(secret.starts_with("vfyd_"));
        let raw = std::fs::read_to_string(&cfg.tokens_file).unwrap();
        assert!(!raw.contains(&secret), "only the hash is stored");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&cfg.tokens_file).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let actor = tokens.verify(&cfg, &secret).unwrap();
        assert_eq!(actor.id, t.id);
        assert_eq!(actor.role, Role::Owner);
        let l = tokens.list(&cfg, &actor);
        assert!(l[0].current && l[0].last_used_at.is_some());

        assert!(revoke(&cfg, &t.id).unwrap());
        assert!(!revoke(&cfg, &t.id).unwrap());
        // The file changed: the cache notices.
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(tokens.verify(&cfg, &secret), None, "revoked");
    }

    #[test]
    fn roles() {
        let d = tempfile::tempdir().unwrap();
        let cfg = cfg(d.path());
        let tokens = Tokens::new("admin-token-0123456789".into());
        // A token file from before roles: its tokens stay owners.
        let secret = "vfyd_legacy";
        std::fs::write(
            &cfg.tokens_file,
            format!(
                r#"{{"tokens":[{{"id":"aa11","name":"old","hash":"{}","created_at":"2026-01-01T00:00:00Z"}}]}}"#,
                sha256_hex(secret)
            ),
        )
        .unwrap();
        assert_eq!(tokens.verify(&cfg, secret).unwrap().role, Role::Owner);
        let (t, s2) = create_with_role(&cfg, "CI", Role::Deployer).unwrap();
        assert_eq!(t.role, Role::Deployer);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let a = tokens.verify(&cfg, &s2).unwrap();
        assert_eq!((a.id.as_str(), a.role), (t.id.as_str(), Role::Deployer));
    }

    #[test]
    fn a_pairing_token_never_used_in_time_stops_working_and_is_pruned() {
        let d = tempfile::tempdir().unwrap();
        let cfg = cfg(d.path());
        let tokens = Tokens::new("admin-token-0123456789".into());
        let (_, used) = create_for_pairing(&cfg, "used in time", std::time::Duration::from_secs(2)).unwrap();
        let (_, late) = create_for_pairing(&cfg, "never used", std::time::Duration::from_secs(2)).unwrap();
        let (_, plain) = create(&cfg, "plain").unwrap();
        assert!(tokens.verify(&cfg, &used).is_some());
        std::thread::sleep(std::time::Duration::from_millis(3100));
        assert!(tokens.verify(&cfg, &late).is_none(), "an unclaimed code goes stale");
        assert!(tokens.verify(&cfg, &used).is_some(), "used once: it stays");
        assert!(tokens.verify(&cfg, &plain).is_some());
        create(&cfg, "next").unwrap();
        let names: Vec<String> = list(&cfg).iter().map(|t| t.line()).collect();
        assert!(!names.iter().any(|l| l.contains("never used")), "{names:?}");
        assert!(names.iter().any(|l| l.contains("used in time")));
    }

    #[test]
    fn audit_log() {
        let d = tempfile::tempdir().unwrap();
        let cfg = cfg(d.path());
        audit(&cfg, "admin", "POST", "/v1/containers/x/stop?y=1", 200);
        audit(&cfg, "ab12cd34", "DELETE", "/v1/tokens/zz", 404);
        let t = audit_tail(&cfg, 10);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].actor, "ab12cd34");
        assert_eq!(t[1].path, "/v1/containers/x/stop");
    }
}

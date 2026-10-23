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
use vercelify_core::{AuditEntry, DeviceToken};

/// Who made a request: `admin` (the host's token) or a device token id.
#[derive(Debug, Clone, PartialEq)]
pub struct Actor(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct StoredToken {
    id: String,
    name: String,
    /// Hex SHA-256 of the secret.
    hash: String,
    created_at: String,
    #[serde(default)]
    last_used_at: Option<String>,
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

fn read_file(cfg: &Config) -> TokenFile {
    std::fs::read_to_string(&cfg.tokens_file)
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default()
}

fn write_file(cfg: &Config, f: &TokenFile) -> std::io::Result<()> {
    write_private(&cfg.tokens_file, &serde_json::to_vec_pretty(f).expect("serializable"))
}

/// Mint a token for a device (also used by `vercelify-agent pair`).
/// Returns (record, secret).
pub fn create(cfg: &Config, name: &str) -> std::io::Result<(DeviceToken, String)> {
    let secret = format!("vfyd_{}", random_hex(32));
    let name: String = name.chars().filter(|c| !c.is_control()).take(80).collect();
    let t = StoredToken {
        id: random_hex(4),
        name: if name.trim().is_empty() { "device".into() } else { name },
        hash: sha256_hex(&secret),
        created_at: now(),
        last_used_at: None,
    };
    let mut f = read_file(cfg);
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
    }
}

pub fn list(cfg: &Config) -> Vec<StoredTokenView> {
    read_file(cfg).tokens.into_iter().map(StoredTokenView).collect()
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
    let mut f = read_file(cfg);
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
            return Some(Actor("admin".into()));
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
            c.tokens = read_file(cfg).tokens;
            c.mtime = mtime;
        }
        let id = c
            .tokens
            .iter()
            .find(|t| bool::from(t.hash.as_bytes().ct_eq(hash.as_bytes())))
            .map(|t| t.id.clone())?;
        c.used.insert(id.clone(), now());
        // Note last use on disk at most every 10 minutes.
        if c.flushed.is_none_or(|t| t.elapsed().as_secs() > 600) {
            let mut f = read_file(cfg);
            for t in &mut f.tokens {
                if let Some(u) = c.used.get(&t.id) {
                    t.last_used_at = Some(u.clone());
                }
            }
            if write_file(cfg, &f).is_ok() {
                c.mtime = stamp(cfg);
                c.tokens = f.tokens;
            }
            c.flushed = Some(Instant::now());
        }
        Some(Actor(id))
    }

    pub fn list(&self, cfg: &Config, current: &Actor) -> Vec<DeviceToken> {
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        read_file(cfg)
            .tokens
            .iter()
            .map(|t| {
                let mut d = public(t, Some(&current.0));
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
    fn device_tokens() {
        let d = tempfile::tempdir().unwrap();
        let cfg = cfg(d.path());
        let tokens = Tokens::new("admin-token-0123456789".into());
        assert_eq!(
            tokens.verify(&cfg, "admin-token-0123456789"),
            Some(Actor("admin".into()))
        );
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
        assert_eq!(actor.0, t.id);
        let l = tokens.list(&cfg, &actor);
        assert!(l[0].current && l[0].last_used_at.is_some());

        assert!(revoke(&cfg, &t.id).unwrap());
        assert!(!revoke(&cfg, &t.id).unwrap());
        // The file changed: the cache notices.
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(tokens.verify(&cfg, &secret), None, "revoked");
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

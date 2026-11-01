//! The host manager's durable state: the one environment, its operation
//! journal and the owner's token (hashed). One JSON file, replaced
//! atomically (write, sync, rename), so a crash leaves the old or the new
//! state, never half of one. A machine-wide lock file makes this process
//! the only manager of the environment, whichever OS account starts it.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use vercelify_core::host::{EnvironmentState, HostOperation, ManagedEnvironment, OperationStatus};

/// Operations kept in the journal (newest last); older finished ones go.
pub const JOURNAL_LIMIT: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    /// Bumped when the file's shape changes.
    pub version: u32,
    pub environment: ManagedEnvironment,
    pub operations: Vec<HostOperation>,
    /// SHA-256 of the owner token, hex.
    pub owner_token_sha256: String,
}

impl State {
    pub fn new(provider: &str, owner_token_sha256: String) -> Self {
        State {
            version: 1,
            environment: ManagedEnvironment {
                id: new_id("env"),
                state: EnvironmentState::NotCreated,
                revision: 0,
                provider: provider.into(),
                grant: None,
                image: String::new(),
                reason: None,
                retry_safe: true,
                last_stage: None,
                operation: None,
                role: None,
            },
            operations: vec![],
            owner_token_sha256,
        }
    }

    /// Record an operation, dropping the oldest finished ones past the limit.
    pub fn push_operation(&mut self, op: HostOperation) {
        self.operations.push(op);
        while self.operations.len() > JOURNAL_LIMIT {
            match self
                .operations
                .iter()
                .position(|o| o.status != OperationStatus::Running)
            {
                Some(i) => {
                    self.operations.remove(i);
                }
                None => break,
            }
        }
    }

    pub fn operation(&self, id: &str) -> Option<&HostOperation> {
        self.operations.iter().find(|o| o.id == id)
    }

    pub fn operation_mut(&mut self, id: &str) -> Option<&mut HostOperation> {
        self.operations.iter_mut().find(|o| o.id == id)
    }

    /// The operation an idempotency key already started, if any.
    pub fn by_key(&self, key: &str) -> Option<&HostOperation> {
        self.operations.iter().rev().find(|o| o.key == key)
    }
}

pub fn new_id(prefix: &str) -> String {
    let mut b = [0u8; 10];
    getrandom::fill(&mut b).expect("the OS has randomness");
    format!("{prefix}_{}", hex(&b))
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn sha256_hex(s: &str) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(s.as_bytes()))
}

/// Load the state, or `None` when there is none yet.
pub fn load(path: &Path) -> Result<Option<State>> {
    match std::fs::read(path) {
        Ok(raw) => serde_json::from_slice(&raw)
            .map(Some)
            .map_err(|e| Error::Internal(format!("{} is damaged: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Internal(format!("reading {}: {e}", path.display()))),
    }
}

/// Replace the state file atomically, readable by its owner only.
pub fn save(path: &Path, s: &State) -> Result<()> {
    let raw = serde_json::to_vec_pretty(s).map_err(|e| Error::Internal(e.to_string()))?;
    write_private(path, &raw)
}

/// Write a file atomically with owner-only permissions (0600 on Unix; on
/// Windows the data directory's ACL, set by the installer, applies).
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::Internal("no parent directory".into()))?;
    std::fs::create_dir_all(dir).map_err(|e| Error::Internal(format!("creating {}: {e}", dir.display())))?;
    let tmp = path.with_extension("tmp");
    {
        let mut o = OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o
            .open(&tmp)
            .map_err(|e| Error::Internal(format!("writing {}: {e}", tmp.display())))?;
        f.write_all(data)
            .and_then(|_| f.sync_all())
            .map_err(|e| Error::Internal(format!("writing {}: {e}", tmp.display())))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| Error::Internal(format!("replacing {}: {e}", path.display())))?;
    Ok(())
}

/// The machine-wide ownership lock, held for the life of the process. A
/// second host manager (another desktop install, another OS account) gets
/// a clear refusal instead of creating a second environment.
pub struct OwnerLock {
    _file: File,
    pub path: PathBuf,
}

impl OwnerLock {
    pub fn acquire(path: &Path) -> Result<OwnerLock> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| Error::Internal(format!("creating {}: {e}", d.display())))?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| Error::Internal(format!("opening {}: {e}", path.display())))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                let who = std::fs::read_to_string(path).unwrap_or_default();
                return Err(Error::Conflict(format!(
                    "another Vercelify host manager already manages this computer's environment{}",
                    if who.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", who.trim())
                    }
                )));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(Error::Internal(format!("locking {}: {e}", path.display())))
            }
        }
        let _ = file.set_len(0);
        let _ = writeln!(file, "pid {} user {}", std::process::id(), current_user());
        Ok(OwnerLock {
            _file: file,
            path: path.to_path_buf(),
        })
    }
}

fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vercelify_core::host::OperationKind;

    fn op(id: &str, status: OperationStatus) -> HostOperation {
        HostOperation {
            id: id.into(),
            kind: OperationKind::Start,
            key: format!("k-{id}"),
            status,
            stage: None,
            error: None,
            created_at: 0,
            finished_at: None,
        }
    }

    #[test]
    fn saves_atomically_and_privately() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        assert!(load(&p).unwrap().is_none());
        let s = State::new("fake", sha256_hex("t"));
        save(&p, &s).unwrap();
        assert_eq!(load(&p).unwrap().unwrap(), s);
        assert!(!p.with_extension("tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(&p, b"{not json").unwrap();
        assert!(load(&p).is_err(), "damage is reported, not replaced by a fresh state");
    }

    #[test]
    fn one_manager_per_machine() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("host.lock");
        let held = OwnerLock::acquire(&p).unwrap();
        let second = OwnerLock::acquire(&p);
        assert!(matches!(second, Err(Error::Conflict(_))), "{:?}", second.err());
        drop(held);
        // A child another test is spawning right now briefly shares open
        // files until it execs: give it a moment.
        let again = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            OwnerLock::acquire(&p).is_ok()
        });
        assert!(again, "free again once the first exits");
    }

    #[test]
    fn journal_is_bounded_but_keeps_running_ones() {
        let mut s = State::new("fake", String::new());
        s.push_operation(op("running", OperationStatus::Running));
        for i in 0..JOURNAL_LIMIT + 5 {
            s.push_operation(op(&i.to_string(), OperationStatus::Succeeded));
        }
        assert_eq!(s.operations.len(), JOURNAL_LIMIT);
        assert!(s.operation("running").is_some());
        assert!(s.operation("0").is_none());
        assert_eq!(s.by_key("k-54").unwrap().id, "54");
    }
}

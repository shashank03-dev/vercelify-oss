//! A node's mesh state on disk (`<dir>/state.json`, 0600): who it is, how
//! to reach and verify the coordinator, and the last map, so the mesh comes
//! back up after a reboot before the coordinator answers.

use crate::cert::NodeKind;
use crate::proto::CoordinatorInfo;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub node_id: String,
    pub name: String,
    pub kind: NodeKind,
    pub ipv4: String,
    pub ipv6: String,
    pub ipv4_prefix_len: u8,
    pub certificate: String,
    pub node_token: String,
    /// Ed25519 key that signs certificates and maps (base64).
    pub coordinator_key: String,
    pub coordinator: CoordinatorInfo,
    /// The coordinator's agent, reached over the mesh.
    pub api_url: String,
    /// The last map (signed), applied at start.
    #[serde(default)]
    pub map: Option<String>,
}

pub fn state_path(dir: &Path) -> PathBuf {
    dir.join("state.json")
}

pub fn key_path(dir: &Path) -> PathBuf {
    dir.join("node.key")
}

/// Write `data` to `path` with mode 0600, atomically (a temp file of this
/// write's own + rename): concurrent writers each replace the file whole.
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// An exclusive lock on a node's state (`<dir>/state.lock`, `flock`),
/// between processes too: held from reading the state to replacing it, so a
/// pairing and a running daemon can't overwrite each other's identity.
pub struct StateLock {
    /// Closing it releases the lock.
    _file: std::fs::File,
}

impl StateLock {
    pub fn acquire(dir: &Path) -> std::io::Result<StateLock> {
        std::fs::create_dir_all(dir)?;
        // Empty and opened read-only (all `flock` needs), so the daemon and a
        // pairing run by another user (the Mac app) share it.
        let path = dir.join("state.lock");
        let f = match std::fs::File::open(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                drop(
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .mode(0o644)
                        .open(&path)?,
                );
                std::fs::File::open(&path)?
            }
            other => other?,
        };
        loop {
            // SAFETY: flock on a descriptor this File owns.
            if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } == 0 {
                return Ok(StateLock { _file: f });
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
}

impl State {
    pub fn load(dir: &Path) -> std::io::Result<Option<State>> {
        match std::fs::read(state_path(dir)) {
            Ok(b) => serde_json::from_slice(&b)
                .map(Some)
                .map_err(|e| std::io::Error::other(format!("{}: {e}", state_path(dir).display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let _lock = StateLock::acquire(dir)?;
        self.save_locked(dir)
    }

    /// Save while holding [`StateLock`].
    pub fn save_locked(&self, dir: &Path) -> std::io::Result<()> {
        write_private(
            &state_path(dir),
            &serde_json::to_vec_pretty(self).expect("serializable"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn saved_privately() {
        let d = tempfile::tempdir().unwrap();
        assert!(State::load(d.path()).unwrap().is_none());
        let s = State {
            node_id: "mn_1".into(),
            name: "pi".into(),
            kind: NodeKind::Server,
            ipv4: "10.77.0.2".into(),
            ipv6: "fd76:6679:6d65::2".into(),
            ipv4_prefix_len: 16,
            certificate: "vfyc1.x.y".into(),
            node_token: "vfyn_x".into(),
            coordinator_key: "k".into(),
            coordinator: CoordinatorInfo {
                public_key: "p".into(),
                ipv4: "10.77.0.1".into(),
                endpoints: vec![],
            },
            api_url: "http://10.77.0.1:7420".into(),
            map: None,
        };
        s.save(d.path()).unwrap();
        assert_eq!(State::load(d.path()).unwrap(), Some(s));
        let mode = std::fs::metadata(state_path(d.path())).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// Regression (audit A09): writers running at once never trip over each
    /// other's temporary file, and the result is always one whole write.
    #[test]
    fn concurrent_writes_each_land_whole() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.json");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    for k in 0..100 {
                        let data = format!("{{\"writer\":{n},\"k\":{k},\"pad\":\"{}\"}}", "x".repeat(1000 + n));
                        write_private(&path, data.as_bytes()).unwrap();
                        let back: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                        assert!(back["writer"].is_u64());
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let left: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["state.json".to_string()], "no temporary files left");
    }

    #[test]
    fn the_lock_is_exclusive() {
        let d = tempfile::tempdir().unwrap();
        let held = StateLock::acquire(d.path()).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = d.path().to_path_buf();
        let t = std::thread::spawn(move || {
            let _l = StateLock::acquire(&dir).unwrap();
            tx.send(()).unwrap();
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200)).is_err(),
            "waits for the holder"
        );
        drop(held);
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        t.join().unwrap();
    }
}

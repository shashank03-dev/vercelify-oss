//! A node's mesh state on disk (`<dir>/state.json`, 0600): who it is, how
//! to reach and verify the coordinator, and the last map, so the mesh comes
//! back up after a reboot before the coordinator answers.

use crate::cert::NodeKind;
use crate::proto::CoordinatorInfo;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

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

/// Write `data` to `path` with mode 0600, atomically (temp file + rename).
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
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
}

//! The warm standby (design §8.4). Every 5 minutes the main server builds a
//! [`Snapshot`] of what it takes to be the main server (the database copy,
//! the master key, the coordinator's keys, the tokens), seals it to the
//! standby's WireGuard key (only that server can open it, and it knows it
//! came from the coordinator) and sends it there. `vercelify promote` on the
//! standby opens the last one and takes over: the coordinator's identity
//! (10.77.0.1, its keys) at a new map epoch, so every node follows it and an
//! old main server that comes back knows it was replaced.

use crate::store::{get_setting, put_setting};
use crate::{mesh_registry, nodes, Error, Result, Store};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use vercelify_core::platform::StandbyStatus;
use vercelify_mesh::addr::COORDINATOR_INDEX;
use zeroize::Zeroizing;

pub const SETTING: &str = "standby";
/// A copy older than this needs `--force` to be promoted.
pub const MAX_AGE_SECS: i64 = 3600;

/// What the main server is, in files (names relative to their place).
#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub v: u8,
    /// Unix seconds.
    pub taken_at: i64,
    /// The main server's name.
    pub from: String,
    /// The map epoch it ran at.
    pub epoch: u64,
    /// name → base64 content: `platform.db`, `master.key`,
    /// `mesh/coordinator.key`, `mesh/node.key`, `tokens.json`, `agent.token`,
    /// and when present `cloudflare.env`, `cloudflared.env` (the tunnel),
    /// `agent.toml`.
    pub files: BTreeMap<String, String>,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        for v in self.files.values_mut() {
            v.zeroize();
        }
    }
}

impl Snapshot {
    pub fn file(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        self.files
            .get(name)
            .map(|b| {
                B64.decode(b)
                    .map(Zeroizing::new)
                    .map_err(|_| Error::Invalid(format!("{name} in the snapshot is not base64")))
            })
            .transpose()
    }
}

pub fn status(c: &Connection) -> Result<StandbyStatus> {
    Ok(get_setting(c, SETTING)?.unwrap_or_default())
}

pub fn set_status(c: &Connection, s: &StandbyStatus) -> Result<()> {
    put_setting(c, SETTING, s)
}

/// Choose the standby (a worker) or turn it off.
pub fn choose(c: &Connection, node: Option<&str>) -> Result<StandbyStatus> {
    let node = node.filter(|n| !n.is_empty());
    let s = match node {
        None => StandbyStatus::default(),
        Some(n) => {
            let n = nodes::get(c, n)?;
            if n.role != "worker" {
                return Err(Error::Invalid(format!(
                    "{} is the main server: the standby is a worker",
                    n.name
                )));
            }
            StandbyStatus {
                node_id: Some(n.id),
                node: Some(n.name),
                ..StandbyStatus::default()
            }
        }
    };
    set_status(c, &s)?;
    Ok(s)
}

/// Build a snapshot: a consistent copy of the database (SQLite's backup API)
/// and the files given as (name in the snapshot, path); missing optional
/// files are skipped, missing required ones are an error.
pub fn build(store: &Store, from: &str, files: &[(&str, &Path, bool)]) -> Result<Snapshot> {
    let epoch = store.with(mesh_registry::versions)?.1;
    let tmp = tempfile_path(store)?;
    store.backup_to(&tmp)?;
    let db = std::fs::read(&tmp);
    let _ = std::fs::remove_file(&tmp);
    let mut out = BTreeMap::new();
    out.insert("platform.db".to_string(), B64.encode(db?));
    for (name, path, required) in files {
        match std::fs::read(path) {
            Ok(b) => {
                out.insert(name.to_string(), B64.encode(Zeroizing::new(b).as_slice()));
            }
            Err(e) if *required => {
                return Err(Error::Unavailable(format!("{}: {e}", path.display())));
            }
            Err(_) => {}
        }
    }
    Ok(Snapshot {
        v: 1,
        taken_at: chrono::Utc::now().timestamp(),
        from: from.to_string(),
        epoch,
        files: out,
    })
}

fn tempfile_path(store: &Store) -> Result<std::path::PathBuf> {
    let base = store
        .path()
        .ok_or_else(|| Error::Internal("the standby copy needs a database file".into()))?;
    Ok(base.with_extension("standby.tmp"))
}

/// The database fix-ups of a promotion, on the restored copy: this server's
/// worker identity goes (its mesh node and platform node; what ran on it now
/// runs on the main server, which is this one), the coordinator's address is
/// re-learned, and the map epoch moves on, so every node takes the new
/// coordinator's maps and the old main server knows it was replaced.
pub fn promote_db(c: &Connection, own_mesh_node: &str) -> Result<u64> {
    let main = nodes::main(c)?;
    let worker: Option<String> = c
        .query_row(
            "SELECT id FROM nodes WHERE mesh_node_id = ?1 AND role = 'worker'",
            [own_mesh_node],
            |r| r.get(0),
        )
        .ok();
    if let Some(w) = &worker {
        c.execute(
            "UPDATE instances SET node_id = ?2 WHERE node_id = ?1",
            params![w, main.id],
        )?;
        c.execute("UPDATE apps SET node_id = ?2 WHERE node_id = ?1", params![w, main.id])?;
        c.execute(
            "UPDATE deployments SET node_id = ?2 WHERE node_id = ?1",
            params![w, main.id],
        )?;
        c.execute("DELETE FROM nodes WHERE id = ?1", [w])?;
    }
    c.execute(
        "DELETE FROM mesh_nodes WHERE id = ?1 AND ip_index != ?2",
        params![own_mesh_node, COORDINATOR_INDEX],
    )?;
    c.execute(
        "UPDATE mesh_nodes SET endpoints = '[]' WHERE ip_index = ?1",
        [COORDINATOR_INDEX],
    )?;
    let epoch = mesh_registry::versions(c)?.1 + 1;
    put_setting(c, mesh_registry::EPOCH, &epoch)?;
    mesh_registry::bump(c)?;
    set_status(c, &StandbyStatus::default())?;
    Ok(epoch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vercelify_mesh::keys::NodeKey;

    #[test]
    fn choose_build_and_promote() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("platform.db")).unwrap();
        let key = crate::secrets::MasterKey::from_bytes(&[3u8; 32]).unwrap();
        let main_key = NodeKey::generate();
        s.tx(|c| {
            nodes::ensure_main(c, "laptop", "http://127.0.0.1:7420", "x86_64")?;
            mesh_registry::ensure_coordinator(c, "laptop", &main_key.public())?;
            c.execute(
                "INSERT INTO mesh_nodes (id, name, kind, public_key, ip_index, token_hash, endpoints, created_at)
                 VALUES ('mn_pi', 'pi', 'server', 'k', 5, 'h', '[]', 'x')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let st = vercelify_core::platform::NodeStatus::default();
        let pi = s
            .tx(|c| {
                nodes::heartbeat(
                    c,
                    &key,
                    &nodes::Heartbeat {
                        mesh_node_id: "mn_pi",
                        name: "pi",
                        mesh_ip: "10.77.0.5",
                        agent_port: 7420,
                        arch: "aarch64",
                        runtime_token: &"t".repeat(40),
                        status: &st,
                    },
                )
            })
            .unwrap();
        assert!(
            s.tx(|c| choose(c, Some("laptop"))).is_err(),
            "the main server isn't a standby"
        );
        let chosen = s.tx(|c| choose(c, Some("pi"))).unwrap();
        assert_eq!(chosen.node_id.as_deref(), Some(pi.id.as_str()));
        assert_eq!(s.tx(|c| choose(c, Some(""))).unwrap(), StandbyStatus::default());

        let secret = dir.path().join("master.key");
        std::fs::write(&secret, [9u8; 32]).unwrap();
        assert!(build(&s, "laptop", &[("master.key", &dir.path().join("nope"), true)]).is_err());
        let snap = build(
            &s,
            "laptop",
            &[
                ("master.key", &secret, true),
                ("cloudflare.env", &dir.path().join("none"), false),
            ],
        )
        .unwrap();
        assert_eq!(snap.epoch, 1);
        assert_eq!(snap.file("master.key").unwrap().unwrap().as_slice(), &[9u8; 32]);
        assert!(snap.file("cloudflare.env").unwrap().is_none());
        assert!(!dir.path().join("platform.standby.tmp").exists(), "no copy left behind");

        // Restore it elsewhere and promote: the worker identity of the
        // standby goes, the epoch moves on.
        let restored = dir.path().join("restored.db");
        std::fs::write(&restored, snap.file("platform.db").unwrap().unwrap().as_slice()).unwrap();
        let r = Store::open(&restored).unwrap();
        let epoch = r.tx(|c| promote_db(c, "mn_pi")).unwrap();
        assert_eq!(epoch, 2);
        assert_eq!(r.with(mesh_registry::versions).unwrap().1, 2);
        assert!(r.with(|c| nodes::get(c, "pi")).is_err());
        assert!(r.with(|c| mesh_registry::get(c, "pi")).is_err());
        let coord = r.with(|c| mesh_registry::get(c, "laptop")).unwrap();
        assert_eq!(coord.ip_index, COORDINATOR_INDEX);
        assert!(coord.endpoints.is_empty());
    }
}

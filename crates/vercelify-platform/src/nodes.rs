//! Servers that run apps (design §8.2, §8.3). The main server registers
//! itself at start; a worker registers with its first heartbeat, which it
//! sends every minute over the mesh with its node token, and keeps current
//! with the next ones (its resources, condition, and the token the main
//! server calls its runtime API with, sealed here).

use crate::ids::new_id;
use crate::secrets::{aad, MasterKey};
use crate::{now, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use vercelify_core::platform::{NodePatch, NodeStatus, PlatformNode};
use zeroize::Zeroizing;

/// A worker not heard from for this long is offline.
pub const ONLINE_SECS: i64 = 120;

fn online(role: &str, last_seen: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> bool {
    if role == "main" {
        return true;
    }
    last_seen
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds() < ONLINE_SECS)
}

fn row(r: &Row) -> rusqlite::Result<PlatformNode> {
    let role: String = r.get("role")?;
    let last_seen_at: Option<String> = r.get("last_seen_at")?;
    let status: String = r.get("status")?;
    Ok(PlatformNode {
        id: r.get("id")?,
        name: r.get("name")?,
        online: online(&role, last_seen_at.as_deref(), chrono::Utc::now()),
        role,
        agent_url: r.get("agent_url")?,
        arch: r.get("arch")?,
        builds: r.get("builds")?,
        last_seen_at,
        created_at: r.get("created_at")?,
        mesh_ip: r.get("mesh_ip")?,
        drained: r.get("drained")?,
        status: serde_json::from_str(&status).unwrap_or_default(),
        instances: r.get("instances")?,
    })
}

const SELECT: &str = "SELECT n.*, (SELECT count(*) FROM instances i
       JOIN deployments d ON d.id = i.deployment_id
       WHERE i.node_id = n.id AND d.status = 'healthy') AS instances
     FROM nodes n";

pub fn list(c: &Connection) -> Result<Vec<PlatformNode>> {
    let mut st = c.prepare(&format!("{SELECT} ORDER BY n.role = 'worker', n.name"))?;
    let rows = st.query_map([], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(c: &Connection, id_or_name: &str) -> Result<PlatformNode> {
    c.query_row(&format!("{SELECT} WHERE n.id = ?1 OR n.name = ?1"), [id_or_name], row)
        .optional()?
        .ok_or_else(|| Error::not_found(format!("server {id_or_name}")))
}

pub fn main(c: &Connection) -> Result<PlatformNode> {
    c.query_row(&format!("{SELECT} WHERE n.role = 'main'"), [], row)
        .optional()?
        .ok_or_else(|| Error::Unavailable("the main server isn't registered yet".into()))
}

/// The main server's own row, created or refreshed at start.
pub fn ensure_main(c: &Connection, name: &str, agent_url: &str, arch: &str) -> Result<PlatformNode> {
    let existing: Option<String> = c
        .query_row("SELECT id FROM nodes WHERE role = 'main'", [], |r| r.get(0))
        .optional()?;
    let t = now();
    // A worker that was once called this (before a promote) gives the name up.
    c.execute(
        "UPDATE nodes SET name = name || '-old' WHERE name = ?1 AND role = 'worker'",
        [name],
    )?;
    let id = match existing {
        Some(id) => {
            c.execute(
                "UPDATE nodes SET name = ?2, agent_url = ?3, arch = ?4, last_seen_at = ?5 WHERE id = ?1",
                params![id, name, agent_url, arch, t],
            )?;
            id
        }
        None => {
            let id = new_id("node");
            c.execute(
                "INSERT INTO nodes (id, name, role, agent_url, arch, builds, last_seen_at, created_at)
                 VALUES (?1, ?2, 'main', ?3, ?4, 1, ?5, ?5)",
                params![id, name, agent_url, arch, t],
            )?;
            id
        }
    };
    get(c, &id)
}

/// The main server's own condition (it sends itself no heartbeat).
pub fn set_main_status(c: &Connection, mesh_ip: &str, status: &NodeStatus) -> Result<()> {
    c.execute(
        "UPDATE nodes SET mesh_ip = ?1, status = ?2, last_seen_at = ?3 WHERE role = 'main'",
        params![mesh_ip, serde_json::to_string(status)?, now()],
    )?;
    Ok(())
}

pub struct Heartbeat<'a> {
    /// The worker's mesh node (`mn_…`), from its authenticated token.
    pub mesh_node_id: &'a str,
    pub name: &'a str,
    pub mesh_ip: &'a str,
    pub agent_port: u16,
    pub arch: &'a str,
    pub runtime_token: &'a str,
    pub status: &'a NodeStatus,
}

/// Record a worker's heartbeat, registering it the first time.
pub fn heartbeat(c: &Connection, key: &MasterKey, h: &Heartbeat) -> Result<PlatformNode> {
    if h.runtime_token.len() < 32 {
        return Err(Error::Invalid("runtime token too short".into()));
    }
    let existing: Option<(String, String)> = c
        .query_row(
            "SELECT id, role FROM nodes WHERE mesh_node_id = ?1 OR (mesh_node_id IS NULL AND name = ?2)",
            params![h.mesh_node_id, h.name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let url = format!("http://{}:{}", h.mesh_ip, h.agent_port);
    let status = serde_json::to_string(h.status)?;
    let t = now();
    let id = match existing {
        Some((_, role)) if role == "main" => {
            return Err(Error::Conflict(format!(
                "{} is the main server; it doesn't send heartbeats",
                h.name
            )))
        }
        Some((id, _)) => {
            c.execute(
                "UPDATE nodes SET name = ?2, agent_url = ?3, arch = ?4, mesh_node_id = ?5, mesh_ip = ?6,
                   status = ?7, last_seen_at = ?8 WHERE id = ?1",
                params![id, h.name, url, h.arch, h.mesh_node_id, h.mesh_ip, status, t],
            )?;
            id
        }
        None => {
            let id = new_id("node");
            c.execute(
                "INSERT INTO nodes (id, name, role, agent_url, arch, builds, mesh_node_id, mesh_ip, status,
                   last_seen_at, created_at)
                 VALUES (?1, ?2, 'worker', ?3, ?4, 0, ?5, ?6, ?7, ?8, ?8)",
                params![id, h.name, url, h.arch, h.mesh_node_id, h.mesh_ip, status, t],
            )?;
            id
        }
    };
    // Sealed each time only when it changed, so a heartbeat is one cheap write.
    let current = runtime_token(c, key, &id).ok().flatten();
    if current.as_deref().map(|s| s.as_str()) != Some(h.runtime_token) {
        let sealed = key.seal_str(&aad("nodes", "token", &id), h.runtime_token);
        c.execute("UPDATE nodes SET token = ?2 WHERE id = ?1", params![id, sealed])?;
    }
    get(c, &id)
}

/// The token the main server calls a worker with.
pub fn runtime_token(c: &Connection, key: &MasterKey, id: &str) -> Result<Option<Zeroizing<String>>> {
    let sealed: Option<Vec<u8>> = c
        .query_row("SELECT token FROM nodes WHERE id = ?1", [id], |r| r.get(0))
        .optional()?
        .flatten();
    match sealed {
        Some(s) => Ok(Some(key.open_string(&aad("nodes", "token", id), &s)?)),
        None => Ok(None),
    }
}

pub fn patch(c: &Connection, id_or_name: &str, p: &NodePatch) -> Result<PlatformNode> {
    let n = get(c, id_or_name)?;
    if let Some(d) = p.drained {
        c.execute("UPDATE nodes SET drained = ?2 WHERE id = ?1", params![n.id, d])?;
    }
    if let Some(b) = p.builds {
        c.execute("UPDATE nodes SET builds = ?2 WHERE id = ?1", params![n.id, b])?;
    }
    get(c, &n.id)
}

/// Forget a worker. Refused while apps are pinned to it or run on it.
pub fn remove(c: &Connection, id_or_name: &str) -> Result<()> {
    let n = get(c, id_or_name)?;
    if n.role == "main" {
        return Err(Error::Invalid("the main server can't be removed".into()));
    }
    let pinned: Vec<String> = c
        .prepare("SELECT name FROM apps WHERE node_id = ?1 ORDER BY name")?
        .query_map([&n.id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !pinned.is_empty() {
        return Err(Error::Conflict(format!(
            "{} runs {}: move them first (or drain it and redeploy them)",
            n.name,
            pinned.join(", ")
        )));
    }
    if n.instances > 0 {
        return Err(Error::Conflict(format!(
            "{} still runs {} instance(s): drain it and redeploy them first",
            n.name, n.instances
        )));
    }
    c.execute("DELETE FROM nodes WHERE id = ?1", [&n.id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn status(mem: u64) -> NodeStatus {
        NodeStatus {
            cpus: 4,
            memory_total_mb: 8000,
            memory_available_mb: mem,
            docker_ok: true,
            http_port: 80,
            ..NodeStatus::default()
        }
    }

    #[test]
    fn main_is_registered_once() {
        let s = Store::open_in_memory().unwrap();
        let a = s
            .tx(|c| ensure_main(c, "laptop", "http://127.0.0.1:7420", "x86_64"))
            .unwrap();
        let b = s
            .tx(|c| ensure_main(c, "laptop2", "http://127.0.0.1:7420", "x86_64"))
            .unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(b.name, "laptop2");
        assert!(b.online, "the main server is always online");
        assert_eq!(s.with(list).unwrap().len(), 1);
        assert_eq!(s.with(|c| get(c, "laptop2")).unwrap().id, a.id);
    }

    #[test]
    fn workers_register_by_heartbeat() {
        let s = Store::open_in_memory().unwrap();
        let key = MasterKey::from_bytes(&[7u8; 32]).unwrap();
        let token = "t".repeat(40);
        s.tx(|c| {
            c.execute(
                "INSERT INTO mesh_nodes (id, name, kind, public_key, ip_index, token_hash, created_at)
                 VALUES ('mn_1', 'pi', 'server', 'k', 5, 'h', 'x')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let hb = |tok: &str, mem: u64| {
            let st = status(mem);
            s.tx(|c| {
                heartbeat(
                    c,
                    &key,
                    &Heartbeat {
                        mesh_node_id: "mn_1",
                        name: "pi",
                        mesh_ip: "10.77.0.5",
                        agent_port: 7420,
                        arch: "aarch64",
                        runtime_token: tok,
                        status: &st,
                    },
                )
            })
        };
        let n = hb(&token, 3000).unwrap();
        assert_eq!(n.role, "worker");
        assert_eq!(n.agent_url, "http://10.77.0.5:7420");
        assert!(n.online && !n.builds && !n.drained);
        assert_eq!(n.status.memory_available_mb, 3000);
        let again = hb(&"u".repeat(40), 2000).unwrap();
        assert_eq!(again.id, n.id, "the same worker");
        let tok = s.with(|c| runtime_token(c, &key, &n.id)).unwrap().unwrap();
        assert_eq!(tok.as_str(), "u".repeat(40), "a new token replaces the old one");
        assert!(hb("short", 1).is_err());

        let p = s
            .tx(|c| {
                patch(
                    c,
                    "pi",
                    &NodePatch {
                        drained: Some(true),
                        builds: None,
                    },
                )
            })
            .unwrap();
        assert!(p.drained);

        // Revoking it on the mesh drops the token.
        s.tx(|c| crate::mesh_registry::revoke(c, "pi").map(|_| ())).unwrap();
        assert!(s.with(|c| runtime_token(c, &key, &n.id)).unwrap().is_none());
        s.tx(|c| remove(c, "pi")).unwrap();
        assert!(s.with(|c| get(c, "pi")).is_err());
    }

    #[test]
    fn offline_after_two_minutes() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(online("worker", Some("2026-10-25T11:59:00Z"), now));
        assert!(!online("worker", Some("2026-10-25T11:57:00Z"), now));
        assert!(!online("worker", None, now));
        assert!(online("main", None, now));
    }
}

//! Servers that run apps. The main server registers itself; workers are
//! added when they join its pool (M9).

use crate::ids::new_id;
use crate::{now, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use vercelify_core::platform::PlatformNode;

fn row(r: &Row) -> rusqlite::Result<PlatformNode> {
    Ok(PlatformNode {
        id: r.get("id")?,
        name: r.get("name")?,
        role: r.get("role")?,
        agent_url: r.get("agent_url")?,
        arch: r.get("arch")?,
        builds: r.get("builds")?,
        last_seen_at: r.get("last_seen_at")?,
        created_at: r.get("created_at")?,
    })
}

pub fn list(c: &Connection) -> Result<Vec<PlatformNode>> {
    let mut st = c.prepare("SELECT * FROM nodes ORDER BY role = 'worker', name")?;
    let rows = st.query_map([], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(c: &Connection, id_or_name: &str) -> Result<PlatformNode> {
    c.query_row("SELECT * FROM nodes WHERE id = ?1 OR name = ?1", [id_or_name], row)
        .optional()?
        .ok_or_else(|| Error::not_found(format!("server {id_or_name}")))
}

/// The main server's own row, created or refreshed at start.
pub fn ensure_main(c: &Connection, name: &str, agent_url: &str, arch: &str) -> Result<PlatformNode> {
    let existing: Option<String> = c
        .query_row("SELECT id FROM nodes WHERE role = 'main'", [], |r| r.get(0))
        .optional()?;
    let t = now();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

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
        assert_eq!(s.with(list).unwrap().len(), 1);
        assert_eq!(s.with(|c| get(c, "laptop2")).unwrap().id, a.id);
    }
}

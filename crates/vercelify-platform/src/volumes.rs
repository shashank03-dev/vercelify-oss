//! Persistent volumes of apps and databases, on the SSD (Docker named volume)
//! or the HDD (bind mount under the storage disk).

use crate::ids::new_id;
use crate::{now, validate, Error, Result};
use rusqlite::{params, Connection, Row};
use vercelify_core::platform::{AppVolume, AppVolumeRequest, VolumeOwner};
use vercelify_core::StorageTier;

pub const MAX_VOLUMES: usize = 10;

fn owner_str(o: VolumeOwner) -> &'static str {
    match o {
        VolumeOwner::App => "app",
        VolumeOwner::Database => "database",
    }
}

fn row(r: &Row) -> rusqlite::Result<AppVolume> {
    Ok(AppVolume {
        id: r.get("id")?,
        owner_kind: if r.get::<_, String>("owner_kind")? == "database" {
            VolumeOwner::Database
        } else {
            VolumeOwner::App
        },
        owner_id: r.get("owner_id")?,
        name: r.get("name")?,
        mount_path: r.get("mount_path")?,
        tier: if r.get::<_, String>("tier")? == "hdd" {
            StorageTier::Hdd
        } else {
            StorageTier::Ssd
        },
        host_path: r.get("host_path")?,
        created_at: r.get("created_at")?,
    })
}

pub fn list(c: &Connection, owner: VolumeOwner, owner_id: &str) -> Result<Vec<AppVolume>> {
    let mut st = c.prepare("SELECT * FROM volumes WHERE owner_kind = ?1 AND owner_id = ?2 ORDER BY name")?;
    let rows = st.query_map(params![owner_str(owner), owner_id], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn all(c: &Connection) -> Result<Vec<AppVolume>> {
    let mut st = c.prepare("SELECT * FROM volumes ORDER BY owner_kind, owner_id, name")?;
    let rows = st.query_map([], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Docker volume name / HDD directory name of a volume: unique per owner.
pub fn docker_name(owner_name: &str, volume: &str) -> String {
    format!("vfy-{owner_name}-{volume}")
}

pub fn create(c: &Connection, owner: VolumeOwner, owner_id: &str, req: &AppVolumeRequest) -> Result<AppVolume> {
    let name = req.name.trim();
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
    {
        return Err(Error::Invalid(format!(
            "`{name}` can't be a volume name (lowercase letters, digits, _ . -)"
        )));
    }
    validate::mount_path(&req.mount_path)?;
    let existing = list(c, owner, owner_id)?;
    if existing.len() >= MAX_VOLUMES {
        return Err(Error::Invalid(format!("at most {MAX_VOLUMES} volumes")));
    }
    let mount = req.mount_path.trim_end_matches('/');
    if existing.iter().any(|v| v.mount_path == mount) {
        return Err(Error::Conflict(format!("something is already mounted at {mount}")));
    }
    let tier = match req.tier.unwrap_or(StorageTier::Ssd) {
        StorageTier::Hdd => "hdd",
        StorageTier::Ssd => "ssd",
    };
    let id = new_id("vol");
    c.execute(
        "INSERT INTO volumes (id, owner_kind, owner_id, name, mount_path, tier, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, owner_str(owner), owner_id, name, mount, tier, now()],
    )
    .map_err(|e| match Error::from(e) {
        Error::Conflict(_) => Error::Conflict(format!("a volume named {name} already exists")),
        e => e,
    })?;
    Ok(c.query_row("SELECT * FROM volumes WHERE id = ?1", [&id], row)?)
}

/// Removes the record. The data stays on disk until the owner is deleted
/// (losing data on a mistyped click is worse than a leftover directory).
pub fn delete(c: &Connection, owner: VolumeOwner, owner_id: &str, id_or_name: &str) -> Result<AppVolume> {
    let v: AppVolume = c
        .query_row(
            "SELECT * FROM volumes WHERE owner_kind = ?1 AND owner_id = ?2 AND (id = ?3 OR name = ?3)",
            params![owner_str(owner), owner_id, id_or_name],
            row,
        )
        .map_err(|_| Error::not_found(format!("volume {id_or_name}")))?;
    c.execute("DELETE FROM volumes WHERE id = ?1", [&v.id])?;
    Ok(v)
}

/// After a tier move: where the data lives now.
pub fn set_tier(c: &Connection, id: &str, tier: StorageTier, host_path: Option<&str>) -> Result<()> {
    let t = match tier {
        StorageTier::Hdd => "hdd",
        StorageTier::Ssd => "ssd",
    };
    let n = c.execute(
        "UPDATE volumes SET tier = ?2, host_path = ?3 WHERE id = ?1",
        params![id, t, host_path],
    )?;
    if n == 0 {
        return Err(Error::not_found(format!("volume {id}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{apps, Store};

    fn req(name: &str, path: &str) -> AppVolumeRequest {
        AppVolumeRequest {
            name: name.into(),
            mount_path: path.into(),
            tier: None,
        }
    }

    #[test]
    fn volumes_per_owner() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| apps::create(c, &apps::tests::git_app("blog"))).unwrap();
        let v = s
            .tx(|c| create(c, VolumeOwner::App, &a.id, &req("data", "/data/")))
            .unwrap();
        assert_eq!(v.mount_path, "/data");
        assert_eq!(v.tier, StorageTier::Ssd);
        assert!(matches!(
            s.tx(|c| create(c, VolumeOwner::App, &a.id, &req("data", "/other"))),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            s.tx(|c| create(c, VolumeOwner::App, &a.id, &req("other", "/data"))),
            Err(Error::Conflict(_))
        ));
        assert!(s.tx(|c| create(c, VolumeOwner::App, &a.id, &req("Bad", "/x"))).is_err());
        assert!(s.tx(|c| create(c, VolumeOwner::App, &a.id, &req("x", "/etc"))).is_err());
        s.tx(|c| set_tier(c, &v.id, StorageTier::Hdd, Some("/mnt/storage/volumes/vfy-blog-data")))
            .unwrap();
        let listed = s.with(|c| list(c, VolumeOwner::App, &a.id)).unwrap();
        assert_eq!(listed[0].tier, StorageTier::Hdd);
        assert_eq!(docker_name("blog", "data"), "vfy-blog-data");
        s.tx(|c| delete(c, VolumeOwner::App, &a.id, "data")).unwrap();
        assert!(s.with(|c| list(c, VolumeOwner::App, &a.id)).unwrap().is_empty());
        // Deleting the app removes its volume records.
        s.tx(|c| create(c, VolumeOwner::App, &a.id, &req("data", "/data")))
            .unwrap();
        s.tx(|c| apps::delete(c, &a.id)).unwrap();
        assert!(s.with(all).unwrap().is_empty());
    }
}

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
        storage_name: r.get("storage_name")?,
        created_at: r.get("created_at")?,
    })
}

pub fn list(c: &Connection, owner: VolumeOwner, owner_id: &str) -> Result<Vec<AppVolume>> {
    let mut st = c.prepare("SELECT * FROM volumes WHERE owner_kind = ?1 AND owner_id = ?2 ORDER BY name")?;
    let rows = st.query_map(params![owner_str(owner), owner_id], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Runtime/destructive callers must validate ownership; read-only lists remain
/// available so a legacy conflict cannot stop unrelated apps or hide recovery UI.
pub fn checked_list(c: &Connection, owner: VolumeOwner, owner_id: &str) -> Result<Vec<AppVolume>> {
    let volumes = list(c, owner, owner_id)?;
    check_bindings(c, &volumes, None)?;
    Ok(volumes)
}

pub fn all(c: &Connection) -> Result<Vec<AppVolume>> {
    let mut st = c.prepare("SELECT * FROM volumes ORDER BY owner_kind, owner_id, name")?;
    let rows = st.query_map([], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(c: &Connection, id: &str) -> Result<AppVolume> {
    let v = c
        .query_row("SELECT * FROM volumes WHERE id = ?1", [id], row)
        .map_err(|_| Error::not_found(format!("volume {id}")))?;
    check_bindings(c, std::slice::from_ref(&v), None)?;
    Ok(v)
}

/// Legacy physical name. Never use this to allocate new storage.
pub fn docker_name(owner_name: &str, volume: &str) -> String {
    format!("vfy-{owner_name}-{volume}")
}

/// Resolve an immutable identity, preserving production and preview data on upgrade.
pub fn physical_name(v: &AppVolume, owner_name: &str, pr: Option<u32>) -> String {
    match (&v.storage_name, pr) {
        (Some(name), None) => name.clone(),
        (Some(name), Some(n)) => format!("{name}-pr-{n}"),
        (None, None) => docker_name(owner_name, &v.name),
        (None, Some(n)) => docker_name(&format!("{owner_name}-pr{n}"), &v.name),
    }
}

pub fn list_for_preview(c: &Connection, owner_id: &str, pr: u32) -> Result<Vec<AppVolume>> {
    let vols = checked_list(c, VolumeOwner::App, owner_id)?;
    check_bindings(c, &vols, Some(pr))?;
    Ok(vols)
}

/// Old releases could alias separate owners' data. Keep the metadata readable,
/// but refuse to mount, move or purge ambiguous storage. Never guess who owns it.
fn check_bindings(c: &Connection, targets: &[AppVolume], pr: Option<u32>) -> Result<()> {
    if targets.is_empty() {
        return Ok(());
    }
    let mut st = c.prepare(
        "SELECT v.*, COALESCE(a.name, 'db-' || d.name) AS owner_name
        FROM volumes v LEFT JOIN apps a ON v.owner_kind = 'app' AND a.id = v.owner_id
        LEFT JOIN databases d ON v.owner_kind = 'database' AND d.id = v.owner_id",
    )?;
    let all: Vec<(AppVolume, String)> = st
        .query_map([], |r| Ok((row(r)?, r.get("owner_name")?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut previews = c.prepare("SELECT app_id, pr_number FROM deployments WHERE pr_number IS NOT NULL UNION SELECT app_id, pr_number FROM previews")?;
    let previews: Vec<(String, u32)> = previews
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for target in targets {
        let Some((_, owner)) = all.iter().find(|(v, _)| v.id == target.id) else {
            continue;
        };
        let target_prs = std::iter::once(pr).chain(
            previews
                .iter()
                .filter(|(id, _)| pr.is_none() && target.owner_kind == VolumeOwner::App && *id == target.owner_id)
                .map(|(_, n)| Some(*n)),
        );
        for target_pr in target_prs {
            let name = physical_name(target, owner, target_pr);
            for (other, other_owner) in &all {
                let candidates = std::iter::once(None).chain(
                    previews
                        .iter()
                        .filter(|(id, _)| other.owner_kind == VolumeOwner::App && *id == other.owner_id)
                        .map(|(_, n)| Some(*n)),
                );
                for other_pr in candidates {
                    if other.id == target.id && other_pr == target_pr {
                        continue;
                    }
                    let same_name = name == physical_name(other, other_owner, other_pr);
                    let same_path = target_pr.is_none()
                        && other_pr.is_none()
                        && target.host_path.is_some()
                        && target.host_path == other.host_path;
                    if same_name || same_path {
                        return Err(Error::Conflict(format!(
                        "storage {name} is shared by {owner}/{} and {other_owner}/{}; back up the data and remove the ambiguous volume mapping before deploying, moving or purging it",
                        target.name, other.name)));
                    }
                }
            }
        }
    }
    Ok(())
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
        "INSERT INTO volumes (id, owner_kind, owner_id, name, mount_path, tier, created_at, storage_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            owner_str(owner),
            owner_id,
            name,
            mount,
            tier,
            now(),
            format!("vfyv2-{id}")
        ],
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
    fn ambiguous_owner_names_must_not_share_storage() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| apps::create(c, &apps::tests::git_app("api-cache"))).unwrap();
        let b = s.tx(|c| apps::create(c, &apps::tests::git_app("api"))).unwrap();
        let av = s
            .tx(|c| create(c, VolumeOwner::App, &a.id, &req("data", "/data")))
            .unwrap();
        let bv = s
            .tx(|c| create(c, VolumeOwner::App, &b.id, &req("cache-data", "/data")))
            .unwrap();
        assert_ne!(physical_name(&av, &a.name, None), physical_name(&bv, &b.name, None));
    }

    #[test]
    fn production_previews_and_databases_have_separate_storage() {
        let s = Store::open_in_memory().unwrap();
        let mut names = std::collections::HashSet::new();
        for owner in ["api", "api-pr7", "db-api-db"] {
            let a = s.tx(|c| apps::create(c, &apps::tests::git_app(owner))).unwrap();
            for name in ["data", "cache-data"] {
                let v = s
                    .tx(|c| create(c, VolumeOwner::App, &a.id, &req(name, &format!("/{name}"))))
                    .unwrap();
                assert!(names.insert(physical_name(&v, owner, None)));
                assert!(names.insert(physical_name(&v, owner, Some(7))));
            }
        }
        let key = crate::secrets::MasterKey::from_bytes(&[42; 32]).unwrap();
        let d = s
            .tx(|c| {
                crate::databases::create(
                    c,
                    &key,
                    &vercelify_core::platform::DatabaseCreate {
                        name: "api-db".into(),
                        kind: vercelify_core::platform::DatabaseKind::Redis,
                        version: None,
                        environment_id: None,
                        project_id: None,
                        public_port: None,
                        tier: None,
                        link_app: None,
                    },
                )
            })
            .unwrap();
        let v = s.with(|c| list(c, VolumeOwner::Database, &d.id)).unwrap().remove(0);
        assert!(names.insert(physical_name(&v, "db-api-db", None)));
        s.with(|c| {
            c.execute("UPDATE volumes SET storage_name = NULL", [])?;
            Ok(())
        })
        .unwrap();
        assert!(
            matches!(
                s.with(|c| checked_list(c, VolumeOwner::Database, &d.id)),
                Err(Error::Conflict(_))
            ),
            "legacy database/app aliases must also fail closed"
        );
    }

    #[test]
    fn upgrading_v4_preserves_existing_ssd_hdd_and_preview_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("platform.db");
        let c = Connection::open(&path).unwrap();
        for (_, sql) in crate::store::MIGRATIONS.iter().take(4) {
            c.execute_batch(sql).unwrap();
        }
        c.pragma_update(None, "user_version", 4).unwrap();
        let a = apps::create(&c, &apps::tests::git_app("legacy")).unwrap();
        c.execute(
            "INSERT INTO volumes (id, owner_kind, owner_id, name, mount_path, tier, host_path, created_at)
            VALUES ('vol_old', 'app', ?1, 'data', '/data', 'hdd', '/mnt/old-data', ?2)",
            params![a.id, now()],
        )
        .unwrap();
        drop(c);
        let s = Store::open(&path).unwrap();
        assert!(crate::store::backup_path(&path).exists());
        let v = s.with(|c| list(c, VolumeOwner::App, &a.id)).unwrap().remove(0);
        assert_eq!(v.storage_name, None);
        assert_eq!(v.host_path.as_deref(), Some("/mnt/old-data"));
        assert_eq!(physical_name(&v, "legacy", None), "vfy-legacy-data");
        assert_eq!(physical_name(&v, "legacy", Some(7)), "vfy-legacy-pr7-data");
        s.tx(|c| set_tier(c, &v.id, StorageTier::Ssd, None)).unwrap();
        assert_eq!(
            physical_name(&s.with(|c| get(c, &v.id)).unwrap(), "legacy", None),
            "vfy-legacy-data"
        );
    }

    #[test]
    fn legacy_aliases_are_refused_before_mount_move_or_purge() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| apps::create(c, &apps::tests::git_app("api-cache"))).unwrap();
        let b = s.tx(|c| apps::create(c, &apps::tests::git_app("api"))).unwrap();
        let av = s
            .tx(|c| create(c, VolumeOwner::App, &a.id, &req("data", "/data")))
            .unwrap();
        let bv = s
            .tx(|c| create(c, VolumeOwner::App, &b.id, &req("cache-data", "/data")))
            .unwrap();
        s.with(|c| {
            c.execute("UPDATE volumes SET storage_name = NULL", [])?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            s.with(|c| checked_list(c, VolumeOwner::App, &a.id)),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(s.with(|c| get(c, &bv.id)), Err(Error::Conflict(_))));
        assert_eq!(s.with(all).unwrap().len(), 2, "metadata stays available for recovery");
        // Removing a mapping keeps the data, and makes the survivor usable again.
        s.tx(|c| delete(c, VolumeOwner::App, &b.id, &bv.id)).unwrap();
        assert_eq!(s.with(|c| get(c, &av.id)).unwrap().id, av.id);
    }

    #[test]
    fn legacy_preview_cannot_mount_another_apps_production_data() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| apps::create(c, &apps::tests::git_app("api"))).unwrap();
        let b = s.tx(|c| apps::create(c, &apps::tests::git_app("api-pr7"))).unwrap();
        for app in [&a, &b] {
            s.tx(|c| create(c, VolumeOwner::App, &app.id, &req("data", "/data")))
                .unwrap();
        }
        s.with(|c| {
            c.execute("UPDATE volumes SET storage_name = NULL", [])?;
            Ok(())
        })
        .unwrap();
        assert!(s.with(|c| list(c, VolumeOwner::App, &a.id)).is_ok());
        assert!(matches!(
            s.with(|c| list_for_preview(c, &a.id, 7)),
            Err(Error::Conflict(_))
        ));
        assert!(s.with(|c| list_for_preview(c, &a.id, 8)).is_ok());
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

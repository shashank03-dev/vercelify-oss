//! The platform database (design §4).
//!
//! SQLite in WAL mode with one connection behind a mutex. Every query touches a
//! few rows of small tables, so a pool would add memory and buy nothing on the
//! hardware this runs on. Async callers go through [`Store::call`], which runs
//! the closure on the blocking pool.
//!
//! Migrations are embedded, run in order, one transaction each, and recorded in
//! `PRAGMA user_version`. A database written by a newer agent is refused, so an
//! agent rollback can't silently run against a schema it doesn't know; the
//! pre-migration copy (`<db>.bak`) is what `vercelify rollback` restores.

use crate::{Error, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Embedded migrations: (version, SQL). Versions are consecutive from 1.
pub const MIGRATIONS: &[(u32, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_multinode.sql")),
    (3, include_str!("../migrations/0003_git_webhooks.sql")),
];

pub fn latest_version() -> u32 {
    MIGRATIONS.last().map(|m| m.0).unwrap_or(0)
}

pub struct Store {
    conn: Mutex<Connection>,
    path: Option<PathBuf>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("path", &self.path).finish()
    }
}

fn configure(conn: &Connection, file: bool) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    if file {
        // WAL: readers (backups, the CLI's sqlite3) never block the agent.
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            tracing::warn!("platform database: journal_mode is {mode}, not WAL");
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
    }
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

impl Store {
    /// Open (creating if needed) and migrate the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let existed = path.exists();
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        restrict_permissions(path);
        configure(&conn, true)?;
        let store = Self {
            conn: Mutex::new(conn),
            path: Some(path.to_path_buf()),
        };
        if existed {
            store.check_integrity()?;
        }
        let version = store.schema_version()?;
        if existed && version > 0 && version < latest_version() {
            let bak = backup_path(path);
            store.backup_to(&bak)?;
            tracing::info!("platform database: saved {} before migrating", bak.display());
        }
        store.migrate()?;
        Ok(store)
    }

    /// A private in-memory database, migrated (tests and dry runs).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        configure(&conn, false)?;
        let store = Self {
            conn: Mutex::new(conn),
            path: None,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves the connection usable: every
        // write happens inside a transaction that was rolled back on drop.
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn schema_version(&self) -> Result<u32> {
        Ok(self.lock().query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    fn check_integrity(&self) -> Result<()> {
        let res: String = self.lock().query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if res != "ok" {
            return Err(Error::Internal(format!(
                "the platform database is damaged ({res}); restore it with `sudo vercelify restore-state`"
            )));
        }
        Ok(())
    }

    fn migrate(&self) -> Result<()> {
        let mut conn = self.lock();
        let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let latest = latest_version();
        if current > latest {
            return Err(Error::Internal(format!(
                "the platform database is version {current}, newer than this agent understands ({latest}); \
                 update the agent, or run `sudo vercelify rollback` to restore the matching database"
            )));
        }
        for (version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql).map_err(|e| {
                Error::Internal(format!(
                    "platform database migration {version} failed: {}",
                    Error::from(e)
                ))
            })?;
            tx.pragma_update(None, "user_version", version)?;
            tx.commit()?;
            tracing::info!("platform database: migrated to version {version}");
        }
        // Foreign keys are checked per statement, but a migration can still
        // leave dangling rows if it rebuilt a table; refuse to run on that.
        let broken: Option<String> = conn
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()?;
        if let Some(table) = broken {
            return Err(Error::Internal(format!(
                "platform database: broken references in {table}"
            )));
        }
        Ok(())
    }

    /// Run `f` with the connection (blocking).
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        f(&self.lock())
    }

    /// Run `f` in a transaction (blocking). Committed if `f` returns Ok,
    /// rolled back otherwise.
    pub fn tx<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Run `f` in a transaction on the blocking pool.
    pub async fn call<T, F>(self: &Arc<Self>, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction) -> Result<T> + Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.tx(f))
            .await
            .map_err(|e| Error::Internal(format!("database task: {e}")))?
    }

    /// Consistent online copy to `dest` (atomic: written next to it, then
    /// renamed), for backups, the warm standby and pre-migration safety.
    pub fn backup_to(&self, dest: &Path) -> Result<()> {
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = dest.with_extension("tmp");
        let _ = std::fs::remove_file(&tmp);
        {
            let conn = self.lock();
            let mut out = Connection::open(&tmp)?;
            let backup = rusqlite::backup::Backup::new(&conn, &mut out)?;
            backup.run_to_completion(256, Duration::from_millis(0), None)?;
        }
        restrict_permissions(&tmp);
        std::fs::rename(&tmp, dest)?;
        Ok(())
    }

    // ----- settings -------------------------------------------------------

    pub fn setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        self.with(|c| get_setting(c, key))
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.with(|c| put_setting(c, key, value))
    }
}

pub fn get_setting<T: DeserializeOwned>(c: &Connection, key: &str) -> Result<Option<T>> {
    let raw: Option<String> = c
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
        .optional()?;
    raw.map(|r| serde_json::from_str(&r).map_err(Error::from)).transpose()
}

pub fn put_setting<T: Serialize>(c: &Connection, key: &str, value: &T) -> Result<()> {
    let json = serde_json::to_string(value)?;
    c.execute(
        "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        rusqlite::params![key, json, crate::now()],
    )?;
    Ok(())
}

pub fn backup_path(db: &Path) -> PathBuf {
    let mut name = db.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".bak");
    db.with_file_name(name)
}

/// The database holds sealed secrets and token hashes: root only.
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!("could not restrict {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn tables(s: &Store) -> Vec<String> {
        s.with(|c| {
            let mut st = c.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
            let rows = st
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(rows)
        })
        .unwrap()
    }

    #[test]
    fn migrates_to_the_latest_version() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.schema_version().unwrap(), latest_version());
        let t = tables(&s);
        for want in [
            "apps",
            "app_domains",
            "database_backups",
            "databases",
            "deploy_hooks",
            "deployments",
            "env_vars",
            "environments",
            "git_sources",
            "instances",
            "mesh_nodes",
            "nodes",
            "previews",
            "projects",
            "resource_names",
            "scheduled_tasks",
            "settings",
            "volumes",
        ] {
            assert!(t.contains(&want.to_string()), "missing table {want}");
        }
    }

    #[test]
    fn migration_versions_are_consecutive() {
        for (i, (v, _)) in MIGRATIONS.iter().enumerate() {
            assert_eq!(*v as usize, i + 1);
        }
    }

    fn project_and_env(c: &Connection) -> Result<String> {
        let now = crate::now();
        c.execute(
            "INSERT INTO projects (id, name, created_at, updated_at) VALUES ('prj_1', 'Blog', ?1, ?1)",
            [&now],
        )?;
        c.execute(
            "INSERT INTO environments (id, project_id, name, created_at) VALUES ('env_1', 'prj_1', 'production', ?1)",
            [&now],
        )?;
        Ok("env_1".into())
    }

    fn insert_app(c: &Connection, id: &str, name: &str) -> Result<()> {
        let now = crate::now();
        c.execute(
            "INSERT INTO resource_names (name, owner_kind, owner_id) VALUES (?1, 'app', ?2)",
            params![name, id],
        )?;
        c.execute(
            "INSERT INTO apps (id, environment_id, name, kind, created_at, updated_at)
             VALUES (?1, 'env_1', ?2, 'git', ?3, ?3)",
            params![id, name, now],
        )?;
        Ok(())
    }

    #[test]
    fn constraints_map_to_api_errors() {
        let s = Store::open_in_memory().unwrap();
        s.with(project_and_env).unwrap();
        s.with(|c| insert_app(c, "app_1", "blog")).unwrap();

        // Duplicate name: 409.
        let e = s.tx(|c| insert_app(c, "app_2", "blog")).unwrap_err();
        assert!(matches!(e, Error::Conflict(_)), "{e:?}");
        // Not a DNS label: 400.
        let e = s.tx(|c| insert_app(c, "app_3", "Bad_Name")).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        // Unknown environment: 409 (foreign key).
        let e = s
            .with(|c| {
                c.execute(
                    "INSERT INTO resource_names (name, owner_kind, owner_id) VALUES ('x', 'app', 'app_9')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO apps (id, environment_id, name, kind, created_at, updated_at)
                     VALUES ('app_9', 'env_nope', 'x', 'git', 'now', 'now')",
                    [],
                )?;
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(e, Error::Conflict(_)), "{e:?}");
        // An environment with apps can't be deleted.
        let e = s
            .with(|c| Ok(c.execute("DELETE FROM environments WHERE id = 'env_1'", [])?))
            .unwrap_err();
        assert!(matches!(e, Error::Conflict(_)), "{e:?}");
    }

    #[test]
    fn deployments_check_their_shape() {
        let s = Store::open_in_memory().unwrap();
        s.with(project_and_env).unwrap();
        s.with(|c| insert_app(c, "app_1", "blog")).unwrap();
        let ins = |kind: &str, pr: Option<i64>, number: i64| {
            s.with(|c| {
                Ok(c.execute(
                    "INSERT INTO deployments (id, app_id, number, kind, pr_number, trigger, status, created_at)
                     VALUES (?1, 'app_1', ?2, ?3, ?4, 'push', 'queued', 'now')",
                    params![format!("dpl_{number}_{kind}"), number, kind, pr],
                )?)
            })
        };
        ins("production", None, 1).unwrap();
        assert!(ins("preview", None, 2).is_err(), "a preview needs a PR number");
        assert!(ins("production", Some(3), 3).is_err(), "production has no PR number");
        ins("preview", Some(7), 4).unwrap();
        assert!(
            matches!(ins("production", None, 1), Err(Error::Conflict(_))),
            "numbers are unique per app"
        );
        // Deleting the app removes its deployments.
        s.with(|c| Ok(c.execute("DELETE FROM apps WHERE id = 'app_1'", [])?))
            .unwrap();
        let n: i64 = s
            .with(|c| Ok(c.query_row("SELECT count(*) FROM deployments", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn transactions_roll_back_on_error() {
        let s = Store::open_in_memory().unwrap();
        let r: Result<()> = s.tx(|c| {
            project_and_env(c)?;
            Err(Error::Invalid("stop".into()))
        });
        assert!(r.is_err());
        let n: i64 = s
            .with(|c| Ok(c.query_row("SELECT count(*) FROM projects", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn settings_round_trip() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.setting::<u32>("build_concurrency").unwrap(), None);
        s.set_setting("build_concurrency", &2u32).unwrap();
        s.set_setting("build_concurrency", &1u32).unwrap();
        assert_eq!(s.setting::<u32>("build_concurrency").unwrap(), Some(1));
    }

    #[test]
    fn file_database_backup_and_version_guard() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/platform.db");
        {
            let s = Store::open(&path).unwrap();
            s.with(project_and_env).unwrap();
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            let bak = backup_path(&path);
            s.backup_to(&bak).unwrap();
            let copy = Connection::open(&bak).unwrap();
            let n: i64 = copy
                .query_row("SELECT count(*) FROM projects", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1);
        }
        // Reopen: data kept, no re-migration.
        let s = Store::open(&path).unwrap();
        let n: i64 = s
            .with(|c| Ok(c.query_row("SELECT count(*) FROM environments", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 1);
        // A database from a newer agent is refused.
        s.with(|c| Ok(c.pragma_update(None, "user_version", latest_version() + 1)?))
            .unwrap();
        drop(s);
        let e = Store::open(&path).unwrap_err();
        assert!(e.message().contains("newer than this agent"), "{e}");
    }

    #[tokio::test]
    async fn async_calls() {
        let s = Arc::new(Store::open_in_memory().unwrap());
        s.call(|c| {
            project_and_env(c)?;
            Ok(())
        })
        .await
        .unwrap();
        let name: String = s
            .call(|c| Ok(c.query_row("SELECT name FROM projects", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(name, "Blog");
    }
}

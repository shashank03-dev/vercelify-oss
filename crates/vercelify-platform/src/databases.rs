//! One-click databases (design §10): templates, records and container specs.
//!
//! Credentials are generated here and sealed. Commands the agent runs inside
//! a database container (health checks, dumps, restores) read the password
//! from the container's own environment (`sh -c '… "$POSTGRES_PASSWORD" …'`),
//! so it never appears in a process list on the host.

use crate::apps::claim_name;
use crate::docker::{ContainerSpec, Mount, MANAGED, MANAGED_LABEL};
use crate::ids::new_id;
use crate::secrets::{aad, random_token, MasterKey};
use crate::{now, projects, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use vercelify_core::platform::{Database, DatabaseCreate, DatabaseKind, DatabasePatch, Desired};
use vercelify_core::StorageTier;

/// How a database's data is backed up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupStyle {
    /// A logical dump streamed out of the container, restored by streaming
    /// it back in.
    Dump,
    /// Redis-like stores: `SAVE`, then the data directory as a tar.
    Snapshot,
    /// Files of the data directory as a tar (crash-consistent).
    Files,
}

pub struct Template {
    pub kind: DatabaseKind,
    pub default_version: &'static str,
    pub port: u16,
    pub data_path: &'static str,
    pub backup: BackupStyle,
    /// The URL variable name a linked app gets.
    pub url_var: &'static str,
}

pub fn template(kind: DatabaseKind) -> Template {
    use DatabaseKind::*;
    let t = |default_version, port, data_path, backup, url_var| Template {
        kind,
        default_version,
        port,
        data_path,
        backup,
        url_var,
    };
    match kind {
        Postgresql => t(
            "17",
            5432,
            "/var/lib/postgresql/data",
            BackupStyle::Dump,
            "DATABASE_URL",
        ),
        Mysql => t("8.4", 3306, "/var/lib/mysql", BackupStyle::Dump, "DATABASE_URL"),
        Mariadb => t("11.4", 3306, "/var/lib/mysql", BackupStyle::Dump, "DATABASE_URL"),
        Mongodb => t("8", 27017, "/data/db", BackupStyle::Dump, "MONGODB_URI"),
        Redis => t("8", 6379, "/data", BackupStyle::Snapshot, "REDIS_URL"),
        // KeyDB only publishes a multi-arch image as `latest` (the project is dormant).
        Keydb => t("latest", 6379, "/data", BackupStyle::Snapshot, "REDIS_URL"),
        Dragonfly => t("v1.25.4", 6379, "/data", BackupStyle::Snapshot, "REDIS_URL"),
        Clickhouse => t(
            "24.8",
            9000,
            "/var/lib/clickhouse",
            BackupStyle::Files,
            "CLICKHOUSE_URL",
        ),
    }
}

/// Versions are image tags: digits, dots, dashes, a leading `v`.
pub fn valid_version(v: &str) -> bool {
    v == "latest"
        || !v.is_empty()
            && v.len() <= 32
            && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            && v.bytes().next().is_some_and(|b| b.is_ascii_digit() || b == b'v')
}

pub fn image(kind: DatabaseKind, version: &str) -> String {
    use DatabaseKind::*;
    match kind {
        Postgresql => format!("postgres:{version}-alpine"),
        Mysql => format!("mysql:{version}"),
        Mariadb => format!("mariadb:{version}"),
        Mongodb => format!("mongo:{version}"),
        Redis => format!("valkey/valkey:{version}-alpine"),
        Keydb => format!("eqalpha/keydb:{version}"),
        Dragonfly => format!("docker.dragonflydb.io/dragonflydb/dragonfly:{version}"),
        Clickhouse => format!("clickhouse/clickhouse-server:{version}-alpine"),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Credentials {
    pub user: String,
    pub password: String,
    pub database: String,
}

fn credentials_aad(id: &str) -> String {
    aad("databases", "credentials", id)
}

pub fn credentials(c: &Connection, key: &MasterKey, id: &str) -> Result<Credentials> {
    let sealed: Vec<u8> = c
        .query_row("SELECT credentials FROM databases WHERE id = ?1", [id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| Error::not_found(format!("database {id}")))?;
    let raw = key.open_string(&credentials_aad(id), &sealed)?;
    Ok(serde_json::from_str(&raw)?)
}

/// Container environment of a database.
pub fn env(kind: DatabaseKind, cred: &Credentials) -> Vec<(String, String)> {
    use DatabaseKind::*;
    let v = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    let (u, p, d) = (cred.user.as_str(), cred.password.as_str(), cred.database.as_str());
    match kind {
        Postgresql => v(&[("POSTGRES_USER", u), ("POSTGRES_PASSWORD", p), ("POSTGRES_DB", d)]),
        Mysql => v(&[
            ("MYSQL_USER", u),
            ("MYSQL_PASSWORD", p),
            ("MYSQL_DATABASE", d),
            ("MYSQL_ROOT_PASSWORD", p),
        ]),
        Mariadb => v(&[
            ("MARIADB_USER", u),
            ("MARIADB_PASSWORD", p),
            ("MARIADB_DATABASE", d),
            ("MARIADB_ROOT_PASSWORD", p),
        ]),
        Mongodb => v(&[
            ("MONGO_INITDB_ROOT_USERNAME", u),
            ("MONGO_INITDB_ROOT_PASSWORD", p),
            ("MONGO_INITDB_DATABASE", d),
        ]),
        Redis | Keydb => v(&[("VFY_PASSWORD", p)]),
        Dragonfly => v(&[("DFLY_requirepass", p), ("VFY_PASSWORD", p)]),
        Clickhouse => v(&[
            ("CLICKHOUSE_USER", u),
            ("CLICKHOUSE_PASSWORD", p),
            ("CLICKHOUSE_DB", d),
            ("CLICKHOUSE_DEFAULT_ACCESS_MANAGEMENT", "1"),
        ]),
    }
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".into(), "-c".into(), script.into()]
}

/// The start command (None: the image's own). Redis-likes read their
/// password from a config file written at start, not from arguments.
pub fn command(kind: DatabaseKind) -> Option<Vec<String>> {
    use DatabaseKind::*;
    match kind {
        Redis => Some(sh(
            "printf 'requirepass \"%s\"\\nappendonly yes\\ndir /data\\n' \"$VFY_PASSWORD\" > /tmp/vfy.conf && exec \"$(command -v valkey-server || echo redis-server)\" /tmp/vfy.conf",
        )),
        Keydb => Some(sh(
            "printf 'requirepass \"%s\"\\nappendonly yes\\ndir /data\\n' \"$VFY_PASSWORD\" > /tmp/vfy.conf && exec keydb-server /tmp/vfy.conf",
        )),
        Dragonfly => Some(vec!["dragonfly".into(), "--dir".into(), "/data".into(), "--logtostderr".into()]),
        _ => None,
    }
}

/// A command (in the container) that exits 0 when the database accepts
/// connections; None = probe the port.
pub fn health_command(kind: DatabaseKind) -> Option<Vec<String>> {
    use DatabaseKind::*;
    match kind {
        Postgresql => Some(sh("pg_isready -U \"$POSTGRES_USER\" -d \"$POSTGRES_DB\" -h 127.0.0.1")),
        Mysql => Some(sh(
            "MYSQL_PWD=\"$MYSQL_PASSWORD\" mysqladmin ping -h 127.0.0.1 -u \"$MYSQL_USER\" --silent",
        )),
        Mariadb => Some(sh("healthcheck.sh --connect --innodb_initialized")),
        Mongodb => Some(sh("mongosh --quiet --eval 'db.runCommand({ping:1}).ok' | grep -q 1")),
        Redis => Some(sh(
            "REDISCLI_AUTH=\"$VFY_PASSWORD\" \"$(command -v valkey-cli || echo redis-cli)\" ping | grep -q PONG",
        )),
        Keydb => Some(sh("REDISCLI_AUTH=\"$VFY_PASSWORD\" keydb-cli ping | grep -q PONG")),
        Dragonfly => None,
        Clickhouse => Some(sh("wget -qO- http://127.0.0.1:8123/ping | grep -q Ok")),
    }
}

/// Dump to stdout / restore from stdin (inside the container), for
/// `BackupStyle::Dump` kinds.
pub fn dump_command(kind: DatabaseKind) -> Option<Vec<String>> {
    use DatabaseKind::*;
    match kind {
        Postgresql => Some(sh("pg_dump -U \"$POSTGRES_USER\" -Fc \"$POSTGRES_DB\"")),
        Mysql => Some(sh(
            "MYSQL_PWD=\"$MYSQL_PASSWORD\" mysqldump --single-transaction --routines --triggers -u \"$MYSQL_USER\" \"$MYSQL_DATABASE\"",
        )),
        Mariadb => Some(sh(
            "MYSQL_PWD=\"$MARIADB_PASSWORD\" mariadb-dump --single-transaction --routines --triggers -u \"$MARIADB_USER\" \"$MARIADB_DATABASE\"",
        )),
        Mongodb => Some(sh(
            "mongodump --archive --quiet -u \"$MONGO_INITDB_ROOT_USERNAME\" -p \"$MONGO_INITDB_ROOT_PASSWORD\" --authenticationDatabase admin",
        )),
        _ => None,
    }
}

/// The database's own shell (`psql`, `mysql`…), logged in from the
/// container's environment: the password never crosses the API.
pub fn console_command(kind: DatabaseKind) -> Vec<String> {
    use DatabaseKind::*;
    sh(match kind {
        Postgresql => "exec psql -U \"$POSTGRES_USER\" -d \"$POSTGRES_DB\"",
        Mysql => "MYSQL_PWD=\"$MYSQL_PASSWORD\" exec mysql -u \"$MYSQL_USER\" \"$MYSQL_DATABASE\"",
        Mariadb => "MYSQL_PWD=\"$MARIADB_PASSWORD\" exec mariadb -u \"$MARIADB_USER\" \"$MARIADB_DATABASE\"",
        Mongodb => {
            "exec mongosh -u \"$MONGO_INITDB_ROOT_USERNAME\" -p \"$MONGO_INITDB_ROOT_PASSWORD\" --authenticationDatabase admin \"$MONGO_INITDB_DATABASE\""
        }
        // Valkey, or Redis in a database taken over from Coolify.
        Redis => "REDISCLI_AUTH=\"$VFY_PASSWORD\" exec \"$(command -v valkey-cli || echo redis-cli)\"",
        Keydb => "REDISCLI_AUTH=\"$VFY_PASSWORD\" exec keydb-cli",
        // Dragonfly's image ships no CLI; a shell is the best it has.
        Dragonfly => "exec sh",
        Clickhouse => "exec clickhouse-client --user \"$CLICKHOUSE_USER\" --password \"$CLICKHOUSE_PASSWORD\" -d \"$CLICKHOUSE_DB\"",
    })
}

pub fn restore_command(kind: DatabaseKind) -> Option<Vec<String>> {
    use DatabaseKind::*;
    match kind {
        Postgresql => Some(sh(
            "pg_restore -U \"$POSTGRES_USER\" -d \"$POSTGRES_DB\" --clean --if-exists --no-owner",
        )),
        Mysql => Some(sh("MYSQL_PWD=\"$MYSQL_PASSWORD\" mysql -u \"$MYSQL_USER\" \"$MYSQL_DATABASE\"")),
        Mariadb => Some(sh("MYSQL_PWD=\"$MARIADB_PASSWORD\" mariadb -u \"$MARIADB_USER\" \"$MARIADB_DATABASE\"")),
        Mongodb => Some(sh(
            "mongorestore --archive --drop --quiet -u \"$MONGO_INITDB_ROOT_USERNAME\" -p \"$MONGO_INITDB_ROOT_PASSWORD\" --authenticationDatabase admin",
        )),
        _ => None,
    }
}

fn encode(s: &str) -> String {
    // Our generated user names and passwords are base62: nothing to escape.
    s.to_string()
}

/// URL for apps (host = the database's name on the project network).
pub fn url(kind: DatabaseKind, host: &str, port: u16, cred: &Credentials) -> String {
    use DatabaseKind::*;
    let (u, p, d) = (encode(&cred.user), encode(&cred.password), &cred.database);
    match kind {
        Postgresql => format!("postgres://{u}:{p}@{host}:{port}/{d}"),
        Mysql | Mariadb => format!("mysql://{u}:{p}@{host}:{port}/{d}"),
        Mongodb => format!("mongodb://{u}:{p}@{host}:{port}/{d}?authSource=admin"),
        Redis | Keydb | Dragonfly => format!("redis://default:{p}@{host}:{port}"),
        Clickhouse => format!("clickhouse://{u}:{p}@{host}:{port}/{d}"),
    }
}

/// The URL with the password replaced (for listings).
pub fn masked_url(kind: DatabaseKind, host: &str, port: u16, cred: &Credentials) -> String {
    url(
        kind,
        host,
        port,
        &Credentials {
            password: "••••••".into(),
            ..cred.clone()
        },
    )
}

pub fn container_name(name: &str) -> String {
    format!("vfy-db-{name}")
}

pub fn volume_name(name: &str) -> String {
    format!("vfy-db-{name}-data")
}

fn row(r: &Row, project_id: String) -> rusqlite::Result<Database> {
    let kind = DatabaseKind::parse(&r.get::<_, String>("kind")?).unwrap_or(DatabaseKind::Postgresql);
    Ok(Database {
        id: r.get("id")?,
        name: r.get("name")?,
        project_id,
        environment_id: r.get("environment_id")?,
        kind,
        version: r.get("version")?,
        image: r.get("image")?,
        node_id: r.get("node_id")?,
        internal_url: None,
        public_port: r.get("public_port")?,
        public_bind: r.get("public_bind")?,
        desired: if r.get::<_, String>("desired")? == "stopped" {
            Desired::Stopped
        } else {
            Desired::Running
        },
        container: r.get("container")?,
        backup_schedule: r.get("backup_schedule")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

const SELECT: &str =
    "SELECT d.*, e.project_id AS project_id FROM databases d JOIN environments e ON d.environment_id = e.id";

/// With `reveal`, `internal_url` carries the password.
pub fn list(c: &Connection, key: &MasterKey, project_id: Option<&str>, reveal: bool) -> Result<Vec<Database>> {
    let mut st = c.prepare(&format!(
        "{SELECT} WHERE (?1 IS NULL OR e.project_id = ?1) ORDER BY d.name"
    ))?;
    let mut rows = st.query([project_id])?;
    let mut out = vec![];
    while let Some(r) = rows.next()? {
        let p: String = r.get("project_id")?;
        out.push(row(r, p)?);
    }
    for d in &mut out {
        fill_url(c, key, d, reveal)?;
    }
    Ok(out)
}

fn fill_url(c: &Connection, key: &MasterKey, d: &mut Database, reveal: bool) -> Result<()> {
    let cred = credentials(c, key, &d.id)?;
    let port = template(d.kind).port;
    d.internal_url = Some(if reveal {
        url(d.kind, &d.name, port, &cred)
    } else {
        masked_url(d.kind, &d.name, port, &cred)
    });
    Ok(())
}

pub fn get(c: &Connection, key: &MasterKey, id_or_name: &str, reveal: bool) -> Result<Database> {
    let mut st = c.prepare(&format!("{SELECT} WHERE d.id = ?1 OR d.name = ?1"))?;
    let mut rows = st.query([id_or_name])?;
    let Some(r) = rows.next()? else {
        return Err(Error::not_found(format!("database {id_or_name}")));
    };
    let p: String = r.get("project_id")?;
    let mut d = row(r, p)?;
    drop(rows);
    fill_url(c, key, &mut d, reveal)?;
    Ok(d)
}

fn check_public_port(c: &Connection, port: u16, except: Option<&str>) -> Result<()> {
    if !(1024..=65535).contains(&port) || [7420, 7421, 7443, 8082].contains(&port) {
        return Err(Error::Invalid(format!(
            "port {port} can't be used (1024–65535, not one of Vercelify's own)"
        )));
    }
    let taken: Option<String> = c
        .query_row(
            "SELECT name FROM databases WHERE public_port = ?1 AND id IS NOT ?2",
            params![port, except],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(n) = taken {
        return Err(Error::Conflict(format!("port {port} is already used by {n}")));
    }
    Ok(())
}

pub fn create(c: &Connection, key: &MasterKey, req: &DatabaseCreate) -> Result<Database> {
    let name = req.name.trim();
    let cred = Credentials {
        user: format!("u{}", random_token(11).to_ascii_lowercase()),
        password: random_token(32),
        database: name.replace('-', "_"),
    };
    insert(c, key, req, cred, None, "mesh")
}

/// A database whose data exists already (taken over from Coolify): its
/// credentials, its exact image (the data directory's version), and its
/// public port open on every interface as it was.
pub fn adopt(
    c: &Connection,
    key: &MasterKey,
    req: &DatabaseCreate,
    cred: Credentials,
    image: &str,
) -> Result<Database> {
    crate::validate::image(image)?;
    insert(c, key, req, cred, Some(image), "all")
}

fn insert(
    c: &Connection,
    key: &MasterKey,
    req: &DatabaseCreate,
    cred: Credentials,
    image_ref: Option<&str>,
    bind: &str,
) -> Result<Database> {
    let env = projects::resolve_target(c, req.environment_id.as_deref(), req.project_id.as_deref())?;
    let t = template(req.kind);
    let version = req.version.clone().unwrap_or_else(|| t.default_version.to_string());
    if !valid_version(&version) {
        return Err(Error::Invalid(format!("`{version}` is not a version")));
    }
    if let Some(p) = req.public_port {
        check_public_port(c, p, None)?;
    }
    let id = new_id("db");
    let name = req.name.trim();
    claim_name(c, name, "database", &id)?;
    let sealed = key.seal_str(&credentials_aad(&id), &serde_json::to_string(&cred)?);
    let ts = now();
    c.execute(
        "INSERT INTO databases (id, environment_id, name, kind, version, image, credentials, public_port, public_bind,
                                container, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
        params![
            id,
            env.id,
            name,
            req.kind.as_str(),
            version,
            image_ref
                .map(str::to_string)
                .unwrap_or_else(|| image(req.kind, &version)),
            sealed,
            req.public_port,
            if req.public_port.is_some() { bind } else { "mesh" },
            container_name(name),
            ts
        ],
    )?;
    let tier = match req.tier.unwrap_or(StorageTier::Ssd) {
        StorageTier::Hdd => "hdd",
        StorageTier::Ssd => "ssd",
    };
    c.execute(
        "INSERT INTO volumes (id, owner_kind, owner_id, name, mount_path, tier, created_at)
         VALUES (?1, 'database', ?2, 'data', ?3, ?4, ?5)",
        params![new_id("vol"), id, t.data_path, tier, ts],
    )?;
    if let Some(app) = req.link_app.as_deref().filter(|a| !a.is_empty()) {
        link(c, key, app, name, t.url_var)?;
    }
    get(c, key, &id, false)
}

/// Point an app's variable at a database: `${{db.<name>.url}}`, resolved at
/// every deploy (a new password needs only a redeploy).
pub fn link(c: &Connection, key: &MasterKey, app: &str, db_name: &str, var: &str) -> Result<()> {
    let a = crate::apps::find(c, app)?;
    crate::env::put(
        c,
        key,
        vercelify_core::platform::VarScope::App,
        &a.id,
        &vercelify_core::platform::EnvVarsPut {
            vars: vec![vercelify_core::platform::EnvVarInput {
                key: var.into(),
                value: format!("${{{{db.{db_name}.url}}}}"),
                build: None,
                preview_only: None,
                secret: Some(true),
            }],
            replace: false,
        },
    )?;
    Ok(())
}

pub fn patch(c: &Connection, key: &MasterKey, id: &str, p: &DatabasePatch) -> Result<Database> {
    let d = get(c, key, id, false)?;
    let t = now();
    if let Some(desired) = p.desired {
        let v = if desired == Desired::Stopped {
            "stopped"
        } else {
            "running"
        };
        c.execute(
            "UPDATE databases SET desired = ?2, updated_at = ?3 WHERE id = ?1",
            params![d.id, v, t],
        )?;
    }
    if let Some(port) = p.public_port {
        if port == 0 {
            c.execute(
                "UPDATE databases SET public_port = NULL, updated_at = ?2 WHERE id = ?1",
                params![d.id, t],
            )?;
        } else {
            check_public_port(c, port, Some(&d.id))?;
            c.execute(
                "UPDATE databases SET public_port = ?2, updated_at = ?3 WHERE id = ?1",
                params![d.id, port, t],
            )?;
        }
    }
    if let Some(bind) = &p.public_bind {
        if bind != "mesh" && bind != "all" {
            return Err(Error::Invalid("public_bind is mesh or all".into()));
        }
        c.execute(
            "UPDATE databases SET public_bind = ?2, updated_at = ?3 WHERE id = ?1",
            params![d.id, bind, t],
        )?;
    }
    if let Some(s) = &p.backup_schedule {
        c.execute(
            "UPDATE databases SET backup_schedule = ?2, updated_at = ?3 WHERE id = ?1",
            params![d.id, s.trim(), t],
        )?;
    }
    get(c, key, &d.id, false)
}

/// Refused while an app's variable references it.
pub fn delete(c: &Connection, key: &MasterKey, id: &str) -> Result<Database> {
    let d = get(c, key, id, false)?;
    let needle = format!("${{{{db.{}.url}}}}", d.name);
    let mut st = c.prepare("SELECT id, value, scope_id FROM env_vars WHERE scope = 'app'")?;
    let rows: Vec<(String, Vec<u8>, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (vid, sealed, app_id) in rows {
        if key
            .open_string(&aad("env_vars", "value", &vid), &sealed)
            .is_ok_and(|v| v.contains(&needle))
        {
            let app = crate::apps::get(c, &app_id).map(|a| a.name).unwrap_or(app_id);
            return Err(Error::Conflict(format!(
                "{app} uses {} in a variable; remove it from the app first",
                d.name
            )));
        }
    }
    c.execute(
        "DELETE FROM volumes WHERE owner_kind = 'database' AND owner_id = ?1",
        [&d.id],
    )?;
    c.execute("DELETE FROM databases WHERE id = ?1", [&d.id])?;
    c.execute("DELETE FROM resource_names WHERE owner_id = ?1", [&d.id])?;
    Ok(d)
}

/// Replace `${{db.<name>.url}}` (and `.host`, `.port`, `.user`, `.password`,
/// `.database`) in variable values. Unknown names are an error: deploying
/// an app that points at a deleted database should fail loudly.
pub fn resolve_references(c: &Connection, key: &MasterKey, value: &str) -> Result<String> {
    resolve_references_at(c, key, value, None)
}

/// [`resolve_references`] for an app on another server than its databases
/// (they run on the main server): host and port are `main_ip` (its mesh
/// address) and the database's public port, which it then needs.
pub fn resolve_references_at(c: &Connection, key: &MasterKey, value: &str, main_ip: Option<&str>) -> Result<String> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(i) = rest.find("${{db.") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 6..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[i..]);
            return Ok(out);
        };
        let reference = &after[..end];
        let (name, field) = reference
            .rsplit_once('.')
            .ok_or_else(|| Error::Invalid(format!("`${{{{db.{reference}}}}}` names no field (.url)")))?;
        let d = get(c, key, name, false)
            .map_err(|_| Error::Invalid(format!("a variable refers to the database {name}, which doesn't exist")))?;
        let cred = credentials(c, key, &d.id)?;
        let (host, port) = match main_ip {
            None => (d.name.clone(), template(d.kind).port),
            Some(ip) => {
                let p = d.public_port.ok_or_else(|| {
                    Error::Invalid(format!(
                        "the app runs on another server than the database {name}: give {name} a public port \
                         (it listens on the mesh only), or run the app on the main server"
                    ))
                })?;
                (ip.to_string(), p)
            }
        };
        let v = match field {
            "url" => url(d.kind, &host, port, &cred),
            "host" => host.clone(),
            "port" => port.to_string(),
            "user" => cred.user.clone(),
            "password" => cred.password.clone(),
            "database" => cred.database.clone(),
            other => {
                return Err(Error::Invalid(format!(
                    "`{other}` is not a database field (url, host, port, user, password, database)"
                )))
            }
        };
        out.push_str(&v);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The container of a database.
pub fn spec(d: &Database, cred: &Credentials, mesh_ip: Option<&str>, hdd_path: Option<String>) -> ContainerSpec {
    let t = template(d.kind);
    let mut labels = BTreeMap::new();
    labels.insert(MANAGED_LABEL.to_string(), MANAGED.to_string());
    labels.insert("vercelify.database".to_string(), d.id.clone());
    labels.insert("vercelify.kind".to_string(), "db".to_string());
    labels.insert("vercelify.project".to_string(), d.project_id.clone());
    let publish = match d.public_port {
        Some(p) if d.public_bind == "all" => vec![format!("{p}:{}", t.port)],
        Some(p) => vec![format!("{}:{p}:{}", mesh_ip.unwrap_or("127.0.0.1"), t.port)],
        None => vec![],
    };
    ContainerSpec {
        name: d.container.clone(),
        image: d.image.clone(),
        labels,
        env: env(d.kind, cred),
        networks: vec![crate::deploy::project_network(&d.project_id)],
        aliases: vec![d.name.clone()],
        command: command(d.kind),
        mounts: vec![Mount {
            source: hdd_path.unwrap_or_else(|| volume_name(&d.name)),
            target: t.data_path.into(),
            read_only: false,
        }],
        publish,
        restart: "unless-stopped".into(),
        ..ContainerSpec::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{apps, env as vars, Store};

    fn key() -> MasterKey {
        MasterKey::from_bytes(&[4u8; 32]).unwrap()
    }

    fn req(name: &str, kind: DatabaseKind) -> DatabaseCreate {
        DatabaseCreate {
            name: name.into(),
            kind,
            version: None,
            environment_id: None,
            project_id: None,
            public_port: None,
            tier: None,
            link_app: None,
        }
    }

    #[test]
    fn templates_cover_every_kind() {
        for k in DatabaseKind::ALL {
            let t = template(k);
            assert!(valid_version(t.default_version), "{k:?}");
            assert!(image(k, t.default_version).contains(':'));
            if t.backup == BackupStyle::Dump {
                assert!(dump_command(k).is_some() && restore_command(k).is_some(), "{k:?}");
            }
            // Passwords only through the environment.
            let cmds = [command(k), health_command(k), dump_command(k), restore_command(k)];
            for c in cmds.into_iter().flatten() {
                assert!(!c.join(" ").contains("secret-password"), "{k:?}");
            }
        }
        assert!(!valid_version("latest; rm"));
        assert!(!valid_version("-1"));
    }

    #[test]
    fn create_link_resolve_delete() {
        let s = Store::open_in_memory().unwrap();
        let k = key();
        let app = s.tx(|c| apps::create(c, &apps::tests::git_app("blog"))).unwrap();
        let mut r = req("blog-db", DatabaseKind::Postgresql);
        r.link_app = Some("blog".into());
        let d = s.tx(|c| create(c, &k, &r)).unwrap();
        assert_eq!(d.image, "postgres:17-alpine");
        assert_eq!(d.container, "vfy-db-blog-db");
        assert!(d.internal_url.as_deref().unwrap().contains("••••••"));
        let revealed = s.with(|c| get(c, &k, "blog-db", true)).unwrap();
        let url = revealed.internal_url.unwrap();
        assert!(url.starts_with("postgres://u") && url.ends_with("@blog-db:5432/blog_db"));
        let raw: Vec<u8> = s
            .with(|c| Ok(c.query_row("SELECT credentials FROM databases", [], |r| r.get(0))?))
            .unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("postgres"), "sealed");
        // The app got DATABASE_URL as a reference, resolved at deploy.
        let resolved = s.with(|c| vars::resolve(c, &k, &app.id, false)).unwrap();
        let value = resolved["DATABASE_URL"].value.to_string();
        assert_eq!(value, "${{db.blog-db.url}}");
        assert_eq!(s.with(|c| resolve_references(c, &k, &value)).unwrap(), url);
        assert_eq!(
            s.with(|c| resolve_references(c, &k, "h=${{db.blog-db.host}}:${{db.blog-db.port}}"))
                .unwrap(),
            "h=blog-db:5432"
        );
        assert!(s.with(|c| resolve_references(c, &k, "${{db.gone.url}}")).is_err());
        let far = s.with(|c| resolve_references_at(c, &k, "${{db.blog-db.host}}", Some("10.77.0.1")));
        assert!(far.unwrap_err().to_string().contains("public port"));
        s.tx(|c| {
            c.execute("UPDATE databases SET public_port = 15432", [])?;
            Ok(())
        })
        .unwrap();
        assert_eq!(
            s.with(|c| resolve_references_at(c, &k, "${{db.blog-db.host}}:${{db.blog-db.port}}", Some("10.77.0.1")))
                .unwrap(),
            "10.77.0.1:15432"
        );
        assert!(s.with(|c| resolve_references(c, &k, "${{db.blog-db.nope}}")).is_err());
        assert_eq!(
            s.with(|c| resolve_references(c, &k, "plain $HOME")).unwrap(),
            "plain $HOME"
        );
        // Names are shared with apps.
        assert!(matches!(
            s.tx(|c| create(c, &k, &req("blog", DatabaseKind::Redis))),
            Err(Error::Conflict(_))
        ));
        // In use: refused.
        assert!(matches!(s.tx(|c| delete(c, &k, "blog-db")), Err(Error::Conflict(m)) if m.contains("blog")));
        s.tx(|c| {
            vars::delete(
                c,
                vercelify_core::platform::VarScope::App,
                &app.id,
                "DATABASE_URL",
                false,
            )
        })
        .unwrap();
        s.tx(|c| delete(c, &k, "blog-db")).unwrap();
        assert!(s.with(|c| list(c, &k, None, false)).unwrap().is_empty());
    }

    #[test]
    fn public_ports() {
        let s = Store::open_in_memory().unwrap();
        let k = key();
        let mut r = req("cache", DatabaseKind::Redis);
        r.public_port = Some(16379);
        let d = s.tx(|c| create(c, &k, &r)).unwrap();
        let mut r2 = req("cache2", DatabaseKind::Redis);
        r2.public_port = Some(16379);
        assert!(matches!(s.tx(|c| create(c, &k, &r2)), Err(Error::Conflict(_))));
        r2.public_port = Some(7420);
        assert!(matches!(s.tx(|c| create(c, &k, &r2)), Err(Error::Invalid(_))));
        let cred = s.with(|c| credentials(c, &k, &d.id)).unwrap();
        let sp = spec(&d, &cred, Some("10.77.0.1"), None);
        assert_eq!(sp.publish, ["10.77.0.1:16379:6379"], "the mesh only, by default");
        assert_eq!(sp.aliases, ["cache"]);
        assert!(sp.env.iter().any(|(k, _)| k == "VFY_PASSWORD"));
        assert!(
            !sp.command.unwrap().join(" ").contains(&cred.password),
            "password via the environment"
        );
        let d = s
            .tx(|c| {
                patch(
                    c,
                    &k,
                    &d.id,
                    &DatabasePatch {
                        public_bind: Some("all".into()),
                        ..DatabasePatch::default()
                    },
                )
            })
            .unwrap();
        assert_eq!(spec(&d, &cred, None, None).publish, ["16379:6379"]);
        let d = s
            .tx(|c| {
                patch(
                    c,
                    &k,
                    &d.id,
                    &DatabasePatch {
                        public_port: Some(0),
                        ..DatabasePatch::default()
                    },
                )
            })
            .unwrap();
        assert!(spec(&d, &cred, None, None).publish.is_empty());
    }
}

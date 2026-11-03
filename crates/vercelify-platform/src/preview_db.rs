//! Durable, opt-in PostgreSQL preview ownership. No production credentials or
//! database contents enter this journal. Closed resources keep identity until
//! their external database/role cleanup has completed.
use crate::{
    databases,
    ids::new_id,
    now, previews,
    secrets::{aad, random_token, MasterKey},
    Error, Result,
};
use rusqlite::{params, Connection, OptionalExtension, Row};
use vercelify_core::platform::{App, DatabaseCreate, DatabaseKind};
use zeroize::Zeroizing;

pub const CONNECTION_LIMIT: u8 = 4;

pub fn settings(c: &Connection, app: &str) -> Result<Option<vercelify_core::platform::PreviewDatabaseSettings>> {
    let raw: String = c.query_row("SELECT previews FROM apps WHERE id=?1", [app], |r| r.get(0))?;
    let settings: vercelify_core::platform::PreviewSettings = serde_json::from_str(&raw)?;
    Ok(if settings.enabled { settings.database } else { None })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Resource {
    pub id: String,
    pub app_id: String,
    pub app_name: String,
    pub preview_id: String,
    pub pr_number: u32,
    pub service_id: String,
    pub database: String,
    pub username: String,
    pub state: String,
    pub deployment_id: String,
    pub error: String,
}
pub struct Binding {
    pub resource: Resource,
    pub password: Zeroizing<String>,
}
impl std::ops::Deref for Binding {
    type Target = Resource;
    fn deref(&self) -> &Resource {
        &self.resource
    }
}
impl std::fmt::Debug for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binding")
            .field("resource", &self.resource)
            .field("password", &"[redacted]")
            .finish()
    }
}
fn resource(r: &Row<'_>) -> rusqlite::Result<Resource> {
    Ok(Resource {
        id: r.get("id")?,
        app_id: r.get("app_id")?,
        app_name: r.get("app_name")?,
        preview_id: r.get("preview_id")?,
        pr_number: r.get("pr_number")?,
        service_id: r.get("service_id")?,
        database: r.get("database_name")?,
        username: r.get("username")?,
        state: r.get("state")?,
        deployment_id: r.get("deployment_id")?,
        error: r.get("error")?,
    })
}
pub fn get(c: &Connection, id: &str) -> Result<Resource> {
    c.query_row("SELECT * FROM preview_databases WHERE id=?1", [id], resource)
        .optional()?
        .ok_or_else(|| Error::not_found("preview database"))
}
pub fn binding(c: &Connection, key: &MasterKey, id: &str) -> Result<Binding> {
    let resource = get(c, id)?;
    if resource.state == "deleted" {
        return Err(Error::not_found("preview database"));
    }
    let sealed: Vec<u8> = c.query_row("SELECT password FROM preview_databases WHERE id=?1", [id], |r| r.get(0))?;
    let password = key.open_string(&aad("preview_databases", "password", id), &sealed)?;
    Ok(Binding { resource, password })
}
pub fn live(c: &Connection, app: &str, pr: u32) -> Result<Option<Resource>> {
    Ok(c.query_row(
        "SELECT * FROM preview_databases WHERE app_id=?1 AND pr_number=?2 AND state!='deleted'",
        params![app, pr],
        resource,
    )
    .optional()?)
}
pub fn list(c: &Connection, app: &str) -> Result<Vec<Resource>> {
    let mut statement =
        c.prepare("SELECT * FROM preview_databases WHERE app_id=?1 AND state!='deleted' ORDER BY pr_number")?;
    let rows = statement
        .query_map([app], resource)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
pub fn service_id(c: &Connection, app: &str) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT database_id FROM preview_db_services WHERE app_id=?1",
        [app],
        |r| r.get(0),
    )
    .optional()?)
}
pub fn service_network(c: &Connection, database: &str) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT network FROM preview_db_services WHERE database_id=?1",
        [database],
        |r| r.get(0),
    )
    .optional()?)
}
pub fn prepare(c: &Connection, key: &MasterKey, app: &App, pr: u32) -> Result<Binding> {
    if !app.previews.enabled || app.previews.database.is_none() {
        return Err(Error::Invalid("fresh PostgreSQL previews are not enabled".into()));
    }
    let preview = previews::get(c, &app.id, pr)?
        .filter(|p| p.status == "active")
        .ok_or_else(|| Error::Conflict("the pull request is closed".into()))?;
    if let Some(existing) = live(c, &app.id, pr)? {
        if existing.state == "closing" {
            return Err(Error::Capacity("waiting for previous preview database cleanup".into()));
        }
        return binding(c, key, &existing.id);
    }
    let allocated: u32 = c.query_row(
        "SELECT count(*) FROM preview_databases WHERE app_id=?1 AND state!='deleted'",
        [&app.id],
        |r| r.get(0),
    )?;
    if allocated >= u32::from(app.previews.max.min(3)) {
        return Err(Error::Capacity(
            "preview database limit reached; close a PR and finish its cleanup".into(),
        ));
    }
    let service = match service_id(c, &app.id)? {
        Some(id) => {
            let state: String = c.query_row(
                "SELECT state FROM preview_db_services WHERE database_id=?1",
                [&id],
                |r| r.get(0),
            )?;
            if state == "closing" {
                return Err(Error::Capacity("waiting for preview PostgreSQL service cleanup".into()));
            }
            databases::get(c, key, &id, false)?;
            id
        }
        None => {
            let service = databases::create(
                c,
                key,
                &DatabaseCreate {
                    name: format!("preview-{}", app.id.trim_start_matches("app_")),
                    kind: DatabaseKind::Postgresql,
                    version: Some("17".into()),
                    environment_id: Some(app.environment_id.clone()),
                    project_id: None,
                    public_port: None,
                    tier: None,
                    link_app: None,
                },
            )?;
            c.execute(
                "INSERT INTO preview_db_services(app_id,database_id,network,created_at) VALUES(?1,?2,?3,?4)",
                params![app.id, service.id, format!("vfy-preview-db-{}", app.id), now()],
            )?;
            service.id
        }
    };
    let id = new_id("task");
    let suffix = id.trim_start_matches("task_");
    let password = Zeroizing::new(random_token(40));
    let sealed = key.seal_str(&aad("preview_databases", "password", &id), &password);
    c.execute("INSERT INTO preview_databases(id,app_id,app_name,preview_id,pr_number,service_id,database_name,username,password,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
        params![id,app.id,app.name,preview.id,pr,service,format!("pdb_{suffix}"),format!("pr_{suffix}"),sealed,now()])?;
    binding(c, key, &id)
}
pub fn mark_closing(c: &Connection, app: &str, pr: u32) -> Result<()> {
    c.execute("UPDATE preview_databases SET state='closing',updated_at=?3 WHERE app_id=?1 AND pr_number=?2 AND state!='deleted'",params![app,pr,now()])?;
    Ok(())
}
pub fn require_active(c: &Connection, id: &str) -> Result<Resource> {
    let owned = get(c, id)?;
    let active: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM apps a JOIN previews p ON p.app_id=a.id WHERE a.id=?1 AND p.id=?2 AND p.status='active' AND json_extract(a.previews,'$.enabled')=1 AND json_extract(a.previews,'$.database') IS NOT NULL)",params![owned.app_id,owned.preview_id],|r|r.get(0))?;
    if !active || matches!(owned.state.as_str(), "closing" | "deleted") {
        return Err(Error::Conflict("the preview database lifecycle is closed".into()));
    }
    Ok(owned)
}
pub fn set_state(c: &Connection, id: &str, state: &str, error: &str) -> Result<()> {
    if !matches!(state, "pending" | "provisioned" | "ready" | "closing" | "deleted") {
        return Err(Error::Invalid("invalid preview database state".into()));
    }
    let old = get(c, id)?;
    if old.state == "deleted" && state != "deleted" {
        return Err(Error::Conflict("the preview database lifecycle is deleted".into()));
    }
    if state == "deleted" {
        if !matches!(old.state.as_str(), "closing" | "deleted") {
            return Err(Error::Conflict("preview database cleanup was not requested".into()));
        }
    } else if state != "closing" {
        require_active(c, id)?;
    }
    c.execute("UPDATE preview_databases SET state=?2,error=?3,updated_at=?4,password=CASE WHEN ?2='deleted' THEN X'' ELSE password END WHERE id=?1",params![id,state,error.chars().take(1024).collect::<String>(),now()])?;
    Ok(())
}
pub fn pending_cleanup(c: &Connection) -> Result<Vec<Resource>> {
    c.execute("UPDATE preview_databases SET state='closing',updated_at=?1 WHERE state NOT IN ('closing','deleted') AND NOT EXISTS(SELECT 1 FROM apps a JOIN previews p ON p.app_id=a.id WHERE a.id=preview_databases.app_id AND p.id=preview_databases.preview_id AND p.status='active' AND json_extract(a.previews,'$.enabled')=1 AND json_extract(a.previews,'$.database') IS NOT NULL)",[now()])?;
    let mut s = c.prepare("SELECT * FROM preview_databases WHERE state='closing' ORDER BY created_at,id")?;
    let rows = s.query_map([], resource)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn owner_role(owned: &Resource) -> String {
    format!("owner_{}", owned.id.trim_start_matches("task_"))
}

#[derive(Debug, Clone)]
pub struct Service {
    pub app_id: String,
    pub database_id: String,
    pub network: String,
}
pub fn services(c: &Connection) -> Result<Vec<Service>> {
    let mut query = c.prepare("SELECT app_id,database_id,network FROM preview_db_services ORDER BY app_id")?;
    let rows = query
        .query_map([], |r| {
            Ok(Service {
                app_id: r.get(0)?,
                database_id: r.get(1)?,
                network: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
pub fn resources(c: &Connection, service: &str) -> Result<Vec<Resource>> {
    let mut query =
        c.prepare("SELECT * FROM preview_databases WHERE service_id=?1 AND state!='deleted' ORDER BY id")?;
    let rows = query
        .query_map([service], resource)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn close_service_if_unused(c: &Connection, service: &str) -> Result<bool> {
    c.execute("UPDATE preview_db_services SET state='closing' WHERE database_id=?1 AND NOT EXISTS(SELECT 1 FROM preview_databases WHERE service_id=?1 AND state IN ('pending','provisioned','ready'))",[service])?;
    Ok(c.query_row(
        "SELECT state='closing' FROM preview_db_services WHERE database_id=?1",
        [service],
        |r| r.get(0),
    )?)
}

pub fn finish_service(c: &Connection, service: &str) -> Result<()> {
    let closing: bool = c.query_row(
        "SELECT state='closing' FROM preview_db_services WHERE database_id=?1",
        [service],
        |r| r.get(0),
    )?;
    if !closing || resources(c, service)?.iter().any(|r| r.state != "closing") {
        return Err(Error::Conflict(
            "preview PostgreSQL service still has live dependents".into(),
        ));
    }
    c.execute(
        "UPDATE preview_databases SET state='deleted',password=X'',error='',updated_at=?2 WHERE service_id=?1",
        params![service, now()],
    )?;
    c.execute("DELETE FROM preview_db_services WHERE database_id=?1", [service])?;
    c.execute("DELETE FROM databases WHERE id=?1", [service])?;
    c.execute("DELETE FROM resource_names WHERE owner_id=?1", [service])?;
    c.execute(
        "DELETE FROM volumes WHERE owner_kind='database' AND owner_id=?1",
        [service],
    )?;
    Ok(())
}

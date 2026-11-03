//! Versioned restore evidence. A successful upload, repository check and
//! isolated application restore are distinct claims.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u8,
    pub database_id: String,
    pub database_name: String,
    pub server_major: u16,
    pub filename: String,
    pub format: String,
    pub backup_started_at: String,
    pub bytes: u64,
    pub database_bytes: u64,
    pub extensions: Vec<(String, String)>,
    #[serde(default)]
    pub objects: Vec<(String, String, String)>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assertion {
    pub name: String,
    pub query: String,
    pub expected: String,
}
impl Manifest {
    pub fn validate(&self, database: &str) -> Result<()> {
        let token = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        };
        if self.version != 1
            || self.database_id != database
            || !self.database_id.starts_with("db_")
            || !token(&self.database_id)
            || !token(&self.database_name)
            || self.server_major != 17
            || self.filename != format!("{}.dump", self.database_name)
            || self.format != "postgresql-custom"
            || self.bytes == 0
            || self.bytes > 16 * 1024 * 1024 * 1024
            || self.database_bytes == 0
            || self.database_bytes > 1024 * 1024 * 1024 * 1024
            || chrono::DateTime::parse_from_rfc3339(&self.backup_started_at).is_err()
        {
            return Err(Error::Invalid("restore drill requires an exact version-1 PostgreSQL 17 custom-dump manifest with bounded size and a backup timestamp".into()));
        }
        if self.objects.len() > 5000
            || self.objects.iter().any(|(schema, name, kind)| {
                schema.is_empty()
                    || name.is_empty()
                    || schema.len() > 63
                    || name.len() > 63
                    || schema.contains('\0')
                    || name.contains('\0')
                    || !matches!(kind.as_str(), "r" | "p" | "i" | "I" | "v" | "m" | "S" | "f")
            })
        {
            return Err(Error::Invalid("unsupported object manifest".into()));
        }
        let mut names = std::collections::HashSet::new();
        if self.extensions.len() > 64
            || self.extensions.iter().any(|(name, version)| {
                !token(name)
                    || version.is_empty()
                    || version.len() > 64
                    || !version
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
                    || !names.insert(name)
            })
        {
            return Err(Error::Invalid("unsupported or duplicate extension metadata".into()));
        }
        Ok(())
    }
}
pub fn validate_assertions(assertions: &[Assertion]) -> Result<()> {
    if assertions.len() > 20
        || assertions.iter().any(|a| {
            a.name.trim().is_empty()
                || a.name.len() > 80
                || a.name.chars().any(char::is_control)
                || a.query.is_empty()
                || a.query.len() > 8192
                || a.query.contains(['\0', ';', '\\'])
                || a.expected.len() > 65536
                || a.expected.contains('\0')
                || !a
                    .query
                    .split_whitespace()
                    .next()
                    .is_some_and(|word| word.eq_ignore_ascii_case("SELECT") || word.eq_ignore_ascii_case("WITH"))
        })
    {
        return Err(Error::Invalid("use at most 20 named, bounded SELECT/WITH assertions; probes additionally run with read-only database privileges".into()));
    }
    Ok(())
}

use crate::{
    databases, dbengine,
    ids::new_id,
    now,
    secrets::{aad, MasterKey},
};
use rusqlite::{params, Connection, OptionalExtension};
use zeroize::Zeroizing;

#[derive(Debug, Clone, Serialize)]
pub struct Drill {
    pub id: String,
    pub database_id: Option<String>,
    pub source_id: String,
    pub backup_id: String,
    pub snapshot: String,
    pub source_name: String,
    pub state: String,
    pub error: String,
    pub integrity: String,
    pub assertion_names: Vec<String>,
    pub credential_source: String,
    pub verified_at: Option<String>,
    pub cleanup_pending: bool,
    pub cancel_requested: bool,
    pub bytes: u64,
    pub restore_seconds: Option<u64>,
    pub rpo_seconds: Option<u64>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}
pub fn attach_manifest(c: &Connection, backup: &str, manifest: &Manifest) -> Result<()> {
    let row = dbengine::backup(c, backup)?;
    manifest.validate(&row.database_id)?;
    if row.status != "succeeded" || row.bytes != manifest.bytes || row.snapshot.len() != 64 {
        return Err(Error::Invalid(
            "manifest does not match the successful full-snapshot backup".into(),
        ));
    }
    c.execute(
        "UPDATE database_backups SET manifest=?2 WHERE id=?1",
        params![backup, serde_json::to_string(manifest)?],
    )?;
    Ok(())
}
pub fn manifest(c: &Connection, backup: &str) -> Result<Manifest> {
    let row = dbengine::backup(c, backup)?;
    let raw: Option<String> = c.query_row("SELECT manifest FROM database_backups WHERE id=?1", [backup], |r| {
        r.get(0)
    })?;
    let m: Manifest = serde_json::from_str(&raw.ok_or_else(|| {
        Error::Invalid("this legacy backup has no drill manifest; take a new supported backup".into())
    })?)?;
    m.validate(&row.database_id)?;
    if row.status != "succeeded"
        || row.bytes != m.bytes
        || row.snapshot.len() != 64
        || !row.snapshot.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::Invalid(
            "backup identity, size or status differs from its manifest".into(),
        ));
    }
    Ok(m)
}
pub fn get(c: &Connection, id: &str) -> Result<Drill> {
    let row=c.query_row("SELECT id,database_id,source_id,backup_id,snapshot,source_name,state,error,integrity,assertion_names,credential_source,verified_at,cleanup_pending,bytes,restore_seconds,rpo_seconds,created_at,started_at,finished_at,cancel_requested FROM restore_drills WHERE id=?1",[id],|r|Ok(Drill {
        id:r.get(0)?,database_id:r.get(1)?,source_id:r.get(2)?,backup_id:r.get(3)?,snapshot:r.get(4)?,source_name:r.get(5)?,state:r.get(6)?,error:r.get(7)?,integrity:r.get(8)?,assertion_names:vec![],credential_source:r.get(10)?,verified_at:r.get(11)?,cleanup_pending:r.get(12)?,bytes:r.get(13)?,restore_seconds:r.get(14)?,rpo_seconds:r.get(15)?,created_at:r.get(16)?,started_at:r.get(17)?,finished_at:r.get(18)?,cancel_requested:r.get(19)?
    })).optional()?.ok_or_else(||Error::not_found("restore drill"))?;
    let raw: String = c.query_row("SELECT assertion_names FROM restore_drills WHERE id=?1", [id], |r| {
        r.get(0)
    })?;
    Ok(Drill {
        assertion_names: serde_json::from_str(&raw)?,
        ..row
    })
}
pub fn list(c: &Connection, database: Option<&str>, active: bool) -> Result<Vec<Drill>> {
    let mut q=c.prepare("SELECT id FROM restore_drills WHERE (?1 IS NULL OR source_id=?1) AND (?2=0 OR state IN ('queued','deferred','running','verifying','cleanup')) ORDER BY created_at DESC LIMIT CASE WHEN ?2=1 THEN -1 ELSE 100 END")?;
    let ids = q
        .query_map(params![database, active], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.iter().map(|id| get(c, id)).collect()
}
pub fn enqueue(
    c: &Connection,
    key: &MasterKey,
    database: &str,
    backup: &str,
    assertions: &[Assertion],
) -> Result<Drill> {
    check_queue(c)?;
    validate_assertions(assertions)?;
    let source = databases::get(c, key, database, false)?;
    if source.kind != vercelify_core::platform::DatabaseKind::Postgresql {
        return Err(Error::Invalid(
            "restore drills currently support PostgreSQL 17 only".into(),
        ));
    }
    let m = manifest(c, backup)?;
    m.validate(&source.id)?;
    if !list(c, Some(&source.id), true)?.is_empty() {
        return Err(Error::Conflict(
            "a restore drill or its cleanup is already pending".into(),
        ));
    }
    let row = dbengine::backup(c, backup)?;
    let id = new_id("task");
    let raw = Zeroizing::new(serde_json::to_string(assertions)?);
    let sealed = key.seal_str(&aad("restore_drills", "assertions", &id), &raw);
    let names: Vec<_> = assertions.iter().map(|a| a.name.as_str()).collect();
    c.execute("INSERT INTO restore_drills(id,database_id,source_id,backup_id,snapshot,source_name,manifest,assertions,assertion_names,bytes,created_at) VALUES(?1,?2,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![id,source.id,backup,row.snapshot,source.name,serde_json::to_string(&m)?,sealed,serde_json::to_string(&names)?,m.bytes,now()])?;
    get(c, &id)
}
pub fn assertions(c: &Connection, key: &MasterKey, id: &str) -> Result<Vec<Assertion>> {
    let raw: Vec<u8> = c.query_row("SELECT assertions FROM restore_drills WHERE id=?1", [id], |r| r.get(0))?;
    let raw = key.open_string(&aad("restore_drills", "assertions", id), &raw)?;
    Ok(serde_json::from_str(&raw)?)
}
pub fn set_state(c: &Connection, id: &str, state: &str, reason: &str) -> Result<()> {
    let old = get(c, id)?;
    if matches!(old.state.as_str(), "succeeded" | "failed" | "cancelled") {
        return Err(Error::Conflict("restore drill already finished".into()));
    }
    let allowed = matches!(
        (old.state.as_str(), state),
        (
            "queued" | "deferred",
            "deferred" | "running" | "cleanup" | "failed" | "cancelled"
        ) | ("running", "verifying" | "cleanup")
            | ("verifying", "cleanup")
            | ("cleanup", "cleanup" | "deferred" | "succeeded" | "failed" | "cancelled")
    );
    if !allowed {
        return Err(Error::Conflict("invalid restore drill transition".into()));
    }
    let final_state = matches!(state, "succeeded" | "failed" | "cancelled");
    if final_state && old.cleanup_pending {
        return Err(Error::Conflict("restore destination cleanup is still pending".into()));
    }
    if state == "succeeded" && old.verified_at.is_none() {
        return Err(Error::Conflict("restore verification was not recorded".into()));
    }
    c.execute("UPDATE restore_drills SET state=?2,error=?3,started_at=CASE WHEN ?2='running' AND started_at IS NULL THEN ?4 ELSE started_at END,finished_at=CASE WHEN ?5 THEN ?4 ELSE finished_at END,recovery=CASE WHEN ?5 THEN NULL ELSE recovery END,assertions=CASE WHEN ?5 THEN X'' ELSE assertions END,attempts=attempts+CASE WHEN ?2='running' THEN 1 ELSE 0 END WHERE id=?1",params![id,state,crate::logs::strip_ansi(reason).chars().take(1024).collect::<String>(),now(),final_state])?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Service {
    pub drill_id: String,
    pub database_id: String,
    pub network: String,
    pub state: String,
}
pub fn service(c: &Connection, drill: &str) -> Result<Option<Service>> {
    Ok(c.query_row(
        "SELECT drill_id,database_id,network,state FROM restore_drill_services WHERE drill_id=?1",
        [drill],
        |r| {
            Ok(Service {
                drill_id: r.get(0)?,
                database_id: r.get(1)?,
                network: r.get(2)?,
                state: r.get(3)?,
            })
        },
    )
    .optional()?)
}
pub fn service_network(c: &Connection, database: &str) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT network FROM restore_drill_services WHERE database_id=?1",
        [database],
        |r| r.get(0),
    )
    .optional()?)
}
pub fn prepare(c: &Connection, key: &MasterKey, id: &str) -> Result<Service> {
    let job = get(c, id)?;
    if !matches!(job.state.as_str(), "queued" | "deferred" | "running") {
        return Err(Error::Conflict("restore drill is not active".into()));
    }
    if let Some(existing) = service(c, id)? {
        if existing.state != "active" {
            return Err(Error::Conflict("restore drill cleanup is pending".into()));
        }
        return Ok(existing);
    }
    let source = job
        .database_id
        .as_deref()
        .map(|id| databases::get(c, key, id, false))
        .transpose()?;
    let request = vercelify_core::platform::DatabaseCreate {
        name: format!("drill-{}", id.trim_start_matches("task_")),
        kind: vercelify_core::platform::DatabaseKind::Postgresql,
        version: Some("17".into()),
        environment_id: source.map(|d| d.environment_id),
        project_id: None,
        public_port: None,
        tier: Some(vercelify_core::StorageTier::Ssd),
        link_app: None,
    };
    let db = databases::create(c, key, &request)?;
    let network = format!("vfy-preview-drill-{id}");
    c.execute(
        "INSERT INTO restore_drill_services(drill_id,database_id,network) VALUES(?1,?2,?3)",
        params![id, db.id, network],
    )?;
    c.execute("UPDATE restore_drills SET cleanup_pending=1 WHERE id=?1", [id])?;
    service(c, id)?.ok_or_else(|| Error::Internal("restore ownership journal missing".into()))
}
pub fn begin_cleanup(c: &Connection, id: &str, reason: &str) -> Result<()> {
    set_state(c, id, "cleanup", reason)?;
    c.execute(
        "UPDATE restore_drill_services SET state='closing' WHERE drill_id=?1",
        [id],
    )?;
    Ok(())
}
/// Only after exact physical container/volume/network removal succeeds.
pub fn finish_cleanup(c: &Connection, id: &str) -> Result<()> {
    if let Some(s) = service(c, id)? {
        if s.state != "closing" {
            return Err(Error::Conflict("restore service retirement was not requested".into()));
        }
        c.execute("DELETE FROM restore_drill_services WHERE drill_id=?1", [id])?;
        c.execute("DELETE FROM databases WHERE id=?1", [&s.database_id])?;
        c.execute("DELETE FROM resource_names WHERE owner_id=?1", [&s.database_id])?;
        c.execute(
            "DELETE FROM volumes WHERE owner_kind='database' AND owner_id=?1",
            [&s.database_id],
        )?;
    }
    c.execute(
        "UPDATE restore_drills SET cleanup_pending=0 WHERE id=?1 AND state='cleanup'",
        [id],
    )?;
    Ok(())
}

pub fn job_manifest(c: &Connection, id: &str) -> Result<Manifest> {
    let job = get(c, id)?;
    let raw: String = c.query_row("SELECT manifest FROM restore_drills WHERE id=?1", [id], |r| r.get(0))?;
    let m: Manifest = serde_json::from_str(&raw)?;
    m.validate(&job.source_id)?;
    Ok(m)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recovery {
    pub repository: String,
    pub password: String,
    #[serde(default)]
    pub access: std::collections::BTreeMap<String, String>,
}
impl std::fmt::Debug for Recovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Recovery([redacted])")
    }
}
impl Recovery {
    pub fn validate(&self) -> Result<()> {
        const KEYS: &[&str] = &[
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_DEFAULT_REGION",
            "B2_ACCOUNT_ID",
            "B2_ACCOUNT_KEY",
            "AZURE_ACCOUNT_NAME",
            "AZURE_ACCOUNT_KEY",
            "RESTIC_REST_USERNAME",
            "RESTIC_REST_PASSWORD",
        ];
        if self.repository.is_empty()
            || self.repository.len() > 4096
            || self.repository.contains(['\0', '\n', '\r'])
            || self.password.is_empty()
            || self.password.len() > 4096
            || self.password.contains('\0')
            || self.access.len() > KEYS.len()
            || self
                .access
                .iter()
                .any(|(k, v)| !KEYS.contains(&k.as_str()) || v.len() > 8192 || v.contains('\0'))
        {
            return Err(Error::Invalid("invalid recovery repository/key settings; executable credential commands and host-file credential references are not accepted".into()));
        }
        Ok(())
    }
    pub fn restic(&self) -> dbengine::Restic {
        let mut env = self.access.clone();
        env.insert("RESTIC_REPOSITORY".into(), self.repository.clone());
        env.insert("RESTIC_PASSWORD".into(), self.password.clone());
        dbengine::Restic {
            bin: "restic".into(),
            env,
        }
    }
}
pub fn use_recovery(c: &Connection, key: &MasterKey, id: &str, recovery: &Recovery) -> Result<()> {
    recovery.validate()?;
    if get(c, id)?.state != "queued" {
        return Err(Error::Conflict(
            "recovery credentials must be supplied with a new drill".into(),
        ));
    }
    let raw = Zeroizing::new(serde_json::to_string(recovery)?);
    let sealed = key.seal_str(&aad("restore_drills", "recovery", id), &raw);
    c.execute(
        "UPDATE restore_drills SET recovery=?2,credential_source='provided' WHERE id=?1",
        params![id, sealed],
    )?;
    Ok(())
}
pub fn recovery(c: &Connection, key: &MasterKey, id: &str) -> Result<Option<Recovery>> {
    let raw: Option<Vec<u8>> = c.query_row("SELECT recovery FROM restore_drills WHERE id=?1", [id], |r| r.get(0))?;
    raw.map(|raw| {
        let raw = key.open_string(&aad("restore_drills", "recovery", id), &raw)?;
        Ok(serde_json::from_str(&raw)?)
    })
    .transpose()
}
pub fn cancel(c: &Connection, id: &str) -> Result<()> {
    let job = get(c, id)?;
    if matches!(job.state.as_str(), "succeeded" | "failed" | "cancelled") {
        return Err(Error::Conflict("restore drill already finished".into()));
    }
    c.execute("UPDATE restore_drills SET cancel_requested=1 WHERE id=?1", [id])?;
    Ok(())
}
pub fn cancelled(c: &Connection, id: &str) -> Result<bool> {
    Ok(
        c.query_row("SELECT cancel_requested FROM restore_drills WHERE id=?1", [id], |r| {
            r.get(0)
        })?,
    )
}
pub fn recorded_verification(c: &Connection, id: &str, seconds: u64) -> Result<()> {
    let job = get(c, id)?;
    if job.state != "verifying" || cancelled(c, id)? {
        return Err(Error::Conflict("restore verification is no longer active".into()));
    }
    let m = job_manifest(c, id)?;
    let age = (chrono::Utc::now()
        - chrono::DateTime::parse_from_rfc3339(&m.backup_started_at)
            .map_err(|_| Error::Invalid("backup timestamp is invalid".into()))?
            .with_timezone(&chrono::Utc))
    .num_seconds();
    c.execute(
        "UPDATE restore_drills SET verified_at=?2,restore_seconds=?3,rpo_seconds=?4 WHERE id=?1",
        params![id, now(), seconds, if age >= 0 { Some(age) } else { None }],
    )?;
    Ok(())
}

/// Explicit owner-supplied recovery artifacts also work on a clean control store.
/// Their expected values are operator assertions, not a claim of an independent
/// external audit. The original host master key is not needed for the dump.
pub fn enqueue_imported(
    c: &Connection,
    key: &MasterKey,
    m: &Manifest,
    snapshot: &str,
    assertions: &[Assertion],
    recovery_config: &Recovery,
) -> Result<Drill> {
    check_queue(c)?;
    m.validate(&m.database_id)?;
    validate_assertions(assertions)?;
    recovery_config.validate()?;
    if snapshot.len() != 64 || !snapshot.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Invalid(
            "imported recovery requires a full snapshot identity".into(),
        ));
    }
    if !list(c, Some(&m.database_id), true)?.is_empty() {
        return Err(Error::Conflict("a drill for this source is already pending".into()));
    }
    let id = new_id("task");
    let raw = Zeroizing::new(serde_json::to_string(assertions)?);
    let sealed = key.seal_str(&aad("restore_drills", "assertions", &id), &raw);
    let names: Vec<_> = assertions.iter().map(|a| a.name.as_str()).collect();
    c.execute("INSERT INTO restore_drills(id,source_id,backup_id,snapshot,source_name,manifest,assertions,assertion_names,bytes,created_at) VALUES(?1,?2,'imported',?3,?4,?5,?6,?7,?8,?9)",params![id,m.database_id,snapshot,m.database_name,serde_json::to_string(m)?,sealed,serde_json::to_string(&names)?,m.bytes,now()])?;
    use_recovery(c, key, &id, recovery_config)?;
    get(c, &id)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleUpdate {
    pub enabled: bool,
    pub schedule: String,
    #[serde(default)]
    pub assertions: Vec<Assertion>,
}
#[derive(Debug, Serialize)]
pub struct Schedule {
    pub database_id: String,
    pub enabled: bool,
    pub schedule: String,
    pub created_at: String,
    pub last_due: Option<String>,
    pub last_job: Option<String>,
    pub error: String,
    pub assertions: Vec<Assertion>,
}
type StoredSchedule = (bool, String, String, Option<String>, Option<String>, String, Vec<u8>);
pub fn schedule(c: &Connection, key: &MasterKey, database: &str) -> Result<Schedule> {
    let raw:Option<StoredSchedule>=c.query_row("SELECT enabled,schedule,created_at,last_due,last_job,error,assertions FROM restore_drill_schedules WHERE database_id=?1",[database],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    if let Some((enabled, schedule, created_at, last_due, last_job, error, sealed)) = raw {
        let spec = key.open_string(&aad("restore_drill_schedules", "assertions", database), &sealed)?;
        Ok(Schedule {
            database_id: database.into(),
            enabled,
            schedule,
            created_at,
            last_due,
            last_job,
            error,
            assertions: serde_json::from_str(&spec)?,
        })
    } else {
        Ok(Schedule {
            database_id: database.into(),
            enabled: false,
            schedule: "0 3 1 * *".into(),
            created_at: now(),
            last_due: None,
            last_job: None,
            error: String::new(),
            assertions: vec![],
        })
    }
}
pub fn set_schedule(c: &Connection, key: &MasterKey, database: &str, request: &ScheduleUpdate) -> Result<Schedule> {
    validate_assertions(&request.assertions)?;
    let source = databases::get(c, key, database, false)?;
    if source.kind != vercelify_core::platform::DatabaseKind::Postgresql
        || service_network(c, &source.id)?.is_some()
        || crate::preview_db::service_network(c, &source.id)?.is_some()
    {
        return Err(Error::Invalid(
            "schedule drills for a production PostgreSQL service only".into(),
        ));
    }
    if request.schedule.len() > 120 || request.schedule.split_whitespace().count() != 5 {
        return Err(Error::Invalid("use a bounded five-field UTC cron expression".into()));
    }
    let raw = Zeroizing::new(serde_json::to_string(&request.assertions)?);
    let sealed = key.seal_str(&aad("restore_drill_schedules", "assertions", &source.id), &raw);
    c.execute("INSERT INTO restore_drill_schedules(database_id,enabled,schedule,assertions,created_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(database_id) DO UPDATE SET enabled=excluded.enabled,schedule=excluded.schedule,assertions=excluded.assertions,created_at=CASE WHEN restore_drill_schedules.schedule!=excluded.schedule THEN excluded.created_at ELSE restore_drill_schedules.created_at END,last_due=CASE WHEN restore_drill_schedules.schedule!=excluded.schedule THEN NULL ELSE restore_drill_schedules.last_due END,error=''",params![source.id,request.enabled,request.schedule,sealed,now()])?;
    schedule(c, key, &source.id)
}
pub fn schedules(c: &Connection, key: &MasterKey) -> Result<Vec<Schedule>> {
    let mut q = c.prepare("SELECT database_id FROM restore_drill_schedules WHERE enabled=1")?;
    let ids = q
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.iter().map(|id| schedule(c, key, id)).collect()
}

fn check_queue(c: &Connection) -> Result<()> {
    let count: u32 = c.query_row(
        "SELECT count(*) FROM restore_drills WHERE state IN ('queued','deferred','running','verifying','cleanup')",
        [],
        |r| r.get(0),
    )?;
    if count >= 20 {
        return Err(Error::Capacity(
            "restore drill queue is full; finish or cancel pending jobs first".into(),
        ));
    }
    Ok(())
}
/// Called in the same transaction as due-time selection. A missed schedule
/// queues one catch-up job; it never stacks work for the same source.
pub fn enqueue_scheduled(c: &Connection, key: &MasterKey, database: &str, due: &str) -> Result<Option<Drill>> {
    let scheduled = schedule(c, key, database)?;
    if !scheduled.enabled || scheduled.last_due.as_deref().is_some_and(|last| last >= due) {
        return Ok(None);
    }
    if !list(c, Some(database), true)?.is_empty() {
        return Ok(None);
    }
    let backup:Option<String> = c.query_row("SELECT id FROM database_backups WHERE database_id=?1 AND status='succeeded' AND manifest IS NOT NULL ORDER BY created_at DESC,rowid DESC LIMIT 1",[database],|r|r.get(0)).optional()?;
    let result = backup
        .ok_or_else(|| Error::Invalid("overdue: no supported successful backup manifest; take a new backup".into()))
        .and_then(|backup| enqueue(c, key, database, &backup, &scheduled.assertions));
    match result {
        Ok(job) => {
            c.execute(
                "UPDATE restore_drill_schedules SET last_due=?2,last_job=?3,error='' WHERE database_id=?1",
                params![database, due, job.id],
            )?;
            Ok(Some(job))
        }
        Err(error) => {
            c.execute(
                "UPDATE restore_drill_schedules SET error=?2 WHERE database_id=?1",
                params![database, error.message()],
            )?;
            Ok(None)
        }
    }
}

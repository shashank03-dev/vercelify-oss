//! Durable post-publication observation. No database/volume restoration is
//! implied by an image rollback, and a requested rollback is never success.
use crate::{apps, deployments, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use vercelify_core::platform::{Deployment, DeploymentStatus, DeploymentTrigger, Desired};

pub fn fingerprint_base(c: &Connection, key: &crate::secrets::MasterKey, app: &str) -> Result<String> {
    let app = apps::get(c, app)?;
    let vars = crate::env::resolve_for_deploy(c, key, &app.id, false, None)?;
    let vars: Vec<_> = vars
        .vars
        .iter()
        .map(|(name, var)| (name, var.value.as_str(), var.build, var.secret))
        .collect();
    let volumes = crate::volumes::list(c, vercelify_core::platform::VolumeOwner::App, &app.id)?;
    let node = app.node_id.clone().unwrap_or(crate::nodes::main(c)?.id);
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            app.runtime,
            app.environment_id,
            node,
            vars,
            volumes
        ))?)
    ))
}
pub fn with_port(base: &str, port: u16) -> String {
    format!("{:x}", Sha256::digest(format!("{base}:{port}")))
}
pub fn fingerprint(c: &Connection, key: &crate::secrets::MasterKey, app: &str, port: u16) -> Result<String> {
    Ok(with_port(&fingerprint_base(c, key, app)?, port))
}
pub fn compatible(c: &Connection, key: &crate::secrets::MasterKey, g: &Guard) -> Result<bool> {
    let (port, _) = deployments::built_info(c, &g.deployment_id)?;
    let app = apps::get(c, &g.app_id)?;
    let current = fingerprint(c, key, &g.app_id, app.runtime.port.or(port).unwrap_or(3000))?;
    let saved: Option<String> = c
        .query_row(
            "SELECT fingerprint FROM deployment_configs WHERE deployment_id=?1",
            [&g.deployment_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(saved.as_deref() == Some(&current))
}
/// Generated build tags and digest-pinned external images are recoverable.
/// A mutable external tag cannot identify the previous version reliably.
pub fn immutable_image(app: &str, d: &Deployment) -> bool {
    d.image
        .strip_prefix("sha256:")
        .is_some_and(|digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()))
        || d.image == format!("vfy/{app}:{}", d.id.trim_start_matches("dpl_"))
        || d.image
            .rsplit_once("@sha256:")
            .is_some_and(|(_, digest)| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub const WATCH_SECS: i64 = 180;
pub const RECOVERY_SECS: i64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guard {
    pub deployment_id: String,
    pub app_id: String,
    pub previous_id: String,
    pub deadline: i64,
    pub grace_until: i64,
    pub failure_limit: u8,
    pub failures: u8,
    pub baseline: BTreeMap<String, u64>,
    pub state: String,
    pub reason: String,
    pub rollback_id: Option<String>,
    pub requested_at: Option<i64>,
}
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub compatible: bool,
    pub grace_secs: u32,
    pub failure_limit: u8,
}
#[derive(Debug)]
pub enum Observation {
    Healthy,
    Failed {
        reason: String,
        crash_loop: bool,
    },
    /// Paused, missing or unreachable: no evidence of a defective image.
    Unknown,
}
pub fn get(c: &Connection, id: &str) -> Result<Option<Guard>> {
    let row = c.query_row("SELECT deployment_id,app_id,previous_id,deadline,grace_until,failure_limit,failures,baseline,state,reason,rollback_id,requested_at FROM deployment_guards WHERE deployment_id=?1",[id],|r|Ok(Guard {
        deployment_id:r.get(0)?,app_id:r.get(1)?,previous_id:r.get(2)?,deadline:r.get(3)?,grace_until:r.get(4)?,failure_limit:r.get(5)?,failures:r.get(6)?,baseline:BTreeMap::new(),state:r.get(8)?,reason:r.get(9)?,rollback_id:r.get(10)?,requested_at:r.get(11)?
    })).optional()?;
    if let Some(mut row) = row {
        let raw: String = c.query_row(
            "SELECT baseline FROM deployment_guards WHERE deployment_id=?1",
            [id],
            |r| r.get(0),
        )?;
        row.baseline = serde_json::from_str(&raw)?;
        Ok(Some(row))
    } else {
        Ok(None)
    }
}
pub fn list(c: &Connection, app: Option<&str>, active: bool) -> Result<Vec<Guard>> {
    let mut q=c.prepare("SELECT deployment_id FROM deployment_guards WHERE (?1 IS NULL OR app_id=?1) AND (?2=0 OR state IN ('watching','requested')) ORDER BY deadline DESC LIMIT CASE WHEN ?2=1 THEN -1 ELSE 100 END")?;
    let ids = q
        .query_map(params![app, active], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.iter()
        .map(|id| get(c, id)?.ok_or_else(|| Error::not_found("deployment guard")))
        .collect()
}
/// Called in the same transaction as promotion. Every production publication
/// records compatibility; a legacy/changed configuration is never guessed safe.
#[allow(clippy::too_many_arguments)]
pub fn published(
    c: &Connection,
    d: &Deployment,
    previous: Option<&str>,
    fingerprint: &str,
    baseline: &BTreeMap<String, u64>,
    policy: Policy,
    now: i64,
) -> Result<()> {
    if d.pr_number.is_some() {
        return Ok(());
    }
    c.execute(
        "INSERT OR REPLACE INTO deployment_configs(deployment_id,fingerprint) VALUES(?1,?2)",
        params![d.id, fingerprint],
    )?;
    let Some(previous) = previous.filter(|id| *id != d.id) else {
        return Ok(());
    };
    if !policy.compatible
        || baseline.is_empty()
        || !matches!(
            d.trigger,
            DeploymentTrigger::Push | DeploymentTrigger::Manual | DeploymentTrigger::Hook | DeploymentTrigger::Webhook
        )
    {
        return Ok(());
    }
    let old: Option<String> = c
        .query_row(
            "SELECT fingerprint FROM deployment_configs WHERE deployment_id=?1",
            [previous],
            |r| r.get(0),
        )
        .optional()?;
    if old.as_deref() != Some(fingerprint) {
        return Ok(());
    }
    let old = deployments::get(c, previous)?;
    if !immutable_image(&apps::get(c, &d.app_id)?.name, &old)
        || !matches!(old.status, DeploymentStatus::Healthy | DeploymentStatus::Superseded)
    {
        return Ok(());
    }
    c.execute("INSERT OR IGNORE INTO deployment_guards(deployment_id,app_id,previous_id,deadline,grace_until,failure_limit,baseline) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![d.id,d.app_id,previous,now+WATCH_SECS,now+i64::from(policy.grace_secs),policy.failure_limit.max(1),serde_json::to_string(baseline)?])?;
    Ok(())
}
pub fn finish(c: &Connection, id: &str, state: &str, reason: &str, now: i64) -> Result<()> {
    if !matches!(state, "verified" | "failed" | "cancelled" | "expired") {
        return Err(Error::Invalid("invalid guard outcome".into()));
    }
    c.execute("UPDATE deployment_guards SET state=?2,reason=?3,finished_at=?4 WHERE deployment_id=?1 AND state IN ('watching','requested')",params![id,state,crate::logs::strip_ansi(reason).chars().take(1024).collect::<String>(),now])?;
    Ok(())
}
/// Returns true only when this observation may request the single rollback.
pub fn observe(c: &Connection, id: &str, observation: Observation, now: i64) -> Result<bool> {
    let Some(g) = get(c, id)? else { return Ok(false) };
    if g.state != "watching" {
        return Ok(false);
    }
    if now >= g.deadline {
        finish(c, id, "expired", "observation window ended", now)?;
        return Ok(false);
    }
    if now < g.grace_until {
        return Ok(false);
    }
    match observation {
        Observation::Unknown => Ok(false),
        Observation::Healthy => {
            c.execute(
                "UPDATE deployment_guards SET failures=0,reason='' WHERE deployment_id=?1",
                [id],
            )?;
            Ok(false)
        }
        Observation::Failed { reason, crash_loop } => {
            let failures = if crash_loop {
                g.failure_limit
            } else {
                g.failures.saturating_add(1).min(g.failure_limit)
            };
            c.execute(
                "UPDATE deployment_guards SET failures=?2,reason=?3 WHERE deployment_id=?1",
                params![
                    id,
                    failures,
                    crate::logs::strip_ansi(&reason).chars().take(1024).collect::<String>()
                ],
            )?;
            Ok(failures >= g.failure_limit)
        }
    }
}
/// Checked inside the rollback enqueue transaction, fencing newer work/stops.
pub fn requestable(c: &Connection, id: &str, now: i64) -> Result<Guard> {
    let g = get(c, id)?.ok_or_else(|| Error::not_found("deployment guard"))?;
    let app = apps::get(c, &g.app_id)?;
    let busy:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM deployments WHERE app_id=?1 AND pr_number IS NULL AND status IN ('queued','building','deploying'))",[&g.app_id],|r|r.get(0))?;
    if g.state != "watching"
        || g.failures < g.failure_limit
        || now >= g.deadline
        || app.current_deployment_id.as_deref() != Some(id)
        || app.desired != Desired::Running
        || busy
    {
        return Err(Error::Conflict(
            "deployment guard was superseded or is not ready".into(),
        ));
    }
    Ok(g)
}
pub fn requested(c: &Connection, id: &str, rollback: &str, now: i64) -> Result<()> {
    let changed=c.execute("UPDATE deployment_guards SET state='requested',rollback_id=?2,requested_at=?3 WHERE deployment_id=?1 AND state='watching' AND rollback_id IS NULL",params![id,rollback,now])?;
    if changed != 1 {
        return Err(Error::Conflict("rollback was already requested".into()));
    }
    Ok(())
}

pub fn retained_images(c: &Connection, app: &str) -> Result<Vec<String>> {
    let mut q=c.prepare("SELECT DISTINCT d.image FROM deployment_guards g JOIN deployments d ON d.id=g.previous_id JOIN apps a ON a.id=g.app_id WHERE a.name=?1 AND g.state IN ('watching','requested')")?;
    let rows = q
        .query_map([app], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Rechecked before starting and at the publication commit. A queued request
/// grants no authority to override a later stop, configuration change or job.
pub fn rollback_allowed(c: &Connection, key: &crate::secrets::MasterKey, id: &str) -> Result<bool> {
    let owner: Option<String> = c
        .query_row(
            "SELECT deployment_id FROM deployment_guards WHERE rollback_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(owner) = owner else { return Ok(false) };
    let Some(g) = get(c, &owner)? else { return Ok(false) };
    let app = apps::get(c, &g.app_id)?;
    let d = deployments::get(c, id)?;
    let newer:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM deployments WHERE app_id=?1 AND pr_number IS NULL AND number>?2 AND status IN ('queued','building','deploying'))",params![g.app_id,d.number],|r|r.get(0))?;
    Ok(g.state == "requested"
        && app.desired == Desired::Running
        && matches!(app.current_deployment_id.as_deref(),Some(current) if current==g.deployment_id || current==id)
        && chrono::Utc::now().timestamp() < g.requested_at.unwrap_or(0) + RECOVERY_SECS
        && !newer
        && compatible(c, key, &g)?)
}

//! Deployment records (design §7). This module owns the rows; the deployer
//! drives the state machine through [`transition`].

use crate::ids::new_id;
use crate::{now, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use vercelify_core::platform::{Deployment, DeploymentKind, DeploymentStatus, DeploymentTrigger};

fn row(r: &Row) -> rusqlite::Result<Deployment> {
    let status: String = r.get("status")?;
    let trigger: String = r.get("trigger")?;
    let pr: Option<u32> = r.get("pr_number")?;
    Ok(Deployment {
        id: r.get("id")?,
        app_id: r.get("app_id")?,
        number: r.get("number")?,
        kind: if pr.is_some() {
            DeploymentKind::Preview
        } else {
            DeploymentKind::Production
        },
        pr_number: pr,
        branch: r.get("branch")?,
        commit_sha: r.get("commit_sha")?,
        commit_message: r.get("commit_message")?,
        trigger: DeploymentTrigger::parse(&trigger).unwrap_or(DeploymentTrigger::Manual),
        rollback_of: r.get("rollback_of")?,
        image: r.get("image")?,
        node_id: r.get("node_id")?,
        status: DeploymentStatus::parse(&status).unwrap_or(DeploymentStatus::Failed),
        error: r.get("error")?,
        current: r.get("current")?,
        created_at: r.get("created_at")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
    })
}

const SELECT: &str = "SELECT d.*, (a.current_deployment_id IS d.id) AS current
  FROM deployments d JOIN apps a ON a.id = d.app_id";

pub fn list(c: &Connection, app_id: &str, limit: u32) -> Result<Vec<Deployment>> {
    let mut st = c.prepare(&format!("{SELECT} WHERE d.app_id = ?1 ORDER BY d.number DESC LIMIT ?2"))?;
    let rows = st.query_map(params![app_id, limit.clamp(1, 200)], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(c: &Connection, id: &str) -> Result<Deployment> {
    c.query_row(&format!("{SELECT} WHERE d.id = ?1"), [id], row)
        .optional()?
        .ok_or_else(|| Error::not_found(format!("deployment {id}")))
}

/// Deployments in `queued|building|deploying` (oldest first), e.g. to mark
/// them interrupted after a restart.
pub fn active(c: &Connection) -> Result<Vec<Deployment>> {
    let mut st = c.prepare(&format!(
        "{SELECT} WHERE d.status IN ('queued', 'building', 'deploying') ORDER BY d.created_at, d.number"
    ))?;
    let rows = st.query_map([], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub struct NewDeployment<'a> {
    pub app_id: &'a str,
    pub pr_number: Option<u32>,
    pub branch: &'a str,
    pub commit_sha: &'a str,
    pub commit_message: &'a str,
    pub trigger: DeploymentTrigger,
    pub rollback_of: Option<&'a str>,
    /// Known up front for image apps and rollbacks (no build).
    pub image: &'a str,
    pub node_id: Option<&'a str>,
}

/// Insert a queued deployment with the next number for its app. Queued
/// production deployments of the same app (not started yet) are superseded:
/// only the newest push is worth building (design §6.1).
pub fn create(c: &Connection, d: &NewDeployment) -> Result<Deployment> {
    let number: u32 = c.query_row(
        "SELECT coalesce(max(number), 0) + 1 FROM deployments WHERE app_id = ?1",
        [d.app_id],
        |r| r.get(0),
    )?;
    let id = new_id("dpl");
    let t = now();
    let kind = if d.pr_number.is_some() { "preview" } else { "production" };
    c.execute(
        "UPDATE deployments SET status = 'superseded', finished_at = ?3
         WHERE app_id = ?1 AND status = 'queued' AND pr_number IS ?2",
        params![d.app_id, d.pr_number, t],
    )?;
    c.execute(
        "INSERT INTO deployments (id, app_id, number, kind, pr_number, branch, commit_sha, commit_message, trigger,
                                  rollback_of, image, node_id, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'queued', ?13)",
        params![
            id,
            d.app_id,
            number,
            kind,
            d.pr_number,
            d.branch,
            d.commit_sha,
            d.commit_message.chars().take(500).collect::<String>(),
            d.trigger.as_str(),
            d.rollback_of,
            d.image,
            d.node_id,
            t
        ],
    )?;
    get(c, &id)
}

/// Allowed moves of the state machine (design §7.1).
pub fn allowed(from: DeploymentStatus, to: DeploymentStatus) -> bool {
    use DeploymentStatus::*;
    matches!(
        (from, to),
        (Queued, Building | Deploying | Failed | Cancelled | Superseded)
            | (Building, Deploying | Failed | Cancelled)
            | (Deploying, Healthy | Failed | Cancelled)
            | (Healthy, Superseded)
    )
}

/// Move a deployment to `to`, recording times and the error. Refuses moves
/// the state machine doesn't allow, so a late callback can't resurrect a
/// cancelled deployment.
pub fn transition(c: &Connection, id: &str, to: DeploymentStatus, error: Option<&str>) -> Result<Deployment> {
    let cur = get(c, id)?;
    if !allowed(cur.status, to) {
        return Err(Error::Conflict(format!(
            "deployment {} is {}, it can't become {}",
            cur.number,
            cur.status.as_str(),
            to.as_str()
        )));
    }
    let t = now();
    let started = matches!(to, DeploymentStatus::Building | DeploymentStatus::Deploying);
    // Leaving the active states finishes it; healthy → superseded keeps the
    // time it finished deploying.
    let finished = cur.status.is_active() && !to.is_active();
    c.execute(
        "UPDATE deployments SET status = ?2,
           started_at = CASE WHEN ?3 AND started_at IS NULL THEN ?5 ELSE started_at END,
           finished_at = CASE WHEN ?4 THEN ?5 ELSE finished_at END,
           error = CASE WHEN ?6 IS NOT NULL THEN ?6 ELSE error END
         WHERE id = ?1",
        params![id, to.as_str(), started, finished, t, error],
    )?;
    get(c, id)
}

pub fn set_image(c: &Connection, id: &str, image: &str) -> Result<()> {
    c.execute("UPDATE deployments SET image = ?2 WHERE id = ?1", params![id, image])?;
    Ok(())
}

/// What the build found out: the commit, the image, its port and framework.
pub fn set_built(
    c: &Connection,
    id: &str,
    commit_sha: &str,
    commit_message: &str,
    image: &str,
    port: u16,
    framework: &str,
) -> Result<()> {
    c.execute(
        "UPDATE deployments SET commit_sha = CASE WHEN ?2 = '' THEN commit_sha ELSE ?2 END,
           commit_message = CASE WHEN ?3 = '' THEN commit_message ELSE ?3 END,
           image = ?4, port = ?5, framework = ?6 WHERE id = ?1",
        params![
            id,
            commit_sha,
            commit_message.chars().take(500).collect::<String>(),
            image,
            port,
            framework
        ],
    )?;
    Ok(())
}

pub fn set_project_files(c: &Connection, id: &str, files: &[(String, String)]) -> Result<()> {
    c.execute(
        "UPDATE deployments SET project_files = ?2 WHERE id = ?1",
        params![id, serde_json::to_string(files)?],
    )?;
    Ok(())
}

pub fn project_files(c: &Connection, id: &str) -> Result<Vec<(String, String)>> {
    let raw: String = c.query_row("SELECT project_files FROM deployments WHERE id = ?1", [id], |r| {
        r.get(0)
    })?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

/// The port and framework recorded at build time.
pub fn built_info(c: &Connection, id: &str) -> Result<(Option<u16>, String)> {
    Ok(
        c.query_row("SELECT port, framework FROM deployments WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?,
    )
}

pub fn set_container(c: &Connection, id: &str, container: &str, node_id: Option<&str>) -> Result<()> {
    c.execute(
        "UPDATE deployments SET container = ?2, node_id = coalesce(?3, node_id) WHERE id = ?1",
        params![id, container, node_id],
    )?;
    Ok(())
}

pub fn container(c: &Connection, id: &str) -> Result<String> {
    Ok(c.query_row("SELECT container FROM deployments WHERE id = ?1", [id], |r| r.get(0))?)
}

/// Make `id` the deployment serving production traffic; the previous one
/// becomes superseded. The commit point of a deploy (design §7.2 step 4).
pub fn promote(c: &Connection, app_id: &str, id: &str) -> Result<Option<String>> {
    let previous: Option<String> =
        c.query_row("SELECT current_deployment_id FROM apps WHERE id = ?1", [app_id], |r| {
            r.get(0)
        })?;
    // A deploy wakes a sleeping app: it just started, and is routed.
    c.execute(
        "UPDATE apps SET current_deployment_id = ?2, updated_at = ?3,
           desired = CASE desired WHEN 'sleeping' THEN 'running' ELSE desired END
         WHERE id = ?1",
        params![app_id, id, now()],
    )?;
    if let Some(p) = previous.as_deref().filter(|p| *p != id) {
        let prev = get(c, p)?;
        if prev.status == DeploymentStatus::Healthy {
            transition(c, p, DeploymentStatus::Superseded, None)?;
        }
    }
    Ok(previous)
}

/// After a restart: whatever was mid-flight was interrupted.
pub fn fail_interrupted(c: &Connection) -> Result<usize> {
    let mut n = 0;
    for d in active(c)? {
        if d.status != DeploymentStatus::Queued {
            transition(
                c,
                &d.id,
                DeploymentStatus::Failed,
                Some("interrupted: the agent restarted"),
            )?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{apps, Store};
    use DeploymentStatus::*;

    fn new<'a>(app: &'a str, pr: Option<u32>) -> NewDeployment<'a> {
        NewDeployment {
            app_id: app,
            pr_number: pr,
            branch: "main",
            commit_sha: "abc1234",
            commit_message: "fix",
            trigger: DeploymentTrigger::Push,
            rollback_of: None,
            image: "",
            node_id: None,
        }
    }

    #[test]
    fn numbers_and_superseding() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| apps::create(c, &apps::tests::git_app("blog"))).unwrap();
        let d1 = s.tx(|c| create(c, &new(&a.id, None))).unwrap();
        let p1 = s.tx(|c| create(c, &new(&a.id, Some(7)))).unwrap();
        let d2 = s.tx(|c| create(c, &new(&a.id, None))).unwrap();
        assert_eq!((d1.number, p1.number, d2.number), (1, 2, 3));
        assert_eq!(p1.kind, DeploymentKind::Preview);
        assert_eq!(
            s.with(|c| get(c, &d1.id)).unwrap().status,
            Superseded,
            "older queued push skipped"
        );
        assert_eq!(
            s.with(|c| get(c, &p1.id)).unwrap().status,
            Queued,
            "previews are their own queue"
        );
        assert_eq!(s.with(|c| list(c, &a.id, 10)).unwrap()[0].id, d2.id);
        assert_eq!(s.with(active).unwrap().len(), 2);
    }

    #[test]
    fn state_machine() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| apps::create(c, &apps::tests::git_app("blog"))).unwrap();
        let d = s.tx(|c| create(c, &new(&a.id, None))).unwrap();
        let d = s.tx(|c| transition(c, &d.id, Building, None)).unwrap();
        assert!(d.started_at.is_some() && d.finished_at.is_none());
        assert!(s.tx(|c| transition(c, &d.id, Queued, None)).is_err());
        let d = s.tx(|c| transition(c, &d.id, Deploying, None)).unwrap();
        let d = s.tx(|c| transition(c, &d.id, Healthy, None)).unwrap();
        assert!(d.finished_at.is_some());
        assert_eq!(s.tx(|c| promote(c, &a.id, &d.id)).unwrap(), None);
        assert!(s.with(|c| get(c, &d.id)).unwrap().current);

        let d2 = s.tx(|c| create(c, &new(&a.id, None))).unwrap();
        s.tx(|c| transition(c, &d2.id, Deploying, None)).unwrap();
        s.tx(|c| transition(c, &d2.id, Healthy, None)).unwrap();
        assert_eq!(
            s.tx(|c| promote(c, &a.id, &d2.id)).unwrap().as_deref(),
            Some(d.id.as_str())
        );
        let old = s.with(|c| get(c, &d.id)).unwrap();
        assert_eq!(old.status, Superseded);
        assert!(!old.current);

        let d3 = s.tx(|c| create(c, &new(&a.id, None))).unwrap();
        s.tx(|c| transition(c, &d3.id, Cancelled, None)).unwrap();
        assert!(
            s.tx(|c| transition(c, &d3.id, Healthy, None)).is_err(),
            "no resurrection"
        );
    }

    #[test]
    fn interrupted_after_restart() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| apps::create(c, &apps::tests::git_app("blog"))).unwrap();
        let building = s.tx(|c| create(c, &new(&a.id, None))).unwrap();
        s.tx(|c| transition(c, &building.id, Building, None)).unwrap();
        let queued = s.tx(|c| create(c, &new(&a.id, Some(3)))).unwrap();
        assert_eq!(s.tx(|c| fail_interrupted(c)).unwrap(), 1);
        let b = s.with(|c| get(c, &building.id)).unwrap();
        assert_eq!(b.status, Failed);
        assert!(b.error.contains("interrupted"));
        assert_eq!(
            s.with(|c| get(c, &queued.id)).unwrap().status,
            Queued,
            "queued ones resume"
        );
    }
}

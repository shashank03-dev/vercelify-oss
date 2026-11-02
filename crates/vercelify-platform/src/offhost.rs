//! Persisted off-host ownership. Every publication is fenced by the deployment,
//! owner, generation, deadline and configuration/variable revision.
use crate::{apps, deployments, env, previews, secrets::MasterKey, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use vercelify_core::platform::{BuildJob, DeploymentStatus, LocalBuildPlan};

pub const LEASE_SECONDS: i64 = 300;
pub const MAX_SECONDS: i64 = 90 * 60;
pub fn timestamp() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn get(c: &Connection, id: &str) -> Result<BuildJob> {
    c.query_row(
        "SELECT j.generation, j.platform, j.phase, j.deadline, j.image_digest, a.name, j.plan_hash, j.plan_version FROM build_jobs j JOIN deployments d ON d.id=j.deployment_id JOIN apps a ON a.id=d.app_id WHERE deployment_id=?1",
        [id],
        |r| {
            let generation = r.get(0)?;
            Ok(BuildJob {
                deployment_id: id.into(),
                generation,
                platform: r.get(1)?,
                phase: r.get(2)?,
                plan_hash: r.get(6)?,
                plan_version: r.get(7)?,
                deadline: r.get(3)?,
                image: crate::build::image_tag(&r.get::<_,String>(5)?, &format!("{id}-{generation}")),
                image_digest: r.get(4)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::not_found("build job"))
}

pub fn is_external(c: &Connection, id: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM build_jobs WHERE deployment_id=?1)",
        [id],
        |r| r.get(0),
    )?)
}

pub fn dispatchable(c: &Connection, id: &str) -> Result<bool> {
    let phase: Option<String> = c
        .query_row("SELECT phase FROM build_jobs WHERE deployment_id=?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(phase.is_none_or(|p| p == "verified"))
}

pub fn create(
    c: &Connection,
    id: &str,
    owner: &str,
    revision: &str,
    platform: &str,
    plan: &LocalBuildPlan,
    now: i64,
) -> Result<BuildJob> {
    if !matches!(platform, "linux/arm64" | "linux/amd64") {
        return Err(Error::Invalid(
            "build platform must be linux/arm64 or linux/amd64".into(),
        ));
    }
    if plan.dockerfile.len() > 256 * 1024
        || plan.project_files.len() > 2
        || plan
            .project_files
            .iter()
            .any(|(n, v)| !crate::build::PROJECT_FILES.contains(&n.as_str()) || v.len() > 256 * 1024)
    {
        return Err(Error::Invalid(
            "build plan exceeds the supported size or project file limits".into(),
        ));
    }
    c.execute("INSERT INTO build_jobs(deployment_id,owner,generation,platform,revision,plan,phase,deadline,hard_deadline,plan_hash) VALUES(?1,?2,1,?3,?4,?5,'leased',?6,?7,?8)", params![id,owner,platform,revision,serde_json::to_string(plan)?,now+LEASE_SECONDS,now+MAX_SECONDS,format!("{:x}",Sha256::digest(serde_json::to_vec(plan)?))])?;
    get(c, id)
}

pub fn authorize(c: &Connection, id: &str, owner: &str, generation: u32, now: i64) -> Result<BuildJob> {
    let job = get(c, id)?;
    let actual: String = c.query_row("SELECT owner FROM build_jobs WHERE deployment_id=?1", [id], |r| {
        r.get(0)
    })?;
    let d = deployments::get(c, id)?;
    if actual != owner
        || job.generation != generation
        || job.deadline <= now
        || !matches!(job.phase.as_str(), "leased" | "uploading")
        || d.status != DeploymentStatus::Queued
    {
        return Err(Error::Conflict(
            "build lease is no longer current; start a new build".into(),
        ));
    }
    // Queued production supersession is handled by deployments::create. For
    // previews also fence an updated head/close even before the next job starts.
    if let Some(pr) = d.pr_number {
        let current = previews::get(c, &d.app_id, pr)?;
        if current.is_none_or(|p| p.status == "closed" || p.head_sha != d.commit_sha) {
            return Err(Error::Conflict("the preview was closed or its head changed".into()));
        }
    }
    Ok(job)
}

pub fn renew(c: &Connection, id: &str, owner: &str, generation: u32, now: i64) -> Result<BuildJob> {
    authorize(c, id, owner, generation, now)?;
    c.execute(
        "UPDATE build_jobs SET deadline=min(?2,hard_deadline) WHERE deployment_id=?1",
        params![id, now + LEASE_SECONDS],
    )?;
    get(c, id)
}

pub fn begin_upload(c: &Connection, id: &str, owner: &str, generation: u32, now: i64) -> Result<BuildJob> {
    let job = authorize(c, id, owner, generation, now)?;
    if job.phase != "leased" {
        return Err(Error::Conflict("an upload is already in progress".into()));
    }
    c.execute("UPDATE build_jobs SET phase='uploading' WHERE deployment_id=?1", [id])?;
    get(c, id)
}

pub fn expire(c: &Connection, now: i64) -> Result<()> {
    let mut st =
        c.prepare("SELECT deployment_id FROM build_jobs WHERE phase IN ('leased','uploading') AND deadline<=?1")?;
    let ids = st
        .query_map([now], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for id in ids {
        fail(
            c,
            &id,
            "off-host build lease expired; retry from the selected computer or CI",
        )?;
    }
    Ok(())
}

pub fn fail(c: &Connection, id: &str, reason: &str) -> Result<()> {
    c.execute(
        "UPDATE build_jobs SET phase='failed' WHERE deployment_id=?1 AND phase!='verified'",
        [id],
    )?;
    let d = deployments::get(c, id)?;
    if d.status == DeploymentStatus::Queued {
        deployments::transition(c, id, DeploymentStatus::Failed, Some(reason))?;
    }
    Ok(())
}

/// Build values for an off-host computer. Runtime-only DB variables need no
/// published port; an explicitly requested build-time DB reference does.
pub fn build_variables(
    c: &Connection,
    key: &MasterKey,
    app: &str,
    preview: bool,
) -> Result<Vec<(String, String, bool)>> {
    let raw = env::resolve_all(c, key, app, preview)?;
    let resolved = env::resolve_for_deploy(c, key, app, preview, None)?;
    let mut values = vec![];
    for (name, var) in raw.vars.iter().filter(|(_, v)| v.build) {
        let (value, secret) = if var.value.contains("${{db.") {
            let main = crate::nodes::main(c)?;
            if main.mesh_ip.is_empty() {
                return Err(Error::Unavailable(
                    "off-host database build variables need the main server's mesh address".into(),
                ));
            }
            (
                crate::databases::resolve_references_at(c, key, &var.value, Some(&main.mesh_ip))?,
                true,
            )
        } else {
            let value = &resolved.vars[name];
            (value.value.to_string(), value.secret)
        };
        values.push((name.clone(), value, secret));
    }
    Ok(values)
}

/// Includes build-relevant app settings and resolved variables, never stores values.
pub fn revision(c: &Connection, key: &MasterKey, app: &str, preview: bool) -> Result<String> {
    let a = apps::get(c, app)?;
    let vars = env::resolve_for_deploy(c, key, app, preview, None)?;
    let vars: Vec<_> = vars
        .vars
        .iter()
        .map(|(k, v)| (k, v.value.as_str(), v.build, v.secret))
        .collect();
    let build_vars = build_variables(c, key, app, preview)?;
    let bytes = serde_json::to_vec(&(
        a.source,
        a.build,
        a.runtime,
        a.node_id,
        a.environment_id,
        vars,
        build_vars,
    ))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn verify_revision(c: &Connection, key: &MasterKey, id: &str) -> Result<()> {
    let d = deployments::get(c, id)?;
    let expected: String = c.query_row("SELECT revision FROM build_jobs WHERE deployment_id=?1", [id], |r| {
        r.get(0)
    })?;
    if revision(c, key, &d.app_id, d.pr_number.is_some())? != expected {
        return Err(Error::Conflict(
            "app settings or variables changed during the build; retry".into(),
        ));
    }
    Ok(())
}

pub fn publish(
    c: &Connection,
    key: &MasterKey,
    id: &str,
    owner: &str,
    generation: u32,
    digest: &str,
    now: i64,
) -> Result<BuildJob> {
    let job = authorize(c, id, owner, generation, now)?;
    if job.phase != "uploading" {
        return Err(Error::Conflict("image upload has not completed".into()));
    }
    verify_revision(c, key, id)?;
    let plan: String = c.query_row("SELECT plan FROM build_jobs WHERE deployment_id=?1", [id], |r| r.get(0))?;
    let plan: LocalBuildPlan = serde_json::from_str(&plan)?;
    let d = deployments::get(c, id)?;
    deployments::set_built(c, id, &d.commit_sha, "", &job.image, plan.port, &plan.framework)?;
    deployments::set_project_files(c, id, &plan.project_files)?;
    c.execute(
        "UPDATE build_jobs SET phase='verified',image_digest=?2 WHERE deployment_id=?1",
        params![id, digest],
    )?;
    get(c, id)
}

/// An agent restart fences incomplete uploads/builds instead of reviving a
/// disconnected sender. Already verified artifacts may still be dispatched.
pub fn interrupt(c: &Connection) -> Result<usize> {
    let mut statement = c.prepare("SELECT deployment_id FROM build_jobs WHERE phase IN ('leased','uploading')")?;
    let ids = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for id in &ids {
        fail(c, id, "interrupted: the agent restarted during the off-host build")?;
    }
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn build_database_references_need_a_mesh_port_and_resolve_for_the_computer() {
        let s = crate::Store::open_in_memory().unwrap();
        let key = MasterKey::from_bytes(&[7; 32]).unwrap();
        let app=s.tx(|c| {
            let app=apps::create(c,&apps::tests::git_app("local"))?;
            crate::nodes::ensure_main(c,"main","http://127.0.0.1:7420","aarch64")?;
            c.execute("UPDATE nodes SET mesh_ip='10.77.0.1'",[])?;
            let db=serde_json::from_value(serde_json::json!({"name":"build-db","kind":"postgresql","public_port":15432}))?;
            crate::databases::create(c,&key,&db)?;
            let vars=serde_json::from_value(serde_json::json!({"vars":[{"key":"DATABASE_URL","value":"${{db.build-db.url}}","build":true,"secret":true}],"replace":false}))?;
            env::put(c,&key,vercelify_core::platform::VarScope::App,&app.id,&vars)?;
            Ok(app)
        }).unwrap();
        let vars = s.with(|c| build_variables(c, &key, &app.id, false)).unwrap();
        assert!(
            vars[0].1.contains("10.77.0.1:15432"),
            "off-host builds need the mesh URL"
        );
        assert!(vars[0].2);
        s.tx(|c| {
            c.execute("UPDATE databases SET public_port=NULL", [])?;
            Ok(())
        })
        .unwrap();
        assert!(
            s.with(|c| build_variables(c, &key, &app.id, false)).is_err(),
            "no unusable Docker-internal URL"
        );
    }
}

//! Owner-authorized isolated restores. Public outcomes contain scope and status,
//! never assertions, database passwords or recovery credentials.
use crate::{
    error::{ApiResult, AppError},
    json::Json,
    platform_api::platform,
    routes::AppState,
    tokens::Actor,
};
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Extension, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use vercelify_core::platform::Role;
use vercelify_platform::{
    databases,
    restore_drill::{self as drill, Assertion, Drill, Manifest, Recovery, ScheduleUpdate},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/platform/databases/{db}/restore-drills", get(list).post(create))
        .route(
            "/platform/databases/{db}/restore-drill-schedule",
            get(schedule).put(set_schedule),
        )
        .route("/platform/restore-drills/import", post(import))
        .route("/platform/restore-drills/{id}", get(status))
        .route("/platform/restore-drills/{id}/cancel", post(cancel))
}
fn owner(actor: &Actor) -> Result<(), AppError> {
    if actor.role != Role::Owner {
        return Err(AppError::forbidden("only an owner may configure or run restore drills"));
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    backup_id: String,
    #[serde(default)]
    assertions: Vec<Assertion>,
    #[serde(default)]
    full_integrity: bool,
    recovery: Option<Recovery>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Import {
    manifest: Manifest,
    snapshot: String,
    recovery: Recovery,
    #[serde(default)]
    assertions: Vec<Assertion>,
    #[serde(default)]
    full_integrity: bool,
}
async fn list(State(s): State<AppState>, Path(db): Path<String>) -> ApiResult<Vec<Drill>> {
    let p = platform(&s)?;
    let key = p.key.clone();
    Ok(Json(
        p.store
            .call(move |c| {
                let db = databases::get(c, &key, &db, false)?;
                drill::list(c, Some(&db.id), false)
            })
            .await?,
    ))
}
async fn status(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Drill> {
    Ok(Json(platform(&s)?.store.call(move |c| drill::get(c, &id)).await?))
}
async fn create(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(db): Path<String>,
    Json(req): Json<Request>,
) -> ApiResult<Drill> {
    owner(&actor)?;
    let p = platform(&s)?;
    let key = p.key.clone();
    let job = p
        .store
        .call(move |c| {
            let job = drill::enqueue(c, &key, &db, &req.backup_id, &req.assertions)?;
            if let Some(recovery) = req.recovery {
                drill::use_recovery(c, &key, &job.id, &recovery)?;
            }
            c.execute(
                "UPDATE restore_drills SET full_integrity=?2 WHERE id=?1",
                vercelify_platform::rusqlite::params![job.id, req.full_integrity],
            )?;
            drill::get(c, &job.id)
        })
        .await?;
    s.events.changed("restore-drills");
    Ok(Json(job))
}
async fn import(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(req): Json<Import>,
) -> ApiResult<Drill> {
    owner(&actor)?;
    let p = platform(&s)?;
    let key = p.key.clone();
    let job = p
        .store
        .call(move |c| {
            let job = drill::enqueue_imported(c, &key, &req.manifest, &req.snapshot, &req.assertions, &req.recovery)?;
            c.execute(
                "UPDATE restore_drills SET full_integrity=?2 WHERE id=?1",
                vercelify_platform::rusqlite::params![job.id, req.full_integrity],
            )?;
            Ok(job)
        })
        .await?;
    s.events.changed("restore-drills");
    Ok(Json(job))
}
async fn cancel(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
) -> ApiResult<Drill> {
    owner(&actor)?;
    let job = platform(&s)?
        .store
        .call(move |c| {
            drill::cancel(c, &id)?;
            drill::get(c, &id)
        })
        .await?;
    s.events.changed("restore-drills");
    Ok(Json(job))
}
fn schedule_value(schedule: drill::Schedule) -> Value {
    let base = schedule.last_due.as_deref().unwrap_or(&schedule.created_at);
    let next = chrono::DateTime::parse_from_rfc3339(base).ok().and_then(|t| {
        crate::cron::parse(&schedule.schedule)
            .ok()?
            .next_after(t.with_timezone(&chrono::Utc))
    });
    let overdue = schedule.enabled && next.is_some_and(|t| t <= chrono::Utc::now());
    json!({"schedule":schedule,"next_due":next.map(|t| t.to_rfc3339()),"overdue":overdue})
}
async fn schedule(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(db): Path<String>,
) -> ApiResult<Value> {
    owner(&actor)?;
    let p = platform(&s)?;
    let key = p.key.clone();
    let v = p
        .store
        .call(move |c| {
            let db = databases::get(c, &key, &db, false)?;
            drill::schedule(c, &key, &db.id)
        })
        .await?;
    Ok(Json(schedule_value(v)))
}
async fn set_schedule(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(db): Path<String>,
    Json(req): Json<ScheduleUpdate>,
) -> ApiResult<Value> {
    owner(&actor)?;
    crate::cron::parse(&req.schedule).map_err(AppError::bad_request)?;
    let p = platform(&s)?;
    let key = p.key.clone();
    let v = p.store.call(move |c| drill::set_schedule(c, &key, &db, &req)).await?;
    s.events.changed("restore-drills");
    Ok(Json(schedule_value(v)))
}
/// One loop, including when git watching is disabled. No per-tick spawned
/// waiters: a long restore finishes before another scheduling pass starts.
pub async fn run(p: Arc<crate::platform::Platform>, cfg: Arc<crate::config::Config>) {
    loop {
        let now = chrono::Utc::now();
        let scheduled = p.store.tx(|c| {
            for schedule in drill::schedules(c, &p.key)? {
                let base = schedule.last_due.as_deref().unwrap_or(&schedule.created_at);
                let due = chrono::DateTime::parse_from_rfc3339(base).ok().and_then(|t| {
                    crate::cron::parse(&schedule.schedule)
                        .ok()?
                        .next_after(t.with_timezone(&chrono::Utc))
                });
                if due.is_some_and(|due| due <= now) {
                    drill::enqueue_scheduled(c, &p.key, &schedule.database_id, &now.to_rfc3339())?;
                }
            }
            Ok(())
        });
        if let Err(e) = scheduled {
            tracing::warn!("restore drill schedule: {e}");
        }
        for error in p
            .engine
            .reconcile_restore_drills(&crate::platform_api::restic(&cfg))
            .await
        {
            tracing::warn!("{error}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}
/// Doctor and the existing watcher share this bounded, secret-free check. The
/// watcher deduplicates unchanged warnings and resolves them after recovery.
pub fn check(p: &crate::platform::Platform) -> vercelify_core::DoctorCheck {
    let now = chrono::Utc::now();
    let result = p.store.with(|c| {
        let mut failures = 0;
        let mut deferred = 0;
        let mut overdue = 0;
        let mut seen = std::collections::HashSet::new();
        for job in drill::list(c, None, false)? {
            if !seen.insert(job.source_id.clone()) {
                continue;
            }
            if job.state == "failed" {
                failures += 1;
            }
            let age = chrono::DateTime::parse_from_rfc3339(&job.created_at)
                .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds())
                .unwrap_or(0);
            if age >= 300 && (job.state == "deferred" || job.state == "cleanup") {
                deferred += 1;
            }
        }
        for schedule in drill::schedules(c, &p.key)? {
            let base = schedule.last_due.as_deref().unwrap_or(&schedule.created_at);
            let next = chrono::DateTime::parse_from_rfc3339(base).ok().and_then(|t| {
                crate::cron::parse(&schedule.schedule)
                    .ok()?
                    .next_after(t.with_timezone(&chrono::Utc))
            });
            if !schedule.error.is_empty() || next.is_some_and(|due| due + chrono::Duration::minutes(5) <= now) {
                overdue += 1;
            }
        }
        Ok((failures, deferred, overdue))
    });
    let (status,detail)=match result {
        Ok((0,0,0))=>(vercelify_core::CheckStatus::Ok,"No failed, overdue or persistently deferred restore drills. This is scheduling status, not backup verification.".into()),
        Ok((failed,deferred,overdue))=>(vercelify_core::CheckStatus::Warn,format!("{failed} sources have a failed latest drill; {deferred} deferred/cleanup jobs exceed five minutes; {overdue} schedules are overdue. Inspect restore drill status.")),
        Err(_)=>(vercelify_core::CheckStatus::Warn,"Restore drill status is unavailable.".into()),
    };
    vercelify_core::DoctorCheck {
        id: "restore-drills".into(),
        label: "Restore drills".into(),
        status,
        detail,
        fix: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_owner_can_run_or_read_assertions() {
        for role in [Role::Viewer, Role::Deployer] {
            assert!(owner(&Actor {
                id: "test".into(),
                role
            })
            .is_err());
        }
        assert!(owner(&Actor::admin()).is_ok());
    }
}

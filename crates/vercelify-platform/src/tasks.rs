//! Scheduled tasks: a command run in the app's container on a cron schedule
//! (UTC). The agent's minute loop runs the
//! due ones (the schedule is checked with the agent's cron parser).

use crate::ids::new_id;
use crate::{now, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use vercelify_core::platform::{ScheduledTask, ScheduledTaskRequest};

pub const MAX_PER_APP: i64 = 20;

fn row(r: &Row) -> rusqlite::Result<ScheduledTask> {
    Ok(ScheduledTask {
        id: r.get("id")?,
        app_id: r.get("app_id")?,
        name: r.get("name")?,
        schedule: r.get("schedule")?,
        command: r.get("command")?,
        enabled: r.get("enabled")?,
        last_run_at: r.get("last_run_at")?,
        last_status: r.get("last_status")?,
        created_at: r.get("created_at")?,
    })
}

pub fn list(c: &Connection, app_id: Option<&str>) -> Result<Vec<ScheduledTask>> {
    let mut st = c.prepare("SELECT * FROM scheduled_tasks WHERE ?1 IS NULL OR app_id = ?1 ORDER BY app_id, name")?;
    let rows = st.query_map([app_id], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(c: &Connection, app_id: &str, id: &str) -> Result<ScheduledTask> {
    c.query_row(
        "SELECT * FROM scheduled_tasks WHERE app_id = ?1 AND (id = ?2 OR name = ?2)",
        params![app_id, id],
        row,
    )
    .optional()?
    .ok_or_else(|| Error::not_found(format!("task {id}")))
}

/// `valid_schedule` is the agent's cron parser (kept out of this crate).
pub fn create(
    c: &Connection,
    app_id: &str,
    req: &ScheduledTaskRequest,
    valid_schedule: impl Fn(&str) -> std::result::Result<(), String>,
) -> Result<ScheduledTask> {
    let name = req.name.trim();
    if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
        return Err(Error::Invalid("a task name is 1–64 characters".into()));
    }
    valid_schedule(req.schedule.trim()).map_err(|e| Error::Invalid(format!("schedule: {e}")))?;
    let cmd = req.command.trim();
    if cmd.is_empty() || cmd.len() > 4000 || cmd.contains('\0') {
        return Err(Error::Invalid("the command must be 1–4000 characters".into()));
    }
    let n: i64 = c.query_row(
        "SELECT count(*) FROM scheduled_tasks WHERE app_id = ?1",
        [app_id],
        |r| r.get(0),
    )?;
    if n >= MAX_PER_APP {
        return Err(Error::Invalid(format!("at most {MAX_PER_APP} tasks per app")));
    }
    let id = new_id("task");
    c.execute(
        "INSERT INTO scheduled_tasks (id, app_id, name, schedule, command, enabled, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, app_id, name, req.schedule.trim(), cmd, req.enabled, now()],
    )
    .map_err(|e| match Error::from(e) {
        Error::Conflict(_) => Error::Conflict(format!("this app already has a task named {name}")),
        e => e,
    })?;
    get(c, app_id, &id)
}

pub fn delete(c: &Connection, app_id: &str, id: &str) -> Result<()> {
    let t = get(c, app_id, id)?;
    c.execute("DELETE FROM scheduled_tasks WHERE id = ?1", [&t.id])?;
    Ok(())
}

pub fn record_run(c: &Connection, id: &str, status: &str) -> Result<()> {
    c.execute(
        "UPDATE scheduled_tasks SET last_run_at = ?2, last_status = ?3 WHERE id = ?1",
        params![id, now(), status.chars().take(300).collect::<String>()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{apps, Store};

    fn req(name: &str, schedule: &str) -> ScheduledTaskRequest {
        ScheduledTaskRequest {
            name: name.into(),
            schedule: schedule.into(),
            command: "php artisan schedule:run".into(),
            enabled: true,
        }
    }

    #[test]
    fn tasks() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| apps::create(c, &apps::tests::git_app("blog"))).unwrap();
        let ok = |_: &str| Ok(());
        let t = s.tx(|c| create(c, &a.id, &req("cleanup", "*/5 * * * *"), ok)).unwrap();
        assert!(t.enabled && t.last_run_at.is_none());
        assert!(matches!(
            s.tx(|c| create(c, &a.id, &req("cleanup", "* * * * *"), ok)),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            s.tx(|c| create(c, &a.id, &req("bad", "nope"), |_| Err("not cron".into()))),
            Err(Error::Invalid(_))
        ));
        s.with(|c| record_run(c, &t.id, "exit 0")).unwrap();
        assert_eq!(s.with(|c| get(c, &a.id, "cleanup")).unwrap().last_status, "exit 0");
        assert_eq!(s.with(|c| list(c, None)).unwrap().len(), 1);
        s.tx(|c| delete(c, &a.id, "cleanup")).unwrap();
        assert!(s.with(|c| list(c, Some(&a.id))).unwrap().is_empty());
    }
}

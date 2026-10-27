//! Auto-deploy bookkeeping per app (design §11): the branch head last seen,
//! when it was checked, and why a push wasn't built.

use crate::{now, Result};
use rusqlite::{params, Connection, OptionalExtension};
use vercelify_core::platform::GitWatchStatus;

fn ensure(c: &Connection, app_id: &str) -> Result<()> {
    c.execute("INSERT OR IGNORE INTO git_watch (app_id) VALUES (?1)", [app_id])?;
    Ok(())
}

pub fn get(c: &Connection, app_id: &str) -> Result<GitWatchStatus> {
    ensure(c, app_id)?;
    Ok(c.query_row("SELECT * FROM git_watch WHERE app_id = ?1", [app_id], |r| {
        Ok(GitWatchStatus {
            app_id: r.get("app_id")?,
            enabled: r.get("enabled")?,
            last_head: r.get("last_head")?,
            last_checked_at: r.get("last_checked_at")?,
            last_error: r.get("last_error")?,
            skipped: r.get("skipped")?,
            last_skip: r.get("last_skip")?,
            waiting: r.get("waiting")?,
        })
    })?)
}

pub fn set_enabled(c: &Connection, app_id: &str, enabled: bool) -> Result<GitWatchStatus> {
    ensure(c, app_id)?;
    c.execute(
        "UPDATE git_watch SET enabled = ?2 WHERE app_id = ?1",
        params![app_id, enabled],
    )?;
    get(c, app_id)
}

/// A check finished: the head it saw (None: couldn't tell) and its error.
pub fn checked(c: &Connection, app_id: &str, head: Option<&str>, error: Option<&str>) -> Result<()> {
    ensure(c, app_id)?;
    c.execute(
        "UPDATE git_watch SET last_checked_at = ?2, last_error = ?3, waiting = '',
           last_head = CASE WHEN ?4 IS NULL THEN last_head ELSE ?4 END
         WHERE app_id = ?1",
        params![app_id, now(), error.unwrap_or(""), head],
    )?;
    Ok(())
}

/// A check found a new commit that must wait, and why (not an error).
pub fn held(c: &Connection, app_id: &str, why: &str) -> Result<()> {
    ensure(c, app_id)?;
    c.execute(
        "UPDATE git_watch SET last_checked_at = ?2, last_error = '', waiting = ?3 WHERE app_id = ?1",
        params![app_id, now(), why],
    )?;
    Ok(())
}

pub fn skipped(c: &Connection, app_id: &str, why: &str) -> Result<()> {
    ensure(c, app_id)?;
    c.execute(
        "UPDATE git_watch SET skipped = skipped + 1, last_skip = ?2 WHERE app_id = ?1",
        params![app_id, why],
    )?;
    Ok(())
}

pub fn comment_id(c: &Connection, app_id: &str, pr: u32) -> Result<Option<u64>> {
    Ok(c.query_row(
        "SELECT comment_id FROM previews WHERE app_id = ?1 AND pr_number = ?2",
        params![app_id, pr],
        |r| r.get::<_, Option<i64>>(0),
    )
    .optional()?
    .flatten()
    .map(|v| v as u64))
}

pub fn set_comment_id(c: &Connection, app_id: &str, pr: u32, id: u64) -> Result<()> {
    c.execute(
        "UPDATE previews SET comment_id = ?3 WHERE app_id = ?1 AND pr_number = ?2",
        params![app_id, pr, id as i64],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{apps, previews, Store};

    #[test]
    fn bookkeeping() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| apps::create(c, &apps::tests::git_app("blog"))).unwrap();
        let w = s.with(|c| get(c, &a.id)).unwrap();
        assert!(w.enabled && w.last_head.is_empty());
        s.with(|c| checked(c, &a.id, Some("abc1234"), None)).unwrap();
        s.with(|c| checked(c, &a.id, None, Some("network down"))).unwrap();
        let w = s.with(|c| get(c, &a.id)).unwrap();
        assert_eq!(
            (w.last_head.as_str(), w.last_error.as_str()),
            ("abc1234", "network down")
        );
        // A held commit is no error, and the next check clears it.
        s.with(|c| held(c, &a.id, "new commit waiting: the power guard says hot"))
            .unwrap();
        let w = s.with(|c| get(c, &a.id)).unwrap();
        assert_eq!(
            (w.last_error.as_str(), w.waiting.as_str(), w.last_head.as_str()),
            ("", "new commit waiting: the power guard says hot", "abc1234")
        );
        s.with(|c| checked(c, &a.id, Some("def5678"), None)).unwrap();
        assert_eq!(s.with(|c| get(c, &a.id)).unwrap().waiting, "");
        s.with(|c| skipped(c, &a.id, "[skip ci]")).unwrap();
        assert_eq!(s.with(|c| get(c, &a.id)).unwrap().skipped, 1);
        assert!(!s.with(|c| set_enabled(c, &a.id, false)).unwrap().enabled);
        s.tx(|c| previews::upsert(c, &a.id, 3, "abc1234", "x")).unwrap();
        assert_eq!(s.with(|c| comment_id(c, &a.id, 3)).unwrap(), None);
        s.with(|c| set_comment_id(c, &a.id, 3, 991)).unwrap();
        assert_eq!(s.with(|c| comment_id(c, &a.id, 3)).unwrap(), Some(991));
        s.tx(|c| apps::delete(c, &a.id)).unwrap();
    }
}

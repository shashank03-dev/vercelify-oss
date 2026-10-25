//! Preview deployments per pull request (design §11).

use crate::ids::new_id;
use crate::{now, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Preview {
    pub id: String,
    pub app_id: String,
    pub pr_number: u32,
    pub head_sha: String,
    pub title: String,
    pub url: String,
    pub deployment_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub closed_at: Option<String>,
}

fn row(r: &Row) -> rusqlite::Result<Preview> {
    Ok(Preview {
        id: r.get("id")?,
        app_id: r.get("app_id")?,
        pr_number: r.get("pr_number")?,
        head_sha: r.get("head_sha")?,
        title: r.get("title")?,
        url: r.get("url")?,
        deployment_id: r.get("deployment_id")?,
        status: r.get("status")?,
        created_at: r.get("created_at")?,
        closed_at: r.get("closed_at")?,
    })
}

pub fn list(c: &Connection, app_id: &str, active_only: bool) -> Result<Vec<Preview>> {
    let mut st = c.prepare(
        "SELECT * FROM previews WHERE app_id = ?1 AND (?2 = 0 OR status = 'active') ORDER BY pr_number DESC",
    )?;
    let rows = st.query_map(params![app_id, active_only], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(c: &Connection, app_id: &str, pr: u32) -> Result<Option<Preview>> {
    Ok(c.query_row(
        "SELECT * FROM previews WHERE app_id = ?1 AND pr_number = ?2",
        params![app_id, pr],
        row,
    )
    .optional()?)
}

/// Record (or reopen) a preview for a pull request.
pub fn upsert(c: &Connection, app_id: &str, pr: u32, head_sha: &str, title: &str) -> Result<Preview> {
    c.execute(
        "INSERT INTO previews (id, app_id, pr_number, head_sha, title, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (app_id, pr_number) DO UPDATE SET head_sha = excluded.head_sha,
           title = CASE WHEN excluded.title = '' THEN previews.title ELSE excluded.title END,
           status = 'active', closed_at = NULL",
        params![
            new_id("prv"),
            app_id,
            pr,
            head_sha,
            title.chars().take(200).collect::<String>(),
            now()
        ],
    )?;
    get(c, app_id, pr)?.ok_or_else(|| Error::Internal("preview vanished".into()))
}

/// The deployment now serving a preview, and its URL.
pub fn set_serving(c: &Connection, app_id: &str, pr: u32, deployment_id: &str, url: &str) -> Result<()> {
    c.execute(
        "UPDATE previews SET deployment_id = ?3, url = ?4 WHERE app_id = ?1 AND pr_number = ?2",
        params![app_id, pr, deployment_id, url],
    )?;
    Ok(())
}

pub fn close(c: &Connection, app_id: &str, pr: u32) -> Result<Option<Preview>> {
    let p = get(c, app_id, pr)?;
    c.execute(
        "UPDATE previews SET status = 'closed', closed_at = ?3 WHERE app_id = ?1 AND pr_number = ?2 AND status = 'active'",
        params![app_id, pr, now()],
    )?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{apps, Store};

    #[test]
    fn lifecycle() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| apps::create(c, &apps::tests::git_app("blog"))).unwrap();
        let p = s.tx(|c| upsert(c, &a.id, 7, "abc1234", "Fix")).unwrap();
        assert_eq!(p.status, "active");
        s.tx(|c| upsert(c, &a.id, 7, "def5678", "")).unwrap();
        let p = s.with(|c| get(c, &a.id, 7)).unwrap().unwrap();
        assert_eq!((p.head_sha.as_str(), p.title.as_str()), ("def5678", "Fix"));
        s.tx(|c| close(c, &a.id, 7)).unwrap();
        assert!(s.with(|c| list(c, &a.id, true)).unwrap().is_empty());
        assert_eq!(s.with(|c| list(c, &a.id, false)).unwrap().len(), 1);
        s.tx(|c| upsert(c, &a.id, 7, "fff0000", "")).unwrap();
        assert_eq!(s.with(|c| list(c, &a.id, true)).unwrap().len(), 1, "reopened");
    }
}

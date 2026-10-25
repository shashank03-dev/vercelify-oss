//! Projects group apps and databases; environments (`production`, `staging`…)
//! hold their variables. Every project starts with `production`.

use crate::ids::new_id;
use crate::{now, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use vercelify_core::platform::{Environment, Project, ProjectRequest};

pub const DEFAULT_PROJECT: &str = "Default";
pub const PRODUCTION: &str = "production";

fn project_name(n: &str) -> Result<String> {
    let n = n.trim();
    if n.is_empty() || n.chars().count() > 80 || n.chars().any(char::is_control) {
        return Err(Error::Invalid("a project name is 1–80 characters".into()));
    }
    Ok(n.to_string())
}

fn environment_name(n: &str) -> Result<String> {
    let n = n.trim();
    if crate::ids::valid_name(n) {
        Ok(n.to_string())
    } else {
        Err(Error::Invalid(format!(
            "`{n}` can't be an environment name (lowercase letters, digits and -, like staging)"
        )))
    }
}

fn env_row(r: &Row) -> rusqlite::Result<Environment> {
    Ok(Environment {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        name: r.get("name")?,
        created_at: r.get("created_at")?,
    })
}

pub fn environments(c: &Connection, project_id: &str) -> Result<Vec<Environment>> {
    let mut st = c.prepare(
        "SELECT * FROM environments WHERE project_id = ?1
         ORDER BY CASE name WHEN 'production' THEN 0 ELSE 1 END, name",
    )?;
    let rows = st.query_map([project_id], env_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn environment(c: &Connection, id: &str) -> Result<Environment> {
    c.query_row("SELECT * FROM environments WHERE id = ?1", [id], env_row)
        .optional()?
        .ok_or_else(|| Error::not_found(format!("environment {id}")))
}

fn project_row(c: &Connection, r: &Row) -> Result<Project> {
    let id: String = r.get("id")?;
    Ok(Project {
        environments: environments(c, &id)?,
        app_count: r.get("app_count")?,
        database_count: r.get("database_count")?,
        name: r.get("name")?,
        description: r.get("description")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        id,
    })
}

const PROJECT_SELECT: &str = "SELECT p.*,
    (SELECT count(*) FROM apps a JOIN environments e ON a.environment_id = e.id WHERE e.project_id = p.id) AS app_count,
    (SELECT count(*) FROM databases d JOIN environments e ON d.environment_id = e.id WHERE e.project_id = p.id) AS database_count
  FROM projects p";

pub fn list(c: &Connection) -> Result<Vec<Project>> {
    let mut st = c.prepare(&format!("{PROJECT_SELECT} ORDER BY p.name COLLATE NOCASE"))?;
    let mut rows = st.query([])?;
    let mut out = vec![];
    while let Some(r) = rows.next()? {
        out.push(project_row(c, r)?);
    }
    Ok(out)
}

pub fn get(c: &Connection, id: &str) -> Result<Project> {
    let mut st = c.prepare(&format!("{PROJECT_SELECT} WHERE p.id = ?1"))?;
    let mut rows = st.query([id])?;
    match rows.next()? {
        Some(r) => project_row(c, r),
        None => Err(Error::not_found(format!("project {id}"))),
    }
}

pub fn create(c: &Connection, req: &ProjectRequest) -> Result<Project> {
    let name = project_name(req.name.as_deref().unwrap_or(""))?;
    let id = new_id("prj");
    let t = now();
    c.execute(
        "INSERT INTO projects (id, name, description, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
        params![id, name, req.description.as_deref().unwrap_or("").trim(), t],
    )
    .map_err(|e| match Error::from(e) {
        Error::Conflict(_) => Error::Conflict(format!("a project named {name} already exists")),
        e => e,
    })?;
    add_environment(c, &id, PRODUCTION)?;
    get(c, &id)
}

pub fn update(c: &Connection, id: &str, req: &ProjectRequest) -> Result<Project> {
    get(c, id)?;
    if let Some(n) = &req.name {
        let n = project_name(n)?;
        c.execute(
            "UPDATE projects SET name = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, n, now()],
        )?;
    }
    if let Some(d) = &req.description {
        c.execute(
            "UPDATE projects SET description = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, d.trim(), now()],
        )?;
    }
    get(c, id)
}

/// Refused while the project still has apps or databases.
pub fn delete(c: &Connection, id: &str) -> Result<()> {
    let p = get(c, id)?;
    if p.app_count > 0 || p.database_count > 0 {
        return Err(Error::Conflict(format!(
            "{} still has {} app(s) and {} database(s); delete them first",
            p.name, p.app_count, p.database_count
        )));
    }
    c.execute(
        "DELETE FROM env_vars WHERE (scope = 'project' AND scope_id = ?1)
            OR (scope = 'environment' AND scope_id IN (SELECT id FROM environments WHERE project_id = ?1))",
        [id],
    )?;
    c.execute("DELETE FROM projects WHERE id = ?1", [id])?;
    Ok(())
}

pub fn add_environment(c: &Connection, project_id: &str, name: &str) -> Result<Environment> {
    let name = environment_name(name)?;
    let id = new_id("env");
    c.execute(
        "INSERT INTO environments (id, project_id, name, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, project_id, name, now()],
    )
    .map_err(|e| match Error::from(e) {
        Error::Conflict(m) if m.contains("already exists") => {
            Error::Conflict(format!("this project already has an environment {name}"))
        }
        Error::Conflict(_) => Error::not_found(format!("project {project_id}")),
        e => e,
    })?;
    environment(c, &id)
}

pub fn delete_environment(c: &Connection, id: &str) -> Result<()> {
    let env = environment(c, id)?;
    if env.name == PRODUCTION {
        return Err(Error::Conflict("the production environment can't be deleted".into()));
    }
    let used: i64 = c.query_row(
        "SELECT (SELECT count(*) FROM apps WHERE environment_id = ?1) + (SELECT count(*) FROM databases WHERE environment_id = ?1)",
        [id],
        |r| r.get(0),
    )?;
    if used > 0 {
        return Err(Error::Conflict(format!("{} still has apps or databases", env.name)));
    }
    c.execute(
        "DELETE FROM env_vars WHERE scope = 'environment' AND scope_id = ?1",
        [id],
    )?;
    c.execute("DELETE FROM environments WHERE id = ?1", [id])?;
    Ok(())
}

/// The environment a new app goes into: the given one, the project's
/// production environment, or the Default project's (created on first use).
pub fn resolve_target(c: &Connection, environment_id: Option<&str>, project_id: Option<&str>) -> Result<Environment> {
    if let Some(e) = environment_id.filter(|s| !s.is_empty()) {
        return environment(c, e);
    }
    let project_id = match project_id.filter(|s| !s.is_empty()) {
        Some(p) => get(c, p)?.id,
        None => {
            let existing: Option<String> = c
                .query_row("SELECT id FROM projects WHERE name = ?1", [DEFAULT_PROJECT], |r| {
                    r.get(0)
                })
                .optional()?;
            match existing {
                Some(id) => id,
                None => {
                    create(
                        c,
                        &ProjectRequest {
                            name: Some(DEFAULT_PROJECT.into()),
                            description: None,
                        },
                    )?
                    .id
                }
            }
        }
    };
    c.query_row(
        "SELECT * FROM environments WHERE project_id = ?1 AND name = ?2",
        params![project_id, PRODUCTION],
        env_row,
    )
    .optional()?
    .ok_or_else(|| Error::Conflict("this project has no production environment".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn req(name: &str) -> ProjectRequest {
        ProjectRequest {
            name: Some(name.into()),
            description: None,
        }
    }

    #[test]
    fn projects_start_with_production() {
        let s = Store::open_in_memory().unwrap();
        let p = s.tx(|c| create(c, &req(" Blog "))).unwrap();
        assert_eq!(p.name, "Blog");
        assert_eq!(p.environments.len(), 1);
        assert_eq!(p.environments[0].name, PRODUCTION);
        let staging = s.tx(|c| add_environment(c, &p.id, "staging")).unwrap();
        let p = s.with(|c| get(c, &p.id)).unwrap();
        assert_eq!(
            p.environments.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["production", "staging"]
        );
        assert!(matches!(
            s.tx(|c| add_environment(c, &p.id, "staging")),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            s.tx(|c| add_environment(c, &p.id, "Staging!")),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            s.tx(|c| add_environment(c, "prj_nope", "x")),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            s.tx(|c| delete_environment(c, &p.environments[0].id)),
            Err(Error::Conflict(_))
        ));
        s.tx(|c| delete_environment(c, &staging.id)).unwrap();
    }

    #[test]
    fn names_and_updates() {
        let s = Store::open_in_memory().unwrap();
        let p = s.tx(|c| create(c, &req("Blog"))).unwrap();
        assert!(matches!(s.tx(|c| create(c, &req("Blog"))), Err(Error::Conflict(m)) if m.contains("Blog")));
        assert!(matches!(s.tx(|c| create(c, &req("  "))), Err(Error::Invalid(_))));
        let p2 = s
            .tx(|c| {
                update(
                    c,
                    &p.id,
                    &ProjectRequest {
                        name: Some("Site".into()),
                        description: Some("my site".into()),
                    },
                )
            })
            .unwrap();
        assert_eq!((p2.name.as_str(), p2.description.as_str()), ("Site", "my site"));
        assert_eq!(s.with(list).unwrap().len(), 1);
        s.tx(|c| delete(c, &p.id)).unwrap();
        assert!(matches!(s.with(|c| get(c, &p.id)), Err(Error::NotFound(_))));
    }

    #[test]
    fn default_target() {
        let s = Store::open_in_memory().unwrap();
        let e = s.tx(|c| resolve_target(c, None, None)).unwrap();
        assert_eq!(e.name, PRODUCTION);
        let again = s.tx(|c| resolve_target(c, None, None)).unwrap();
        assert_eq!(e.id, again.id, "Default is created once");
        let p = s.tx(|c| create(c, &req("Blog"))).unwrap();
        let e2 = s.tx(|c| resolve_target(c, None, Some(&p.id))).unwrap();
        assert_eq!(e2.project_id, p.id);
        assert!(matches!(
            s.tx(|c| resolve_target(c, Some("env_missing"), None)),
            Err(Error::NotFound(_))
        ));
    }
}

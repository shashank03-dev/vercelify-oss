//! Environment variables at three levels: project, environment, app. Values
//! are sealed (design §5); a deployment sees the merge, app over environment
//! over project, with preview-only values winning in previews.

use crate::ids::new_id;
use crate::secrets::{aad, MasterKey};
use crate::{now, validate, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;
use vercelify_core::platform::{EnvVar, EnvVarsPut, VarScope};
use zeroize::Zeroizing;

pub const MAX_VARS_PER_SCOPE: usize = 500;

fn seal_aad(id: &str) -> String {
    aad("env_vars", "value", id)
}

fn scope_exists(c: &Connection, scope: VarScope, id: &str) -> Result<()> {
    let table = match scope {
        VarScope::Project => "projects",
        VarScope::Environment => "environments",
        VarScope::App => "apps",
    };
    let found: Option<i64> = c
        .query_row(&format!("SELECT 1 FROM {table} WHERE id = ?1"), [id], |r| r.get(0))
        .optional()?;
    found
        .map(|_| ())
        .ok_or_else(|| Error::not_found(format!("{} {id}", scope.as_str())))
}

/// Variables of one scope. Secret values are None unless `reveal`.
pub fn list(c: &Connection, key: &MasterKey, scope: VarScope, scope_id: &str, reveal: bool) -> Result<Vec<EnvVar>> {
    scope_exists(c, scope, scope_id)?;
    let mut st = c.prepare(
        "SELECT id, key, value, build, preview_only, secret, updated_at FROM env_vars
         WHERE scope = ?1 AND scope_id = ?2 ORDER BY key, preview_only",
    )?;
    let mut rows = st.query(params![scope.as_str(), scope_id])?;
    let mut out = vec![];
    while let Some(r) = rows.next()? {
        let id: String = r.get(0)?;
        let secret: bool = r.get(5)?;
        let value = if secret && !reveal {
            None
        } else {
            let sealed: Vec<u8> = r.get(2)?;
            Some(key.open_string(&seal_aad(&id), &sealed)?.to_string())
        };
        out.push(EnvVar {
            key: r.get(1)?,
            value,
            build: r.get(3)?,
            preview_only: r.get(4)?,
            secret,
            scope,
            updated_at: r.get(6)?,
            id,
        });
    }
    Ok(out)
}

/// Upsert; with `replace`, delete this scope's other variables (per
/// `preview_only` group present in the request, so importing a `.env` for
/// production doesn't wipe preview overrides).
pub fn put(c: &Connection, key: &MasterKey, scope: VarScope, scope_id: &str, req: &EnvVarsPut) -> Result<usize> {
    scope_exists(c, scope, scope_id)?;
    let mut seen = std::collections::HashSet::new();
    for v in &req.vars {
        validate::env_key(&v.key)?;
        validate::env_value(&v.key, &v.value)?;
        if !seen.insert((v.key.clone(), v.preview_only.unwrap_or(false))) {
            return Err(Error::Invalid(format!("{} is listed twice", v.key)));
        }
    }
    let t = now();
    for v in &req.vars {
        let preview = v.preview_only.unwrap_or(false);
        let existing: Option<String> = c
            .query_row(
                "SELECT id FROM env_vars WHERE scope = ?1 AND scope_id = ?2 AND key = ?3 AND preview_only = ?4",
                params![scope.as_str(), scope_id, v.key, preview],
                |r| r.get(0),
            )
            .optional()?;
        let id = existing.clone().unwrap_or_else(|| new_id("var"));
        let sealed = key.seal_str(&seal_aad(&id), &v.value);
        if existing.is_some() {
            c.execute(
                "UPDATE env_vars SET value = ?2, build = coalesce(?3, build), secret = coalesce(?4, secret), updated_at = ?5
                 WHERE id = ?1",
                params![id, sealed, v.build, v.secret, t],
            )?;
        } else {
            c.execute(
                "INSERT INTO env_vars (id, scope, scope_id, key, value, build, preview_only, secret, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                params![
                    id,
                    scope.as_str(),
                    scope_id,
                    v.key,
                    sealed,
                    v.build.unwrap_or(false),
                    preview,
                    v.secret.unwrap_or(true),
                    t
                ],
            )?;
        }
    }
    if req.replace {
        let groups: std::collections::HashSet<bool> = if req.vars.is_empty() {
            [false].into()
        } else {
            req.vars.iter().map(|v| v.preview_only.unwrap_or(false)).collect()
        };
        let mut st = c.prepare("SELECT id, key, preview_only FROM env_vars WHERE scope = ?1 AND scope_id = ?2")?;
        let rows: Vec<(String, String, bool)> = st
            .query_map(params![scope.as_str(), scope_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (id, k, preview) in rows {
            if groups.contains(&preview) && !seen.contains(&(k, preview)) {
                c.execute("DELETE FROM env_vars WHERE id = ?1", [id])?;
            }
        }
    }
    let count: i64 = c.query_row(
        "SELECT count(*) FROM env_vars WHERE scope = ?1 AND scope_id = ?2",
        params![scope.as_str(), scope_id],
        |r| r.get(0),
    )?;
    if count as usize > MAX_VARS_PER_SCOPE {
        return Err(Error::Invalid(format!(
            "at most {MAX_VARS_PER_SCOPE} variables per scope"
        )));
    }
    Ok(req.vars.len())
}

pub fn delete(c: &Connection, scope: VarScope, scope_id: &str, key: &str, preview_only: bool) -> Result<()> {
    let n = c.execute(
        "DELETE FROM env_vars WHERE scope = ?1 AND scope_id = ?2 AND key = ?3 AND preview_only = ?4",
        params![scope.as_str(), scope_id, key, preview_only],
    )?;
    if n == 0 {
        return Err(Error::not_found(format!("variable {key}")));
    }
    Ok(())
}

/// One variable as a deployment sees it.
pub struct Resolved {
    pub value: Zeroizing<String>,
    pub build: bool,
}

/// The merged variables for an app: project < environment < app; in a
/// preview, preview-only values override at each level.
pub fn resolve(c: &Connection, key: &MasterKey, app_id: &str, preview: bool) -> Result<BTreeMap<String, Resolved>> {
    let (env_id, project_id): (String, String) = c
        .query_row(
            "SELECT a.environment_id, e.project_id FROM apps a JOIN environments e ON a.environment_id = e.id
             WHERE a.id = ?1",
            [app_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| Error::not_found(format!("app {app_id}")))?;
    let mut out = BTreeMap::new();
    let mut st = c.prepare(
        "SELECT id, key, value, build FROM env_vars
         WHERE scope = ?1 AND scope_id = ?2 AND (preview_only = 0 OR ?3)
         ORDER BY preview_only",
    )?;
    for (scope, id) in [
        ("project", &project_id),
        ("environment", &env_id),
        ("app", &app_id.to_string()),
    ] {
        let mut rows = st.query(params![scope, id, preview])?;
        while let Some(r) = rows.next()? {
            let vid: String = r.get(0)?;
            let sealed: Vec<u8> = r.get(2)?;
            out.insert(
                r.get::<_, String>(1)?,
                Resolved {
                    value: key.open_string(&seal_aad(&vid), &sealed)?,
                    build: r.get(3)?,
                },
            );
        }
    }
    Ok(out)
}

/// [`resolve`], with database references (`${{db.<name>.url}}`) filled in:
/// what a deployment actually gets.
pub fn resolve_for_deploy(
    c: &Connection,
    key: &MasterKey,
    app_id: &str,
    preview: bool,
) -> Result<BTreeMap<String, Resolved>> {
    let mut vars = resolve(c, key, app_id, preview)?;
    for (k, v) in vars.iter_mut() {
        if v.value.contains("${{db.") {
            let filled = crate::databases::resolve_references(c, key, &v.value)
                .map_err(|e| Error::Invalid(format!("{k}: {}", e.message())))?;
            v.value = Zeroizing::new(filled);
        }
    }
    Ok(vars)
}

/// Parse a `.env` file: `KEY=value`, `export KEY=value`, quotes, comments.
pub fn parse_dotenv(raw: &str) -> Result<Vec<(String, String)>> {
    let mut out = vec![];
    for (n, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| Error::Invalid(format!("line {}: expected KEY=value", n + 1)))?;
        let k = k.trim();
        validate::env_key(k).map_err(|e| Error::Invalid(format!("line {}: {e}", n + 1)))?;
        let v = v.trim();
        let v = if let Some(q) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            q.replace("\\n", "\n").replace("\\\"", "\"").replace("\\\\", "\\")
        } else if let Some(q) = v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
            q.to_string()
        } else {
            // Unquoted: a ` #` starts a comment.
            v.split(" #").next().unwrap_or("").trim().to_string()
        };
        out.push((k.to_string(), v));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps;
    use crate::Store;
    use vercelify_core::platform::EnvVarInput;

    fn key() -> MasterKey {
        MasterKey::from_bytes(&[9u8; 32]).unwrap()
    }

    fn input(k: &str, v: &str) -> EnvVarInput {
        EnvVarInput {
            key: k.into(),
            value: v.into(),
            build: None,
            preview_only: None,
            secret: None,
        }
    }

    fn setup() -> (Store, String, String, String) {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| apps::create(c, &apps::tests::git_app("blog"))).unwrap();
        (s, a.id, a.environment_id, a.project_id)
    }

    #[test]
    fn values_are_sealed_and_masked() {
        let (s, app, _, _) = setup();
        let k = key();
        s.tx(|c| {
            put(
                c,
                &k,
                VarScope::App,
                &app,
                &EnvVarsPut {
                    vars: vec![
                        input("DATABASE_URL", "postgres://x"),
                        EnvVarInput {
                            secret: Some(false),
                            ..input("NODE_ENV", "production")
                        },
                    ],
                    replace: false,
                },
            )
        })
        .unwrap();
        let raw: Vec<u8> = s
            .with(|c| {
                Ok(
                    c.query_row("SELECT value FROM env_vars WHERE key = 'DATABASE_URL'", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("postgres"), "stored sealed");
        let masked = s.with(|c| list(c, &k, VarScope::App, &app, false)).unwrap();
        assert_eq!(masked[0].key, "DATABASE_URL");
        assert_eq!(masked[0].value, None);
        assert_eq!(masked[1].value.as_deref(), Some("production"), "not secret: shown");
        let revealed = s.with(|c| list(c, &k, VarScope::App, &app, true)).unwrap();
        assert_eq!(revealed[0].value.as_deref(), Some("postgres://x"));
        // Another master key can't read them.
        let other = MasterKey::from_bytes(&[1u8; 32]).unwrap();
        assert!(s.with(|c| list(c, &other, VarScope::App, &app, true)).is_err());
    }

    #[test]
    fn replace_imports_a_dotenv() {
        let (s, app, _, _) = setup();
        let k = key();
        let put_vars =
            |vars: Vec<EnvVarInput>, replace| s.tx(|c| put(c, &k, VarScope::App, &app, &EnvVarsPut { vars, replace }));
        put_vars(
            vec![
                input("A", "1"),
                input("B", "2"),
                EnvVarInput {
                    preview_only: Some(true),
                    ..input("A", "preview")
                },
            ],
            false,
        )
        .unwrap();
        put_vars(vec![input("B", "3"), input("C", "4")], true).unwrap();
        let vars = s.with(|c| list(c, &k, VarScope::App, &app, true)).unwrap();
        let got: Vec<(String, String, bool)> = vars
            .into_iter()
            .map(|v| (v.key, v.value.unwrap(), v.preview_only))
            .collect();
        assert_eq!(
            got,
            [
                ("A".into(), "preview".into(), true),
                ("B".into(), "3".into(), false),
                ("C".into(), "4".into(), false)
            ],
            "A removed, B updated, C added, the preview override kept"
        );
        assert!(put_vars(vec![input("1BAD", "x")], false).is_err());
        assert!(put_vars(vec![input("A", "x"), input("A", "y")], false).is_err());
        s.tx(|c| delete(c, VarScope::App, &app, "C", false)).unwrap();
        assert!(matches!(
            s.tx(|c| delete(c, VarScope::App, &app, "C", false)),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            s.with(|c| list(c, &k, VarScope::App, "app_nope", false)),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn resolution_order() {
        let (s, app, env, project) = setup();
        let k = key();
        let set = |scope, id: &str, vars: Vec<EnvVarInput>| {
            s.tx(|c| put(c, &k, scope, id, &EnvVarsPut { vars, replace: false }))
                .unwrap();
        };
        set(
            VarScope::Project,
            &project,
            vec![input("A", "project"), input("B", "project")],
        );
        set(VarScope::Environment, &env, vec![input("B", "env"), input("C", "env")]);
        set(
            VarScope::App,
            &app,
            vec![
                input("C", "app"),
                EnvVarInput {
                    preview_only: Some(true),
                    ..input("A", "app-preview")
                },
                EnvVarInput {
                    build: Some(true),
                    ..input("D", "build")
                },
            ],
        );
        let prod = s.with(|c| resolve(c, &k, &app, false)).unwrap();
        let get = |m: &BTreeMap<String, Resolved>, key: &str| m[key].value.to_string();
        assert_eq!(get(&prod, "A"), "project");
        assert_eq!(get(&prod, "B"), "env");
        assert_eq!(get(&prod, "C"), "app");
        assert!(prod["D"].build);
        let preview = s.with(|c| resolve(c, &k, &app, true)).unwrap();
        assert_eq!(get(&preview, "A"), "app-preview");
        assert_eq!(get(&preview, "B"), "env");
    }

    #[test]
    fn dotenv() {
        let parsed =
            parse_dotenv("# comment\nA=1\nexport B=\"two words\\n\"\nC='$literal'\nD=x # trailing\nEMPTY=\n").unwrap();
        assert_eq!(
            parsed,
            [
                ("A".into(), "1".into()),
                ("B".into(), "two words\n".into()),
                ("C".into(), "$literal".into()),
                ("D".into(), "x".into()),
                ("EMPTY".into(), "".into())
            ]
        );
        assert!(parse_dotenv("NOEQUALS").is_err());
        assert!(parse_dotenv("1X=y").is_err());
    }
}

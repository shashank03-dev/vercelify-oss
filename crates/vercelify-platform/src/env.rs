//! Environment variables at three levels: project, environment, app. Values
//! are sealed (design §5); a deployment sees the merge, app over environment
//! over project. Previews (design §5.1) get preview-only values, which win,
//! and only the normal values shared with them on purpose: pull-request code
//! never reads a production value by default, and a shared value that points
//! at a production database stops a preview instead of being used.

use crate::ids::new_id;
use crate::secrets::{aad, MasterKey};
use crate::{now, validate, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;
use vercelify_core::platform::{EffectiveVar, EnvProvenance, EnvVar, EnvVarsPut, VarOrigin, VarScope, WithheldVar};
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
        "SELECT id, key, value, build, preview_only, secret, updated_at, share_with_previews FROM env_vars
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
            share_with_previews: r.get(7)?,
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
        // Sharing is a property of a normal value; a preview-only one is the
        // preview's already.
        let share = if preview { Some(false) } else { v.share_with_previews };
        if existing.is_some() {
            c.execute(
                "UPDATE env_vars SET value = ?2, build = coalesce(?3, build), secret = coalesce(?4, secret),
                 share_with_previews = coalesce(?6, share_with_previews), updated_at = ?5
                 WHERE id = ?1",
                params![id, sealed, v.build, v.secret, t, share],
            )?;
        } else {
            c.execute(
                "INSERT INTO env_vars (id, scope, scope_id, key, value, build, preview_only, secret, share_with_previews,
                 created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                params![
                    id,
                    scope.as_str(),
                    scope_id,
                    v.key,
                    sealed,
                    v.build.unwrap_or(false),
                    preview,
                    v.secret.unwrap_or(true),
                    share.unwrap_or(false),
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

/// Share a normal value with previews, or stop sharing it, without
/// touching its value (a hidden secret needn't be sent again).
pub fn set_sharing(c: &Connection, scope: VarScope, scope_id: &str, key: &str, share: bool) -> Result<()> {
    let n = c.execute(
        "UPDATE env_vars SET share_with_previews = ?4, updated_at = ?5
         WHERE scope = ?1 AND scope_id = ?2 AND key = ?3 AND preview_only = 0",
        params![scope.as_str(), scope_id, key, share, now()],
    )?;
    if n == 0 {
        return Err(Error::not_found(format!("variable {key}")));
    }
    Ok(())
}

/// The app's environment and project ids.
fn parents(c: &Connection, app_id: &str) -> Result<(String, String)> {
    c.query_row(
        "SELECT a.environment_id, e.project_id FROM apps a JOIN environments e ON a.environment_id = e.id
         WHERE a.id = ?1",
        [app_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| Error::not_found(format!("app {app_id}")))
}

/// The variables a production deployment of the app gets, merged the way
/// [`resolve`] merges them (project < environment < app, preview-only
/// values left out), each with the scope it came from. Database references
/// stay as written: they name the database, not a password. Secret values
/// are None unless `reveal`.
pub fn effective(c: &Connection, key: &MasterKey, app_id: &str, reveal: bool) -> Result<Vec<EnvVar>> {
    let (env_id, project_id) = parents(c, app_id)?;
    let mut out = BTreeMap::new();
    for (scope, id) in [
        (VarScope::Project, project_id.as_str()),
        (VarScope::Environment, env_id.as_str()),
        (VarScope::App, app_id),
    ] {
        for v in list(c, key, scope, id, reveal)? {
            if !v.preview_only {
                out.insert(v.key.clone(), v);
            }
        }
    }
    Ok(out.into_values().collect())
}

/// One variable as a deployment sees it.
pub struct Resolved {
    pub value: Zeroizing<String>,
    pub build: bool,
    /// Hidden in logs (`logs::Redactor`).
    pub secret: bool,
    pub scope: VarScope,
    pub origin: VarOrigin,
}

/// What a deployment gets, and (in a preview) what it was denied.
#[derive(Default)]
pub struct Resolution {
    pub vars: BTreeMap<String, Resolved>,
    /// Normal values a preview goes without (not shared), by key.
    pub withheld: BTreeMap<String, VarScope>,
}

/// The merged variables for an app: project < environment < app. A preview
/// gets the normal values shared with previews, then preview-only values
/// (again project < environment < app) on top, so any preview-only value
/// wins; every other normal value is withheld.
pub fn resolve_all(c: &Connection, key: &MasterKey, app_id: &str, preview: bool) -> Result<Resolution> {
    let (env_id, project_id) = parents(c, app_id)?;
    let mut out = Resolution::default();
    let mut st = c.prepare(
        "SELECT id, key, value, build, secret, share_with_previews FROM env_vars
         WHERE scope = ?1 AND scope_id = ?2 AND preview_only = ?3",
    )?;
    let levels = [
        (VarScope::Project, project_id.as_str()),
        (VarScope::Environment, env_id.as_str()),
        (VarScope::App, app_id),
    ];
    let groups: &[bool] = if preview { &[false, true] } else { &[false] };
    for &preview_only in groups {
        for (scope, id) in levels {
            let mut rows = st.query(params![scope.as_str(), id, preview_only])?;
            while let Some(r) = rows.next()? {
                let k: String = r.get(1)?;
                let shared: bool = r.get(5)?;
                let origin = match (preview_only, shared) {
                    (true, _) => VarOrigin::PreviewOnly,
                    (false, true) => VarOrigin::Shared,
                    (false, false) => VarOrigin::Production,
                };
                if preview && origin == VarOrigin::Production {
                    // A more specific level that isn't shared hides a less
                    // specific shared one too: production's value is the
                    // app's, and that one is production-only.
                    out.vars.remove(&k);
                    out.withheld.insert(k, scope);
                    continue;
                }
                let vid: String = r.get(0)?;
                let sealed: Vec<u8> = r.get(2)?;
                out.withheld.remove(&k);
                out.vars.insert(
                    k,
                    Resolved {
                        value: key.open_string(&seal_aad(&vid), &sealed)?,
                        build: r.get(3)?,
                        secret: r.get(4)?,
                        scope,
                        origin,
                    },
                );
            }
        }
    }
    Ok(out)
}

/// [`resolve_all`]'s variables.
pub fn resolve(c: &Connection, key: &MasterKey, app_id: &str, preview: bool) -> Result<BTreeMap<String, Resolved>> {
    Ok(resolve_all(c, key, app_id, preview)?.vars)
}

fn database_reference(value: &str) -> bool {
    value.contains("${{db.")
}

/// A preview may not use a production database through a shared value
/// (design §5.1): checked before any reference is filled in, for build and
/// runtime alike. Platform DB references currently name production services;
/// preview-only scope is not permission to resolve those credentials.
fn check_preview(vars: &BTreeMap<String, Resolved>) -> Result<()> {
    let blocked: Vec<&str> = vars
        .iter()
        .filter(|(_, v)| database_reference(&v.value))
        .map(|(k, _)| k.as_str())
        .collect();
    if blocked.is_empty() {
        return Ok(());
    }
    Err(Error::Invalid(format!(
        "{} {} a production database, which previews can't use: give {} a preview-only value \
         value for an isolated database (not a platform production DB reference), or remove {}",
        blocked.join(", "),
        if blocked.len() == 1 { "points at" } else { "point at" },
        if blocked.len() == 1 { "it" } else { "each" },
        if blocked.len() == 1 { "it" } else { "them" },
    )))
}

/// [`resolve_all`], with database references (`${{db.<name>.url}}`) filled
/// in: what a deployment actually gets.
///
/// `main_ip`: the main server's mesh address, for an app on another server
/// (the databases' public ports are used then).
pub fn resolve_for_deploy(
    c: &Connection,
    key: &MasterKey,
    app_id: &str,
    preview: bool,
    main_ip: Option<&str>,
) -> Result<Resolution> {
    let mut res = resolve_all(c, key, app_id, preview)?;
    if preview {
        if let Some(db) = crate::preview_db::settings(c, app_id)? {
            // Explicit opt-in replaces only this runtime binding. No production
            // URL or fresh PR credential enters the build environment.
            res.vars.remove(&db.variable);
            res.withheld.remove(&db.variable);
        }
        check_preview(&res.vars)?;
    }
    for (k, v) in res.vars.iter_mut() {
        validate::env_key(k)?;
        if database_reference(&v.value) {
            let filled = crate::databases::resolve_references_at(c, key, &v.value, main_ip)
                .map_err(|e| Error::Invalid(format!("{k}: {}", e.message())))?;
            v.value = Zeroizing::new(filled);
            // A filled reference carries a password.
            v.secret = true;
        }
    }
    Ok(res)
}

/// Where each variable of production and of previews comes from, and what
/// previews go without. Never values.
pub fn provenance(c: &Connection, key: &MasterKey, app_id: &str) -> Result<EnvProvenance> {
    let effective = |res: &Resolution| -> Vec<EffectiveVar> {
        res.vars
            .iter()
            .map(|(k, v)| EffectiveVar {
                key: k.clone(),
                scope: v.scope,
                origin: v.origin,
                build: v.build,
                secret: v.secret,
                database_reference: database_reference(&v.value),
            })
            .collect()
    };
    let prod = resolve_all(c, key, app_id, false)?;
    let preview = resolve_all(c, key, app_id, true)?;
    let mut withheld: Vec<WithheldVar> = preview
        .withheld
        .iter()
        .map(|(k, scope)| WithheldVar {
            key: k.clone(),
            scope: *scope,
            reason: "production_only".into(),
        })
        .collect();
    let mut preview_vars = effective(&preview);
    if let Some(db) = crate::preview_db::settings(c, app_id)? {
        preview_vars.retain(|v| v.key != db.variable);
        withheld.retain(|v| v.key != db.variable);
        preview_vars.push(EffectiveVar {
            key: db.variable,
            scope: VarScope::App,
            origin: VarOrigin::PreviewDatabase,
            build: false,
            secret: true,
            database_reference: false,
        });
    }
    preview_vars.retain(|v| {
        let blocked = v.database_reference;
        if blocked {
            withheld.push(WithheldVar {
                key: v.key.clone(),
                scope: v.scope,
                reason: "production_database".into(),
            });
        }
        !blocked
    });
    withheld.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(EnvProvenance {
        production: effective(&prod),
        preview: preview_vars,
        withheld,
    })
}

/// Parse a `.env` file: `KEY=value`, `export KEY=value`, quotes, comments.
pub fn parse_dotenv(raw: &str) -> Result<Vec<(String, String)>> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let lines: Vec<&str> = raw.lines().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < lines.len() {
        let n = i + 1;
        let line = lines[i].trim();
        i += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |what: &str| Error::Invalid(format!("line {n}: {what}"));
        let line = line
            .strip_prefix("export")
            .filter(|l| l.starts_with([' ', '\t']))
            .unwrap_or(line);
        let (k, v) = line.split_once('=').ok_or_else(|| bad("expected KEY=value"))?;
        let k = k.trim();
        validate::env_key(k).map_err(|e| bad(&e.to_string()))?;
        let v = v.trim_start();
        let value = match v.chars().next() {
            Some(q @ ('"' | '\'' | '`')) => {
                // A quoted value may span lines (PEM keys) and be followed
                // by a comment.
                let mut body = v[1..].to_string();
                let close = loop {
                    if let Some(at) = closing_quote(&body, q) {
                        break at;
                    }
                    let next = lines
                        .get(i)
                        .ok_or_else(|| bad(&format!("the {q} quote is never closed")))?;
                    body.push('\n');
                    body.push_str(next);
                    i += 1;
                };
                let rest = body[close + 1..].trim();
                if !rest.is_empty() && !rest.starts_with('#') {
                    return Err(bad(&format!("unexpected `{rest}` after the closing quote")));
                }
                let inner = &body[..close];
                if q == '"' {
                    unescape(inner)
                } else {
                    inner.to_string()
                }
            }
            // Unquoted: a whitespace-preceded `#` starts a comment.
            _ => {
                let end = v
                    .char_indices()
                    .find(|&(at, c)| c == '#' && (at == 0 || v[..at].ends_with([' ', '\t'])))
                    .map_or(v.len(), |(at, _)| at);
                v[..end].trim().to_string()
            }
        };
        out.push((k.to_string(), value));
    }
    Ok(out)
}

/// Where the closing quote is (a backslash escapes it inside "…").
fn closing_quote(s: &str, q: char) -> Option<usize> {
    let mut escaped = false;
    for (at, c) in s.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if q == '"' => escaped = true,
            c if c == q => return Some(at),
            _ => {}
        }
    }
    None
}

/// The escapes dotenv files use inside double quotes.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some(e @ ('"' | '\\' | '$' | '/')) => out.push(e),
            // `vfy env pull` writes values as JSON strings.
            Some('u') => {
                let hex: String = chars.clone().take(4).collect();
                match u32::from_str_radix(&hex, 16)
                    .ok()
                    .filter(|_| hex.len() == 4)
                    .and_then(char::from_u32)
                {
                    Some(c) => {
                        out.push(c);
                        chars.nth(3);
                    }
                    None => out.push_str("\\u"),
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn preview_only_cannot_resolve_production_database_credentials() {
        let (s, app, _, _) = setup();
        let k = key();
        s.tx(|c| {
            put(
                c,
                &k,
                VarScope::App,
                &app,
                &EnvVarsPut {
                    vars: vec![EnvVarInput {
                        preview_only: Some(true),
                        ..input("DATABASE_URL", "${{db.prod.url}}")
                    }],
                    replace: false,
                },
            )
        })
        .unwrap();
        let err = s
            .with(|c| resolve_for_deploy(c, &k, &app, true, None))
            .err()
            .expect("production DB reference refused before lookup");
        assert!(err.to_string().contains("production database"), "{err}");
        let provenance = s.with(|c| provenance(c, &k, &app)).unwrap();
        assert!(provenance.preview.iter().all(|v| v.key != "DATABASE_URL"));
    }
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
            share_with_previews: None,
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
    fn legacy_variables_cannot_redirect_deployment_tools() {
        let (store, app, _, _) = setup();
        let key = key();
        store
            .tx(|c| {
                put(
                    c,
                    &key,
                    VarScope::App,
                    &app,
                    &EnvVarsPut {
                        vars: vec![input("APP_ENDPOINT", "private-test-value")],
                        replace: false,
                    },
                )
            })
            .unwrap();
        store
            .tx(|c| {
                c.execute("UPDATE env_vars SET key='DOCKER_HOST'", [])?;
                Ok(())
            })
            .unwrap();
        let error = store
            .with(|c| resolve_for_deploy(c, &key, &app, false, None))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("DOCKER_HOST"));
        assert!(!error.contains("private-test-value"));
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
        let preview = s.with(|c| resolve_all(c, &k, &app, true)).unwrap();
        assert_eq!(get(&preview.vars, "A"), "app-preview");
        assert!(!preview.vars.contains_key("B"), "not shared: production-only");
        assert_eq!(
            preview.withheld.keys().collect::<Vec<_>>(),
            ["B", "C", "D"],
            "previews say what they go without"
        );
    }

    /// Design §5.1: previews get normal values only when shared on purpose,
    /// a production database never, and existing variables (migration 0007)
    /// start production-only.
    #[test]
    fn previews_get_only_what_is_shared() {
        let (s, app, env, project) = setup();
        let k = key();
        let set = |scope, id: &str, vars: Vec<EnvVarInput>| {
            s.tx(|c| put(c, &k, scope, id, &EnvVarsPut { vars, replace: false }))
                .unwrap();
        };
        let shared = |k: &str, v: &str| EnvVarInput {
            share_with_previews: Some(true),
            secret: Some(false),
            ..input(k, v)
        };
        set(
            VarScope::Project,
            &project,
            vec![
                shared("NEXT_PUBLIC_SITE", "https://example.test"),
                shared("API_URL", "https://api"),
                input("STRIPE_KEY", "sk_live_123456"),
            ],
        );
        set(
            VarScope::Environment,
            &env,
            vec![
                shared("DATABASE_URL", "${{db.main.url}}"),
                EnvVarInput {
                    preview_only: Some(true),
                    share_with_previews: Some(true),
                    ..input("FEATURE", "on")
                },
            ],
        );
        // The app's own API_URL isn't shared: it hides the project's shared one.
        set(VarScope::App, &app, vec![input("API_URL", "https://app-api")]);

        let prod = s.with(|c| resolve_all(c, &k, &app, false)).unwrap();
        assert_eq!(prod.vars.len(), 4, "production gets every normal value");
        assert!(prod.withheld.is_empty());
        let preview = s.with(|c| resolve_all(c, &k, &app, true)).unwrap();
        let got: Vec<(&str, VarOrigin)> = preview.vars.iter().map(|(k, v)| (k.as_str(), v.origin)).collect();
        assert_eq!(
            got,
            [
                ("DATABASE_URL", VarOrigin::Shared),
                ("FEATURE", VarOrigin::PreviewOnly),
                ("NEXT_PUBLIC_SITE", VarOrigin::Shared),
            ]
        );
        assert_eq!(preview.withheld.keys().collect::<Vec<_>>(), ["API_URL", "STRIPE_KEY"]);
        // A preview-only value is never "shared" (it is the preview's).
        let listed = s.with(|c| list(c, &k, VarScope::Environment, &env, false)).unwrap();
        assert!(listed.iter().all(|v| !(v.preview_only && v.share_with_previews)));

        // The shared database reference stops the preview before anything is
        // filled in, with what to do; production fills it as before.
        let err = s
            .with(|c| resolve_for_deploy(c, &k, &app, true, None))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("DATABASE_URL points at a production database"), "{err}");
        assert!(err.contains("preview-only value"), "{err}");
        let prod_err = s.with(|c| resolve_for_deploy(c, &k, &app, false, None)).err().unwrap();
        assert!(
            !prod_err.to_string().contains("production database"),
            "production resolves references as before: {prod_err}"
        );

        // Provenance: no values, the withheld keys and why.
        let p = s.with(|c| provenance(c, &k, &app)).unwrap();
        let withheld: Vec<(&str, &str)> = p.withheld.iter().map(|w| (w.key.as_str(), w.reason.as_str())).collect();
        assert_eq!(
            withheld,
            [
                ("API_URL", "production_only"),
                ("DATABASE_URL", "production_database"),
                ("STRIPE_KEY", "production_only")
            ]
        );
        assert_eq!(
            p.preview.iter().map(|v| v.key.as_str()).collect::<Vec<_>>(),
            ["FEATURE", "NEXT_PUBLIC_SITE"]
        );
        assert!(p
            .production
            .iter()
            .any(|v| v.key == "DATABASE_URL" && v.database_reference));
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            !json.contains("sk_live") && !json.contains("example.test"),
            "never values"
        );

        s.tx(|c| set_sharing(c, VarScope::Project, &project, "STRIPE_KEY", true))
            .unwrap();
        assert!(s
            .with(|c| resolve(c, &k, &app, true))
            .unwrap()
            .contains_key("STRIPE_KEY"));
        s.tx(|c| set_sharing(c, VarScope::Project, &project, "STRIPE_KEY", false))
            .unwrap();
        assert!(matches!(
            s.tx(|c| set_sharing(c, VarScope::Project, &project, "NOPE", true)),
            Err(Error::NotFound(_))
        ));

        // Unsharing and replacing keep the flag where it was not given.
        set(
            VarScope::Project,
            &project,
            vec![input("NEXT_PUBLIC_SITE", "https://new.test")],
        );
        let preview = s.with(|c| resolve(c, &k, &app, true)).unwrap();
        assert_eq!(
            preview["NEXT_PUBLIC_SITE"].value.as_str(),
            "https://new.test",
            "still shared"
        );
        set(
            VarScope::Project,
            &project,
            vec![EnvVarInput {
                share_with_previews: Some(false),
                ..input("NEXT_PUBLIC_SITE", "https://new.test")
            }],
        );
        assert!(!s
            .with(|c| resolve(c, &k, &app, true))
            .unwrap()
            .contains_key("NEXT_PUBLIC_SITE"));
    }

    /// Rows written before migration 0007 read as production-only.
    #[test]
    fn existing_variables_become_production_only() {
        let (s, app, _, _) = setup();
        let k = key();
        s.tx(|c| {
            put(
                c,
                &k,
                VarScope::App,
                &app,
                &EnvVarsPut {
                    vars: vec![input("TOKEN", "abcdef")],
                    replace: false,
                },
            )
        })
        .unwrap();
        s.with(|c| {
            assert_eq!(
                c.query_row(
                    "SELECT dflt_value FROM pragma_table_info('env_vars') WHERE name = 'share_with_previews'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "0"
            );
            Ok(())
        })
        .unwrap();
        let r = s.with(|c| resolve_all(c, &k, &app, true)).unwrap();
        assert!(r.vars.is_empty());
        assert!(r.withheld.contains_key("TOKEN"));
    }

    #[test]
    fn effective_production_variables() {
        let (s, app, env, project) = setup();
        let k = key();
        let set = |scope, id: &str, vars: Vec<EnvVarInput>| {
            s.tx(|c| put(c, &k, scope, id, &EnvVarsPut { vars, replace: false }))
                .unwrap();
        };
        let preview = |k: &str, v: &str| EnvVarInput {
            preview_only: Some(true),
            ..input(k, v)
        };
        set(
            VarScope::Project,
            &project,
            vec![
                input("DATABASE_URL", "postgres://project"),
                input("SHARED", "project"),
                preview("PREVIEW_URL", "https://pr.project"),
            ],
        );
        set(
            VarScope::Environment,
            &env,
            vec![input("API_URL", "https://env"), input("SHARED", "env")],
        );
        set(
            VarScope::App,
            &app,
            vec![
                input("GREETING", "hi"),
                input("SHARED", "app"),
                preview("SHARED", "app-preview"),
            ],
        );
        let vars = s.with(|c| effective(c, &k, &app, true)).unwrap();
        let got: Vec<(&str, &str, VarScope)> = vars
            .iter()
            .map(|v| (v.key.as_str(), v.value.as_deref().unwrap(), v.scope))
            .collect();
        assert_eq!(
            got,
            [
                ("API_URL", "https://env", VarScope::Environment),
                ("DATABASE_URL", "postgres://project", VarScope::Project),
                ("GREETING", "hi", VarScope::App),
                ("SHARED", "app", VarScope::App),
            ]
        );
        assert!(vars.iter().all(|v| !v.preview_only));
        // The same merge a production deployment gets.
        let prod = s.with(|c| resolve(c, &k, &app, false)).unwrap();
        assert_eq!(prod.len(), vars.len());
        for v in &vars {
            assert_eq!(prod[&v.key].value.as_str(), v.value.as_deref().unwrap());
        }
        // Masked unless revealed.
        let masked = s.with(|c| effective(c, &k, &app, false)).unwrap();
        assert!(masked.iter().all(|v| v.value.is_none()));
        assert!(s.with(|c| effective(c, &k, "app_missing", true)).is_err());
    }

    #[test]
    fn preview_only_wins_across_levels() {
        let (s, app, env, project) = setup();
        let k = key();
        let set = |scope, id: &str, vars: Vec<EnvVarInput>| {
            s.tx(|c| put(c, &k, scope, id, &EnvVarsPut { vars, replace: false }))
                .unwrap();
        };
        let preview_only = |k: &str, v: &str| EnvVarInput {
            preview_only: Some(true),
            ..input(k, v)
        };
        set(
            VarScope::Project,
            &project,
            vec![
                preview_only("DATABASE_URL", "project-preview"),
                preview_only("B", "project-preview"),
                preview_only("C", "project-preview"),
                preview_only("ONLY_PREVIEW", "x"),
            ],
        );
        set(VarScope::Environment, &env, vec![input("C", "env")]);
        set(
            VarScope::App,
            &app,
            vec![
                input("DATABASE_URL", "app"),
                input("B", "app"),
                preview_only("B", "app-preview"),
            ],
        );
        let get = |m: &BTreeMap<String, Resolved>, key: &str| m[key].value.to_string();
        let prod = s.with(|c| resolve(c, &k, &app, false)).unwrap();
        let preview = s.with(|c| resolve(c, &k, &app, true)).unwrap();
        // a) project preview-only beats app normal in a preview only.
        assert_eq!(get(&preview, "DATABASE_URL"), "project-preview");
        assert_eq!(get(&prod, "DATABASE_URL"), "app");
        // b) among preview-only values the more specific level wins.
        assert_eq!(get(&preview, "B"), "app-preview");
        // c) environment normal loses to project preview-only.
        assert_eq!(get(&preview, "C"), "project-preview");
        assert_eq!(get(&prod, "C"), "env");
        // d) production never sees preview-only values.
        assert!(!prod.contains_key("ONLY_PREVIEW"));
        assert_eq!(get(&prod, "B"), "app");
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
        // What real files hold: a BOM, a multi-line key, a comment after a
        // quoted value, an escaped backslash, a # inside a value.
        let parsed = parse_dotenv(
            "\u{feff}KEY=\"-----BEGIN KEY-----\nabc\n-----END KEY-----\"\nQ=\"v\" # note\nS=\"a\\\\nb \\\"q\\\"\"\nURL=http://x/#frag\nexport\tT='x y'\r\n",
        )
        .unwrap();
        assert_eq!(
            parsed,
            [
                ("KEY".into(), "-----BEGIN KEY-----\nabc\n-----END KEY-----".into()),
                ("Q".into(), "v".into()),
                ("S".into(), "a\\nb \"q\"".into()),
                ("URL".into(), "http://x/#frag".into()),
                ("T".into(), "x y".into()),
            ]
        );
        assert!(parse_dotenv("A=\"never closed\nB=1")
            .unwrap_err()
            .to_string()
            .contains("line 1"));
        assert!(parse_dotenv("A=\"v\" junk").is_err());
        // What `vfy env pull` writes (jq's @json) reads back as it was.
        assert_eq!(
            parse_dotenv("J=\"a\\u0001b\\tc\\/d\\u00e9\"").unwrap(),
            [("J".into(), "a\u{1}b\tc/d\u{e9}".into())]
        );
        // A variable called `exporter` is not an `export` line.
        assert_eq!(parse_dotenv("exporter=1").unwrap(), [("exporter".into(), "1".into())]);
        assert!(parse_dotenv("1X=y").is_err());
    }
}

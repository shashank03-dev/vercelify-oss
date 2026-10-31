//! Apps and their domains.

use crate::ids::{new_id, valid_name};
use crate::store::get_setting;
use crate::{now, projects, validate, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use vercelify_core::platform::{
    App, AppBuild, AppCreate, AppDomain, AppKind, AppPatch, AppRuntime, AppSource, CertStatus, DeploymentStatus,
    Desired, DomainRequest, PreviewSettings,
};

/// Server-wide domain setting (`settings.domain`): new apps get
/// `<name>.<wildcard>`; previews `pr-<n>-<name>.<wildcard>`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DomainSettings {
    pub wildcard: Option<String>,
    /// Serve generated domains over HTTPS (direct mode; tunnel mode is
    /// HTTPS at Cloudflare's edge and plain HTTP here).
    pub https: bool,
    /// TLS ends at Cloudflare (tunnel mode).
    pub edge_tls: bool,
}

pub const DOMAIN_SETTING: &str = "domain";

pub fn domain_settings(c: &Connection) -> Result<DomainSettings> {
    Ok(get_setting(c, DOMAIN_SETTING)?.unwrap_or_default())
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("serializable")
}

fn from_json<T: for<'de> Deserialize<'de> + Default>(raw: &str) -> T {
    // Columns are written by us; an unreadable one (a downgrade after a newer
    // agent added a variant) falls back to defaults instead of hiding the app.
    serde_json::from_str(raw).unwrap_or_default()
}

fn kind(s: &str) -> AppKind {
    match s {
        "image" => AppKind::Image,
        "compose" => AppKind::Compose,
        _ => AppKind::Git,
    }
}

fn desired(s: &str) -> Desired {
    match s {
        "stopped" => Desired::Stopped,
        "sleeping" => Desired::Sleeping,
        _ => Desired::Running,
    }
}

fn cert_status(s: &str) -> CertStatus {
    match s {
        "issued" => CertStatus::Issued,
        "failed" => CertStatus::Failed,
        "edge" => CertStatus::Edge,
        "none" => CertStatus::None,
        _ => CertStatus::Pending,
    }
}

fn domain_row(r: &Row) -> rusqlite::Result<AppDomain> {
    Ok(AppDomain {
        id: r.get("id")?,
        host: r.get("host")?,
        https: r.get("https")?,
        is_primary: r.get("is_primary")?,
        redirect_to: r.get("redirect_to")?,
        cert_status: cert_status(&r.get::<_, String>("cert_status")?),
        created_at: r.get("created_at")?,
    })
}

pub fn domains(c: &Connection, app_id: &str) -> Result<Vec<AppDomain>> {
    let mut st = c.prepare("SELECT * FROM app_domains WHERE app_id = ?1 ORDER BY is_primary DESC, host")?;
    let rows = st.query_map([app_id], domain_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn url_of(domains: &[AppDomain]) -> Option<String> {
    domains
        .iter()
        .find(|d| d.is_primary && d.redirect_to.is_none())
        .or_else(|| domains.iter().find(|d| d.redirect_to.is_none()))
        .map(|d| {
            // Edge TLS: plain HTTP here, HTTPS for visitors.
            let https = d.https || d.cert_status == CertStatus::Edge;
            format!("{}://{}", if https { "https" } else { "http" }, d.host)
        })
}

const APP_SELECT: &str = "SELECT a.*, e.name AS environment, e.project_id AS project_id,
  (SELECT d.status FROM deployments d WHERE d.app_id = a.id AND d.kind = 'production'
    ORDER BY d.number DESC LIMIT 1) AS last_status
  FROM apps a JOIN environments e ON a.environment_id = e.id";

fn app_row(c: &Connection, r: &Row) -> Result<App> {
    let id: String = r.get("id")?;
    let domains = domains(c, &id)?;
    Ok(App {
        url: url_of(&domains),
        domains,
        name: r.get("name")?,
        project_id: r.get("project_id")?,
        environment_id: r.get("environment_id")?,
        environment: r.get("environment")?,
        kind: kind(&r.get::<_, String>("kind")?),
        source: from_json(&r.get::<_, String>("source")?),
        build: from_json(&r.get::<_, String>("build")?),
        runtime: from_json(&r.get::<_, String>("runtime")?),
        previews: from_json(&r.get::<_, String>("previews")?),
        node_id: r.get("node_id")?,
        desired: desired(&r.get::<_, String>("desired")?),
        current_deployment_id: r.get("current_deployment_id")?,
        last_status: r
            .get::<_, Option<String>>("last_status")?
            .and_then(|s| DeploymentStatus::parse(&s)),
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        id,
    })
}

pub fn list(c: &Connection, project_id: Option<&str>) -> Result<Vec<App>> {
    let mut st = c.prepare(&format!(
        "{APP_SELECT} WHERE (?1 IS NULL OR e.project_id = ?1) ORDER BY a.name"
    ))?;
    let mut rows = st.query([project_id])?;
    let mut out = vec![];
    while let Some(r) = rows.next()? {
        out.push(app_row(c, r)?);
    }
    Ok(out)
}

pub fn get(c: &Connection, id: &str) -> Result<App> {
    let mut st = c.prepare(&format!("{APP_SELECT} WHERE a.id = ?1"))?;
    let mut rows = st.query([id])?;
    match rows.next()? {
        Some(r) => app_row(c, r),
        None => Err(Error::not_found(format!("app {id}"))),
    }
}

/// By id or by name (the CLI and URLs use names).
pub fn find(c: &Connection, id_or_name: &str) -> Result<App> {
    let id: Option<String> = c
        .query_row("SELECT id FROM apps WHERE id = ?1 OR name = ?1", [id_or_name], |r| {
            r.get(0)
        })
        .optional()?;
    match id {
        Some(id) => get(c, &id),
        None => Err(Error::not_found(format!("app {id_or_name}"))),
    }
}

fn check_source(kind: AppKind, s: &AppSource) -> Result<AppSource> {
    let mut s = s.clone();
    let inline = kind == AppKind::Compose && s.compose_content.as_deref().is_some_and(|c| !c.trim().is_empty());
    match kind {
        AppKind::Compose if inline => {
            let content = s.compose_content.clone().unwrap_or_default();
            if content.len() > 64 * 1024 {
                return Err(Error::Invalid("the compose file is larger than 64 KiB".into()));
            }
            if !content.lines().any(|l| l.trim_end() == "services:") {
                return Err(Error::Invalid("the compose file has no `services:`".into()));
            }
            s = AppSource {
                compose_content: Some(content),
                ..AppSource::default()
            };
        }
        AppKind::Git | AppKind::Compose => {
            let repo = s
                .repository
                .as_deref()
                .filter(|r| !r.trim().is_empty())
                .ok_or_else(|| Error::Invalid("a repository is required".into()))?;
            s.repository = Some(validate::repository(repo)?);
            s.image = None;
        }
        AppKind::Image => {
            let image = s
                .image
                .as_deref()
                .map(str::trim)
                .filter(|i| !i.is_empty())
                .ok_or_else(|| Error::Invalid("an image is required (nginx:1.27)".into()))?
                .to_string();
            validate::image(&image)?;
            s = AppSource {
                image: Some(image),
                ..AppSource::default()
            };
        }
    }
    if let Some(b) = s.branch.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        validate::branch(b)?;
        s.branch = Some(b.to_string());
    } else {
        s.branch = None;
    }
    if let Some(p) = &s.pinned_sha {
        validate::commit_sha(p)?;
    }
    // Inline compose files belong to compose apps without a repository.
    if kind != AppKind::Compose || s.repository.is_some() {
        s.compose_content = None;
    }
    if kind == AppKind::Compose {
        let f = s.compose_file.clone().unwrap_or_else(|| "docker-compose.yml".into());
        validate::repo_path("compose file", &f)?;
        s.compose_file = Some(f);
    } else {
        s.compose_file = None;
    }
    if let Some(g) = &s.git_source_id {
        if !crate::ids::valid_id("git", g) {
            return Err(Error::Invalid(format!("`{g}` is not a git source id")));
        }
    }
    Ok(s)
}

fn check_build(b: &AppBuild) -> Result<()> {
    if let Some(d) = &b.dockerfile {
        validate::repo_path("dockerfile", d)?;
    }
    if let Some(ctx) = b.context.as_deref().filter(|c| *c != "." && !c.is_empty()) {
        validate::repo_path("context", ctx)?;
    }
    if let Some(o) = &b.output_directory {
        validate::repo_path("output directory", o)?;
    }
    for cmd in [&b.install_command, &b.build_command].into_iter().flatten() {
        if cmd.len() > 2000 || cmd.contains('\0') {
            return Err(Error::Invalid("build commands are at most 2000 characters".into()));
        }
    }
    if b.watch_paths.len() > 50 {
        return Err(Error::Invalid("at most 50 watch paths".into()));
    }
    for w in &b.watch_paths {
        validate::repo_path("watch path", w.trim_start_matches('!'))?;
    }
    Ok(())
}

fn check_runtime(r: &AppRuntime) -> Result<()> {
    if let Some(p) = r.port {
        validate::port(p)?;
    }
    if !(1..=10).contains(&r.replicas) {
        return Err(Error::Invalid("replicas must be between 1 and 10".into()));
    }
    if r.memory_mb.is_some_and(|m| !(16..=262_144).contains(&m)) {
        return Err(Error::Invalid("memory must be between 16 MB and 256 GB".into()));
    }
    if r.cpus.is_some_and(|c| !(0.01..=64.0).contains(&c)) {
        return Err(Error::Invalid("cpus must be between 0.01 and 64".into()));
    }
    if let Some(h) = &r.health {
        if let Some(p) = &h.path {
            if !p.starts_with('/') || p.len() > 400 || p.contains([' ', '\n']) {
                return Err(Error::Invalid(format!("health check path `{p}` must start with /")));
            }
        }
        if h.command.as_ref().is_some_and(|c| c.is_empty() || c.len() > 50) {
            return Err(Error::Invalid("the health check command needs 1–50 arguments".into()));
        }
    }
    if !(10..=3600).contains(&r.health_timeout_secs) {
        return Err(Error::Invalid(
            "the health check timeout must be 10–3600 seconds".into(),
        ));
    }
    if r.drain_secs > 300 {
        return Err(Error::Invalid("the drain time is at most 300 seconds".into()));
    }
    if let Some(cmd) = &r.command {
        if cmd.is_empty() || cmd.len() > 100 {
            return Err(Error::Invalid("the start command needs 1–100 arguments".into()));
        }
    }
    if let Some(s) = &r.service {
        let compose_name = !s.is_empty()
            && s.len() <= 63
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
        if !valid_name(s) && !compose_name {
            return Err(Error::Invalid(format!("`{s}` is not a compose service name")));
        }
    }
    Ok(())
}

fn check_previews(p: &PreviewSettings) -> Result<()> {
    if !(1..=20).contains(&p.max) {
        return Err(Error::Invalid("previews: max must be between 1 and 20".into()));
    }
    Ok(())
}

fn check_node(c: &Connection, node: Option<&str>) -> Result<Option<String>> {
    match node.filter(|n| !n.is_empty()) {
        None => Ok(None),
        Some(n) => {
            let exists: Option<String> = c
                .query_row("SELECT id FROM nodes WHERE id = ?1 OR name = ?1", [n], |r| r.get(0))
                .optional()?;
            exists.map(Some).ok_or_else(|| Error::not_found(format!("server {n}")))
        }
    }
}

/// Claim a name for an app or database (one namespace, design §3).
pub fn claim_name(c: &Connection, name: &str, owner_kind: &str, owner_id: &str) -> Result<()> {
    if !valid_name(name) {
        return Err(Error::Invalid(format!(
            "`{name}` can't be a name: lowercase letters, digits and -, at most 40 characters"
        )));
    }
    c.execute(
        "INSERT INTO resource_names (name, owner_kind, owner_id) VALUES (?1, ?2, ?3)",
        params![name, owner_kind, owner_id],
    )
    .map_err(|e| match Error::from(e) {
        Error::Conflict(_) => Error::Conflict(format!("the name {name} is already used by an app or database")),
        e => e,
    })?;
    Ok(())
}

pub fn create(c: &Connection, req: &AppCreate) -> Result<App> {
    let env = projects::resolve_target(c, req.environment_id.as_deref(), req.project_id.as_deref())?;
    let source = check_source(req.kind, &req.source)?;
    let build = req.build.clone().unwrap_or_default();
    check_build(&build)?;
    let runtime = req.runtime.clone().unwrap_or_default();
    check_runtime(&runtime)?;
    let previews = req.previews.clone().unwrap_or(PreviewSettings {
        enabled: req.kind == AppKind::Git,
        max: 3,
    });
    check_previews(&previews)?;
    let node = check_node(c, req.node_id.as_deref())?;
    let name = req.name.trim();
    let id = new_id("app");
    claim_name(c, name, "app", &id)?;
    let t = now();
    c.execute(
        "INSERT INTO apps (id, environment_id, name, kind, source, build, runtime, previews, node_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
        params![
            id,
            env.id,
            name,
            req.kind.as_str(),
            json(&source),
            json(&build),
            json(&runtime),
            json(&previews),
            node,
            t
        ],
    )?;
    let settings = domain_settings(c)?;
    let mut first = true;
    if let Some(w) = settings.wildcard.as_deref().filter(|w| !w.is_empty()) {
        let host = format!("{name}.{w}");
        insert_domain(c, &id, &host, settings.https, true, None, &settings)?;
        first = false;
    }
    for d in &req.domains {
        let host = validate::host(d)?;
        insert_domain(c, &id, &host, true, first, None, &settings)?;
        first = false;
    }
    get(c, &id)
}

pub fn patch(c: &Connection, id: &str, p: &AppPatch) -> Result<App> {
    let app = get(c, id)?;
    let set = |col: &str, val: String| -> Result<()> {
        c.execute(
            &format!("UPDATE apps SET {col} = ?2, updated_at = ?3 WHERE id = ?1"),
            params![id, val, now()],
        )?;
        Ok(())
    };
    if let Some(s) = &p.source {
        set("source", json(&check_source(app.kind, s)?))?;
    }
    if let Some(b) = &p.build {
        check_build(b)?;
        set("build", json(b))?;
    }
    if let Some(r) = &p.runtime {
        check_runtime(r)?;
        set("runtime", json(r))?;
    }
    if let Some(pr) = &p.previews {
        check_previews(pr)?;
        set("previews", json(pr))?;
    }
    if let Some(d) = p.desired {
        set("desired", d.as_str().to_string())?;
    }
    if let Some(n) = &p.node_id {
        let node = check_node(c, Some(n))?;
        // Volumes live on the server the app runs on (design §8.3).
        let before = get(c, id)?;
        // Unpinning ("any server") counts: the next deploy could land elsewhere.
        if before.node_id.is_some()
            && node != before.node_id
            && before.current_deployment_id.is_some()
            && !crate::volumes::list(c, vercelify_core::platform::VolumeOwner::App, id)?.is_empty()
        {
            return Err(Error::Conflict(format!(
                "{} keeps volumes on its server: it can't move to another one (copy the data, then recreate the app there)",
                before.name
            )));
        }
        c.execute(
            "UPDATE apps SET node_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, node, now()],
        )?;
    }
    get(c, id)
}

/// Removes the app and everything that belongs to it from the database.
/// Its containers, routes and images are removed by the reconciler.
pub fn delete(c: &Connection, id: &str) -> Result<App> {
    let app = get(c, id)?;
    c.execute("DELETE FROM env_vars WHERE scope = 'app' AND scope_id = ?1", [id])?;
    for v in crate::volumes::list(c, vercelify_core::platform::VolumeOwner::App, id)? {
        let name = crate::volumes::physical_name(&v, &app.name, None);
        c.execute(
            "UPDATE volumes SET storage_name = ?2 WHERE id = ?1",
            params![v.id, name],
        )?;
    }
    c.execute("DELETE FROM apps WHERE id = ?1", [id])?;
    c.execute("DELETE FROM resource_names WHERE owner_id = ?1", [id])?;
    Ok(app)
}

fn insert_domain(
    c: &Connection,
    app_id: &str,
    host: &str,
    https: bool,
    primary: bool,
    redirect_to: Option<&str>,
    settings: &DomainSettings,
) -> Result<AppDomain> {
    let id = new_id("dom");
    let status = if settings.edge_tls {
        "edge"
    } else if https {
        "pending"
    } else {
        "none"
    };
    if primary {
        c.execute("UPDATE app_domains SET is_primary = 0 WHERE app_id = ?1", [app_id])?;
    }
    c.execute(
        "INSERT INTO app_domains (id, app_id, host, https, is_primary, redirect_to, cert_status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            app_id,
            host,
            https && !settings.edge_tls,
            primary,
            redirect_to,
            status,
            now()
        ],
    )
    .map_err(|e| match Error::from(e) {
        Error::Conflict(_) => Error::Conflict(format!("{host} is already used by another app")),
        e => e,
    })?;
    Ok(c.query_row("SELECT * FROM app_domains WHERE id = ?1", [&id], domain_row)?)
}

pub fn add_domain(c: &Connection, app_id: &str, req: &DomainRequest) -> Result<AppDomain> {
    let app = get(c, app_id)?;
    let host = validate::host(&req.host)?;
    let redirect = match req.redirect_to.as_deref().filter(|r| !r.is_empty()) {
        Some(r) => {
            let r = validate::host(r)?;
            if r == host {
                return Err(Error::Invalid("a domain can't redirect to itself".into()));
            }
            if !app.domains.iter().any(|d| d.host == r) {
                return Err(Error::Invalid(format!("{r} is not a domain of this app; add it first")));
            }
            Some(r)
        }
        None => None,
    };
    let primary = req.primary.unwrap_or(app.domains.is_empty()) && redirect.is_none();
    let settings = domain_settings(c)?;
    insert_domain(
        c,
        app_id,
        &host,
        req.https.unwrap_or(true),
        primary,
        redirect.as_deref(),
        &settings,
    )
}

pub fn remove_domain(c: &Connection, app_id: &str, host_or_id: &str) -> Result<()> {
    let key = host_or_id.trim();
    // Removed by id, the redirects below still need the host.
    let host: String = c
        .query_row(
            "SELECT host FROM app_domains WHERE app_id = ?1 AND (id = ?2 OR host = ?3)",
            params![app_id, key, key.to_ascii_lowercase()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::not_found(format!("domain {host_or_id}")))?;
    c.execute(
        "DELETE FROM app_domains WHERE app_id = ?1 AND host = ?2",
        params![app_id, host],
    )?;
    // Keep one primary while there are domains left.
    c.execute(
        "UPDATE app_domains SET is_primary = 1 WHERE id = (
           SELECT id FROM app_domains WHERE app_id = ?1 AND redirect_to IS NULL ORDER BY created_at LIMIT 1)
         AND NOT EXISTS (SELECT 1 FROM app_domains WHERE app_id = ?1 AND is_primary = 1)",
        [app_id],
    )?;
    // Redirects to the removed host would point nowhere.
    c.execute(
        "DELETE FROM app_domains WHERE app_id = ?1 AND redirect_to = ?2",
        params![app_id, host],
    )?;
    Ok(())
}

/// HTTP Basic Auth in front of the app: `Some((user, password))` sets it,
/// `None` removes it. Stored as an htpasswd line with a bcrypt hash.
pub fn set_basic_auth(c: &Connection, id: &str, cred: Option<(&str, &str)>) -> Result<()> {
    get(c, id)?;
    let line = match cred {
        None => String::new(),
        Some((user, password)) => {
            let user = user.trim();
            if user.is_empty() || user.len() > 64 || user.contains([':', ' ', '\n']) {
                return Err(Error::Invalid(
                    "the user name must be 1–64 characters without : or spaces".into(),
                ));
            }
            if password.len() < 8 || password.len() > 72 {
                return Err(Error::Invalid("the password must be 8–72 characters".into()));
            }
            let hash = bcrypt::hash(password, 10).map_err(|e| Error::Internal(format!("bcrypt: {e}")))?;
            format!("{user}:{hash}")
        }
    };
    c.execute(
        "UPDATE apps SET basic_auth = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, line, now()],
    )?;
    Ok(())
}

/// The htpasswd lines of an app (empty: no password).
pub fn basic_auth(c: &Connection, id: &str) -> Result<Vec<String>> {
    let line: String = c.query_row("SELECT basic_auth FROM apps WHERE id = ?1", [id], |r| r.get(0))?;
    Ok(if line.is_empty() { vec![] } else { vec![line] })
}

/// Match HTTPS domains to the certificates the proxy holds: pending ones
/// that have one are issued; issued ones that lost theirs are pending again.
/// Returns how many changed.
pub fn sync_cert_status(c: &Connection, has_cert: &dyn Fn(&str) -> bool) -> Result<usize> {
    let mut st = c.prepare(
        "SELECT host, cert_status FROM app_domains WHERE https = 1 AND cert_status IN ('pending', 'issued')",
    )?;
    let rows: Vec<(String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut changed = 0;
    for (host, status) in rows {
        let want = if has_cert(&host) {
            CertStatus::Issued
        } else {
            CertStatus::Pending
        };
        if cert_status(&status) != want {
            set_cert_status(c, &host, want)?;
            changed += 1;
        }
    }
    Ok(changed)
}

pub fn set_cert_status(c: &Connection, host: &str, status: CertStatus) -> Result<()> {
    let s = match status {
        CertStatus::Pending => "pending",
        CertStatus::Issued => "issued",
        CertStatus::Failed => "failed",
        CertStatus::Edge => "edge",
        CertStatus::None => "none",
    };
    c.execute(
        "UPDATE app_domains SET cert_status = ?2 WHERE host = ?1",
        params![host, s],
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::store::put_setting;
    use crate::Store;
    use vercelify_core::platform::HealthCheck;

    pub fn git_app(name: &str) -> AppCreate {
        AppCreate {
            name: name.into(),
            kind: AppKind::Git,
            environment_id: None,
            project_id: None,
            source: AppSource {
                repository: Some("https://github.com/me/blog".into()),
                branch: Some("main".into()),
                ..AppSource::default()
            },
            build: None,
            runtime: None,
            previews: None,
            node_id: None,
            domains: vec![],
        }
    }

    #[test]
    fn git_apps_enable_previews_by_default() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| create(c, &git_app("blog"))).unwrap();
        assert!(a.previews.enabled);
    }

    #[test]
    fn create_get_list_delete() {
        let s = Store::open_in_memory().unwrap();
        s.with(|c| {
            put_setting(
                c,
                DOMAIN_SETTING,
                &DomainSettings {
                    wildcard: Some("example.com".into()),
                    https: true,
                    edge_tls: false,
                },
            )
        })
        .unwrap();
        let mut req = git_app("blog");
        req.domains = vec!["Blog.Example.org".into()];
        let a = s.tx(|c| create(c, &req)).unwrap();
        assert_eq!(a.environment, "production");
        assert_eq!(a.runtime.replicas, 1);
        assert_eq!(a.url.as_deref(), Some("https://blog.example.com"));
        assert_eq!(a.domains.len(), 2);
        assert_eq!(a.domains.iter().filter(|d| d.is_primary).count(), 1);
        assert_eq!(s.with(|c| find(c, "blog")).unwrap().id, a.id);
        assert_eq!(s.with(|c| list(c, None)).unwrap().len(), 1);
        assert_eq!(s.with(|c| list(c, Some(&a.project_id))).unwrap().len(), 1);
        assert_eq!(s.with(|c| list(c, Some("prj_other"))).unwrap().len(), 0);

        // The name is taken, by apps and databases alike.
        assert!(matches!(s.tx(|c| create(c, &git_app("blog"))), Err(Error::Conflict(m)) if m.contains("blog")));
        // So is the domain.
        let mut dup = git_app("blog2");
        dup.domains = vec!["blog.example.org".into()];
        assert!(matches!(s.tx(|c| create(c, &dup)), Err(Error::Conflict(m)) if m.contains("blog.example.org")));
        assert!(s.with(|c| find(c, "blog2")).is_err(), "rolled back");

        s.tx(|c| delete(c, &a.id)).unwrap();
        s.tx(|c| create(c, &git_app("blog"))).unwrap();
    }

    #[test]
    fn app_creation_rejects_injection_payloads_without_writing_rows() {
        let s = Store::open_in_memory().unwrap();
        for name in [
            "app;touch /tmp/pwned",
            "app$(id)",
            "app`id`",
            "app' OR 1=1 --",
            "app.example.com`)",
            "app\ntraefik.enable=true",
            "../app",
        ] {
            assert!(s.tx(|c| create(c, &git_app(name))).is_err(), "accepted {name:?}");
        }
        assert!(s.with(|c| list(c, None)).unwrap().is_empty());
        assert!(s.tx(|c| create(c, &git_app("valid-app"))).is_ok());
    }

    #[test]
    fn last_status_is_the_newest_production_deployment() {
        use crate::deployments::{self, NewDeployment};
        use vercelify_core::platform::{DeploymentStatus, DeploymentTrigger};
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| create(c, &git_app("blog"))).unwrap();
        assert_eq!(a.last_status, None);
        let new = |pr| NewDeployment {
            app_id: &a.id,
            pr_number: pr,
            branch: "main",
            commit_sha: "abc",
            commit_message: "",
            trigger: DeploymentTrigger::Manual,
            rollback_of: None,
            image: "",
            node_id: None,
        };
        let d = s.tx(|c| deployments::create(c, &new(None))).unwrap();
        s.tx(|c| deployments::transition(c, &d.id, DeploymentStatus::Building, None))
            .unwrap();
        // A preview doesn't count.
        s.tx(|c| deployments::create(c, &new(Some(7)))).unwrap();
        assert_eq!(
            s.with(|c| get(c, &a.id)).unwrap().last_status,
            Some(DeploymentStatus::Building)
        );
        assert_eq!(
            s.with(|c| list(c, None)).unwrap()[0].last_status,
            Some(DeploymentStatus::Building)
        );
    }

    #[test]
    fn validation() {
        let s = Store::open_in_memory().unwrap();
        let bad = |f: &dyn Fn(&mut AppCreate)| {
            let mut r = git_app("x");
            f(&mut r);
            s.tx(|c| create(c, &r)).unwrap_err()
        };
        assert!(matches!(bad(&|r| r.name = "Bad Name".into()), Error::Invalid(_)));
        assert!(matches!(bad(&|r| r.source.repository = None), Error::Invalid(_)));
        assert!(matches!(
            bad(&|r| r.source.repository = Some("file:///etc".into())),
            Error::Invalid(_)
        ));
        assert!(matches!(
            bad(&|r| r.source.branch = Some("-x".into())),
            Error::Invalid(_)
        ));
        assert!(matches!(
            bad(&|r| {
                r.runtime = Some(AppRuntime {
                    replicas: 0,
                    ..AppRuntime::default()
                })
            }),
            Error::Invalid(_)
        ));
        assert!(matches!(
            bad(&|r| {
                r.runtime = Some(AppRuntime {
                    health: Some(HealthCheck {
                        path: Some("healthz".into()),
                        command: None,
                    }),
                    ..AppRuntime::default()
                })
            }),
            Error::Invalid(_)
        ));
        assert!(matches!(
            bad(&|r| {
                r.build = Some(AppBuild {
                    dockerfile: Some("../Dockerfile".into()),
                    ..AppBuild::default()
                })
            }),
            Error::Invalid(_)
        ));
        assert!(matches!(
            bad(&|r| r.node_id = Some("node_missing".into())),
            Error::NotFound(_)
        ));
        assert!(matches!(
            bad(&|r| r.environment_id = Some("env_missing".into())),
            Error::NotFound(_)
        ));
        // Image apps need an image and keep nothing else.
        let mut img = git_app("web");
        img.kind = AppKind::Image;
        assert!(s.tx(|c| create(c, &img)).is_err());
        img.source.image = Some("nginx:1.27".into());
        let a = s.tx(|c| create(c, &img)).unwrap();
        assert_eq!(a.source.repository, None);
        assert_eq!(a.source.image.as_deref(), Some("nginx:1.27"));
    }

    #[test]
    fn patch_fields() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| create(c, &git_app("blog"))).unwrap();
        let p = s
            .tx(|c| {
                patch(
                    c,
                    &a.id,
                    &AppPatch {
                        runtime: Some(AppRuntime {
                            port: Some(8080),
                            ..AppRuntime::default()
                        }),
                        desired: Some(Desired::Stopped),
                        ..AppPatch::default()
                    },
                )
            })
            .unwrap();
        assert_eq!(p.runtime.port, Some(8080));
        assert_eq!(p.desired, Desired::Stopped);
        assert_eq!(p.source, a.source, "untouched");
        assert!(s
            .tx(|c| patch(
                c,
                &a.id,
                &AppPatch {
                    source: Some(AppSource::default()),
                    ..AppPatch::default()
                }
            ))
            .is_err());
    }

    #[test]
    fn an_app_with_volumes_stays_on_its_server() {
        use vercelify_core::platform::AppVolumeRequest;
        let s = Store::open_in_memory().unwrap();
        let main = s
            .tx(|c| crate::nodes::ensure_main(c, "main", "http://127.0.0.1:7420", "x86_64"))
            .unwrap();
        let a = s.tx(|c| create(c, &git_app("blog"))).unwrap();
        s.tx(|c| {
            crate::volumes::create(
                c,
                vercelify_core::platform::VolumeOwner::App,
                &a.id,
                &AppVolumeRequest {
                    name: "data".into(),
                    mount_path: "/data".into(),
                    tier: None,
                },
            )?;
            // Deployed and pinned, as the engine leaves it.
            c.execute(
                "UPDATE apps SET node_id = ?2, current_deployment_id = 'dep_x' WHERE id = ?1",
                params![a.id, main.id],
            )?;
            Ok(())
        })
        .unwrap();
        let unpin = AppPatch {
            node_id: Some(String::new()),
            ..AppPatch::default()
        };
        assert!(matches!(s.tx(|c| patch(c, &a.id, &unpin)), Err(Error::Conflict(_))));
        assert_eq!(s.with(|c| get(c, &a.id)).unwrap().node_id, Some(main.id.clone()));
        // Choosing the server it is already on is fine.
        let same = AppPatch {
            node_id: Some(main.id.clone()),
            ..AppPatch::default()
        };
        assert!(s.tx(|c| patch(c, &a.id, &same)).is_ok());
    }

    #[test]
    fn domains_keep_one_primary() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| create(c, &git_app("blog"))).unwrap();
        assert!(a.url.is_none());
        let add = |host: &str, primary: Option<bool>, redirect: Option<&str>| {
            s.tx(|c| {
                add_domain(
                    c,
                    &a.id,
                    &DomainRequest {
                        host: host.into(),
                        https: None,
                        primary,
                        redirect_to: redirect.map(String::from),
                    },
                )
            })
        };
        assert!(
            add("blog.example.com", None, None).unwrap().is_primary,
            "first is primary"
        );
        assert!(
            !add("www.example.com", None, Some("blog.example.com"))
                .unwrap()
                .is_primary
        );
        assert!(add("x.example.com", None, Some("nope.example.com")).is_err());
        assert!(add("new.example.com", Some(true), None).unwrap().is_primary);
        let app = s.with(|c| get(c, &a.id)).unwrap();
        assert_eq!(app.domains.iter().filter(|d| d.is_primary).count(), 1);
        assert_eq!(app.url.as_deref(), Some("https://new.example.com"));

        s.tx(|c| remove_domain(c, &a.id, "new.example.com")).unwrap();
        let app = s.with(|c| get(c, &a.id)).unwrap();
        assert_eq!(
            app.domains.iter().filter(|d| d.is_primary).count(),
            1,
            "a primary is chosen again"
        );
        s.tx(|c| remove_domain(c, &a.id, "blog.example.com")).unwrap();
        let app = s.with(|c| get(c, &a.id)).unwrap();
        assert!(app.domains.is_empty(), "the redirect to the removed host went with it");

        // By id, too.
        let target = add("site.example.com", None, None).unwrap();
        add("www.example.com", None, Some("site.example.com")).unwrap();
        s.tx(|c| remove_domain(c, &a.id, &target.id)).unwrap();
        let app = s.with(|c| get(c, &a.id)).unwrap();
        assert!(app.domains.is_empty(), "the redirect went with a domain removed by id");
        assert!(matches!(
            s.tx(|c| remove_domain(c, &a.id, "gone.example.com")),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn basic_auth_is_hashed() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| create(c, &git_app("blog"))).unwrap();
        s.tx(|c| set_basic_auth(c, &a.id, Some(("me", "correct horse"))))
            .unwrap();
        let lines = s.with(|c| basic_auth(c, &a.id)).unwrap();
        let (user, hash) = lines[0].split_once(':').unwrap();
        assert_eq!(user, "me");
        assert!(bcrypt::verify("correct horse", hash).unwrap());
        assert!(!lines[0].contains("correct horse"));
        assert!(s.tx(|c| set_basic_auth(c, &a.id, Some(("me", "short")))).is_err());
        assert!(s
            .tx(|c| set_basic_auth(c, &a.id, Some(("a:b", "long enough"))))
            .is_err());
        s.tx(|c| set_basic_auth(c, &a.id, None)).unwrap();
        assert!(s.with(|c| basic_auth(c, &a.id)).unwrap().is_empty());
    }

    #[test]
    fn cert_status_follows_the_proxys_certificates() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| create(c, &git_app("blog"))).unwrap();
        for host in ["blog.example.com", "www.example.com"] {
            s.tx(|c| {
                add_domain(
                    c,
                    &a.id,
                    &DomainRequest {
                        host: host.into(),
                        https: None,
                        primary: None,
                        redirect_to: None,
                    },
                )
            })
            .unwrap();
        }
        let status = |h: &str| {
            s.with(|c| get(c, &a.id))
                .unwrap()
                .domains
                .into_iter()
                .find(|d| d.host == h)
                .unwrap()
                .cert_status
        };
        assert_eq!(status("blog.example.com"), CertStatus::Pending);
        let n = s.with(|c| sync_cert_status(c, &|h| h == "blog.example.com")).unwrap();
        assert_eq!(n, 1);
        assert_eq!(status("blog.example.com"), CertStatus::Issued);
        assert_eq!(status("www.example.com"), CertStatus::Pending);
        assert_eq!(
            s.with(|c| sync_cert_status(c, &|h| h == "blog.example.com")).unwrap(),
            0,
            "nothing new"
        );
        // The certificate went (storage reset): pending again.
        s.with(|c| sync_cert_status(c, &|_| false)).unwrap();
        assert_eq!(status("blog.example.com"), CertStatus::Pending);
    }

    #[test]
    fn tunnel_mode_domains_are_edge_tls() {
        let s = Store::open_in_memory().unwrap();
        s.with(|c| {
            put_setting(
                c,
                DOMAIN_SETTING,
                &DomainSettings {
                    wildcard: Some("example.com".into()),
                    https: true,
                    edge_tls: true,
                },
            )
        })
        .unwrap();
        let a = s.tx(|c| create(c, &git_app("blog"))).unwrap();
        assert_eq!(a.domains[0].cert_status, CertStatus::Edge);
        assert!(!a.domains[0].https, "plain HTTP behind the tunnel");
        assert_eq!(
            a.url.as_deref(),
            Some("https://blog.example.com"),
            "visitors still get HTTPS"
        );
    }
}

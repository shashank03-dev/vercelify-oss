//! Taking over a Coolify v4 installation (`vercelify migrate`, design §18):
//! what Coolify's export says, the plan (nothing changed yet), and the
//! records written from it in one transaction. Containers, images and
//! volume data are the agent's part (`agent/src/migrate.rs`).
//!
//! The export comes from `agent/src/migrate_export.php`, run in Coolify's
//! own container, with secrets decrypted by Coolify.

use crate::apps;
use crate::databases::{self, Credentials};
use crate::deployments::{self, NewDeployment};
use crate::git_sources::{self, GithubAppCreds};
use crate::secrets::MasterKey;
use crate::{env, git_watch, ids, projects, tasks, validate, volumes, Error, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use vercelify_core::platform::{
    AppBuild, AppCreate, AppKind, AppRuntime, AppSource, AppVolumeRequest, BuildPack, DatabaseCreate, DatabaseKind,
    DeploymentTrigger, EnvVarInput, EnvVarsPut, GitAuthKind, GitProvider, GitSourceRequest, HealthCheck,
    PreviewSettings, ProjectRequest, ScheduledTaskRequest, VarScope, VolumeOwner,
};

// ----- the export --------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Export {
    pub version: Option<String>,
    pub projects: Vec<CProject>,
    pub apps: Vec<CApp>,
    pub databases: Vec<CDatabase>,
    pub services: Vec<CService>,
    pub sources: Vec<CSource>,
    pub shared: Vec<CShared>,
    pub servers: Vec<CServer>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CProject {
    pub uuid: String,
    pub name: String,
    pub description: String,
    pub environments: Vec<CEnvironment>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CEnvironment {
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CServer {
    pub name: String,
    pub ip: String,
    pub local: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CWhere {
    pub environment: String,
    pub project: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CVar {
    pub key: String,
    pub value: String,
    pub preview: bool,
    pub build: bool,
    pub runtime: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CVolume {
    pub name: String,
    pub mount_path: String,
    pub host_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CTask {
    pub name: String,
    pub command: String,
    pub frequency: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CBasicAuth {
    pub user: String,
    pub password: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CApp {
    pub uuid: String,
    pub name: String,
    pub server: CServer,
    #[serde(rename = "where")]
    pub at: CWhere,
    pub status: String,
    pub build_pack: String,
    pub repository: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub source: Option<String>,
    pub base_directory: Option<String>,
    pub publish_directory: Option<String>,
    pub install_command: Option<String>,
    pub build_command: Option<String>,
    pub start_command: Option<String>,
    pub dockerfile_location: Option<String>,
    pub dockerfile: Option<String>,
    pub compose_location: Option<String>,
    pub compose_raw: Option<String>,
    pub compose_domains: Option<String>,
    pub image: Option<String>,
    pub image_tag: Option<String>,
    pub ports: Option<String>,
    pub fqdn: Option<String>,
    pub health_path: Option<String>,
    pub memory: Option<String>,
    pub cpus: Option<String>,
    pub watch_paths: Option<String>,
    pub auto_deploy: bool,
    pub previews: bool,
    pub basic_auth: Option<CBasicAuth>,
    pub envs: Vec<CVar>,
    pub volumes: Vec<CVolume>,
    pub tasks: Vec<CTask>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CDatabase {
    pub uuid: String,
    pub name: String,
    pub kind: String,
    pub image: String,
    pub user: Option<String>,
    pub password: String,
    pub database: Option<String>,
    pub public_port: Option<u16>,
    pub status: String,
    pub server: CServer,
    #[serde(rename = "where")]
    pub at: CWhere,
    pub volumes: Vec<CVolume>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CRoute {
    pub service: String,
    pub fqdn: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CService {
    pub uuid: String,
    pub name: String,
    #[serde(rename = "where")]
    pub at: CWhere,
    pub server: CServer,
    pub compose_raw: Option<String>,
    pub routes: Vec<CRoute>,
    pub envs: Vec<CVar>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CSource {
    pub key: String,
    /// `public`, `github_app`, `deploy_key`.
    pub kind: String,
    pub name: String,
    pub html_url: Option<String>,
    pub organization: Option<String>,
    pub app_id: Option<u64>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub webhook_secret: Option<String>,
    pub private_key: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CShared {
    pub key: String,
    pub value: String,
    /// `team`, `project`, `environment`, `server`.
    #[serde(rename = "type")]
    pub kind: String,
    pub project: Option<String>,
    pub environment: Option<String>,
}

// ----- the plan -------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Create,
    Skip,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlanItem {
    /// `app`, `database`, `service`.
    pub kind: &'static str,
    pub coolify: String,
    pub coolify_name: String,
    /// Its name here.
    pub name: String,
    pub project: String,
    pub environment: String,
    pub action: Action,
    /// What it becomes (`git app, built by Dockerfile`) or why it is skipped.
    pub summary: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Plan {
    pub coolify_version: String,
    /// Projects to create (`false`: one of that name exists and is used).
    pub projects: Vec<(String, bool)>,
    pub items: Vec<PlanItem>,
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn creates(&self) -> usize {
        self.items.iter().filter(|i| i.action == Action::Create).count()
    }
}

/// A volume whose data the agent copies: Coolify's Docker volume (or its
/// compose project and volume key) into ours.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VolumeCopy {
    pub from: String,
    pub to: String,
}

/// The image a migrated app's first deployment runs, without a build.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Adopt {
    /// The image Coolify built for the app (`<uuid>:<commit>`), tagged as
    /// `tag` before the deployment runs; missing, it is built again.
    Built { tag: String },
    /// A registry image: pulled if it isn't here.
    Registry,
    /// Compose: `docker compose up` builds or pulls.
    Compose { coolify_project: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImportedApp {
    pub coolify: String,
    pub id: String,
    pub name: String,
    pub adopt: Adopt,
    pub deployment: Option<String>,
    pub volumes: Vec<VolumeCopy>,
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImportedDb {
    pub coolify: String,
    pub id: String,
    pub name: String,
    pub volume: Option<VolumeCopy>,
}

/// What the import wrote, kept in the migration's state for the agent's
/// Docker steps and for a rollback.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Imported {
    pub projects: Vec<String>,
    pub sources: Vec<String>,
    pub apps: Vec<ImportedApp>,
    pub databases: Vec<ImportedDb>,
}

/// Coolify's own variables; Vercelify sets its own (`VERCEL_URL`…).
fn coolify_var(k: &str) -> bool {
    k.starts_with("COOLIFY_") || k == "SOURCE_COMMIT"
}

/// A Coolify name as a Vercelify name: lowercase, digits and `-`.
pub fn slug(name: &str, fallback: &str) -> String {
    let mut s = String::new();
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch.to_ascii_lowercase());
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s: String = s.trim_matches('-').chars().take(ids::MAX_NAME_LEN).collect();
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

fn unique(base: &str, taken: &mut HashSet<String>) -> String {
    let mut n = 1;
    loop {
        let cand = if n == 1 {
            base.to_string()
        } else {
            let suffix = format!("-{n}");
            let keep = ids::MAX_NAME_LEN.saturating_sub(suffix.len());
            format!(
                "{}{suffix}",
                base.chars().take(keep).collect::<String>().trim_end_matches('-')
            )
        };
        if taken.insert(cand.clone()) {
            return cand;
        }
        n += 1;
    }
}

/// `https://a.test:3000/api` → (host, https, port, path).
fn parse_url(u: &str) -> Option<(String, bool, Option<u16>, String)> {
    let u = u.trim();
    let (https, rest) = if let Some(r) = u.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = u.strip_prefix("http://") {
        (false, r)
    } else {
        (true, u)
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()),
        None => (hostport, None),
    };
    (!host.is_empty()).then(|| (host.to_ascii_lowercase(), https, port, path.to_string()))
}

/// Domains of an `fqdn` list: hosts (https?) and the container port, if one
/// of them names it.
fn domains(fqdn: Option<&str>, notes: &mut Vec<String>) -> (Vec<(String, bool)>, Option<u16>) {
    let mut out = vec![];
    let mut port = None;
    for u in fqdn.unwrap_or("").split(',').filter(|u| !u.trim().is_empty()) {
        let Some((host, https, p, path)) = parse_url(u) else {
            notes.push(format!("domain `{u}` isn't a URL: left out"));
            continue;
        };
        if validate::host(&host).is_err() {
            notes.push(format!("domain `{host}` isn't valid here: left out"));
            continue;
        }
        if !path.is_empty() && path != "/" {
            notes.push(format!(
                "{host}{path}: path prefixes aren't routed separately; {host} serves the whole app"
            ));
        }
        port = port.or(p);
        if !out.iter().any(|(h, _): &(String, bool)| *h == host) {
            out.push((host, https));
        }
    }
    (out, port)
}

fn megabytes(v: Option<&str>) -> Option<u32> {
    let v = v?.trim().to_ascii_lowercase();
    let (num, mult) = match v.chars().last()? {
        'k' => (&v[..v.len() - 1], 1.0 / 1024.0),
        'm' => (&v[..v.len() - 1], 1.0),
        'g' => (&v[..v.len() - 1], 1024.0),
        'b' => (&v[..v.len() - 1], 1.0 / 1_048_576.0),
        _ => (v.as_str(), 1.0 / 1_048_576.0),
    };
    let mb = (num.parse::<f64>().ok()? * mult).round();
    (mb >= 1.0).then_some(mb as u32)
}

fn cpus(v: Option<&str>) -> Option<f32> {
    v?.trim().parse::<f32>().ok().filter(|c| *c > 0.0)
}

fn rel(p: Option<&str>) -> Option<String> {
    let p = p?.trim().trim_matches('/');
    (!p.is_empty() && p != ".").then(|| p.to_string())
}

fn some(v: Option<&str>) -> Option<String> {
    v.map(str::trim).filter(|v| !v.is_empty()).map(str::to_string)
}

/// Coolify's frequency aliases as cron.
fn cron(f: &str) -> String {
    match f.trim() {
        "every_minute" => "* * * * *",
        "hourly" => "0 * * * *",
        "daily" => "0 0 * * *",
        "weekly" => "0 0 * * 0",
        "monthly" => "0 0 1 * *",
        "yearly" | "annually" => "0 0 1 1 *",
        other => other,
    }
    .to_string()
}

fn db_kind(k: &str) -> Option<DatabaseKind> {
    DatabaseKind::ALL.iter().copied().find(|d| d.as_str() == k)
}

/// The version in an image tag: `postgres:16-alpine` → `16`.
fn version_of(image: &str) -> String {
    let tag = image.rsplit_once(':').map(|(_, t)| t).unwrap_or("latest");
    let v = tag.split('-').next().unwrap_or(tag);
    if databases::valid_version(v) {
        v.to_string()
    } else {
        "latest".into()
    }
}

/// The repository URL: Coolify keeps `owner/repo` for GitHub/GitLab Apps.
fn repo_url(repo: &str, source: Option<&CSource>) -> String {
    let r = repo.trim();
    if r.contains("://") || r.starts_with("git@") {
        return r.to_string();
    }
    let base = source
        .and_then(|s| s.html_url.as_deref())
        .unwrap_or("https://github.com")
        .trim_end_matches('/');
    format!("{base}/{}", r.trim_start_matches('/'))
}

/// What a Coolify app becomes: kind, source, build, a note.
struct Shape {
    kind: AppKind,
    source: AppSource,
    build: AppBuild,
    summary: String,
    adopt_built: bool,
}

fn shape(a: &CApp, sources: &HashMap<&str, &CSource>, notes: &mut Vec<String>) -> std::result::Result<Shape, String> {
    let src = a.source.as_deref().and_then(|k| sources.get(k).copied());
    let repo = || -> std::result::Result<String, String> {
        let r = some(a.repository.as_deref()).ok_or("it has no repository")?;
        validate::repository(&repo_url(&r, src)).map_err(|e| e.to_string())
    };
    let git_source = || src.filter(|s| s.kind != "public").map(|s| s.key.clone());
    let branch = some(a.branch.as_deref());
    let base = AppBuild {
        context: rel(a.base_directory.as_deref()),
        install_command: some(a.install_command.as_deref()),
        build_command: some(a.build_command.as_deref()),
        watch_paths: a
            .watch_paths
            .as_deref()
            .unwrap_or("")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        ..AppBuild::default()
    };
    Ok(match a.build_pack.as_str() {
        "dockerimage" => {
            let name = some(a.image.as_deref()).ok_or("it has no image")?;
            let tag = some(a.image_tag.as_deref()).unwrap_or_else(|| "latest".into());
            let image = if name.contains('@') {
                name
            } else {
                format!("{name}:{tag}")
            };
            validate::image(&image).map_err(|e| e.to_string())?;
            Shape {
                kind: AppKind::Image,
                source: AppSource {
                    image: Some(image.clone()),
                    ..AppSource::default()
                },
                build: AppBuild::default(),
                summary: format!("image {image}"),
                adopt_built: false,
            }
        }
        "dockercompose" => Shape {
            kind: AppKind::Compose,
            source: AppSource {
                repository: Some(repo()?),
                branch,
                git_source_id: git_source(),
                compose_file: rel(a.compose_location.as_deref()),
                ..AppSource::default()
            },
            build: base,
            summary: "compose app, from its repository".into(),
            adopt_built: false,
        },
        "dockerfile" if a.source.is_none() && some(a.dockerfile.as_deref()).is_some() => {
            notes.push(
                "its Dockerfile was written in Coolify, not in a repository: it runs the image Coolify built; \
                 to build it again, put the Dockerfile in a repository"
                    .into(),
            );
            Shape {
                kind: AppKind::Image,
                source: AppSource::default(),
                build: AppBuild::default(),
                summary: "the image Coolify built (inline Dockerfile)".into(),
                adopt_built: true,
            }
        }
        pack => {
            let (pack, summary) = match pack {
                "dockerfile" => (BuildPack::Dockerfile, "git app, built by its Dockerfile"),
                "static" => (BuildPack::Static, "static site from git"),
                "nixpacks" | "railpack" => (BuildPack::Auto, "git app, build detected (was Nixpacks)"),
                other => {
                    notes.push(format!("Coolify build pack `{other}`: its build is detected here"));
                    (BuildPack::Auto, "git app, build detected")
                }
            };
            Shape {
                kind: AppKind::Git,
                source: AppSource {
                    repository: Some(repo()?),
                    branch,
                    git_source_id: git_source(),
                    ..AppSource::default()
                },
                build: AppBuild {
                    pack,
                    dockerfile: (pack == BuildPack::Dockerfile)
                        .then(|| rel(a.dockerfile_location.as_deref()))
                        .flatten()
                        .filter(|d| d != "Dockerfile"),
                    output_directory: (pack == BuildPack::Static)
                        .then(|| rel(a.publish_directory.as_deref()))
                        .flatten(),
                    ..base
                },
                summary: summary.into(),
                adopt_built: true,
            }
        }
    })
}

/// Coolify's `{{project.KEY}}`, `{{environment.KEY}}`, `{{team.KEY}}`
/// references, resolved (Vercelify has no shared-variable references).
fn resolve_shared(value: &str, project: Option<&str>, environment: &str, shared: &[CShared]) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut missing = vec![];
    let mut rest = value;
    while let Some(i) = rest.find("{{") {
        let Some(end) = rest[i..].find("}}") else { break };
        let inner = rest[i + 2..i + end].trim();
        out.push_str(&rest[..i]);
        let found = inner.split_once('.').and_then(|(kind, key)| {
            shared
                .iter()
                .find(|s| {
                    s.kind == kind
                        && s.key == key
                        && match kind {
                            "project" => s.project.as_deref() == project,
                            "environment" => {
                                s.project.as_deref() == project && s.environment.as_deref() == Some(environment)
                            }
                            _ => true,
                        }
                })
                .map(|s| s.value.clone())
        });
        match found {
            Some(v) => out.push_str(&v),
            None => {
                missing.push(inner.to_string());
                out.push_str(&rest[i..i + end + 2]);
            }
        }
        rest = &rest[i + end + 2..];
    }
    out.push_str(rest);
    (out, missing)
}

/// Coolify's variables of one resource as ours: previews only where their
/// value differs; database hosts become references; shared references are
/// resolved.
#[allow(clippy::too_many_arguments)]
fn variables(
    vars: &[CVar],
    project: Option<&str>,
    environment: &str,
    shared: &[CShared],
    db_hosts: &[(String, String, String)],
    my_project: &str,
    notes: &mut Vec<String>,
) -> Vec<EnvVarInput> {
    let mut normal: BTreeMap<&str, &CVar> = BTreeMap::new();
    let mut preview: BTreeMap<&str, &CVar> = BTreeMap::new();
    for v in vars {
        if coolify_var(&v.key) {
            continue;
        }
        if validate::env_key(&v.key).is_err() {
            notes.push(format!(
                "variable `{}` has a name that isn't allowed here: left out",
                v.key
            ));
            continue;
        }
        if v.preview {
            preview.insert(&v.key, v);
        } else {
            normal.insert(&v.key, v);
        }
    }
    let fix = |v: &CVar, notes: &mut Vec<String>| {
        let (mut value, missing) = resolve_shared(&v.value, project, environment, shared);
        for m in missing {
            notes.push(format!(
                "{}: the shared variable `{{{{{m}}}}}` wasn't found; kept as text",
                v.key
            ));
        }
        for (uuid, name, db_project) in db_hosts {
            if value.contains(uuid.as_str()) {
                value = value.replace(uuid.as_str(), &format!("${{{{db.{name}.host}}}}"));
                if db_project != my_project {
                    notes.push(format!(
                        "{} refers to the database {name} of another project: move one of them so they share a project",
                        v.key
                    ));
                }
            }
        }
        value
    };
    let mut out = vec![];
    for (k, v) in &normal {
        out.push(EnvVarInput {
            key: k.to_string(),
            value: fix(v, notes),
            build: Some(v.build),
            preview_only: Some(false),
            secret: Some(true),
        });
    }
    for (k, v) in &preview {
        if normal.get(k).is_some_and(|n| n.value == v.value) {
            continue;
        }
        out.push(EnvVarInput {
            key: k.to_string(),
            value: fix(v, notes),
            build: Some(v.build),
            preview_only: Some(true),
            secret: Some(true),
        });
    }
    out
}

/// Names in use here (apps and databases share one namespace).
fn taken_names(c: &Connection) -> Result<HashSet<String>> {
    let mut st = c.prepare("SELECT name FROM resource_names")?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn project_name_of(e: &Export, uuid: Option<&str>) -> String {
    uuid.and_then(|u| e.projects.iter().find(|p| p.uuid == u))
        .map(|p| p.name.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Default".into())
}

/// What the migration will do, changing nothing.
pub fn plan(c: &Connection, e: &Export) -> Result<Plan> {
    let mut taken = taken_names(c)?;
    let existing: HashSet<String> = projects::list(c)?.into_iter().map(|p| p.name).collect();
    let mut warnings = vec![];
    let mut items = vec![];
    let sources: HashMap<&str, &CSource> = e.sources.iter().map(|s| (s.key.as_str(), s)).collect();
    let remote = |s: &CServer, notes: &mut Vec<String>| -> bool {
        if s.local {
            return false;
        }
        notes.push(format!(
            "it runs on {} ({}): join that server to Vercelify (sudo vercelify mesh invite --platform here), \
             then create it again there; Coolify keeps running it until then",
            s.name, s.ip
        ));
        true
    };
    for d in &e.databases {
        let mut notes = vec![];
        let name = unique(
            &slug(&d.name, &format!("db-{}", &d.uuid[..d.uuid.len().min(8)])),
            &mut taken,
        );
        let (action, summary) = match db_kind(&d.kind) {
            _ if remote(&d.server, &mut notes) => (Action::Skip, "on another server".to_string()),
            None => {
                notes.push(format!("Vercelify has no {} databases", d.kind));
                (Action::Skip, d.kind.clone())
            }
            Some(k) => {
                let t = databases::template(k);
                if d.volumes
                    .iter()
                    .any(|v| v.mount_path.trim_end_matches('/') == t.data_path)
                {
                    for v in d
                        .volumes
                        .iter()
                        .filter(|v| v.mount_path.trim_end_matches('/') != t.data_path)
                    {
                        notes.push(format!(
                            "its volume at {} isn't taken over (only {})",
                            v.mount_path, t.data_path
                        ));
                    }
                    if d.public_port.is_some() {
                        notes.push(format!(
                            "public port {} stays open on every interface, as in Coolify",
                            d.public_port.unwrap_or_default()
                        ));
                    }
                    (Action::Create, format!("{} ({}), data copied", d.kind, d.image))
                } else {
                    notes.push(format!(
                        "its data isn't at {} (image {}): copy it into a new database by hand (backup and restore)",
                        t.data_path, d.image
                    ));
                    (Action::Skip, d.kind.clone())
                }
            }
        };
        if action == Action::Skip {
            taken.remove(&name);
        }
        items.push(PlanItem {
            kind: "database",
            coolify: d.uuid.clone(),
            coolify_name: d.name.clone(),
            name,
            project: project_name_of(e, d.at.project.as_deref()),
            environment: d.at.environment.clone(),
            action,
            summary,
            notes,
        });
    }
    for a in &e.apps {
        let mut notes = vec![];
        let name = unique(
            &slug(&a.name, &format!("app-{}", &a.uuid[..a.uuid.len().min(8)])),
            &mut taken,
        );
        let (action, summary) = if remote(&a.server, &mut notes) {
            (Action::Skip, "on another server".to_string())
        } else {
            match shape(a, &sources, &mut notes) {
                Ok(s) => {
                    if !a.volumes.is_empty() {
                        notes.push("it has volumes: it stops while their data is copied".into());
                    }
                    if a.previews {
                        notes.push("open previews start again with the next push to their pull request".into());
                    }
                    if a.basic_auth
                        .as_ref()
                        .is_some_and(|b| !b.user.is_empty() && !(8..=72).contains(&b.password.len()))
                    {
                        notes.push(
                            "its password protection uses a password Vercelify doesn't accept (8–72 characters): \
                             it is taken over stopped; set a new password, then start it"
                                .into(),
                        );
                    }
                    if let Some(s) = a.source.as_deref().and_then(|k| sources.get(k)) {
                        if s.kind == "github_app" && s.private_key.is_none() {
                            notes.push(format!(
                                "the GitHub App {} has no key in Coolify: connect it again",
                                s.name
                            ));
                        }
                    }
                    (Action::Create, s.summary)
                }
                Err(why) => {
                    notes.push(format!("not taken over: {why}"));
                    (Action::Skip, "can't be taken over".to_string())
                }
            }
        };
        if action == Action::Skip {
            taken.remove(&name);
        }
        items.push(PlanItem {
            kind: "app",
            coolify: a.uuid.clone(),
            coolify_name: a.name.clone(),
            name,
            project: project_name_of(e, a.at.project.as_deref()),
            environment: a.at.environment.clone(),
            action,
            summary,
            notes,
        });
    }
    for s in &e.services {
        let mut notes = vec![];
        let name = unique(
            &slug(&s.name, &format!("svc-{}", &s.uuid[..s.uuid.len().min(8)])),
            &mut taken,
        );
        let (action, summary) = if remote(&s.server, &mut notes) {
            (Action::Skip, "on another server".to_string())
        } else if some(s.compose_raw.as_deref()).is_none() {
            notes.push("Coolify has no compose file for it".into());
            (Action::Skip, "can't be taken over".to_string())
        } else {
            notes.push("a Coolify service: its compose file and variables are taken over; check it afterwards".into());
            (Action::Create, "compose app (Coolify service)".to_string())
        };
        if action == Action::Skip {
            taken.remove(&name);
        }
        items.push(PlanItem {
            kind: "service",
            coolify: s.uuid.clone(),
            coolify_name: s.name.clone(),
            name,
            project: project_name_of(e, s.at.project.as_deref()),
            environment: s.at.environment.clone(),
            action,
            summary,
            notes,
        });
    }
    let mut projects: Vec<(String, bool)> = vec![];
    for i in items.iter().filter(|i| i.action == Action::Create) {
        if !projects.iter().any(|(p, _)| *p == i.project) {
            projects.push((i.project.clone(), !existing.contains(&i.project)));
        }
    }
    let other: Vec<&str> = e.servers.iter().filter(|s| !s.local).map(|s| s.name.as_str()).collect();
    if !other.is_empty() {
        warnings.push(format!(
            "Coolify also manages {}: their apps stay with Coolify until each server joins Vercelify",
            other.join(", ")
        ));
    }
    if let Some(v) = &e.version {
        if v != "4.4.6" {
            warnings.push(format!(
                "Coolify {v}: the migration is tested with 4.4.6; check the plan closely"
            ));
        }
    }
    Ok(Plan {
        coolify_version: e.version.clone().unwrap_or_default(),
        projects,
        items,
        warnings,
    })
}

/// The tag a migrated app's first image gets here.
pub fn adopted_tag(name: &str) -> String {
    format!("vfy/{name}:coolify")
}

/// Write the plan's records. One transaction: all or nothing.
pub fn apply(
    c: &Connection,
    key: &MasterKey,
    e: &Export,
    plan: &Plan,
    valid_schedule: &dyn Fn(&str) -> std::result::Result<(), String>,
) -> Result<Imported> {
    let mut out = Imported::default();
    let item = |kind: &str, uuid: &str| {
        plan.items
            .iter()
            .find(|i| i.kind == kind && i.coolify == uuid && i.action == Action::Create)
    };
    // Projects and environments.
    let mut project_ids: HashMap<String, String> = HashMap::new();
    for (name, new) in &plan.projects {
        let id = if *new {
            let desc = e
                .projects
                .iter()
                .find(|p| p.name.trim() == name)
                .map(|p| p.description.clone());
            let p = projects::create(
                c,
                &ProjectRequest {
                    name: Some(name.clone()),
                    description: desc,
                },
            )?;
            out.projects.push(p.id.clone());
            p.id
        } else {
            projects::list(c)?
                .into_iter()
                .find(|p| p.name == *name)
                .map(|p| p.id)
                .ok_or_else(|| Error::not_found(format!("project {name}")))?
        };
        project_ids.insert(name.clone(), id);
    }
    let env_id = |c: &Connection, project: &str, env: &str| -> Result<String> {
        let pid = project_ids
            .get(project)
            .ok_or_else(|| Error::Internal(format!("project {project} not planned")))?;
        let env = if env.trim().is_empty() {
            "production"
        } else {
            env.trim()
        };
        let env = slug(env, "production");
        match projects::environments(c, pid)?.into_iter().find(|x| x.name == env) {
            Some(x) => Ok(x.id),
            None => Ok(projects::add_environment(c, pid, &env)?.id),
        }
    };
    // Git sources the apps use.
    let mut source_ids: HashMap<String, String> = HashMap::new();
    let existing_sources = git_sources::list(c)?;
    for a in e.apps.iter().filter(|a| item("app", &a.uuid).is_some()) {
        let Some(s) = a.source.as_deref().and_then(|k| e.sources.iter().find(|s| s.key == k)) else {
            continue;
        };
        if s.kind == "public" || source_ids.contains_key(&s.key) {
            continue;
        }
        let id = match s.kind.as_str() {
            "github_app" => {
                let (Some(app_id), Some(pem)) = (s.app_id, s.private_key.clone()) else {
                    continue;
                };
                let slug = s.name.trim().to_string();
                let html = s
                    .html_url
                    .as_deref()
                    .unwrap_or("https://github.com")
                    .trim_end_matches('/');
                let creds = GithubAppCreds {
                    app_id,
                    slug: slug.clone(),
                    pem,
                    client_id: s.client_id.clone().unwrap_or_default(),
                    client_secret: s.client_secret.clone().unwrap_or_default(),
                    html_url: format!("{html}/apps/{slug}"),
                    owner: s.organization.clone().unwrap_or_default(),
                };
                match existing_sources.iter().find(|g| g.name == format!("GitHub App {slug}")) {
                    Some(g) => g.id.clone(),
                    None => {
                        let g =
                            git_sources::create_github_app(c, key, &creds, s.webhook_secret.as_deref().unwrap_or(""))?;
                        out.sources.push(g.id.clone());
                        g.id
                    }
                }
            }
            "deploy_key" => {
                let Some(pem) = s.private_key.clone() else { continue };
                let name = format!("Coolify key {}", s.name.trim());
                let name: String = name.chars().take(80).collect();
                match existing_sources.iter().find(|g| g.name == name) {
                    Some(g) => g.id.clone(),
                    None => {
                        let repo = a.repository.as_deref().unwrap_or("");
                        let provider = if repo.contains("gitlab") {
                            GitProvider::Gitlab
                        } else if repo.contains("github") {
                            GitProvider::Github
                        } else {
                            GitProvider::Generic
                        };
                        let g = git_sources::create(
                            c,
                            key,
                            &GitSourceRequest {
                                name,
                                provider,
                                auth: GitAuthKind::DeployKey,
                                token: None,
                            },
                            Some((pem, String::new())),
                        )?;
                        out.sources.push(g.id.clone());
                        g.id
                    }
                }
            }
            _ => continue,
        };
        source_ids.insert(s.key.clone(), id);
    }
    // Databases first: apps refer to them.
    let mut db_hosts: Vec<(String, String, String)> = vec![];
    for d in &e.databases {
        let Some(i) = item("database", &d.uuid) else { continue };
        let kind = db_kind(&d.kind).ok_or_else(|| Error::Internal(d.kind.clone()))?;
        let t = databases::template(kind);
        let environment_id = env_id(c, &i.project, &i.environment)?;
        let cred = Credentials {
            user: d
                .user
                .clone()
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| "default".into()),
            password: d.password.clone(),
            database: d.database.clone().unwrap_or_default(),
        };
        let db = databases::adopt(
            c,
            key,
            &DatabaseCreate {
                name: i.name.clone(),
                kind,
                version: Some(version_of(&d.image)),
                environment_id: Some(environment_id),
                project_id: None,
                public_port: d.public_port,
                tier: None,
                link_app: None,
            },
            cred,
            &d.image,
        )?;
        let vol = d
            .volumes
            .iter()
            .find(|v| v.mount_path.trim_end_matches('/') == t.data_path);
        let volume = match vol {
            Some(v) if v.host_path.is_some() => {
                let ours = volumes::list(c, VolumeOwner::Database, &db.id)?;
                if let Some(o) = ours.first() {
                    volumes::set_tier(c, &o.id, vercelify_core::StorageTier::Hdd, v.host_path.as_deref())?;
                }
                None
            }
            Some(v) => Some(VolumeCopy {
                from: v.name.clone(),
                to: databases::volume_name(&db.name),
            }),
            None => None,
        };
        db_hosts.push((d.uuid.clone(), db.name.clone(), db.project_id.clone()));
        out.databases.push(ImportedDb {
            coolify: d.uuid.clone(),
            id: db.id,
            name: db.name,
            volume,
        });
    }
    // Apps.
    let sources: HashMap<&str, &CSource> = e.sources.iter().map(|s| (s.key.as_str(), s)).collect();
    for a in &e.apps {
        let Some(i) = item("app", &a.uuid) else { continue };
        let mut notes = vec![];
        let mut s = shape(a, &sources, &mut notes).map_err(Error::Invalid)?;
        if let Some(k) = &s.source.git_source_id {
            s.source.git_source_id = source_ids.get(k).cloned();
        }
        let tag = adopted_tag(&i.name);
        if s.kind == AppKind::Image && s.source.image.is_none() {
            s.source.image = Some(tag.clone());
        }
        let (hosts, fqdn_port) = domains(a.fqdn.as_deref(), &mut notes);
        let (hosts, service) = if s.kind == AppKind::Compose {
            compose_domains(a.compose_domains.as_deref(), &mut notes)
        } else {
            (hosts, None)
        };
        let port = a
            .ports
            .as_deref()
            .and_then(|p| p.split(',').next())
            .and_then(|p| p.trim().split(':').next_back().and_then(|p| p.parse::<u16>().ok()))
            .or(fqdn_port);
        let environment_id = env_id(c, &i.project, &i.environment)?;
        let app = apps::create(
            c,
            &AppCreate {
                name: i.name.clone(),
                kind: s.kind,
                environment_id: Some(environment_id),
                project_id: None,
                source: s.source.clone(),
                build: Some(s.build.clone()),
                runtime: Some(AppRuntime {
                    port,
                    command: some(a.start_command.as_deref()).map(|c| vec!["sh".into(), "-c".into(), c]),
                    memory_mb: megabytes(a.memory.as_deref()),
                    cpus: cpus(a.cpus.as_deref()),
                    health: some(a.health_path.as_deref()).map(|p| HealthCheck {
                        path: Some(p),
                        command: None,
                    }),
                    service,
                    ..AppRuntime::default()
                }),
                previews: Some(PreviewSettings {
                    enabled: a.previews && s.kind == AppKind::Git,
                    max: 3,
                }),
                node_id: None,
                domains: vec![],
            },
        )?;
        add_hosts(c, &app.id, &hosts)?;
        let vars = variables(
            &a.envs,
            a.at.project.as_deref(),
            &a.at.environment,
            &e.shared,
            &db_hosts,
            &app.project_id,
            &mut notes,
        );
        put_vars(c, key, &app.id, vars)?;
        // A protection that can't be kept here leaves the app stopped, not open.
        let mut stopped = false;
        if let Some(b) = a.basic_auth.as_ref().filter(|b| !b.user.is_empty()) {
            if let Err(Error::Invalid(_)) = apps::set_basic_auth(c, &app.id, Some((&b.user, &b.password))) {
                apps::patch(
                    c,
                    &app.id,
                    &vercelify_core::platform::AppPatch {
                        desired: Some(vercelify_core::platform::Desired::Stopped),
                        ..Default::default()
                    },
                )?;
                stopped = true;
            }
        }
        if matches!(s.kind, AppKind::Git | AppKind::Compose) {
            git_watch::set_enabled(c, &app.id, a.auto_deploy)?;
        }
        for t in &a.tasks {
            tasks::create(
                c,
                &app.id,
                &ScheduledTaskRequest {
                    name: t.name.clone(),
                    schedule: cron(&t.frequency),
                    command: t.command.clone(),
                    enabled: t.enabled,
                },
                valid_schedule,
            )?;
        }
        let mut copies = vec![];
        if s.kind != AppKind::Compose {
            for v in &a.volumes {
                let vname = slug(
                    v.name.trim_start_matches(&a.uuid).trim_start_matches(['-', '_']),
                    "data",
                );
                let created = volumes::create(
                    c,
                    VolumeOwner::App,
                    &app.id,
                    &AppVolumeRequest {
                        name: vname.clone(),
                        mount_path: v.mount_path.clone(),
                        tier: None,
                    },
                )?;
                match &v.host_path {
                    Some(h) => volumes::set_tier(c, &created.id, vercelify_core::StorageTier::Hdd, Some(h))?,
                    None => copies.push(VolumeCopy {
                        from: v.name.clone(),
                        to: volumes::docker_name(&app.name, &created.name),
                    }),
                }
            }
        }
        let commit = a
            .commit
            .as_deref()
            .filter(|c| c.len() == 40 && c.bytes().all(|b| b.is_ascii_hexdigit()))
            .unwrap_or("");
        let (adopt, image) = match s.kind {
            AppKind::Compose => (
                Adopt::Compose {
                    coolify_project: a.uuid.clone(),
                },
                String::new(),
            ),
            AppKind::Image if !s.adopt_built => (Adopt::Registry, s.source.image.clone().unwrap_or_default()),
            _ => (Adopt::Built { tag: tag.clone() }, tag),
        };
        let d = (!stopped)
            .then(|| {
                deployments::create(
                    c,
                    &NewDeployment {
                        app_id: &app.id,
                        pr_number: None,
                        branch: app.source.branch.as_deref().unwrap_or(""),
                        commit_sha: commit,
                        commit_message: "Moved from Coolify",
                        trigger: DeploymentTrigger::Manual,
                        rollback_of: None,
                        image: &image,
                        node_id: None,
                    },
                )
            })
            .transpose()?;
        out.apps.push(ImportedApp {
            coolify: a.uuid.clone(),
            id: app.id,
            name: app.name,
            adopt,
            deployment: d.map(|d| d.id),
            volumes: copies,
            hosts: hosts.into_iter().map(|(h, _)| h).collect(),
        });
    }
    // Coolify services: compose apps from their compose file.
    for s in &e.services {
        let Some(i) = item("service", &s.uuid) else { continue };
        let mut notes = vec![];
        let mut hosts = vec![];
        let mut port = None;
        let mut service = None;
        for r in &s.routes {
            let (h, p) = domains(r.fqdn.as_deref(), &mut notes);
            if !h.is_empty() && service.is_none() {
                service = Some(r.service.clone());
                port = p;
            }
            if service.as_deref() == Some(r.service.as_str()) {
                hosts.extend(h);
            }
        }
        let port = port.or_else(|| service_port(&s.envs));
        let environment_id = env_id(c, &i.project, &i.environment)?;
        let app = apps::create(
            c,
            &AppCreate {
                name: i.name.clone(),
                kind: AppKind::Compose,
                environment_id: Some(environment_id),
                project_id: None,
                source: AppSource {
                    compose_content: s.compose_raw.as_deref().map(declare_volumes),
                    ..AppSource::default()
                },
                build: None,
                runtime: Some(AppRuntime {
                    port,
                    service,
                    ..AppRuntime::default()
                }),
                previews: None,
                node_id: None,
                domains: vec![],
            },
        )?;
        add_hosts(c, &app.id, &hosts)?;
        let vars = variables(
            &s.envs,
            s.at.project.as_deref(),
            &s.at.environment,
            &e.shared,
            &db_hosts,
            &app.project_id,
            &mut notes,
        );
        put_vars(c, key, &app.id, vars)?;
        let d = deployments::create(
            c,
            &NewDeployment {
                app_id: &app.id,
                pr_number: None,
                branch: "",
                commit_sha: "",
                commit_message: "Moved from Coolify",
                trigger: DeploymentTrigger::Manual,
                rollback_of: None,
                image: "",
                node_id: None,
            },
        )?;
        out.apps.push(ImportedApp {
            coolify: s.uuid.clone(),
            id: app.id,
            name: app.name,
            adopt: Adopt::Compose {
                coolify_project: s.uuid.clone(),
            },
            deployment: Some(d.id),
            volumes: vec![],
            hosts: hosts.into_iter().map(|(h, _)| h).collect(),
        });
    }
    Ok(out)
}

/// Coolify's service templates use named volumes without declaring them
/// (Coolify adds that when it renders the file): declare them, so Compose
/// accepts the file. Text, not YAML: the templates are regular, and only
/// top-level `volumes:` entries are added.
pub fn declare_volumes(raw: &str) -> String {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let mut used: Vec<String> = vec![];
    let mut declared: Vec<String> = vec![];
    let mut in_list: Option<usize> = None;
    let mut top_level = false;
    for line in raw.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let ind = indent(line);
        if ind == 0 {
            top_level = t == "volumes:";
            in_list = None;
            continue;
        }
        if top_level {
            if ind == 2 {
                if let Some(name) = t.strip_suffix(':').or_else(|| t.split_once(':').map(|(n, _)| n)) {
                    declared.push(name.trim_matches(['\'', '"']).to_string());
                }
            }
            continue;
        }
        match in_list {
            Some(base) if ind > base && t.starts_with('-') => {
                let entry = t.trim_start_matches('-').trim().trim_matches(['\'', '"']);
                if let Some((src, _)) = entry.split_once(':') {
                    let named = !src.is_empty()
                        && !src.contains('/')
                        && !src.starts_with(['.', '~', '$'])
                        && src
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
                    if named && !used.iter().any(|u| u == src) {
                        used.push(src.to_string());
                    }
                }
                continue;
            }
            Some(base) if ind > base => continue,
            _ => in_list = None,
        }
        if t == "volumes:" {
            in_list = Some(ind);
        }
    }
    let missing: Vec<&String> = used.iter().filter(|u| !declared.contains(u)).collect();
    if missing.is_empty() {
        return raw.to_string();
    }
    let entries: String = missing.iter().map(|m| format!("  {m}: {{}}\n")).collect();
    let mut out = String::new();
    let mut added = false;
    for line in raw.lines() {
        out.push_str(line);
        out.push('\n');
        if !added && line.trim_end() == "volumes:" && indent(line) == 0 {
            out.push_str(&entries);
            added = true;
        }
    }
    if !added {
        out.push_str("volumes:\n");
        out.push_str(&entries);
    }
    out
}

/// A service's container port, from Coolify's `SERVICE_URL_<NAME>_<PORT>`
/// (or `SERVICE_FQDN_…`) variables.
fn service_port(vars: &[CVar]) -> Option<u16> {
    vars.iter().find_map(|v| {
        let rest = v
            .key
            .strip_prefix("SERVICE_URL_")
            .or_else(|| v.key.strip_prefix("SERVICE_FQDN_"))?;
        rest.rsplit_once('_')?.1.parse().ok()
    })
}

/// `{"web": {"domain": "https://a.test"}}` → its hosts and the service.
fn compose_domains(raw: Option<&str>, notes: &mut Vec<String>) -> (Vec<(String, bool)>, Option<String>) {
    let map: BTreeMap<String, serde_json::Value> = raw.and_then(|r| serde_json::from_str(r).ok()).unwrap_or_default();
    for (svc, v) in map {
        let d = v.get("domain").and_then(|d| d.as_str()).unwrap_or("");
        let (hosts, _) = domains(Some(d), notes);
        if !hosts.is_empty() {
            return (hosts, Some(svc));
        }
    }
    (vec![], None)
}

/// Coolify's domains, the first one primary (over a generated wildcard
/// one). A host another app here has already is left out.
fn add_hosts(c: &Connection, app_id: &str, hosts: &[(String, bool)]) -> Result<()> {
    for (n, (host, https)) in hosts.iter().enumerate() {
        let used: bool = c.query_row("SELECT count(*) > 0 FROM app_domains WHERE host = ?1", [host], |r| {
            r.get(0)
        })?;
        if used {
            continue;
        }
        apps::add_domain(
            c,
            app_id,
            &vercelify_core::platform::DomainRequest {
                host: host.clone(),
                https: Some(*https),
                primary: Some(n == 0),
                redirect_to: None,
            },
        )?;
    }
    Ok(())
}

fn put_vars(c: &Connection, key: &MasterKey, app_id: &str, vars: Vec<EnvVarInput>) -> Result<()> {
    for preview in [false, true] {
        let vs: Vec<EnvVarInput> = vars
            .iter()
            .filter(|v| v.preview_only == Some(preview))
            .cloned()
            .collect();
        if !vs.is_empty() {
            env::put(
                c,
                key,
                VarScope::App,
                app_id,
                &EnvVarsPut {
                    vars: vs,
                    replace: false,
                },
            )?;
        }
    }
    Ok(())
}

/// Undo an import: the records it wrote (rollback). Containers, volumes and
/// images are the agent's.
pub fn remove(c: &Connection, key: &MasterKey, imported: &Imported) -> Result<()> {
    for a in &imported.apps {
        match apps::delete(c, &a.id) {
            Ok(_) | Err(Error::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
    }
    for d in &imported.databases {
        match databases::delete(c, key, &d.id) {
            Ok(_) | Err(Error::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
    }
    for s in &imported.sources {
        match git_sources::delete(c, s) {
            Ok(_) | Err(Error::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
    }
    for p in &imported.projects {
        match projects::delete(c, p) {
            Ok(_) | Err(Error::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn key() -> MasterKey {
        MasterKey::from_bytes(&[7u8; 32]).unwrap()
    }

    /// The shape of a real export (Coolify 4.4.6, the contract test's data).
    const EXPORT: &str = r#"{
      "version": "4.4.6",
      "projects": [{"uuid": "p1", "name": "shop", "description": "the shop", "environments": [{"id": 1, "name": "production"}, {"id": 2, "name": "staging"}]}],
      "servers": [{"name": "localhost", "ip": "host.docker.internal", "local": true}, {"name": "pi", "ip": "10.0.0.9", "local": false}],
      "sources": [
        {"key": "github:0", "kind": "public", "name": "Public GitHub", "html_url": "https://github.com"},
        {"key": "github:1", "kind": "github_app", "name": "my-coolify-app", "html_url": "https://github.com", "organization": "acme",
         "app_id": 1234, "client_id": "Iv1.x", "client_secret": "cs", "webhook_secret": "wh", "private_key": "-----BEGIN RSA PRIVATE KEY-----\nMII\n-----END RSA PRIVATE KEY-----"}
      ],
      "shared": [{"key": "API_HOST", "value": "api.shop.test", "type": "project", "project": "p1", "environment": null},
                 {"key": "REGION", "value": "eu", "type": "team", "project": null, "environment": null}],
      "databases": [
        {"uuid": "dbuuid000000000000000001", "name": "shopdb", "kind": "postgresql", "image": "postgres:16-alpine", "user": "postgres",
         "password": "pw", "database": "postgres", "public_port": null, "status": "running:healthy",
         "server": {"name": "localhost", "local": true}, "where": {"environment": "production", "project": "p1"},
         "volumes": [{"name": "postgres-data-dbuuid000000000000000001", "mount_path": "/var/lib/postgresql/data", "host_path": null}]},
        {"uuid": "dbuuid000000000000000002", "name": "cache", "kind": "redis", "image": "redis:7.2", "user": null,
         "password": "rpw", "database": null, "public_port": 6380, "status": "running:healthy",
         "server": {"name": "localhost", "local": true}, "where": {"environment": "production", "project": "p1"},
         "volumes": [{"name": "redis-data-x", "mount_path": "/data", "host_path": null}]},
        {"uuid": "dbuuid000000000000000003", "name": "far", "kind": "postgresql", "image": "postgres:16", "password": "x",
         "server": {"name": "pi", "ip": "10.0.0.9", "local": false}, "where": {"environment": "production", "project": "p1"}, "volumes": []}
      ],
      "apps": [
        {"uuid": "webuuid00000000000000001", "name": "Web Shop", "server": {"name": "localhost", "local": true},
         "where": {"environment": "production", "project": "p1"}, "build_pack": "dockerimage", "repository": "coollabsio/coolify",
         "image": "traefik/whoami", "image_tag": "latest", "ports": "80", "fqdn": "http://web.shop.test,https://www.shop.test/api",
         "memory": "512m", "cpus": "0", "auto_deploy": true, "previews": false,
         "basic_auth": {"user": "me", "password": "secret-password"},
         "envs": [
           {"key": "DATABASE_URL", "value": "postgres://postgres:pw@dbuuid000000000000000001:5432/postgres", "preview": false, "build": true, "runtime": true},
           {"key": "DATABASE_URL", "value": "postgres://postgres:pw@dbuuid000000000000000001:5432/postgres", "preview": true, "build": true, "runtime": true},
           {"key": "GREETING", "value": "hello", "preview": false, "build": false, "runtime": true},
           {"key": "GREETING", "value": "hello preview", "preview": true, "build": false, "runtime": true},
           {"key": "API", "value": "https://{{project.API_HOST}}/{{ team.REGION }}", "preview": false},
           {"key": "COOLIFY_URL", "value": "x", "preview": false},
           {"key": "bad key", "value": "x", "preview": false}
         ],
         "volumes": [{"name": "webuuid00000000000000001-uploads", "mount_path": "/uploads", "host_path": null}],
         "tasks": [{"name": "cleanup", "command": "echo cleaned", "frequency": "daily", "enabled": true}]},
        {"uuid": "siteuuid0000000000000001", "name": "site", "server": {"name": "localhost", "local": true},
         "where": {"environment": "staging", "project": "p1"}, "build_pack": "nixpacks", "repository": "acme/site", "branch": "main",
         "commit": "0123456789abcdef0123456789abcdef01234567", "source": "github:1", "base_directory": "/apps/web",
         "ports": "3000", "fqdn": "https://site.shop.test", "health_path": "/healthz", "watch_paths": "apps/web/**\n\n",
         "auto_deploy": false, "previews": true, "envs": [], "volumes": [], "tasks": []},
        {"uuid": "dockuuid0000000000000001", "name": "docs", "server": {"name": "localhost", "local": true},
         "where": {"environment": "production", "project": "p1"}, "build_pack": "dockerfile", "repository": "https://github.com/acme/docs",
         "dockerfile_location": "/docker/Dockerfile", "source": "github:0", "ports": "8080", "fqdn": "", "envs": [], "volumes": [], "tasks": []},
        {"uuid": "farapp000000000000000001", "name": "far-app", "server": {"name": "pi", "ip": "10.0.0.9", "local": false},
         "where": {"environment": "production", "project": "p1"}, "build_pack": "dockerimage", "image": "nginx", "envs": [], "volumes": [], "tasks": []}
      ],
      "services": [
        {"uuid": "svcuuid00000000000000001", "name": "Uptime Kuma", "where": {"environment": "production", "project": "p1"},
         "server": {"name": "localhost", "local": true}, "compose_raw": "services:\n  uptime-kuma:\n    image: louislam/uptime-kuma:1\n",
         "routes": [{"service": "uptime-kuma", "fqdn": "https://status.shop.test:3001"}],
         "envs": [{"key": "SERVICE_FQDN_UPTIME_KUMA_3001", "value": "https://status.shop.test", "preview": false}]},
        {"uuid": "svcuuid00000000000000002", "name": "it-tools-svcuuid00000000000000002", "where": {"environment": "production", "project": "p1"},
         "server": {"name": "localhost", "local": true}, "compose_raw": "services:\n  it-tools:\n    image: corentinth/it-tools\n",
         "routes": [{"service": "it-tools", "fqdn": "http://ittools-svc.127.0.0.1.sslip.io"}],
         "envs": [{"key": "SERVICE_URL_ITTOOLS", "value": "http://ittools-svc.127.0.0.1.sslip.io"},
                  {"key": "SERVICE_URL_ITTOOLS_80", "value": "http://ittools-svc.127.0.0.1.sslip.io:80"}]}
      ]
    }"#;

    fn export() -> Export {
        serde_json::from_str(EXPORT).unwrap()
    }

    #[test]
    fn helpers() {
        assert_eq!(slug("Web Shop!", "x"), "web-shop");
        assert_eq!(slug("---", "app-1"), "app-1");
        assert_eq!(slug(&"a".repeat(60), "x").len(), ids::MAX_NAME_LEN);
        let mut taken: HashSet<String> = ["web".to_string()].into();
        assert_eq!(unique("web", &mut taken), "web-2");
        assert_eq!(megabytes(Some("512m")), Some(512));
        assert_eq!(megabytes(Some("2g")), Some(2048));
        assert_eq!(megabytes(Some("0")), None);
        assert_eq!(megabytes(Some("1073741824")), Some(1024));
        assert_eq!(cpus(Some("0")), None);
        assert_eq!(cpus(Some("1.5")), Some(1.5));
        assert_eq!(cron("daily"), "0 0 * * *");
        assert_eq!(cron("*/5 * * * *"), "*/5 * * * *");
        assert_eq!(version_of("postgres:16-alpine"), "16");
        assert_eq!(version_of("redis:7.2"), "7.2");
        assert_eq!(version_of("mongo"), "latest");
        assert_eq!(repo_url("acme/site", None), "https://github.com/acme/site");
        assert_eq!(repo_url("git@github.com:a/b.git", None), "git@github.com:a/b.git");
        assert_eq!(
            parse_url("https://a.test:3000/api"),
            Some(("a.test".into(), true, Some(3000), "/api".into()))
        );
        let (v, missing) = resolve_shared(
            "{{project.A}}-{{team.B}}-{{x}}",
            Some("p"),
            "production",
            &[
                CShared {
                    key: "A".into(),
                    value: "1".into(),
                    kind: "project".into(),
                    project: Some("p".into()),
                    environment: None,
                },
                CShared {
                    key: "B".into(),
                    value: "2".into(),
                    kind: "team".into(),
                    project: None,
                    environment: None,
                },
            ],
        );
        assert_eq!(v, "1-2-{{x}}");
        assert_eq!(missing, vec!["x".to_string()]);
    }

    #[test]
    fn service_volumes_are_declared() {
        let raw = "services:\n  it-tools:\n    image: 'corentinth/it-tools:latest'\n    volumes:\n      - 'it-tools-data:/app/data'\n      - ./conf:/conf:ro\n      - /srv/x:/x\n    healthcheck:\n      test: [CMD, true]\n  db:\n    volumes:\n      - \"db-data:/var/lib/db\"\n      - it-tools-data:/shared\n";
        let out = declare_volumes(raw);
        assert!(out.starts_with(raw));
        assert!(out.ends_with("volumes:\n  it-tools-data: {}\n  db-data: {}\n"), "{out}");
        // Declared ones stay as they are; missing ones join the list.
        let partly = format!("{raw}volumes:\n  db-data:\n    driver: local\n");
        let out = declare_volumes(&partly);
        assert!(
            out.contains("volumes:\n  it-tools-data: {}\n  db-data:\n    driver: local\n"),
            "{out}"
        );
        assert_eq!(
            declare_volumes("services:\n  a:\n    image: x\n"),
            "services:\n  a:\n    image: x\n"
        );
    }

    #[test]
    fn the_plan_says_what_happens_and_changes_nothing() {
        let s = Store::open_in_memory().unwrap();
        let p = s.with(|c| plan(c, &export())).unwrap();
        let by = |n: &str| p.items.iter().find(|i| i.coolify_name == n).unwrap();
        assert_eq!(by("Web Shop").name, "web-shop");
        assert_eq!(by("Web Shop").action, Action::Create);
        assert!(by("Web Shop").notes.iter().any(|n| n.contains("volumes")));
        assert_eq!(by("site").summary, "git app, build detected (was Nixpacks)");
        assert_eq!(by("far-app").action, Action::Skip);
        assert!(by("far-app").notes[0].contains("runs on pi"));
        assert_eq!(by("far").action, Action::Skip);
        assert_eq!(by("cache").action, Action::Create);
        assert_eq!(by("Uptime Kuma").name, "uptime-kuma");
        assert_eq!(p.projects, vec![("shop".to_string(), true)]);
        assert!(p.warnings.iter().any(|w| w.contains("pi")));
        assert_eq!(p.creates(), 7);
        assert!(s.with(|c| crate::apps::list(c, None)).unwrap().is_empty());
    }

    #[test]
    fn apply_writes_every_record_and_remove_takes_them_back() {
        let s = Store::open_in_memory().unwrap();
        let k = key();
        let e = export();
        let p = s.with(|c| plan(c, &e)).unwrap();
        let imported = s.tx(|c| apply(c, &k, &e, &p, &|_| Ok(()))).unwrap();
        assert_eq!(imported.projects.len(), 1);
        assert_eq!(imported.sources.len(), 1, "the GitHub App, not the public source");
        assert_eq!(imported.databases.len(), 2);
        assert_eq!(imported.apps.len(), 5);

        s.with(|c| {
            let web = apps::find(c, "web-shop")?;
            assert_eq!(web.kind, AppKind::Image);
            assert_eq!(web.source.image.as_deref(), Some("traefik/whoami:latest"));
            assert_eq!(web.runtime.port, Some(80));
            assert_eq!(web.runtime.memory_mb, Some(512));
            let hosts: Vec<(String, bool)> = web.domains.iter().map(|d| (d.host.clone(), d.https)).collect();
            assert_eq!(
                hosts,
                vec![("web.shop.test".into(), false), ("www.shop.test".into(), true)]
            );
            assert_eq!(apps::basic_auth(c, &web.id)?.len(), 1);
            let vars = env::list(c, &k, VarScope::App, &web.id, true)?;
            let get = |key: &str, preview: bool| {
                vars.iter()
                    .find(|v| v.key == key && v.preview_only == preview)
                    .and_then(|v| v.value.clone())
            };
            assert_eq!(
                get("DATABASE_URL", false).as_deref(),
                Some("postgres://postgres:pw@${{db.shopdb.host}}:5432/postgres")
            );
            assert_eq!(get("DATABASE_URL", true), None, "same value: no preview copy");
            assert_eq!(get("GREETING", true).as_deref(), Some("hello preview"));
            assert_eq!(get("API", false).as_deref(), Some("https://api.shop.test/eu"));
            assert_eq!(get("COOLIFY_URL", false), None);
            assert!(vars.iter().all(|v| v.key != "bad key"));
            assert_eq!(
                databases::resolve_references(c, &k, &get("DATABASE_URL", false).unwrap())?,
                "postgres://postgres:pw@shopdb:5432/postgres"
            );
            let t = tasks::list(c, Some(&web.id))?;
            assert_eq!(t[0].schedule, "0 0 * * *");
            let d = deployments::list(c, &web.id, 5)?;
            assert_eq!(d[0].image, "traefik/whoami:latest");
            assert_eq!(d[0].commit_message, "Moved from Coolify");

            let site = apps::find(c, "site")?;
            assert_eq!(site.environment, "staging");
            assert_eq!(site.source.repository.as_deref(), Some("https://github.com/acme/site"));
            assert_eq!(site.source.git_source_id.as_deref(), Some(imported.sources[0].as_str()));
            assert_eq!(site.build.context.as_deref(), Some("apps/web"));
            assert_eq!(site.build.watch_paths, vec!["apps/web/**".to_string()]);
            assert!(site.previews.enabled);
            assert_eq!(
                site.runtime.health.as_ref().and_then(|h| h.path.as_deref()),
                Some("/healthz")
            );
            assert!(!git_watch::get(c, &site.id)?.enabled);
            let d = deployments::list(c, &site.id, 5)?;
            assert_eq!(d[0].image, "vfy/site:coolify");
            assert_eq!(d[0].commit_sha, "0123456789abcdef0123456789abcdef01234567");
            let gh = git_sources::github_app(c, &k, imported.sources[0].as_str())?.unwrap();
            assert_eq!(
                (gh.app_id, gh.html_url.as_str()),
                (1234, "https://github.com/apps/my-coolify-app")
            );
            assert_eq!(
                git_sources::webhook_secret(c, &k, &imported.sources[0])?
                    .as_deref()
                    .map(|s| s.as_str()),
                Some("wh")
            );

            let docs = apps::find(c, "docs")?;
            assert_eq!(docs.build.pack, BuildPack::Dockerfile);
            assert_eq!(docs.build.dockerfile.as_deref(), Some("docker/Dockerfile"));
            assert_eq!(docs.source.git_source_id, None);

            let kuma = apps::find(c, "uptime-kuma")?;
            assert_eq!(kuma.kind, AppKind::Compose);
            assert_eq!(kuma.runtime.service.as_deref(), Some("uptime-kuma"));
            assert_eq!(kuma.runtime.port, Some(3001));
            assert_eq!(kuma.domains[0].host, "status.shop.test");

            let tools = apps::find(c, "it-tools-svcuuid00000000000000002")?;
            assert_eq!(tools.runtime.port, Some(80), "from SERVICE_URL_ITTOOLS_80");
            assert_eq!(tools.domains[0].host, "ittools-svc.127.0.0.1.sslip.io");
            assert!(!tools.domains[0].https);

            let db = databases::get(c, &k, "shopdb", false)?;
            assert_eq!(db.image, "postgres:16-alpine");
            assert_eq!(db.version, "16");
            let cred = databases::credentials(c, &k, &db.id)?;
            assert_eq!(
                (cred.user.as_str(), cred.password.as_str(), cred.database.as_str()),
                ("postgres", "pw", "postgres")
            );
            let cache = databases::get(c, &k, "cache", false)?;
            assert_eq!((cache.public_port, cache.public_bind.as_str()), (Some(6380), "all"));
            assert_eq!(databases::credentials(c, &k, &cache.id)?.user, "default");
            Ok(())
        })
        .unwrap();
        let web = imported.apps.iter().find(|a| a.name == "web-shop").unwrap();
        assert_eq!(web.adopt, Adopt::Registry);
        assert_eq!(
            web.volumes,
            vec![VolumeCopy {
                from: "webuuid00000000000000001-uploads".into(),
                to: volumes::docker_name("web-shop", "uploads"),
            }]
        );
        assert_eq!(
            imported.databases[0].volume,
            Some(VolumeCopy {
                from: "postgres-data-dbuuid000000000000000001".into(),
                to: "vfy-db-shopdb-data".into(),
            })
        );
        assert_eq!(
            imported.apps.iter().find(|a| a.name == "site").unwrap().adopt,
            Adopt::Built {
                tag: "vfy/site:coolify".into()
            }
        );

        // A second plan sees the names taken.
        let again = s.with(|c| plan(c, &e)).unwrap();
        assert_eq!(
            again.items.iter().find(|i| i.coolify_name == "site").unwrap().name,
            "site-2"
        );
        assert_eq!(again.projects, vec![("shop".to_string(), false)]);

        s.tx(|c| remove(c, &k, &imported)).unwrap();
        // A password protection that can't be kept: stopped, nothing deployed.
        let mut short = e.clone();
        short.apps[0].basic_auth = Some(CBasicAuth {
            user: "me".into(),
            password: "short".into(),
        });
        short.apps.truncate(1);
        let p = s.with(|c| plan(c, &short)).unwrap();
        assert!(p
            .items
            .iter()
            .any(|i| i.notes.iter().any(|n| n.contains("taken over stopped"))));
        let got = s.tx(|c| apply(c, &k, &short, &p, &|_| Ok(()))).unwrap();
        let web = got.apps.iter().find(|a| a.name == "web-shop").unwrap();
        assert_eq!(web.deployment, None);
        assert_eq!(
            s.with(|c| apps::find(c, "web-shop")).unwrap().desired,
            vercelify_core::platform::Desired::Stopped
        );
        s.tx(|c| remove(c, &k, &got)).unwrap();
        s.with(|c| {
            assert!(apps::list(c, None)?.is_empty());
            assert!(databases::list(c, &k, None, false)?.is_empty());
            assert!(git_sources::list(c)?.is_empty());
            assert!(projects::list(c)?.iter().all(|p| p.name != "shop"));
            Ok(())
        })
        .unwrap();
    }
}

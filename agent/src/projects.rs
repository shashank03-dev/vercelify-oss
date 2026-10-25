//! Project configuration at work: where an app's `vercel.json` comes from,
//! keeping the proxy's routes in line with it, and running its cron jobs.
//!
//! The configuration comes from the repository at the deployed commit (read
//! through GitHub's API when auto-deploy deploys a branch or a pull request),
//! or is uploaded from the app or `vfy` (any Git host). An upload wins until
//! it is removed.
//!
//! Once a minute (and right after a change) the agent reads the labels of the
//! app's container, builds the routes (`routing.rs`) and writes one file per
//! app or preview into Coolify's proxy configuration folder, where Traefik
//! picks it up without a restart. Nothing runs when no app has rules.
//!
//! Cron jobs run like on Vercel: a GET to the production URL at the scheduled
//! minute (UTC) with `Authorization: Bearer $CRON_SECRET` when the app has
//! that variable, and `User-Agent: vercel-cron/1.0`. The request goes to the
//! proxy on this machine, so it works without public DNS and wakes a
//! sleeping app like any visitor.

use crate::config::Config;
use crate::error::AppError;
use crate::events::{Events, JobHandle, Jobs};
use crate::exec::run;
use crate::project::{self, Plan};
use crate::routing::{self, CoolifyRouter, Proxy};
use crate::watch::{Change, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use vercelify_core::{CheckStatus, CronInfo, ProjectInfo, ProjectSummary, ProjectsStatus};

const FILE_PREFIX: &str = "vercelify-routes-";
const MAX_FILE_BYTES: usize = 256 * 1024;
/// Like a Vercel function's longest duration.
const CRON_TIMEOUT_SECS: u64 = 300;
/// Failed runs in a row before an alert.
const CRON_ALERT_AFTER: u32 = 3;

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct CronState {
    last_run_at: Option<String>,
    last_status: Option<u16>,
    last_error: Option<String>,
    last_duration_ms: Option<u64>,
    #[serde(default)]
    failures: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Stored {
    key: String,
    app_uuid: String,
    pr: u64,
    source: String,
    /// (file name, text), in the order they apply.
    files: Vec<(String, String)>,
    commit: Option<String>,
    updated_at: String,
    /// `"<schedule> <path>"` → its last run.
    #[serde(default)]
    crons: BTreeMap<String, CronState>,
}

impl Stored {
    fn plan(&self, cfg: &Config) -> Result<Plan, String> {
        let files: Vec<(&str, &str)> = self.files.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
        let mut plan = project::parse(&files)?;
        if !cfg.rewrite_private_targets {
            project::drop_private_rewrites(&mut plan);
        }
        Ok(plan)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    projects: Vec<Stored>,
}

/// What the last reconcile found for one app or preview.
#[derive(Debug, Clone, Default)]
struct Live {
    routes_active: bool,
    hosts: Vec<String>,
    error: Option<String>,
    notes: Vec<String>,
    /// `https://host` to call cron jobs on.
    base_url: Option<String>,
    cron_secret: Option<String>,
}

pub fn key_for(app_uuid: &str, pr: u64) -> String {
    if pr == 0 {
        app_uuid.to_string()
    } else {
        format!("{app_uuid}-pr-{pr}")
    }
}

// --- pure helpers -----------------------------------------------------------

/// One of Coolify's app containers, from `docker inspect`.
#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub id: String,
    pub key: String,
    pub running: bool,
    /// Creation time (seconds since the epoch, UTC).
    pub created: i64,
    pub labels: BTreeMap<String, String>,
    pub env: Vec<String>,
}

/// `docker inspect` lines: id, running, created (RFC 3339, UTC), labels and
/// environment as JSON (tab-separated; JSON never holds a raw tab).
pub const INSPECT_FORMAT: &str =
    "{{.Id}}\t{{.State.Running}}\t{{.Created}}\t{{json .Config.Labels}}\t{{json .Config.Env}}";

pub fn parse_inspect(out: &str) -> Vec<Container> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(5, '\t').collect();
            if f.len() != 5 {
                return None;
            }
            let labels: BTreeMap<String, String> = serde_json::from_str(f[3]).ok()?;
            let uuid = labels.get("coolify.applicationUuid").filter(|u| !u.is_empty())?;
            let pr = labels
                .get("coolify.pullRequestId")
                .and_then(|p| p.parse().ok())
                .unwrap_or(0);
            Some(Container {
                id: f[0].to_string(),
                key: key_for(uuid, pr),
                running: f[1] == "true",
                created: chrono::DateTime::parse_from_rfc3339(f[2]).map_or(0, |t| t.timestamp()),
                env: serde_json::from_str(f[4]).unwrap_or_default(),
                labels,
            })
        })
        .collect()
}

/// The container whose labels count for each key: a running one, else the
/// newest (its labels are what Coolify will start next).
pub fn pick(containers: Vec<Container>) -> HashMap<String, Container> {
    let mut best: HashMap<String, Container> = HashMap::new();
    for c in containers {
        let better = best
            .get(&c.key)
            .is_none_or(|b| (c.running, c.created) > (b.running, b.created));
        if better {
            best.insert(c.key.clone(), c);
        }
    }
    best
}

/// `Host(`a`) || Host(`b`)` → [a, b].
pub fn hosts_in(rule: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = rule;
    while let Some(i) = rest.find("Host(`") {
        rest = &rest[i + 6..];
        if let Some(j) = rest.find('`') {
            out.push(rest[..j].to_ascii_lowercase());
            rest = &rest[j..];
        }
    }
    out
}

/// Where cron jobs call the app: its https address when it has one.
pub fn cron_base(routers: &[CoolifyRouter]) -> Option<String> {
    let pick = routers.iter().find(|r| r.tls.is_some()).or_else(|| routers.first())?;
    let host = hosts_in(&pick.rule).into_iter().next()?;
    let scheme = if pick.tls.is_some() { "https" } else { "http" };
    Some(format!("{scheme}://{host}"))
}

/// curl's config for one cron run: the proxy on this machine answers for the
/// app's host (its certificate may be for the public name: not verified, the
/// connection never leaves the machine).
pub fn curl_config(base: &str, path: &str, secret: Option<&str>, http_port: u16, https_port: u16) -> String {
    let host = base.split("://").nth(1).unwrap_or("");
    let clean = |s: &str| s.replace(['"', '\\', '\n', '\r'], "");
    let mut c = format!(
        "url = \"{}{}\"\nuser-agent = \"vercel-cron/1.0\"\nconnect-to = \"{host}:80:127.0.0.1:{http_port}\"\nconnect-to = \"{host}:443:127.0.0.1:{https_port}\"\n",
        clean(base),
        clean(path)
    );
    if let Some(s) = secret {
        c.push_str(&format!("header = \"Authorization: Bearer {}\"\n", clean(s)));
    }
    c
}

/// `KEY=value` from a container's environment list.
pub fn env_value(env: &[String], key: &str) -> Option<String> {
    env.iter()
        .find_map(|e| e.strip_prefix(key)?.strip_prefix('=').map(String::from))
        .filter(|v| !v.is_empty())
}

/// Uploaded files: known names only, small, valid. Returned in the order
/// they apply (vercel.json first).
pub fn check_upload(files: &[(String, String)]) -> Result<Vec<(String, String)>, String> {
    if files.is_empty() {
        return Err("no files".into());
    }
    let mut out = Vec::new();
    for name in project::FILES {
        let mut found = files.iter().filter(|(n, _)| n == name);
        if let Some((n, t)) = found.next() {
            if found.next().is_some() {
                return Err(format!("{name} sent twice"));
            }
            if t.len() > MAX_FILE_BYTES {
                return Err(format!("{name} is larger than {} KB", MAX_FILE_BYTES / 1024));
            }
            out.push((n.clone(), t.clone()));
        }
    }
    if let Some((n, _)) = files.iter().find(|(n, _)| !project::FILES.contains(&n.as_str())) {
        return Err(format!("{n}: only vercel.json and vercelify.json are read"));
    }
    let refs: Vec<(&str, &str)> = out.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
    project::merge(&refs)?;
    Ok(out)
}

pub fn summary(plan: &Plan) -> ProjectSummary {
    ProjectSummary {
        redirects: plan.redirects.len() as u32,
        rewrites: plan.rewrites.len() as u32,
        headers: plan.headers.len() as u32,
        firewall_rules: (plan.firewall.rules.len()
            + usize::from(!plan.firewall.block.is_empty())
            + usize::from(!plan.firewall.trusted.is_empty())) as u32,
        clean_urls: plan.clean_urls,
        trailing_slash: plan.trailing_slash,
        crons: plan
            .crons
            .iter()
            .map(|c| CronInfo {
                path: c.path.clone(),
                schedule: c.schedule.clone(),
                next_run_at: crate::cron::parse(&c.schedule)
                    .ok()
                    .and_then(|s| s.next_after(chrono::Utc::now()))
                    .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                last_run_at: None,
                last_status: None,
                last_error: None,
                last_duration_ms: None,
                running: false,
            })
            .collect(),
        warnings: plan.warnings.clone(),
    }
}

fn cron_id(schedule: &str, path: &str) -> String {
    format!("{schedule} {path}")
}

// --- the service ------------------------------------------------------------

pub struct Projects {
    cfg: Arc<Config>,
    store: Mutex<Store>,
    live: Mutex<HashMap<String, Live>>,
    proxy_image: Mutex<Option<String>>,
    error: Mutex<Option<String>>,
    /// `<key> <schedule> <path>` of cron runs in progress.
    running: Mutex<HashSet<String>>,
    /// One reconcile at a time.
    reconciling: Mutex<()>,
    events: Arc<Events>,
    jobs: Arc<Jobs>,
    watcher: OnceLock<Arc<Watcher>>,
    /// The platform engine, when this server runs it instead of Coolify.
    engine: OnceLock<Arc<vercelify_platform::engine::Engine>>,
}

/// A platform app's base routers and its `CRON_SECRET`.
type PlatformRoutes = (Vec<CoolifyRouter>, Option<String>);

/// The base routers of a platform app or preview, in the shape the
/// `vercel.json` layer copies (it sits above them at higher priorities).
pub fn platform_routers(route: &vercelify_platform::proxy::Route) -> Vec<CoolifyRouter> {
    use vercelify_platform::proxy::{router_name, service_name, RESOLVER};
    let auth = (!route.basic_auth.is_empty()).then(|| format!("vfy-{}-auth@file", route.key));
    route
        .hosts
        .iter()
        .enumerate()
        .filter(|(_, h)| h.redirect_to.is_none())
        .map(|(i, h)| CoolifyRouter {
            name: router_name(&route.key, i, h.https),
            rule: format!("Host(`{}`)", h.host),
            entry_points: vec![if h.https { "websecure" } else { "web" }.to_string()],
            service: format!("{}@file", service_name(&route.key)),
            middlewares: auth.iter().cloned().collect(),
            tls: h.https.then(|| serde_json::json!({ "certResolver": RESOLVER })),
        })
        .collect()
}

impl Projects {
    fn path(cfg: &Config) -> PathBuf {
        cfg.state_dir.join("projects.json")
    }

    fn dir(&self) -> PathBuf {
        self.cfg.proxy_dir.join("dynamic")
    }

    pub fn load(cfg: Arc<Config>, events: Arc<Events>, jobs: Arc<Jobs>) -> Self {
        let store = std::fs::read_to_string(Self::path(&cfg))
            .ok()
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default();
        Self {
            cfg,
            store: Mutex::new(store),
            live: Mutex::new(HashMap::new()),
            proxy_image: Mutex::new(None),
            error: Mutex::new(None),
            running: Mutex::new(HashSet::new()),
            reconciling: Mutex::new(()),
            events,
            jobs,
            watcher: OnceLock::new(),
            engine: OnceLock::new(),
        }
    }

    /// Route the platform engine's apps instead of Coolify's.
    pub fn set_engine(&self, e: Arc<vercelify_platform::engine::Engine>) {
        let _ = self.engine.set(e);
    }

    /// Routers and `CRON_SECRET` per key, from the platform's database.
    fn platform_routes(&self, engine: &vercelify_platform::engine::Engine) -> Option<HashMap<String, PlatformRoutes>> {
        use vercelify_platform::{apps, deployments, env, previews, reconcile};
        let res = engine.store.with(|c| {
            let settings = apps::domain_settings(c)?;
            let mut out = HashMap::new();
            for app in apps::list(c, None)? {
                let mut deps: Vec<(u64, String)> = vec![];
                if let Some(d) = &app.current_deployment_id {
                    deps.push((0, d.clone()));
                }
                for p in previews::list(c, &app.id, true)? {
                    if let Some(d) = p.deployment_id {
                        deps.push((u64::from(p.pr_number), d));
                    }
                }
                for (pr, dep) in deps {
                    let d = deployments::get(c, &dep)?;
                    let container = deployments::container(c, &dep)?;
                    if container.is_empty() {
                        continue;
                    }
                    let Some(route) = reconcile::route_for(c, &settings, &app, &d, &container)? else {
                        continue;
                    };
                    let secret = env::resolve(c, &engine.key, &app.id, pr > 0)?
                        .get("CRON_SECRET")
                        .map(|v| v.value.to_string());
                    out.insert(key_for(&app.id, pr), (platform_routers(&route), secret));
                }
            }
            Ok(out)
        });
        match res {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!("projects: reading platform routes: {e}");
                None
            }
        }
    }

    pub fn set_watcher(&self, w: Arc<Watcher>) {
        let _ = self.watcher.set(w);
    }

    fn save(&self, store: &Store) {
        // Uploaded files may carry IPs and paths the owner keeps private.
        if let Err(e) =
            crate::watch::write_private(&Self::path(&self.cfg), &serde_json::to_vec(store).unwrap_or_default())
        {
            tracing::warn!("projects: could not save: {e}");
        }
    }

    async fn info(&self, s: &Stored) -> ProjectInfo {
        let live = self.live.lock().await.get(&s.key).cloned().unwrap_or_default();
        let running = self.running.lock().await.clone();
        let (mut summary, error) = match s.plan(&self.cfg) {
            Ok(plan) => (summary(&plan), live.error.clone()),
            Err(e) => (ProjectSummary::default(), Some(e)),
        };
        summary.warnings.extend(live.notes.iter().cloned());
        for c in &mut summary.crons {
            if let Some(st) = s.crons.get(&cron_id(&c.schedule, &c.path)) {
                c.last_run_at = st.last_run_at.clone();
                c.last_status = st.last_status;
                c.last_error = st.last_error.clone();
                c.last_duration_ms = st.last_duration_ms;
            }
            c.running = running.contains(&format!("{} {}", s.key, cron_id(&c.schedule, &c.path)));
            if s.pr != 0 {
                c.next_run_at = None;
            }
        }
        ProjectInfo {
            key: s.key.clone(),
            app_uuid: s.app_uuid.clone(),
            pull_request_id: s.pr,
            source: s.source.clone(),
            files: s.files.iter().map(|(n, _)| n.clone()).collect(),
            commit: s.commit.clone(),
            updated_at: s.updated_at.clone(),
            summary,
            routes_active: live.routes_active,
            hosts: live.hosts,
            error,
        }
    }

    pub async fn status(&self) -> ProjectsStatus {
        let projects = self.store.lock().await.projects.clone();
        let mut out = Vec::new();
        for p in &projects {
            out.push(self.info(p).await);
        }
        ProjectsStatus {
            projects: out,
            proxy_image: self.proxy_image.lock().await.clone(),
            error: self.error.lock().await.clone(),
        }
    }

    pub async fn get(&self, key: &str) -> Option<ProjectInfo> {
        let s = self
            .store
            .lock()
            .await
            .projects
            .iter()
            .find(|p| p.key == key)
            .cloned()?;
        Some(self.info(&s).await)
    }

    async fn put(&self, app_uuid: &str, pr: u64, source: &str, files: Vec<(String, String)>, commit: Option<String>) {
        let key = key_for(app_uuid, pr);
        {
            let mut store = self.store.lock().await;
            // Keep the history of cron jobs that are still in the file.
            let refs: Vec<(&str, &str)> = files.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
            let still: HashSet<String> = project::parse(&refs)
                .map(|p| p.crons.iter().map(|c| cron_id(&c.schedule, &c.path)).collect())
                .unwrap_or_default();
            let crons = store
                .projects
                .iter()
                .find(|p| p.key == key)
                .map(|p| p.crons.clone())
                .unwrap_or_default()
                .into_iter()
                .filter(|(id, _)| still.contains(id))
                .collect();
            store.projects.retain(|p| p.key != key);
            store.projects.push(Stored {
                key,
                app_uuid: app_uuid.into(),
                pr,
                source: source.into(),
                files,
                commit,
                updated_at: now(),
                crons,
            });
            store.projects.sort_by(|a, b| a.key.cmp(&b.key));
            self.save(&store);
        }
        self.tick().await;
        self.events.changed("projects");
    }

    /// Upload from the app or `vfy` (production).
    pub async fn upload(&self, app_uuid: &str, files: Vec<(String, String)>) -> Result<ProjectInfo, AppError> {
        if !crate::autodeploy::valid_app_uuid(app_uuid) {
            return Err(AppError::bad_request("invalid app uuid"));
        }
        let files = check_upload(&files).map_err(AppError::bad_request)?;
        self.put(app_uuid, 0, "upload", files, None).await;
        self.get(app_uuid)
            .await
            .ok_or_else(|| AppError::internal("lost the configuration"))
    }

    /// What the repository holds at a deployed commit. An upload for the
    /// app is kept; no files at all removes the repository's old rules.
    pub async fn set_from_repo(&self, app_uuid: &str, pr: u64, files: Vec<(String, String)>, commit: &str) {
        let key = key_for(app_uuid, pr);
        let existing = self.store.lock().await.projects.iter().find(|p| p.key == key).cloned();
        if existing.as_ref().is_some_and(|p| p.source == "upload") {
            return;
        }
        if files.is_empty() {
            if existing.is_some() {
                self.remove(&key).await;
            }
            return;
        }
        let unchanged = existing
            .as_ref()
            .is_some_and(|p| p.files == files && p.commit.as_deref() == Some(commit));
        if !unchanged {
            self.put(app_uuid, pr, "repo", files, Some(commit.to_string())).await;
        }
    }

    pub async fn remove(&self, key: &str) -> bool {
        let removed = {
            let mut store = self.store.lock().await;
            let before = store.projects.len();
            store.projects.retain(|p| p.key != key);
            let removed = store.projects.len() != before;
            if removed {
                self.save(&store);
            }
            removed
        };
        if removed {
            self.live.lock().await.remove(key);
            self.tick().await;
            self.events.changed("projects");
        }
        removed
    }

    /// Remove an app's rules and those of all its previews.
    pub async fn remove_app(&self, app_uuid: &str) -> bool {
        let keys: Vec<String> = self
            .store
            .lock()
            .await
            .projects
            .iter()
            .filter(|p| p.app_uuid == app_uuid)
            .map(|p| p.key.clone())
            .collect();
        let mut any = false;
        for k in keys {
            any |= self.remove(&k).await;
        }
        any
    }

    pub async fn preview_keys(&self, app_uuid: &str) -> Vec<(u64, String)> {
        self.store
            .lock()
            .await
            .projects
            .iter()
            .filter(|p| p.app_uuid == app_uuid && p.pr != 0)
            .map(|p| (p.pr, p.key.clone()))
            .collect()
    }

    // --- proxy routes -------------------------------------------------------

    async fn docker(&self, args: &[&str]) -> Option<String> {
        run(&self.cfg.docker_bin, args, Duration::from_secs(20))
            .await
            .ok()
            .filter(|o| o.success())
            .map(|o| o.stdout)
    }

    /// The chosen container for each key. None: Docker couldn't be asked
    /// (then nothing is changed).
    async fn containers(&self) -> Option<HashMap<String, Container>> {
        let ids = self
            .docker(&["ps", "-aq", "--no-trunc", "--filter", "label=coolify.applicationUuid"])
            .await?;
        let ids: Vec<&str> = ids.split_whitespace().collect();
        if ids.is_empty() {
            return Some(HashMap::new());
        }
        let mut args = vec!["inspect", "--format", INSPECT_FORMAT];
        args.extend(ids);
        // A container removed since `ps` (a deploy finishing) fails the
        // command but the others are still printed.
        let out = run(&self.cfg.docker_bin, &args, Duration::from_secs(20)).await.ok()?;
        let found = parse_inspect(&out.stdout);
        if found.is_empty() && !out.success() {
            return None;
        }
        Some(pick(found))
    }

    /// The proxy's image and the subnets of its Docker networks (None: they
    /// couldn't be read).
    async fn proxy_info(&self) -> (Option<String>, Option<Vec<project::Net>>) {
        let Some(out) = self
            .docker(&[
                "inspect",
                "-f",
                "{{.Config.Image}}\t{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}",
                &self.cfg.proxy_container,
            ])
            .await
        else {
            return (None, None);
        };
        let (image, networks) = out.trim().split_once('\t').unwrap_or((out.trim(), ""));
        let image = (!image.is_empty()).then(|| image.to_string());
        let networks: Vec<&str> = networks.split_whitespace().filter(|n| *n != "host").collect();
        if networks.is_empty() {
            return (image, Some(vec![]));
        }
        let mut args = vec!["network", "inspect", "-f", "{{range .IPAM.Config}}{{.Subnet}} {{end}}"];
        args.extend(networks);
        let subnets = self.docker(&args).await.map(|o| routing::parse_subnets(&o));
        (image, subnets)
    }

    fn write_file(&self, key: &str, text: Option<String>) -> Result<bool, String> {
        let dir = self.dir();
        let path = dir.join(format!("{FILE_PREFIX}{key}.yaml"));
        match text {
            Some(t) => {
                if std::fs::read_to_string(&path).ok().as_deref() == Some(t.as_str()) {
                    return Ok(true);
                }
                if !dir.is_dir() {
                    return Err(format!(
                        "Coolify's proxy folder {} is missing — open Coolify → Servers → Proxy once",
                        dir.display()
                    ));
                }
                // Traefik only reads .yaml/.yml/.toml: the temporary name is skipped.
                let tmp = dir.join(format!(".{FILE_PREFIX}{key}.tmp"));
                std::fs::write(&tmp, t.as_bytes())
                    .and_then(|_| std::fs::rename(&tmp, &path))
                    .map_err(|e| format!("writing {}: {e}", path.display()))?;
                Ok(true)
            }
            None => {
                let _ = std::fs::remove_file(&path);
                Ok(false)
            }
        }
    }

    /// Our files in the proxy folder, by key.
    fn written_keys(&self) -> Vec<String> {
        std::fs::read_dir(self.dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                Some(n.strip_prefix(FILE_PREFIX)?.strip_suffix(".yaml")?.to_string())
            })
            .collect()
    }

    /// Bring the proxy's routes in line with the stored configurations.
    pub async fn tick(&self) {
        let _one = self.reconciling.lock().await;
        let projects = self.store.lock().await.projects.clone();
        if projects.is_empty() {
            for k in self.written_keys() {
                let _ = self.write_file(&k, None);
            }
            self.live.lock().await.clear();
            *self.error.lock().await = None;
            return;
        }
        // Rules of apps that no longer have any.
        for k in self.written_keys() {
            if !projects.iter().any(|p| p.key == k) {
                let _ = self.write_file(&k, None);
            }
        }
        let (image, subnets) = self.proxy_info().await;
        *self.proxy_image.lock().await = image.clone();
        let version = image.as_deref().and_then(routing::traefik_version);
        let proxy = Proxy {
            status_rewrites: version.is_some_and(|v| v >= (3, 5)),
            cloudflare: self.cfg.install_value("domain_mode").as_deref() == Some("cloudflare"),
            local: routing::local_nets(subnets.as_deref()),
        };
        let too_old = version.is_some_and(|v| v.0 < 3);
        *self.error.lock().await = if too_old {
            Some(format!(
                "Coolify's proxy runs {}; project rules need Traefik 3 (Coolify → Servers → Proxy → upgrade)",
                image.clone().unwrap_or_default()
            ))
        } else if image.is_none() {
            Some("the proxy isn't running; rules apply once it starts".into())
        } else {
            None
        };
        let platform = match self.engine.get() {
            Some(e) => match self.platform_routes(e) {
                Some(m) => Some(m),
                None => {
                    *self.error.lock().await =
                        Some("can't read the platform's apps right now; routes are unchanged".into());
                    return;
                }
            },
            None => None,
        };
        let containers = if platform.is_some() {
            HashMap::new()
        } else {
            match self.containers().await {
                Some(c) => c,
                None => {
                    // Leave the routes as they are rather than drop them all.
                    *self.error.lock().await =
                        Some("can't read Docker's containers right now; routes are unchanged".into());
                    return;
                }
            }
        };
        let mut live = HashMap::new();
        for p in &projects {
            let mut l = Live::default();
            let plan = match p.plan(&self.cfg) {
                Ok(plan) => plan,
                Err(e) => {
                    l.error = Some(e);
                    let _ = self.write_file(&p.key, None);
                    live.insert(p.key.clone(), l);
                    continue;
                }
            };
            let from_platform = platform.as_ref().map(|m| m.get(&p.key));
            let (routers, skipped) = match (from_platform, containers.get(&p.key)) {
                (Some(Some((routers, secret))), _) => {
                    l.cron_secret = secret.clone();
                    (routers.clone(), vec![])
                }
                (Some(None), _) => {
                    l.error = Some("this app isn't deployed yet (or it runs on another server)".into());
                    (vec![], vec![])
                }
                (None, Some(c)) => {
                    l.cron_secret = env_value(&c.env, "CRON_SECRET");
                    routing::coolify_routers(&c.labels)
                }
                (None, None) => {
                    l.error = Some(
                        "no container for this app on this server yet (deploy it, or it runs on another server)".into(),
                    );
                    (vec![], vec![])
                }
            };
            l.notes.extend(skipped);
            l.hosts = routers.iter().flat_map(|r| hosts_in(&r.rule)).collect();
            l.hosts.sort();
            l.hosts.dedup();
            l.base_url = cron_base(&routers);
            if !routers.is_empty() && plan.routes_anything() && too_old {
                let _ = self.write_file(&p.key, None);
            } else {
                let (config, notes) = routing::config(&p.key, &routers, &plan, &proxy);
                l.notes.extend(notes);
                match self.write_file(&p.key, config.as_ref().map(routing::render)) {
                    Ok(active) => l.routes_active = active,
                    Err(e) => l.error = Some(e),
                }
            }
            let mut seen = HashSet::new();
            l.notes.retain(|n| seen.insert(n.clone()));
            live.insert(p.key.clone(), l);
        }
        *self.live.lock().await = live;
    }

    /// Background loop: reconcile every minute.
    pub async fn run(self: Arc<Self>) {
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            self.tick().await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }

    // --- cron jobs ----------------------------------------------------------

    fn shutting_down(&self) -> bool {
        std::fs::read_to_string(&self.cfg.power_guard_state)
            .ok()
            .and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok())
            .and_then(|v| v.get("state")?.as_str().map(String::from))
            .is_some_and(|s| s == "shutting_down")
    }

    /// Background loop: at the start of every minute, run what is due.
    pub async fn run_crons(self: Arc<Self>) {
        use chrono::Timelike;
        let mut last_minute = None;
        loop {
            let now = chrono::Utc::now();
            let wait = 60 - u64::from(now.second()) + 1;
            tokio::time::sleep(Duration::from_secs(wait)).await;
            let now = chrono::Utc::now();
            // Once per minute, even if the clock was set back.
            let minute = now.timestamp() / 60;
            if last_minute == Some(minute) || self.shutting_down() {
                continue;
            }
            last_minute = Some(minute);
            let projects = self.store.lock().await.projects.clone();
            for p in projects.iter().filter(|p| p.pr == 0) {
                let Ok(plan) = p.plan(&self.cfg) else { continue };
                for c in plan.crons {
                    if crate::cron::parse(&c.schedule).is_ok_and(|s| s.matches(now)) {
                        let me = self.clone();
                        let key = p.key.clone();
                        tokio::spawn(async move {
                            let _ = me.run_cron(&key, &c.schedule, &c.path, None).await;
                        });
                    }
                }
            }
        }
    }

    /// Start a cron job by hand; it shows in the activity list.
    pub async fn run_now(self: &Arc<Self>, app_uuid: &str, path: &str) -> Result<vercelify_core::Job, AppError> {
        let stored = self
            .store
            .lock()
            .await
            .projects
            .iter()
            .find(|p| p.key == app_uuid)
            .cloned()
            .ok_or_else(|| AppError::not_found("this app has no project configuration"))?;
        let plan = stored.plan(&self.cfg).map_err(AppError::conflict)?;
        let cron = plan
            .crons
            .into_iter()
            .find(|c| c.path == path)
            .ok_or_else(|| AppError::not_found(format!("no cron job for {path}")))?;
        let job = self.jobs.start("cron", format!("Run {path}"), Some(app_uuid));
        let snapshot = job.snapshot();
        let me = self.clone();
        let key = stored.key.clone();
        tokio::spawn(async move {
            let _ = me.run_cron(&key, &cron.schedule, &cron.path, Some(job)).await;
        });
        snapshot.ok_or_else(|| AppError::internal("job not recorded"))
    }

    async fn run_cron(&self, key: &str, schedule: &str, path: &str, job: Option<JobHandle>) -> Result<u16, String> {
        let id = cron_id(schedule, path);
        let running_id = format!("{key} {id}");
        if !self.running.lock().await.insert(running_id.clone()) {
            let msg = "the previous run is still going".to_string();
            if let Some(j) = job {
                j.fail(msg.clone());
            }
            return Err(msg);
        }
        self.events.changed("projects");
        let started = Instant::now();
        let at = now();
        let live = self.live.lock().await.get(key).cloned().unwrap_or_default();
        let result = match live.base_url.as_deref() {
            None => Err("the app has no address on this server (is it deployed here?)".to_string()),
            Some(base) => self.call(base, path, live.cron_secret.as_deref()).await,
        };
        self.running.lock().await.remove(&running_id);
        let ms = started.elapsed().as_millis() as u64;
        let ok = matches!(result, Ok(code) if (200..300).contains(&code));
        let failures = {
            let mut store = self.store.lock().await;
            let Some(p) = store.projects.iter_mut().find(|p| p.key == key) else {
                return result;
            };
            let st = p.crons.entry(id).or_default();
            st.last_run_at = Some(at);
            st.last_duration_ms = Some(ms);
            match &result {
                Ok(code) => {
                    st.last_status = Some(*code);
                    st.last_error = (!ok).then(|| format!("answered HTTP {code}"));
                }
                Err(e) => {
                    st.last_status = None;
                    st.last_error = Some(e.clone());
                }
            }
            st.failures = if ok { 0 } else { st.failures.saturating_add(1) };
            let f = st.failures;
            self.save(&store);
            f
        };
        self.events.changed("projects");
        if let Some(j) = job {
            match &result {
                Ok(code) if ok => j.done(format!("HTTP {code} in {ms} ms")),
                Ok(code) => j.fail(format!("HTTP {code}")),
                Err(e) => j.fail(e.clone()),
            }
        }
        if let Some(w) = self.watcher.get() {
            let source = format!("cron:{key}:{path}");
            if failures == CRON_ALERT_AFTER {
                w.raise(Change {
                    source,
                    status: CheckStatus::Bad,
                    title: format!("Cron job {path} keeps failing"),
                    detail: format!(
                        "{CRON_ALERT_AFTER} runs in a row failed; the last: {}",
                        match &result {
                            Ok(c) => format!("HTTP {c}"),
                            Err(e) => e.clone(),
                        }
                    ),
                    resolved: false,
                })
                .await;
            } else if ok {
                w.resolve(
                    &source,
                    format!("Cron job {path} works again"),
                    "The last run succeeded.".into(),
                )
                .await;
            }
        }
        result
    }

    async fn call(&self, base: &str, path: &str, secret: Option<&str>) -> Result<u16, String> {
        let conf = curl_config(base, path, secret, self.cfg.proxy_http_port, self.cfg.proxy_https_port);
        let mut random = [0u8; 6];
        let _ = getrandom::fill(&mut random);
        let file = self.cfg.run_dir.join(format!(
            "cron-{}.curl",
            random.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ));
        crate::watch::write_private(&file, conf.as_bytes()).map_err(|e| format!("writing {}: {e}", file.display()))?;
        let f = file.to_string_lossy().to_string();
        let timeout = CRON_TIMEOUT_SECS.to_string();
        let out = run(
            &self.cfg.curl_bin,
            &[
                // The proxy on this machine, never one from the environment.
                "--noproxy",
                "*",
                "-sS",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "-k",
                "-L",
                "--max-redirs",
                "3",
                "--proto",
                "=http,https",
                "-m",
                &timeout,
                "-K",
                &f,
            ],
            Duration::from_secs(CRON_TIMEOUT_SECS + 10),
        )
        .await;
        let _ = std::fs::remove_file(&file);
        let out = out.map_err(|e| e.to_string())?;
        match out.stdout.trim().parse::<u16>() {
            Ok(code) if code > 0 => Ok(code),
            _ => Err(format!("no answer: {}", out.stderr.trim())),
        }
    }
}

/// Fetch the project files of an app from GitHub at a commit or branch.
pub async fn fetch_github(
    cfg: &Config,
    slug: &str,
    base_dir: &str,
    git_ref: &str,
    token: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for name in project::FILES {
        let path = crate::github::repo_path(base_dir, name).ok_or("invalid base directory")?;
        if let Some(text) = crate::github::file_at(cfg, slug, &path, git_ref, token).await? {
            if text.len() > MAX_FILE_BYTES {
                return Err(format!("{name} is larger than {} KB", MAX_FILE_BYTES / 1024));
            }
            out.push((name.to_string(), text));
        }
    }
    if out.is_empty() {
        // No file, or no access (an expired token): only the first may
        // remove an app's rules.
        crate::github::readable(cfg, slug, token).await?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn picks_running_then_newest() {
        let line = |id: &str, running: bool, created: &str, uuid: &str, pr: &str| {
            format!(
                "{id}\t{running}\t{created}\t{{\"coolify.applicationUuid\":\"{uuid}\",\"coolify.pullRequestId\":\"{pr}\",\"x\":\"a\\tb\"}}\t[\"CRON_SECRET=s\"]"
            )
        };
        let out = [
            line("a1", false, "2026-10-20T10:00:00.5Z", "u1", ""),
            line("a2", true, "2026-10-19T10:00:00Z", "u1", "0"),
            line("a3", false, "2026-10-21T10:00:00Z", "u1", ""),
            // Docker prints a variable number of fraction digits: compared as times.
            line("b1", false, "2026-10-21T10:00:00.9Z", "u1", "7"),
            line("b2", false, "2026-10-21T10:00:00.12345Z", "u1", "7"),
            line("c1", true, "2026-10-21T10:00:00Z", "", ""),
            "garbage".to_string(),
        ]
        .join("\n");
        let found = parse_inspect(&out);
        assert_eq!(found.len(), 5);
        assert_eq!(found[0].env, vec!["CRON_SECRET=s"]);
        assert_eq!(found[0].labels.get("x").map(String::as_str), Some("a\tb"));
        let p = pick(found);
        assert_eq!(p.get("u1").map(|c| c.id.as_str()), Some("a2"));
        // Same second: the later fraction wins regardless of its length.
        assert_eq!(p.get("u1-pr-7").map(|c| c.id.as_str()), Some("b1"));
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn platform_base_routers() {
        use vercelify_platform::proxy::{Route, RouteHost};
        let r = Route {
            key: "blog".into(),
            hosts: vec![
                RouteHost {
                    host: "blog.example.com".into(),
                    https: true,
                    redirect_to: None,
                },
                RouteHost {
                    host: "www.example.com".into(),
                    https: true,
                    redirect_to: Some("blog.example.com".into()),
                },
                RouteHost {
                    host: "blog.lan".into(),
                    https: false,
                    redirect_to: None,
                },
            ],
            upstreams: vec!["http://vfy-blog-1:3000".into()],
            basic_auth: vec!["me:x".into()],
            dns_challenge: false,
        };
        let routers = platform_routers(&r);
        assert_eq!(routers.len(), 2, "redirect hosts are not copied");
        assert_eq!(routers[0].name, "vfy-blog-0-secure");
        assert_eq!(routers[0].service, "vfy-blog@file");
        assert_eq!(routers[0].entry_points, ["websecure"]);
        assert_eq!(routers[0].middlewares, ["vfy-blog-auth@file"]);
        assert!(routers[0].tls.is_some());
        assert_eq!(routers[1].name, "vfy-blog-2");
        assert!(routers[1].tls.is_none());
        assert_eq!(cron_base(&routers).as_deref(), Some("https://blog.example.com"));
    }

    #[test]
    fn hosts_and_cron_address() {
        assert_eq!(
            hosts_in("(Host(`A.test`) || Host(`b.test`)) && PathPrefix(`/`)"),
            vec!["a.test", "b.test"]
        );
        let r = |tls: bool, host: &str| CoolifyRouter {
            name: "x".into(),
            rule: format!("Host(`{host}`) && PathPrefix(`/`)"),
            entry_points: vec![],
            service: "s@docker".into(),
            middlewares: vec![],
            tls: tls.then(|| json!({})),
        };
        assert_eq!(
            cron_base(&[r(false, "h.test"), r(true, "s.test")]).as_deref(),
            Some("https://s.test")
        );
        assert_eq!(cron_base(&[r(false, "h.test")]).as_deref(), Some("http://h.test"));
        assert_eq!(cron_base(&[]), None);
    }

    #[test]
    fn cron_requests_go_to_the_local_proxy() {
        let c = curl_config("https://app.test", "/api/cron?x=\"1\"", Some("s3cret"), 80, 8443);
        assert!(c.contains("url = \"https://app.test/api/cron?x=1\""));
        assert!(c.contains("connect-to = \"app.test:443:127.0.0.1:8443\""));
        assert!(c.contains("header = \"Authorization: Bearer s3cret\""));
        assert!(c.contains("user-agent = \"vercel-cron/1.0\""));
        assert!(!curl_config("http://a", "/", None, 80, 443).contains("Authorization"));
        let env = vec!["PATH=/bin".to_string(), "CRON_SECRET=abc=def".into(), "EMPTY=".into()];
        assert_eq!(env_value(&env, "CRON_SECRET").as_deref(), Some("abc=def"));
        assert_eq!(env_value(&env, "EMPTY"), None);
        assert_eq!(env_value(&env, "CRON"), None);
    }

    #[test]
    fn uploads() {
        let f = |n: &str, t: &str| (n.to_string(), t.to_string());
        let ok = check_upload(&[f("vercelify.json", "{}"), f("vercel.json", r#"{"cleanUrls":true}"#)]).unwrap();
        assert_eq!(ok[0].0, "vercel.json");
        assert_eq!(ok[1].0, "vercelify.json");
        assert!(check_upload(&[]).is_err());
        assert!(check_upload(&[f("now.json", "{}")]).is_err());
        assert!(check_upload(&[f("vercel.json", "{")]).is_err());
        assert!(check_upload(&[f("vercel.json", "{}"), f("vercel.json", "{}")]).is_err());
        let big = format!("{{\"x\": \"{}\"}}", "a".repeat(MAX_FILE_BYTES));
        assert!(check_upload(&[f("vercel.json", &big)]).is_err());
    }

    #[test]
    fn summary_counts() {
        let plan = project::parse(&[(
            "vercel.json",
            r#"{"redirects":[{"source":"/a","destination":"/b"}],"crons":[{"path":"/c","schedule":"0 0 * * *"}],
                "firewall":{"blockIps":["1.2.3.4"],"rules":[{"path":"/x","action":"deny"}]}}"#,
        )])
        .unwrap();
        let s = summary(&plan);
        assert_eq!((s.redirects, s.firewall_rules), (1, 2));
        assert_eq!(s.crons[0].schedule, "0 0 * * *");
        assert!(s.crons[0]
            .next_run_at
            .as_deref()
            .is_some_and(|t| t.ends_with("T00:00:00Z")));
    }

    /// A fake GitHub API on localhost: (base URL, requests seen).
    async fn fake_github() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        use axum::extract::{OriginalUri, State};
        use axum::http::{HeaderMap, StatusCode};
        use base64::Engine;
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let app = axum::Router::new()
            .fallback(
                |State(seen): State<Arc<std::sync::Mutex<Vec<String>>>>, uri: OriginalUri, h: HeaderMap| async move {
                    let auth = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("-");
                    let path = uri.0.to_string();
                    seen.lock().unwrap().push(format!("{path} {auth}"));
                    let file = |text: &str| {
                        let c = base64::engine::general_purpose::STANDARD.encode(text);
                        // GitHub wraps the base64 content every 60 characters.
                        let wrapped: String = c
                            .as_bytes()
                            .chunks(60)
                            .map(|l| format!("{}\n", String::from_utf8_lossy(l)))
                            .collect();
                        (
                            StatusCode::OK,
                            serde_json::json!({"type": "file", "encoding": "base64", "content": wrapped}).to_string(),
                        )
                    };
                    match path.as_str() {
                        "/repos/o/web/contents/apps/web/vercel.json?ref=abc123" => file(r#"{"cleanUrls": true}"#),
                        "/repos/o/web/contents/vercel.json" => file(r#"{"trailingSlash": false}"#),
                        "/repos/o/web" | "/repos/o/empty" => (StatusCode::OK, "{}".to_string()),
                        _ => (StatusCode::NOT_FOUND, r#"{"message": "Not Found"}"#.to_string()),
                    }
                },
            )
            .with_state(seen.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        (url, seen)
    }

    #[tokio::test]
    async fn reads_project_files_from_github() {
        let dir = tempfile::tempdir().unwrap();
        let (url, seen) = fake_github().await;
        let cfg = Config {
            github_api: url,
            run_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        // At a commit, under the app's base directory; the token is sent.
        let files = fetch_github(&cfg, "o/web", "/apps/web/", "abc123", Some("tok"))
            .await
            .unwrap();
        assert_eq!(
            files,
            vec![("vercel.json".to_string(), r#"{"cleanUrls": true}"#.to_string())]
        );
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.contains("vercelify.json?ref=abc123 Bearer tok")));
        // No branch known: the default branch.
        let files = fetch_github(&cfg, "o/web", "", "", None).await.unwrap();
        assert_eq!(files[0].1, r#"{"trailingSlash": false}"#);
        // No file in a readable repository: nothing (the app's rules go).
        assert!(fetch_github(&cfg, "o/empty", "", "main", None)
            .await
            .unwrap()
            .is_empty());
        // A repository GitHub won't show (an expired token): an error, so
        // the app keeps its rules.
        let err = fetch_github(&cfg, "o/private", "", "main", Some("old"))
            .await
            .unwrap_err();
        assert!(err.contains("does the token have access"), "{err}");
        assert!(fetch_github(&cfg, "o/web", "../etc", "main", None).await.is_err());
        assert!(fetch_github(&cfg, "o/web", "", "main;rm", None).await.is_err());
    }

    fn test_cfg(dir: &std::path::Path) -> Arc<Config> {
        Arc::new(Config {
            state_dir: dir.join("state"),
            run_dir: dir.join("run"),
            proxy_dir: dir.join("proxy"),
            // No Docker in unit tests: every call fails fast.
            docker_bin: "/nonexistent/docker".into(),
            install_env: dir.join("install.env"),
            ..Config::default()
        })
    }

    #[tokio::test]
    async fn store_lifecycle_and_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg(dir.path());
        std::fs::create_dir_all(cfg.proxy_dir.join("dynamic")).unwrap();
        // Left over from an app that no longer has rules.
        std::fs::write(cfg.proxy_dir.join("dynamic/vercelify-routes-gone.yaml"), "x").unwrap();
        std::fs::write(cfg.proxy_dir.join("dynamic/coolify.yaml"), "keep").unwrap();
        let events = Arc::new(Events::default());
        let jobs = Arc::new(Jobs::load(&cfg, events.clone()));
        let p = Projects::load(cfg.clone(), events.clone(), jobs.clone());
        let files = vec![("vercel.json".to_string(), r#"{"cleanUrls":true}"#.to_string())];
        let info = p.upload("abc123", files.clone()).await.unwrap();
        assert_eq!(info.source, "upload");
        assert!(info.summary.clean_urls);
        assert!(!info.routes_active);
        // Docker can't be asked here: routes are left alone, and that is said.
        assert!(info.error.is_none());
        assert!(p.status().await.error.is_some_and(|e| e.contains("Docker")));
        assert!(!cfg.proxy_dir.join("dynamic/vercelify-routes-gone.yaml").exists());
        assert!(cfg.proxy_dir.join("dynamic/coolify.yaml").exists());
        // An upload wins over the repository.
        p.set_from_repo("abc123", 0, vec![("vercel.json".into(), "{}".into())], "deadbeef")
            .await;
        assert_eq!(p.get("abc123").await.unwrap().source, "upload");
        // Previews come from the repository.
        p.set_from_repo("abc123", 7, files.clone(), "cafe").await;
        assert_eq!(p.preview_keys("abc123").await, vec![(7, "abc123-pr-7".to_string())]);
        assert_eq!(p.get("abc123-pr-7").await.unwrap().commit.as_deref(), Some("cafe"));
        // The file was deleted from the branch: its rules go too.
        p.set_from_repo("abc123", 7, vec![], "beef").await;
        assert!(p.get("abc123-pr-7").await.is_none());
        // Survives a restart.
        let again = Projects::load(cfg.clone(), events.clone(), jobs.clone());
        assert_eq!(again.status().await.projects.len(), 1);
        assert!(again.remove_app("abc123").await);
        assert!(again.status().await.projects.is_empty());
        assert!(p.upload("bad uuid!", files).await.is_err());
        // A cron run without an address fails and is recorded.
        let p = Arc::new(Projects::load(cfg.clone(), events, jobs));
        p.upload(
            "abc123",
            vec![(
                "vercel.json".into(),
                r#"{"crons":[{"path":"/api/c","schedule":"* * * * *"}]}"#.into(),
            )],
        )
        .await
        .unwrap();
        assert!(p.run_cron("abc123", "* * * * *", "/api/c", None).await.is_err());
        let c = &p.get("abc123").await.unwrap().summary.crons[0];
        assert!(c.last_error.as_deref().is_some_and(|e| e.contains("no address")));
        assert!(c.last_run_at.is_some() && !c.running);
        assert!(p.run_now("abc123", "/nope").await.is_err());
        // A cron job taken out of the file takes its history with it.
        p.upload("abc123", vec![("vercel.json".into(), r#"{"cleanUrls":true}"#.into())])
            .await
            .unwrap();
        assert!(p.store.lock().await.projects[0].crons.is_empty());
    }
}

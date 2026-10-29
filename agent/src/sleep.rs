//! Sleeping apps: scale to zero on a small machine, wake on request.
//!
//! An app (and optionally its pull request previews) that got no requests
//! for `idle_minutes` is stopped by the platform engine, giving its memory
//! back. A low-priority route in our proxy (Traefik, file provider) sends
//! requests for a stopped app to this agent's wake listener, which has the
//! engine start it again (a second or two) and shows a "waking up" page that
//! reloads itself. Once the app is healthy its own route wins again (higher
//! priority).
//!
//! Requests are counted by Traefik (`traefik_router_requests_total`, one
//! router per route: `vfy-<app>-<n>[-secure]@file`), so health checks inside
//! the container and database traffic don't count as visits.

use crate::config::Config;
use crate::error::AppError;
use crate::events::{Events, Jobs};
use crate::exec::run;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use vercelify_core::{ProxyMetricsResult, SleepRequest, SleepSetting, SleepStatus, SleepTarget};

pub const DEFAULT_IDLE_MINUTES: u32 = 30;
const DYNAMIC_FILE: &str = "vercelify-sleep.yaml";
/// A woken app is left awake at least this many minutes.
const MIN_AWAKE_MINUTES: u64 = 5;

/// Seconds in a "minute" of idle time: 60, except in the live test
/// (`VERCELIFY_SLEEP_SECONDS_PER_MINUTE`), which can't wait half an hour.
fn minute() -> u64 {
    static M: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *M.get_or_init(|| {
        std::env::var("VERCELIFY_SLEEP_SECONDS_PER_MINUTE")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(60)
    })
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// --- pure helpers -----------------------------------------------------------

/// An app id (`app_…`) or name.
pub fn valid_app_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Requests per router from Traefik's Prometheus text.
pub fn router_counts(metrics: &str) -> HashMap<String, f64> {
    let mut out: HashMap<String, f64> = HashMap::new();
    for line in metrics.lines() {
        let Some(rest) = line.strip_prefix("traefik_router_requests_total{") else {
            continue;
        };
        let Some((labels, value)) = rest.rsplit_once("} ") else {
            continue;
        };
        let Some(router) = labels
            .split(',')
            .find_map(|l| l.strip_prefix("router=\""))
            .map(|r| r.trim_end_matches('"'))
        else {
            continue;
        };
        let Ok(v) = value.trim().parse::<f64>() else { continue };
        *out.entry(router.to_string()).or_default() += v;
    }
    out
}

/// `vfy-<key>-<n>[-secure]@file` → `<key>` (an app name, or
/// `<name>-pr-<n>`).
pub fn router_key(router: &str) -> Option<&str> {
    let name = router.strip_suffix("@file")?;
    let rest = name.strip_prefix("vfy-")?;
    let rest = rest.strip_suffix("-secure").unwrap_or(rest);
    let (key, n) = rest.rsplit_once('-')?;
    (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && !key.is_empty()).then_some(key)
}

/// Total requests per sleep key.
pub fn requests_per_key(counts: &HashMap<String, f64>) -> HashMap<String, f64> {
    let mut out: HashMap<String, f64> = HashMap::new();
    for (router, v) in counts {
        if let Some(k) = router_key(router) {
            *out.entry(k.to_string()).or_default() += v;
        }
    }
    out
}

/// Traefik dynamic configuration: requests for these hosts reach the wake
/// listener when the app's own (higher priority) route is gone.
pub fn dynamic_config_for(hosts: &[(bool, String)], wake_url: &str) -> Option<String> {
    if hosts.is_empty() {
        return None;
    }
    let rule = |https: bool| {
        let mut hs: Vec<&str> = hosts
            .iter()
            .filter(|(h, _)| *h == https || !https)
            .map(|(_, h)| h.as_str())
            .collect();
        hs.sort_unstable();
        hs.dedup();
        hs.iter()
            .map(|h| format!("Host(`{h}`)"))
            .collect::<Vec<_>>()
            .join(" || ")
    };
    let mut y = String::from("# Written by vercelify-agent (sleeping apps). Do not edit.\nhttp:\n  routers:\n");
    y.push_str(&format!(
        "    vercelify-wake-http:\n      rule: \"{}\"\n      entryPoints: [web]\n      service: vercelify-wake\n      priority: 1\n",
        rule(false),
    ));
    if hosts.iter().any(|(https, _)| *https) {
        y.push_str(&format!(
            "    vercelify-wake-https:\n      rule: \"{}\"\n      entryPoints: [websecure]\n      service: vercelify-wake\n      priority: 1\n      tls:\n        certResolver: {}\n",
            rule(true),
            vercelify_platform::proxy::RESOLVER
        ));
    }
    y.push_str(&format!(
        "  services:\n    vercelify-wake:\n      loadBalancer:\n        servers:\n          - url: \"{wake_url}\"\n"
    ));
    Some(y)
}

fn waking_page(host: &str, starting: bool) -> String {
    let host = host.replace(['<', '>', '&', '"'], "");
    let msg = if starting {
        "This app was asleep to save memory. It is starting and this page reloads by itself."
    } else {
        "This app is not running right now."
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<meta name=\"robots\" content=\"noindex\">{refresh}<title>Waking up {host}</title>\
<style>body{{margin:0;min-height:100vh;display:grid;place-items:center;font:15px/1.5 system-ui,sans-serif;background:#0b0a0c;color:#e8e6e3}}\
@media (prefers-color-scheme:light){{body{{background:#f7f6f4;color:#1c1b1a}}}}main{{max-width:28rem;padding:2rem;text-align:center}}\
.dot{{display:inline-block;width:.6rem;height:.6rem;border-radius:50%;background:#e2683c;margin-right:.5rem;animation:p 1.2s ease-in-out infinite}}\
@keyframes p{{50%{{opacity:.3}}}}@media (prefers-reduced-motion:reduce){{.dot{{animation:none}}}}p{{opacity:.7}}</style></head>\
<body><main><h1><span class=\"dot\"></span>{title}</h1><p>{msg}</p></main></body></html>",
        refresh = if starting { "<meta http-equiv=\"refresh\" content=\"2\">" } else { "" },
        title = if starting { format!("Waking up {host}") } else { host.clone() },
    )
}

// --- state ------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Asleep {
    containers: Vec<String>,
    since: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    settings: BTreeMap<String, SleepSetting>,
    /// key → containers this agent stopped.
    asleep: BTreeMap<String, Asleep>,
}

#[derive(Debug, Clone)]
struct Seen {
    requests: f64,
    changed: Instant,
    at: String,
}

#[derive(Debug, Clone)]
struct Target {
    key: String,
    app_uuid: String,
    pr: u64,
    hosts: Vec<(bool, String)>,
    idle: Duration,
}

pub struct Sleep {
    cfg: Arc<Config>,
    store: Mutex<Store>,
    seen: Mutex<HashMap<String, Seen>>,
    targets: Mutex<Vec<Target>>,
    waking: Mutex<HashSet<String>>,
    error: Mutex<Option<String>>,
    metrics_on: Mutex<bool>,
    /// Last time an unknown host made us reload the targets.
    reloaded: Mutex<Option<Instant>>,
    events: Arc<Events>,
    jobs: Arc<Jobs>,
    /// The platform engine (none on a worker, or when it didn't start).
    engine: std::sync::OnceLock<Arc<vercelify_platform::engine::Engine>>,
}

/// One row of `docker ps -a` for the platform's containers.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub running: bool,
    pub pr: u64,
    /// `vercelify.app`.
    pub app: String,
    pub image: String,
}

/// Rows: id, name, state, pr label, app uuid label, image (tab-separated).
pub fn parse_containers(out: &str) -> Vec<Row> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 4).then(|| Row {
                id: f[0].to_string(),
                name: f[1].to_string(),
                running: f[2] == "running",
                pr: f[3].parse().unwrap_or(0),
                app: f.get(4).copied().unwrap_or("").to_string(),
                image: f.get(5).copied().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// The containers of `uuid`'s production app (`pr` 0) or a preview.
pub fn containers_for(rows: &[Row], uuid: &str, pr: u64) -> Vec<(String, bool)> {
    rows.iter()
        .filter(|r| r.pr == pr && r.app == uuid)
        .map(|r| (r.id.clone(), r.running))
        .collect()
}

impl Sleep {
    fn path(cfg: &Config) -> PathBuf {
        cfg.state_dir.join("sleep.json")
    }

    pub fn load(cfg: Arc<Config>, events: Arc<Events>, jobs: Arc<Jobs>) -> Self {
        let store = std::fs::read_to_string(Self::path(&cfg))
            .ok()
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default();
        Self {
            cfg,
            store: Mutex::new(store),
            seen: Mutex::new(HashMap::new()),
            targets: Mutex::new(Vec::new()),
            waking: Mutex::new(HashSet::new()),
            error: Mutex::new(None),
            metrics_on: Mutex::new(false),
            reloaded: Mutex::new(None),
            events,
            jobs,
            engine: std::sync::OnceLock::new(),
        }
    }

    pub fn set_engine(&self, e: Arc<vercelify_platform::engine::Engine>) {
        let _ = self.engine.set(e);
    }

    /// Platform apps and previews with sleep on, with their hosts.
    fn platform_targets(&self, engine: &vercelify_platform::engine::Engine, settings: &[SleepSetting]) -> Vec<Target> {
        let by_id: HashMap<&str, &SleepSetting> = settings.iter().map(|s| (s.app_uuid.as_str(), s)).collect();
        let res = engine.store.with(|c| {
            let domain = vercelify_platform::apps::domain_settings(c)?;
            let mut out = vec![];
            for app in vercelify_platform::apps::list(c, None)? {
                let Some(s) = by_id.get(app.id.as_str()) else { continue };
                let idle = Duration::from_secs(u64::from(s.idle_minutes) * minute());
                let hosts_of_route = |pr: Option<u32>| -> Vec<(bool, String)> {
                    vercelify_platform::engine::hosts_for(&domain, &app, pr)
                        .0
                        .into_iter()
                        .filter(|h| h.redirect_to.is_none())
                        .map(|h| (h.https, h.host))
                        .collect()
                };
                if app.current_deployment_id.is_some() && app.desired != vercelify_core::platform::Desired::Stopped {
                    out.push(Target {
                        key: vercelify_platform::deploy::route_key(&app.name, None),
                        app_uuid: app.id.clone(),
                        pr: 0,
                        hosts: hosts_of_route(None),
                        idle,
                    });
                }
                if s.previews {
                    for p in vercelify_platform::previews::list(c, &app.id, true)? {
                        if p.deployment_id.is_none() {
                            continue;
                        }
                        out.push(Target {
                            key: vercelify_platform::deploy::route_key(&app.name, Some(p.pr_number)),
                            app_uuid: app.id.clone(),
                            pr: u64::from(p.pr_number),
                            hosts: hosts_of_route(Some(p.pr_number)),
                            idle,
                        });
                    }
                }
            }
            Ok(out)
        });
        res.unwrap_or_else(|e| {
            tracing::warn!("sleep: reading platform apps: {e}");
            vec![]
        })
    }

    fn save(&self, store: &Store) {
        if let Err(e) =
            crate::watch::write_private(&Self::path(&self.cfg), &serde_json::to_vec(store).unwrap_or_default())
        {
            tracing::warn!("sleep: could not save: {e}");
        }
    }

    /// Apps with sleep on (their uptime isn't checked: a check is a visit).
    pub async fn enabled_apps(&self) -> HashSet<String> {
        self.store
            .lock()
            .await
            .settings
            .values()
            .filter(|s| s.enabled)
            .map(|s| s.app_uuid.clone())
            .collect()
    }

    pub async fn status(&self) -> SleepStatus {
        let store = self.store.lock().await;
        let seen = self.seen.lock().await;
        let waking = self.waking.lock().await;
        let targets = self.targets.lock().await;
        SleepStatus {
            proxy_metrics: *self.metrics_on.lock().await,
            settings: store.settings.values().cloned().collect(),
            targets: targets
                .iter()
                .map(|t| SleepTarget {
                    key: t.key.clone(),
                    app_uuid: t.app_uuid.clone(),
                    pull_request_id: t.pr,
                    hosts: t.hosts.iter().map(|(_, h)| h.clone()).collect(),
                    state: if waking.contains(&t.key) {
                        "waking"
                    } else if store.asleep.contains_key(&t.key) {
                        "asleep"
                    } else {
                        "awake"
                    }
                    .into(),
                    last_request_at: seen.get(&t.key).map(|s| s.at.clone()),
                    asleep_since: store.asleep.get(&t.key).map(|a| a.since.clone()),
                })
                .collect(),
            error: self.error.lock().await.clone(),
        }
    }

    /// Turn sleep on or off for an app. Turning it off wakes it.
    pub async fn set(&self, app_uuid: &str, req: SleepRequest) -> Result<SleepSetting, AppError> {
        if !valid_app_id(app_uuid) {
            return Err(AppError::bad_request("invalid app id"));
        }
        if self.cfg.role() == "worker" {
            return Err(AppError::conflict(
                "apps sleep on the main server; select it in the app",
            ));
        }
        if self.cfg.wake_listen.trim().is_empty() {
            return Err(AppError::conflict(
                "waking is turned off on this server (wake_listen in agent.toml)",
            ));
        }
        let Some(engine) = self.engine.get() else {
            return Err(AppError::unavailable(
                "apps sleep on the platform engine, which isn't running here",
            ));
        };
        // Settings are kept by app id: a name would be saved and never apply.
        let wanted = app_uuid.to_string();
        let found = engine
            .store
            .with(|c| vercelify_platform::apps::find(c, &wanted))
            .map(|a| a.id);
        let resolved = match found {
            Ok(id) => id,
            // Turning off a deleted app's setting still clears it.
            Err(_) if !req.enabled => wanted,
            Err(e) => return Err(AppError::not_found(e.to_string())),
        };
        let app_uuid = resolved.as_str();
        let idle = req.idle_minutes.unwrap_or(DEFAULT_IDLE_MINUTES);
        if !(5..=24 * 60).contains(&idle) {
            return Err(AppError::bad_request("idle time must be 5 minutes to 24 hours"));
        }
        let setting = SleepSetting {
            app_uuid: app_uuid.into(),
            enabled: req.enabled,
            idle_minutes: idle,
            previews: req.previews.unwrap_or(false),
        };
        let mut wake: Vec<String> = Vec::new();
        {
            let mut store = self.store.lock().await;
            if req.enabled {
                store.settings.insert(app_uuid.into(), setting.clone());
            } else {
                store.settings.remove(app_uuid);
            }
            if !req.enabled || !setting.previews {
                wake = store
                    .asleep
                    .keys()
                    .filter(|k| (k.as_str() == app_uuid && !req.enabled) || k.starts_with(&format!("{app_uuid}-pr-")))
                    .cloned()
                    .collect();
            }
            self.save(&store);
        }
        for k in wake {
            self.wake_key(&k, "sleep turned off").await;
        }
        self.events.changed("sleep");
        Ok(setting)
    }

    /// Our proxy always counts requests per router (kept for the API).
    pub async fn ensure_proxy_metrics(&self) -> Result<ProxyMetricsResult, AppError> {
        *self.metrics_on.lock().await = true;
        Ok(ProxyMetricsResult {
            metrics: true,
            restarted: false,
        })
    }

    async fn scrape(&self) -> Result<HashMap<String, f64>, String> {
        let (status, body) =
            crate::http::call(crate::http::Req::get(&self.cfg.traefik_metrics_url).timeout(Duration::from_secs(10)))
                .await
                .map_err(|e| format!("the proxy's request counters are unreachable ({e})"))?;
        if !status.is_success() {
            return Err("the proxy does not count requests yet — turn sleep on again to set it up".into());
        }
        Ok(requests_per_key(&router_counts(&String::from_utf8_lossy(&body))))
    }

    /// Apps and previews with sleep on, with their hosts.
    async fn load_targets(&self, settings: &[SleepSetting]) -> Vec<Target> {
        match self.engine.get() {
            Some(engine) if !settings.is_empty() => self.platform_targets(engine, settings),
            _ => vec![],
        }
    }

    async fn docker_rows(&self) -> Option<Vec<Row>> {
        let out = run(
            &self.cfg.docker_bin,
            &[
                "ps",
                "-a",
                "--format",
                "{{.ID}}\t{{.Names}}\t{{.State}}\t{{.Label \"vercelify.pr\"}}\t{{.Label \"vercelify.app\"}}\t{{.Image}}",
            ],
            Duration::from_secs(20),
        )
        .await
        .ok()
        .filter(|o| o.success())?;
        Some(parse_containers(&out.stdout))
    }

    fn write_routes(&self, targets: &[Target]) {
        let dir = self.cfg.proxy_dir.join("dynamic");
        let path = dir.join(DYNAMIC_FILE);
        let wake_port = self.cfg.wake_listen.rsplit(':').next().unwrap_or("7421");
        let hosts: Vec<(bool, String)> = targets.iter().flat_map(|t| t.hosts.clone()).collect();
        match dynamic_config_for(&hosts, &format!("http://host.docker.internal:{wake_port}")) {
            Some(y) => {
                if std::fs::read_to_string(&path).ok().as_deref() != Some(y.as_str()) && dir.is_dir() {
                    if let Err(e) = std::fs::write(&path, y) {
                        tracing::warn!("sleep: writing {}: {e}", path.display());
                    }
                }
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// One pass (from the background loop): refresh targets and routes, put
    /// idle apps to sleep.
    pub async fn tick(&self) {
        let settings: Vec<SleepSetting> = self
            .store
            .lock()
            .await
            .settings
            .values()
            .filter(|s| s.enabled)
            .cloned()
            .collect();
        let targets = self.load_targets(&settings).await;
        self.write_routes(&targets);
        *self.targets.lock().await = targets.clone();
        if targets.is_empty() {
            *self.error.lock().await = None;
            return;
        }
        let counts = match self.scrape().await {
            Ok(c) => {
                *self.metrics_on.lock().await = true;
                *self.error.lock().await = None;
                c
            }
            Err(e) => {
                // Never put an app to sleep without knowing it is idle.
                *self.metrics_on.lock().await = false;
                *self.error.lock().await = Some(e);
                return;
            }
        };
        let Some(rows) = self.docker_rows().await else { return };
        // A build is running: memory is tight and deploys restart containers.
        let building = self.engine.get().is_some_and(|e| e.building());
        for t in &targets {
            let containers = containers_for(&rows, &t.app_uuid, t.pr);
            let running: Vec<String> = containers
                .iter()
                .filter(|(_, r)| *r)
                .map(|(id, _)| id.clone())
                .collect();
            let requests = counts.get(&t.key).copied().unwrap_or(0.0);
            let idle_for = {
                let mut seen = self.seen.lock().await;
                let s = seen.entry(t.key.clone()).or_insert_with(|| Seen {
                    requests,
                    changed: Instant::now(),
                    at: now(),
                });
                if (s.requests - requests).abs() > f64::EPSILON {
                    *s = Seen {
                        requests,
                        changed: Instant::now(),
                        at: now(),
                    };
                }
                s.changed.elapsed()
            };
            let asleep = self.store.lock().await.asleep.contains_key(&t.key);
            if asleep {
                if !running.is_empty() {
                    // Redeployed or started by hand: awake again.
                    let mut store = self.store.lock().await;
                    store.asleep.remove(&t.key);
                    self.save(&store);
                    self.events.changed("sleep");
                }
                continue;
            }
            if running.is_empty()
                || building
                || idle_for < t.idle.max(Duration::from_secs(MIN_AWAKE_MINUTES * minute()))
                || t.hosts.is_empty()
            {
                continue;
            }
            self.put_to_sleep(t, running, idle_for).await;
        }
    }

    async fn put_to_sleep(&self, t: &Target, containers: Vec<String>, idle_for: Duration) {
        let job = self.jobs.start(
            "sleep",
            format!(
                "{} went to sleep",
                t.hosts.first().map_or(t.key.as_str(), |(_, h)| h.as_str())
            ),
            Some(&t.app_uuid),
        );
        // The engine records it first, so its reconciler keeps it stopped.
        let stopped: Result<(), String> = match self.engine.get() {
            Some(e) => {
                let pr = (t.pr > 0).then_some(t.pr as u32);
                e.sleep(&t.app_uuid, pr).await.map_err(|e| e.to_string())
            }
            None => Err("the platform engine isn't running".into()),
        };
        match stopped {
            Ok(()) => {
                let mut store = self.store.lock().await;
                store.asleep.insert(
                    t.key.clone(),
                    Asleep {
                        containers,
                        since: now(),
                    },
                );
                self.save(&store);
                drop(store);
                job.done(format!(
                    "no requests for {} minutes; wakes on the next one",
                    idle_for.as_secs() / 60
                ));
                self.events.changed("sleep");
            }
            Err(e) => job.fail(e),
        }
    }

    /// Start a sleeping app again. Returns false if it wasn't asleep.
    async fn wake_key(&self, key: &str, why: &str) -> bool {
        if !self.store.lock().await.asleep.contains_key(key) {
            return false;
        }
        if !self.waking.lock().await.insert(key.to_string()) {
            return true; // already starting
        }
        self.events.changed("sleep");
        let target = self.targets.lock().await.iter().find(|t| t.key == key).cloned();
        let app = match &target {
            Some(t) => t.app_uuid.clone(),
            None => key.split("-pr-").next().unwrap_or(key).to_string(),
        };
        let job = self.jobs.start("wake", format!("Waking {key}"), Some(&app));
        let result: Result<(), String> = match (self.engine.get(), &target) {
            // Starts it, waits until it answers, then routes it again.
            (Some(e), Some(t)) => {
                let pr = (t.pr > 0).then_some(t.pr as u32);
                e.wake(&t.app_uuid, pr).await.map_err(|e| e.to_string())
            }
            (Some(_), None) => Err(format!("{key} is not a sleeping app any more")),
            (None, _) => Err("the platform engine isn't running".into()),
        };
        let ok = result.is_ok();
        if ok {
            let mut store = self.store.lock().await;
            store.asleep.remove(key);
            self.save(&store);
            drop(store);
            // A fresh start counts as activity: no sleeping right away.
            if let Some(s) = self.seen.lock().await.get_mut(key) {
                s.changed = Instant::now();
                s.at = now();
            }
            job.done(why.to_string());
        } else if let Err(e) = result {
            job.fail(e);
        }
        self.waking.lock().await.remove(key);
        self.events.changed("sleep");
        ok
    }

    /// A request for `host` reached the wake listener.
    async fn key_for(&self, host: &str) -> Option<String> {
        self.targets
            .lock()
            .await
            .iter()
            .find(|t| t.hosts.iter().any(|(_, h)| h == host))
            .map(|t| t.key.clone())
    }

    pub async fn on_request(self: &Arc<Self>, host: &str) -> (u16, String) {
        let host = host.split(':').next().unwrap_or("").to_ascii_lowercase();
        let mut key = self.key_for(&host).await;
        if key.is_none() {
            // Right after a restart the targets aren't loaded yet (the loop
            // runs a little later): load them now, at most every 10 s.
            let due = {
                let mut r = self.reloaded.lock().await;
                let due = r.is_none_or(|t| t.elapsed() > Duration::from_secs(10));
                if due {
                    *r = Some(Instant::now());
                }
                due
            };
            if due {
                let settings: Vec<SleepSetting> = self
                    .store
                    .lock()
                    .await
                    .settings
                    .values()
                    .filter(|s| s.enabled)
                    .cloned()
                    .collect();
                let targets = self.load_targets(&settings).await;
                *self.targets.lock().await = targets;
                key = self.key_for(&host).await;
            }
        }
        let Some(key) = key else {
            return (404, waking_page(&host, false));
        };
        let asleep = self.store.lock().await.asleep.contains_key(&key);
        if asleep {
            let me = self.clone();
            let k = key.clone();
            // Answer now; the page reloads until the app's route is back.
            tokio::spawn(async move { me.wake_key(&k, "woken by a visit").await });
            return (503, waking_page(&host, true));
        }
        if self.waking.lock().await.contains(&key) {
            return (503, waking_page(&host, true));
        }
        // Awake per our records but its route is gone: starting (a deploy,
        // a health check not passed yet), or stopped by someone else, which
        // is not ours to undo.
        let target = self.targets.lock().await.iter().find(|t| t.key == key).cloned();
        let starting = match (target, self.docker_rows().await) {
            (Some(t), Some(rows)) => containers_for(&rows, &t.app_uuid, t.pr)
                .iter()
                .any(|(_, running)| *running),
            _ => true,
        };
        (503, waking_page(&host, starting))
    }
}

/// The wake listener: Traefik forwards requests for sleeping apps here.
pub fn wake_router(sleep: Arc<Sleep>) -> axum::Router {
    use axum::http::{header, HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    axum::Router::new().fallback(move |headers: HeaderMap| {
        let sleep = sleep.clone();
        async move {
            let host = headers
                .get("x-forwarded-host")
                .or_else(|| headers.get(header::HOST))
                .and_then(|h| h.to_str().ok())
                .unwrap_or("")
                .to_string();
            let (code, page) = sleep.on_request(&host).await;
            (
                StatusCode::from_u16(code).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-store"),
                    (header::RETRY_AFTER, "2"),
                ],
                page,
            )
                .into_response()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const METRICS: &str = r#"# HELP traefik_router_requests_total How many HTTP requests
# TYPE traefik_router_requests_total counter
traefik_router_requests_total{code="200",method="GET",protocol="http",router="vfy-blog-0@file",service="vfy-blog@file"} 3
traefik_router_requests_total{code="404",method="GET",protocol="http",router="vfy-blog-0-secure@file",service="vfy-blog@file"} 2
traefik_router_requests_total{code="200",method="GET",protocol="http",router="vfy-blog-pr-7-0-secure@file",service="x"} 1e+01
traefik_router_requests_total{code="200",method="GET",protocol="http",router="api@internal",service="api@internal"} 9
traefik_entrypoint_requests_total{code="200",entrypoint="web"} 99
"#;

    #[test]
    fn platform_router_names() {
        assert_eq!(router_key("vfy-blog-0@file"), Some("blog"));
        assert_eq!(router_key("vfy-my-blog-1-secure@file"), Some("my-blog"));
        assert_eq!(router_key("vfy-blog-pr-7-0-secure@file"), Some("blog-pr-7"));
        assert_eq!(router_key("vercelify-wake-http@file"), None);
        assert_eq!(router_key("vfy-blog@file"), None);
        let y = dynamic_config_for(&[(true, "a.test".into())], "http://h:1").unwrap();
        assert!(y.contains("entryPoints: [web]") && y.contains("entryPoints: [websecure]"));
        assert!(y.contains("certResolver: le\n"));
    }

    #[test]
    fn counts_requests_per_app_and_preview() {
        let per = requests_per_key(&router_counts(METRICS));
        assert_eq!(per.get("blog"), Some(&5.0));
        assert_eq!(per.get("blog-pr-7"), Some(&10.0));
        assert_eq!(per.len(), 2);
        assert_eq!(router_key("http-0-u1@docker"), None, "other routers aren't ours");
    }

    #[test]
    fn wake_routes_only_add_https_where_used() {
        assert_eq!(dynamic_config_for(&[], "http://h:7421"), None);
        let y = dynamic_config_for(&[(false, "a.test".into())], "http://host.docker.internal:7421").unwrap();
        assert!(y.contains("rule: \"Host(`a.test`)\""));
        assert!(y.contains("priority: 1"));
        assert!(!y.contains("vercelify-wake-https"));
        assert!(y.contains("url: \"http://host.docker.internal:7421\""));
        let y = dynamic_config_for(&[(true, "b.test".into()), (false, "a.test".into())], "http://h:1").unwrap();
        // http serves both (Cloudflare tunnels arrive on http); https only the https one.
        assert!(y.contains("rule: \"Host(`a.test`) || Host(`b.test`)\""));
        assert!(y.contains("vercelify-wake-https:\n      rule: \"Host(`b.test`)\""));
    }

    #[test]
    fn containers_by_label() {
        let rows = parse_containers(
            "c1\tvfy-blog-x1\trunning\t\tapp_1\tvfy/blog:x\nc2\tvfy-blog-pr7\texited\t7\tapp_1\nc4\tother\trunning\t\tapp_2\n",
        );
        assert_eq!(containers_for(&rows, "app_1", 0), vec![("c1".into(), true)]);
        assert_eq!(containers_for(&rows, "app_1", 7), vec![("c2".into(), false)]);
    }

    #[test]
    fn waking_page_escapes_the_host() {
        let p = waking_page("<script>x.test", true);
        assert!(!p.contains("<script>"));
        assert!(p.contains("http-equiv=\"refresh\""));
        assert!(!waking_page("a.test", false).contains("refresh"));
    }
}

//! The agent's background loop: every `watch_interval_secs` it
//!
//! 1. runs the doctor and turns status changes into alerts (sent to an ntfy
//!    topic or webhook, and kept for the cockpit's activity list),
//! 2. records a metrics point (24 hours kept, flushed to disk),
//! 3. checks that every app URL and every pooled server answers,
//! 4. repairs what it safely can (firewall rules, a stopped tunnel), and
//! 5. sets OOM priorities so builds die before apps and apps before data.
//!
//! One loop, one sample per minute: the cockpit reads the results instead
//! of making the server recompute them on every poll.

use crate::config::Config;
use crate::exec::run;
use crate::http::{self, Req};
use crate::routes::AppState;
use crate::{doctor, services};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use vercelify_core::{Alert, AlertsStatus, CheckStatus, DoctorCheck, MetricPoint, SystemSnapshot, UptimeCheck};

const MAX_ALERTS: usize = 200;
/// 24 hours at one point a minute.
const MAX_POINTS: usize = 24 * 60;
const REPAIR_COOLDOWN: Duration = Duration::from_secs(600);

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AlertStore {
    next_id: u64,
    /// Last known status per source, so restarts don't re-alert.
    last: HashMap<String, CheckStatus>,
    alerts: VecDeque<Alert>,
    #[serde(default)]
    notify_url: Option<String>,
}

/// A status change worth telling someone about.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub source: String,
    pub status: CheckStatus,
    pub title: String,
    pub detail: String,
    pub resolved: bool,
}

/// Compare new check results with the last known ones. Only changes count;
/// a new source counts when it is not OK.
pub fn diff(last: &HashMap<String, CheckStatus>, checks: &[(String, CheckStatus, String, String)]) -> Vec<Change> {
    let mut out = Vec::new();
    for (source, status, label, detail) in checks {
        let before = last.get(source).copied().unwrap_or(CheckStatus::Ok);
        if before == *status {
            continue;
        }
        let resolved = *status == CheckStatus::Ok;
        out.push(Change {
            source: source.clone(),
            status: *status,
            title: if resolved {
                format!("{label}: back to normal")
            } else {
                format!(
                    "{label}: {}",
                    if *status == CheckStatus::Bad {
                        "needs attention"
                    } else {
                        "warning"
                    }
                )
            },
            detail: detail.clone(),
            resolved,
        });
    }
    out
}

/// Notices with a timestamp in their source: nothing to resolve later.
pub fn is_one_off(source: &str) -> bool {
    ["repair:", "cleanup:", "test:"].iter().any(|p| source.starts_with(p))
}

/// Clean up from 90% used.
pub fn needs_cleanup(used: u64, total: u64) -> bool {
    total > 0 && used * 100 / total >= 90
}

/// Hide secrets in a notification URL (ntfy topics and webhook paths are
/// passwords): keep the scheme and host.
pub fn mask_url(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let host = rest.split('/').next().unwrap_or("");
    if scheme.is_empty() {
        "•••".into()
    } else {
        format!("{scheme}://{host}/•••")
    }
}

/// How to post to a notification URL: (content type, body).
pub fn notification_body(url: &str, title: &str, detail: &str) -> (&'static str, String) {
    let text = if detail.is_empty() {
        title.to_string()
    } else {
        format!("{title}\n{detail}")
    };
    if url.contains("discord.com/api/webhooks") || url.contains("discordapp.com/api/webhooks") {
        ("application/json", serde_json::json!({ "content": text }).to_string())
    } else if url.contains("hooks.slack.com") {
        ("application/json", serde_json::json!({ "text": text }).to_string())
    } else {
        ("text/plain", detail.to_string())
    }
}

pub fn valid_notify_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && url.len() <= 1024
        && url.chars().all(|c| c.is_ascii_graphic())
}

/// Classify a container for the OOM killer (higher = killed first).
pub fn oom_score_for(name: &str, image: &str, pr_label: &str) -> Option<i32> {
    const DATA: &[&str] = &[
        "postgres",
        "postgis",
        "mysql",
        "mariadb",
        "mongo",
        "redis",
        "valkey",
        "keydb",
        "dragonfly",
        "clickhouse",
        "minio",
    ];
    let image = image.to_lowercase();
    if matches!(name, "coolify-db" | "coolify-redis") || DATA.iter().any(|d| image.contains(d)) {
        Some(-500)
    } else if matches!(
        name,
        "coolify" | "coolify-proxy" | "coolify-realtime" | "coolify-sentinel"
    ) {
        Some(-300)
    } else if image.contains("coolify-helper") {
        Some(900)
    } else if !pr_label.is_empty() && pr_label != "0" {
        Some(600)
    } else if name == "vercelify-ollama" {
        Some(500)
    } else {
        None
    }
}

pub struct Watcher {
    cfg: std::sync::Arc<Config>,
    store: Mutex<AlertStore>,
    metrics: Mutex<VecDeque<MetricPoint>>,
    uptime: Mutex<Vec<UptimeCheck>>,
    repairs: Mutex<HashMap<&'static str, Instant>>,
    events: std::sync::Arc<crate::events::Events>,
}

impl Watcher {
    fn alerts_path(cfg: &Config) -> PathBuf {
        cfg.state_dir.join("alerts.json")
    }
    fn metrics_path(cfg: &Config) -> PathBuf {
        cfg.state_dir.join("metrics.json")
    }

    pub fn load(cfg: std::sync::Arc<Config>, events: std::sync::Arc<crate::events::Events>) -> Self {
        let store = std::fs::read_to_string(Self::alerts_path(&cfg))
            .ok()
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default();
        let metrics: VecDeque<MetricPoint> = std::fs::read_to_string(Self::metrics_path(&cfg))
            .ok()
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default();
        Self {
            cfg,
            store: Mutex::new(store),
            metrics: Mutex::new(metrics),
            uptime: Mutex::new(Vec::new()),
            repairs: Mutex::new(HashMap::new()),
            events,
        }
    }

    /// Where notifications go: set from the cockpit, else agent.toml, else
    /// the power guard's NOTIFY_URL (one setting for both).
    fn notify_url(&self, store: &AlertStore) -> Option<String> {
        store
            .notify_url
            .clone()
            .or_else(|| self.cfg.notify_url.clone())
            .or_else(|| self.cfg.power_guard_values().get("NOTIFY_URL").cloned())
            .filter(|u| !u.is_empty())
    }

    fn save_alerts(&self, store: &AlertStore) {
        if let Err(e) = write_private(
            &Self::alerts_path(&self.cfg),
            &serde_json::to_vec(store).unwrap_or_default(),
        ) {
            tracing::warn!("alerts: could not save: {e}");
        }
    }

    pub async fn status(&self) -> AlertsStatus {
        let store = self.store.lock().await;
        AlertsStatus {
            notify_target: self.notify_url(&store).map(|u| mask_url(&u)),
            alerts: store.alerts.iter().rev().cloned().collect(),
        }
    }

    pub async fn set_notify_url(&self, url: &str) -> Result<(), String> {
        let url = url.trim();
        if !url.is_empty() && !valid_notify_url(url) {
            return Err("use an http(s) URL, e.g. https://ntfy.sh/your-secret-topic".into());
        }
        let mut store = self.store.lock().await;
        store.notify_url = (!url.is_empty()).then(|| url.to_string());
        self.save_alerts(&store);
        Ok(())
    }

    /// Record (and send) one alert.
    pub async fn raise(&self, change: Change) -> Alert {
        let mut store = self.store.lock().await;
        let url = self.notify_url(&store);
        store.next_id += 1;
        let mut alert = Alert {
            id: store.next_id,
            at: now(),
            source: change.source.clone(),
            status: change.status,
            title: change.title,
            detail: change.detail,
            resolved: change.resolved,
            notified: None,
        };
        // One-off notices (repairs, cleanups, tests) have no state to remember.
        if !is_one_off(&change.source) {
            store.last.insert(change.source, change.status);
        }
        drop(store);
        if let Some(url) = url {
            alert.notified = Some(send(&self.cfg, &url, &alert).await);
        }
        let mut store = self.store.lock().await;
        store.alerts.push_back(alert.clone());
        while store.alerts.len() > MAX_ALERTS {
            store.alerts.pop_front();
        }
        self.save_alerts(&store);
        drop(store);
        self.events.alert(&alert);
        alert
    }

    /// Close a problem raised for `source` (a failed deploy, a rollback)
    /// once things are fine again. Does nothing if none is open.
    pub async fn resolve(&self, source: &str, title: String, detail: String) {
        let open = {
            let store = self.store.lock().await;
            store.last.get(source).is_some_and(|s| *s != CheckStatus::Ok)
        };
        if open {
            self.raise(Change {
                source: source.to_string(),
                status: CheckStatus::Ok,
                title,
                detail,
                resolved: true,
            })
            .await;
        }
    }

    pub async fn metrics(&self, minutes: usize) -> Vec<MetricPoint> {
        let m = self.metrics.lock().await;
        let skip = m.len().saturating_sub(minutes);
        m.iter().skip(skip).copied().collect()
    }

    pub async fn uptime(&self) -> Vec<UptimeCheck> {
        self.uptime.lock().await.clone()
    }

    async fn record_metrics(&self, snap: &SystemSnapshot) {
        let pct = |a: u64, b: u64| {
            if b == 0 {
                0.0
            } else {
                (a as f64 * 100.0 / b as f64) as f32
            }
        };
        let root = snap.disks.iter().find(|d| d.mount_point == "/");
        let point = MetricPoint {
            t: unix_now(),
            cpu_pct: snap.cpu.usage_pct,
            mem_used_pct: pct(
                snap.memory.total_bytes.saturating_sub(snap.memory.available_bytes),
                snap.memory.total_bytes,
            ),
            swap_used_pct: pct(snap.memory.swap_used_bytes, snap.memory.swap_total_bytes),
            disk_root_pct: root.map(|d| pct(d.used_bytes, d.total_bytes)).unwrap_or(0.0),
            load1: snap.cpu.load_avg[0] as f32,
            temp_c: snap.thermal.max_c,
            mem_pressure: snap
                .hardware
                .as_ref()
                .and_then(|h| h.pressure)
                .and_then(|p| p.memory_some_avg10),
        };
        let mut m = self.metrics.lock().await;
        m.push_back(point);
        while m.len() > MAX_POINTS {
            m.pop_front();
        }
        // Every 10 minutes is plenty: a crash loses at most that.
        if m.len() % 10 == 0 {
            let json = serde_json::to_vec(&*m).unwrap_or_default();
            if let Err(e) = write_private(&Self::metrics_path(&self.cfg), &json) {
                tracing::warn!("metrics: could not save: {e}");
            }
        }
    }

    /// Turn doctor results and uptime into alerts.
    async fn evaluate(&self, checks: &[DoctorCheck], uptime: &[UptimeCheck]) {
        let mut rows: Vec<(String, CheckStatus, String, String)> = checks
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    c.status,
                    c.label.clone(),
                    match &c.fix {
                        Some(f) => format!("{} — {f}", c.detail),
                        None => c.detail.clone(),
                    },
                )
            })
            .collect();
        for u in uptime {
            rows.push((
                u.id.clone(),
                if u.up { CheckStatus::Ok } else { CheckStatus::Bad },
                u.name.clone(),
                if u.up {
                    format!("{} answers again", u.target)
                } else {
                    format!(
                        "{} is not answering ({})",
                        u.target,
                        u.error
                            .clone()
                            .or(u.http_status.map(|s| format!("HTTP {s}")))
                            .unwrap_or_default()
                    )
                },
            ));
        }
        let changes = {
            let store = self.store.lock().await;
            diff(&store.last, &rows)
        };
        for c in changes {
            tracing::info!("alert: {} — {}", c.title, c.detail);
            self.raise(c).await;
        }
    }

    async fn repair(&self, what: &'static str) -> bool {
        let mut r = self.repairs.lock().await;
        if r.get(what).is_some_and(|t| t.elapsed() < REPAIR_COOLDOWN) {
            return false;
        }
        r.insert(what, Instant::now());
        true
    }

    /// The system disk is nearly full: remove what can never be needed again
    /// (dangling images, build cache unused for 3 days). Never volumes, never
    /// tagged images (rollbacks use them). At most once an hour.
    async fn clean_disk(&self, snap: &SystemSnapshot) -> Option<String> {
        let root = snap.disks.iter().find(|d| d.mount_point == "/")?;
        if !needs_cleanup(root.used_bytes, root.total_bytes) || !self.repair_every("disk-cleanup", 3600).await {
            return None;
        }
        let docker = &self.cfg.docker_bin;
        let mut out = Vec::new();
        for args in [
            &["image", "prune", "-f"][..],
            &["builder", "prune", "-f", "--filter", "until=72h"][..],
        ] {
            if let Ok(o) = run(docker, args, Duration::from_secs(600)).await {
                if let Some(l) = o.stdout.lines().find(|l| l.contains("reclaimed")) {
                    out.push(l.trim().to_string());
                }
            }
        }
        Some(format!(
            "system disk was {}% full: removed unused images and old build cache ({})",
            root.used_bytes * 100 / root.total_bytes.max(1),
            if out.is_empty() {
                "nothing to remove".into()
            } else {
                out.join("; ")
            }
        ))
    }

    async fn repair_every(&self, what: &'static str, secs: u64) -> bool {
        let mut r = self.repairs.lock().await;
        if r.get(what).is_some_and(|t| t.elapsed() < Duration::from_secs(secs)) {
            return false;
        }
        r.insert(what, Instant::now());
        true
    }

    /// Put back what should be there. Returns what was repaired.
    async fn reconcile(&self) -> Vec<String> {
        let cfg = &self.cfg;
        let mut fixed = Vec::new();
        let unit_exists = |u: &str| cfg.systemd_dir.join(format!("{u}.service")).exists();
        let docker_up = run(
            &cfg.docker_bin,
            &["info", "--format", "{{.ServerVersion}}"],
            Duration::from_secs(15),
        )
        .await
        .is_ok_and(|o| o.success());
        if !docker_up && self.repair("docker").await {
            let _ = run(&cfg.systemctl_bin, &["start", "docker"], Duration::from_secs(90)).await;
            fixed.push("started Docker, which was not running".to_string());
        }
        if docker_up && unit_exists("vercelify-firewall") {
            let enabled = run(
                &cfg.systemctl_bin,
                &["is-enabled", "--quiet", "vercelify-firewall"],
                Duration::from_secs(10),
            )
            .await
            .is_ok_and(|o| o.success());
            let hooked = run(
                &cfg.iptables_bin,
                &["-w", "-C", "DOCKER-USER", "-j", "VERCELIFY-FWD"],
                Duration::from_secs(10),
            )
            .await
            .is_ok_and(|o| o.success());
            if enabled && !hooked && self.repair("firewall").await {
                let _ = run(
                    &cfg.systemctl_bin,
                    &["restart", "vercelify-firewall"],
                    Duration::from_secs(60),
                )
                .await;
                fixed.push("re-applied the firewall rules (Docker had dropped them)".to_string());
            }
        }
        // The power guard must always run (it shuts down on an empty battery).
        if unit_exists(crate::powerguard::UNIT) {
            let st = services::status(&cfg.systemctl_bin, crate::powerguard::UNIT).await;
            if st.enabled == Some(true)
                && matches!(st.active_state.as_str(), "failed" | "inactive")
                && self.repair("power-guard").await
            {
                let _ = run(
                    &cfg.systemctl_bin,
                    &["restart", crate::powerguard::UNIT],
                    Duration::from_secs(60),
                )
                .await;
                fixed.push("restarted the power guard, which had stopped".to_string());
            }
        }
        if unit_exists("cloudflared") {
            let st = services::status(&cfg.systemctl_bin, "cloudflared").await;
            if st.enabled == Some(true)
                && matches!(st.active_state.as_str(), "failed" | "inactive")
                && self.repair("cloudflared").await
            {
                let _ = run(&cfg.systemctl_bin, &["restart", "cloudflared"], Duration::from_secs(60)).await;
                fixed.push("restarted the Cloudflare tunnel".to_string());
            }
        }
        fixed
    }

    /// One pass of the loop.
    pub async fn tick(&self, s: &AppState) {
        let collector = s.collector.clone();
        let pg = s.config.power_guard_state.clone();
        if let Ok(snap) = tokio::task::spawn_blocking(move || {
            collector.snapshot(
                std::fs::read_to_string(pg)
                    .ok()
                    .and_then(|r| serde_json::from_str(&r).ok()),
            )
        })
        .await
        {
            self.record_metrics(&snap).await;
            if let Some(freed) = self.clean_disk(&snap).await {
                s.jobs.start("cleanup", "Freed disk space", None).done(freed.clone());
                self.raise(Change {
                    source: format!("cleanup:{}", unix_now()),
                    status: CheckStatus::Ok,
                    title: "Freed disk space automatically".into(),
                    detail: freed,
                    resolved: true,
                })
                .await;
            }
        }
        for what in self.reconcile().await {
            self.raise(Change {
                source: format!("repair:{}", unix_now()),
                status: CheckStatus::Ok,
                title: "Fixed automatically".into(),
                detail: what,
                resolved: true,
            })
            .await;
        }
        set_oom_priorities(&self.cfg).await;
        if self.cfg.role() != "worker" {
            s.sleep.tick().await;
        }
        let uptime = if self.cfg.uptime_checks && self.cfg.role() != "worker" {
            // A check is a visit: apps that sleep when idle aren't checked.
            let sleeping = s.sleep.enabled_apps().await;
            let checks = check_uptime(&self.cfg, &self.uptime.lock().await.clone(), &sleeping).await;
            *self.uptime.lock().await = checks.clone();
            checks
        } else {
            vec![]
        };
        let report = doctor::report(s).await;
        self.evaluate(&report.checks, &uptime).await;
    }

    pub async fn run(self: std::sync::Arc<Self>, s: AppState) {
        let every = self.cfg.watch_interval_secs;
        if every == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_secs(20)).await;
        loop {
            self.tick(&s).await;
            tokio::time::sleep(Duration::from_secs(every.max(15))).await;
        }
    }
}

pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// POST an alert with curl (TLS without linking a TLS stack into the agent).
/// The URL and headers go in a private curl config file, not on the command
/// line, so `ps` never shows the secret topic or webhook.
async fn send(cfg: &Config, url: &str, alert: &Alert) -> bool {
    use tokio::io::AsyncWriteExt;
    let (ctype, body) = notification_body(url, &alert.title, &alert.detail);
    let priority = match (alert.resolved, alert.status) {
        (false, CheckStatus::Bad) => "high",
        _ => "default",
    };
    let host = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    let clean = |s: &str| s.replace(['"', '\n', '\r', '\\'], " ");
    let curl_cfg = format!(
        "url = \"{}\"\nheader = \"Title: Vercelify {}: {}\"\nheader = \"Priority: {priority}\"\nheader = \"Content-Type: {ctype}\"\n",
        clean(url),
        clean(host.trim()),
        clean(&alert.title)
    );
    let cfg_file = cfg.run_dir.join(format!("notify-{}.curl", alert.id));
    if write_private(&cfg_file, curl_cfg.as_bytes()).is_err() {
        return false;
    }
    let spawned = tokio::process::Command::new(&cfg.curl_bin)
        .args(["-fsS", "-m", "15", "-X", "POST", "--data-binary", "@-", "-K"])
        .arg(&cfg_file)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let ok = match spawned {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(body.as_bytes()).await;
            }
            matches!(
                tokio::time::timeout(Duration::from_secs(20), child.wait()).await,
                Ok(Ok(st)) if st.success()
            )
        }
        Err(_) => false,
    };
    let _ = std::fs::remove_file(&cfg_file);
    ok
}

/// Write `oom_score_adj` for every process of every classified container.
async fn set_oom_priorities(cfg: &Config) {
    let Ok(out) = run(
        &cfg.docker_bin,
        &[
            "ps",
            "--format",
            "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.Label \"coolify.pullRequestId\"}}",
        ],
        Duration::from_secs(15),
    )
    .await
    else {
        return;
    };
    let mut wanted: Vec<(String, i32)> = Vec::new();
    for line in out.stdout.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 3 {
            continue;
        }
        if let Some(score) = oom_score_for(f[1], f[2], f.get(3).copied().unwrap_or("")) {
            wanted.push((f[0].to_string(), score));
        }
    }
    if wanted.is_empty() {
        return;
    }
    let mut args = vec!["inspect", "-f", "{{.Id}} {{.State.Pid}}"];
    args.extend(wanted.iter().map(|(id, _)| id.as_str()));
    let Ok(pids) = run(&cfg.docker_bin, &args, Duration::from_secs(15)).await else {
        return;
    };
    for line in pids.stdout.lines() {
        let Some((id, pid)) = line.split_once(' ') else {
            continue;
        };
        let Some(score) = wanted.iter().find(|(w, _)| id.starts_with(w.as_str())).map(|(_, s)| *s) else {
            continue;
        };
        for p in container_pids(pid.trim()) {
            let path = format!("/proc/{p}/oom_score_adj");
            if std::fs::read_to_string(&path).ok().map(|v| v.trim().to_string()) != Some(score.to_string()) {
                let _ = std::fs::write(&path, score.to_string());
            }
        }
    }
}

/// All processes in the cgroup of `pid` (falls back to just `pid`).
fn container_pids(pid: &str) -> Vec<String> {
    if pid.is_empty() || pid == "0" {
        return vec![];
    }
    let cg = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .ok()
        .and_then(|c| c.lines().find_map(|l| l.strip_prefix("0::").map(str::to_string)));
    cg.and_then(|p| std::fs::read_to_string(format!("/sys/fs/cgroup{p}/cgroup.procs")).ok())
        .map(|procs| procs.split_whitespace().map(String::from).collect())
        .unwrap_or_else(|| vec![pid.to_string()])
}

#[derive(Debug, Clone)]
struct Target {
    id: String,
    name: String,
    url: String,
    /// Address to connect to instead of DNS (the server running the app).
    resolve: Option<String>,
}

/// Apps (from Coolify's DB, with the server each runs on) and pooled servers.
async fn targets(cfg: &Config) -> Vec<Target> {
    let sql = "select a.uuid, a.name, coalesce(a.fqdn,''), coalesce(s.ip,''), coalesce(a.status,'') \
               from applications a \
               left join standalone_dockers d on d.id = a.destination_id \
               left join servers s on s.id = d.server_id \
               where a.deleted_at is null";
    let mut out = Vec::new();
    // Older Coolify versions have no soft deletes.
    let rows = match crate::coolify::query(cfg, sql).await {
        Ok(r) => r,
        Err(_) => crate::coolify::query(cfg, sql.trim_end_matches("where a.deleted_at is null"))
            .await
            .unwrap_or_default(),
    };
    for r in rows {
        if r.len() < 5 || r[2].is_empty() || r[4].starts_with("exited") {
            // Stopped on purpose: nothing to check.
            continue;
        }
        let ip = match r[3].as_str() {
            "" | "host.docker.internal" | "localhost" | "127.0.0.1" => "127.0.0.1".to_string(),
            other => other.to_string(),
        };
        if let Some(url) = r[2].split(',').map(str::trim).find(|u| u.starts_with("http")) {
            out.push(Target {
                id: format!("app:{}", r[0]),
                name: format!("App {}", r[1]),
                url: url.to_string(),
                resolve: Some(ip),
            });
        }
    }
    let servers = crate::coolify::query(cfg, "select name, ip from servers where id <> 0")
        .await
        .unwrap_or_default();
    for r in servers {
        if r.len() == 2 && !r[1].is_empty() {
            out.push(Target {
                id: format!("server:{}", r[0]),
                name: format!("Server {}", r[0]),
                url: format!("http://{}:{}/v1/health", r[1], cfg.port),
                resolve: None,
            });
        }
    }
    out
}

/// (`host`, `port`) of an http(s) URL.
pub fn host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h, p.parse().ok()?),
        _ => (authority, if scheme == "https" { 443 } else { 80 }),
    };
    (!host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')))
    .then(|| (host.to_string(), port))
}

/// `curl -w '%{http_code} %{time_total}'` → (status, ms).
pub fn parse_curl_w(out: &str) -> Option<(u16, u32)> {
    let mut it = out.split_whitespace();
    let code: u16 = it.next()?.parse().ok()?;
    let secs: f64 = it.next()?.parse().ok()?;
    Some((code, (secs * 1000.0) as u32))
}

async fn check_uptime(
    cfg: &Config,
    previous: &[UptimeCheck],
    skip_apps: &std::collections::HashSet<String>,
) -> Vec<UptimeCheck> {
    let mut out = Vec::new();
    for t in targets(cfg).await {
        if t.id.strip_prefix("app:").is_some_and(|u| skip_apps.contains(u)) {
            continue;
        }
        let checked_at = now();
        let (up, status, latency, error) = if t.id.starts_with("server:") {
            let start = Instant::now();
            match http::call(Req::get(&t.url).timeout(Duration::from_secs(8))).await {
                Ok((s, _)) => (
                    s.is_success(),
                    Some(s.as_u16()),
                    Some(start.elapsed().as_millis() as u32),
                    None,
                ),
                Err(e) => (false, None, None, Some(e.to_string())),
            }
        } else {
            let Some((host, port)) = host_port(&t.url) else {
                continue;
            };
            let mut args: Vec<String> = vec![
                "-sS".into(),
                "-k".into(),
                "-o".into(),
                "/dev/null".into(),
                "-m".into(),
                "10".into(),
                "-w".into(),
                "%{http_code} %{time_total}".into(),
            ];
            if let Some(ip) = &t.resolve {
                args.push("--resolve".into());
                args.push(format!("{host}:{port}:{ip}"));
            }
            args.push(t.url.clone());
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            match run(&cfg.curl_bin, &refs, Duration::from_secs(15)).await {
                Ok(o) => match parse_curl_w(&o.stdout) {
                    // 5xx (and no answer) = down; a 404 at / is still an app answering.
                    Some((code, ms)) if code > 0 && code < 500 => (true, Some(code), Some(ms), None),
                    Some((code, ms)) if code > 0 => (false, Some(code), Some(ms), None),
                    _ => (false, None, None, Some(o.stderr.trim().chars().take(200).collect())),
                },
                Err(e) => (false, None, None, Some(e.to_string())),
            }
        };
        let since = previous
            .iter()
            .find(|p| p.id == t.id && p.up == up)
            .map(|p| p.since.clone())
            .unwrap_or_else(|| checked_at.clone());
        out.push(UptimeCheck {
            id: t.id,
            name: t.name,
            target: t.url,
            up,
            http_status: status,
            latency_ms: latency,
            checked_at,
            since,
            error,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, st: CheckStatus) -> (String, CheckStatus, String, String) {
        (id.into(), st, id.to_uppercase(), "detail".into())
    }

    #[test]
    fn only_changes_alert() {
        let mut last = HashMap::new();
        let c = diff(&last, &[row("disk", CheckStatus::Ok), row("temp", CheckStatus::Warn)]);
        assert_eq!(c.len(), 1, "new OK sources are silent");
        assert_eq!(c[0].source, "temp");
        assert!(!c[0].resolved);
        last.insert("temp".to_string(), CheckStatus::Warn);
        assert!(diff(&last, &[row("temp", CheckStatus::Warn)]).is_empty(), "no repeat");
        let back = diff(&last, &[row("temp", CheckStatus::Ok)]);
        assert!(back[0].resolved);
        assert!(back[0].title.contains("back to normal"));
        let worse = diff(&last, &[row("temp", CheckStatus::Bad)]);
        assert_eq!(worse[0].status, CheckStatus::Bad);
    }

    #[test]
    fn urls() {
        assert_eq!(mask_url("https://ntfy.sh/my-secret-topic"), "https://ntfy.sh/•••");
        assert_eq!(mask_url("garbage"), "•••");
        assert!(valid_notify_url("https://ntfy.sh/x"));
        assert!(!valid_notify_url("ftp://x"));
        assert!(!valid_notify_url("https://a b"));
        let (t, b) = notification_body("https://discord.com/api/webhooks/1/x", "T", "D");
        assert_eq!(t, "application/json");
        assert!(b.contains("\"content\":\"T\\nD\""));
        let (t, b) = notification_body("https://ntfy.sh/x", "T", "D");
        assert_eq!((t, b.as_str()), ("text/plain", "D"));
        assert_eq!(
            host_port("https://blog.example.com/x"),
            Some(("blog.example.com".into(), 443))
        );
        assert_eq!(
            host_port("http://a.100.1.2.3.sslip.io:8080"),
            Some(("a.100.1.2.3.sslip.io".into(), 8080))
        );
        assert_eq!(host_port("http://bad host/"), None);
        assert_eq!(parse_curl_w("200 0.123456"), Some((200, 123)));
        assert_eq!(parse_curl_w("000 0.0"), Some((0, 0)));
        assert_eq!(parse_curl_w(""), None);
    }

    #[tokio::test]
    async fn one_off_notices_and_resolving() {
        let dir = tempfile::tempdir().unwrap();
        let w = Watcher::load(
            std::sync::Arc::new(Config {
                state_dir: dir.path().into(),
                power_guard_conf: dir.path().join("none"),
                power_guard_overrides: dir.path().join("none2"),
                ..Config::default()
            }),
            Default::default(),
        );
        for i in 0..3 {
            w.raise(Change {
                source: format!("repair:{i}"),
                status: CheckStatus::Ok,
                title: "Fixed".into(),
                detail: String::new(),
                resolved: true,
            })
            .await;
        }
        assert!(w.store.lock().await.last.is_empty(), "one-off notices aren't tracked");
        // resolving something that isn't open does nothing
        w.resolve("deploy:a1", "ok".into(), String::new()).await;
        assert_eq!(w.status().await.alerts.len(), 3);
        w.raise(Change {
            source: "deploy:a1".into(),
            status: CheckStatus::Bad,
            title: "blog: deploy failed".into(),
            detail: String::new(),
            resolved: false,
        })
        .await;
        w.resolve("deploy:a1", "blog: deployed again".into(), String::new())
            .await;
        let st = w.status().await;
        assert_eq!(st.alerts.len(), 5);
        assert!(st.alerts[0].resolved && st.alerts[0].title == "blog: deployed again");
        w.resolve("deploy:a1", "again".into(), String::new()).await;
        assert_eq!(w.status().await.alerts.len(), 5, "already resolved");
    }

    #[test]
    fn cleanup_threshold() {
        assert!(needs_cleanup(91, 100));
        assert!(needs_cleanup(90, 100));
        assert!(!needs_cleanup(89, 100));
        assert!(!needs_cleanup(0, 0));
    }

    #[test]
    fn oom_classes() {
        assert_eq!(oom_score_for("coolify-db", "postgres:15", ""), Some(-500));
        assert_eq!(oom_score_for("pg-abc", "postgres:16-alpine", ""), Some(-500));
        assert_eq!(oom_score_for("coolify-proxy", "traefik:v3", ""), Some(-300));
        assert_eq!(
            oom_score_for("x1y2", "ghcr.io/coollabsio/coolify-helper:1.0", ""),
            Some(900)
        );
        assert_eq!(oom_score_for("app-pr-4", "app:abc", "4"), Some(600));
        assert_eq!(oom_score_for("app", "app:abc", "0"), None);
        assert_eq!(oom_score_for("vercelify-ollama", "ollama/ollama:0.12.3", ""), Some(500));
    }

    #[tokio::test]
    async fn alerts_persist_and_dedupe() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = std::sync::Arc::new(Config {
            state_dir: dir.path().into(),
            power_guard_conf: dir.path().join("none"),
            power_guard_overrides: dir.path().join("none2"),
            ..Config::default()
        });
        let events = std::sync::Arc::new(crate::events::Events::default());
        let mut rx = events.subscribe();
        let w = Watcher::load(cfg.clone(), events);
        let checks = vec![DoctorCheck {
            id: "disk".into(),
            label: "System disk".into(),
            status: CheckStatus::Bad,
            detail: "96% used".into(),
            fix: Some("prune".into()),
        }];
        w.evaluate(&checks, &[]).await;
        w.evaluate(&checks, &[]).await;
        let st = w.status().await;
        assert_eq!(st.alerts.len(), 1);
        assert_eq!(st.alerts[0].detail, "96% used — prune");
        assert_eq!(st.alerts[0].notified, None, "no target configured");
        // Pushed to the live stream too.
        assert_eq!(rx.try_recv().unwrap().alert.unwrap().id, st.alerts[0].id);
        assert!(st.notify_target.is_none());
        // A restarted agent remembers it already alerted.
        let again = Watcher::load(cfg, Default::default());
        again.evaluate(&checks, &[]).await;
        assert_eq!(again.status().await.alerts.len(), 1);
        assert!(again.set_notify_url("nope").await.is_err());
        again.set_notify_url("https://ntfy.sh/secret").await.unwrap();
        assert_eq!(
            again.status().await.notify_target.as_deref(),
            Some("https://ntfy.sh/•••")
        );
        let raw = std::fs::read_to_string(dir.path().join("alerts.json")).unwrap();
        assert!(raw.contains("ntfy.sh/secret"), "stored for sending");
    }
}

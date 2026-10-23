//! Auto-deploy from a branch without webhooks.
//!
//! GitHub can't deliver push webhooks to a Coolify that is private to the
//! tailnet (or behind CGNAT), so the agent polls instead: every
//! `autodeploy_interval_secs` it asks the remote for the head of each
//! watched branch (`git ls-remote`, a few hundred bytes, no clone) and
//! queues a Coolify deployment when it moved. Works for any Git host.
//!
//! On GitHub it can also
//! - deploy a preview for every open pull request (and remove it when the
//!   pull request closes), without webhooks either,
//! - report each deploy as a commit status and comment the preview URL, and
//! - roll back automatically when a new version comes up unhealthy.

use crate::config::Config;
use crate::error::AppError;
use crate::exec::run_env;
use crate::github;
use crate::http::{self, Req};
use crate::watch::{Change, Watcher};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Mutex;
use vercelify_core::{AutoDeployRequest, AutoDeployStatus, AutoDeployWatch, CheckStatus, PreviewWatch};

const GIT_TIMEOUT: Duration = Duration::from_secs(25);
const MIN_INTERVAL: u64 = 15;
/// Without a token GitHub allows 60 API calls an hour: ask every 5 minutes.
const ANONYMOUS_PR_POLL_SECS: u64 = 300;
/// How long a new version is watched for health before it counts as good.
const HEALTH_WINDOW_SECS: u64 = 180;
/// Deployments still unfinished after this are given up on.
const DEPLOY_TIMEOUT_SECS: u64 = 3600;
const STATUS_CONTEXT: &str = "vercelify";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
struct StoredPreview {
    number: u64,
    title: String,
    head_sha: String,
    url: Option<String>,
    deployed_at: Option<String>,
    #[serde(default)]
    comment_id: Option<u64>,
}

/// A deployment the agent queued and is following to the end.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
struct Pending {
    deployment_uuid: String,
    commit: String,
    /// What was live before (the rollback target).
    previous: Option<String>,
    started_at: u64,
    finished_at: Option<u64>,
    /// This deployment is itself a rollback: never roll it back again.
    #[serde(default)]
    rollback: bool,
    /// Its entry in the activity list.
    #[serde(default)]
    job_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
struct StoredWatch {
    app_uuid: String,
    name: String,
    repo: String,
    branch: String,
    enabled: bool,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    last_commit: Option<String>,
    #[serde(default)]
    last_checked_at: Option<String>,
    #[serde(default)]
    last_deployed_commit: Option<String>,
    /// What runs now: the head seen first, then each deploy or rollback
    /// (not commits skipped as irrelevant). Changes are measured against it
    /// and rollbacks return to it.
    #[serde(default)]
    live_commit: Option<String>,
    #[serde(default)]
    last_deployed_at: Option<String>,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    deploys: u32,
    #[serde(default)]
    previews: bool,
    #[serde(default)]
    auto_rollback: bool,
    #[serde(default)]
    github_feedback: bool,
    #[serde(default)]
    open_previews: Vec<StoredPreview>,
    #[serde(default)]
    rollbacks: u32,
    #[serde(default)]
    pending: Option<Pending>,
    /// The app is pinned to an older commit after a rollback.
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    last_pr_poll: Option<u64>,
    /// Pushes not built because nothing relevant changed.
    #[serde(default)]
    skipped: u32,
    #[serde(default)]
    last_skip: Option<String>,
}

impl StoredWatch {
    fn public(&self) -> AutoDeployWatch {
        AutoDeployWatch {
            app_uuid: self.app_uuid.clone(),
            name: self.name.clone(),
            repo: self.repo.clone(),
            branch: self.branch.clone(),
            enabled: self.enabled,
            has_token: self.token.is_some(),
            last_commit: self.last_commit.clone(),
            last_checked_at: self.last_checked_at.clone(),
            last_deployed_commit: self.last_deployed_commit.clone(),
            last_deployed_at: self.last_deployed_at.clone(),
            last_error: self.last_error.clone(),
            deploys: self.deploys,
            previews: self.previews,
            auto_rollback: self.auto_rollback,
            github_feedback: self.github_feedback,
            open_previews: self
                .open_previews
                .iter()
                .map(|p| PreviewWatch {
                    number: p.number,
                    title: p.title.clone(),
                    head_sha: p.head_sha.clone(),
                    url: p.url.clone(),
                    deployed_at: p.deployed_at.clone(),
                })
                .collect(),
            rollbacks: self.rollbacks,
            skipped: self.skipped,
            last_skip: self.last_skip.clone(),
        }
    }

    fn slug(&self) -> Option<String> {
        github::repo_slug(&self.repo)
    }

    /// The commit that runs now (older stores: the last one deployed or seen).
    fn live(&self) -> Option<&str> {
        self.live_commit
            .as_deref()
            .or(self.last_deployed_commit.as_deref())
            .or(self.last_commit.as_deref())
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Store {
    watches: Vec<StoredWatch>,
}

/// What to do after learning the branch head.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// First check: remember the head, don't deploy what is already live.
    Baseline,
    Unchanged,
    Deploy,
}

pub fn decide(last_commit: Option<&str>, head: &str) -> Action {
    match last_commit {
        None => Action::Baseline,
        Some(c) if c == head => Action::Unchanged,
        Some(_) => Action::Deploy,
    }
}

/// What the previews of a repository should become.
#[derive(Debug, PartialEq, Default)]
pub struct PreviewPlan {
    /// (number, head sha, title): deploy or redeploy.
    pub deploy: Vec<(u64, String, String)>,
    /// Pull requests that closed: remove their preview.
    pub remove: Vec<u64>,
    /// Fork pull requests left alone (someone else's code).
    pub skipped_forks: Vec<u64>,
    /// Over the per-app limit.
    pub over_limit: Vec<u64>,
}

pub fn plan_previews(open: &[github::PullRequest], live: &[(u64, String)], max: usize) -> PreviewPlan {
    let mut plan = PreviewPlan::default();
    let is_open = |n: u64| open.iter().any(|p| p.number == n && !p.from_fork);
    // Live previews keep their slot; new pull requests take what is left.
    let mut used = live.iter().filter(|(n, _)| is_open(*n)).count();
    for pr in open {
        if pr.from_fork {
            plan.skipped_forks.push(pr.number);
            continue;
        }
        match live.iter().find(|(n, _)| *n == pr.number) {
            Some((_, sha)) if *sha == pr.head_sha => {}
            Some(_) => plan.deploy.push((pr.number, pr.head_sha.clone(), pr.title.clone())),
            None if used < max => {
                used += 1;
                plan.deploy.push((pr.number, pr.head_sha.clone(), pr.title.clone()));
            }
            None => plan.over_limit.push(pr.number),
        }
    }
    for (n, _) in live {
        if !is_open(*n) {
            plan.remove.push(*n);
        }
    }
    plan
}

/// Coolify's `status` of an app ("running:healthy", "exited:unhealthy"…)
/// means the new version is broken.
pub fn unhealthy(status: &str) -> bool {
    let s = status.to_ascii_lowercase();
    s.contains("unhealthy") || s.starts_with("exited") || s.starts_with("restarting") || s.starts_with("degraded")
}

/// `https://app.example.com` + template `pr-{{pr_id}}-{{domain}}` + 7 →
/// `https://pr-7-app.example.com`.
pub fn preview_url(fqdn: &str, template: Option<&str>, pr: u64) -> Option<String> {
    let first = fqdn.split(',').map(str::trim).find(|u| u.starts_with("http"))?;
    let (scheme, rest) = first.split_once("://")?;
    let host = rest.split('/').next()?;
    let tpl = template.filter(|t| !t.is_empty()).unwrap_or("{{pr_id}}.{{domain}}");
    Some(format!(
        "{scheme}://{}",
        tpl.replace("{{pr_id}}", &pr.to_string()).replace("{{domain}}", host)
    ))
}

pub fn valid_app_uuid(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// https:// (or http:// without a token) URL with no credentials or oddities.
pub fn valid_repo(s: &str) -> bool {
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"));
    match rest {
        Some(r) => {
            s.len() <= 512
                && r.contains('/')
                && !r.starts_with('/')
                && !r.split('/').next().unwrap_or("").contains('@')
                && s.chars().all(|c| c.is_ascii_graphic())
        }
        None => false,
    }
}

/// A conservative subset of git's ref-name rules.
pub fn valid_branch(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && !s.starts_with(['-', '/', '.'])
        && !s.ends_with(['/', '.'])
        && !s.ends_with(".lock")
        && !s.contains("..")
        && !s.contains("//")
        && !s.contains("@{")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

fn valid_token(s: &str) -> bool {
    s.len() <= 512 && s.chars().all(|c| c.is_ascii_graphic())
}

/// Commit of `refs/heads/<branch>` in `git ls-remote` output.
pub fn parse_ls_remote(stdout: &str, branch: &str) -> Option<String> {
    let want = format!("refs/heads/{branch}");
    stdout.lines().find_map(|l| {
        let (sha, r) = l.split_once('\t')?;
        (r.trim() == want && sha.len() >= 40 && sha.chars().all(|c| c.is_ascii_hexdigit())).then(|| sha.to_string())
    })
}

/// Coolify's watch-path glob (`Application::globToRegex`): `*` and `?`
/// stay within a directory, `**` (and `**/`) cross them, `[…]`/`[!…]` are
/// classes.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    #[derive(Debug)]
    enum T {
        Lit(char),
        Star,
        Any,
        One,
        Class(bool, Vec<(char, char)>),
    }
    let mut toks = Vec::new();
    let p: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            '*' if p.get(i + 1) == Some(&'*') => {
                toks.push(T::Any);
                i += 2;
                if p.get(i) == Some(&'/') {
                    i += 1;
                }
                continue;
            }
            '*' => toks.push(T::Star),
            '?' => toks.push(T::One),
            '[' if p[i + 1..].contains(&']') => {
                i += 1;
                let neg = matches!(p.get(i), Some('!' | '^'));
                if neg {
                    i += 1;
                }
                let mut set = Vec::new();
                while i < p.len() && p[i] != ']' {
                    if p.get(i + 1) == Some(&'-') && p.get(i + 2).is_some_and(|c| *c != ']') {
                        set.push((p[i], p[i + 2]));
                        i += 3;
                    } else {
                        set.push((p[i], p[i]));
                        i += 1;
                    }
                }
                toks.push(T::Class(neg, set));
            }
            c => toks.push(T::Lit(c)),
        }
        i += 1;
    }
    fn go(t: &[T], s: &[char]) -> bool {
        match t.first() {
            None => s.is_empty(),
            Some(T::Lit(c)) => s.first() == Some(c) && go(&t[1..], &s[1..]),
            Some(T::One) => s.first().is_some_and(|c| *c != '/') && go(&t[1..], &s[1..]),
            Some(T::Class(neg, set)) => {
                s.first()
                    .is_some_and(|c| set.iter().any(|(a, b)| a <= c && c <= b) != *neg)
                    && go(&t[1..], &s[1..])
            }
            Some(T::Star) => (0..=s.len())
                .take_while(|&k| k == 0 || s[k - 1] != '/')
                .any(|k| go(&t[1..], &s[k..])),
            Some(T::Any) => (0..=s.len()).any(|k| go(&t[1..], &s[k..])),
        }
    }
    let s: Vec<char> = path.chars().collect();
    go(&toks, &s)
}

/// Coolify's `matchPaths`: the last matching pattern decides (`!` excludes);
/// a file no pattern matches counts only when all patterns are exclusions.
pub fn watched(file: &str, patterns: &[&str]) -> bool {
    let mut decision = None;
    for p in patterns {
        let (exclude, pat) = match p.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, *p),
        };
        if glob_match(pat, file) {
            decision = Some(!exclude);
        }
    }
    decision.unwrap_or_else(|| patterns.iter().all(|p| p.starts_with('!')))
}

const SKIP_MARKERS: [&str; 4] = ["[skip ci]", "[ci skip]", "[skip deploy]", "[deploy skip]"];

/// Why a push needn't be built (Vercel's "Ignored Build Step"): the newest
/// commit asks so, or no changed file is in the app's watch paths. None:
/// build it (also whenever the list of changes may be incomplete).
pub fn skip_reason(changes: &github::Changes, watch_paths: Option<&str>) -> Option<String> {
    if let Some(msg) = changes.messages.last() {
        let lower = msg.to_ascii_lowercase();
        if let Some(m) = SKIP_MARKERS.iter().find(|m| lower.contains(*m)) {
            return Some(format!("the commit message says {m}"));
        }
    }
    let patterns: Vec<&str> = watch_paths
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if patterns.is_empty() || changes.truncated || changes.files.is_empty() {
        return None;
    }
    (!changes.files.iter().any(|f| watched(f, &patterns)))
        .then(|| "no changed file is in the app's watch paths".to_string())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn redact(text: &str, token: Option<&str>) -> String {
    let mut t: String = text.trim().chars().take(400).collect();
    if let Some(tok) = token.filter(|t| t.len() >= 4) {
        t = t.replace(tok, "***");
    }
    t
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

pub struct AutoDeploy {
    cfg: Arc<Config>,
    store: Mutex<Store>,
    watcher: OnceLock<Arc<Watcher>>,
    jobs: OnceLock<Arc<crate::events::Jobs>>,
    projects: OnceLock<Arc<crate::projects::Projects>>,
}

impl AutoDeploy {
    fn path(cfg: &Config) -> PathBuf {
        cfg.state_dir.join("autodeploy.json")
    }

    pub fn load(cfg: Arc<Config>) -> Self {
        let path = Self::path(&cfg);
        let store = match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
                tracing::warn!("{} is unreadable ({e}); starting with no watches", path.display());
                let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                Store::default()
            }),
            Err(_) => Store::default(),
        };
        Self {
            cfg,
            store: Mutex::new(store),
            watcher: OnceLock::new(),
            jobs: OnceLock::new(),
            projects: OnceLock::new(),
        }
    }

    /// Deploys and rollbacks show up in the activity list.
    pub fn set_jobs(&self, j: Arc<crate::events::Jobs>) {
        let _ = self.jobs.set(j);
    }

    fn job(&self, kind: &str, title: String, app_uuid: &str) -> Option<String> {
        self.jobs.get().map(|j| j.start(kind, title, Some(app_uuid)).detach())
    }

    fn job_done(&self, p: &Pending, result: Result<String, String>) {
        if let (Some(j), Some(id)) = (self.jobs.get(), p.job_id.as_deref()) {
            j.finish_by_id(id, result);
        }
    }

    /// Keeps each deployed commit's vercel.json applied.
    pub fn set_projects(&self, p: Arc<crate::projects::Projects>) {
        let _ = self.projects.set(p);
    }

    /// Where deploy failures and rollbacks are reported.
    pub fn set_watcher(&self, w: Arc<Watcher>) {
        let _ = self.watcher.set(w);
    }

    async fn alert(&self, source: String, status: CheckStatus, title: String, detail: String) {
        if let Some(w) = self.watcher.get() {
            w.raise(Change {
                source,
                status,
                title,
                detail,
                resolved: status == CheckStatus::Ok,
            })
            .await;
        }
    }

    /// 0 = no background checks (only "Check now").
    fn interval(&self) -> u64 {
        match self.cfg.autodeploy_interval_secs {
            0 => 0,
            n => n.max(MIN_INTERVAL),
        }
    }

    fn save(&self, store: &Store) -> Result<(), AppError> {
        let json = serde_json::to_vec_pretty(store).expect("serializable");
        crate::watch::write_private(&Self::path(&self.cfg), &json)
            .map_err(|e| AppError::internal(format!("writing {}: {e}", Self::path(&self.cfg).display())))
    }

    pub async fn status(&self) -> AutoDeployStatus {
        let store = self.store.lock().await;
        AutoDeployStatus {
            interval_secs: self.interval(),
            coolify_configured: self.cfg.coolify_token().is_some(),
            watches: store.watches.iter().map(StoredWatch::public).collect(),
        }
    }

    pub async fn upsert(&self, req: AutoDeployRequest) -> Result<AutoDeployWatch, AppError> {
        let repo = req.repo.trim().to_string();
        let branch = req.branch.trim().to_string();
        if !valid_app_uuid(&req.app_uuid) {
            return Err(AppError::bad_request("invalid application uuid"));
        }
        if !valid_repo(&repo) {
            return Err(AppError::bad_request(
                "repository must be an https:// clone URL, e.g. https://github.com/you/app.git",
            ));
        }
        if !valid_branch(&branch) {
            return Err(AppError::bad_request(format!("`{branch}` is not a valid branch name")));
        }
        let token = match req.token.as_deref().map(str::trim) {
            Some("") => Some(None),
            Some(t) if valid_token(t) => Some(Some(t.to_string())),
            Some(_) => return Err(AppError::bad_request("the access token contains invalid characters")),
            None => None,
        };
        let name: String = req
            .name
            .as_deref()
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_control())
            .take(100)
            .collect();
        // Added from the CLI (no name): Coolify knows it; alerts read better.
        let name = if name.is_empty() {
            self.app(&req.app_uuid)
                .await
                .and_then(|a| {
                    a.get("name")?
                        .as_str()
                        .map(|n| n.chars().filter(|c| !c.is_control()).take(100).collect())
                })
                .unwrap_or_default()
        } else {
            name
        };

        let mut store = self.store.lock().await;
        let idx = store.watches.iter().position(|w| w.app_uuid == req.app_uuid);
        let mut w = idx.map(|i| store.watches[i].clone()).unwrap_or_else(|| StoredWatch {
            app_uuid: req.app_uuid.clone(),
            ..Default::default()
        });
        if w.repo != repo || w.branch != branch {
            // A different branch: start from its current head.
            w.last_commit = None;
            w.live_commit = None;
            w.last_error = None;
            w.pending = None;
        }
        w.repo = repo;
        w.branch = branch;
        w.enabled = req.enabled.unwrap_or(true);
        if !name.is_empty() || w.name.is_empty() {
            w.name = if name.is_empty() { w.app_uuid.clone() } else { name };
        }
        if let Some(t) = token {
            w.token = t;
        }
        if let Some(p) = req.previews {
            w.previews = p;
        }
        if let Some(r) = req.auto_rollback {
            w.auto_rollback = r;
        }
        if let Some(g) = req.github_feedback {
            w.github_feedback = g;
        }
        if w.token.is_some() && w.repo.starts_with("http://") {
            return Err(AppError::bad_request(
                "use an https:// URL when sending an access token",
            ));
        }
        if (w.previews || w.github_feedback) && w.slug().is_none() {
            return Err(AppError::bad_request(
                "pull request previews and GitHub statuses need a https://github.com/owner/repo URL",
            ));
        }
        if w.github_feedback && w.token.is_none() {
            return Err(AppError::bad_request(
                "reporting to GitHub needs a token with Commit statuses and Pull requests write access",
            ));
        }
        match idx {
            Some(i) => store.watches[i] = w.clone(),
            None => store.watches.push(w.clone()),
        }
        self.save(&store)?;
        Ok(w.public())
    }

    pub async fn remove(&self, app_uuid: &str) -> Result<(), AppError> {
        let mut store = self.store.lock().await;
        let before = store.watches.len();
        store.watches.retain(|w| w.app_uuid != app_uuid);
        if store.watches.len() == before {
            return Err(AppError::not_found("no auto-deploy for this application"));
        }
        self.save(&store)
    }

    /// Check one watch now (button in the cockpit). Also works when disabled.
    pub async fn check_now(&self, app_uuid: &str) -> Result<AutoDeployWatch, AppError> {
        let w = {
            let store = self.store.lock().await;
            store
                .watches
                .iter()
                .find(|w| w.app_uuid == app_uuid)
                .cloned()
                .ok_or_else(|| AppError::not_found("no auto-deploy for this application"))?
        };
        self.check(w).await;
        let store = self.store.lock().await;
        store
            .watches
            .iter()
            .find(|w| w.app_uuid == app_uuid)
            .map(StoredWatch::public)
            .ok_or_else(|| AppError::not_found("removed while checking"))
    }

    /// Check every enabled watch once.
    pub async fn tick(&self) {
        let watches: Vec<StoredWatch> = {
            let store = self.store.lock().await;
            store.watches.iter().filter(|w| w.enabled).cloned().collect()
        };
        for w in watches {
            self.check(w).await;
        }
    }

    /// Background loop; the first check runs shortly after start.
    pub async fn run(self: Arc<Self>) {
        if self.interval() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
        loop {
            self.tick().await;
            tokio::time::sleep(Duration::from_secs(self.interval())).await;
        }
    }

    fn power_guard_busy(&self) -> Option<String> {
        let raw = std::fs::read_to_string(&self.cfg.power_guard_state).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let state = v.get("state")?.as_str()?;
        matches!(state, "thermal_paused" | "shutting_down").then(|| state.to_string())
    }

    /// Run `f` on the stored copy of `w` (if it still exists) and save.
    async fn update<T>(&self, w: &StoredWatch, f: impl FnOnce(&mut StoredWatch) -> T) -> Option<T> {
        let mut store = self.store.lock().await;
        // The watch may have been removed or retargeted while we were away.
        let entry = store
            .watches
            .iter_mut()
            .find(|x| x.app_uuid == w.app_uuid && x.repo == w.repo && x.branch == w.branch)?;
        let out = f(entry);
        if let Err(e) = self.save(&store) {
            tracing::warn!("auto-deploy: could not save state: {}", e.message);
        }
        Some(out)
    }

    async fn check(&self, w: StoredWatch) {
        if w.pending.is_some() {
            self.follow_up(&w).await;
        }
        let head = ls_remote(&self.cfg, &w.repo, &w.branch, w.token.as_deref()).await;
        let mut outcome = Outcome {
            checked_at: now(),
            ..Default::default()
        };
        match head {
            Err(e) => outcome.error = Some(e),
            Ok(head) => match decide(w.last_commit.as_deref(), &head) {
                Action::Baseline | Action::Unchanged => outcome.commit = Some(head),
                Action::Deploy => {
                    if let Some(state) = self.power_guard_busy() {
                        // Leave last_commit alone: it deploys once the guard clears.
                        outcome.error = Some(format!("new commit waiting: power guard is {state}"));
                    } else if let Some(reason) = self.ignored(&w, &head).await {
                        tracing::info!("auto-deploy: {} {} skipped: {reason}", w.name, short(&head));
                        outcome.commit = Some(head.clone());
                        outcome.skipped = Some(format!("{}: {reason}", short(&head)));
                        self.report(&w, &head, "success", &format!("Not deployed: {reason}"), None)
                            .await;
                    } else {
                        if w.pinned {
                            // A rollback pinned an older commit: follow the branch again.
                            let _ = self.pin(&w.app_uuid, "HEAD").await;
                        }
                        if let Some(old) = w.pending.as_ref().filter(|p| p.finished_at.is_none()) {
                            // Never left "pending" on GitHub.
                            self.report(&w, &old.commit, "error", "Superseded by a newer commit", None)
                                .await;
                            self.job_done(old, Err("superseded by a newer commit".into()));
                        }
                        match self.deploy(&w.app_uuid, None).await {
                            Ok(uuid) => {
                                tracing::info!("auto-deploy: {} {} → {}", w.name, w.branch, short(&head));
                                outcome.deployed = true;
                                outcome.pending = uuid.map(|u| Pending {
                                    deployment_uuid: u,
                                    commit: head.clone(),
                                    previous: w.live().map(String::from),
                                    started_at: unix_now(),
                                    finished_at: None,
                                    rollback: false,
                                    job_id: self.job(
                                        "deploy",
                                        format!("Deploy {} ({} on {})", w.name, short(&head), w.branch),
                                        &w.app_uuid,
                                    ),
                                });
                                outcome.commit = Some(head.clone());
                                self.report(&w, &head, "pending", "Deploying on your server", None)
                                    .await;
                            }
                            Err(DeployError::Gone(e)) => {
                                outcome.error = Some(e);
                                outcome.disable = true;
                            }
                            Err(DeployError::Failed(e)) => outcome.error = Some(e),
                        }
                    }
                }
            },
        }
        let error = outcome.error.clone();
        self.update(&w, |e| {
            e.last_checked_at = Some(outcome.checked_at.clone());
            e.last_error = outcome.error;
            if let Some(c) = outcome.commit {
                if outcome.deployed || e.last_commit.is_none() {
                    // Deployed, or the first head seen (already live).
                    e.live_commit = Some(c.clone());
                }
                if outcome.deployed {
                    e.last_deployed_commit = Some(c.clone());
                    e.last_deployed_at = Some(outcome.checked_at);
                    e.deploys = e.deploys.saturating_add(1);
                    e.pinned = false;
                    if outcome.pending.is_some() {
                        e.pending = outcome.pending;
                    }
                }
                e.last_commit = Some(c);
            }
            if let Some(why) = outcome.skipped {
                e.skipped = e.skipped.saturating_add(1);
                e.last_skip = Some(why);
            }
            if outcome.disable {
                e.enabled = false;
            }
        })
        .await;
        if error.is_none() && w.previews {
            self.sync_previews(&w).await;
        }
    }

    /// Skip a build when nothing relevant changed (GitHub repositories).
    async fn ignored(&self, w: &StoredWatch, head: &str) -> Option<String> {
        let slug = w.slug()?;
        let base = w.live()?;
        let changes = match github::compare(&self.cfg, &slug, base, head, w.token.as_deref()).await {
            Ok(c) => c,
            Err(e) => {
                tracing::info!("auto-deploy: {}: can't compare commits ({e}); building", w.name);
                return None;
            }
        };
        let watch_paths = self
            .app(&w.app_uuid)
            .await
            .and_then(|a| a.get("watch_paths")?.as_str().map(String::from));
        skip_reason(&changes, watch_paths.as_deref())
    }

    /// Apply the project configuration (vercel.json) of a deployed commit.
    async fn sync_project(&self, w: &StoredWatch, pr: u64, sha: &str) {
        let (Some(projects), Some(slug)) = (self.projects.get(), w.slug()) else {
            return;
        };
        let base = self
            .app(&w.app_uuid)
            .await
            .and_then(|a| a.get("base_directory")?.as_str().map(String::from))
            .unwrap_or_default();
        match crate::projects::fetch_github(&self.cfg, &slug, &base, sha, w.token.as_deref()).await {
            Ok(files) => projects.set_from_repo(&w.app_uuid, pr, files, sha).await,
            Err(e) => {
                tracing::warn!("auto-deploy: {}: reading vercel.json: {e}", w.name);
                self.update(w, |x| x.last_error = Some(format!("vercel.json: {e}")))
                    .await;
            }
        }
    }

    /// Repository, branch and token of a watched app.
    pub async fn source_of(&self, app_uuid: &str) -> Option<(String, String, Option<String>)> {
        self.store
            .lock()
            .await
            .watches
            .iter()
            .find(|w| w.app_uuid == app_uuid)
            .map(|w| (w.repo.clone(), w.branch.clone(), w.token.clone()))
    }

    /// Coolify's view of an application.
    pub async fn coolify_app(&self, app_uuid: &str) -> Option<Value> {
        self.app(app_uuid).await
    }

    async fn coolify(&self, method: &str, path: &str, body: Option<Value>) -> Result<(u16, Value), String> {
        let token = self
            .cfg
            .coolify_token()
            .ok_or("no Coolify API token on this host (sudo vercelify pair --refresh)")?;
        let url = format!("{}/api/v1{path}", self.cfg.coolify_url.trim_end_matches('/'));
        let body = body.unwrap_or(Value::Object(Default::default()));
        let req = match method {
            "GET" => Req::get(&url),
            "PATCH" => Req::patch(&url, &body),
            "DELETE" => Req::delete(&url, &body),
            _ => Req::post(&url, &body),
        };
        let (status, bytes) = http::call(req.bearer(&token).timeout(Duration::from_secs(30)))
            .await
            .map_err(|e| e.to_string())?;
        let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status.is_success() {
            Ok((status.as_u16(), v))
        } else {
            Err(http::error_text(status, &bytes))
        }
    }

    /// Queue a deployment (of a pull request with `pr`). Returns its uuid.
    async fn deploy(&self, app_uuid: &str, pr: Option<u64>) -> Result<Option<String>, DeployError> {
        let token = self.cfg.coolify_token().ok_or_else(|| {
            DeployError::Failed("no Coolify API token on this host (sudo vercelify pair --refresh)".into())
        })?;
        let mut url = format!(
            "{}/api/v1/deploy?uuid={app_uuid}&force=false",
            self.cfg.coolify_url.trim_end_matches('/')
        );
        if let Some(n) = pr {
            url.push_str(&format!("&pr={n}"));
        }
        let (status, body) = http::call(
            Req::post(&url, &serde_json::json!({}))
                .bearer(&token)
                .timeout(Duration::from_secs(30)),
        )
        .await
        .map_err(|e| DeployError::Failed(e.to_string()))?;
        if status.is_success() {
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            Ok(v.pointer("/deployments/0/deployment_uuid")
                .and_then(|u| u.as_str())
                .map(String::from))
        } else if status == hyper::StatusCode::NOT_FOUND {
            Err(DeployError::Gone(format!(
                "Coolify no longer has this application ({}) — auto-deploy turned off",
                http::error_text(status, &body)
            )))
        } else {
            Err(DeployError::Failed(format!(
                "Coolify refused the deployment: {}",
                http::error_text(status, &body)
            )))
        }
    }

    /// Put an earlier commit back live. Coolify's rollback endpoint reuses the
    /// image it kept; older Coolify versions lack it, so pin the commit and
    /// deploy instead. Returns (deployment uuid, pinned).
    async fn rollback(&self, app_uuid: &str, commit: &str) -> Result<(Option<String>, bool), String> {
        match self
            .coolify(
                "POST",
                &format!("/applications/{app_uuid}/rollback"),
                Some(serde_json::json!({ "commit": commit })),
            )
            .await
        {
            Ok((_, v)) => Ok((
                v.get("deployment_uuid").and_then(|u| u.as_str()).map(String::from),
                false,
            )),
            Err(e) if e.starts_with("HTTP 404") || e.starts_with("HTTP 405") => {
                self.pin(app_uuid, commit).await?;
                let uuid = self.deploy(app_uuid, None).await.map_err(|e| match e {
                    DeployError::Gone(m) | DeployError::Failed(m) => m,
                })?;
                Ok((uuid, true))
            }
            Err(e) => Err(e),
        }
    }

    /// Point the app at a commit (`HEAD` = follow the branch). Coolify reuses
    /// the image it already has for that commit, so a rollback is instant.
    async fn pin(&self, app_uuid: &str, commit: &str) -> Result<(), String> {
        self.coolify(
            "PATCH",
            &format!("/applications/{app_uuid}"),
            Some(serde_json::json!({ "git_commit_sha": commit })),
        )
        .await
        .map(|_| ())
    }

    /// Commit status on GitHub, when enabled.
    async fn report(&self, w: &StoredWatch, sha: &str, state: &str, description: &str, url: Option<&str>) {
        let (Some(slug), Some(token), true) = (w.slug(), w.token.as_deref(), w.github_feedback) else {
            return;
        };
        let status = github::CommitStatus {
            sha,
            state,
            target_url: url,
            description,
            context: STATUS_CONTEXT,
        };
        if let Err(e) = github::set_status(&self.cfg, &slug, token, status).await {
            tracing::warn!("auto-deploy: GitHub status for {}: {e}", w.name);
        }
    }

    async fn app(&self, app_uuid: &str) -> Option<Value> {
        self.coolify("GET", &format!("/applications/{app_uuid}"), None)
            .await
            .ok()
            .map(|(_, v)| v)
    }

    /// Follow the deployment the agent queued: report the result, and roll
    /// back when the new version comes up unhealthy.
    async fn follow_up(&self, w: &StoredWatch) {
        let Some(p) = w.pending.clone() else { return };
        let now = unix_now();
        if now.saturating_sub(p.started_at) > DEPLOY_TIMEOUT_SECS {
            if p.finished_at.is_none() {
                self.job_done(&p, Err("no result from Coolify in time".into()));
            }
            self.update(w, |e| e.pending = None).await;
            return;
        }
        if p.finished_at.is_none() {
            let status = match self
                .coolify("GET", &format!("/deployments/{}", p.deployment_uuid), None)
                .await
            {
                Ok((_, v)) => v.get("status").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                Err(_) => return,
            };
            match status.as_str() {
                "finished" => {
                    let url = self
                        .app(&w.app_uuid)
                        .await
                        .and_then(|a| a.get("fqdn")?.as_str().map(String::from))
                        .and_then(|f| f.split(',').next().map(|s| s.trim().to_string()));
                    self.report(w, &p.commit, "success", "Live on your server", url.as_deref())
                        .await;
                    self.job_done(&p, Ok(url.clone().map_or("live".into(), |u| format!("live at {u}"))));
                    self.sync_project(w, 0, &p.commit).await;
                    if let Some(watcher) = self.watcher.get() {
                        watcher
                            .resolve(
                                &format!("deploy:{}", w.app_uuid),
                                format!("{}: deployed {}", w.name, short(&p.commit)),
                                "The latest deploy succeeded.".into(),
                            )
                            .await;
                    }
                    self.update(w, |e| {
                        if let Some(x) = e.pending.as_mut() {
                            x.finished_at = Some(now);
                        }
                    })
                    .await;
                    if !w.auto_rollback || p.rollback {
                        self.update(w, |e| e.pending = None).await;
                    }
                }
                "failed" | "cancelled-by-user" | "cancelled" => {
                    self.report(w, &p.commit, "failure", "Build or deploy failed", None)
                        .await;
                    self.job_done(&p, Err(format!("deployment {status}")));
                    self.alert(
                        format!("deploy:{}", w.app_uuid),
                        CheckStatus::Bad,
                        format!("{}: deploy of {} failed", w.name, short(&p.commit)),
                        "The previous version keeps running. See the build log in the app's Deployments.".into(),
                    )
                    .await;
                    self.update(w, |e| {
                        e.pending = None;
                        // The previous version keeps running.
                        if p.previous.is_some() {
                            e.live_commit = p.previous.clone();
                        }
                    })
                    .await;
                }
                _ => {}
            }
            return;
        }
        // Finished: watch its health for a few minutes.
        let finished = p.finished_at.unwrap_or(now);
        let status = self
            .app(&w.app_uuid)
            .await
            .and_then(|a| a.get("status")?.as_str().map(String::from))
            .unwrap_or_default();
        if unhealthy(&status) {
            self.update(w, |e| e.pending = None).await;
            let Some(prev) = p.previous.clone().filter(|prev| *prev != p.commit) else {
                return;
            };
            let result = self.rollback(&w.app_uuid, &prev).await;
            match result {
                Ok((uuid, pinned)) => {
                    self.update(w, |e| {
                        e.rollbacks = e.rollbacks.saturating_add(1);
                        e.pinned = pinned;
                        e.last_deployed_commit = Some(prev.clone());
                        e.live_commit = Some(prev.clone());
                        e.pending = uuid.map(|u| Pending {
                            deployment_uuid: u,
                            commit: prev.clone(),
                            previous: None,
                            started_at: unix_now(),
                            finished_at: None,
                            rollback: true,
                            job_id: self.job(
                                "rollback",
                                format!("Roll {} back to {}", w.name, short(&prev)),
                                &w.app_uuid,
                            ),
                        });
                    })
                    .await;
                    self.report(w, &p.commit, "failure", "Unhealthy after deploy — rolled back", None)
                        .await;
                    self.alert(
                        format!("rollback:{}", w.app_uuid),
                        CheckStatus::Warn,
                        format!("{}: rolled back to {}", w.name, short(&prev)),
                        format!(
                            "{} was {status} after deploying; the previous version is going back live. New pushes deploy again.",
                            short(&p.commit)
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    self.alert(
                        format!("rollback:{}", w.app_uuid),
                        CheckStatus::Bad,
                        format!("{}: unhealthy and the rollback failed", w.name),
                        e,
                    )
                    .await;
                }
            }
        } else if now.saturating_sub(finished) >= HEALTH_WINDOW_SECS {
            self.update(w, |e| e.pending = None).await;
        }
    }

    /// Deploy, update and remove pull request previews to match GitHub.
    async fn sync_previews(&self, w: &StoredWatch) {
        let Some(slug) = w.slug() else { return };
        let now = unix_now();
        if w.token.is_none()
            && w.last_pr_poll
                .is_some_and(|t| now.saturating_sub(t) < ANONYMOUS_PR_POLL_SECS)
        {
            return;
        }
        self.update(w, |e| e.last_pr_poll = Some(now)).await;
        let open = match github::open_pulls(&self.cfg, &slug, w.token.as_deref()).await {
            Ok(o) => o,
            Err(e) => {
                self.update(w, |x| x.last_error = Some(format!("previews: {e}"))).await;
                return;
            }
        };
        let live: Vec<(u64, String)> = w.open_previews.iter().map(|p| (p.number, p.head_sha.clone())).collect();
        let plan = plan_previews(&open, &live, self.cfg.max_previews);
        let app = if plan.deploy.is_empty() {
            None
        } else {
            self.app(&w.app_uuid).await
        };
        let fqdn = app
            .as_ref()
            .and_then(|a| a.get("fqdn")?.as_str().map(String::from))
            .unwrap_or_default();
        let template = app
            .as_ref()
            .and_then(|a| a.get("preview_url_template")?.as_str().map(String::from));
        let mut notes = Vec::new();
        for (number, sha, title) in plan.deploy {
            if self.power_guard_busy().is_some() {
                break;
            }
            match self.deploy(&w.app_uuid, Some(number)).await {
                Ok(_) => {
                    let url = preview_url(&fqdn, template.as_deref(), number);
                    let previous_comment = w
                        .open_previews
                        .iter()
                        .find(|p| p.number == number)
                        .and_then(|p| p.comment_id);
                    let mut comment_id = previous_comment;
                    if let (true, Some(token), Some(u)) = (w.github_feedback, w.token.as_deref(), url.as_deref()) {
                        let text = github::preview_comment(&w.name, u, &sha);
                        match github::upsert_comment(&self.cfg, &slug, number, token, previous_comment, &text).await {
                            Ok(id) => comment_id = Some(id),
                            Err(e) => notes.push(format!("comment on #{number}: {e}")),
                        }
                        let context = format!("{STATUS_CONTEXT}/preview");
                        let status = github::CommitStatus {
                            sha: &sha,
                            state: "success",
                            target_url: Some(u),
                            description: "Preview deployed on your server",
                            context: &context,
                        };
                        let _ = github::set_status(&self.cfg, &slug, token, status).await;
                    }
                    self.sync_project(w, number, &sha).await;
                    tracing::info!("auto-deploy: preview #{number} of {} at {}", w.name, short(&sha));
                    let stored = StoredPreview {
                        number,
                        title,
                        head_sha: sha,
                        url,
                        deployed_at: Some(crate::autodeploy::now()),
                        comment_id,
                    };
                    self.update(w, |e| {
                        e.open_previews.retain(|p| p.number != number);
                        e.open_previews.push(stored);
                        e.open_previews.sort_by_key(|p| p.number);
                    })
                    .await;
                }
                Err(DeployError::Gone(e) | DeployError::Failed(e)) => notes.push(format!("preview #{number}: {e}")),
            }
        }
        for number in plan.remove {
            let path = format!("/applications/{}/previews/{number}", w.app_uuid);
            match self.coolify("DELETE", &path, None).await {
                Ok(_) => {}
                // Already gone in Coolify: just forget it.
                Err(e) if e.starts_with("HTTP 404") => {}
                Err(e) => {
                    notes.push(format!("removing preview #{number}: {e}"));
                    continue;
                }
            }
            if let Some(projects) = self.projects.get() {
                projects.remove(&crate::projects::key_for(&w.app_uuid, number)).await;
            }
            self.update(w, |e| e.open_previews.retain(|p| p.number != number)).await;
        }
        // Rules of previews that are gone (closed while the agent was down).
        if let Some(projects) = self.projects.get() {
            let open: Vec<u64> = self
                .store
                .lock()
                .await
                .watches
                .iter()
                .find(|x| x.app_uuid == w.app_uuid)
                .map(|x| x.open_previews.iter().map(|p| p.number).collect())
                .unwrap_or_default();
            for (n, key) in projects.preview_keys(&w.app_uuid).await {
                if !open.contains(&n) {
                    projects.remove(&key).await;
                }
            }
        }
        if !plan.skipped_forks.is_empty() {
            notes.push(format!(
                "not building pull requests from forks ({}) — someone else's code; deploy them by hand if you trust them",
                plan.skipped_forks.iter().map(|n| format!("#{n}")).collect::<Vec<_>>().join(", ")
            ));
        }
        if !plan.over_limit.is_empty() {
            notes.push(format!(
                "{} more pull request(s) wait for a free preview slot (at most {} per app)",
                plan.over_limit.len(),
                self.cfg.max_previews
            ));
        }
        if !notes.is_empty() {
            let msg = notes.join("; ");
            self.update(w, |e| e.last_error = Some(msg)).await;
        }
    }
}

#[derive(Default)]
struct Outcome {
    checked_at: String,
    skipped: Option<String>,
    commit: Option<String>,
    deployed: bool,
    pending: Option<Pending>,
    error: Option<String>,
    disable: bool,
}

enum DeployError {
    Gone(String),
    Failed(String),
}

/// Head commit of `branch` on `repo`.
async fn ls_remote(cfg: &Config, repo: &str, branch: &str, token: Option<&str>) -> Result<String, String> {
    let refname = format!("refs/heads/{branch}");
    let header;
    let mut env: Vec<(&str, &str)> = vec![
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_ASKPASS", "/bin/false"),
        ("GIT_ALLOW_PROTOCOL", "https:http"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        // Ignore any GIT_CONFIG_* the agent itself was started with.
        ("GIT_CONFIG_COUNT", "0"),
    ];
    if let Some(t) = token {
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{t}"));
        header = format!("Authorization: Basic {basic}");
        env.retain(|(k, _)| *k != "GIT_CONFIG_COUNT");
        env.extend([
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "http.extraHeader"),
            ("GIT_CONFIG_VALUE_0", header.as_str()),
        ]);
    }
    let out = run_env(
        &cfg.git_bin,
        &["ls-remote", "--heads", repo, &refname],
        &env,
        GIT_TIMEOUT,
    )
    .await
    .map_err(|e| e.to_string())?;
    if !out.success() {
        let msg = redact(&out.stderr, token);
        let hint = if msg.contains("could not read Username")
            || msg.contains("Authentication failed")
            || msg.contains("403")
        {
            " — is the repository private? Add a read-only access token."
        } else {
            ""
        };
        return Err(format!("git ls-remote failed: {msg}{hint}"));
    }
    parse_ls_remote(&out.stdout, branch).ok_or_else(|| format!("branch `{branch}` not found in {repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_like_coolify() {
        assert!(glob_match("src/*", "src/a.ts"));
        assert!(!glob_match("src/*", "src/a/b.ts"));
        assert!(glob_match("src/**", "src/a/b.ts"));
        assert!(glob_match("apps/web/**/*.ts", "apps/web/x.ts"));
        assert!(glob_match("apps/web/**/*.ts", "apps/web/a/b/x.ts"));
        assert!(!glob_match("apps/web/**/*.ts", "apps/api/x.ts"));
        assert!(glob_match("file?.md", "file1.md"));
        assert!(!glob_match("file?.md", "file/.md"));
        assert!(glob_match("v[0-9].txt", "v3.txt"));
        assert!(!glob_match("v[!0-9].txt", "v3.txt"));
        assert!(glob_match("a.b", "a.b"));
        assert!(!glob_match("a.b", "axb"));
        assert!(glob_match("[x", "[x"));
    }

    #[test]
    fn watch_paths_last_match_wins() {
        let p = ["apps/web/**", "!apps/web/**/*.md"];
        assert!(watched("apps/web/page.tsx", &p));
        assert!(!watched("apps/web/README.md", &p));
        assert!(!watched("apps/api/x.ts", &p));
        // Only exclusions: everything else counts.
        assert!(watched("src/x.ts", &["!docs/**"]));
        assert!(!watched("docs/a.md", &["!docs/**"]));
    }

    #[test]
    fn ignored_build_step() {
        let ch = |files: &[&str], msg: &str| github::Changes {
            files: files.iter().map(|f| f.to_string()).collect(),
            messages: vec!["older [skip ci]".into(), msg.into()],
            truncated: false,
        };
        let wp = Some("apps/web/**\npackages/**\n");
        assert_eq!(skip_reason(&ch(&["apps/web/a.ts"], "feat"), wp), None);
        assert_eq!(skip_reason(&ch(&["packages/ui/b.ts"], "feat"), wp), None);
        assert!(
            skip_reason(&ch(&["apps/api/a.ts", "README.md"], "feat"), wp).is_some_and(|r| r.contains("watch paths"))
        );
        // Only the newest commit's message counts.
        assert_eq!(skip_reason(&ch(&["apps/web/a.ts"], "feat"), None), None);
        assert!(skip_reason(&ch(&["apps/web/a.ts"], "Docs [Skip Deploy]"), None)
            .is_some_and(|r| r.contains("[skip deploy]")));
        // Without watch paths or with an incomplete list: build.
        assert_eq!(skip_reason(&ch(&["x"], "feat"), Some("  \n")), None);
        let mut big = ch(&["apps/api/a.ts"], "feat");
        big.truncated = true;
        assert_eq!(skip_reason(&big, wp), None);
        assert_eq!(skip_reason(&ch(&[], "empty"), wp), None);
    }

    #[test]
    fn decisions() {
        assert_eq!(decide(None, "abc"), Action::Baseline);
        assert_eq!(decide(Some("abc"), "abc"), Action::Unchanged);
        assert_eq!(decide(Some("abc"), "def"), Action::Deploy);
    }

    #[test]
    fn ls_remote_output() {
        let out = "0123456789abcdef0123456789abcdef01234567\trefs/heads/main\n\
                   89abcdef0123456789abcdef0123456789abcdef\trefs/heads/main-old\n";
        assert_eq!(
            parse_ls_remote(out, "main").as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(parse_ls_remote(out, "dev"), None);
        assert_eq!(parse_ls_remote("garbage\trefs/heads/main", "main"), None);
    }

    #[test]
    fn validation() {
        assert!(valid_repo("https://github.com/you/app.git"));
        assert!(valid_repo("https://gitlab.example.com/group/sub/app"));
        assert!(!valid_repo("git@github.com:you/app.git"));
        assert!(!valid_repo("https://user:pw@github.com/you/app"));
        assert!(!valid_repo("https://github.com"));
        assert!(!valid_repo("https://github.com/a b"));
        assert!(!valid_repo("ext::sh -c id"));
        assert!(!valid_repo("--upload-pack=x"));

        assert!(valid_branch("main"));
        assert!(valid_branch("release/v1.2"));
        assert!(valid_branch("feat_x-1"));
        assert!(!valid_branch("-x"));
        assert!(!valid_branch("a..b"));
        assert!(!valid_branch("a/"));
        assert!(!valid_branch("x.lock"));
        assert!(!valid_branch("a b"));
        assert!(!valid_branch(""));

        assert!(valid_app_uuid("ok8s0gk4wc4k"));
        assert!(!valid_app_uuid("../x"));
    }

    fn pr(n: u64, sha: &str, fork: bool) -> github::PullRequest {
        github::PullRequest {
            number: n,
            title: format!("PR {n}"),
            head_sha: sha.into(),
            from_fork: fork,
        }
    }

    #[test]
    fn preview_plans() {
        // new, updated, unchanged, closed, fork
        let open = vec![
            pr(1, "a", false),
            pr(2, "b2", false),
            pr(3, "c", false),
            pr(9, "x", true),
        ];
        let live = vec![(2, "b1".to_string()), (3, "c".to_string()), (4, "d".to_string())];
        let p = plan_previews(&open, &live, 5);
        assert_eq!(
            p.deploy,
            vec![
                (1, "a".to_string(), "PR 1".to_string()),
                (2, "b2".to_string(), "PR 2".to_string())
            ]
        );
        assert_eq!(p.remove, vec![4]);
        assert_eq!(p.skipped_forks, vec![9]);
        assert!(p.over_limit.is_empty());
        // limit: live previews keep their slot, new ones wait
        let p = plan_previews(&[pr(5, "e", false), pr(3, "c2", false)], &[(3, "c".into())], 1);
        assert_eq!(p.over_limit, vec![5]);
        assert_eq!(p.deploy, vec![(3, "c2".to_string(), "PR 3".to_string())]);
    }

    #[test]
    fn preview_urls_and_health() {
        assert_eq!(
            preview_url("https://blog.example.com", Some("pr-{{pr_id}}-{{domain}}"), 7).as_deref(),
            Some("https://pr-7-blog.example.com")
        );
        assert_eq!(
            preview_url("http://a.100.1.2.3.sslip.io,https://b.example.com", None, 2).as_deref(),
            Some("http://2.a.100.1.2.3.sslip.io")
        );
        assert_eq!(preview_url("", None, 1), None);
        assert!(unhealthy("running:unhealthy"));
        assert!(unhealthy("exited:unhealthy"));
        assert!(unhealthy("restarting"));
        assert!(!unhealthy("running:healthy"));
        assert!(!unhealthy("running:unknown"));
    }

    #[test]
    fn redacts_tokens() {
        assert_eq!(redact("fatal: secret123 bad", Some("secret123")), "fatal: *** bad");
        assert_eq!(redact(" x ", None), "x");
    }

    fn cfg(dir: &std::path::Path) -> Arc<Config> {
        Arc::new(Config {
            state_dir: dir.into(),
            coolify_token_file: dir.join("none"),
            ..Config::default()
        })
    }

    #[tokio::test]
    async fn upsert_persists_and_keeps_tokens_private() {
        let dir = tempfile::tempdir().unwrap();
        let ad = AutoDeploy::load(cfg(dir.path()));
        let req = AutoDeployRequest {
            app_uuid: "app1".into(),
            name: Some("blog".into()),
            repo: "https://github.com/you/blog.git".into(),
            branch: "main".into(),
            token: Some("ghp_secret".into()),
            enabled: Some(true),
            previews: None,
            auto_rollback: None,
            github_feedback: None,
        };
        let w = ad.upsert(req.clone()).await.unwrap();
        assert!(w.has_token);
        let raw = std::fs::read_to_string(dir.path().join("autodeploy.json")).unwrap();
        assert!(raw.contains("ghp_secret"), "stored on disk for git");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("autodeploy.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let status = serde_json::to_string(&ad.status().await).unwrap();
        assert!(!status.contains("ghp_secret"), "never returned over the API");

        // token omitted = kept; empty = removed
        let w = ad
            .upsert(AutoDeployRequest {
                token: None,
                ..req.clone()
            })
            .await
            .unwrap();
        assert!(w.has_token);
        let w = ad
            .upsert(AutoDeployRequest {
                token: Some(String::new()),
                ..req.clone()
            })
            .await
            .unwrap();
        assert!(!w.has_token);

        // reload from disk
        let again = AutoDeploy::load(cfg(dir.path()));
        assert_eq!(again.status().await.watches.len(), 1);
        again.remove("app1").await.unwrap();
        assert!(again.remove("app1").await.is_err());
        assert!(again.status().await.watches.is_empty());
    }

    #[tokio::test]
    async fn rejects_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let ad = AutoDeploy::load(cfg(dir.path()));
        let base = AutoDeployRequest {
            app_uuid: "app1".into(),
            name: None,
            repo: "https://github.com/you/blog.git".into(),
            branch: "main".into(),
            token: None,
            enabled: None,
            previews: None,
            auto_rollback: None,
            github_feedback: None,
        };
        for bad in [
            AutoDeployRequest {
                repo: "file:///etc".into(),
                ..base.clone()
            },
            AutoDeployRequest {
                branch: "--exec".into(),
                ..base.clone()
            },
            AutoDeployRequest {
                app_uuid: "a/b".into(),
                ..base.clone()
            },
            AutoDeployRequest {
                repo: "http://github.com/you/blog.git".into(),
                token: Some("tok".into()),
                ..base.clone()
            },
            AutoDeployRequest {
                repo: "https://gitlab.com/you/blog.git".into(),
                previews: Some(true),
                ..base.clone()
            },
            AutoDeployRequest {
                github_feedback: Some(true),
                ..base.clone()
            },
        ] {
            assert_eq!(ad.upsert(bad).await.unwrap_err().status, 400);
        }
        assert!(ad.status().await.watches.is_empty());
    }
}

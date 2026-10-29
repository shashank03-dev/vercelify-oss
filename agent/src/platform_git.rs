//! Push-to-deploy and pull request previews for the platform engine.
//!
//! Each app's branch is polled with `git ls-remote` (no webhook needed, so it
//! works behind CGNAT, and webhooks only make it sooner); this skips
//! pushes that change nothing the app watches, and deploys the rest. For
//! GitHub repositories it also keeps previews in line with open pull
//! requests (forks are never built) and reports commit statuses and a
//! preview comment. After every successful deployment the project rules
//! (`vercel.json`) of the deployed commit are applied.

use crate::config::Config;
use crate::github;
use crate::gitpoll::{plan_previews, skip_reason};
use std::sync::Arc;
use std::time::Duration;
use vercelify_core::platform::{App, AppKind, DeploymentKind, DeploymentTrigger, GitWatchStatus};
use vercelify_platform::engine::{Engine, EngineEvent, Enqueue};
use vercelify_platform::git::Git;
use vercelify_platform::{apps, deployments, git_watch, previews};

pub struct PlatformGit {
    cfg: Arc<Config>,
    engine: Arc<Engine>,
    projects: Arc<crate::projects::Projects>,
    git: Git,
    /// One check per app at a time: a webhook landing during a poll (or
    /// GitHub's push and pull_request events together) must not deploy the
    /// same commit twice.
    checking: std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// `owner/repo` of a GitHub repository URL (https or scp-style ssh).
pub fn github_slug(repo: &str) -> Option<String> {
    if let Some(rest) = repo.strip_prefix("git@github.com:") {
        let s = rest.trim_end_matches('/').trim_end_matches(".git");
        return (s.split('/').count() == 2 && !s.contains("..")).then(|| s.to_string());
    }
    github::repo_slug(repo)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

impl PlatformGit {
    pub fn new(cfg: Arc<Config>, engine: Arc<Engine>, projects: Arc<crate::projects::Projects>) -> Arc<Self> {
        let git = Git {
            bin: cfg.git_bin.clone(),
            timeout: Duration::from_secs(60),
        };
        Arc::new(Self {
            cfg,
            engine,
            projects,
            git,
            checking: Default::default(),
        })
    }

    pub async fn run(self: Arc<Self>) {
        let every = self.cfg.autodeploy_interval_secs;
        if every == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
        loop {
            self.tick().await;
            tokio::time::sleep(Duration::from_secs(every.max(15))).await;
        }
    }

    pub async fn tick(&self) {
        let list = match self.engine.store.with(|c| apps::list(c, None)) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("auto-deploy: {e}");
                return;
            }
        };
        for app in list {
            if app.kind == AppKind::Image || app.source.repository.is_none() {
                continue;
            }
            if let Err(e) = self.check(&app).await {
                tracing::info!("auto-deploy: {}: {e}", app.name);
            }
        }
    }

    /// A token for the GitHub API of the app's repository (its source's
    /// token, or a GitHub App's installation token).
    async fn token(&self, app: &App) -> Option<String> {
        self.engine
            .api_token(
                app.source.git_source_id.as_deref(),
                app.source.repository.as_deref().unwrap_or(""),
            )
            .await
    }

    fn record(&self, app: &App, head: Option<&str>, error: Option<&str>) {
        let _ = self.engine.store.with(|c| git_watch::checked(c, &app.id, head, error));
    }

    /// A new commit that waits: last_head stays, so it deploys once `why` clears.
    fn hold(&self, app: &App, why: &str) {
        let _ = self.engine.store.with(|c| git_watch::held(c, &app.id, why));
    }

    /// Check one app now: deploy a new head, sync previews.
    pub async fn check(&self, app: &App) -> Result<GitWatchStatus, String> {
        let lock = self
            .checking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(app.id.clone())
            .or_default()
            .clone();
        let _held = lock.lock().await;
        // The app may have changed (or gone) while another check ran.
        let app = self
            .engine
            .store
            .with(|c| apps::get(c, &app.id))
            .map_err(|e| e.to_string())?;
        self.check_now(&app).await
    }

    async fn check_now(&self, app: &App) -> Result<GitWatchStatus, String> {
        let w = self
            .engine
            .store
            .with(|c| git_watch::get(c, &app.id))
            .map_err(|e| e.to_string())?;
        if !w.enabled {
            return Ok(w);
        }
        let repo = app.source.repository.clone().unwrap_or_default();
        let branch = app.source.branch.clone().unwrap_or_else(|| "main".into());
        let (auth, _key) = self
            .engine
            .git_auth(app.source.git_source_id.as_deref(), &repo)
            .await
            .map_err(|e| e.to_string())?;
        let head = match self.git.resolve(&repo, &branch, &auth).await {
            Ok(h) => h,
            Err(e) => {
                self.record(app, None, Some(&e.to_string()));
                return Err(e.to_string());
            }
        };
        let slug = github_slug(&repo);
        let token = self.token(app).await;
        if app.previews.enabled {
            if let Some(slug) = &slug {
                self.sync_previews(app, slug, token.as_deref()).await;
            }
        } else {
            // Turned off: the previews still running go (their memory too).
            self.remove_previews(app).await;
        }
        // Compare with the head seen last, or on the first look with the
        // last production deployment. Never deployed: nothing to follow yet
        // (the first deploy is the person's).
        let base = if w.last_head.is_empty() {
            self.engine
                .store
                .with(|c| deployments::list(c, &app.id, 20))
                .unwrap_or_default()
                .into_iter()
                .find(|d| d.pr_number.is_none() && !d.commit_sha.is_empty())
                .map(|d| d.commit_sha)
                .unwrap_or_default()
        } else {
            w.last_head.clone()
        };
        if base.is_empty() || head == base || head.starts_with(&base) {
            self.record(app, Some(&head), None);
            return self.status(app);
        }
        if let Some(pin) = &app.source.pinned_sha {
            self.record(
                app,
                None,
                Some(&format!(
                    "{} is pinned to {} by a rollback: deploy it to follow {branch} again",
                    app.name,
                    short(pin)
                )),
            );
            return self.status(app);
        }
        if let Some(state) = self.engine.gate.held() {
            self.hold(app, &format!("new commit waiting: the power guard says {state}"));
            return self.status(app);
        }
        if let Some(slug) = &slug {
            match github::compare(&self.cfg, slug, &base, &head, token.as_deref()).await {
                Ok(changes) => {
                    let watch = app.build.watch_paths.join("\n");
                    let watch = (!watch.is_empty()).then_some(watch.as_str());
                    if let Some(why) = skip_reason(&changes, watch) {
                        tracing::info!("auto-deploy: {} {} skipped: {why}", app.name, short(&head));
                        let _ = self.engine.store.with(|c| git_watch::skipped(c, &app.id, &why));
                        self.record(app, Some(&head), None);
                        if let Some(t) = &token {
                            let _ = github::set_status(
                                &self.cfg,
                                slug,
                                t,
                                github::CommitStatus {
                                    sha: &head,
                                    state: "success",
                                    target_url: None,
                                    description: &format!("Not deployed: {why}"),
                                    context: &context(&app.name, None),
                                },
                            )
                            .await;
                        }
                        return self.status(app);
                    }
                }
                Err(e) => tracing::info!("auto-deploy: {}: can't compare commits ({e}); building", app.name),
            }
        }
        match self.engine.enqueue(
            &app.id,
            Enqueue {
                trigger: Some(DeploymentTrigger::Push),
                commit_sha: Some(head.clone()),
                ..Enqueue::default()
            },
        ) {
            Ok(d) => {
                tracing::info!("auto-deploy: {} {branch} → {} (#{})", app.name, short(&head), d.number);
                self.record(app, Some(&head), None);
                self.report(app, &head, None, "pending", "Deploying on your server", None)
                    .await;
            }
            Err(e) => self.record(app, None, Some(&e.to_string())),
        }
        self.status(app)
    }

    fn status(&self, app: &App) -> Result<GitWatchStatus, String> {
        self.engine
            .store
            .with(|c| git_watch::get(c, &app.id))
            .map_err(|e| e.to_string())
    }

    async fn remove_previews(&self, app: &App) {
        let live = self
            .engine
            .store
            .with(|c| previews::list(c, &app.id, true))
            .unwrap_or_default();
        for p in live {
            match self.engine.remove_preview(&app.id, p.pr_number).await {
                Ok(()) => {
                    self.projects
                        .remove(&crate::projects::key_for(&app.id, u64::from(p.pr_number)))
                        .await;
                }
                Err(e) => tracing::warn!("previews: {} #{}: {e}", app.name, p.pr_number),
            }
        }
    }

    async fn sync_previews(&self, app: &App, slug: &str, token: Option<&str>) {
        let (open, complete) = match github::open_pulls(&self.cfg, slug, token).await {
            Ok(o) => o,
            Err(e) => {
                tracing::info!("previews: {}: {e}", app.name);
                return;
            }
        };
        let live: Vec<(u64, String)> = self
            .engine
            .store
            .with(|c| previews::list(c, &app.id, true))
            .unwrap_or_default()
            .into_iter()
            .map(|p| (u64::from(p.pr_number), p.head_sha))
            .collect();
        let mut plan = plan_previews(&open, &live, usize::from(app.previews.max));
        if !complete {
            // Past the pages read, a live preview's pull request may well be open.
            plan.remove.retain(|n| open.iter().any(|p| p.number == *n));
        }
        for n in plan.remove {
            if let Err(e) = self.engine.remove_preview(&app.id, n as u32).await {
                tracing::warn!("previews: {} #{n}: {e}", app.name);
            } else {
                // The preview's rules go with it.
                self.projects.remove(&crate::projects::key_for(&app.id, n)).await;
            }
        }
        for (n, sha, title) in plan.deploy {
            match self.engine.enqueue(
                &app.id,
                Enqueue {
                    trigger: Some(DeploymentTrigger::Push),
                    commit_sha: Some(sha.clone()),
                    pr_number: Some(n as u32),
                    pr_title: Some(title),
                    ..Enqueue::default()
                },
            ) {
                Ok(_) => {
                    self.report(
                        app,
                        &sha,
                        Some(n as u32),
                        "pending",
                        "Building a preview on your server",
                        None,
                    )
                    .await
                }
                Err(e) => tracing::warn!("previews: {} #{n}: {e}", app.name),
            }
        }
    }

    /// Commit status on GitHub (when the app has a token for it).
    async fn report(&self, app: &App, sha: &str, pr: Option<u32>, state: &str, description: &str, url: Option<&str>) {
        let (Some(slug), Some(token)) = (
            github_slug(app.source.repository.as_deref().unwrap_or("")),
            self.token(app).await,
        ) else {
            return;
        };
        if sha.is_empty() {
            return;
        }
        let description: String = description.chars().take(140).collect();
        if let Err(e) = github::set_status(
            &self.cfg,
            &slug,
            &token,
            github::CommitStatus {
                sha,
                state,
                target_url: url,
                description: &description,
                context: &context(&app.name, pr),
            },
        )
        .await
        {
            tracing::info!("github status for {}: {e}", app.name);
        }
    }

    /// Engine events: statuses, preview comments, and the deployed commit's
    /// project rules.
    pub async fn on_event(&self, e: EngineEvent) {
        let (name, deployment, ok, text, url) = match e {
            EngineEvent::Changed => return,
            EngineEvent::Succeeded { app, deployment, url } => (app, deployment, true, String::new(), url),
            EngineEvent::Failed { app, deployment, error } => (app, deployment, false, error, None),
        };
        let Ok(app) = self.engine.store.with(|c| apps::find(c, &name)) else {
            return;
        };
        let pr = deployment.pr_number;
        if ok {
            let files = self
                .engine
                .store
                .with(|c| deployments::project_files(c, &deployment.id))
                .unwrap_or_default();
            let key = crate::projects::key_for(&app.id, u64::from(pr.unwrap_or(0)));
            if !files.is_empty() || self.projects.get(&key).await.is_some() {
                self.projects
                    .set_from_repo(&app.id, u64::from(pr.unwrap_or(0)), files, &deployment.commit_sha)
                    .await;
            }
        }
        if matches!(deployment.trigger, DeploymentTrigger::Recover) {
            return;
        }
        let (state, desc) = if ok {
            ("success", format!("Deployed #{}", deployment.number))
        } else {
            (
                "failure",
                text.lines().next().unwrap_or("Deployment failed").to_string(),
            )
        };
        self.report(&app, &deployment.commit_sha, pr, state, &desc, url.as_deref())
            .await;
        if let (true, Some(n), Some(u), DeploymentKind::Preview) = (ok, pr, url.as_deref(), deployment.kind) {
            self.comment(&app, n, u, &deployment.commit_sha).await;
        }
    }

    async fn comment(&self, app: &App, pr: u32, url: &str, sha: &str) {
        let (Some(slug), Some(token)) = (
            github_slug(app.source.repository.as_deref().unwrap_or("")),
            self.token(app).await,
        ) else {
            return;
        };
        let existing = self
            .engine
            .store
            .with(|c| git_watch::comment_id(c, &app.id, pr))
            .ok()
            .flatten();
        let text = github::preview_comment(&app.name, url, sha);
        match github::upsert_comment(&self.cfg, &slug, u64::from(pr), &token, existing, &text).await {
            Ok(id) => {
                let _ = self
                    .engine
                    .store
                    .with(|c| git_watch::set_comment_id(c, &app.id, pr, id));
            }
            Err(e) => tracing::info!("preview comment for {} #{pr}: {e}", app.name),
        }
    }
}

/// The status context GitHub groups our statuses under.
pub fn context(app: &str, pr: Option<u32>) -> String {
    match pr {
        Some(_) => format!("vercelify/{app} (preview)"),
        None => format!("vercelify/{app}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_contexts() {
        assert_eq!(github_slug("https://github.com/me/blog").as_deref(), Some("me/blog"));
        assert_eq!(
            github_slug("https://github.com/me/blog.git").as_deref(),
            Some("me/blog")
        );
        assert_eq!(github_slug("git@github.com:me/blog.git").as_deref(), Some("me/blog"));
        assert_eq!(github_slug("https://gitlab.com/me/blog"), None);
        assert_eq!(github_slug("git@github.com:me"), None);
        assert_eq!(context("blog", None), "vercelify/blog");
        assert_eq!(context("blog", Some(4)), "vercelify/blog (preview)");
    }
}

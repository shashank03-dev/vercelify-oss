//! The deployment engine: takes queued deployments through the state machine
//! (design §7): queued → building → deploying → healthy, or failed/cancelled.
//!
//! One deployment per app at a time (a lock per app); builds additionally go
//! through the [`BuildGate`]. The database is updated at every step, so a
//! restart knows exactly where things stood (`deployments::fail_interrupted`).

use crate::apps::{self, domain_settings};
use crate::build::{BuildRequest, Builder};
use crate::deploy::{self, DeployInput, Deployer};
use crate::detect::PlanKind;
use crate::gate::BuildGate;
use crate::git::GitAuth;
use crate::logs::{LogHub, LogWriter};
use crate::proc::{cancel_pair, CancelHandle};
use crate::proxy::RouteHost;
use crate::secrets::MasterKey;
use crate::{compose, deployments, env, git_sources, previews, volumes, Error, Result, Store};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use vercelify_core::platform::{
    App, AppKind, AppPatch, AppSource, CertStatus, Deployment, DeploymentStatus, DeploymentTrigger, VolumeOwner,
};

/// What happened, for the agent's events, alerts and GitHub statuses.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// Something about deployments changed (refresh lists).
    Changed,
    Succeeded {
        app: String,
        deployment: Deployment,
        url: Option<String>,
    },
    Failed {
        app: String,
        deployment: Deployment,
        error: String,
    },
}

pub type Listener = Box<dyn Fn(EngineEvent) + Send + Sync>;

pub struct Engine {
    pub store: Arc<Store>,
    pub key: Arc<MasterKey>,
    pub builder: Builder,
    pub deployer: Deployer,
    pub gate: BuildGate,
    pub logs: Arc<LogHub>,
    wake: Notify,
    running: Mutex<HashMap<String, CancelHandle>>,
    busy: Mutex<HashSet<String>>,
    listener: Mutex<Option<Listener>>,
    /// This server's private address (mesh/Tailscale): where databases'
    /// public ports listen unless opened to everyone.
    private_ip: Mutex<Option<String>>,
}

/// A request to deploy.
#[derive(Debug, Clone, Default)]
pub struct Enqueue {
    pub trigger: Option<DeploymentTrigger>,
    pub commit_sha: Option<String>,
    pub pr_number: Option<u32>,
    /// Preview branch (head ref of the PR).
    pub branch: Option<String>,
    pub pr_title: Option<String>,
    /// Roll back to this deployment.
    pub rollback_of: Option<String>,
    pub force_rebuild: bool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Engine {
    pub fn new(
        store: Arc<Store>,
        key: Arc<MasterKey>,
        builder: Builder,
        deployer: Deployer,
        gate: BuildGate,
        logs: Arc<LogHub>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            key,
            builder,
            deployer,
            gate,
            logs,
            wake: Notify::new(),
            running: Mutex::default(),
            busy: Mutex::default(),
            listener: Mutex::default(),
            private_ip: Mutex::default(),
        })
    }

    pub fn set_private_ip(&self, ip: Option<String>) {
        *lock(&self.private_ip) = ip;
    }

    pub fn private_ip(&self) -> Option<String> {
        lock(&self.private_ip).clone()
    }

    pub fn set_listener(&self, l: Listener) {
        *lock(&self.listener) = Some(l);
    }

    pub(crate) fn emit(&self, e: EngineEvent) {
        if let Some(l) = lock(&self.listener).as_ref() {
            l(e);
        }
    }

    /// Queue a deployment. Returns it as queued.
    pub fn enqueue(&self, app: &str, req: Enqueue) -> Result<Deployment> {
        let d = self.store.tx(|c| {
            let a = apps::find(c, app)?;
            let trigger = req.trigger.unwrap_or(DeploymentTrigger::Manual);
            if let Some(sha) = &req.commit_sha {
                crate::validate::commit_sha(sha)?;
            }
            let mut image = String::new();
            let mut commit = req.commit_sha.clone().unwrap_or_default();
            let mut message = String::new();
            let branch = match req.pr_number {
                Some(_) => req.branch.clone().unwrap_or_default(),
                None => a.source.branch.clone().unwrap_or_else(|| "main".into()),
            };
            if !branch.is_empty() {
                crate::validate::branch(&branch)?;
            }
            if let Some(target) = &req.rollback_of {
                let t = deployments::get(c, target)?;
                if t.app_id != a.id || t.pr_number.is_some() {
                    return Err(Error::Invalid(
                        "roll back to a production deployment of this app".into(),
                    ));
                }
                if t.commit_sha.is_empty() && t.image.is_empty() {
                    return Err(Error::Invalid(format!(
                        "deployment {} has nothing to roll back to",
                        t.number
                    )));
                }
                if !req.force_rebuild {
                    image = t.image.clone();
                }
                commit = t.commit_sha.clone();
                message = t.commit_message.clone();
                // Keep auto-deploy from rolling forward again right away
                // (not for the reconciler restoring what already runs).
                if !commit.is_empty() && trigger != DeploymentTrigger::Recover {
                    let mut s = a.source.clone();
                    s.pinned_sha = Some(commit.clone());
                    pin(c, &a, s)?;
                }
            } else if req.pr_number.is_none() {
                match trigger {
                    DeploymentTrigger::Push | DeploymentTrigger::Webhook if a.source.pinned_sha.is_some() => {
                        return Err(Error::Conflict(format!(
                            "{} is pinned to {} by a rollback; deploy it manually to unpin",
                            a.name,
                            a.source.pinned_sha.as_deref().unwrap_or("")
                        )));
                    }
                    DeploymentTrigger::Manual | DeploymentTrigger::Hook if a.source.pinned_sha.is_some() => {
                        let mut s = a.source.clone();
                        s.pinned_sha = None;
                        pin(c, &a, s)?;
                    }
                    _ => {}
                }
            }
            // Image apps deploy their configured image, except a rollback,
            // which goes back to the image the target ran.
            if a.kind == AppKind::Image && req.rollback_of.is_none() {
                image = a.source.image.clone().unwrap_or_default();
            }
            if let Some(pr) = req.pr_number {
                previews::upsert(c, &a.id, pr, &commit, req.pr_title.as_deref().unwrap_or(""))?;
            }
            deployments::create(
                c,
                &deployments::NewDeployment {
                    app_id: &a.id,
                    pr_number: req.pr_number,
                    branch: &branch,
                    commit_sha: &commit,
                    commit_message: &message,
                    trigger: if req.rollback_of.is_some() {
                        trigger_or(req.trigger, DeploymentTrigger::Rollback)
                    } else {
                        trigger
                    },
                    rollback_of: req.rollback_of.as_deref(),
                    image: &image,
                    node_id: a.node_id.as_deref(),
                },
            )
        })?;
        self.wake.notify_one();
        self.emit(EngineEvent::Changed);
        Ok(d)
    }

    /// Cancel a queued or running deployment.
    pub fn cancel(&self, id: &str) -> Result<Deployment> {
        if let Some(h) = lock(&self.running).get(id) {
            h.cancel();
            return self.store.with(|c| deployments::get(c, id));
        }
        let d = self
            .store
            .tx(|c| deployments::transition(c, id, DeploymentStatus::Cancelled, None))?;
        self.emit(EngineEvent::Changed);
        Ok(d)
    }

    /// The dispatcher: start every queued deployment whose app is free.
    pub async fn run(self: Arc<Self>) {
        loop {
            match self.store.with(deployments::active) {
                Ok(active) => {
                    for d in active.into_iter().filter(|d| d.status == DeploymentStatus::Queued) {
                        if !lock(&self.busy).insert(d.app_id.clone()) {
                            continue;
                        }
                        let me = self.clone();
                        tokio::spawn(async move {
                            let app_id = d.app_id.clone();
                            me.execute(d).await;
                            lock(&me.busy).remove(&app_id);
                            me.wake.notify_one();
                        });
                    }
                }
                Err(e) => tracing::error!("deployment queue: {e}"),
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), self.wake.notified()).await;
        }
    }

    async fn execute(self: &Arc<Self>, d: Deployment) {
        // It may have been cancelled or superseded since it was listed.
        let d = match self.store.with(|c| deployments::get(c, &d.id)) {
            Ok(d) if d.status == DeploymentStatus::Queued => d,
            _ => return,
        };
        let app = match self.store.with(|c| apps::get(c, &d.app_id)) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("deployment {} of a missing app: {e}", d.id);
                return;
            }
        };
        let (handle, cancel) = cancel_pair();
        lock(&self.running).insert(d.id.clone(), handle);
        let mut log = self.logs.writer(&app.name, &d.id);
        log.info(&format!(
            "deployment #{} of {} ({}{})",
            d.number,
            app.name,
            d.trigger.as_str(),
            d.pr_number.map(|n| format!(", pull request #{n}")).unwrap_or_default()
        ));
        let res = self.steps(&app, &d, cancel, &mut log).await;
        lock(&self.running).remove(&d.id);
        match res {
            Ok((done, url, previous)) => {
                log.info(&format!(
                    "healthy{}",
                    url.as_deref().map(|u| format!(": {u}")).unwrap_or_default()
                ));
                let watched = matches!(
                    done.trigger,
                    DeploymentTrigger::Push
                        | DeploymentTrigger::Manual
                        | DeploymentTrigger::Hook
                        | DeploymentTrigger::Webhook
                );
                if let (Some(prev), true, None) = (previous, watched, done.pr_number) {
                    tokio::spawn(self.clone().watch_after_deploy(app.clone(), done.clone(), prev));
                }
                self.emit(EngineEvent::Succeeded {
                    app: app.name.clone(),
                    deployment: done,
                    url,
                });
            }
            Err(e) => {
                let cancelled = matches!(&e, Error::Conflict(m) if m == "cancelled");
                let (status, msg) = if cancelled {
                    (DeploymentStatus::Cancelled, "cancelled".to_string())
                } else {
                    (DeploymentStatus::Failed, e.to_string())
                };
                log.error(&msg);
                let updated = self
                    .store
                    .tx(|c| deployments::transition(c, &d.id, status, Some(&msg)))
                    .unwrap_or(d.clone());
                if !cancelled {
                    self.emit(EngineEvent::Failed {
                        app: app.name.clone(),
                        deployment: updated,
                        error: msg,
                    });
                }
            }
        }
        drop(log);
        self.logs.prune(&app.name, crate::logs::KEEP_PER_APP);
        self.emit(EngineEvent::Changed);
    }

    /// Credentials of the app's git source; the key file (deploy keys)
    /// lives as long as the returned guard.
    fn auth(&self, app: &App) -> Result<(GitAuth, Option<git_sources::KeyFile>)> {
        let tmp = self.builder.work_dir.join(".keys");
        let known_hosts = self.builder.work_dir.join("known_hosts");
        self.store
            .with(|c| git_sources::auth(c, &self.key, app.source.git_source_id.as_deref(), &tmp, &known_hosts))
    }

    async fn steps(
        self: &Arc<Self>,
        app: &App,
        d: &Deployment,
        cancel: crate::proc::Cancel,
        log: &mut LogWriter,
    ) -> Result<(Deployment, Option<String>, Option<String>)> {
        let preview = d.pr_number.is_some();
        let vars = {
            let key = self.key.clone();
            let id = app.id.clone();
            self.store
                .call(move |c| env::resolve_for_deploy(c, &key, &id, preview))
                .await?
        };
        // Image: reuse (rollback, image apps) or build. Compose apps keep
        // their checkout for `docker compose up`.
        let mut image = d.image.clone();
        let mut compose_job: Option<(String, PathBuf)> = None;
        let (mut port, mut framework) = self.store.with(|c| deployments::built_info(c, &d.id))?;
        if let Some(target) = &d.rollback_of {
            // The rules of the commit rolled back to come back with it.
            self.store.with(|c| {
                let files = deployments::project_files(c, target)?;
                deployments::set_project_files(c, &d.id, &files)
            })?;
            let (p, f) = self.store.with(|c| deployments::built_info(c, target))?;
            port = port.or(p);
            framework = if framework.is_empty() { f } else { framework };
        }
        let reusable = !image.is_empty()
            && (app.kind == AppKind::Image || self.deployer.docker.image_exists(&image).await.unwrap_or(false));
        let inline_compose = app.kind == AppKind::Compose && app.source.repository.is_none();
        if inline_compose {
            // A catalogue service: the compose file is part of the app.
            let dir = self.builder.work_dir.join(&d.id).join("src");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join("docker-compose.yml"),
                app.source.compose_content.as_deref().unwrap_or_default(),
            )?;
            log.info("using the compose file of the app (no repository)");
            compose_job = Some(("docker-compose.yml".into(), dir));
            self.store
                .tx(|c| deployments::transition(c, &d.id, DeploymentStatus::Deploying, None))?;
        } else if reusable {
            log.info(&format!("using the existing image {image} (no build)"));
            self.store
                .tx(|c| deployments::transition(c, &d.id, DeploymentStatus::Deploying, None))?;
        } else {
            if !image.is_empty() {
                log.info(&format!(
                    "{image} is gone (pruned): rebuilding commit {}",
                    short(&d.commit_sha)
                ));
            }
            self.store
                .tx(|c| deployments::transition(c, &d.id, DeploymentStatus::Building, None))?;
            self.emit(EngineEvent::Changed);
            let _permit = self.gate.acquire(&mut |m| log.info(m)).await;
            if cancel.is_cancelled() {
                return Err(Error::Conflict("cancelled".into()));
            }
            let rev = if !d.commit_sha.is_empty() {
                d.commit_sha.clone()
            } else if !d.branch.is_empty() {
                d.branch.clone()
            } else {
                "main".into()
            };
            let build_env: Vec<(String, String)> = vars
                .iter()
                .filter(|(_, v)| v.build)
                .map(|(k, v)| (k.clone(), v.value.to_string()))
                .collect();
            let (auth, _key_file) = self.auth(app)?;
            let built = self
                .builder
                .build(
                    BuildRequest {
                        app_name: &app.name,
                        deployment_id: &d.id,
                        kind: app.kind,
                        source: &app.source,
                        build: &app.build,
                        rev: &rev,
                        auth,
                        build_env,
                    },
                    cancel.clone(),
                    log,
                )
                .await?;
            if let PlanKind::Compose(file) = &built.plan.kind {
                compose_job = Some((file.clone(), built.checkout.clone().unwrap_or_default()));
            }
            image = built.image.clone();
            port = Some(built.plan.port);
            framework = built.plan.framework.clone();
            let (sha, msg) = (built.commit_sha.clone(), built.commit_message.clone());
            let (img, fw, p) = (image.clone(), framework.clone(), built.plan.port);
            let files = built.project_files.clone();
            self.store.tx(|c| {
                deployments::set_built(c, &d.id, &sha, &msg, &img, p, &fw)?;
                deployments::set_project_files(c, &d.id, &files)?;
                deployments::transition(c, &d.id, DeploymentStatus::Deploying, None)
            })?;
        }
        if cancel.is_cancelled() {
            return Err(Error::Conflict("cancelled".into()));
        }
        self.emit(EngineEvent::Changed);
        let d = self.store.with(|c| deployments::get(c, &d.id))?;
        let port = app.runtime.port.or(port).unwrap_or(3000);
        if d.image != image {
            let img = image.clone();
            self.store.with(|c| deployments::set_image(c, &d.id, &img))?;
        }

        // Where it will be reachable.
        let settings = self.store.with(domain_settings)?;
        let (hosts, url) = hosts_for(&settings, app, d.pr_number);
        if d.pr_number.is_some() && hosts.is_empty() {
            log.info("no wildcard domain is set: the preview runs but has no URL");
        }

        let mut runtime_env: Vec<(String, String)> =
            vars.iter().map(|(k, v)| (k.clone(), v.value.to_string())).collect();
        let system = [
            ("PORT", port.to_string()),
            (
                "VERCELIFY_ENV",
                if preview { "preview" } else { "production" }.to_string(),
            ),
            ("VERCELIFY_URL", url.clone().unwrap_or_default()),
            ("VERCELIFY_GIT_COMMIT_SHA", d.commit_sha.clone()),
            ("VERCELIFY_GIT_BRANCH", d.branch.clone()),
            ("VERCELIFY_DEPLOYMENT_ID", d.id.clone()),
            ("SOURCE_COMMIT", d.commit_sha.clone()),
        ];
        for (k, v) in system {
            // The app's own value wins (e.g. a fixed PORT the image expects).
            if !vars.contains_key(k) {
                runtime_env.push((k.to_string(), v));
            }
        }
        let vols = self.store.with(|c| volumes::list(c, VolumeOwner::App, &app.id))?;
        let previous = if preview {
            self.store
                .with(|c| previews::get(c, &app.id, d.pr_number.unwrap_or(0)))?
                .and_then(|p| p.deployment_id)
        } else {
            app.current_deployment_id.clone()
        };
        let previous_container = match &previous {
            Some(p) if *p != d.id => self.store.with(|c| deployments::container(c, p)).ok(),
            _ => None,
        }
        .filter(|c| !c.is_empty());

        let basic_auth = self.store.with(|c| apps::basic_auth(c, &app.id))?;
        let input = DeployInput {
            app,
            deployment: &d,
            image: &image,
            env: runtime_env,
            volumes: &vols,
            port,
            hosts,
            basic_auth,
            dns_challenge: false,
        };
        let container = match &compose_job {
            Some((file, checkout)) => {
                let _cleanup = CheckoutGuard(checkout.parent().map(PathBuf::from).unwrap_or_else(|| checkout.clone()));
                let project = compose::project_name(&app.name, d.pr_number);
                let res = compose::up(
                    compose::Up {
                        docker: &self.deployer.docker,
                        dir: checkout,
                        file,
                        project: &project,
                        service: app.runtime.service.as_deref(),
                        env: &input.env,
                        labels: compose::Labels {
                            app_id: &app.id,
                            app_name: &app.name,
                            deployment_id: &d.id,
                            project_id: &app.project_id,
                            pr: d.pr_number,
                        },
                        timeout: Duration::from_secs(app.runtime.health_timeout_secs as u64),
                    },
                    cancel.clone(),
                    &mut |s, l| log.line(s, l),
                )
                .await?;
                // `compose up --wait` only knows the services' own health
                // checks: probe the routed one like any other deployment.
                log.info(&format!("waiting for {res} to be healthy"));
                crate::health::wait_healthy(
                    crate::health::Wait {
                        docker: &self.deployer.docker,
                        container: &res,
                        network: crate::docker::PROXY_NETWORK,
                        port,
                        probe: deploy::probe_for(app.runtime.health.as_ref()),
                        timeout: Duration::from_secs(app.runtime.health_timeout_secs as u64),
                        interval: self.deployer.health_interval,
                    },
                    &mut |m| log.info(m),
                )
                .await
                .map_err(|e| Error::Invalid(format!("{e} (the compose project stays up; fix it and deploy again)")))?;
                res
            }
            None => self.deployer.start(&input, &mut |m| log.info(m)).await?,
        };
        if cancel.is_cancelled() {
            let _ = self.deployer.docker.remove(&container).await;
            return Err(Error::Conflict("cancelled".into()));
        }
        // Commit point: route, then record. A crash in between leaves the
        // route on a healthy container; the reconciler adopts it.
        if let Err(e) = self.deployer.switch(&input, &container) {
            let _ = self.deployer.docker.remove(&container).await;
            return Err(e);
        }
        let (id, app_id, c2, url2) = (d.id.clone(), app.id.clone(), container.clone(), url.clone());
        let pr = d.pr_number;
        let done = self.store.tx(move |c| {
            deployments::set_container(c, &id, &c2, None)?;
            deployments::transition(c, &id, DeploymentStatus::Healthy, None)?;
            match pr {
                Some(n) => {
                    if let Some(prev) = previews::get(c, &app_id, n)?.and_then(|p| p.deployment_id) {
                        if prev != id {
                            if let Ok(p) = deployments::get(c, &prev) {
                                if p.status == DeploymentStatus::Healthy {
                                    deployments::transition(c, &prev, DeploymentStatus::Superseded, None)?;
                                }
                            }
                        }
                    }
                    previews::set_serving(c, &app_id, n, &id, url2.as_deref().unwrap_or(""))?;
                }
                None => {
                    deployments::promote(c, &app_id, &id)?;
                }
            }
            deployments::get(c, &id)
        })?;
        if let Some(old) = previous_container.filter(|o| *o != container) {
            let deployer = self.deployer.clone();
            let drain = Duration::from_secs(app.runtime.drain_secs as u64);
            log.info(&format!(
                "draining the previous version ({old}) for {}s",
                drain.as_secs()
            ));
            tokio::spawn(async move { deployer.retire(&old, drain).await });
        }
        if !preview {
            let keep = vec![image.clone()];
            if let Err(e) = self.deployer.prune_images(&app.name, &keep).await {
                log.info(&format!("image cleanup skipped: {e}"));
            }
        }
        if !framework.is_empty() {
            tracing::debug!("{} deployed ({framework})", app.name);
        }
        Ok((done, url, previous.filter(|p| *p != d.id)))
    }
}

/// Removes a compose checkout when the deployment step ends.
struct CheckoutGuard(PathBuf);

impl Drop for CheckoutGuard {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// How long a new production deployment is watched for automatic rollback.
pub const WATCH_AFTER_DEPLOY: Duration = Duration::from_secs(180);

impl Engine {
    /// Automatic rollback (design §7.2): a deployment that crashes or keeps
    /// restarting within 3 minutes of going live is replaced by the one before.
    async fn watch_after_deploy(self: Arc<Self>, app: App, d: Deployment, previous: String) {
        let container = match self.store.with(|c| deployments::container(c, &d.id)) {
            Ok(c) if !c.is_empty() => c,
            _ => return,
        };
        let start = std::time::Instant::now();
        let mut base_restarts = None;
        while start.elapsed() < WATCH_AFTER_DEPLOY {
            tokio::time::sleep(Duration::from_secs(10)).await;
            // Something else took over (a newer deploy, a manual rollback, a stop).
            let current = self
                .store
                .with(|c| apps::get(c, &app.id))
                .ok()
                .filter(|a| a.desired == vercelify_core::platform::Desired::Running)
                .and_then(|a| a.current_deployment_id);
            if current.as_deref() != Some(d.id.as_str()) {
                return;
            }
            let i = match self.deployer.docker.inspect(&container).await {
                Ok(Some(i)) => i,
                Ok(None) => return, // removed: the reconciler's business
                Err(_) => continue,
            };
            if i.paused {
                continue;
            }
            let base = *base_restarts.get_or_insert(i.restart_count);
            let crashing = !i.running || i.restart_count >= base + 2;
            if !crashing {
                continue;
            }
            let why = if i.oom_killed {
                "ran out of memory".to_string()
            } else if !i.running {
                format!("exited with code {}", i.exit_code)
            } else {
                format!("restarted {} times", i.restart_count - base)
            };
            tracing::warn!("{}: deployment #{} {why}; rolling back", app.name, d.number);
            let mut log = self.logs.writer(&app.name, &d.id);
            log.error(&format!("after going live the app {why}: rolling back automatically"));
            drop(log);
            match self.enqueue(
                &app.id,
                Enqueue {
                    trigger: Some(DeploymentTrigger::AutoRollback),
                    rollback_of: Some(previous.clone()),
                    ..Enqueue::default()
                },
            ) {
                Ok(rb) => self.emit(EngineEvent::Failed {
                    app: app.name.clone(),
                    deployment: d.clone(),
                    error: format!("{why} after going live; rolled back (deployment #{})", rb.number),
                }),
                Err(e) => tracing::error!("{}: automatic rollback failed: {e}", app.name),
            }
            return;
        }
    }

    /// Take a preview down: container(s), route, its own volumes, and the
    /// preview record (closed).
    pub async fn remove_preview(&self, app_id: &str, pr: u32) -> Result<()> {
        let (app, preview, vols) = self.store.with(|c| {
            Ok((
                apps::get(c, app_id)?,
                previews::get(c, app_id, pr)?,
                volumes::list(c, VolumeOwner::App, app_id)?,
            ))
        })?;
        let docker = &self.deployer.docker;
        if let Some(dep) = preview.as_ref().and_then(|p| p.deployment_id.clone()) {
            let container = self.store.with(|c| deployments::container(c, &dep)).unwrap_or_default();
            if !container.is_empty() {
                docker.stop(&container, 10).await?;
                docker.remove(&container).await?;
            }
        }
        if app.kind == AppKind::Compose {
            compose::down(docker, &compose::project_name(&app.name, Some(pr))).await?;
        }
        crate::proxy::remove_route(&self.deployer.proxy, &deploy::route_key(&app.name, Some(pr)))?;
        for v in &vols {
            let name = volumes::docker_name(&format!("{}-pr{pr}", app.name), &v.name);
            let _ = docker.volume_remove(&name).await;
            let _ = std::fs::remove_dir_all(self.deployer.hdd_root.join(&name));
        }
        self.store.tx(|c| {
            previews::close(c, app_id, pr)?;
            // Queued deployments of a closed preview won't run.
            for d in deployments::active(c)? {
                if d.app_id == app_id && d.pr_number == Some(pr) && d.status == DeploymentStatus::Queued {
                    deployments::transition(c, &d.id, DeploymentStatus::Cancelled, Some("the pull request closed"))?;
                }
            }
            Ok(())
        })?;
        self.emit(EngineEvent::Changed);
        Ok(())
    }

    /// After an app was deleted from the database: remove what it left in
    /// Docker and on disk. Volumes are removed only with `purge_volumes`.
    pub async fn cleanup_app(&self, app: &App, purge_volumes: bool, vols: &[vercelify_core::platform::AppVolume]) {
        let docker = &self.deployer.docker;
        if let Ok(all) = docker.list_managed().await {
            for c in all.iter().filter(|c| c.labels.get(deploy::LABEL_APP) == Some(&app.id)) {
                let _ = docker.remove(&c.name).await;
            }
        }
        if app.kind == AppKind::Compose {
            let _ = compose::down(docker, &compose::project_name(&app.name, None)).await;
        }
        for key in crate::proxy::route_keys(&self.deployer.proxy) {
            if key == app.name || key.starts_with(&format!("{}-pr-", app.name)) {
                let _ = crate::proxy::remove_route(&self.deployer.proxy, &key);
            }
        }
        if let Ok(images) = docker.images(&format!("vfy/{}", app.name)).await {
            for (tag, _) in images {
                let _ = docker.remove_image(&format!("vfy/{}:{tag}", app.name)).await;
            }
        }
        if purge_volumes {
            for v in vols {
                let name = volumes::docker_name(&app.name, &v.name);
                let _ = docker.volume_remove(&name).await;
                if let Some(p) = &v.host_path {
                    let _ = std::fs::remove_dir_all(p);
                }
                let _ = std::fs::remove_dir_all(self.deployer.hdd_root.join(&name));
            }
        }
        self.logs.remove_app(&app.name);
        self.emit(EngineEvent::Changed);
    }
}

/// The hosts routed to an app (production) or one of its previews, and the
/// URL people use.
pub fn hosts_for(settings: &apps::DomainSettings, app: &App, pr: Option<u32>) -> (Vec<RouteHost>, Option<String>) {
    match pr {
        Some(pr) => match settings.wildcard.as_deref().filter(|w| !w.is_empty()) {
            Some(w) => {
                let host = deploy::preview_host(&app.name, pr, w);
                let scheme = if settings.https || settings.edge_tls {
                    "https"
                } else {
                    "http"
                };
                (
                    vec![RouteHost {
                        host: host.clone(),
                        https: settings.https && !settings.edge_tls,
                        redirect_to: None,
                    }],
                    Some(format!("{scheme}://{host}")),
                )
            }
            None => (vec![], None),
        },
        None => (
            app.domains
                .iter()
                .map(|dm| RouteHost {
                    host: dm.host.clone(),
                    https: dm.https && dm.cert_status != CertStatus::Edge,
                    redirect_to: dm.redirect_to.clone(),
                })
                .collect(),
            app.url.clone(),
        ),
    }
}

fn trigger_or(t: Option<DeploymentTrigger>, default: DeploymentTrigger) -> DeploymentTrigger {
    match t {
        Some(t @ (DeploymentTrigger::AutoRollback | DeploymentTrigger::Recover)) => t,
        _ => default,
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

fn pin(c: &rusqlite::Connection, a: &App, source: AppSource) -> Result<()> {
    apps::patch(
        c,
        &a.id,
        &AppPatch {
            source: Some(source),
            ..AppPatch::default()
        },
    )
    .map(|_| ())
}

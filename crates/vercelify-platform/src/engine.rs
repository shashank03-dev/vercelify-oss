//! The deployment engine: takes queued deployments through the state machine
//! (design §7): queued → building → deploying → healthy, or failed/cancelled.
//!
//! One deployment per app at a time (a lock per app); builds additionally go
//! through the [`BuildGate`]. The database is updated at every step, so a
//! restart knows exactly where things stood (`deployments::fail_interrupted`).

use crate::apps::{self, domain_settings};
use crate::build::{BuildRequest, Builder};
use crate::deploy::{self, Deployer};
use crate::detect::PlanKind;
use crate::fleet::{self, Placed, RouteBase};
use crate::gate::BuildGate;
use crate::git::GitAuth;
use crate::instances::{self, Instance};
use crate::logs::{LogHub, LogWriter};
use crate::proc::{cancel_pair, CancelHandle};
use crate::proxy::RouteHost;
use crate::runtime::{Apply, StartSpec};
use crate::secrets::MasterKey;
use crate::{
    compose, deployments, env, git_sources, nodes, placement, previews, registry, volumes, Error, Result, Store,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use vercelify_core::platform::{
    App, AppKind, AppPatch, AppSource, CertStatus, Deployment, DeploymentStatus, DeploymentTrigger, GitAuthKind,
    PlatformNode, PreviewAccess, VolumeOwner,
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
    pub(crate) app_runtime: tokio::sync::Mutex<()>,
    /// A preview's publication (check it is open, route, record) and its
    /// closing don't interleave: a deployment of a closed pull request
    /// never goes live after `remove_preview` closed it.
    preview_commit: tokio::sync::Mutex<()>,
    database_ops: Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    listener: Mutex<Option<Listener>>,
    /// This server's private address (mesh): where databases'
    /// public ports listen unless opened to everyone.
    private_ip: Mutex<Option<String>>,
    /// Where the image registry keeps its data, and the addresses it is
    /// published on (the mesh one): set once there are other servers.
    registry: Mutex<Option<(PathBuf, Vec<String>)>>,
    minter: Mutex<Option<TokenMinter>>,
    /// Memory held for deployments being placed (placement.rs).
    pub reservations: placement::Reservations,
    pub capacity_refresh: Arc<Notify>,
}

pub(crate) struct AppOperation<'a> {
    engine: &'a Engine,
    id: String,
}

impl Drop for AppOperation<'_> {
    fn drop(&mut self) {
        lock(&self.engine.busy).remove(&self.id);
        self.engine.wake.notify_one();
    }
}

/// Makes a GitHub App's installation token for (git source, repository).
pub type TokenMinter = Arc<
    dyn Fn(String, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>> + Send + Sync,
>;

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
        let capacity_refresh = Arc::new(Notify::new());
        let reservations = placement::Reservations::new(store.clone()).with_refresh(capacity_refresh.clone());
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
            database_ops: Mutex::default(),
            app_runtime: tokio::sync::Mutex::new(()),
            preview_commit: tokio::sync::Mutex::new(()),
            listener: Mutex::default(),
            private_ip: Mutex::default(),
            registry: Mutex::default(),
            minter: Mutex::default(),
            reservations,
            capacity_refresh,
        })
    }

    pub fn capacity_updated(&self) {
        self.wake.notify_one();
    }

    pub async fn admit(
        &self,
        operation: &str,
        requests: &[crate::capacity::Request],
    ) -> Result<crate::capacity::Permit> {
        for attempt in 0..10 {
            match self
                .store
                .tx(|c| crate::capacity::reserve(c, operation, requests, chrono::Utc::now().timestamp()))
            {
                Ok(()) => {
                    return Ok(crate::capacity::Permit::after_reserved(
                        self.store.clone(),
                        operation.into(),
                        self.capacity_refresh.clone(),
                    ))
                }
                Err(e) if matches!(&e, Error::Capacity(_)) && attempt < 9 => {
                    self.capacity_refresh.notify_one();
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!("admission loop returns on its last attempt")
    }

    fn limited_app(&self, app: &App, d: &Deployment) -> Result<App> {
        let node = self.store.with(|c| match d.node_id.as_ref().or(app.node_id.as_ref()) {
            Some(id) => nodes::get(c, id),
            None => nodes::main(c),
        })?;
        let total = node
            .status
            .capacity
            .as_ref()
            .map(|s| s.total_mb.min(s.host_grant_mb.unwrap_or(s.total_mb)))
            .unwrap_or(node.status.memory_total_mb);
        let mut app = app.clone();
        app.runtime
            .memory_mb
            .get_or_insert(crate::capacity::default_app_mb(total));
        app.runtime.cpus.get_or_insert(1.0);
        Ok(app)
    }

    /// Stop only our dedicated idle builder. Holding all build slots across
    /// inspect and stop excludes a concurrently starting build.
    pub async fn stop_idle_builder(&self, timeout: Duration) -> Result<bool> {
        if self.builder.driver != crate::build::Driver::Container {
            return Ok(false);
        }
        let Some(_slots) = self.gate.try_idle(timeout) else {
            return Ok(false);
        };
        let docker = crate::docker::Docker {
            bin: self.builder.docker.clone(),
        };
        let Some(state) = docker.inspect(crate::build::BUILDER_CONTAINER).await? else {
            return Ok(false);
        };
        if !state.running || state.paused || self.gate.held().is_some() {
            return Ok(false);
        }
        // Stop retains the container and cache volume; never remove/prune.
        docker.stop(crate::build::BUILDER_CONTAINER, 10).await?;
        Ok(true)
    }

    pub(crate) fn app_operation(&self, id: &str) -> Result<AppOperation<'_>> {
        if !lock(&self.busy).insert(id.to_string()) {
            return Err(Error::Conflict(
                "this app already has an operation running; try again when it finishes".into(),
            ));
        }
        Ok(AppOperation {
            engine: self,
            id: id.to_string(),
        })
    }

    /// Serialize database starts, moves, backup/restore and removal. Weak
    /// entries keep this bounded after operations and database deletion.
    pub(crate) async fn database_operation(&self, id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let operation = {
            let mut operations = lock(&self.database_ops);
            operations.retain(|_, v| v.strong_count() > 0);
            let operation = operations
                .get(id)
                .and_then(std::sync::Weak::upgrade)
                .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(())));
            operations.insert(id.to_string(), Arc::downgrade(&operation));
            operation
        };
        operation.lock_owned().await
    }

    pub fn set_private_ip(&self, ip: Option<String>) {
        *lock(&self.private_ip) = ip;
    }

    pub fn private_ip(&self) -> Option<String> {
        lock(&self.private_ip).clone()
    }

    pub fn set_registry(&self, dir: PathBuf, binds: Vec<String>) {
        *lock(&self.registry) = Some((dir, binds));
    }

    /// The registry, running and answering, before a push.
    pub async fn ensure_registry(&self) -> Result<()> {
        let (dir, binds) = lock(&self.registry)
            .clone()
            .ok_or_else(|| Error::Unavailable("the image registry isn't set up on this server yet".into()))?;
        registry::ensure(&self.deployer.docker, &dir, &binds).await?;
        registry::wait_ready(Duration::from_secs(20)).await
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
        self.enqueue_with_build(app, req, None)
    }

    /// Enqueue and lease in the same transaction: no dispatcher can start a
    /// target-side build between creation of the deployment and its job.
    pub fn enqueue_external(
        &self,
        app: &str,
        owner: &str,
        req: &vercelify_core::platform::LocalBuildRequest,
    ) -> Result<vercelify_core::platform::BuildJob> {
        if req.plan.port == 0 || req.plan.dockerfile.is_empty() {
            return Err(Error::Invalid(
                "a build plan needs a Dockerfile and a nonzero port".into(),
            ));
        }
        let d = self.enqueue_with_build(
            app,
            Enqueue {
                commit_sha: Some(req.commit_sha.clone()),
                pr_number: req.pr_number,
                trigger: Some(DeploymentTrigger::Manual),
                ..Default::default()
            },
            Some((owner, req)),
        )?;
        self.store.with(|c| crate::offhost::get(c, &d.id))
    }

    fn enqueue_with_build(
        &self,
        app: &str,
        req: Enqueue,
        external: Option<(&str, &vercelify_core::platform::LocalBuildRequest)>,
    ) -> Result<Deployment> {
        let d = self.store.tx(|c| {
            let a = apps::find(c, app)?;
            let trigger = req.trigger.unwrap_or(DeploymentTrigger::Manual);
            if let Some(sha) = &req.commit_sha {
                crate::validate::commit_sha(sha)?;
                // A git server fetches a commit by its full id only.
                if sha.len() != 40 {
                    return Err(Error::Invalid(format!(
                        "`{sha}`: give the full 40-character commit hash (git rev-parse {sha})"
                    )));
                }
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
            let d = deployments::create(
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
            )?;
            if let Some((owner, request)) = external {
                if a.kind != AppKind::Git {
                    return Err(Error::Invalid("off-host build jobs require a Git app".into()));
                }
                let node = match a.node_id.as_deref() {
                    Some(id) => nodes::get(c, id)?,
                    None => nodes::main(c)?,
                };
                let platform = format!("linux/{}", registry::docker_arch(&node.arch));
                if request.platform != platform {
                    return Err(Error::Invalid(format!("this app needs {platform}")));
                }
                c.execute(
                    "UPDATE deployments SET node_id=?2 WHERE id=?1",
                    rusqlite::params![d.id, node.id],
                )?;
                let revision = crate::offhost::revision(c, &self.key, &a.id, req.pr_number.is_some())?;
                crate::offhost::create(
                    c,
                    &d.id,
                    owner,
                    &revision,
                    &platform,
                    &request.plan,
                    crate::offhost::timestamp(),
                )?;
            }
            Ok(d)
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

    /// Delete an old deployment and its log (see [`deployments::delete`]).
    pub fn delete_deployment(&self, id: &str) -> Result<Deployment> {
        if lock(&self.running).contains_key(id) {
            return Err(Error::Conflict("this deployment is running: cancel it first".into()));
        }
        let (d, app) = self.store.tx(|c| {
            let d = deployments::delete(c, id)?;
            let app: String = c.query_row("SELECT name FROM apps WHERE id = ?1", [&d.app_id], |r| r.get(0))?;
            Ok((d, app))
        })?;
        let _ = std::fs::remove_file(self.logs.path(&app, &d.id));
        self.emit(EngineEvent::Changed);
        Ok(d)
    }

    /// The dispatcher: start every queued deployment whose app is free.
    pub async fn run(self: Arc<Self>) {
        loop {
            if let Err(e) = self
                .store
                .tx(|c| crate::offhost::expire(c, crate::offhost::timestamp()))
            {
                tracing::warn!("off-host build expiry: {e}");
            }
            match self.store.with(deployments::active) {
                Ok(active) => {
                    for d in active.into_iter().filter(|d| d.status == DeploymentStatus::Queued) {
                        if !self
                            .store
                            .with(|c| crate::offhost::dispatchable(c, &d.id))
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        if !self.gate.builds_enabled() && d.image.is_empty() {
                            let no_builder = match self.store.with(|c| apps::get(c, &d.app_id)) {
                                Ok(app) => self.needs_build(&app, &d).await.unwrap_or(true),
                                Err(_) => false,
                            };
                            if no_builder {
                                let _=self.store.tx(|c| {c.execute("UPDATE deployments SET error='queued: no permitted builder; use vfy deploy APP --build-here or CI' WHERE id=?1 AND error NOT LIKE 'queued: no permitted builder%'",[&d.id])?;Ok(())});
                                continue;
                            }
                        }
                        if !lock(&self.busy).insert(d.app_id.clone()) {
                            continue;
                        }
                        let prepared = match self.store.with(|c| apps::get(c, &d.app_id)) {
                            Ok(a) => self.choose(&a, &d).await,
                            Err(e) => Err(e),
                        };
                        if let Err(e) = prepared {
                            if matches!(&e, Error::Capacity(_) | Error::Unavailable(_)) {
                                let reason = format!("queued: {e}");
                                let _=self.store.tx(|c| {c.execute("UPDATE deployments SET error=?2 WHERE id=?1 AND status='queued' AND coalesce(error,'')!=?2",rusqlite::params![d.id,reason])?;Ok(())});
                                self.capacity_refresh.notify_one();
                                lock(&self.busy).remove(&d.app_id);
                                continue;
                            }
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
            _ => {
                self.reservations.finish(&d.id, false);
                return;
            }
        };
        let app = match self.store.with(|c| apps::get(c, &d.app_id)) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("deployment {} of a missing app: {e}", d.id);
                self.reservations.finish(&d.id, false);
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
        // Git, image pulls and failed starts can consume resources before an
        // instance is recorded. A fresh inventory, not the instances table,
        // decides when this operation's physical demand can be released.
        self.reservations.finish(&d.id, true);
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
                let msg = log.redact_text(&msg);
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

    /// Credentials for fetching `repo` from a git source; the key file
    /// (deploy keys) lives as long as the returned guard. GitHub Apps get an
    /// installation token for the repository from the minter.
    pub async fn git_auth(&self, source: Option<&str>, repo: &str) -> Result<(GitAuth, Option<git_sources::KeyFile>)> {
        if let Some(id) = source.filter(|s| !s.is_empty()) {
            if self.store.with(|c| git_sources::get(c, id))?.auth == GitAuthKind::GithubApp {
                let token = self.mint(id, repo).await?;
                return Ok((
                    GitAuth::Token {
                        user: "x-access-token".into(),
                        token,
                    },
                    None,
                ));
            }
        }
        let tmp = self.builder.work_dir.join(".keys");
        let known_hosts = self.builder.work_dir.join("known_hosts");
        self.store
            .with(|c| git_sources::auth(c, &self.key, source, &tmp, &known_hosts))
    }

    /// A token for the provider's API (statuses, pull requests) of `repo`.
    pub async fn api_token(&self, source: Option<&str>, repo: &str) -> Option<String> {
        let id = source.filter(|s| !s.is_empty())?;
        match self.store.with(|c| git_sources::get(c, id)).ok()?.auth {
            GitAuthKind::GithubApp => self.mint(id, repo).await.ok(),
            _ => self.store.with(|c| git_sources::token(c, &self.key, id)).ok().flatten(),
        }
    }

    pub fn set_token_minter(&self, m: TokenMinter) {
        *lock(&self.minter) = Some(m);
    }

    async fn mint(&self, source: &str, repo: &str) -> Result<String> {
        let m = lock(&self.minter)
            .clone()
            .ok_or_else(|| Error::Unavailable("GitHub App tokens can't be made here".into()))?;
        m(source.to_string(), repo.to_string()).await
    }

    async fn needs_build(&self, app: &App, d: &Deployment) -> Result<bool> {
        if app.kind == AppKind::Image || self.store.with(|c| crate::offhost::is_external(c, &d.id))? {
            return Ok(false);
        }
        if app.kind == AppKind::Compose {
            if app.source.repository.is_none() {
                if let Some(raw) = &app.source.compose_content {
                    return compose::requires_build(raw);
                }
            }
            return Ok(true);
        }
        Ok(d.image.is_empty() || !self.deployer.docker.image_exists(&d.image).await?)
    }

    /// Where a deployment's instances go (design §8.3): the main server,
    /// and (server, replica) for each instance.
    async fn choose(&self, app: &App, d: &Deployment) -> Result<(PlatformNode, Vec<(PlatformNode, u8)>)> {
        let app = self.limited_app(app, d)?;
        let (all, vols) = self
            .store
            .with(|c| Ok((nodes::list(c)?, volumes::checked_list(c, VolumeOwner::App, &app.id)?)))?;

        let main = all
            .iter()
            .find(|n| n.role == "main")
            .cloned()
            .ok_or_else(|| Error::Unavailable("the main server isn't registered yet".into()))?;
        let mut extra = Vec::new();
        let database_preview = d.pr_number.is_some() && app.previews.database.is_some();
        if database_preview {
            if d.node_id
                .as_ref()
                .or(app.node_id.as_ref())
                .is_some_and(|id| id != &main.id)
            {
                return Err(Error::Invalid(
                    "built-in PostgreSQL previews currently run on the main node".into(),
                ));
            }
            let binding = self
                .store
                .tx(|c| crate::preview_db::prepare(c, &self.key, &app, d.pr_number.unwrap()))?;
            let service = self
                .store
                .with(|c| crate::databases::get(c, &self.key, &binding.service_id, false))?;
            extra.push(crate::capacity::Request {
                node_id: main.id.clone(),
                owner: service.id,
                kind: "database".into(),
                memory_mb: u64::from(service.memory_mb.unwrap_or(256)) + 128,
                disk_mb: 1024,
                heavy: true,
            });
        }
        if self.needs_build(&app, d).await? {
            if !self.gate.builds_enabled() || !main.builds {
                return Err(Error::Capacity(
                    "no permitted builder; use the selected computer or CI".into(),
                ));
            }
            if self.builder.driver != crate::build::Driver::Container {
                return Err(Error::Capacity("this serving-host builder cannot enforce its memory budget; select the container driver or build on the selected computer/CI".into()));
            }
            self.builder.check_running_budget().await?;
            extra.push(crate::capacity::Request {
                node_id: main.id.clone(),
                owner: "builder".into(),
                kind: "build".into(),
                memory_mb: crate::capacity::build_memory_mb(self.builder.memory.as_deref())?,
                disk_mb: 2048,
                heavy: true,
            });
        }
        let total = main
            .status
            .capacity
            .as_ref()
            .map(|s| s.total_mb.min(s.host_grant_mb.unwrap_or(s.total_mb)))
            .unwrap_or(main.status.memory_total_mb);
        let need = placement::Need {
            pinned: d.node_id.clone().or_else(|| app.node_id.clone()),
            replicas: if d.pr_number.is_some() { 1 } else { app.runtime.replicas },
            memory_mb: app.runtime.memory_mb,
            overhead_mb: crate::capacity::transfer_mb(total),
            disk_mb: 1024,
            extra,
            main_only: app.kind == AppKind::Compose || database_preview,
            one_node: !vols.is_empty(),
            failover: app.runtime.failover,
        };
        let slots = self.reservations.place(&d.id, &all, &need)?;
        let targets = slots
            .into_iter()
            .filter_map(|s| all.iter().find(|n| n.id == s.node_id).map(|n| (n.clone(), s.replica)))
            .collect();
        Ok((main, targets))
    }

    /// The port of an image app nobody set one for: the image's one TCP
    /// `EXPOSE`. None or several: stop and ask, rather than guess a port the
    /// health check then waits on for minutes.
    async fn image_port(&self, image: &str, log: &mut LogWriter) -> Result<u16> {
        let docker = &self.deployer.docker;
        if !docker.image_exists(image).await.unwrap_or(false) {
            log.info(&format!("pulling {image} to read its port"));
            docker.pull(image).await?;
        }
        match docker.exposed_tcp_ports(image).await?.as_slice() {
            [p] => {
                log.info(&format!(
                    "listening port {p}, from the image's EXPOSE (set a port to change it)"
                ));
                Ok(*p)
            }
            [] => Err(Error::Invalid(format!(
                "{image} doesn't say which port it listens on (no EXPOSE): set the app's port"
            ))),
            many => Err(Error::Invalid(format!(
                "{image} exposes several ports ({}): set the app's port to the one that serves HTTP",
                many.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
            ))),
        }
    }

    async fn steps(
        self: &Arc<Self>,
        app: &App,
        d: &Deployment,
        cancel: crate::proc::Cancel,
        log: &mut LogWriter,
    ) -> Result<(Deployment, Option<String>, Option<String>)> {
        let limited = self.limited_app(app, d)?;
        let app = &limited;
        let preview = d.pr_number.is_some();
        let (main, targets) = self.choose(app, d).await?;
        let remote = targets.iter().any(|(n, _)| n.role != "main");
        if remote || targets.len() > 1 {
            let names: Vec<&str> = targets.iter().map(|(n, _)| n.name.as_str()).collect();
            log.info(&format!("runs on: {}", names.join(", ")));
        }
        let main_ip = if remote {
            Some(main.mesh_ip.clone()).filter(|ip| !ip.is_empty()).ok_or_else(|| {
                Error::Unavailable("the main server has no mesh address yet (is vercelify-mesh up?)".into())
            })?
        } else {
            String::new()
        };
        let resolution = {
            let key = self.key.clone();
            let id = app.id.clone();
            let ip = main_ip.clone();
            self.store
                .call(move |c| {
                    env::resolve_for_deploy(c, &key, &id, preview, Some(ip.as_str()).filter(|i| !i.is_empty()))
                })
                .await?
        };
        // Secret values never show in this deployment's log, whatever the
        // build prints.
        log.redact(crate::logs::Redactor::new(
            resolution.vars.values().filter(|v| v.secret).map(|v| v.value.as_str()),
        ));
        if preview && !resolution.withheld.is_empty() {
            let keys: Vec<&str> = resolution.withheld.keys().map(String::as_str).collect();
            log.info(&format!(
                "previews don't get production-only variables: {} (give them preview values, or share them with previews)",
                keys.join(", ")
            ));
        }
        let vars = resolution.vars;
        let native = main.arch.clone();
        // Builds happen here; other architectures are built too (emulated).
        let mut extra_archs: Vec<String> = vec![];
        if app.kind == AppKind::Git {
            for (n, _) in &targets {
                let a = registry::docker_arch(&n.arch);
                if a != registry::docker_arch(&native) && !extra_archs.iter().any(|x| registry::docker_arch(x) == a) {
                    extra_archs.push(n.arch.clone());
                }
            }
        }
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
        let docker = &self.deployer.docker;
        let mut reusable =
            !image.is_empty() && (app.kind == AppKind::Image || docker.image_exists(&image).await.unwrap_or(false));
        if reusable && app.kind != AppKind::Image {
            for a in &extra_archs {
                if !docker
                    .image_exists(&registry::arch_tag(&image, a, &native))
                    .await
                    .unwrap_or(false)
                {
                    log.info(&format!("no {a} image of it is left: rebuilding"));
                    reusable = false;
                }
            }
        }
        if self.store.with(|c| crate::offhost::is_external(c, &d.id))? {
            if !reusable {
                return Err(Error::Unavailable(
                    "off-host image is missing; rebuild on the selected computer or CI (no serving-host fallback)"
                        .into(),
                ));
            }
            self.store
                .with(|c| crate::offhost::verify_revision(c, &self.key, &d.id))?;
        }
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
            if !self.gate.builds_enabled() {
                return Err(Error::Unavailable(
                    "no permitted builder; rebuild on the selected computer or CI".into(),
                ));
            }
            // Cancelling doesn't wait for the power guard or a free slot.
            let mut cancelled = cancel.clone();
            let mut waiting = |m: &str| log.info(m);
            let _permit = tokio::select! {
                p = self.gate.acquire(&mut waiting) => p,
                () = cancelled.cancelled() => return Err(Error::Conflict("cancelled".into())),
            };
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
            let (auth, _key_file) = self
                .git_auth(
                    app.source.git_source_id.as_deref(),
                    app.source.repository.as_deref().unwrap_or(""),
                )
                .await?;
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
                        extra_archs: extra_archs.clone(),
                        native_arch: native.clone(),
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
        let port = match app.runtime.port.or(port) {
            Some(p) => p,
            None if app.kind == AppKind::Image => {
                let p = self.image_port(&image, log).await?;
                self.store.tx(|c| deployments::set_port(c, &d.id, p))?;
                p
            }
            None => 3000,
        };
        if d.image != image {
            let img = image.clone();
            self.store.with(|c| deployments::set_image(c, &d.id, &img))?;
        }

        // Where it will be reachable.
        let settings = self.store.with(domain_settings)?;
        let (hosts, url) = hosts_for(&settings, app, d.pr_number);
        if d.pr_number.is_some() && hosts.is_empty() {
            log.info(if app.previews.access == PreviewAccess::Private {
                "previews are private (mesh only) and this server isn't on the mesh yet: the preview runs but has no URL"
            } else {
                "no wildcard domain is set: the preview runs but has no URL"
            });
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
        runtime_env.retain(|(key, _)| key != "VERCELIFY_PR_NUMBER");
        runtime_env.push(("VERCELIFY_PR_NUMBER".into(), d.pr_number.unwrap_or(0).to_string()));
        self.provision_preview_db(app, &d, &image, &mut runtime_env, cancel.clone(), log)
            .await?;
        let vols = self.store.with(|c| match d.pr_number {
            Some(pr) => volumes::list_for_preview(c, &app.id, pr),
            None => volumes::checked_list(c, VolumeOwner::App, &app.id),
        })?;
        let previous = if preview {
            self.store
                .with(|c| previews::get(c, &app.id, d.pr_number.unwrap_or(0)))?
                .and_then(|p| p.deployment_id)
        } else {
            app.current_deployment_id.clone()
        };
        let previous_placed = match &previous {
            Some(p) if *p != d.id => self.placed(p).unwrap_or_default(),
            _ => vec![],
        };
        let basic_auth = self.store.with(|c| apps::basic_auth(c, &app.id))?;

        let started: Vec<Placed> = match &compose_job {
            Some((file, checkout)) => {
                let _cleanup = CheckoutGuard(checkout.parent().map(PathBuf::from).unwrap_or_else(|| checkout.clone()));
                let project = compose::project_name(&app.name, d.pr_number);
                let build = compose::requires_build(&std::fs::read_to_string(checkout.join(file))?)?;
                let _compose_permit = if build {
                    if !self.gate.builds_enabled() {
                        return Err(Error::Unavailable("no permitted builder".into()));
                    }
                    let mut cancelled = cancel.clone();
                    let mut waiting = |m: &str| log.info(m);
                    let permit = tokio::select! {
                        permit = self.gate.acquire(&mut waiting) => permit,
                        () = cancelled.cancelled() => return Err(Error::Conflict("cancelled".into())),
                    };
                    self.builder.ensure_builder(log).await?;
                    Some(permit)
                } else {
                    None
                };
                let res = compose::up(
                    compose::Up {
                        limits: compose::Limits {
                            memory_mb: app.runtime.memory_mb.expect("deployment budget"),
                            cpus: app.runtime.cpus.expect("deployment CPU budget"),
                            cgroup_parent: self.deployer.cgroup_parent.as_deref(),
                        },
                        build,
                        docker,
                        dir: checkout,
                        file,
                        project: &project,
                        service: app.runtime.service.as_deref(),
                        env: &runtime_env,
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
                        docker,
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
                vec![Placed {
                    node: main.clone(),
                    instance: Instance {
                        node_id: main.id.clone(),
                        container: res,
                        replica: 0,
                    },
                }]
            }
            None => {
                let mut started: Vec<Placed> = vec![];
                let mut pushed: HashMap<String, String> = HashMap::new();
                for (node, replica) in &targets {
                    let res = self
                        .start_one(
                            app,
                            &d,
                            node,
                            *replica,
                            StartArgs {
                                image: &image,
                                native: &native,
                                env: &runtime_env,
                                volumes: &vols,
                                port,
                            },
                            &mut pushed,
                            log,
                        )
                        .await;
                    // An instance started as the cancel came is removed with
                    // the others: it is in `started` before the check.
                    if let Ok(p) = &res {
                        started.push(p.clone());
                    }
                    let res = match res {
                        Ok(_) if cancel.is_cancelled() => Err(Error::Conflict("cancelled".into())),
                        other => other,
                    };
                    if let Err(e) = res {
                        self.remove_placed(&started).await;
                        return Err(e);
                    }
                }
                started
            }
        };
        if cancel.is_cancelled() {
            self.remove_placed(&started).await;
            return Err(Error::Conflict("cancelled".into()));
        }
        // A preview publishes only while its pull request is open, and
        // `remove_preview` can't close it halfway through (see preview_commit).
        let _commit = match d.pr_number {
            Some(n) => {
                let guard = self.preview_commit.lock().await;
                let open = self
                    .store
                    .with(|c| previews::get(c, &app.id, n))?
                    .is_some_and(|p| p.status == "active");
                if !open || cancel.is_cancelled() {
                    self.remove_placed(&started).await;
                    return Err(Error::Conflict("cancelled".into()));
                }
                Some(guard)
            }
            None => None,
        };
        // Commit point: route, then record. A crash in between leaves the
        // route on healthy containers; the reconciler adopts them.
        let base = RouteBase {
            key: deploy::route_key(&app.name, d.pr_number),
            hosts,
            port,
            basic_auth,
            dns_challenge: false,
        };
        if let Err(e) = self.switch_routes(&base, &started, &previous_placed, &main.id).await {
            self.remove_placed(&started).await;
            return Err(e);
        }
        if previous_placed.is_empty() {
            if let Some(h) = base.hosts.first() {
                crate::proxy::wait_routed(self.deployer.proxy.http_port, &h.host, Duration::from_secs(5)).await;
            }
        }
        let (id, app_id, url2) = (d.id.clone(), app.id.clone(), url.clone());
        let pr = d.pr_number;
        let list: Vec<Instance> = started.iter().map(|p| p.instance.clone()).collect();
        let primary = targets.first().map(|(n, _)| n.id.clone());
        let done = self.store.tx(move |c| {
            instances::set(c, &id, &list)?;
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
                    // Placed once: later deploys go to the same server (volumes stay put).
                    if let Some(node) = &primary {
                        c.execute(
                            "UPDATE apps SET node_id = ?2 WHERE id = ?1 AND node_id IS NULL",
                            rusqlite::params![app_id, node],
                        )?;
                    }
                }
            }
            deployments::get(c, &id)
        })?;
        self.retire_previous(
            &base.key,
            &previous_placed,
            &started,
            Duration::from_secs(app.runtime.drain_secs as u64),
            log,
        );
        if !preview {
            self.prune_after(&app.name, &image, &native, &started).await;
        }
        if !framework.is_empty() {
            tracing::debug!("{} deployed ({framework})", app.name);
        }
        Ok((done, url, previous.filter(|p| *p != d.id)))
    }

    /// Start one instance on `node` (its image pushed to the registry first
    /// when it runs elsewhere), healthy, or an error.
    #[allow(clippy::too_many_arguments)]
    async fn start_one(
        &self,
        app: &App,
        d: &Deployment,
        node: &PlatformNode,
        replica: u8,
        a: StartArgs<'_>,
        pushed: &mut HashMap<String, String>,
        log: &mut LogWriter,
    ) -> Result<Placed> {
        let rt = self.node_runtime(node)?;
        let img = if app.kind == AppKind::Image && rt.is_local() {
            a.image.to_string()
        } else {
            // Image apps elsewhere get the image through this server too:
            // pulled once here (for the server's architecture), so workers
            // need no internet access or registry credentials of their own.
            let source = if app.kind == AppKind::Image {
                let base = crate::build::image_tag(&app.name, &d.id);
                let tag = registry::arch_tag(&base, &node.arch, a.native);
                registry::stage(&self.deployer.docker, a.image, &node.arch, a.native, &tag, &mut |m| {
                    log.info(m)
                })
                .await?;
                tag
            } else {
                a.image.to_string()
            };
            let variant = if app.kind == AppKind::Image {
                source
            } else {
                registry::arch_tag(&source, &node.arch, a.native)
            };
            if rt.is_local() {
                variant
            } else if let Some(r) = pushed.get(&variant) {
                r.clone()
            } else {
                if pushed.is_empty() {
                    self.ensure_registry().await?;
                }
                let r = registry::push(&self.deployer.docker, &variant, &mut |m| log.info(m)).await?;
                pushed.insert(variant, r.clone());
                r
            }
        };
        if replica > 0 || !rt.is_local() {
            log.info(&format!("starting replica {} on {}", replica + 1, node.name));
        }
        let spec = StartSpec {
            app: app.clone(),
            deployment: d.clone(),
            image: img,
            env: a.env.to_vec(),
            volumes: a.volumes.to_vec(),
            port: a.port,
            replica,
        };
        let container = rt.start_instance(&spec, &mut |m| log.info(m)).await?;
        Ok(Placed {
            node: node.clone(),
            instance: Instance {
                node_id: node.id.clone(),
                container,
                replica,
            },
        })
    }

    /// Remove instances (a failed or cancelled deployment's).
    pub(crate) async fn remove_placed(&self, placed: &[Placed]) {
        for p in placed {
            match self.node_runtime(&p.node) {
                Ok(rt) => {
                    if let Err(e) = rt.remove(&p.instance.container).await {
                        tracing::warn!("removing {}: {e}", p.instance.container);
                    }
                }
                Err(e) => tracing::warn!("removing {}: {e}", p.instance.container),
            }
        }
    }

    /// Point every server's proxy at the new instances: the other servers
    /// first, so the main server never forwards to a server without the
    /// route. Without hosts the routes go everywhere they were.
    pub(crate) async fn switch_routes(
        &self,
        base: &RouteBase,
        placed: &[Placed],
        previous: &[Placed],
        main_id: &str,
    ) -> Result<()> {
        let _route_update = self.app_runtime.lock().await;
        let routes = fleet::routes(base, placed, main_id);
        if routes.is_empty() {
            let mut seen: HashSet<String> = HashSet::new();
            for p in placed.iter().chain(previous) {
                if p.node.id != main_id && seen.insert(p.node.id.clone()) {
                    let rt = self.node_runtime(&p.node)?;
                    rt.apply_ok(&Apply {
                        remove_routes: vec![base.key.clone()],
                        ..Apply::default()
                    })
                    .await?;
                }
            }
            return crate::proxy::remove_route(&self.deployer.proxy, &base.key);
        }
        for (node_id, route) in routes.iter().filter(|(id, _)| id.as_str() != main_id) {
            let node = placed
                .iter()
                .find(|p| &p.node.id == node_id)
                .map(|p| p.node.clone())
                .ok_or_else(|| Error::Internal(format!("no server {node_id}")))?;
            self.node_runtime(&node)?
                .apply_ok(&Apply {
                    write_routes: vec![route.clone()],
                    ..Apply::default()
                })
                .await?;
        }
        if let Some(r) = routes.get(main_id) {
            crate::proxy::write_route(&self.deployer.proxy, r)?;
        }
        Ok(())
    }

    /// After the switch: the previous instances drain, then go; servers
    /// that served them and don't serve the new ones drop the route.
    fn retire_previous(
        self: &Arc<Self>,
        key: &str,
        previous: &[Placed],
        started: &[Placed],
        drain: Duration,
        log: &mut LogWriter,
    ) {
        let new: HashSet<&str> = started.iter().map(|p| p.instance.container.as_str()).collect();
        let new_nodes: HashSet<&str> = started.iter().map(|p| p.node.id.as_str()).collect();
        let old: Vec<Placed> = previous
            .iter()
            .filter(|p| !new.contains(p.instance.container.as_str()))
            .cloned()
            .collect();
        if old.is_empty() {
            return;
        }
        let names: Vec<&str> = old.iter().map(|p| p.instance.container.as_str()).collect();
        log.info(&format!(
            "draining the previous version ({}) for {}s",
            names.join(", "),
            drain.as_secs()
        ));
        let mut leave: Vec<PlatformNode> = vec![];
        for p in &old {
            if p.node.role != "main"
                && !new_nodes.contains(p.node.id.as_str())
                && !leave.iter().any(|n| n.id == p.node.id)
            {
                leave.push(p.node.clone());
            }
        }
        let me = self.clone();
        let key = key.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(drain).await;
            for p in &old {
                match me.node_runtime(&p.node) {
                    Ok(rt) => rt.retire(&p.instance.container, Duration::ZERO).await,
                    Err(e) => tracing::warn!("retiring {}: {e}", p.instance.container),
                }
            }
            for n in &leave {
                if let Ok(rt) = me.node_runtime(n) {
                    let _ = rt
                        .apply(&Apply {
                            remove_routes: vec![key.clone()],
                            ..Apply::default()
                        })
                        .await;
                }
            }
        });
    }

    /// Keep the 5 newest images of the app on every server it runs on
    /// (and in the registry: what this server no longer keeps).
    async fn prune_after(&self, app: &str, image: &str, native: &str, started: &[Placed]) {
        let mut done: HashSet<String> = HashSet::new();
        let mut keep: Vec<String> = vec![image.to_string()];
        for p in started {
            keep.push(registry::arch_tag(image, &p.node.arch, native));
        }
        let mut remote = false;
        for p in started {
            if !done.insert(p.node.id.clone()) {
                continue;
            }
            let Ok(rt) = self.node_runtime(&p.node) else { continue };
            remote |= !rt.is_local();
            let keep: Vec<String> = if rt.is_local() {
                keep.clone()
            } else {
                keep.iter().map(|k| registry::reference(k)).collect()
            };
            match rt
                .apply(&Apply {
                    prune: Some((app.to_string(), keep)),
                    ..Apply::default()
                })
                .await
            {
                Ok(a) => {
                    for e in a.errors {
                        tracing::info!("image cleanup on {}: {e}", p.node.name);
                    }
                }
                Err(e) => tracing::info!("image cleanup on {}: {e}", p.node.name),
            }
        }
        if !started.iter().any(|p| p.node.role == "main") {
            let _ = self.deployer.prune_images(app, &keep).await;
        }
        if remote {
            if let Err(e) = registry::prune(&self.deployer.docker, app).await {
                tracing::info!("registry cleanup for {app}: {e}");
            }
        }
    }
}

pub(crate) struct StartArgs<'a> {
    image: &'a str,
    native: &'a str,
    env: &'a [(String, String)],
    volumes: &'a [vercelify_core::platform::AppVolume],
    port: u16,
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
        let placed = match self.placed(&d.id) {
            Ok(p) if !p.is_empty() => p,
            _ => return,
        };
        let start = std::time::Instant::now();
        let mut base_restarts: HashMap<String, u64> = HashMap::new();
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
            let mut crashed = None;
            for p in &placed {
                let Ok(rt) = self.node_runtime(&p.node) else { continue };
                let i = match rt.inspect(&p.instance.container).await {
                    Ok(Some(i)) => i,
                    Ok(None) => return, // removed: the reconciler's business
                    Err(_) => continue, // its server is unreachable: not the app's fault
                };
                if i.paused {
                    continue;
                }
                let base = *base_restarts
                    .entry(p.instance.container.clone())
                    .or_insert(i.restart_count);
                if !i.running || i.restart_count >= base + 2 {
                    crashed = Some((i, base));
                    break;
                }
            }
            let Some((i, base)) = crashed else { continue };
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
    /// preview record (closed). It is closed first, so its deployments still
    /// under way are cancelled and can't go live; the teardown waits for
    /// them, so nothing of theirs appears after it. Database-preview cleanup
    /// stays closed and retries from its durable ownership journal.
    pub async fn remove_preview(&self, app_id: &str, pr: u32) -> Result<()> {
        let app = self.store.with(|c| apps::get(c, app_id))?;
        let database_owned = self.store.with(|c| crate::preview_db::live(c, app_id, pr))?.is_some();
        let (was_open, in_flight) = {
            let _commit = self.preview_commit.lock().await;
            self.store.tx(|c| {
                let was_open = previews::close(c, app_id, pr)?.is_some_and(|p| p.status == "active");
                let mut running = vec![];
                for d in deployments::active(c)? {
                    if d.app_id != app_id || d.pr_number != Some(pr) {
                        continue;
                    }
                    if d.status == DeploymentStatus::Queued {
                        // Queued deployments of a closed preview won't run.
                        deployments::transition(
                            c,
                            &d.id,
                            DeploymentStatus::Cancelled,
                            Some("the pull request closed"),
                        )?;
                    } else {
                        running.push(d.id);
                    }
                }
                Ok((was_open, running))
            })?
        };
        for id in &in_flight {
            if let Some(h) = lock(&self.running).get(id) {
                h.cancel();
            }
        }
        let wait = Duration::from_secs(u64::from(app.runtime.health_timeout_secs) + 300);
        if !self.wait_finished(&in_flight, wait).await {
            tracing::warn!(
                "{} #{pr}: a deployment didn't stop in time; removing the preview anyway",
                app.name
            );
        }
        let errors = self.reconcile_preview_databases().await;
        if let Some(error) = errors.first() {
            return Err(Error::Unavailable(error.clone()));
        }
        if self.store.with(|c| crate::preview_db::live(c, app_id, pr))?.is_some() {
            return Err(Error::Unavailable(
                "preview is closed; database cleanup is pending".into(),
            ));
        }
        // The PG journal already removed this lifecycle's runtime and network.
        // A reopened generation must not be torn down by this older callback.
        if database_owned {
            self.emit(EngineEvent::Changed);
            return Ok(());
        }
        if let Err(e) = self.teardown_preview(&app, pr).await {
            if was_open {
                self.store.tx(|c| previews::reopen(c, app_id, pr))?;
            }
            self.emit(EngineEvent::Changed);
            return Err(e);
        }
        self.emit(EngineEvent::Changed);
        Ok(())
    }

    /// Upgrade safety: old previews may retain production credentials and
    /// shared bridges. Drain the durable migration list before starting jobs.
    pub async fn retire_legacy_previews(&self) -> Result<()> {
        let mut pending: Vec<(String, u32)> = self
            .store
            .with(|c| Ok(crate::store::get_setting(c, "preview_safety_retire")?.unwrap_or_default()))?;
        while let Some((app, pr)) = pending.first().cloned() {
            if self.store.with(|c| apps::get(c, &app)).is_ok() {
                self.remove_preview(&app, pr).await?;
                tracing::warn!(
                    "{app} PR #{pr}: retired preview for the safety upgrade; review preview variables and redeploy"
                );
            }
            pending.remove(0);
            self.store
                .tx(|c| crate::store::put_setting(c, "preview_safety_retire", &pending))?;
        }
        Ok(())
    }

    /// Whether the deployments `ids` all ended within `timeout`.
    async fn wait_finished(&self, ids: &[String], timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let busy = {
                let running = lock(&self.running);
                ids.iter().any(|id| running.contains_key(id))
            } || self
                .store
                .with(deployments::active)
                .map(|a| a.iter().any(|d| ids.contains(&d.id)))
                .unwrap_or(false);
            if !busy {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn teardown_preview(&self, app: &App, pr: u32) -> Result<()> {
        self.teardown_preview_owned(&app.id, &app.name, app.kind == AppKind::Compose, pr)
            .await
    }

    pub(crate) async fn teardown_preview_owned(
        &self,
        app_id: &str,
        app_name: &str,
        compose_app: bool,
        pr: u32,
    ) -> Result<()> {
        let (preview, vols) = self
            .store
            .with(|c| Ok((previews::get(c, app_id, pr)?, volumes::list_for_preview(c, app_id, pr)?)))?;
        let docker = &self.deployer.docker;
        let key = deploy::route_key(app_name, Some(pr));
        let vol_names: Vec<String> = vols
            .iter()
            .map(|v| volumes::physical_name(v, app_name, Some(pr)))
            .collect();
        if let Some(dep) = preview.as_ref().and_then(|p| p.deployment_id.clone()) {
            for p in self.placed(&dep)? {
                let rt = self.node_runtime(&p.node)?;
                rt.apply_ok(&Apply {
                    stop: vec![p.instance.container.clone()],
                    remove: vec![p.instance.container.clone()],
                    grace_secs: Some(10),
                    remove_routes: if rt.is_local() { vec![] } else { vec![key.clone()] },
                    remove_volumes: if rt.is_local() { vec![] } else { vol_names.clone() },
                    remove_preview_networks: if rt.is_local() {
                        vec![]
                    } else {
                        vec![crate::preview_network::name(app_id, pr)]
                    },
                    ..Apply::default()
                })
                .await?;
            }
        }
        if compose_app {
            compose::down(docker, &compose::project_name(app_name, Some(pr))).await?;
            compose::remove_preview_volumes(docker, app_id, pr).await?;
        }
        crate::proxy::remove_route(&self.deployer.proxy, &key)?;
        if preview.is_some() || self.store.with(|c| crate::preview_db::live(c, app_id, pr))?.is_some() {
            crate::preview_network::remove(docker, &crate::preview_network::name(app_id, pr)).await?;
        }
        for name in &vol_names {
            let _ = docker.volume_remove(name).await;
            let _ = std::fs::remove_dir_all(self.deployer.hdd_root.join(name));
        }
        Ok(())
    }

    /// After an app was deleted from the database: remove what it left in
    /// Docker and on disk. Volumes are removed only with `purge_volumes`.
    pub async fn cleanup_app(&self, app: &App, purge_volumes: bool, vols: &[vercelify_core::platform::AppVolume]) {
        let docker = &self.deployer.docker;
        // Other servers: its containers, routes, images (and volumes).
        let workers: Vec<PlatformNode> = self
            .store
            .with(nodes::list)
            .unwrap_or_default()
            .into_iter()
            .filter(|n| n.role != "main" && n.online)
            .collect();
        for n in &workers {
            let Ok(rt) = self.node_runtime(n) else { continue };
            let mut a = Apply {
                remove_preview_apps: vec![app.id.clone()],
                prune: Some((app.name.clone(), vec![])),
                ..Apply::default()
            };
            if let Ok(all) = rt.list().await {
                a.remove = all
                    .iter()
                    .filter(|c| c.labels.get(deploy::LABEL_APP) == Some(&app.id))
                    .map(|c| c.name.clone())
                    .collect();
            }
            if let Ok(keys) = rt.route_keys().await {
                a.remove_routes = keys
                    .into_iter()
                    .filter(|k| *k == app.name || k.starts_with(&format!("{}-pr-", app.name)))
                    .collect();
            }
            if purge_volumes {
                a.remove_volumes = vols
                    .iter()
                    .map(|v| volumes::physical_name(v, &app.name, None))
                    .collect();
            }
            if let Err(e) = rt.apply(&a).await {
                tracing::warn!("cleaning {} up on {}: {e}", app.name, n.name);
            }
        }
        if let Ok(all) = docker.list_managed().await {
            for c in all.iter().filter(|c| c.labels.get(deploy::LABEL_APP) == Some(&app.id)) {
                let _ = docker.remove(&c.name).await;
            }
        }
        if app.kind == AppKind::Compose {
            let _ = compose::down(docker, &compose::project_name(&app.name, None)).await;
        }
        if let Err(e) = crate::preview_network::remove_for_app(docker, &app.id).await {
            tracing::warn!("cleaning preview networks of {}: {e}", app.name);
        }
        // Its project's network, unless other apps or databases still use it
        // (Docker refuses then).
        let _ = docker
            .run(
                &crate::proc::args(&["network", "rm", &deploy::project_network(&app.project_id)]),
                Duration::from_secs(30),
            )
            .await;
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
                let name = volumes::physical_name(v, &app.name, None);
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
        // Private (design §9.1): this server's mesh name, plain HTTP inside
        // the WireGuard tunnel, answered only to mesh addresses.
        Some(pr) if app.previews.access == PreviewAccess::Private => match &settings.mesh {
            Some(m) => {
                let host = deploy::preview_host(&app.name, pr, &format!("{}.vfy", m.node));
                (
                    vec![RouteHost {
                        host: host.clone(),
                        https: false,
                        redirect_to: None,
                        allow_from: m.cidrs.clone(),
                    }],
                    Some(format!("http://{host}")),
                )
            }
            None => (vec![], None),
        },
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
                        allow_from: vec![],
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
                    allow_from: vec![],
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{apps::DomainSettings, build::Driver, docker::Docker, proxy::ProxyConfig};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    /// A Docker that answers everything and keeps track of containers
    /// (`<dir>/containers/<name>`): `create --name N` waits while
    /// `hold-create` exists in `dir` (after touching `created`), then makes
    /// N, running on the proxy network; `rm` removes it.
    fn stub_docker(dir: &Path) -> String {
        let bin = dir.join("docker");
        let d = dir.display();
        std::fs::create_dir_all(dir.join("containers")).unwrap();
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"$*\" >> {d}/calls\nfor a; do last=$a; done\ncase \"$1\" in\n  \
                 create) touch {d}/created; while [ -f {d}/hold-create ]; do sleep 0.02; done; \
                 if [ -f {d}/fail-create ]; then cat {d}/fail-create >&2; exit 1; fi; \
                 touch {d}/containers/$3; prev=''; for a; do [ \"$prev\" != --network ] || echo \"$a\" > {d}/network-$3; prev=$a; done; echo cid ;;\n  \
                 network) case \"$2\" in inspect) echo '{{\"Driver\":\"bridge\",\"Internal\":true,\"EnableIPv6\":false,\"Options\":{{\"com.docker.network.bridge.gateway_mode_ipv4\":\"isolated\"}},\"Labels\":{{\"vercelify.preview-network\":\"true\",\"vercelify.managed\":\"platform\"}}}}' ;; esac ;;\n  \
                 rm) rm -f {d}/containers/$last ;;\n  \
                 image) case \"$4\" in *ExposedPorts*) cat {d}/exposed 2>/dev/null || echo '{{\"80/tcp\":{{}}}}' ;; esac ;;\n  \
                 container) [ -f {d}/containers/$last ] || {{ echo \"Error: No such container: $last\" >&2; exit 1; }}; \
                 net=$(cat {d}/network-$last 2>/dev/null || echo vfy-proxy); \
                 echo '{{\"State\":{{\"Running\":true,\"Status\":\"running\"}},\"NetworkSettings\":\
                 {{\"Networks\":{{\"vfy-proxy\":{{\"IPAddress\":\"127.0.0.1\"}},\"'\"$net\"'\":{{\"IPAddress\":\"127.0.0.1\"}}}}}}}}' ;;\nesac\nexit 0\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin.display().to_string()
    }

    pub(crate) fn engine(dir: &Path, concurrency: usize) -> Arc<Engine> {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store
            .with(|c| {
                let node = nodes::ensure_main(c, "main", "http://127.0.0.1:7420", "x86_64")?;
                nodes::set_main_status(
                    c,
                    "",
                    &vercelify_core::platform::NodeStatus {
                        memory_total_mb: 8192,
                        memory_available_mb: 7000,
                        docker_ok: true,
                        capacity: Some(crate::capacity::Sample {
                            epoch: "test".into(),
                            measured_at: chrono::Utc::now().timestamp(),
                            total_mb: 8192,
                            available_mb: 7000,
                            workload_limit_mb: 7168,
                            disk_free_mb: Some(20000),
                            host_ready: true,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )?;
                Ok(node)
            })
            .unwrap();
        store
            .with(|c| {
                crate::store::put_setting(
                    c,
                    crate::apps::DOMAIN_SETTING,
                    &DomainSettings {
                        wildcard: Some("example.test".into()),
                        https: false,
                        edge_tls: false,
                        mesh: None,
                    },
                )
            })
            .unwrap();
        store
            .with(|c| {
                crate::apps::set_mesh_ingress(
                    c,
                    Some(&crate::apps::MeshIngress {
                        node: "main".into(),
                        cidrs: vec!["10.77.0.0/16".into()],
                    }),
                )
            })
            .unwrap();
        let docker = stub_docker(dir);
        Engine::new(
            store,
            Arc::new(MasterKey::from_bytes(&[4; 32]).unwrap()),
            Builder {
                docker: docker.clone(),
                git: "/nonexistent/git".into(),
                work_dir: dir.join("work"),
                driver: Driver::Container,
                memory: None,
                timeout: Duration::from_secs(10),
            },
            Deployer {
                docker: Docker { bin: docker },
                proxy: ProxyConfig {
                    dir: dir.join("proxy"),
                    // Nothing listens: the first-route wait returns at once.
                    http_port: 1,
                    https_port: 2,
                    metrics_port: 3,
                    acme_email: None,
                    dns_challenge: false,
                    image: crate::proxy::TRAEFIK_IMAGE.into(),
                },
                hdd_root: dir.join("hdd"),
                cgroup_parent: None,
                health_interval: Duration::from_millis(10),
            },
            BuildGate::new(concurrency, dir.join("power")).with_poll(Duration::from_millis(20)),
            LogHub::new(dir.join("logs")),
        )
    }

    #[tokio::test]
    async fn missing_capacity_keeps_an_image_deployment_queued() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path(), 1);
        e.store
            .with(|c| crate::nodes::set_main_status(c, "", &vercelify_core::platform::NodeStatus::default()))
            .unwrap();
        let mut request = apps::tests::git_app("capacity-wait");
        request.kind = AppKind::Image;
        request.source.image = Some("test:one".into());
        let app = e.store.tx(|c| apps::create(c, &request)).unwrap();
        let d = e.enqueue(&app.id, Enqueue::default()).unwrap();
        let task = tokio::spawn(e.clone().run());
        tokio::time::sleep(Duration::from_millis(150)).await;
        let d = e.store.with(|c| deployments::get(c, &d.id)).unwrap();
        task.abort();
        assert_eq!(d.status, DeploymentStatus::Queued);
        assert!(d.error.contains("capacity"), "{}", d.error);
    }

    #[tokio::test]
    async fn a_builds_disabled_server_keeps_git_work_queued() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path(), 0);
        let app = e
            .store
            .tx(|c| apps::create(c, &apps::tests::git_app("waiting")))
            .unwrap();
        let d = e.enqueue(&app.id, Enqueue::default()).unwrap();
        let queue = tokio::spawn(e.clone().run());
        tokio::time::sleep(Duration::from_millis(150)).await;
        let d = e.store.with(|c| deployments::get(c, &d.id)).unwrap();
        assert_eq!(d.status, DeploymentStatus::Queued);
        assert!(d.error.contains("no permitted builder"), "{}", d.error);
        queue.abort();
    }

    #[tokio::test]
    async fn an_external_job_waits_without_starting_a_target_build() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path(), 1);
        let app = e
            .store
            .tx(|c| apps::create(c, &apps::tests::git_app("offhost")))
            .unwrap();
        let request = vercelify_core::platform::LocalBuildRequest {
            commit_sha: "a".repeat(40),
            platform: "linux/amd64".into(),
            pr_number: None,
            plan: vercelify_core::platform::LocalBuildPlan {
                dockerfile: "FROM scratch".into(),
                context: ".".into(),
                port: 80,
                ..Default::default()
            },
        };
        let job = e.enqueue_external(&app.id, "device", &request).unwrap();
        let queue = tokio::spawn(e.clone().run());
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            e.store
                .with(|c| deployments::get(c, &job.deployment_id))
                .unwrap()
                .status,
            DeploymentStatus::Queued
        );
        queue.abort();
        e.enqueue(&app.id, Enqueue::default()).unwrap();
        assert!(e
            .store
            .with(|c| crate::offhost::authorize(c, &job.deployment_id, "device", 1, crate::offhost::timestamp()))
            .is_err());
    }

    #[tokio::test]
    async fn idle_builder_stops_only_when_safe_and_retries_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine(dir.path(), 2);
        Arc::get_mut(&mut e).unwrap().builder.driver = Driver::Container;
        let bin = dir.path().join("builder-docker");
        std::fs::write(
            &bin,
            format!(
                r#"#!/bin/sh
case "$1" in
  container) cat '{d}/builder-state' ;;
  stop) echo stop >> '{d}/builder-stops'; [ ! -f '{d}/fail-stop' ] ;;
esac
"#,
                d = dir.path().display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Arc::get_mut(&mut e).unwrap().builder.docker = bin.display().to_string();
        let timeout = Duration::from_millis(10);
        tokio::time::sleep(Duration::from_millis(20)).await;
        for state in [
            r#"{"State":{"Running":false}}"#,
            r#"{"State":{"Running":true,"Paused":true}}"#,
        ] {
            std::fs::write(dir.path().join("builder-state"), state).unwrap();
            assert!(!e.stop_idle_builder(timeout).await.unwrap());
        }
        std::fs::write(
            dir.path().join("builder-state"),
            r#"{"State":{"Running":true,"Paused":false}}"#,
        )
        .unwrap();
        let build = e.gate.acquire(&mut |_| {}).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!e.stop_idle_builder(timeout).await.unwrap());
        drop(build);
        assert!(!e.stop_idle_builder(timeout).await.unwrap());
        tokio::time::sleep(Duration::from_millis(20)).await;
        std::fs::write(dir.path().join("fail-stop"), "").unwrap();
        assert!(e.stop_idle_builder(timeout).await.is_err());
        std::fs::remove_file(dir.path().join("fail-stop")).unwrap();
        assert!(e.stop_idle_builder(timeout).await.unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("builder-stops")).unwrap(),
            "stop\nstop\n"
        );
        assert!(!e.stop_idle_builder(Duration::ZERO).await.unwrap());
        Arc::get_mut(&mut e).unwrap().builder.driver = Driver::Docker;
        assert!(!e.stop_idle_builder(timeout).await.unwrap());
    }

    fn image_app(e: &Engine) -> App {
        let mut req = apps::tests::git_app("shop");
        req.kind = AppKind::Image;
        req.source.image = Some("nginx:1.27".into());
        req.runtime = Some(vercelify_core::platform::AppRuntime {
            health: Some(vercelify_core::platform::HealthCheck {
                path: None,
                command: Some(vec!["true".into()]),
            }),
            ..Default::default()
        });
        e.store.tx(|c| apps::create(c, &req)).unwrap()
    }

    fn fresh_capacity(e: &Engine) {
        // Stub Docker creates no real processes. A new empty report acknowledges
        // completed work; this fixture deliberately has ample measured capacity.
        e.store
            .tx(|c| {
                let mut status = nodes::main(c)?.status;
                status.capacity.as_mut().unwrap().measured_at = chrono::Utc::now().timestamp() + 1;
                nodes::set_main_status(c, "", &status)
            })
            .unwrap();
        e.capacity_updated();
    }

    async fn until(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The containers the stub Docker has, of the preview of PR `n`.
    fn containers_of(dir: &Path, n: u32) -> Vec<String> {
        std::fs::read_dir(dir.join("containers"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|c| c.starts_with(&format!("vfy-shop-pr{n}")))
            .collect()
    }

    fn status(e: &Engine, id: &str) -> DeploymentStatus {
        e.store.with(|c| deployments::get(c, id)).unwrap().status
    }

    fn preview(n: u32) -> Enqueue {
        Enqueue {
            pr_number: Some(n),
            branch: Some("feature".into()),
            pr_title: Some("A change".into()),
            ..Enqueue::default()
        }
    }

    #[tokio::test]
    async fn deployment_errors_do_not_store_or_publish_secrets_from_log_tails() {
        use vercelify_core::platform::{EnvVarInput, EnvVarsPut, VarScope};
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path(), 1);
        let app = image_app(&e);
        e.store
            .tx(|c| {
                env::put(
                    c,
                    &e.key,
                    VarScope::App,
                    &app.id,
                    &EnvVarsPut {
                        vars: vec![EnvVarInput {
                            key: "TOKEN".into(),
                            value: "sensitive-runtime-token".into(),
                            build: None,
                            preview_only: None,
                            secret: Some(true),
                            share_with_previews: None,
                        }],
                        replace: false,
                    },
                )
            })
            .unwrap();
        std::fs::write(dir.path().join("fail-create"), "failed: token=sensitive-runtime-token").unwrap();
        tokio::spawn(e.clone().run());
        let dep = e.enqueue(&app.id, Enqueue::default()).unwrap();
        until("the failed deployment", || {
            status(&e, &dep.id) == DeploymentStatus::Failed
        })
        .await;
        let failed = e.store.with(|c| deployments::get(c, &dep.id)).unwrap();
        assert!(
            !failed.error.contains("sensitive-runtime-token"),
            "stored deployment error leaked a secret"
        );
        assert!(failed.error.contains("[redacted]"));
    }

    /// M1 release gate: withholding variables must not leave PR code on the
    /// network used by production databases. This fixture records Docker
    /// arguments; a passing result still requires a real network contract.
    #[tokio::test]
    async fn preview_does_not_join_the_production_database_network() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path(), 1);
        let app = image_app(&e);
        tokio::spawn(e.clone().run());
        let d = e.enqueue(&app.id, preview(7)).unwrap();
        until("the preview deployment", || {
            matches!(status(&e, &d.id), DeploymentStatus::Healthy | DeploymentStatus::Failed)
        })
        .await;
        assert_eq!(status(&e, &d.id), DeploymentStatus::Healthy);
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        let create = calls
            .lines()
            .find(|line| line.starts_with("create "))
            .expect("container created");
        let production_network = deploy::project_network(&app.project_id);
        assert!(
            !create.split_whitespace().any(|arg| arg == production_network),
            "preview joins the production database network {production_network}: {create}"
        );
        assert!(
            !calls
                .lines()
                .any(|line| line.starts_with(&format!("network connect {production_network} "))),
            "preview connects to production after creation: {calls}"
        );
    }

    #[tokio::test]
    async fn an_upgrade_retires_old_previews_before_new_work_and_survives_retry() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path(), 1);
        let app = image_app(&e);
        tokio::spawn(e.clone().run());
        let dep = e.enqueue(&app.id, preview(4)).unwrap();
        until("the old preview", || status(&e, &dep.id) == DeploymentStatus::Healthy).await;
        e.store
            .set_setting("preview_safety_retire", &vec![(app.id.clone(), 4u32)])
            .unwrap();
        e.retire_legacy_previews().await.unwrap();
        assert_eq!(
            e.store.with(|c| previews::get(c, &app.id, 4)).unwrap().unwrap().status,
            "closed"
        );
        assert!(containers_of(dir.path(), 4).is_empty());
        let pending: Vec<(String, u32)> = e.store.setting("preview_safety_retire").unwrap().unwrap();
        assert!(pending.is_empty());
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        e.retire_legacy_previews().await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("calls")).unwrap(), calls);
    }

    /// Previews are private by default: this server's mesh name, plain HTTP,
    /// mesh sources only; none without the mesh. Public ones use the wildcard.
    #[test]
    fn preview_hosts_follow_their_access() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path(), 1);
        let mut app = image_app(&e);
        let settings = e.store.with(domain_settings).unwrap();
        let (hosts, url) = hosts_for(&settings, &app, Some(7));
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].host, "pr-7-shop.main.vfy");
        assert!(!hosts[0].https);
        assert_eq!(hosts[0].allow_from, ["10.77.0.0/16"]);
        assert_eq!(url.as_deref(), Some("http://pr-7-shop.main.vfy"));
        // Production is untouched.
        assert!(hosts_for(&settings, &app, None)
            .0
            .iter()
            .all(|h| h.allow_from.is_empty()));

        let off_mesh = apps::DomainSettings {
            mesh: None,
            ..settings.clone()
        };
        assert_eq!(
            hosts_for(&off_mesh, &app, Some(7)),
            (vec![], None),
            "never falls back to public"
        );

        app.previews.access = PreviewAccess::Public;
        let (hosts, url) = hosts_for(&settings, &app, Some(7));
        assert_eq!(hosts[0].host, "pr-7-shop.example.test");
        assert!(hosts[0].allow_from.is_empty());
        assert_eq!(url.as_deref(), Some("http://pr-7-shop.example.test"));
    }

    /// An image app with no port set listens where its one TCP `EXPOSE` says
    /// (whoami: 80, not the old default 3000 that failed after minutes);
    /// several or none stop at once with what to do; a set port wins.
    #[tokio::test]
    async fn an_image_apps_port_comes_from_its_expose() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path(), 1);
        let app = image_app(&e);
        tokio::spawn(e.clone().run());
        let run = |e: &Arc<Engine>| {
            fresh_capacity(e);
            e.enqueue(&app.id, Enqueue::default()).unwrap()
        };
        let done = |e: &Engine, id: &str| matches!(status(e, id), DeploymentStatus::Healthy | DeploymentStatus::Failed);

        let d = run(&e);
        until("the first deployment", || done(&e, &d.id)).await;
        assert_eq!(status(&e, &d.id), DeploymentStatus::Healthy);
        assert_eq!(e.store.with(|c| deployments::built_info(c, &d.id)).unwrap().0, Some(80));
        let route = std::fs::read_to_string(
            e.deployer
                .proxy
                .dynamic_dir()
                .join(crate::proxy::base_file(&deploy::route_key("shop", None))),
        )
        .unwrap();
        assert!(route.contains(":80\""), "the proxy sends to port 80: {route}");

        std::fs::write(dir.path().join("exposed"), r#"{"80/tcp":{},"443/tcp":{},"53/udp":{}}"#).unwrap();
        let d = run(&e);
        until("the second deployment", || done(&e, &d.id)).await;
        let failed = e.store.with(|c| deployments::get(c, &d.id)).unwrap();
        assert_eq!(failed.status, DeploymentStatus::Failed);
        assert!(failed.error.contains("several ports (80, 443)"), "{}", failed.error);

        std::fs::write(dir.path().join("exposed"), "null").unwrap();
        let d = run(&e);
        until("the third deployment", || done(&e, &d.id)).await;
        let failed = e.store.with(|c| deployments::get(c, &d.id)).unwrap();
        assert!(failed.error.contains("no EXPOSE"), "{}", failed.error);

        // A set port wins, and the image isn't asked.
        e.store
            .tx(|c| {
                let mut patch = vercelify_core::platform::AppPatch::default();
                let mut rt = app.runtime.clone();
                rt.port = Some(8080);
                patch.runtime = Some(rt);
                apps::patch(c, &app.id, &patch)
            })
            .unwrap();
        std::fs::write(dir.path().join("calls"), "").unwrap();
        let d = run(&e);
        until("the fourth deployment", || done(&e, &d.id)).await;
        assert_eq!(status(&e, &d.id), DeploymentStatus::Healthy);
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert!(!calls.contains("ExposedPorts"), "{calls}");
    }

    /// Closing a pull request while its preview deployment is creating its
    /// container: the deployment never goes live, and nothing of it is left.
    #[tokio::test]
    async fn a_closed_preview_never_goes_live() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path(), 1);
        let app = image_app(&e);
        tokio::spawn(e.clone().run());

        // Without a close, the stub deploys a preview: the test can tell.
        let first = e.enqueue(&app.id, preview(1)).unwrap();
        until("the first preview", || {
            status(&e, &first.id) == DeploymentStatus::Healthy
        })
        .await;
        let route = e
            .deployer
            .proxy
            .dynamic_dir()
            .join(crate::proxy::base_file(&deploy::route_key("shop", Some(1))));
        assert!(route.exists(), "a live preview has a route");
        assert_eq!(containers_of(dir.path(), 1).len(), 1, "the stub keeps what was made");

        let hold = dir.path().join("hold-create");
        let created = dir.path().join("created");
        std::fs::write(&hold, "").unwrap();
        let _ = std::fs::remove_file(&created);
        fresh_capacity(&e);
        let d = e.enqueue(&app.id, preview(2)).unwrap();
        until("container creation", || created.exists()).await;
        assert_eq!(status(&e, &d.id), DeploymentStatus::Deploying);

        let closing = {
            let e = e.clone();
            let id = app.id.clone();
            tokio::spawn(async move { e.remove_preview(&id, 2).await })
        };
        // The close waits for the deployment under way instead of racing it.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!closing.is_finished(), "the teardown waits for the deployment");
        std::fs::remove_file(&hold).unwrap();
        closing.await.unwrap().unwrap();

        assert_eq!(status(&e, &d.id), DeploymentStatus::Cancelled);
        let p = e.store.with(|c| previews::get(c, &app.id, 2)).unwrap().unwrap();
        assert_eq!(p.status, "closed");
        assert_eq!(p.deployment_id, None, "it never served");
        let route2 = e
            .deployer
            .proxy
            .dynamic_dir()
            .join(crate::proxy::base_file(&deploy::route_key("shop", Some(2))));
        assert!(!route2.exists(), "no route after the close");
        assert!(e.store.with(|c| instances::of(c, &d.id)).unwrap().is_empty());
        // The container made while it closed is gone, not only one before it.
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(containers_of(dir.path(), 2), Vec::<String>::new(), "{calls}");
        assert_eq!(
            containers_of(dir.path(), 1).len(),
            1,
            "the open preview keeps its container"
        );
        // Still nothing later.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!route2.exists());
        assert_eq!(status(&e, &d.id), DeploymentStatus::Cancelled);
    }

    /// The commit point checks the pull request itself: closed by any path
    /// (here, without a cancel reaching the deployment), it doesn't publish.
    #[tokio::test]
    async fn the_commit_point_checks_the_preview_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path(), 1);
        let app = image_app(&e);
        tokio::spawn(e.clone().run());
        let hold = dir.path().join("hold-create");
        let created = dir.path().join("created");
        std::fs::write(&hold, "").unwrap();
        let d = e.enqueue(&app.id, preview(3)).unwrap();
        until("container creation", || created.exists()).await;
        e.store.tx(|c| previews::close(c, &app.id, 3)).unwrap();
        std::fs::remove_file(&hold).unwrap();
        until("the end", || status(&e, &d.id) != DeploymentStatus::Deploying).await;
        assert_eq!(status(&e, &d.id), DeploymentStatus::Cancelled);
        let route = e
            .deployer
            .proxy
            .dynamic_dir()
            .join(crate::proxy::base_file(&deploy::route_key("shop", Some(3))));
        assert!(!route.exists());
        assert_eq!(
            e.store
                .with(|c| previews::get(c, &app.id, 3))
                .unwrap()
                .unwrap()
                .deployment_id,
            None
        );
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(containers_of(dir.path(), 3), Vec::<String>::new(), "{calls}");
    }

    /// Cancelling a build that waits for the power guard, or for the only
    /// build slot, ends it at once and frees the app for the next one.
    #[tokio::test]
    async fn cancel_does_not_wait_for_the_build_gate() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path(), 1);
        let app = e.store.tx(|c| apps::create(c, &apps::tests::git_app("blog"))).unwrap();
        tokio::spawn(e.clone().run());

        // Too hot to build.
        std::fs::write(dir.path().join("power"), r#"{"state":"thermal_paused"}"#).unwrap();
        fresh_capacity(&e);
        let d = e.enqueue(&app.id, Enqueue::default()).unwrap();
        until("building", || status(&e, &d.id) == DeploymentStatus::Building).await;
        e.cancel(&d.id).unwrap();
        until("cancelled while hot", || {
            status(&e, &d.id) == DeploymentStatus::Cancelled
        })
        .await;
        assert!(e.gate.held().is_some(), "still hot: the cancel didn't wait for it");

        // Cool, but the only slot is taken.
        std::fs::write(dir.path().join("power"), r#"{"state":"ok"}"#).unwrap();
        let slot = e.gate.acquire(&mut |_| {}).await;
        fresh_capacity(&e);
        let d = e.enqueue(&app.id, Enqueue::default()).unwrap();
        until("building", || status(&e, &d.id) == DeploymentStatus::Building).await;
        e.cancel(&d.id).unwrap();
        until("cancelled while queued for a slot", || {
            status(&e, &d.id) == DeploymentStatus::Cancelled
        })
        .await;
        // The app is free: the next deployment starts (and waits its turn).
        fresh_capacity(&e);
        let next = e.enqueue(&app.id, Enqueue::default()).unwrap();
        until("the next one starts", || {
            status(&e, &next.id) == DeploymentStatus::Building
        })
        .await;
        drop(slot);
        e.cancel(&next.id).unwrap();
        until("the next one ends", || {
            status(&e, &next.id) != DeploymentStatus::Building
        })
        .await;
    }
}

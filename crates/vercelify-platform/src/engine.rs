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
    PlatformNode, VolumeOwner,
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
    /// This server's private address (mesh): where databases'
    /// public ports listen unless opened to everyone.
    private_ip: Mutex<Option<String>>,
    /// Where the image registry keeps its data, and the addresses it is
    /// published on (the mesh one): set once there are other servers.
    registry: Mutex<Option<(PathBuf, Vec<String>)>>,
    minter: Mutex<Option<TokenMinter>>,
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
            registry: Mutex::default(),
            minter: Mutex::default(),
        })
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

    /// Where a deployment's instances go (design §8.3): the main server,
    /// and (server, replica) for each instance.
    fn choose(&self, app: &App, d: &Deployment) -> Result<(PlatformNode, Vec<(PlatformNode, u8)>)> {
        self.store.with(|c| {
            let all = nodes::list(c)?;
            let main = all
                .iter()
                .find(|n| n.role == "main")
                .cloned()
                .ok_or_else(|| Error::Unavailable("the main server isn't registered yet".into()))?;
            let vols = volumes::list(c, VolumeOwner::App, &app.id)?;
            let need = placement::Need {
                pinned: app.node_id.clone(),
                replicas: if d.pr_number.is_some() { 1 } else { app.runtime.replicas },
                memory_mb: app.runtime.memory_mb,
                main_only: app.kind == AppKind::Compose,
                one_node: !vols.is_empty(),
                failover: app.runtime.failover,
            };
            let slots = placement::place(&all, &need)?;
            let targets = slots
                .into_iter()
                .filter_map(|s| all.iter().find(|n| n.id == s.node_id).map(|n| (n.clone(), s.replica)))
                .collect();
            Ok((main, targets))
        })
    }

    async fn steps(
        self: &Arc<Self>,
        app: &App,
        d: &Deployment,
        cancel: crate::proc::Cancel,
        log: &mut LogWriter,
    ) -> Result<(Deployment, Option<String>, Option<String>)> {
        let preview = d.pr_number.is_some();
        let (main, targets) = self.choose(app, d)?;
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
        let vars = {
            let key = self.key.clone();
            let id = app.id.clone();
            let ip = main_ip.clone();
            self.store
                .call(move |c| {
                    env::resolve_for_deploy(c, &key, &id, preview, Some(ip.as_str()).filter(|i| !i.is_empty()))
                })
                .await?
        };
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
        let previous_placed = match &previous {
            Some(p) if *p != d.id => self.placed(p).unwrap_or_default(),
            _ => vec![],
        };
        let basic_auth = self.store.with(|c| apps::basic_auth(c, &app.id))?;

        let started: Vec<Placed> = match &compose_job {
            Some((file, checkout)) => {
                let _cleanup = CheckoutGuard(checkout.parent().map(PathBuf::from).unwrap_or_else(|| checkout.clone()));
                let project = compose::project_name(&app.name, d.pr_number);
                let res = compose::up(
                    compose::Up {
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
                    let res = match res {
                        Ok(_) if cancel.is_cancelled() => Err(Error::Conflict("cancelled".into())),
                        other => other,
                    };
                    match res {
                        Ok(p) => started.push(p),
                        Err(e) => {
                            self.remove_placed(&started).await;
                            return Err(e);
                        }
                    }
                }
                started
            }
        };
        if cancel.is_cancelled() {
            self.remove_placed(&started).await;
            return Err(Error::Conflict("cancelled".into()));
        }
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
        let key = deploy::route_key(&app.name, Some(pr));
        let vol_names: Vec<String> = vols
            .iter()
            .map(|v| volumes::docker_name(&format!("{}-pr{pr}", app.name), &v.name))
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
                    ..Apply::default()
                })
                .await?;
            }
        }
        if app.kind == AppKind::Compose {
            compose::down(docker, &compose::project_name(&app.name, Some(pr))).await?;
        }
        crate::proxy::remove_route(&self.deployer.proxy, &key)?;
        for name in &vol_names {
            let _ = docker.volume_remove(name).await;
            let _ = std::fs::remove_dir_all(self.deployer.hdd_root.join(name));
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
                a.remove_volumes = vols.iter().map(|v| volumes::docker_name(&app.name, &v.name)).collect();
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

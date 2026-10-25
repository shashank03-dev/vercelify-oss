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
use crate::{deployments, env, previews, volumes, Error, Result, Store};
use std::collections::{HashMap, HashSet};
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
        })
    }

    pub fn set_listener(&self, l: Listener) {
        *lock(&self.listener) = Some(l);
    }

    fn emit(&self, e: EngineEvent) {
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
            if a.kind == AppKind::Image {
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
            Ok((done, url)) => {
                log.info(&format!(
                    "healthy{}",
                    url.as_deref().map(|u| format!(": {u}")).unwrap_or_default()
                ));
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

    fn auth(&self, _app: &App) -> GitAuth {
        // Git sources (tokens, deploy keys) are resolved here from M4 on.
        GitAuth::None
    }

    async fn steps(
        self: &Arc<Self>,
        app: &App,
        d: &Deployment,
        cancel: crate::proc::Cancel,
        log: &mut LogWriter,
    ) -> Result<(Deployment, Option<String>)> {
        let preview = d.pr_number.is_some();
        let vars = {
            let key = self.key.clone();
            let id = app.id.clone();
            self.store.call(move |c| env::resolve(c, &key, &id, preview)).await?
        };
        // Image: reuse (rollback, image apps) or build.
        let mut image = d.image.clone();
        let (mut port, mut framework) = self.store.with(|c| deployments::built_info(c, &d.id))?;
        if let Some(target) = &d.rollback_of {
            let (p, f) = self.store.with(|c| deployments::built_info(c, target))?;
            port = port.or(p);
            framework = if framework.is_empty() { f } else { framework };
        }
        let reusable = !image.is_empty()
            && (app.kind == AppKind::Image || self.deployer.docker.image_exists(&image).await.unwrap_or(false));
        if reusable {
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
                        auth: self.auth(app),
                        build_env,
                    },
                    cancel.clone(),
                    log,
                )
                .await?;
            if matches!(built.plan.kind, PlanKind::Compose(_)) {
                if let Some(dir) = &built.checkout {
                    let _ = std::fs::remove_dir_all(dir.parent().unwrap_or(dir));
                }
                return Err(Error::Invalid(
                    "Docker Compose apps deploy with the compose runner (not available yet on this engine)".into(),
                ));
            }
            image = built.image.clone();
            port = Some(built.plan.port);
            framework = built.plan.framework.clone();
            let (sha, msg) = (built.commit_sha.clone(), built.commit_message.clone());
            let (img, fw, p) = (image.clone(), framework.clone(), built.plan.port);
            self.store.tx(|c| {
                deployments::set_built(c, &d.id, &sha, &msg, &img, p, &fw)?;
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

        let input = DeployInput {
            app,
            deployment: &d,
            image: &image,
            env: runtime_env,
            volumes: &vols,
            port,
            hosts,
            basic_auth: vec![],
            dns_challenge: false,
        };
        let container = self.deployer.start(&input, &mut |m| log.info(m)).await?;
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
        Ok((done, url))
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

//! Converge Docker and the proxy files on what the database says (design §8.1).
//!
//! The decision is a pure function ([`plan`]) of the desired state, the
//! containers Docker reports and the route files on disk, so it is tested
//! without Docker. The reconciler never builds and never changes which
//! deployment is current: a missing container comes back as a `recover`
//! deployment of the same image, through the normal health-checked path.

use crate::apps::domain_settings;
use crate::deploy::{self, LABEL_DEPLOYMENT};
use crate::docker::ContainerState;
use crate::engine::{hosts_for, Engine, Enqueue};
use crate::proxy::{self, Route};
use crate::{apps, deployments, previews, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vercelify_core::platform::{Deployment, DeploymentTrigger, Desired};

/// Containers we don't recognise are removed only after this long, so a
/// deployment that is still being set up or drained isn't touched.
pub const ORPHAN_GRACE: Duration = Duration::from_secs(10 * 60);
/// Recovering one app is tried at most this often.
pub const RECOVER_BACKOFF: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq)]
pub struct Want {
    pub app_id: String,
    pub app_name: String,
    pub deployment_id: String,
    pub container: String,
    pub desired: Desired,
    /// Present when it should be routed.
    pub route: Option<Route>,
    /// Production (false) or a preview (true).
    pub preview: bool,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Plan {
    pub start: Vec<String>,
    pub stop: Vec<String>,
    pub remove: Vec<String>,
    /// (app id, deployment id) whose container is gone.
    pub recover: Vec<(String, String)>,
    pub write_routes: Vec<Route>,
    pub remove_routes: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        *self == Plan::default()
    }
}

/// `2026-10-25 05:00:00 +0000 UTC` (docker ps) → age.
fn age(created: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let trimmed = created.trim_end_matches(" UTC");
    let t = chrono::DateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S %z").ok()?;
    (now - t.with_timezone(&chrono::Utc)).to_std().ok()
}

/// Decide what to do. `active` = deployments in flight (their containers are
/// the engine's business); `route_keys` = base files on disk.
pub fn plan(
    wants: &[Want],
    actual: &[ContainerState],
    active: &HashSet<String>,
    route_keys: &[String],
    now: chrono::DateTime<chrono::Utc>,
) -> Plan {
    let mut p = Plan::default();
    let by_name: HashMap<&str, &ContainerState> = actual.iter().map(|c| (c.name.as_str(), c)).collect();
    let wanted: HashSet<&str> = wants.iter().map(|w| w.container.as_str()).collect();
    // Compose apps run more containers than the routed one; they carry the
    // wanted deployment's label.
    let wanted_deployments: HashSet<&str> = wants.iter().map(|w| w.deployment_id.as_str()).collect();
    for w in wants {
        match (by_name.get(w.container.as_str()), w.desired) {
            (None, Desired::Running) => p.recover.push((w.app_id.clone(), w.deployment_id.clone())),
            (None, _) => {}
            // The power guard froze it on purpose; leave it frozen.
            (Some(c), _) if c.state == "paused" => {}
            (Some(c), Desired::Running) if matches!(c.state.as_str(), "exited" | "created" | "dead") => {
                p.start.push(w.container.clone())
            }
            (Some(c), Desired::Stopped) if c.state == "running" || c.state == "restarting" => {
                p.stop.push(w.container.clone())
            }
            // Sleeping: the sleep module stops and starts it.
            _ => {}
        }
    }
    for c in actual {
        if wanted.contains(c.name.as_str()) {
            continue;
        }
        let dep = c.labels.get(LABEL_DEPLOYMENT).map(String::as_str).unwrap_or("");
        if active.contains(dep) || wanted_deployments.contains(dep) {
            continue;
        }
        if age(&c.created, now).is_some_and(|a| a >= ORPHAN_GRACE) {
            p.remove.push(c.name.clone());
        }
    }
    let mut keep: HashSet<String> = HashSet::new();
    for w in wants {
        if let Some(r) = &w.route {
            // Stopped or asleep: no route, so the wake page (or a clean 404)
            // answers instead of a 502 from a dead upstream.
            if w.desired == Desired::Running {
                keep.insert(r.key.clone());
                p.write_routes.push(r.clone());
            }
        }
    }
    // Routes of deployments in flight are the engine's (it writes them at the
    // switch); everything else not wanted goes.
    for k in route_keys {
        if !keep.contains(k) {
            p.remove_routes.push(k.clone());
        }
    }
    p
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    pub started: Vec<String>,
    pub stopped: Vec<String>,
    pub removed: Vec<String>,
    pub recovering: Vec<String>,
    pub routes_written: usize,
    pub routes_removed: usize,
    pub errors: Vec<String>,
}

pub struct Reconciler {
    pub engine: Arc<Engine>,
    last_recover: Mutex<HashMap<String, Instant>>,
}

impl Reconciler {
    pub fn new(engine: Arc<Engine>) -> Arc<Self> {
        Arc::new(Self {
            engine,
            last_recover: Mutex::default(),
        })
    }

    /// The desired containers and routes, from the database.
    pub fn wants(&self) -> Result<(Vec<Want>, HashSet<String>)> {
        self.engine.store.with(|c| {
            let settings = domain_settings(c)?;
            let mut out = vec![];
            for app in apps::list(c, None)? {
                let mut push = |d: &Deployment, preview: bool, desired: Desired| -> Result<()> {
                    let container = deployments::container(c, &d.id)?;
                    if container.is_empty() {
                        return Ok(());
                    }
                    let route = route_for(c, &settings, &app, d, &container)?;
                    out.push(Want {
                        app_id: app.id.clone(),
                        app_name: app.name.clone(),
                        deployment_id: d.id.clone(),
                        container,
                        desired,
                        route,
                        preview,
                    });
                    Ok(())
                };
                if let Some(cur) = &app.current_deployment_id {
                    let d = deployments::get(c, cur)?;
                    push(&d, false, app.desired)?;
                }
                for p in previews::list(c, &app.id, true)? {
                    if let Some(id) = &p.deployment_id {
                        let d = deployments::get(c, id)?;
                        // Previews follow the app: a stopped app's previews stop too.
                        let desired = if app.desired == Desired::Stopped {
                            Desired::Stopped
                        } else if p.asleep {
                            Desired::Sleeping
                        } else {
                            Desired::Running
                        };
                        push(&d, true, desired)?;
                    }
                }
            }
            let active: HashSet<String> = deployments::active(c)?.into_iter().map(|d| d.id).collect();
            Ok((out, active))
        })
    }

    pub async fn once(&self) -> Result<Report> {
        let (wants, active) = self.wants()?;
        let docker = &self.engine.deployer.docker;
        let actual = docker.list_managed().await?;
        let cfg = &self.engine.deployer.proxy;
        let keys = proxy::route_keys(cfg);
        // Apps with a deployment in flight keep their routes untouched.
        let busy_apps: HashSet<String> = self
            .engine
            .store
            .with(deployments::active)?
            .into_iter()
            .map(|d| d.app_id)
            .collect();
        let busy_keys: HashSet<String> = wants
            .iter()
            .filter(|w| busy_apps.contains(&w.app_id))
            .filter_map(|w| w.route.as_ref().map(|r| r.key.clone()))
            .collect();
        let keys: Vec<String> = keys.into_iter().filter(|k| !busy_keys.contains(k)).collect();
        let p = plan(&wants, &actual, &active, &keys, chrono::Utc::now());
        let mut r = Report::default();
        for name in &p.start {
            match docker.start(name).await {
                Ok(()) => r.started.push(name.clone()),
                Err(e) => r.errors.push(format!("start {name}: {e}")),
            }
        }
        for name in &p.stop {
            match docker.stop(name, 20).await {
                Ok(()) => r.stopped.push(name.clone()),
                Err(e) => r.errors.push(format!("stop {name}: {e}")),
            }
        }
        for name in &p.remove {
            match docker.remove(name).await {
                Ok(()) => r.removed.push(name.clone()),
                Err(e) => r.errors.push(format!("remove {name}: {e}")),
            }
        }
        for route in p.write_routes.iter().filter(|r| !busy_keys.contains(&r.key)) {
            match proxy::write_route(cfg, route) {
                Ok(true) => r.routes_written += 1,
                Ok(false) => {}
                Err(e) => r.errors.push(format!("route {}: {e}", route.key)),
            }
        }
        for key in &p.remove_routes {
            match proxy::remove_route(cfg, key) {
                Ok(()) => r.routes_removed += 1,
                Err(e) => r.errors.push(format!("route {key}: {e}")),
            }
        }
        for (app_id, dep) in &p.recover {
            if busy_apps.contains(app_id) {
                continue;
            }
            {
                let mut last = self.last_recover.lock().unwrap_or_else(|e| e.into_inner());
                if last.get(app_id).is_some_and(|t| t.elapsed() < RECOVER_BACKOFF) {
                    continue;
                }
                last.insert(app_id.clone(), Instant::now());
            }
            let want = wants.iter().find(|w| &w.deployment_id == dep);
            let res = match want {
                Some(w) if w.preview => {
                    // A preview's container is gone: redeploy its head.
                    let d = self.engine.store.with(|c| deployments::get(c, dep))?;
                    self.engine.enqueue(
                        app_id,
                        Enqueue {
                            trigger: Some(DeploymentTrigger::Recover),
                            commit_sha: Some(d.commit_sha.clone()).filter(|s| !s.is_empty()),
                            pr_number: d.pr_number,
                            branch: Some(d.branch.clone()),
                            ..Enqueue::default()
                        },
                    )
                }
                _ => self.engine.enqueue(
                    app_id,
                    Enqueue {
                        trigger: Some(DeploymentTrigger::Recover),
                        rollback_of: Some(dep.clone()),
                        ..Enqueue::default()
                    },
                ),
            };
            match res {
                Ok(d) => r.recovering.push(format!(
                    "{} #{}",
                    want.map(|w| w.app_name.as_str()).unwrap_or(app_id),
                    d.number
                )),
                Err(e) => r.errors.push(format!("recover {app_id}: {e}")),
            }
        }
        Ok(r)
    }

    pub async fn run(self: Arc<Self>, every: Duration) {
        loop {
            match self.once().await {
                Ok(r) if r != Report::default() => tracing::info!("reconciled: {r:?}"),
                Ok(_) => {}
                Err(e) => tracing::warn!("reconcile: {e}"),
            }
            tokio::time::sleep(every).await;
        }
    }
}

/// The route of a deployment served by `container`, if it has hosts.
pub fn route_for(
    c: &rusqlite::Connection,
    settings: &apps::DomainSettings,
    app: &vercelify_core::platform::App,
    d: &Deployment,
    container: &str,
) -> Result<Option<Route>> {
    let (port, _) = deployments::built_info(c, &d.id)?;
    let port = app.runtime.port.or(port).unwrap_or(3000);
    let (hosts, _) = hosts_for(settings, app, d.pr_number);
    if hosts.is_empty() {
        return Ok(None);
    }
    Ok(Some(Route {
        key: deploy::route_key(&app.name, d.pr_number),
        hosts,
        upstreams: vec![format!("http://{container}:{port}")],
        basic_auth: apps::basic_auth(c, &app.id)?,
        dns_challenge: false,
    }))
}

/// For the UI: what runs where, by app id.
pub fn containers_by_app(actual: &[ContainerState]) -> BTreeMap<String, Vec<ContainerState>> {
    let mut m: BTreeMap<String, Vec<ContainerState>> = BTreeMap::new();
    for c in actual {
        if let Some(a) = c.labels.get(deploy::LABEL_APP) {
            m.entry(a.clone()).or_default().push(c.clone());
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::RouteHost;

    fn want(name: &str, desired: Desired, routed: bool) -> Want {
        Want {
            app_id: format!("app_{name}"),
            app_name: name.into(),
            deployment_id: format!("dpl_{name}"),
            container: format!("vfy-{name}-1"),
            desired,
            route: routed.then(|| Route {
                key: name.into(),
                hosts: vec![RouteHost {
                    host: format!("{name}.example.com"),
                    https: true,
                    redirect_to: None,
                }],
                upstreams: vec![format!("http://vfy-{name}-1:3000")],
                basic_auth: vec![],
                dns_challenge: false,
            }),
            preview: false,
        }
    }

    fn c(name: &str, state: &str, created: &str, deployment: &str) -> ContainerState {
        ContainerState {
            id: name.into(),
            name: name.into(),
            image: "x".into(),
            state: state.into(),
            labels: [(LABEL_DEPLOYMENT.to_string(), deployment.to_string())].into(),
            created: created.into(),
        }
    }

    #[test]
    fn decisions() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let old = "2026-10-25 10:00:00 +0000 UTC";
        let fresh = "2026-10-25 11:58:00 +0000 UTC";
        let wants = vec![
            want("crashed", Desired::Running, true),
            want("gone", Desired::Running, true),
            want("off", Desired::Stopped, true),
            want("asleep", Desired::Sleeping, true),
            want("frozen", Desired::Running, true),
            want("fine", Desired::Running, true),
        ];
        let actual = vec![
            c("vfy-crashed-1", "exited", old, "dpl_crashed"),
            c("vfy-off-1", "running", old, "dpl_off"),
            c("vfy-asleep-1", "exited", old, "dpl_asleep"),
            c("vfy-frozen-1", "paused", old, "dpl_frozen"),
            c("vfy-fine-1", "running", old, "dpl_fine"),
            c("vfy-orphan-1", "running", old, "dpl_old"),
            c("vfy-new-1", "running", fresh, "dpl_other"),
            c("vfy-building-1", "running", old, "dpl_inflight"),
            c("vfy-fine-worker-1", "running", old, "dpl_fine"),
        ];
        let active: HashSet<String> = ["dpl_inflight".to_string()].into();
        let keys = vec!["fine".into(), "off".into(), "deleted-app".into()];
        let p = plan(&wants, &actual, &active, &keys, now);
        assert_eq!(p.start, ["vfy-crashed-1"]);
        assert_eq!(p.stop, ["vfy-off-1"]);
        assert_eq!(p.recover, [("app_gone".to_string(), "dpl_gone".to_string())]);
        assert_eq!(
            p.remove,
            ["vfy-orphan-1"],
            "fresh and in-flight containers are left alone"
        );
        let written: Vec<&str> = p.write_routes.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(written, ["crashed", "gone", "frozen", "fine"]);
        assert_eq!(
            p.remove_routes,
            ["off", "deleted-app"],
            "stopped apps and deleted apps lose their routes"
        );
    }

    #[test]
    fn nothing_to_do() {
        let now = chrono::Utc::now();
        let p = plan(&[], &[], &HashSet::new(), &[], now);
        assert!(p.is_empty());
        assert_eq!(
            age(
                "2026-10-25 10:00:00 +0000 UTC",
                chrono::DateTime::parse_from_rfc3339("2026-10-25T10:10:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            ),
            Some(Duration::from_secs(600))
        );
        assert_eq!(age("garbage", now), None);
    }
}

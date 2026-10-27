//! Converge every server's Docker and proxy files on what the database
//! says (design §8.1).
//!
//! The decision is a pure function ([`plan`]) of one server's desired
//! state, the containers its Docker reports and the route files it has, so
//! it is tested without Docker. The reconciler never builds and never
//! changes which deployment is current: a missing container comes back as
//! a `recover` deployment of the same image, through the normal
//! health-checked path. A server that can't be reached is left alone (its
//! containers keep running); stateless apps that may fail over
//! (`runtime.failover`) are redeployed elsewhere when it is offline.

use crate::apps::domain_settings;
use crate::deploy::{self, LABEL_DEPLOYMENT};
use crate::docker::ContainerState;
use crate::engine::{hosts_for, Engine, Enqueue};
use crate::fleet::{self, Placed, RouteBase};
use crate::proxy::Route;
use crate::runtime::Apply;
use crate::{apps, deployments, nodes, previews, volumes, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vercelify_core::platform::{Deployment, DeploymentTrigger, Desired, PlatformNode, VolumeOwner};

/// Containers we don't recognise are removed only after this long, so a
/// deployment that is still being set up or drained isn't touched.
pub const ORPHAN_GRACE: Duration = Duration::from_secs(10 * 60);
/// Recovering one app is tried at most this often.
pub const RECOVER_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// One desired container.
#[derive(Debug, Clone, PartialEq)]
pub struct Want {
    pub app_id: String,
    pub app_name: String,
    pub deployment_id: String,
    pub container: String,
    pub node_id: String,
    pub desired: Desired,
    /// Production (false) or a preview (true).
    pub preview: bool,
    /// May be redeployed elsewhere when its server is offline.
    pub failover: bool,
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

/// `2026-10-25 05:00:00 +0000 UTC` (docker ps) → age. Docker writes the
/// server's own zone (`+0530 IST`), so its name after the offset is ignored.
fn age(created: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let stamp = created.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
    let t = chrono::DateTime::parse_from_str(&stamp, "%Y-%m-%d %H:%M:%S %z").ok()?;
    (now - t.with_timezone(&chrono::Utc)).to_std().ok()
}

/// Decide what to do on one server. `wants` = its desired containers;
/// `routes` = the routes it should have (running apps only); `active` =
/// deployments in flight (their containers are the engine's business);
/// `route_keys` = base files it has.
pub fn plan(
    wants: &[Want],
    routes: &[Route],
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
            (None, Desired::Running) => {
                let r = (w.app_id.clone(), w.deployment_id.clone());
                if !p.recover.contains(&r) {
                    p.recover.push(r);
                }
            }
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
        // Databases are reconciled on their own (`Engine::reconcile_databases`).
        if c.labels.get("vercelify.kind").map(String::as_str) == Some("db") {
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
    // Stopped or asleep apps have no route here (the caller leaves them
    // out), so the wake page (or a clean 404) answers instead of a 502.
    let mut keep: HashSet<&str> = HashSet::new();
    for r in routes {
        if keep.insert(r.key.as_str()) {
            p.write_routes.push(r.clone());
        }
    }
    for k in route_keys {
        if !keep.contains(k.as_str()) {
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

/// What the database wants everywhere.
#[derive(Debug, Default)]
pub struct Desire {
    pub wants: Vec<Want>,
    /// Routes per server (node id), for running apps.
    pub routes: BTreeMap<String, Vec<Route>>,
    /// Deployments in flight.
    pub active: HashSet<String>,
    /// Route keys of apps with a deployment in flight (the engine's).
    pub busy_keys: HashSet<String>,
    pub busy_apps: HashSet<String>,
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
    pub fn desire(&self) -> Result<Desire> {
        let e = &self.engine;
        let (deps, active, main) = e.store.with(|c| {
            let settings = domain_settings(c)?;
            let main = nodes::main(c)?.id;
            let mut deps: Vec<(String, Desired, bool, RouteBase, bool)> = vec![];
            for app in apps::list(c, None)? {
                let stateless = volumes::list(c, VolumeOwner::App, &app.id)?.is_empty();
                let mut push = |d: &Deployment, preview: bool, desired: Desired| -> Result<()> {
                    let (port, _) = deployments::built_info(c, &d.id)?;
                    let (hosts, _) = hosts_for(&settings, &app, d.pr_number);
                    let base = RouteBase {
                        key: deploy::route_key(&app.name, d.pr_number),
                        hosts,
                        port: app.runtime.port.or(port).unwrap_or(3000),
                        basic_auth: apps::basic_auth(c, &app.id)?,
                        dns_challenge: false,
                    };
                    deps.push((
                        d.id.clone(),
                        desired,
                        preview,
                        base,
                        app.runtime.failover && stateless && app.kind != vercelify_core::platform::AppKind::Compose,
                    ));
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
            let active: Vec<Deployment> = deployments::active(c)?;
            Ok((deps, active, main))
        })?;
        let mut out = Desire {
            active: active.iter().map(|d| d.id.clone()).collect(),
            busy_apps: active.iter().map(|d| d.app_id.clone()).collect(),
            ..Desire::default()
        };
        for (dep, desired, preview, base, failover) in deps {
            let placed: Vec<Placed> = e.placed(&dep)?;
            let d = e.store.with(|c| deployments::get(c, &dep))?;
            if out.busy_apps.contains(&d.app_id) {
                out.busy_keys.insert(base.key.clone());
            }
            for p in &placed {
                out.wants.push(Want {
                    app_id: d.app_id.clone(),
                    app_name: base.key.clone(),
                    deployment_id: dep.clone(),
                    container: p.instance.container.clone(),
                    node_id: p.node.id.clone(),
                    desired,
                    preview,
                    failover,
                });
            }
            if desired == Desired::Running {
                for (node, r) in fleet::routes(&base, &placed, &main) {
                    out.routes.entry(node).or_default().push(r);
                }
            }
        }
        Ok(out)
    }

    pub async fn once(&self) -> Result<Report> {
        let desire = self.desire()?;
        let servers: Vec<PlatformNode> = self.engine.store.with(nodes::list)?;
        let mut r = Report::default();
        let mut recover: Vec<(String, String)> = vec![];
        for n in &servers {
            let wants: Vec<Want> = desire.wants.iter().filter(|w| w.node_id == n.id).cloned().collect();
            if n.role != "main" && !n.online {
                // Offline: its containers keep running if it is just cut
                // off; apps that may fail over move.
                for w in wants.iter().filter(|w| w.failover && w.desired == Desired::Running) {
                    let x = (w.app_id.clone(), w.deployment_id.clone());
                    if !recover.contains(&x) {
                        recover.push(x);
                    }
                }
                continue;
            }
            let rt = match self.engine.node_runtime(n) {
                Ok(rt) => rt,
                Err(e) => {
                    r.errors.push(e.to_string());
                    continue;
                }
            };
            let (actual, keys) = match (rt.list().await, rt.route_keys().await) {
                (Ok(a), Ok(k)) => (a, k),
                (Err(e), _) | (_, Err(e)) => {
                    r.errors.push(e.to_string());
                    continue;
                }
            };
            // Apps with a deployment in flight keep their routes untouched.
            let keys: Vec<String> = keys.into_iter().filter(|k| !desire.busy_keys.contains(k)).collect();
            let routes: Vec<Route> = desire
                .routes
                .get(&n.id)
                .map(|v| {
                    v.iter()
                        .filter(|r| !desire.busy_keys.contains(&r.key))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let p = plan(&wants, &routes, &actual, &desire.active, &keys, chrono::Utc::now());
            for x in &p.recover {
                if !recover.contains(x) {
                    recover.push(x.clone());
                }
            }
            let batch = Apply {
                start: p.start.clone(),
                stop: p.stop.clone(),
                remove: p.remove.clone(),
                write_routes: p.write_routes.clone(),
                remove_routes: p.remove_routes.clone(),
                ..Apply::default()
            };
            if batch == Apply::default() {
                continue;
            }
            let label = |x: &String| {
                if rt.is_local() {
                    x.clone()
                } else {
                    format!("{x}@{}", n.name)
                }
            };
            match rt.apply(&batch).await {
                Ok(done) => {
                    let failed = |x: &String| done.errors.iter().any(|e| e.contains(x.as_str()));
                    r.started.extend(p.start.iter().filter(|x| !failed(x)).map(label));
                    r.stopped.extend(p.stop.iter().filter(|x| !failed(x)).map(label));
                    r.removed.extend(p.remove.iter().filter(|x| !failed(x)).map(label));
                    r.routes_written += done.routes_written;
                    r.routes_removed += p.remove_routes.len();
                    r.errors.extend(done.errors.iter().map(|e| format!("{}: {e}", n.name)));
                }
                Err(e) => r.errors.push(e.to_string()),
            }
        }
        r.errors.extend(self.engine.reconcile_databases().await);
        for (app_id, dep) in &recover {
            if desire.busy_apps.contains(app_id) {
                continue;
            }
            {
                let mut last = self.last_recover.lock().unwrap_or_else(|e| e.into_inner());
                if last.get(app_id).is_some_and(|t| t.elapsed() < RECOVER_BACKOFF) {
                    continue;
                }
                last.insert(app_id.clone(), Instant::now());
            }
            let want = desire.wants.iter().find(|w| &w.deployment_id == dep);
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

    fn want(name: &str, desired: Desired) -> Want {
        Want {
            app_id: format!("app_{name}"),
            app_name: name.into(),
            deployment_id: format!("dpl_{name}"),
            container: format!("vfy-{name}-1"),
            node_id: "node_main".into(),
            desired,
            preview: false,
            failover: false,
        }
    }

    fn route(name: &str) -> Route {
        Route {
            key: name.into(),
            hosts: vec![RouteHost {
                host: format!("{name}.example.com"),
                https: true,
                redirect_to: None,
            }],
            upstreams: vec![format!("http://vfy-{name}-1:3000")],
            basic_auth: vec![],
            dns_challenge: false,
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
            want("crashed", Desired::Running),
            want("gone", Desired::Running),
            want("off", Desired::Stopped),
            want("asleep", Desired::Sleeping),
            want("frozen", Desired::Running),
            want("fine", Desired::Running),
            // A second replica of the gone one: one recovery, not two.
            Want {
                container: "vfy-gone-1-r1".into(),
                ..want("gone", Desired::Running)
            },
        ];
        // Running apps are routed (the caller leaves the others out); two
        // replicas share one route.
        let routes = vec![
            route("crashed"),
            route("gone"),
            route("frozen"),
            route("fine"),
            route("fine"),
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
            ContainerState {
                labels: [("vercelify.kind".to_string(), "db".to_string())].into(),
                ..c("vfy-db-pg", "running", old, "")
            },
        ];
        let active: HashSet<String> = ["dpl_inflight".to_string()].into();
        let keys = vec!["fine".into(), "off".into(), "deleted-app".into()];
        let p = plan(&wants, &routes, &actual, &active, &keys, now);
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
        let p = plan(&[], &[], &[], &HashSet::new(), &[], now);
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

    #[test]
    fn orphans_are_found_on_a_server_outside_utc() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-25T10:10:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        for created in [
            "2026-10-25 15:30:00 +0530 IST",
            "2026-10-25 03:00:00 -0700 PDT",
            "2026-10-25 12:00:00 +0200 CEST",
        ] {
            assert_eq!(age(created, now), Some(Duration::from_secs(600)), "{created}");
        }
        let stray = ContainerState {
            name: "vfy-blog-old".into(),
            labels: [(LABEL_DEPLOYMENT.to_string(), "dpl_failed".to_string())].into(),
            created: "2026-10-25 15:20:00 +0530 IST".into(),
            id: "c1".into(),
            image: "vfy/blog:old".into(),
            state: "running".into(),
        };
        assert_eq!(
            plan(&[], &[], &[stray], &HashSet::new(), &[], now).remove,
            vec!["vfy-blog-old".to_string()]
        );
    }
}

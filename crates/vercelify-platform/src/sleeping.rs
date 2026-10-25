//! Sleeping apps on the platform engine (scale to zero, design §8.1).
//!
//! Putting an app (or a preview) to sleep records it in the database first,
//! so the reconciler doesn't start it again, then stops its containers and
//! removes its route: the wake catch-all (priority 1) answers its hosts.
//! Waking reverses that, and restores the route only once the app answers,
//! so visitors see the "waking up" page rather than a 502.

use crate::deploy::{self, probe_for};
use crate::engine::{hosts_for, Engine, EngineEvent};
use crate::fleet::{Placed, RouteBase};
use crate::runtime::{Apply, HealthWait};
use crate::{apps, deployments, nodes, previews, proxy, Error, Result};
use vercelify_core::platform::{AppPatch, Desired};

/// The deployment serving an app or preview, its instances, and the local
/// compose sidecars (the routed one first).
async fn serving(engine: &Engine, app_id: &str, pr: Option<u32>) -> Result<Option<(String, Vec<Placed>, Vec<String>)>> {
    let dep = engine.store.with(|c| {
        Ok(match pr {
            None => apps::get(c, app_id)?.current_deployment_id,
            Some(n) => previews::get(c, app_id, n)?.and_then(|p| p.deployment_id),
        })
    })?;
    let Some(dep) = dep else {
        return Ok(None);
    };
    let placed = engine.placed(&dep)?;
    let mut sidecars = vec![];
    if placed.iter().all(|p| p.node.role == "main") {
        for c in engine.deployer.docker.list_managed().await? {
            if c.labels.get(deploy::LABEL_DEPLOYMENT) == Some(&dep)
                && !placed.iter().any(|p| p.instance.container == c.name)
            {
                sidecars.push(c.name);
            }
        }
    }
    Ok(Some((dep, placed, sidecars)))
}

impl Engine {
    /// Put an app (`pr` None) or one of its previews to sleep.
    pub async fn sleep(&self, app_id: &str, pr: Option<u32>) -> Result<()> {
        let app = self.store.with(|c| apps::get(c, app_id))?;
        self.store.tx(|c| match pr {
            None => apps::patch(
                c,
                app_id,
                &AppPatch {
                    desired: Some(Desired::Sleeping),
                    ..AppPatch::default()
                },
            )
            .map(|_| ()),
            Some(n) => previews::set_asleep(c, app_id, n, true),
        })?;
        let key = deploy::route_key(&app.name, pr);
        proxy::remove_route(&self.deployer.proxy, &key)?;
        if let Some((_, placed, sidecars)) = serving(self, app_id, pr).await? {
            for p in &placed {
                let rt = self.node_runtime(&p.node)?;
                rt.apply_ok(&Apply {
                    stop: vec![p.instance.container.clone()],
                    remove_routes: if rt.is_local() { vec![] } else { vec![key.clone()] },
                    ..Apply::default()
                })
                .await?;
            }
            for n in &sidecars {
                self.deployer.docker.stop(n, 20).await?;
            }
        }
        self.emit(EngineEvent::Changed);
        Ok(())
    }

    /// Wake an app or preview: start it, wait until it answers, route it.
    pub async fn wake(&self, app_id: &str, pr: Option<u32>) -> Result<()> {
        let app = self.store.with(|c| apps::get(c, app_id))?;
        let Some((dep, placed, sidecars)) = serving(self, app_id, pr).await? else {
            return Err(Error::Conflict(format!("{} has no deployment to wake", app.name)));
        };
        // Sidecars first, the routed containers last.
        for n in sidecars.iter().rev() {
            self.deployer.docker.start(n).await?;
        }
        let (port, _) = self.store.with(|c| deployments::built_info(c, &dep))?;
        let port = app.runtime.port.or(port).unwrap_or(3000);
        for p in &placed {
            let rt = self.node_runtime(&p.node)?;
            rt.apply_ok(&Apply {
                start: vec![p.instance.container.clone()],
                ..Apply::default()
            })
            .await?;
        }
        // The first one answering is enough to route visitors to it.
        if let Some(p) = placed.first() {
            self.node_runtime(&p.node)?
                .wait_healthy(&HealthWait {
                    container: p.instance.container.clone(),
                    port,
                    probe: probe_for(app.runtime.health.as_ref()),
                    timeout_secs: app.runtime.health_timeout_secs as u64,
                })
                .await?;
        }
        let (base, main_id) = self.store.tx(|c| {
            match pr {
                None => {
                    apps::patch(
                        c,
                        app_id,
                        &AppPatch {
                            desired: Some(Desired::Running),
                            ..AppPatch::default()
                        },
                    )?;
                }
                Some(n) => previews::set_asleep(c, app_id, n, false)?,
            }
            let settings = apps::domain_settings(c)?;
            let (hosts, _) = hosts_for(&settings, &app, pr);
            Ok((
                RouteBase {
                    key: deploy::route_key(&app.name, pr),
                    hosts,
                    port,
                    basic_auth: apps::basic_auth(c, &app.id)?,
                    dns_challenge: false,
                },
                nodes::main(c)?.id,
            ))
        })?;
        self.switch_routes(&base, &placed, &[], &main_id).await?;
        self.emit(EngineEvent::Changed);
        Ok(())
    }

    /// Whether a build is running (memory is tight; don't put apps to sleep).
    pub fn building(&self) -> bool {
        self.store
            .with(deployments::active)
            .map(|a| {
                a.iter()
                    .any(|d| d.status == vercelify_core::platform::DeploymentStatus::Building)
            })
            .unwrap_or(false)
    }
}

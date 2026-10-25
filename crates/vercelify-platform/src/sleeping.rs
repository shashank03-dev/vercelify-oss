//! Sleeping apps on the platform engine (scale to zero, design §8.1).
//!
//! Putting an app (or a preview) to sleep records it in the database first,
//! so the reconciler doesn't start it again, then stops its containers and
//! removes its route: the wake catch-all (priority 1) answers its hosts.
//! Waking reverses that, and restores the route only once the app answers,
//! so visitors see the "waking up" page rather than a 502.

use crate::deploy::{self, probe_for};
use crate::docker::PROXY_NETWORK;
use crate::engine::{Engine, EngineEvent};
use crate::reconcile::route_for;
use crate::{apps, deployments, health, previews, proxy, Error, Result};
use std::time::Duration;
use vercelify_core::platform::{AppPatch, Desired};

/// The containers serving an app or preview (the routed one first; compose
/// apps have more).
async fn containers_of(engine: &Engine, app_id: &str, pr: Option<u32>) -> Result<(Vec<String>, Option<String>)> {
    let dep = engine.store.with(|c| {
        Ok(match pr {
            None => apps::get(c, app_id)?.current_deployment_id,
            Some(n) => previews::get(c, app_id, n)?.and_then(|p| p.deployment_id),
        })
    })?;
    let Some(dep) = dep else {
        return Ok((vec![], None));
    };
    let routed = engine.store.with(|c| deployments::container(c, &dep))?;
    let mut names = vec![];
    if !routed.is_empty() {
        names.push(routed);
    }
    for c in engine.deployer.docker.list_managed().await? {
        if c.labels.get(deploy::LABEL_DEPLOYMENT) == Some(&dep) && !names.contains(&c.name) {
            names.push(c.name);
        }
    }
    Ok((names, Some(dep)))
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
        proxy::remove_route(&self.deployer.proxy, &deploy::route_key(&app.name, pr))?;
        let (names, _) = containers_of(self, app_id, pr).await?;
        for n in &names {
            self.deployer.docker.stop(n, 20).await?;
        }
        self.emit(EngineEvent::Changed);
        Ok(())
    }

    /// Wake an app or preview: start it, wait until it answers, route it.
    pub async fn wake(&self, app_id: &str, pr: Option<u32>) -> Result<()> {
        let app = self.store.with(|c| apps::get(c, app_id))?;
        let (names, dep) = containers_of(self, app_id, pr).await?;
        let Some(dep) = dep else {
            return Err(Error::Conflict(format!("{} has no deployment to wake", app.name)));
        };
        // Sidecars first, the routed container last.
        for n in names.iter().rev() {
            self.deployer.docker.start(n).await?;
        }
        let d = self.store.with(|c| deployments::get(c, &dep))?;
        let routed = names.first().cloned().unwrap_or_default();
        let (port, _) = self.store.with(|c| deployments::built_info(c, &dep))?;
        let port = app.runtime.port.or(port).unwrap_or(3000);
        health::wait_healthy(
            health::Wait {
                docker: &self.deployer.docker,
                container: &routed,
                network: PROXY_NETWORK,
                port,
                probe: probe_for(app.runtime.health.as_ref()),
                timeout: Duration::from_secs(app.runtime.health_timeout_secs as u64),
                interval: Duration::from_millis(250),
            },
            &mut |_| {},
        )
        .await?;
        let route = self.store.tx(|c| {
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
            route_for(c, &settings, &app, &d, &routed)
        })?;
        if let Some(r) = route {
            proxy::write_route(&self.deployer.proxy, &r)?;
        }
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

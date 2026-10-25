//! Runtime operations on an app's live containers: restart, and moving an
//! app volume between the SSD and the HDD (design §10, storage tiers).

use crate::deploy::LABEL_DEPLOYMENT;
use crate::engine::{Engine, EngineEvent, Enqueue};
use crate::proc::{self, Spec};
use crate::{apps, deployments, volumes, Error, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use vercelify_core::platform::{AppVolume, DeploymentTrigger, VolumeOwner};
use vercelify_core::StorageTier;

impl Engine {
    /// The container serving the app now (for compose apps: the routed
    /// service), or why there is none.
    pub fn live_container(&self, app_id: &str) -> Result<String> {
        self.store.with(|c| {
            let a = apps::get(c, app_id)?;
            let d = a
                .current_deployment_id
                .ok_or_else(|| Error::Conflict(format!("{} isn't deployed yet", a.name)))?;
            let name = deployments::container(c, &d)?;
            if name.is_empty() {
                return Err(Error::Conflict(format!("{} has no running container", a.name)));
            }
            Ok(name)
        })
    }

    /// Restart every container of the live deployment (a compose app's
    /// sidecars too). Returns how many were restarted.
    pub async fn restart_app(&self, app_id: &str) -> Result<usize> {
        let (dep, name) = self.store.with(|c| {
            let a = apps::get(c, app_id)?;
            Ok((a.current_deployment_id, a.name))
        })?;
        let dep = dep.ok_or_else(|| Error::Conflict(format!("{name} isn't deployed yet")))?;
        let docker = &self.deployer.docker;
        let mut names: Vec<String> = docker
            .list_managed()
            .await?
            .into_iter()
            .filter(|c| c.labels.get(LABEL_DEPLOYMENT) == Some(&dep))
            .map(|c| c.name)
            .collect();
        if names.is_empty() {
            names.push(self.live_container(app_id)?);
        }
        for n in &names {
            docker.restart(n, 20).await?;
        }
        self.emit(EngineEvent::Changed);
        Ok(names.len())
    }

    /// Move an app volume's data to the other tier: stop the app, copy with
    /// owners and permissions kept, record the new place, then recreate the
    /// container from the same image (a `recover` deployment) so it mounts
    /// the data from there. The app is down while the data is copied.
    pub async fn move_app_volume(&self, volume_id: &str, tier: StorageTier, docker_root: &Path) -> Result<AppVolume> {
        let vol = self.store.with(|c| volumes::get(c, volume_id))?;
        if vol.owner_kind != VolumeOwner::App {
            return Err(Error::Invalid("database volumes move with their database".into()));
        }
        if vol.tier == tier {
            return Ok(vol);
        }
        let app = self.store.with(|c| apps::get(c, &vol.owner_id))?;
        if self.store.with(deployments::active)?.iter().any(|d| d.app_id == app.id) {
            return Err(Error::Conflict(format!(
                "{} is deploying; move its volume when that's done",
                app.name
            )));
        }
        let docker = &self.deployer.docker;
        let name = volumes::docker_name(&app.name, &vol.name);
        let named = docker_root.join(&name).join("_data");
        let hdd = self.deployer.hdd_root.join(&name);
        let (from, to) = match tier {
            StorageTier::Hdd => (named.clone(), hdd.clone()),
            StorageTier::Ssd => (vol.host_path.clone().map(PathBuf::from).unwrap_or(hdd), named),
        };
        if tier == StorageTier::Ssd {
            let _ = proc::run(Spec {
                bin: &docker.bin,
                args: &proc::args(&["volume", "create", &name]),
                env: &[],
                cwd: None,
                timeout: Duration::from_secs(30),
            })
            .await?;
        }
        let live = app.current_deployment_id.clone();
        let container = match &live {
            Some(d) => self.store.with(|c| deployments::container(c, d))?,
            None => String::new(),
        };
        if !container.is_empty() {
            docker.stop(&container, 30).await?;
        }
        if from.is_dir() {
            if let Err(e) = crate::dbengine::copy_tree(&from, &to).await {
                // As it was: same container, same data.
                if !container.is_empty() {
                    let _ = docker.start(&container).await;
                }
                return Err(e);
            }
        } else {
            // Nothing written yet: an empty directory on the new tier.
            std::fs::create_dir_all(&to)?;
        }
        let host_path = (tier == StorageTier::Hdd).then(|| to.to_string_lossy().into_owned());
        self.store
            .with(|c| volumes::set_tier(c, &vol.id, tier, host_path.as_deref()))?;
        if let Some(dep) = live {
            if !container.is_empty() {
                docker.remove(&container).await?;
            }
            self.enqueue(
                &app.id,
                Enqueue {
                    trigger: Some(DeploymentTrigger::Recover),
                    rollback_of: Some(dep),
                    ..Enqueue::default()
                },
            )?;
        }
        self.emit(EngineEvent::Changed);
        self.store.with(|c| volumes::get(c, &vol.id))
    }
}

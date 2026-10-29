//! Runtime operations on an app's live containers: restart, and moving an
//! app volume between the SSD and the HDD (design §10, storage tiers).

use crate::deploy::LABEL_DEPLOYMENT;
use crate::engine::{Engine, EngineEvent, Enqueue};
use crate::proc::{self, Spec};
use crate::runtime::{Apply, NodeRuntime};
use crate::{apps, deployments, volumes, Error, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use vercelify_core::platform::{AppVolume, DeploymentTrigger, VolumeOwner};
use vercelify_core::StorageTier;

impl Engine {
    /// The container serving the app now (for compose apps: the routed
    /// service), or why there is none.
    pub fn live_container(&self, app_id: &str) -> Result<String> {
        Ok(self.live_instance(app_id)?.1)
    }

    /// Where the live deployment's first instance runs, and its container.
    pub fn live_instance(&self, app_id: &str) -> Result<(NodeRuntime, String)> {
        let (name, dep) = self.store.with(|c| {
            let a = apps::get(c, app_id)?;
            Ok((a.name, a.current_deployment_id))
        })?;
        let dep = dep.ok_or_else(|| Error::Conflict(format!("{name} isn't deployed yet")))?;
        let p = self
            .placed(&dep)?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Conflict(format!("{name} has no running container")))?;
        Ok((self.node_runtime(&p.node)?, p.instance.container))
    }

    /// Restart every container of the live deployment (every replica, a
    /// compose app's sidecars too). Returns how many were restarted.
    pub async fn restart_app(&self, app_id: &str) -> Result<usize> {
        let _runtime = self.app_runtime.lock().await;
        let (dep, name) = self.store.with(|c| {
            let a = apps::get(c, app_id)?;
            Ok((a.current_deployment_id, a.name))
        })?;
        let dep = dep.ok_or_else(|| Error::Conflict(format!("{name} isn't deployed yet")))?;
        let app = self.store.with(|c| apps::find(c, app_id))?;
        let _operation = self.app_operation(&app.id)?;
        let placed = self.placed(&dep)?;
        let mut n = 0;
        let mut local: Vec<String> = self
            .deployer
            .docker
            .list_managed()
            .await?
            .into_iter()
            .filter(|c| c.labels.get(LABEL_DEPLOYMENT) == Some(&dep))
            .map(|c| c.name)
            .collect();
        for p in &placed {
            let rt = self.node_runtime(&p.node)?;
            if rt.is_local() {
                if !local.contains(&p.instance.container) {
                    local.push(p.instance.container.clone());
                }
                continue;
            }
            rt.apply_ok(&Apply {
                restart: vec![p.instance.container.clone()],
                ..Apply::default()
            })
            .await?;
            n += 1;
        }
        if local.is_empty() && placed.is_empty() {
            return Err(Error::Conflict(format!("{name} has no running container")));
        }
        for c in &local {
            self.deployer.docker.restart(c, 20).await?;
        }
        self.emit(EngineEvent::Changed);
        Ok(n + local.len())
    }

    /// Move an app volume's data to the other tier: stop the app, copy with
    /// owners and permissions kept, record the new place, then recreate the
    /// container from the same image (a `recover` deployment) so it mounts
    /// the data from there. The app is down while the data is copied.
    pub async fn move_app_volume(&self, volume_id: &str, tier: StorageTier, docker_root: &Path) -> Result<AppVolume> {
        let _runtime = self.app_runtime.lock().await;
        let vol = self.store.with(|c| volumes::get(c, volume_id))?;
        if vol.owner_kind != VolumeOwner::App {
            return Err(Error::Invalid("database volumes move with their database".into()));
        }
        if vol.tier == tier {
            return Ok(vol);
        }
        let app = self.store.with(|c| apps::get(c, &vol.owner_id))?;
        let _operation = self.app_operation(&app.id)?;
        if self.store.with(deployments::active)?.iter().any(|d| d.app_id == app.id) {
            return Err(Error::Conflict(format!(
                "{} is deploying; move its volume when that's done",
                app.name
            )));
        }
        // The data lives where the app runs; the copy is done here.
        if let Some(node) = app.node_id.as_deref() {
            if !self.runtime_of(node)?.is_local() {
                return Err(Error::Invalid(format!(
                    "{} runs on another server: moving volumes between disks works for apps on the main server",
                    app.name
                )));
            }
        }
        let docker = &self.deployer.docker;
        let name = volumes::physical_name(&vol, &app.name, None);
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
        // Placement, rather than only a pin, tells us where the data actually is.
        if let Some(dep) = &live {
            for placed in self.placed(dep)? {
                if !self.node_runtime(&placed.node)?.is_local() {
                    return Err(Error::Invalid(
                        "this app runs on another server; move its volumes on that server".into(),
                    ));
                }
            }
        }
        // Every replica and compose sidecar can write to this volume.
        let containers: Vec<String> = match &live {
            Some(_) => docker
                .list_managed()
                .await?
                .into_iter()
                .filter(|c| {
                    matches!(c.state.as_str(), "running" | "restarting" | "paused")
                        && c.labels.get(crate::deploy::LABEL_APP) == Some(&app.id)
                        && !c.labels.contains_key(crate::deploy::LABEL_PR)
                })
                .map(|c| c.name)
                .collect(),
            None => vec![],
        };
        let mut stopped: Vec<&str> = vec![];
        for container in &containers {
            if let Err(e) = docker.stop(container, 30).await {
                for c in &stopped {
                    let _ = docker.start(c).await;
                }
                return Err(e);
            }
            stopped.push(container.as_str());
        }
        if let Err(e) = copy_or_init_volume(&from, &to).await {
            for c in &stopped {
                let _ = docker.start(c).await;
            }
            return Err(e);
        }
        let host_path = (tier == StorageTier::Hdd).then(|| to.to_string_lossy().into_owned());
        if let Err(e) = self
            .store
            .with(|c| volumes::set_tier(c, &vol.id, tier, host_path.as_deref()))
        {
            for c in &stopped {
                let _ = docker.start(c).await;
            }
            return Err(e);
        }
        if let Some(dep) = live {
            for c in &containers {
                if let Err(e) = docker.remove(c).await {
                    tracing::warn!("removing stopped container {c}: {e}");
                }
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

async fn copy_or_init_volume(from: &Path, to: &Path) -> Result<()> {
    match std::fs::metadata(from) {
        // An app that has never mounted the volume has no data to copy.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(to)?;
            Ok(())
        }
        // Anything else (unreadable, not a directory) is copy_tree's to refuse.
        _ => crate::dbengine::copy_tree(from, to).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn non_directory_volume_source_must_not_be_treated_as_empty() {
        let temp = tempfile::tempdir().unwrap();
        let from = temp.path().join("volume-source");
        let to = temp.path().join("volume-destination");
        std::fs::write(&from, b"must not discard").unwrap();

        assert!(copy_or_init_volume(&from, &to).await.is_err());
        assert!(!to.exists());
    }

    /// The case behind `4d6ae09`: Docker's volume directories are root's, so
    /// a lookup can fail with "permission denied", which `Path::is_dir`
    /// reported like a missing source (an empty volume moved).
    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_volume_source_must_not_be_treated_as_empty() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let locked = temp.path().join("docker-volumes");
        let from = locked.join("app_data").join("_data");
        let to = temp.path().join("volume-destination");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("upload.txt"), b"must not discard").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = matches!(std::fs::metadata(&from), Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied);
        let result = copy_or_init_volume(&from, &to).await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !denied {
            eprintln!("skipped: permissions don't apply to this user (root)");
            return;
        }
        assert!(result.is_err(), "an unreadable source was moved as empty");
        assert!(!to.exists());
    }
}

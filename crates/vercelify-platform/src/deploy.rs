//! Starting a deployment's container and switching traffic to it without
//! downtime (design §7.2). The database side (statuses, the current
//! deployment) is the engine's; this module only touches Docker and the
//! proxy files.

use crate::docker::{ContainerSpec, Docker, Mount, MANAGED, MANAGED_LABEL, PROXY_NETWORK};
use crate::health::{self, Probe};
use crate::proxy::{self, ProxyConfig, Route, RouteHost};
use crate::volumes::physical_name;
use crate::{Error, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use vercelify_core::platform::{App, AppVolume, Deployment, HealthCheck};
use vercelify_core::StorageTier;

pub const LABEL_APP: &str = "vercelify.app";
pub const LABEL_APP_NAME: &str = "vercelify.app-name";
pub const LABEL_DEPLOYMENT: &str = "vercelify.deployment";
pub const LABEL_KIND: &str = "vercelify.kind";
pub const LABEL_PROJECT: &str = "vercelify.project";
pub const LABEL_PR: &str = "vercelify.pr";

#[derive(Debug, Clone)]
pub struct Deployer {
    pub docker: Docker,
    pub proxy: ProxyConfig,
    /// HDD volumes live under here (`/mnt/storage/volumes`).
    pub hdd_root: PathBuf,
    /// Memory-capped slice for apps (`vercelify.slice`), if the host has it.
    pub cgroup_parent: Option<String>,
    pub health_interval: Duration,
}

pub fn project_network(project_id: &str) -> String {
    format!("vfy-net-{}", project_id.to_ascii_lowercase().replace('_', "-"))
}

fn suffix(deployment_id: &str) -> String {
    let s = deployment_id.trim_start_matches("dpl_").to_ascii_lowercase();
    s[s.len().saturating_sub(8)..].to_string()
}

pub fn container_name(app: &str, pr: Option<u32>, deployment_id: &str) -> String {
    match pr {
        Some(n) => format!("vfy-{app}-pr{n}-{}", suffix(deployment_id)),
        None => format!("vfy-{app}-{}", suffix(deployment_id)),
    }
}

/// Replica `n` of a deployment: the first keeps the plain name.
pub fn instance_name(app: &str, pr: Option<u32>, deployment_id: &str, replica: u8) -> String {
    let base = container_name(app, pr, deployment_id);
    if replica == 0 {
        base
    } else {
        format!("{base}-r{replica}")
    }
}

/// The proxy key of an app or preview (file `base-<key>.yaml`).
pub fn route_key(app: &str, pr: Option<u32>) -> String {
    match pr {
        Some(n) => format!("{app}-pr-{n}"),
        None => app.to_string(),
    }
}

/// `pr-<n>-<app>.<wildcard>`: one label deep, so `*.<wildcard>` covers it.
pub fn preview_host(app: &str, pr: u32, wildcard: &str) -> String {
    format!("pr-{pr}-{app}.{wildcard}")
}

pub fn probe_for(h: Option<&HealthCheck>) -> Probe {
    match h {
        Some(HealthCheck { command: Some(c), .. }) if !c.is_empty() => Probe::Command(c.clone()),
        Some(HealthCheck { path: Some(p), .. }) => Probe::Http { path: p.clone() },
        _ => Probe::Tcp,
    }
}

pub struct DeployInput<'a> {
    pub app: &'a App,
    pub deployment: &'a Deployment,
    pub image: &'a str,
    /// Runtime variables, system variables included.
    pub env: Vec<(String, String)>,
    pub volumes: &'a [AppVolume],
    pub port: u16,
    /// Hosts routed to this deployment (none: start it, route nothing).
    pub hosts: Vec<RouteHost>,
    pub basic_auth: Vec<String>,
    pub dns_challenge: bool,
    /// Replica number (0 = the first).
    pub replica: u8,
}

impl Deployer {
    fn mounts(&self, app: &App, pr: Option<u32>, volumes: &[AppVolume]) -> Result<Vec<Mount>> {
        let mut out = vec![];
        for v in volumes {
            // Previews never touch production data: they get volumes of their own.
            let name = physical_name(v, &app.name, pr);
            let source = match (v.tier, &v.host_path, pr) {
                (StorageTier::Hdd, Some(p), None) => p.clone(),
                (StorageTier::Hdd, _, _) => {
                    let p = self.hdd_root.join(&name);
                    std::fs::create_dir_all(&p).map_err(|e| {
                        Error::Unavailable(format!("can't create {} on the storage disk: {e}", p.display()))
                    })?;
                    p.to_string_lossy().into_owned()
                }
                (StorageTier::Ssd, _, _) => name,
            };
            out.push(Mount {
                source,
                target: v.mount_path.clone(),
                read_only: false,
            });
        }
        Ok(out)
    }

    /// Start the new container and wait for it to be healthy. On failure the
    /// new container is removed and the error carries its last log lines.
    pub async fn start(&self, i: &DeployInput<'_>, log: &mut (dyn FnMut(&str) + Send)) -> Result<String> {
        let pr = i.deployment.pr_number;
        let name = instance_name(&i.app.name, pr, &i.deployment.id, i.replica);
        let net = project_network(&i.app.project_id);
        self.docker.ensure_network(&net).await?;
        self.docker.ensure_network(PROXY_NETWORK).await?;
        if !self.docker.image_exists(i.image).await? {
            log(&format!("pulling {}", i.image));
            self.docker.pull(i.image).await?;
        }
        let mut labels = BTreeMap::new();
        labels.insert(MANAGED_LABEL.to_string(), MANAGED.to_string());
        labels.insert(LABEL_APP.to_string(), i.app.id.clone());
        labels.insert(LABEL_APP_NAME.to_string(), i.app.name.clone());
        labels.insert(LABEL_DEPLOYMENT.to_string(), i.deployment.id.clone());
        labels.insert(LABEL_PROJECT.to_string(), i.app.project_id.clone());
        labels.insert(
            LABEL_KIND.to_string(),
            if pr.is_some() { "preview" } else { "app" }.to_string(),
        );
        if let Some(n) = pr {
            labels.insert(LABEL_PR.to_string(), n.to_string());
        }
        let spec = ContainerSpec {
            name: name.clone(),
            image: i.image.to_string(),
            labels,
            env: i.env.clone(),
            networks: vec![net, PROXY_NETWORK.into()],
            // Other apps of the project reach production as `http://<name>:<port>`.
            aliases: if pr.is_none() { vec![i.app.name.clone()] } else { vec![] },
            memory_mb: i.app.runtime.memory_mb,
            cpus: i.app.runtime.cpus,
            command: i.app.runtime.command.clone(),
            mounts: self.mounts(i.app, pr, i.volumes)?,
            cgroup_parent: self.cgroup_parent.clone(),
            restart: "unless-stopped".into(),
            ..ContainerSpec::default()
        };
        self.own_volumes(i.image, &spec.mounts, log).await;
        // A leftover from an interrupted attempt of this same deployment.
        self.docker.remove(&name).await?;
        self.docker.create(&spec).await?;
        self.docker.start(&name).await?;
        log(&format!("started {name}, waiting for it to be healthy"));
        let res = health::wait_healthy(
            health::Wait {
                docker: &self.docker,
                container: &name,
                network: PROXY_NETWORK,
                port: i.port,
                probe: probe_for(i.app.runtime.health.as_ref()),
                timeout: Duration::from_secs(i.app.runtime.health_timeout_secs as u64),
                interval: self.health_interval,
            },
            log,
        )
        .await;
        if let Err(e) = res {
            let tail = self.docker.logs(&name, 50).await.unwrap_or_default();
            let _ = self.docker.remove(&name).await;
            let tail = tail.trim();
            return Err(Error::Invalid(if tail.is_empty() {
                e.to_string()
            } else {
                format!("{e}\n--- last lines of the app's log ---\n{tail}")
            }));
        }
        Ok(name)
    }

    /// Images that run as a non-root user (ours do) can't write to a new
    /// volume, whose top directory belongs to root: hand it to that user,
    /// resolved in the image itself (`node`, `1000:1000`…). Only the top
    /// directory changes; a failure is logged, not fatal.
    async fn own_volumes(&self, image: &str, mounts: &[Mount], log: &mut (dyn FnMut(&str) + Send)) {
        if mounts.is_empty() {
            return;
        }
        let user = match self.docker.image_user(image).await {
            Ok(u) => u,
            Err(_) => return,
        };
        if !needs_chown(&user) {
            return;
        }
        for m in mounts.iter().filter(|m| !m.read_only) {
            let out = self
                .docker
                .run(
                    &crate::proc::args(&[
                        "run",
                        "--rm",
                        "--network",
                        "none",
                        "--user",
                        "0:0",
                        "--entrypoint",
                        "chown",
                        "-v",
                        &format!("{}:/vfy-volume", m.source),
                        image,
                        &user,
                        "/vfy-volume",
                    ]),
                    Duration::from_secs(120),
                )
                .await;
            match out {
                Ok(o) if o.success() => {}
                Ok(o) => log(&format!("could not give {} to {user}: {}", m.target, o.tail(1))),
                Err(e) => log(&format!("could not give {} to {user}: {e}", m.target)),
            }
        }
    }

    /// The commit point: route traffic to `container`.
    pub fn switch(&self, i: &DeployInput<'_>, container: &str) -> Result<bool> {
        let key = route_key(&i.app.name, i.deployment.pr_number);
        if i.hosts.is_empty() {
            proxy::remove_route(&self.proxy, &key)?;
            return Ok(false);
        }
        let upstreams = vec![format!("http://{container}:{}", i.port)];
        proxy::write_route(
            &self.proxy,
            &Route {
                key,
                hosts: i.hosts.clone(),
                upstreams,
                basic_auth: i.basic_auth.clone(),
                dns_challenge: i.dns_challenge,
            },
        )
    }

    /// Stop and remove an old container after it drained.
    pub async fn retire(&self, container: &str, drain: Duration) {
        tokio::time::sleep(drain).await;
        if let Err(e) = self.docker.stop(container, 20).await {
            tracing::warn!("stopping {container}: {e}");
        }
        if let Err(e) = self.docker.remove(container).await {
            tracing::warn!("removing {container}: {e}");
        }
    }

    /// Keep the 5 newest images of an app plus the ones in `keep` (built
    /// here as `vfy/<app>`, pulled on a worker from the mesh registry).
    pub async fn prune_images(&self, app: &str, keep: &[String]) -> Result<usize> {
        let mut removed = 0;
        for repo in [format!("vfy/{app}"), crate::registry::reference(&format!("vfy/{app}"))] {
            let images = self.docker.images(&repo).await?;
            for (i, (tag, _)) in images.iter().enumerate() {
                let full = format!("{repo}:{tag}");
                if i < 5 || keep.contains(&full) {
                    continue;
                }
                self.docker.remove_image(&full).await?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// Whether the image's user is someone other than root.
pub fn needs_chown(user: &str) -> bool {
    let u = user.split(':').next().unwrap_or("").trim();
    !(u.is_empty() || u == "root" || u == "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_needs_a_chown() {
        for root in ["", "root", "0", "0:0", "root:root"] {
            assert!(!needs_chown(root), "{root}");
        }
        for user in ["node", "1000", "1000:1000", "app:app"] {
            assert!(needs_chown(user), "{user}");
        }
    }

    #[test]
    fn names() {
        assert_eq!(
            container_name("blog", None, "dpl_7k2m9q4w1x8z3c5v"),
            "vfy-blog-1x8z3c5v"
        );
        assert_eq!(
            container_name("blog", Some(7), "dpl_7k2m9q4w1x8z3c5v"),
            "vfy-blog-pr7-1x8z3c5v"
        );
        assert_eq!(route_key("blog", Some(7)), "blog-pr-7");
        assert_eq!(
            instance_name("blog", None, "dpl_7k2m9q4w1x8z3c5v", 2),
            "vfy-blog-1x8z3c5v-r2"
        );
        assert_eq!(preview_host("blog", 7, "example.com"), "pr-7-blog.example.com");
        assert_eq!(project_network("prj_ABC"), "vfy-net-prj-abc");
        assert_eq!(probe_for(None), Probe::Tcp);
        assert_eq!(
            probe_for(Some(&HealthCheck {
                path: Some("/healthz".into()),
                command: None
            })),
            Probe::Http {
                path: "/healthz".into()
            }
        );
    }
}

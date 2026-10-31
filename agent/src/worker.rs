//! A worker on the platform engine (design §8.2): it keeps no platform
//! state. It runs what the main server asks through `/v1/runtime/*`
//! (runtime_api.rs), sends the main server a heartbeat every minute over
//! the mesh (its resources, condition, and the token to call it with), runs
//! its own proxy for the apps placed here, and forwards 127.0.0.1:5055 to
//! the main server's image registry.

use crate::config::Config;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use vercelify_core::platform::{NodeHeartbeat, NodeHeartbeatReply, NodeStatus};
use vercelify_mesh::addr::{Addressing, COORDINATOR_INDEX};
use vercelify_mesh::state::State as NodeState;
use vercelify_platform::deploy::Deployer;
use vercelify_platform::docker::Docker;
use vercelify_platform::registry;

pub struct Worker {
    pub deployer: Deployer,
    token: String,
    token_hash: [u8; 32],
}

fn sha(s: &str) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(s.as_bytes()).into()
}

/// The mesh addresses the main server calls from.
pub fn coordinator_ips() -> [IpAddr; 2] {
    let a = Addressing::default();
    [
        IpAddr::V4(a.ipv4(COORDINATOR_INDEX)),
        IpAddr::V6(a.ipv6(COORDINATOR_INDEX)),
    ]
}

/// What this server reports about itself (heartbeats, and the main
/// server's own row).
pub async fn node_status(cfg: &Config, collector: &vercelify_core::collect::Collector) -> NodeStatus {
    let pg = std::fs::read_to_string(&cfg.power_guard_state)
        .ok()
        .and_then(|r| serde_json::from_str::<vercelify_core::PowerGuardState>(&r).ok());
    let (cpu, mem, power) = {
        let (cpu, mem) = collector.cpu_and_memory();
        (cpu, mem, collector.power())
    };
    let docker_ok = tokio::process::Command::new(&cfg.docker_bin)
        .args(["info", "--format", "{{.ServerVersion}}"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success());
    NodeStatus {
        cpus: cpu.cores,
        memory_total_mb: mem.total_bytes / 1_048_576,
        memory_available_mb: mem.available_bytes / 1_048_576,
        on_battery: power.on_battery,
        battery_percent: power.lowest_capacity_pct,
        thermal_paused: pg.is_some_and(|p| p.state.contains("thermal")),
        docker_ok,
        http_port: cfg.proxy_http_port,
        agent_version: crate::VERSION.into(),
    }
}

impl Worker {
    /// On a worker of the platform engine.
    pub fn open(cfg: &Config) -> Option<Arc<Self>> {
        if cfg.role() != "worker" || cfg.engine != "platform" {
            return None;
        }
        let path = cfg.state_dir.join("runtime.token");
        let token = match std::fs::read_to_string(&path).map(|t| t.trim().to_string()) {
            Ok(t) if t.len() >= 32 => t,
            _ => {
                let t = vercelify_platform::secrets::random_token(32);
                if let Err(e) = vercelify_mesh::state::write_private(&path, t.as_bytes()) {
                    tracing::error!("worker: can't save {}: {e}", path.display());
                }
                t
            }
        };
        let deployer = Deployer {
            docker: Docker {
                bin: cfg.docker_bin.clone(),
            },
            proxy: crate::platform::proxy_config(cfg),
            hdd_root: cfg.hdd_volume_root.clone(),
            cgroup_parent: cfg
                .systemd_dir
                .join("vercelify.slice")
                .exists()
                .then(|| "vercelify.slice".to_string()),
            health_interval: Duration::from_secs(2),
        };
        tracing::info!("worker: runs what the main server places here");
        Some(Arc::new(Self {
            deployer,
            token_hash: sha(&token),
            token,
        }))
    }

    /// Whether a request comes from the main server with our token.
    pub fn verify(&self, presented: &str, from: Option<IpAddr>) -> bool {
        use subtle::ConstantTimeEq;
        let ok: bool = sha(presented).ct_eq(&self.token_hash).into();
        ok && from.is_some_and(|ip| coordinator_ips().contains(&ip) || ip.is_loopback())
    }

    /// The proxy, the heartbeat and the registry forwarder.
    pub fn spawn(self: &Arc<Self>, cfg: Arc<Config>, collector: Arc<vercelify_core::collect::Collector>) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut wait = Duration::from_secs(5);
            loop {
                match vercelify_platform::proxy::ensure(&me.deployer.docker, &me.deployer.proxy, vec![]).await {
                    Ok(_) => break,
                    Err(e) => {
                        tracing::warn!("worker proxy: {e}; retrying in {}s", wait.as_secs());
                        tokio::time::sleep(wait).await;
                        wait = (wait * 2).min(Duration::from_secs(300));
                    }
                }
            }
        });
        let me = self.clone();
        let c = cfg.clone();
        tokio::spawn(async move {
            let mut failing = 0u32;
            loop {
                // Every minute; sooner after a failure (the main server
                // restarting, the mesh coming up), quietly at first.
                let next = match me.heartbeat(&c, &collector).await {
                    Ok(()) => {
                        failing = 0;
                        60
                    }
                    Err(e) => {
                        failing += 1;
                        if failing > 1 {
                            tracing::warn!("worker heartbeat: {e}");
                        }
                        10
                    }
                };
                tokio::time::sleep(Duration::from_secs(next)).await;
            }
        });
        tokio::spawn(forward_registry(cfg));
    }

    async fn heartbeat(&self, cfg: &Config, collector: &vercelify_core::collect::Collector) -> Result<(), String> {
        let st = NodeState::load(&cfg.mesh_dir)
            .map_err(|e| e.to_string())?
            .ok_or("not on the mesh yet (sudo vercelify mesh join …)")?;
        let status = node_status(cfg, collector).await;
        let hb = NodeHeartbeat {
            arch: std::env::consts::ARCH.into(),
            agent_port: cfg.port,
            runtime_token: self.token.clone(),
            status,
        };
        let url = format!("{}/v1/platform/nodes/heartbeat", st.api_url.trim_end_matches('/'));
        let body = serde_json::to_value(&hb).map_err(|e| e.to_string())?;
        let (code, bytes) = crate::http::call(
            crate::http::Req::post(&url, &body)
                .bearer(&st.node_token)
                .timeout(Duration::from_secs(20)),
        )
        .await
        .map_err(|e| e.to_string())?;
        if !code.is_success() {
            return Err(crate::http::error_text(code, &bytes));
        }
        let reply: NodeHeartbeatReply = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        tracing::debug!("heartbeat: {} (registry {})", reply.node_id, reply.registry);
        Ok(())
    }
}

/// 127.0.0.1:5055 → the main server's registry over the mesh, so Docker
/// here pulls `127.0.0.1:5055/vfy/…` (a loopback registry needs no TLS).
async fn forward_registry(cfg: Arc<Config>) {
    let listen = registry::LOCAL;
    let listener = loop {
        match tokio::net::TcpListener::bind(listen).await {
            Ok(l) => break l,
            Err(e) => {
                tracing::warn!("registry forwarder: bind {listen}: {e}; retrying in 30s");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    };
    loop {
        let Ok((mut inbound, _)) = listener.accept().await else {
            continue;
        };
        let cfg = cfg.clone();
        tokio::spawn(async move {
            let target = NodeState::load(&cfg.mesh_dir).ok().flatten().and_then(|s| {
                let host = s.api_url.strip_prefix("http://")?.rsplit_once(':')?.0.to_string();
                Some(format!("{host}:{}", registry::PORT))
            });
            let Some(target) = target else { return };
            match tokio::net::TcpStream::connect(&target).await {
                Ok(mut out) => {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut out).await;
                }
                Err(e) => tracing::warn!("registry forwarder: {target}: {e}"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_main_server_with_the_token() {
        let w = Worker {
            deployer: Deployer {
                docker: Docker::default(),
                proxy: crate::platform::proxy_config(&Config::default()),
                hdd_root: "/tmp".into(),
                cgroup_parent: None,
                health_interval: Duration::from_secs(1),
            },
            token: "a".repeat(43),
            token_hash: sha(&"a".repeat(43)),
        };
        let main: IpAddr = "10.77.0.1".parse().unwrap();
        assert!(w.verify(&"a".repeat(43), Some(main)));
        assert!(w.verify(&"a".repeat(43), Some("fd76:6679:6d65::1".parse().unwrap())));
        assert!(
            !w.verify(&"a".repeat(43), Some("10.77.0.9".parse().unwrap())),
            "another node"
        );
        assert!(!w.verify("b", Some(main)));
        assert!(!w.verify(&"a".repeat(43), None));
    }
}

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
use vercelify_core::host::HostStatus;
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
    let mut own = NodeStatus {
        capacity: None,
        cpus: cpu.cores,
        memory_total_mb: mem.total_bytes / 1_048_576,
        memory_available_mb: mem.available_bytes / 1_048_576,
        on_battery: power.on_battery,
        battery_percent: power.lowest_capacity_pct,
        thermal_paused: pg.is_some_and(|p| vercelify_platform::gate::holds_builds(&p.state)),
        docker_ok,
        http_port: cfg.proxy_http_port,
        agent_version: crate::VERSION.into(),
    };
    own.capacity = crate::capacity::collect(cfg, &own).await;
    match &cfg.host_status {
        Some(src) => with_host(
            own,
            read_host_status(src).await.as_ref(),
            chrono::Utc::now().timestamp(),
        ),
        None => own,
    }
}

/// The host manager's view of the physical computer (see `Config::host_status`).
async fn read_host_status(src: &str) -> Option<HostStatus> {
    let raw: Vec<u8> = if src.starts_with("http://") {
        let (code, body) = crate::http::call(crate::http::Req::get(src).timeout(Duration::from_secs(3)))
            .await
            .ok()?;
        if !code.is_success() {
            return None;
        }
        body.to_vec()
    } else {
        tokio::fs::read(src).await.ok()?
    };
    serde_json::from_slice(&raw).ok()
}

/// Inside a VM the battery and thermal readings are of virtual hardware:
/// the computer's own fresh ones replace them. Observed guest memory stays
/// unchanged; the accepted grant and actual physical availability additionally
/// bound admission. Missing host-manager data defers new work.
fn with_host(mut own: NodeStatus, host: Option<&HostStatus>, now: i64) -> NodeStatus {
    match host.filter(|h| h.fresh(now)) {
        Some(h) => {
            own.on_battery = h.on_battery;
            own.battery_percent = h.battery_percent;
            own.thermal_paused |= h.thermal_pressure;
            if let Some(capacity) = &mut own.capacity {
                capacity.host_ready = h.going_down.is_none()
                    && h.memory_available_bytes.is_some()
                    && h.environment_memory_bytes.is_some_and(|m| m > 0)
                    && h.provider_overhead_bytes.is_some();
                capacity.host_grant_mb = h.environment_memory_bytes.map(|b| b / 1_048_576);
                capacity.host_available_mb = h.memory_available_bytes.map(|b| b / 1_048_576);
                capacity.provider_overhead_mb = h.provider_overhead_bytes.map(|b| b.div_ceil(1_048_576));
                if let Some(grant) = capacity.host_grant_mb {
                    capacity.workload_limit_mb = capacity
                        .workload_limit_mb
                        .min(grant.saturating_sub(vercelify_platform::capacity::os_reserve(grant)));
                }
                capacity.disk_free_mb = capacity
                    .disk_free_mb
                    .zip(h.disk_free_bytes)
                    .map(|(guest, physical)| guest.min(physical / 1_048_576));
            }
        }
        None => {
            if let Some(capacity) = &mut own.capacity {
                capacity.host_ready = false;
            }
            tracing::debug!("no fresh report from the host manager");
        }
    }
    own
}

#[cfg(test)]
mod host_tests {
    use super::*;

    #[test]
    fn the_real_computers_battery_and_heat_win() {
        let own = NodeStatus {
            cpus: 4,
            memory_available_mb: 3000,
            ..NodeStatus::default()
        };
        let host = HostStatus {
            measured_at: 1_000,
            on_battery: true,
            battery_percent: Some(40),
            thermal_pressure: true,
            ..HostStatus::default()
        };
        let s = with_host(own.clone(), Some(&host), 1_010);
        assert!(s.on_battery && s.thermal_paused);
        assert_eq!(s.battery_percent, Some(40));
        assert_eq!(s.memory_available_mb, 3000, "the guest's memory stays");
        // Placement now refuses it: on battery below 50 %.
        let node = vercelify_core::platform::PlatformNode {
            id: "node_1".into(),
            name: "mac".into(),
            role: "worker".into(),
            agent_url: String::new(),
            arch: "aarch64".into(),
            builds: false,
            online: true,
            last_seen_at: None,
            created_at: String::new(),
            mesh_ip: String::new(),
            drained: false,
            status: NodeStatus { docker_ok: true, ..s },
            instances: 0,
        };
        assert!(vercelify_platform::placement::unusable(&node).is_some());

        assert_eq!(with_host(own.clone(), Some(&host), 2_000), own, "stale: ignored");
        assert_eq!(with_host(own.clone(), None, 1_010), own);
    }

    fn capacity_node() -> NodeStatus {
        NodeStatus {
            memory_total_mb: 2048,
            memory_available_mb: 1024,
            capacity: Some(vercelify_core::platform::CapacitySample {
                epoch: "boot".into(),
                measured_at: 100,
                total_mb: 2048,
                available_mb: 1024,
                workload_limit_mb: 1664,
                host_ready: true,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn admission_cannot_use_guest_ram_when_the_host_report_is_missing_or_stale() {
        let own = capacity_node();
        assert!(!with_host(own.clone(), None, 100).capacity.unwrap().host_ready);
        let old = HostStatus {
            measured_at: 1,
            ..Default::default()
        };
        assert!(!with_host(own, Some(&old), 100).capacity.unwrap().host_ready);
    }

    #[test]
    fn accepted_grant_and_physical_availability_bound_guest_admission() {
        let host = HostStatus {
            measured_at: 100,
            memory_available_bytes: Some(512 * 1_048_576),
            environment_memory_bytes: Some(1024 * 1_048_576),
            provider_overhead_bytes: Some(256 * 1_048_576),
            ..Default::default()
        };
        let s = with_host(capacity_node(), Some(&host), 100).capacity.unwrap();
        assert!(s.host_ready);
        assert_eq!(s.available_mb, 1024, "observed guest availability is not rewritten");
        assert_eq!(s.host_available_mb, Some(512));
        assert_eq!(s.workload_limit_mb, 640);
        assert_eq!(s.provider_overhead_mb, Some(256));
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
                    // Refused for good (removed from hosting): ask rarely.
                    Err(Beat::Refused(e)) => {
                        if failing == 0 {
                            tracing::warn!("worker heartbeat: {e}");
                        }
                        failing = 1;
                        600
                    }
                    Err(Beat::Failed(e)) => {
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

    async fn heartbeat(&self, cfg: &Config, collector: &vercelify_core::collect::Collector) -> Result<(), Beat> {
        let st = NodeState::load(&cfg.mesh_dir)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "not on the mesh yet (sudo vercelify mesh join …)".to_string())?;
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
        if code == hyper::StatusCode::CONFLICT {
            return Err(Beat::Refused(crate::http::error_text(code, &bytes)));
        }
        if !code.is_success() {
            return Err(crate::http::error_text(code, &bytes).into());
        }
        let reply: NodeHeartbeatReply = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        tracing::debug!("heartbeat: {} (registry {})", reply.node_id, reply.registry);
        Ok(())
    }
}

/// Why a heartbeat didn't land.
enum Beat {
    /// Try again soon (the main server restarting, the mesh coming up).
    Failed(String),
    /// The main server said no for good (this server was removed).
    Refused(String),
}

impl From<String> for Beat {
    fn from(e: String) -> Self {
        Beat::Failed(e)
    }
}

/// 127.0.0.1:5055 → the main server's registry over the mesh, so Docker
/// here pulls `127.0.0.1:5055/vfy/…` (a loopback registry needs no TLS).
async fn forward_registry(cfg: Arc<Config>) {
    loop {
        match tokio::net::TcpListener::bind(registry::LOCAL).await {
            Ok(listener) => {
                let router = axum::Router::new()
                    .route("/{*path}", axum::routing::get(crate::registry_proxy::worker_read))
                    .with_state(cfg.clone());
                if let Err(e) = axum::serve(listener, router).await {
                    tracing::warn!("registry forwarder: {e}");
                }
            }
            Err(e) => tracing::warn!("registry forwarder: bind {}: {e}", registry::LOCAL),
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
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

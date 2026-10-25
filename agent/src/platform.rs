//! The agent's handle on Vercelify's own platform engine (design doc §2):
//! the state database, the master key, the deployment engine and the
//! reconciler, opened once at start on a main server whose `engine` is
//! `platform`.

use crate::config::Config;
use std::sync::Arc;
use std::time::Duration;
use vercelify_platform::build::{Builder, Driver};
use vercelify_platform::deploy::Deployer;
use vercelify_platform::docker::Docker;
use vercelify_platform::engine::Engine;
use vercelify_platform::gate::BuildGate;
use vercelify_platform::logs::LogHub;
use vercelify_platform::proxy::{ProxyConfig, TRAEFIK_IMAGE};
use vercelify_platform::reconcile::Reconciler;
use vercelify_platform::secrets::MasterKey;
use vercelify_platform::Store;

pub struct Platform {
    pub store: Arc<Store>,
    pub key: Arc<MasterKey>,
    pub engine: Arc<Engine>,
    pub reconciler: Arc<Reconciler>,
    pub proxy: ProxyConfig,
}

/// What the doctor reports when the platform couldn't be opened.
#[derive(Debug, Clone, Default)]
pub struct OpenError(pub String);

pub fn proxy_config(cfg: &Config) -> ProxyConfig {
    ProxyConfig {
        dir: cfg.proxy_dir.clone(),
        http_port: cfg.proxy_http_port,
        https_port: cfg.proxy_https_port,
        metrics_port: cfg.proxy_metrics_port,
        acme_email: cfg.acme_email.clone(),
        dns_challenge: false,
        image: TRAEFIK_IMAGE.into(),
    }
}

impl Platform {
    pub fn open(cfg: &Config) -> Result<Self, OpenError> {
        let key = Arc::new(
            MasterKey::load_or_create(&cfg.master_key_file).map_err(|e| OpenError(format!("master key: {e}")))?,
        );
        let store = Arc::new(Store::open(&cfg.platform_db).map_err(|e| OpenError(e.to_string()))?);
        let docker = Docker {
            bin: cfg.docker_bin.clone(),
        };
        let proxy = proxy_config(cfg);
        let builder = Builder {
            docker: cfg.docker_bin.clone(),
            git: cfg.git_bin.clone(),
            work_dir: cfg.state_dir.join("builds"),
            driver: Driver::parse(&cfg.build_driver),
            memory: cfg.build_memory.clone(),
            timeout: Duration::from_secs(45 * 60),
        };
        let deployer = Deployer {
            docker,
            proxy: proxy.clone(),
            hdd_root: cfg.hdd_volume_root.clone(),
            // The installer's memory-capped slice for apps, when present.
            cgroup_parent: cfg
                .systemd_dir
                .join("vercelify.slice")
                .exists()
                .then(|| "vercelify.slice".to_string()),
            health_interval: Duration::from_secs(2),
        };
        let gate = BuildGate::new(cfg.build_concurrency.max(1) as usize, cfg.power_guard_state.clone());
        let logs = LogHub::new(cfg.state_dir.join("logs"));
        let engine = Engine::new(store.clone(), key.clone(), builder, deployer, gate, logs);
        let reconciler = Reconciler::new(engine.clone());
        Ok(Self {
            store,
            key,
            engine,
            reconciler,
            proxy,
        })
    }

    /// Bookkeeping at agent start: deployments that were mid-flight when the
    /// agent stopped failed; this server is registered as the main node.
    pub fn on_start(&self, cfg: &Config, host: &vercelify_core::HostInfo) {
        let url = format!("http://127.0.0.1:{}", cfg.port);
        let res = self.store.tx(|c| {
            let n = vercelify_platform::deployments::fail_interrupted(c)?;
            vercelify_platform::nodes::ensure_main(c, &host.hostname, &url, &host.arch)?;
            Ok(n)
        });
        match res {
            Ok(0) => {}
            Ok(n) => tracing::warn!("platform: {n} deployment(s) were interrupted by the restart"),
            Err(e) => tracing::error!("platform: start-up bookkeeping failed: {e}"),
        }
    }

    /// Start the proxy, the deployment queue and the reconciler.
    pub fn spawn(self: &Arc<Self>, reconcile_every: Duration) {
        let me = self.clone();
        tokio::spawn(async move {
            // The proxy first: deployments write routes for it. Docker may
            // still be starting at boot, so retry.
            let mut wait = Duration::from_secs(5);
            loop {
                match vercelify_platform::proxy::ensure(&me.engine.deployer.docker, &me.proxy, vec![]).await {
                    Ok(_) => break,
                    Err(e) => {
                        tracing::warn!("platform proxy: {e}; retrying in {}s", wait.as_secs());
                        tokio::time::sleep(wait).await;
                        wait = (wait * 2).min(Duration::from_secs(300));
                    }
                }
            }
            tokio::spawn(me.engine.clone().run());
            // A consistent copy for restic (and the warm standby): backups
            // never read the live database file.
            let store = me.store.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(300)).await;
                    let s = store.clone();
                    let res = tokio::task::spawn_blocking(move || {
                        let path = s.path().map(vercelify_platform::store::backup_path);
                        match path {
                            Some(p) => s.backup_to(&p),
                            None => Ok(()),
                        }
                    })
                    .await;
                    if let Ok(Err(e)) = res {
                        tracing::warn!("platform database copy: {e}");
                    }
                }
            });
            if !reconcile_every.is_zero() {
                me.reconciler.clone().run(reconcile_every).await;
            }
        });
    }

    /// Open when this server runs the platform engine. A failure is logged and
    /// kept for the doctor; everything that doesn't need the platform keeps
    /// working.
    pub fn open_if_enabled(cfg: &Config) -> (Option<Arc<Platform>>, Option<OpenError>) {
        if !cfg.platform_engine() {
            return (None, None);
        }
        // A promoted standby took this server's place (design §8.4).
        if let Some(m) = crate::standby::superseded(cfg) {
            tracing::error!("{m}");
            return (None, Some(OpenError(m)));
        }
        match Self::open(cfg) {
            Ok(p) => {
                tracing::info!("platform engine: {}", cfg.platform_db.display());
                (Some(Arc::new(p)), None)
            }
            Err(e) => {
                tracing::error!("platform engine unavailable: {}", e.0);
                (None, Some(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &std::path::Path, engine: &str) -> Config {
        let mut c = Config {
            engine: engine.into(),
            role: Some("main".into()),
            state_dir: dir.into(),
            platform_db: dir.join("platform.db"),
            master_key_file: dir.join("master.key"),
            ..Config::default()
        };
        c.apply_engine_defaults();
        c
    }

    #[test]
    fn opens_only_for_the_platform_engine() {
        let dir = tempfile::tempdir().unwrap();
        let (p, e) = Platform::open_if_enabled(&cfg(dir.path(), "coolify"));
        assert!(p.is_none() && e.is_none());
        assert!(!dir.path().join("platform.db").exists());

        let c = cfg(dir.path(), "platform");
        assert_eq!(c.proxy_dir, dir.path().join("proxy"), "our proxy, not Coolify's");
        assert_eq!(c.proxy_container, "vercelify-proxy");
        assert_eq!(c.traefik_metrics_url, "http://127.0.0.1:8082/metrics");
        let (p, e) = Platform::open_if_enabled(&c);
        assert!(e.is_none(), "{e:?}");
        let p = p.unwrap();
        assert_eq!(
            p.store.schema_version().unwrap(),
            vercelify_platform::store::latest_version()
        );
        assert!(dir.path().join("master.key").exists());

        let mut worker = cfg(dir.path(), "platform");
        worker.role = Some("worker".into());
        assert!(
            Platform::open_if_enabled(&worker).0.is_none(),
            "workers keep no platform state"
        );
    }

    #[test]
    fn a_broken_key_is_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("master.key"), b"short").unwrap();
        let (p, e) = Platform::open_if_enabled(&cfg(dir.path(), "platform"));
        assert!(p.is_none());
        assert!(e.unwrap().0.contains("master key"));
    }
}

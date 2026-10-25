//! The agent's handle on Vercelify's own platform engine (design doc §2):
//! the state database and the master key, opened once at start on a main
//! server whose `engine` is `platform`.

use crate::config::Config;
use std::sync::Arc;
use vercelify_platform::secrets::MasterKey;
use vercelify_platform::Store;

pub struct Platform {
    pub store: Arc<Store>,
    // Read by the platform API from M1 on (sealed variables and credentials).
    #[allow(dead_code)]
    pub key: Arc<MasterKey>,
}

/// What the doctor reports when the platform couldn't be opened.
#[derive(Debug, Clone, Default)]
pub struct OpenError(pub String);

impl Platform {
    pub fn open(cfg: &Config) -> Result<Self, OpenError> {
        let key = MasterKey::load_or_create(&cfg.master_key_file).map_err(|e| OpenError(format!("master key: {e}")))?;
        let store = Store::open(&cfg.platform_db).map_err(|e| OpenError(e.to_string()))?;
        Ok(Self {
            store: Arc::new(store),
            key: Arc::new(key),
        })
    }

    /// Open when this server runs the platform engine. A failure is logged and
    /// kept for the doctor; everything that doesn't need the platform keeps
    /// working.
    pub fn open_if_enabled(cfg: &Config) -> (Option<Arc<Platform>>, Option<OpenError>) {
        if !cfg.platform_engine() {
            return (None, None);
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
        Config {
            engine: engine.into(),
            role: Some("main".into()),
            platform_db: dir.join("platform.db"),
            master_key_file: dir.join("master.key"),
            ..Config::default()
        }
    }

    #[test]
    fn opens_only_for_the_platform_engine() {
        let dir = tempfile::tempdir().unwrap();
        let (p, e) = Platform::open_if_enabled(&cfg(dir.path(), "coolify"));
        assert!(p.is_none() && e.is_none());
        assert!(!dir.path().join("platform.db").exists());

        let (p, e) = Platform::open_if_enabled(&cfg(dir.path(), "platform"));
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

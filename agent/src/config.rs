//! Agent configuration: `/etc/vercelify/agent.toml`, overridable by env vars.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Unknown keys are reported as warnings, not errors: an agent rolled back
/// after a newer one added a setting must still start.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    /// Addresses to listen on. `"tailscale"` resolves to this node's
    /// Tailscale IPv4 (waiting for it if tailscaled is still starting).
    pub listen: Vec<String>,
    pub port: u16,
    /// File holding the bearer token (created by the installer).
    pub token_file: PathBuf,
    /// Token given directly (tests / containers). Takes precedence.
    pub token: Option<String>,
    pub storage_mount: String,
    pub hdd_volume_root: PathBuf,
    /// Refuse to create HDD volumes unless `storage_mount` is a real mount.
    pub require_storage_mount: bool,
    pub sys_class: PathBuf,
    pub sys_block: PathBuf,
    pub power_guard_state: PathBuf,
    pub docker_bin: String,
    pub tailscale_bin: Option<String>,
    pub systemctl_bin: String,
    pub mpv_bin: String,
    /// `drm` (no desktop; draws straight to the panel), `x11`, `wayland` or `auto`.
    pub display_backend: String,
    /// Extra environment for the display receiver (e.g. DISPLAY, XAUTHORITY).
    pub display_env: BTreeMap<String, String>,
    /// Extra mpv arguments appended after the built-in low-latency set.
    pub mpv_extra_args: Vec<String>,
    /// systemd units the cockpit may inspect and start/stop/restart.
    pub services: Vec<String>,
    /// Browser origins allowed via CORS (only needed for the web dev mode).
    pub cors_origins: Vec<String>,
    /// Container running Coolify's PostgreSQL (for storage-tier moves).
    pub coolify_db_container: String,
    /// Docker's named-volume directory (SSD tier).
    pub docker_volumes_root: PathBuf,
    /// `main` or `worker`. Unset: read `role=` from `install_env`.
    pub role: Option<String>,
    /// Written by the installer (`role=…`, `version=…`).
    pub install_env: PathBuf,
    /// Persistent agent state (auto-deploy watches).
    pub state_dir: PathBuf,
    /// Volatile state shared with the CLI (short pairing codes).
    pub run_dir: PathBuf,
    /// Coolify API as seen from this machine.
    pub coolify_url: String,
    /// Coolify API token created by the installer.
    pub coolify_token_file: PathBuf,
    /// Seconds between checks of watched branches (minimum 15; 0 = only on demand).
    pub autodeploy_interval_secs: u64,
    pub git_bin: String,
    /// Ollama API as seen from this machine.
    pub ollama_url: String,
    pub ollama_image: String,
    pub ollama_container: String,
    /// Port Ollama is published on (all interfaces; the firewall keeps it to the tailnet).
    pub ollama_port: u16,
    /// Where an attached main host's Coolify SSH key is authorised.
    pub authorized_keys_file: PathBuf,
    /// Coolify's SSH key store (main host).
    pub coolify_ssh_keys_dir: PathBuf,
    pub ssh_keygen_bin: String,
    /// Used to tell whether the installer set up a storage disk.
    pub fstab_file: PathBuf,
    /// The `vercelify` admin CLI (runs updates).
    pub cli_bin: String,
    pub systemd_run_bin: String,
    /// Thresholds shared with the power guard (doctor reads them).
    pub power_guard_conf: PathBuf,
    /// Unit files the installer writes (cloudflared, firewall…).
    pub systemd_dir: PathBuf,
    pub logind_dropin: PathBuf,
    pub reboot_required_file: PathBuf,
    pub smartctl_bin: String,
    pub timedatectl_bin: String,
    pub iptables_bin: String,
    pub curl_bin: String,
    /// Where alerts go (ntfy topic URL, or any URL accepting a POSTed text
    /// body). Unset: NOTIFY_URL from power-guard.conf. The cockpit can set
    /// one at runtime (stored in the state directory).
    pub notify_url: Option<String>,
    /// Seconds between background health evaluations (alerts, repairs).
    pub watch_interval_secs: u64,
    /// Check that every Coolify app's URL answers (main host).
    pub uptime_checks: bool,
    /// Agent tokens issued to paired devices (hashed).
    pub tokens_file: PathBuf,
    /// Append-only log of every change made through the API.
    pub audit_log: PathBuf,
    /// GitHub's REST API (pull request previews and commit statuses).
    pub github_api: String,
    /// Most pull request previews per app at once (memory on small machines).
    pub max_previews: usize,
    /// Backup settings written by `vercelify backup setup`.
    pub backup_env: PathBuf,
    /// The reboot window (`vercelify maintenance`).
    pub maintenance_conf: PathBuf,
    /// Power guard thresholds changed from the cockpit (/etc is read-only
    /// to the agent); the guard reads this after its own config.
    pub power_guard_overrides: PathBuf,
    /// Where sleeping apps are woken: Traefik sends their requests here.
    /// Empty = apps never sleep.
    pub wake_listen: String,
    /// Traefik's Prometheus endpoint (requests per app, to tell idle apps).
    pub traefik_metrics_url: String,
    /// Coolify's proxy: compose file and dynamic configuration directory.
    pub proxy_dir: PathBuf,
    /// Coolify's proxy container (its image tells the Traefik version).
    pub proxy_container: String,
    /// Where the proxy listens on this machine (cron jobs call apps through it).
    pub proxy_http_port: u16,
    pub proxy_https_port: u16,
    /// Let `vercel.json` rewrites proxy to this machine or private networks
    /// (off: they could publish Ollama, Coolify or a database).
    pub rewrite_private_targets: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: vec!["tailscale".into(), "127.0.0.1".into()],
            port: vercelify_core::DEFAULT_AGENT_PORT,
            token_file: "/etc/vercelify/agent.token".into(),
            token: None,
            storage_mount: vercelify_core::STORAGE_MOUNT.into(),
            hdd_volume_root: vercelify_core::HDD_VOLUME_ROOT.into(),
            require_storage_mount: true,
            sys_class: "/sys/class".into(),
            sys_block: "/sys/block".into(),
            power_guard_state: "/run/vercelify/power-guard.state".into(),
            docker_bin: "docker".into(),
            tailscale_bin: None,
            systemctl_bin: "systemctl".into(),
            mpv_bin: "mpv".into(),
            display_backend: "auto".into(),
            display_env: BTreeMap::new(),
            mpv_extra_args: vec![],
            services: [
                "docker",
                "tailscaled",
                "cloudflared",
                "sunshine",
                "vercelify-power-guard",
                "vercelify-firewall",
                "vercelify-agent",
            ]
            .map(String::from)
            .to_vec(),
            cors_origins: vec![],
            coolify_db_container: "coolify-db".into(),
            docker_volumes_root: vercelify_core::SSD_VOLUME_ROOT.into(),
            role: None,
            install_env: "/etc/vercelify/install.env".into(),
            state_dir: "/var/lib/vercelify".into(),
            run_dir: "/run/vercelify".into(),
            coolify_url: "http://127.0.0.1:8000".into(),
            coolify_token_file: "/etc/vercelify/coolify.token".into(),
            autodeploy_interval_secs: 60,
            git_bin: "git".into(),
            ollama_url: "http://127.0.0.1:11434".into(),
            ollama_image: "ollama/ollama:0.40.6".into(),
            ollama_container: "vercelify-ollama".into(),
            ollama_port: 11434,
            authorized_keys_file: "/root/.ssh/authorized_keys".into(),
            coolify_ssh_keys_dir: "/data/coolify/ssh/keys".into(),
            ssh_keygen_bin: "ssh-keygen".into(),
            fstab_file: "/etc/fstab".into(),
            cli_bin: "/usr/local/bin/vercelify".into(),
            systemd_run_bin: "systemd-run".into(),
            power_guard_conf: "/etc/vercelify/power-guard.conf".into(),
            systemd_dir: "/etc/systemd/system".into(),
            logind_dropin: "/etc/systemd/logind.conf.d/90-vercelify.conf".into(),
            reboot_required_file: "/var/run/reboot-required".into(),
            smartctl_bin: "smartctl".into(),
            timedatectl_bin: "timedatectl".into(),
            iptables_bin: "iptables".into(),
            curl_bin: "curl".into(),
            notify_url: None,
            watch_interval_secs: 60,
            uptime_checks: true,
            // /etc is read-only to the agent (ProtectSystem=full); state lives here.
            tokens_file: "/var/lib/vercelify/tokens.json".into(),
            audit_log: "/var/lib/vercelify/audit.log".into(),
            github_api: "https://api.github.com".into(),
            max_previews: 3,
            backup_env: "/etc/vercelify/backup.env".into(),
            maintenance_conf: "/etc/vercelify/maintenance.conf".into(),
            power_guard_overrides: "/var/lib/vercelify/power-guard.local.conf".into(),
            wake_listen: "0.0.0.0:7421".into(),
            traefik_metrics_url: "http://127.0.0.1:8080/metrics".into(),
            proxy_dir: "/data/coolify/proxy".into(),
            proxy_container: "coolify-proxy".into(),
            proxy_http_port: 80,
            proxy_https_port: 443,
            rewrite_private_targets: false,
        }
    }
}

/// `KEY=value` lines (shell-style config), comments skipped.
pub fn env_file_values(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') {
                return None;
            }
            let (k, v) = l.split_once('=')?;
            Some((
                k.trim().to_string(),
                v.trim().trim_matches('"').trim_matches('\'').to_string(),
            ))
        })
        .collect()
}

impl Config {
    pub fn load(path: Option<&Path>) -> anyhow_lite::Result<Self> {
        let mut cfg = match path {
            Some(p) if p.exists() => {
                let raw = std::fs::read_to_string(p)
                    .map_err(|e| anyhow_lite::err(format!("reading {}: {e}", p.display())))?;
                for key in unknown_keys(&raw) {
                    tracing::warn!(
                        "{}: unknown setting `{key}` ignored (typo, or from a newer version?)",
                        p.display()
                    );
                }
                toml::from_str(&raw).map_err(|e| anyhow_lite::err(format!("parsing {}: {e}", p.display())))?
            }
            _ => Config::default(),
        };
        cfg.apply_env();
        Ok(cfg)
    }

    fn apply_env(&mut self) {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        if let Some(v) = var("VERCELIFY_LISTEN") {
            self.listen = v.split(',').map(|s| s.trim().to_string()).collect();
        }
        if let Some(v) = var("VERCELIFY_PORT").and_then(|v| v.parse().ok()) {
            self.port = v;
        }
        if let Some(v) = var("VERCELIFY_TOKEN") {
            self.token = Some(v);
        }
        if let Some(v) = var("VERCELIFY_TOKEN_FILE") {
            self.token_file = v.into();
        }
        if let Some(v) = var("VERCELIFY_SYS_CLASS") {
            self.sys_class = v.into();
        }
        if let Some(v) = var("VERCELIFY_SYS_BLOCK") {
            self.sys_block = v.into();
        }
        if let Some(v) = var("VERCELIFY_STORAGE_MOUNT") {
            self.storage_mount = v;
        }
        if let Some(v) = var("VERCELIFY_HDD_VOLUME_ROOT") {
            self.hdd_volume_root = v.into();
        }
        if let Some(v) = var("VERCELIFY_REQUIRE_STORAGE_MOUNT") {
            self.require_storage_mount = !matches!(v.as_str(), "0" | "false" | "no");
        }
        if let Some(v) = var("VERCELIFY_POWER_GUARD_STATE") {
            self.power_guard_state = v.into();
        }
        if let Some(v) = var("VERCELIFY_DOCKER_BIN") {
            self.docker_bin = v;
        }
        if let Some(v) = var("VERCELIFY_TAILSCALE_BIN") {
            self.tailscale_bin = Some(v);
        }
        if let Some(v) = var("VERCELIFY_SYSTEMCTL_BIN") {
            self.systemctl_bin = v;
        }
        if let Some(v) = var("VERCELIFY_MPV_BIN") {
            self.mpv_bin = v;
        }
        if let Some(v) = var("VERCELIFY_DISPLAY_BACKEND") {
            self.display_backend = v;
        }
        if let Some(v) = var("VERCELIFY_MPV_EXTRA_ARGS") {
            self.mpv_extra_args = v.split_whitespace().map(String::from).collect();
        }
        if let Some(v) = var("VERCELIFY_COOLIFY_DB_CONTAINER") {
            self.coolify_db_container = v;
        }
        if let Some(v) = var("VERCELIFY_DOCKER_VOLUMES_ROOT") {
            self.docker_volumes_root = v.into();
        }
        if let Some(v) = var("VERCELIFY_ROLE") {
            self.role = Some(v);
        }
        if let Some(v) = var("VERCELIFY_INSTALL_ENV") {
            self.install_env = v.into();
        }
        if let Some(v) = var("VERCELIFY_STATE_DIR") {
            self.state_dir = v.into();
        }
        if let Some(v) = var("VERCELIFY_RUN_DIR") {
            self.run_dir = v.into();
        }
        if let Some(v) = var("VERCELIFY_COOLIFY_URL") {
            self.coolify_url = v;
        }
        if let Some(v) = var("VERCELIFY_COOLIFY_TOKEN_FILE") {
            self.coolify_token_file = v.into();
        }
        if let Some(v) = var("VERCELIFY_AUTODEPLOY_INTERVAL").and_then(|v| v.parse().ok()) {
            self.autodeploy_interval_secs = v;
        }
        if let Some(v) = var("VERCELIFY_GIT_BIN") {
            self.git_bin = v;
        }
        if let Some(v) = var("VERCELIFY_OLLAMA_URL") {
            self.ollama_url = v;
        }
        if let Some(v) = var("VERCELIFY_AUTHORIZED_KEYS") {
            self.authorized_keys_file = v.into();
        }
        if let Some(v) = var("VERCELIFY_COOLIFY_SSH_KEYS_DIR") {
            self.coolify_ssh_keys_dir = v.into();
        }
        if let Some(v) = var("VERCELIFY_SSH_KEYGEN_BIN") {
            self.ssh_keygen_bin = v;
        }
        if let Some(v) = var("VERCELIFY_FSTAB") {
            self.fstab_file = v.into();
        }
        if let Some(v) = var("VERCELIFY_CLI_BIN") {
            self.cli_bin = v;
        }
        if let Some(v) = var("VERCELIFY_SYSTEMD_RUN_BIN") {
            self.systemd_run_bin = v;
        }
        if let Some(v) = var("VERCELIFY_POWER_GUARD_CONF") {
            self.power_guard_conf = v.into();
        }
        if let Some(v) = var("VERCELIFY_SYSTEMD_DIR") {
            self.systemd_dir = v.into();
        }
        if let Some(v) = var("VERCELIFY_LOGIND_DROPIN") {
            self.logind_dropin = v.into();
        }
        if let Some(v) = var("VERCELIFY_REBOOT_REQUIRED_FILE") {
            self.reboot_required_file = v.into();
        }
        if let Some(v) = var("VERCELIFY_MAINTENANCE_CONF") {
            self.maintenance_conf = v.into();
        }
        if let Some(v) = var("VERCELIFY_POWER_GUARD_OVERRIDES") {
            self.power_guard_overrides = v.into();
        }
        if let Ok(v) = std::env::var("VERCELIFY_WAKE_LISTEN") {
            // Empty turns waking off.
            self.wake_listen = v;
        }
        if let Some(v) = var("VERCELIFY_TRAEFIK_METRICS_URL") {
            self.traefik_metrics_url = v;
        }
        if let Some(v) = var("VERCELIFY_PROXY_DIR") {
            self.proxy_dir = v.into();
        }
        if let Some(v) = var("VERCELIFY_PROXY_CONTAINER") {
            self.proxy_container = v;
        }
        if let Some(v) = var("VERCELIFY_PROXY_HTTP_PORT").and_then(|v| v.parse().ok()) {
            self.proxy_http_port = v;
        }
        if let Some(v) = var("VERCELIFY_PROXY_HTTPS_PORT").and_then(|v| v.parse().ok()) {
            self.proxy_https_port = v;
        }
        if let Some(v) = var("VERCELIFY_REWRITE_PRIVATE_TARGETS") {
            self.rewrite_private_targets = matches!(v.as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = var("VERCELIFY_SMARTCTL_BIN") {
            self.smartctl_bin = v;
        }
        if let Some(v) = var("VERCELIFY_TIMEDATECTL_BIN") {
            self.timedatectl_bin = v;
        }
        if let Some(v) = var("VERCELIFY_IPTABLES_BIN") {
            self.iptables_bin = v;
        }
        if let Some(v) = var("VERCELIFY_CURL_BIN") {
            self.curl_bin = v;
        }
        if let Some(v) = var("VERCELIFY_NOTIFY_URL") {
            self.notify_url = Some(v);
        }
        if let Some(v) = var("VERCELIFY_WATCH_INTERVAL").and_then(|v| v.parse().ok()) {
            self.watch_interval_secs = v;
        }
        if let Some(v) = var("VERCELIFY_UPTIME_CHECKS") {
            self.uptime_checks = !matches!(v.as_str(), "0" | "false" | "no");
        }
        if let Some(v) = var("VERCELIFY_TOKENS_FILE") {
            self.tokens_file = v.into();
        }
        if let Some(v) = var("VERCELIFY_AUDIT_LOG") {
            self.audit_log = v.into();
        }
        if let Some(v) = var("VERCELIFY_GITHUB_API") {
            self.github_api = v;
        }
        if let Some(v) = var("VERCELIFY_MAX_PREVIEWS").and_then(|v| v.parse().ok()) {
            self.max_previews = v;
        }
        if let Some(v) = var("VERCELIFY_BACKUP_ENV") {
            self.backup_env = v.into();
        }
        if let Some(v) = var("VERCELIFY_CORS_ORIGINS") {
            self.cors_origins = v.split(',').map(|s| s.trim().to_string()).collect();
        }
    }

    /// `main` or `worker`.
    pub fn role(&self) -> String {
        let from_env = || {
            std::fs::read_to_string(&self.install_env).ok()?.lines().find_map(|l| {
                l.trim()
                    .strip_prefix("role=")
                    .map(|v| v.trim().trim_matches('"').to_string())
            })
        };
        match self.role.clone().or_else(from_env).as_deref() {
            Some("worker") => "worker".into(),
            _ => "main".into(),
        }
    }

    /// KEY=VALUE pairs of power-guard.conf (shell syntax, comments ignored).
    /// The power guard's settings: its config file, then what was changed
    /// from the cockpit (the guard reads them in the same order).
    pub fn power_guard_values(&self) -> BTreeMap<String, String> {
        let mut v = env_file_values(&self.power_guard_conf);
        v.extend(env_file_values(&self.power_guard_overrides));
        v
    }

    /// The reboot window when automatic reboots are on (`04:30`).
    pub fn maintenance_window(&self) -> Option<String> {
        let v = env_file_values(&self.maintenance_conf);
        if !self.maintenance_conf.exists() || v.get("AUTO_REBOOT").map(String::as_str).unwrap_or("yes") != "yes" {
            return None;
        }
        Some(v.get("REBOOT_WINDOW").cloned().unwrap_or_else(|| "04:30".into()))
    }

    /// `install.env` value (profile=, version=, channel=…).
    pub fn install_value(&self, key: &str) -> Option<String> {
        std::fs::read_to_string(&self.install_env).ok()?.lines().find_map(|l| {
            l.trim()
                .strip_prefix(&format!("{key}="))
                .map(|v| v.trim().trim_matches('"').to_string())
                .filter(|v| !v.is_empty())
        })
    }

    /// The Coolify API token, if the installer created one.
    pub fn coolify_token(&self) -> Option<String> {
        std::fs::read_to_string(&self.coolify_token_file)
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    }

    /// Resolve the bearer token from config or the token file.
    pub fn resolve_token(&self) -> anyhow_lite::Result<String> {
        let token = match &self.token {
            Some(t) => t.trim().to_string(),
            None => std::fs::read_to_string(&self.token_file)
                .map_err(|e| {
                    anyhow_lite::err(format!(
                        "reading token file {}: {e} (run `vercelify-agent token --write`)",
                        self.token_file.display()
                    ))
                })?
                .trim()
                .to_string(),
        };
        if token.len() < 16 {
            return Err(anyhow_lite::err("agent token must be at least 16 characters"));
        }
        Ok(token)
    }
}

/// Top-level keys of an agent.toml that this version doesn't know.
pub fn unknown_keys(raw: &str) -> Vec<String> {
    let Ok(table) = raw.parse::<toml::Table>() else {
        return vec![];
    };
    let known = serde_json::to_value(Config::default()).unwrap_or_default();
    let known = known.as_object().cloned().unwrap_or_default();
    table.keys().filter(|k| !known.contains_key(*k)).cloned().collect()
}

/// Minimal error type so the binary doesn't need `anyhow`.
pub mod anyhow_lite {
    #[derive(Debug)]
    pub struct Error(pub String);
    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }
    impl std::error::Error for Error {}
    pub type Result<T> = std::result::Result<T, Error>;
    pub fn err(msg: impl Into<String>) -> Error {
        Error(msg.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let cfg: Config = toml::from_str(
            r#"
            listen = ["100.64.0.1"]
            port = 9000
            display_backend = "x11"
            display_env = { DISPLAY = ":0" }
            services = ["docker"]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.port, 9000);
        assert_eq!(cfg.display_env.get("DISPLAY").unwrap(), ":0");
        assert_eq!(cfg.storage_mount, "/mnt/storage");
    }

    #[test]
    fn role_from_install_env() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join("install.env");
        let mut cfg = Config {
            install_env: env.clone(),
            ..Config::default()
        };
        assert_eq!(cfg.role(), "main", "no file: main");
        std::fs::write(&env, "version=0.2.0\nrole=worker\n").unwrap();
        assert_eq!(cfg.role(), "worker");
        cfg.role = Some("main".into());
        assert_eq!(cfg.role(), "main", "explicit config wins");
    }

    #[test]
    fn power_guard_values() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("pg.conf");
        std::fs::write(
            &f,
            "# c\nTHERMAL_PAUSE_C=78\nNOTIFY_URL=\"https://ntfy.sh/x\"\n#THERMAL_FREEZE_C=1\n",
        )
        .unwrap();
        let cfg = Config {
            power_guard_conf: f,
            power_guard_overrides: dir.path().join("local.conf"),
            maintenance_conf: dir.path().join("maintenance.conf"),
            ..Config::default()
        };
        let v = cfg.power_guard_values();
        assert_eq!(v.get("THERMAL_PAUSE_C").map(String::as_str), Some("78"));
        assert_eq!(v.get("NOTIFY_URL").map(String::as_str), Some("https://ntfy.sh/x"));
        assert!(!v.contains_key("THERMAL_FREEZE_C"));
        // Changes from the cockpit win.
        std::fs::write(&cfg.power_guard_overrides, "THERMAL_PAUSE_C=75\n").unwrap();
        assert_eq!(
            cfg.power_guard_values().get("THERMAL_PAUSE_C").map(String::as_str),
            Some("75")
        );

        // Reboot window: off without the file, on by default with it.
        assert_eq!(cfg.maintenance_window(), None);
        std::fs::write(&cfg.maintenance_conf, "REBOOT_WINDOW=03:10\n").unwrap();
        assert_eq!(cfg.maintenance_window().as_deref(), Some("03:10"));
        std::fs::write(&cfg.maintenance_conf, "AUTO_REBOOT=no\nREBOOT_WINDOW=03:10\n").unwrap();
        assert_eq!(cfg.maintenance_window(), None);
    }

    #[test]
    fn unknown_keys_are_reported_not_fatal() {
        let raw = "prot = 1\nport = 9000\nfuture_setting = true\n";
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.port, 9000);
        assert_eq!(
            unknown_keys(raw),
            vec!["future_setting".to_string(), "prot".to_string()]
        );
        assert!(
            unknown_keys("port = 1\nlisten = []\ntoken = \"x\"").is_empty(),
            "Option fields are known too"
        );
    }

    #[test]
    fn shipped_config_has_no_unknown_keys() {
        let raw = include_str!("../../host/config/agent.toml");
        assert!(unknown_keys(raw).is_empty(), "{:?}", unknown_keys(raw));
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.services, Config::default().services, "one allowlist, two places");
    }
}

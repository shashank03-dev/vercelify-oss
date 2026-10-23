//! The power guard's thresholds, from the cockpit.
//!
//! The guard stays its own small daemon on purpose: it must shut the machine
//! down cleanly on an empty battery even when this agent is stopped or
//! broken. The agent shows its state, lets you tune it (written to
//! `power_guard_overrides`, which the guard reads after its own config, since
//! `/etc` is read-only to the agent), restarts it to apply, and restarts it
//! when it stopped (see `watch::reconcile`).

use crate::config::{env_file_values, Config};
use crate::error::AppError;
use crate::exec::run;
use std::collections::BTreeMap;
use std::time::Duration;
use vercelify_core::{PowerGuardSettings, PowerGuardState, PowerGuardStatus};

pub const UNIT: &str = "vercelify-power-guard";

const KEYS: [&str; 7] = [
    "BATTERY_SHUTDOWN_PCT",
    "BATTERY_WARN_PCT",
    "THERMAL_PAUSE_C",
    "THERMAL_FREEZE_C",
    "THERMAL_RESUME_C",
    "THERMAL_SHUTDOWN_C",
    "THERMAL_CPU_CAP_PCT",
];

fn num(v: &BTreeMap<String, String>, k: &str) -> Option<u32> {
    v.get(k)
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|f| f.round() as u32)
}

/// Settings from KEY=value pairs, with the guard's own fallbacks.
pub fn settings_from(v: &BTreeMap<String, String>, profile: &str) -> PowerGuardSettings {
    let d = vercelify_core::hardware::defaults(profile);
    let shutdown = num(v, "THERMAL_SHUTDOWN_C").unwrap_or(d.shutdown_c as u32);
    PowerGuardSettings {
        battery_shutdown_pct: num(v, "BATTERY_SHUTDOWN_PCT").unwrap_or(15),
        battery_warn_pct: num(v, "BATTERY_WARN_PCT").unwrap_or(30),
        thermal_pause_c: num(v, "THERMAL_PAUSE_C").unwrap_or(d.pause_c as u32),
        // Same default as power-guard.sh: three degrees below shutdown.
        thermal_freeze_c: num(v, "THERMAL_FREEZE_C").unwrap_or(shutdown.saturating_sub(3)),
        thermal_resume_c: num(v, "THERMAL_RESUME_C").unwrap_or(d.resume_c as u32),
        thermal_shutdown_c: shutdown,
        thermal_cpu_cap_pct: num(v, "THERMAL_CPU_CAP_PCT").unwrap_or(50),
    }
}

/// The overrides file: only the keys that differ from the installer's.
pub fn overrides_file(s: &PowerGuardSettings, base: &PowerGuardSettings) -> String {
    let pairs = [
        (KEYS[0], s.battery_shutdown_pct, base.battery_shutdown_pct),
        (KEYS[1], s.battery_warn_pct, base.battery_warn_pct),
        (KEYS[2], s.thermal_pause_c, base.thermal_pause_c),
        (KEYS[3], s.thermal_freeze_c, base.thermal_freeze_c),
        (KEYS[4], s.thermal_resume_c, base.thermal_resume_c),
        (KEYS[5], s.thermal_shutdown_c, base.thermal_shutdown_c),
        (KEYS[6], s.thermal_cpu_cap_pct, base.thermal_cpu_cap_pct),
    ];
    let mut out = String::from("# Changed from the Vercelify app; read by the power guard after power-guard.conf.\n");
    for (k, v, b) in pairs {
        if v != b {
            out.push_str(&format!("{k}={v}\n"));
        }
    }
    out
}

fn profile(cfg: &Config) -> String {
    cfg.install_value("profile").unwrap_or_else(|| "pc".into())
}

pub async fn status(cfg: &Config) -> PowerGuardStatus {
    let p = profile(cfg);
    let defaults = settings_from(&env_file_values(&cfg.power_guard_conf), &p);
    let overrides = env_file_values(&cfg.power_guard_overrides);
    PowerGuardStatus {
        active: run(
            &cfg.systemctl_bin,
            &["is-active", "--quiet", UNIT],
            Duration::from_secs(10),
        )
        .await
        .is_ok_and(|o| o.success()),
        state: std::fs::read_to_string(&cfg.power_guard_state)
            .ok()
            .and_then(|r| serde_json::from_str::<PowerGuardState>(&r).ok()),
        settings: settings_from(&cfg.power_guard_values(), &p),
        defaults,
        customized: KEYS.iter().any(|k| overrides.contains_key(*k)),
    }
}

async fn restart(cfg: &Config) -> Result<(), AppError> {
    if !cfg.systemd_dir.join(format!("{UNIT}.service")).exists() {
        return Ok(());
    }
    let out = run(&cfg.systemctl_bin, &["restart", UNIT], Duration::from_secs(60)).await?;
    if out.success() {
        Ok(())
    } else {
        Err(AppError::internal(format!(
            "restarting the power guard: {}",
            out.stderr.trim()
        )))
    }
}

/// Apply new thresholds (None = back to the installer's).
pub async fn set(cfg: &Config, s: Option<PowerGuardSettings>) -> Result<PowerGuardStatus, AppError> {
    match s {
        Some(s) => {
            if let Some(p) = s.problem() {
                return Err(AppError::bad_request(p));
            }
            let base = settings_from(&env_file_values(&cfg.power_guard_conf), &profile(cfg));
            crate::watch::write_private(&cfg.power_guard_overrides, overrides_file(&s, &base).as_bytes())
                .map_err(|e| AppError::internal(format!("saving the settings: {e}")))?;
        }
        None => {
            let _ = std::fs::remove_file(&cfg.power_guard_overrides);
        }
    }
    restart(cfg).await?;
    Ok(status(cfg).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(s: &str) -> BTreeMap<String, String> {
        s.lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.into(), v.into()))
            .collect()
    }

    #[test]
    fn fallbacks_follow_the_guard_and_the_board() {
        let pi = settings_from(&map(""), "pi5");
        assert_eq!(
            (
                pi.thermal_pause_c,
                pi.thermal_resume_c,
                pi.thermal_shutdown_c,
                pi.thermal_freeze_c
            ),
            (78, 68, 85, 82)
        );
        let pc = settings_from(&map("THERMAL_SHUTDOWN_C=100\nBATTERY_SHUTDOWN_PCT=10"), "laptop");
        assert_eq!(
            (pc.thermal_freeze_c, pc.battery_shutdown_pct, pc.battery_warn_pct),
            (97, 10, 30)
        );
        assert_eq!(pc.problem(), None);
        assert_eq!(pi.problem(), None);
    }

    #[test]
    fn only_changed_keys_are_written_and_bad_values_refused() {
        let base = settings_from(&map(""), "pc");
        let mut s = base;
        s.thermal_pause_c = 88;
        let f = overrides_file(&s, &base);
        assert!(f.contains("THERMAL_PAUSE_C=88\n"));
        assert_eq!(f.lines().filter(|l| !l.starts_with('#')).count(), 1);
        // Read back exactly as the guard and the doctor read it.
        assert_eq!(settings_from(&map(&f), "pc"), s);

        s.thermal_resume_c = 90;
        assert!(s.problem().unwrap().contains("resume < pause"));
        let mut s = base;
        s.battery_warn_pct = 10;
        assert!(s.problem().is_some());
        let mut s = base;
        s.thermal_cpu_cap_pct = 5;
        assert!(s.problem().is_some());
    }
}

//! `GET /v1/doctor`: every health check of a machine, as data. This is the
//! one implementation: `sudo vercelify doctor` prints this report, the
//! cockpit shows it, and the watcher turns changes into alerts.
//!
//! Facts are gathered first (some cached, the slow ones for an hour) so the
//! rules in [`evaluate`] stay pure and unit-testable.

use crate::config::Config;
use crate::exec::run;
use crate::http::{self, Req};
use crate::routes::AppState;
use crate::{coolify, services, tailscale, volumes};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};
use vercelify_core::{CheckStatus, DoctorCheck, DoctorReport, HardwareInfo, MediaKind, SystemSnapshot};

/// Thresholds the checks compare against (from power-guard.conf, falling
/// back to the machine's profile).
#[derive(Debug, Clone, PartialEq)]
pub struct Thresholds {
    pub pause_c: f32,
    pub resume_c: f32,
    pub battery_shutdown_pct: u8,
}

impl Thresholds {
    pub fn from_config(cfg: &Config, profile: &str) -> Self {
        let v = cfg.power_guard_values();
        let d = vercelify_core::hardware::defaults(profile);
        let num = |k: &str, def: f32| v.get(k).and_then(|s| s.parse::<f32>().ok()).unwrap_or(def);
        Self {
            pause_c: num("THERMAL_PAUSE_C", d.pause_c),
            resume_c: num("THERMAL_RESUME_C", d.resume_c),
            battery_shutdown_pct: num("BATTERY_SHUTDOWN_PCT", 15.0) as u8,
        }
    }
}

/// Result of a SMART health query for one disk.
#[derive(Debug, Clone, PartialEq)]
pub struct SmartFact {
    pub device: String,
    pub passed: bool,
    pub reallocated: Option<u64>,
}

/// Last run of `vercelify backup` (`backup.json` in the state directory).
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
pub struct BackupFact {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// Age in hours, filled in when read.
    #[serde(skip)]
    pub age_hours: Option<u64>,
}

/// Everything the checks look at.
#[derive(Debug, Clone)]
pub struct Facts {
    pub role: String,
    pub docker: Result<String, String>,
    pub tailscale_state: String,
    pub tailscale_name: String,
    /// Days until the node key expires (negative = expired). None = never.
    pub key_expiry_days: Option<i64>,
    pub system_disk: Option<(u64, u64)>,
    /// The installer set up a storage disk (fstab marker).
    pub storage_expected: bool,
    pub storage_mounted: bool,
    pub memory: (u64, u64),
    pub max_temp_c: Option<f32>,
    pub on_battery: bool,
    pub battery_pct: Option<u8>,
    pub power_guard: Option<String>,
    pub firewall_active: bool,
    pub power_guard_active: bool,
    /// Main only: Coolify answers `/api/health`.
    pub coolify_healthy: Option<bool>,
    /// The platform engine's database: Ok(schema version) or why it failed.
    /// None = this server doesn't run the platform engine.
    pub platform: Option<Result<u32, String>>,
    /// Main only: the stored API token is accepted (None = no token).
    pub coolify_token_ok: Option<bool>,
    /// Main only: required columns missing from Coolify's database.
    pub coolify_schema_missing: Option<Vec<String>>,
    pub coolify_auto_update: Option<bool>,
    /// Main only: localhost server row has an SSH user.
    pub coolify_ssh_user_ok: Option<bool>,
    pub proxy_health: Option<String>,
    /// Main only: Coolify's wildcard domain is set.
    pub wildcard_set: Option<bool>,
    /// None = no tunnel installed.
    pub tunnel_active: Option<bool>,
    /// None = not a laptop.
    pub lid_handled: Option<bool>,
    /// None = Ollama not installed.
    pub ollama_ready: Option<bool>,
    pub autodeploy_errors: usize,
    pub hardware: Option<HardwareInfo>,
    /// Hours since the OOM killer last struck (None = not seen while running).
    pub oom_recent_hours: Option<u64>,
    pub clock_synced: Option<bool>,
    pub reboot_required: bool,
    /// Automatic reboots for updates: the nightly window (None = off).
    pub reboot_window: Option<String>,
    /// Why the last night's reboot was skipped, if it was.
    pub reboot_skipped: Option<String>,
    pub smart: Vec<SmartFact>,
    /// None = no backup has run yet.
    pub backup: Option<BackupFact>,
    /// Hours since backups were set up (`backup.env`); None = not set up.
    pub backup_setup_hours: Option<u64>,
    pub thresholds: Thresholds,
}

fn check(id: &str, label: &str, status: CheckStatus, detail: impl Into<String>, fix: Option<&str>) -> DoctorCheck {
    DoctorCheck {
        id: id.into(),
        label: label.into(),
        status,
        detail: detail.into(),
        fix: if status == CheckStatus::Ok {
            None
        } else {
            fix.map(String::from)
        },
    }
}

fn gb(b: u64) -> String {
    format!("{:.1} GB", b as f64 / 1024f64.powi(3))
}

pub fn evaluate(f: &Facts) -> Vec<DoctorCheck> {
    use CheckStatus::{Bad, Ok as Fine, Warn};
    let main = f.role != "worker";
    let mut out = Vec::new();
    out.push(match &f.docker {
        Ok(v) => check("docker", "Docker", Fine, format!("Docker {v}"), None),
        Err(e) => check(
            "docker",
            "Docker",
            Bad,
            e.clone(),
            Some("sudo systemctl restart docker; journalctl -u docker -n 50"),
        ),
    });
    out.push(if f.tailscale_state == "Running" {
        check("tailscale", "Tailscale", Fine, f.tailscale_name.clone(), None)
    } else {
        check(
            "tailscale",
            "Tailscale",
            Bad,
            format!("not connected ({})", f.tailscale_state),
            Some("sudo tailscale up"),
        )
    });
    if let Some(days) = f.key_expiry_days {
        let fix = Some("Tailscale admin console → Machines → this machine → Disable key expiry (or re-authenticate: sudo tailscale up --force-reauth)");
        out.push(if days < 0 {
            check(
                "tailscale-key",
                "Tailscale key",
                Bad,
                "expired — this machine has left your tailnet",
                fix,
            )
        } else if days <= 14 {
            check(
                "tailscale-key",
                "Tailscale key",
                Warn,
                format!("expires in {days} day{}", if days == 1 { "" } else { "s" }),
                fix,
            )
        } else {
            check(
                "tailscale-key",
                "Tailscale key",
                Fine,
                format!("expires in {days} days"),
                None,
            )
        });
    }
    match &f.platform {
        Some(Ok(v)) => out.push(check(
            "platform",
            "Platform",
            Fine,
            format!("state database ready (schema {v})"),
            None,
        )),
        Some(Err(e)) => out.push(check(
            "platform",
            "Platform",
            Bad,
            e.clone(),
            Some("journalctl -u vercelify-agent -n 50; sudo vercelify restore-state restores the last good copy"),
        )),
        None => {}
    }
    if main {
        out.push(match f.coolify_healthy {
            Some(true) => check("coolify", "Coolify", Fine, "dashboard and API are up", None),
            _ => check(
                "coolify",
                "Coolify",
                Bad,
                "not answering on port 8000",
                Some("docker logs coolify --tail 50 (Coolify's web server needs IPv6 enabled)"),
            ),
        });
        out.push(match f.coolify_token_ok {
            Some(true) => check("coolify-token", "Coolify API token", Fine, "accepted", None),
            Some(false) => check(
                "coolify-token",
                "Coolify API token",
                Bad,
                "rejected by Coolify",
                Some("sudo vercelify pair --refresh, then pair again"),
            ),
            None => check(
                "coolify-token",
                "Coolify API token",
                Warn,
                "none stored — deploys from the Mac app and auto-deploy won't work",
                Some("sudo vercelify pair --refresh"),
            ),
        });
        if let Some(missing) = &f.coolify_schema_missing {
            out.push(if missing.is_empty() {
                check(
                    "coolify-schema",
                    "Coolify version",
                    Fine,
                    "compatible with Vercelify",
                    None,
                )
            } else {
                check(
                    "coolify-schema",
                    "Coolify version",
                    Bad,
                    format!("missing {} — storage moves and pools may fail", missing.join(", ")),
                    Some("sudo vercelify update (a newer Vercelify may support this Coolify)"),
                )
            });
        }
        if f.coolify_auto_update == Some(true) {
            out.push(check(
                "coolify-autoupdate",
                "Coolify auto-update",
                Warn,
                "on — a Coolify release could change what Vercelify relies on while you're away",
                Some("re-run the installer (turns it off); upgrade with: sudo vercelify coolify upgrade"),
            ));
        }
        if f.coolify_ssh_user_ok == Some(false) {
            out.push(check(
                "coolify-ssh",
                "Coolify SSH user",
                Bad,
                "empty — every deployment fails",
                Some("re-run the installer (sets it to root)"),
            ));
        }
        if let Some(h) = &f.proxy_health {
            out.push(if h == "healthy" || h == "running" {
                check("proxy", "Traefik proxy", Fine, h.clone(), None)
            } else {
                check(
                    "proxy",
                    "Traefik proxy",
                    Bad,
                    h.clone(),
                    Some("Coolify → Servers → localhost → Proxy → Restart"),
                )
            });
        }
        if f.wildcard_set == Some(false) {
            out.push(check(
                "wildcard",
                "App domains",
                Warn,
                "no wildcard domain — new apps get no URL",
                Some("sudo vercelify domain [your-domain]"),
            ));
        }
    }
    if let Some(active) = f.tunnel_active {
        out.push(if active {
            check("tunnel", "Cloudflare tunnel", Fine, "connected", None)
        } else {
            check(
                "tunnel",
                "Cloudflare tunnel",
                Bad,
                "not running — public apps are unreachable",
                Some("sudo systemctl restart cloudflared; journalctl -u cloudflared -n 50"),
            )
        });
    }
    if let Some((used, total)) = f.system_disk {
        let pct = (used * 100).checked_div(total).unwrap_or(0);
        let free = total.saturating_sub(used);
        out.push(check(
            "disk",
            "System disk",
            if pct >= 95 {
                Bad
            } else if pct >= 85 {
                Warn
            } else {
                Fine
            },
            format!("{pct}% used, {} free", gb(free)),
            Some("docker system prune -af (keeps volumes), or Coolify → Servers → Docker cleanup"),
        ));
    }
    if let Some(hw) = &f.hardware {
        if hw.root_media == MediaKind::SdCard {
            out.push(check(
                "sd-card",
                "System disk type",
                Warn,
                "running from an SD card — it wears out under a server's writes",
                Some("boot from a USB SSD or an NVMe HAT; keep backups on (sudo vercelify backup)"),
            ));
        }
    }
    for s in &f.smart {
        out.push(if !s.passed {
            check(
                &format!("smart-{}", s.device.trim_start_matches("/dev/")),
                &format!("Disk health {}", s.device),
                Bad,
                "SMART says this disk is failing",
                Some("back up now (sudo vercelify backup run) and replace the disk"),
            )
        } else if s.reallocated.unwrap_or(0) > 0 {
            check(
                &format!("smart-{}", s.device.trim_start_matches("/dev/")),
                &format!("Disk health {}", s.device),
                Warn,
                format!(
                    "{} reallocated sectors — the disk is wearing",
                    s.reallocated.unwrap_or(0)
                ),
                Some("keep backups current and plan to replace it"),
            )
        } else {
            check(
                &format!("smart-{}", s.device.trim_start_matches("/dev/")),
                &format!("Disk health {}", s.device),
                Fine,
                "SMART OK",
                None,
            )
        });
    }
    out.push(match (f.storage_expected, f.storage_mounted) {
        (_, true) => check("storage", "Storage disk", Fine, "mounted", None),
        (true, false) => check(
            "storage",
            "Storage disk",
            Bad,
            "missing — databases on the HDD tier will not start",
            Some("check the cable, then: sudo mount -a; see dmesg | tail"),
        ),
        (false, false) => check(
            "storage",
            "Storage disk",
            Warn,
            "single disk: everything lives on the system disk",
            Some("add an HDD and re-run the installer to get a storage tier"),
        ),
    });
    let (total, available) = f.memory;
    if let Some(free_pct) = (available * 100).checked_div(total) {
        let pressure = f
            .hardware
            .as_ref()
            .and_then(|h| h.pressure)
            .and_then(|p| p.memory_some_avg10)
            .unwrap_or(0.0);
        let status = if free_pct < 5 || pressure >= 40.0 {
            Bad
        } else if free_pct < 12 || pressure >= 15.0 {
            Warn
        } else {
            Fine
        };
        let mut detail = format!("{} free of {}", gb(available), gb(total));
        if pressure >= 15.0 {
            detail.push_str(&format!(", tasks stalled on memory {pressure:.0}% of the time"));
        }
        out.push(check(
            "memory",
            "Memory",
            status,
            detail,
            Some("stop apps you don't use, stop idle AI models (sudo vercelify llm stop), or move apps to another server"),
        ));
    }
    if let Some(h) = f.oom_recent_hours {
        if h < 24 {
            out.push(check(
                "oom",
                "Out of memory",
                Warn,
                format!(
                    "the kernel killed a process {} for lack of memory",
                    if h == 0 {
                        "in the last hour".to_string()
                    } else {
                        format!("{h} h ago")
                    }
                ),
                Some("set memory limits on apps (Coolify → app → Resource limits) or move one to another server"),
            ));
        }
    }
    if let Some(hw) = &f.hardware {
        if !hw.memory_cgroup {
            out.push(check(
                "memory-limits",
                "Memory limits",
                Bad,
                "the kernel ignores container memory limits (cgroup memory controller off)",
                Some("re-run the installer (adds cgroup_enable=memory to cmdline.txt), then reboot"),
            ));
        }
        if let Some(t) = hw.throttling {
            out.push(if t.under_voltage_now {
                check(
                    "power-supply",
                    "Power supply",
                    Bad,
                    "under-voltage right now — expect crashes and SD card corruption",
                    Some("use the official Raspberry Pi power supply (27 W for a Pi 5) and a short cable"),
                )
            } else if t.under_voltage_since_boot || t.throttled_since_boot {
                check(
                    "power-supply",
                    "Power supply",
                    Warn,
                    if t.under_voltage_since_boot {
                        "under-voltage happened since boot"
                    } else {
                        "the CPU was throttled since boot"
                    },
                    Some("check the power supply and cooling (a fan or heatsink case)"),
                )
            } else {
                check(
                    "power-supply",
                    "Power supply",
                    Fine,
                    "no under-voltage or throttling",
                    None,
                )
            });
        }
    }
    if let Some(t) = f.max_temp_c {
        let th = &f.thresholds;
        out.push(check(
            "temperature",
            "Temperature",
            if t >= th.pause_c {
                Bad
            } else if t >= th.resume_c {
                Warn
            } else {
                Fine
            },
            format!("{t:.0} °C (builds pause at {:.0} °C)", th.pause_c),
            Some("clean the fan and vents, keep the lid open or raise the machine for airflow"),
        ));
    }
    if f.on_battery {
        let fix = format!(
            "plug the charger back in; the power guard shuts down cleanly at {}%",
            f.thresholds.battery_shutdown_pct
        );
        out.push(check(
            "power",
            "Power",
            Warn,
            format!(
                "on battery{}",
                f.battery_pct.map(|p| format!(" ({p}%)")).unwrap_or_default()
            ),
            Some(&fix),
        ));
    }
    out.push(if f.power_guard_active {
        let state = f.power_guard.clone().unwrap_or_else(|| "ok".into());
        let status = match state.as_str() {
            "thermal_paused" | "shutting_down" => Warn,
            _ => Fine,
        };
        check(
            "power-guard",
            "Power guard",
            status,
            state,
            Some("journalctl -u vercelify-power-guard -n 50"),
        )
    } else {
        check(
            "power-guard",
            "Power guard",
            Warn,
            "not running",
            Some("sudo systemctl enable --now vercelify-power-guard"),
        )
    });
    if f.lid_handled == Some(false) {
        out.push(check(
            "lid",
            "Lid close",
            Warn,
            "closing the lid may suspend the machine",
            Some("re-run the installer"),
        ));
    }
    out.push(if f.firewall_active {
        check(
            "firewall",
            "Firewall",
            Fine,
            "dashboard and AI API private to the tailnet",
            None,
        )
    } else {
        check(
            "firewall",
            "Firewall",
            Warn,
            "off — Coolify and Ollama ports are reachable from the local network",
            Some("sudo systemctl enable --now vercelify-firewall"),
        )
    });
    if f.clock_synced == Some(false) {
        out.push(check(
            "clock",
            "Clock",
            Warn,
            "not synchronised — HTTPS, Git and backups can fail",
            Some("sudo timedatectl set-ntp true; check the internet connection"),
        ));
    }
    if f.reboot_required {
        out.push(match (&f.reboot_window, &f.reboot_skipped) {
            (Some(w), None) => check(
                "reboot",
                "Security updates",
                Fine,
                format!("installed; the server reboots by itself around {w} tonight"),
                None,
            ),
            (Some(w), Some(why)) => check(
                "reboot",
                "Security updates",
                Warn,
                format!("installed, waiting for a reboot (skipped last night: {why}; next try around {w})"),
                Some("sudo reboot now, or let the nightly window try again"),
            ),
            (None, _) => check(
                "reboot",
                "Security updates",
                Warn,
                "installed, waiting for a reboot (automatic reboots are off)",
                Some("sudo reboot (apps start again by themselves), or: sudo vercelify maintenance on"),
            ),
        });
    }
    if let Some(b) = &f.backup {
        out.push(if !b.ok {
            check(
                "backup",
                "Backups",
                Bad,
                format!("last backup failed: {}", b.error.clone().unwrap_or_default()),
                Some("sudo vercelify backup run, and see: sudo vercelify logs backup"),
            )
        } else if b.age_hours.unwrap_or(0) > 48 {
            check(
                "backup",
                "Backups",
                Warn,
                format!("last one {} days ago", b.age_hours.unwrap_or(0) / 24),
                Some("sudo systemctl status vercelify-backup.timer"),
            )
        } else {
            check(
                "backup",
                "Backups",
                Fine,
                format!("last one {} h ago", b.age_hours.unwrap_or(0)),
                None,
            )
        });
    } else if main {
        out.push(match f.backup_setup_hours {
            // The installer sets them up; the first one runs that night.
            Some(h) if h <= 48 => check("backup", "Backups", Fine, "set up; the first one runs tonight", None),
            Some(h) => check(
                "backup",
                "Backups",
                Warn,
                format!("set up {} days ago, but none has run", h / 24),
                Some("sudo vercelify backup run, and: sudo systemctl status vercelify-backup.timer"),
            ),
            None => check(
                "backup",
                "Backups",
                Warn,
                "not set up — losing this disk loses every app's data and secrets",
                Some("sudo vercelify backup setup"),
            ),
        });
    }
    if let Some(ready) = f.ollama_ready {
        out.push(if ready {
            check("ollama", "AI models", Fine, "Ollama is running", None)
        } else {
            check(
                "ollama",
                "AI models",
                Warn,
                "Ollama is installed but not running",
                Some("sudo vercelify llm start"),
            )
        });
    }
    if f.autodeploy_errors > 0 {
        out.push(check(
            "autodeploy",
            "Auto-deploy",
            Warn,
            format!(
                "{} branch watch{} reporting errors",
                f.autodeploy_errors,
                if f.autodeploy_errors == 1 { "" } else { "es" }
            ),
            Some("open the app's Auto-deploy tab, or: sudo vercelify autodeploy list"),
        ));
    }
    out
}

fn fstab_has_storage(fstab: &Path, mount: &str) -> bool {
    std::fs::read_to_string(fstab)
        .map(|s| s.lines().any(|l| l.trim() == format!("# vercelify {mount}")))
        .unwrap_or(false)
}

/// Whole days from now until an RFC 3339 time (negative when past).
pub fn days_until(rfc3339: &str, now: chrono::DateTime<chrono::Utc>) -> Option<i64> {
    let t = chrono::DateTime::parse_from_rfc3339(rfc3339).ok()?;
    Some((t.with_timezone(&chrono::Utc) - now).num_hours().div_euclid(24))
}

/// Parse `smartctl -H -A -j` output.
pub fn parse_smart(device: &str, json: &str) -> Option<SmartFact> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let passed = v.get("smart_status")?.get("passed")?.as_bool()?;
    let reallocated = v
        .pointer("/ata_smart_attributes/table")
        .and_then(|t| t.as_array())
        .and_then(|t| t.iter().find(|a| a.get("id").and_then(|i| i.as_u64()) == Some(5)))
        .and_then(|a| a.pointer("/raw/value"))
        .and_then(|r| r.as_u64());
    Some(SmartFact {
        device: device.into(),
        passed,
        reallocated,
    })
}

async fn coolify_facts(cfg: &Config) -> (Option<bool>, Option<bool>) {
    let base = cfg.coolify_url.trim_end_matches('/');
    let health_url = format!("{base}/api/health");
    let healthy = match http::call(Req::get(&health_url).timeout(Duration::from_secs(5))).await {
        Ok((s, body)) => s.is_success() && String::from_utf8_lossy(&body).trim() == "OK",
        Err(_) => false,
    };
    let token_ok = match cfg.coolify_token() {
        None => None,
        Some(t) => {
            let url = format!("{base}/api/v1/version");
            match http::call(Req::get(&url).bearer(&t).timeout(Duration::from_secs(5))).await {
                Ok((s, _)) => Some(s.is_success()),
                // Coolify down: the health check already says so.
                Err(_) => healthy.then_some(false),
            }
        }
    };
    (Some(healthy), token_ok)
}

/// Facts that are expensive to collect, refreshed at most once an hour.
#[derive(Debug, Clone, Default)]
pub struct SlowFacts {
    pub schema_missing: Option<Vec<String>>,
    pub auto_update: Option<bool>,
    pub ssh_user_ok: Option<bool>,
    pub wildcard_set: Option<bool>,
    pub smart: Vec<SmartFact>,
}

/// Remembers slow facts and when the OOM killer last struck.
#[derive(Default)]
pub struct DoctorState {
    slow: Mutex<Option<(Instant, SlowFacts)>>,
    oom: Mutex<(Option<u64>, Option<SystemTime>)>,
}

impl DoctorState {
    /// Record the OOM counter; returns hours since it last went up.
    pub fn observe_oom(&self, count: Option<u64>) -> Option<u64> {
        let mut g = self.oom.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = count {
            if g.0.is_some_and(|prev| c > prev) {
                g.1 = Some(SystemTime::now());
            }
            g.0 = Some(c);
        }
        g.1.and_then(|t| t.elapsed().ok()).map(|d| d.as_secs() / 3600)
    }
}

async fn psql_value(cfg: &Config, sql: &str) -> Option<String> {
    let out = run(
        &cfg.docker_bin,
        &[
            "exec",
            &cfg.coolify_db_container,
            "psql",
            "-U",
            "coolify",
            "-d",
            "coolify",
            "-At",
            "-c",
            sql,
        ],
        Duration::from_secs(15),
    )
    .await
    .ok()?;
    out.success().then(|| out.stdout.trim().to_string())
}

async fn smart_facts(cfg: &Config, snap: Option<&SystemSnapshot>) -> Vec<SmartFact> {
    let Some(snap) = snap else { return vec![] };
    let mut devices: Vec<String> = snap
        .disks
        .iter()
        .filter(|d| matches!(d.media, MediaKind::Ssd | MediaKind::Hdd))
        .filter_map(|d| vercelify_core::disks::parent_block_device(&d.device))
        .map(|p| format!("/dev/{p}"))
        .collect();
    devices.sort();
    devices.dedup();
    let mut out = Vec::new();
    for dev in devices {
        if let Ok(o) = run(&cfg.smartctl_bin, &["-H", "-A", "-j", &dev], Duration::from_secs(20)).await {
            // smartctl's exit code is a bit mask; the JSON is what counts.
            if let Some(f) = parse_smart(&dev, &o.stdout) {
                out.push(f);
            }
        }
    }
    out
}

async fn slow_facts(s: &AppState, role: &str, snap: Option<&SystemSnapshot>) -> SlowFacts {
    {
        let g = s.doctor.slow.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, f)) = g.as_ref() {
            if at.elapsed() < Duration::from_secs(3600) {
                return f.clone();
            }
        }
    }
    let cfg = &s.config;
    let mut f = SlowFacts {
        smart: smart_facts(cfg, snap).await,
        ..Default::default()
    };
    if role != "worker" {
        f.schema_missing = coolify::schema_missing(cfg).await.ok();
        f.auto_update = coolify::auto_update_enabled(cfg).await;
        f.ssh_user_ok = psql_value(cfg, "select coalesce(\"user\",'') from servers where id=0")
            .await
            .map(|u| !u.is_empty());
        f.wildcard_set = psql_value(
            cfg,
            "select coalesce(wildcard_domain,'') from server_settings where server_id=0",
        )
        .await
        .map(|w| !w.is_empty());
    }
    *s.doctor.slow.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), f.clone()));
    f
}

/// Forget cached slow facts (after an upgrade or a fix).
pub fn invalidate(s: &AppState) {
    *s.doctor.slow.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub fn read_backup(cfg: &Config) -> Option<BackupFact> {
    let path = cfg.state_dir.join("backup.json");
    let mut b: BackupFact = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    b.age_hours = b
        .finished_at
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_hours().max(0) as u64);
    Some(b)
}

pub async fn gather(s: &AppState) -> Facts {
    let cfg = &s.config;
    let role = cfg.role();
    let collector = s.collector.clone();
    let snap: Option<SystemSnapshot> = tokio::task::spawn_blocking(move || collector.snapshot(None)).await.ok();
    let docker = match run(
        &cfg.docker_bin,
        &["info", "--format", "{{.ServerVersion}}"],
        Duration::from_secs(15),
    )
    .await
    {
        Ok(o) if o.success() && !o.stdout.trim().is_empty() => Ok(o.stdout.trim().to_string()),
        Ok(o) => Err(format!("not running: {}", o.stderr.trim().lines().last().unwrap_or(""))),
        Err(e) => Err(e.to_string()),
    };
    let ts = tailscale::status(cfg.tailscale_bin.as_deref()).await;
    let main = role != "worker";
    let (coolify_healthy, coolify_token_ok) = if main { coolify_facts(cfg).await } else { (None, None) };
    let slow = slow_facts(s, &role, snap.as_ref()).await;
    let proxy_health = if main {
        run(
            &cfg.docker_bin,
            &[
                "inspect",
                "-f",
                "{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}",
                "coolify-proxy",
            ],
            Duration::from_secs(10),
        )
        .await
        .ok()
        .map(|o| {
            if o.success() {
                o.stdout.trim().to_string()
            } else {
                "missing".into()
            }
        })
    } else {
        None
    };
    let tunnel_active = if cfg.systemd_dir.join("cloudflared.service").exists() {
        Some(services::status(&cfg.systemctl_bin, "cloudflared").await.active_state == "active")
    } else {
        None
    };
    let clock_synced = run(
        &cfg.timedatectl_bin,
        &["show", "-p", "NTPSynchronized", "--value"],
        Duration::from_secs(5),
    )
    .await
    .ok()
    .filter(|o| o.success())
    .map(|o| o.stdout.trim() == "yes");
    let llm = s.llm.status().await;
    let pg: Option<vercelify_core::PowerGuardState> = std::fs::read_to_string(&cfg.power_guard_state)
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok());
    let active = |st: vercelify_core::ServiceStatus| st.active_state == "active";
    let hardware = snap.as_ref().and_then(|s| s.hardware.clone());
    let profile = hardware
        .as_ref()
        .map(|h| h.profile.clone())
        .or_else(|| cfg.install_value("profile"))
        .unwrap_or_else(|| "laptop".into());
    Facts {
        role: role.clone(),
        docker,
        tailscale_state: ts.backend_state.clone(),
        tailscale_name: ts.self_node.as_ref().map(|n| n.dns_name.clone()).unwrap_or_default(),
        key_expiry_days: ts
            .self_node
            .as_ref()
            .and_then(|n| n.key_expiry.as_deref())
            .and_then(|k| days_until(k, chrono::Utc::now())),
        system_disk: snap
            .as_ref()
            .and_then(|s| s.disks.iter().find(|d| d.mount_point == "/"))
            .map(|d| (d.used_bytes, d.total_bytes)),
        storage_expected: fstab_has_storage(&cfg.fstab_file, &cfg.storage_mount),
        storage_mounted: volumes::is_mount_point(Path::new(&cfg.storage_mount)),
        memory: snap
            .as_ref()
            .map(|s| (s.memory.total_bytes, s.memory.available_bytes))
            .unwrap_or_default(),
        max_temp_c: snap.as_ref().and_then(|s| s.thermal.max_c),
        on_battery: snap.as_ref().is_some_and(|s| s.power.on_battery),
        battery_pct: snap.as_ref().and_then(|s| s.power.lowest_capacity_pct),
        power_guard: pg.map(|p| p.state),
        firewall_active: active(services::status(&cfg.systemctl_bin, "vercelify-firewall").await),
        power_guard_active: active(services::status(&cfg.systemctl_bin, "vercelify-power-guard").await),
        coolify_healthy,
        platform: match (&s.platform, &s.platform_error) {
            (Some(p), _) => Some(p.store.schema_version().map_err(|e| e.to_string())),
            (None, Some(e)) => Some(Err(e.0.clone())),
            (None, None) => None,
        },
        coolify_token_ok,
        coolify_schema_missing: slow.schema_missing,
        coolify_auto_update: slow.auto_update,
        coolify_ssh_user_ok: slow.ssh_user_ok,
        proxy_health,
        wildcard_set: slow.wildcard_set,
        tunnel_active,
        lid_handled: (profile == "laptop").then(|| cfg.logind_dropin.exists()),
        ollama_ready: llm.installed.then_some(llm.ready),
        autodeploy_errors: s
            .autodeploy
            .status()
            .await
            .watches
            .iter()
            .filter(|w| w.enabled && w.last_error.is_some())
            .count(),
        oom_recent_hours: s.doctor.observe_oom(hardware.as_ref().and_then(|h| h.oom_kills)),
        hardware,
        clock_synced,
        reboot_required: cfg.reboot_required_file.exists(),
        reboot_window: cfg.maintenance_window(),
        reboot_skipped: read_maintenance(cfg)
            .filter(|(result, _)| result == "skipped")
            .map(|(_, detail)| {
                detail
                    .split_once("not now: ")
                    .map_or(detail.clone(), |(_, w)| w.to_string())
            }),
        smart: slow.smart,
        backup: read_backup(cfg),
        backup_setup_hours: std::fs::metadata(&cfg.backup_env)
            .and_then(|m| m.modified())
            .ok()
            .map(|t| t.elapsed().map_or(0, |d| d.as_secs() / 3600)),
        thresholds: Thresholds::from_config(cfg, &profile),
    }
}

/// Last night's reboot-window check: (result, detail) from
/// `maintenance.json`, if it ran in the last 30 hours.
pub fn read_maintenance(cfg: &Config) -> Option<(String, String)> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.state_dir.join("maintenance.json")).ok()?).ok()?;
    let at = chrono::DateTime::parse_from_rfc3339(v["at"].as_str()?).ok()?;
    if (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_hours() > 30 {
        return None;
    }
    Some((
        v["result"].as_str()?.to_string(),
        v["detail"].as_str().unwrap_or("").to_string(),
    ))
}

pub async fn report(s: &AppState) -> DoctorReport {
    let facts = gather(s).await;
    DoctorReport {
        role: facts.role.clone(),
        agent_version: env!("CARGO_PKG_VERSION").into(),
        checks: evaluate(&facts),
    }
}

#[cfg(test)]
mod maintenance_tests {
    use super::*;

    #[test]
    fn only_last_nights_reboot_check_counts() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            state_dir: dir.path().into(),
            ..Config::default()
        };
        assert_eq!(read_maintenance(&cfg), None);
        let at = |h: i64| {
            (chrono::Utc::now() - chrono::Duration::hours(h)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };
        let write = |at: String| {
            std::fs::write(
                dir.path().join("maintenance.json"),
                format!(r#"{{"result":"skipped","detail":"updates; not now: a deployment is building","at":"{at}"}}"#),
            )
            .unwrap()
        };
        write(at(5));
        assert_eq!(read_maintenance(&cfg).unwrap().0, "skipped");
        write(at(72));
        assert_eq!(read_maintenance(&cfg), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vercelify_core::{Pressure, Throttling};
    use CheckStatus::{Bad, Ok as Fine, Warn};

    fn laptop_hw() -> HardwareInfo {
        HardwareInfo {
            profile: "laptop".into(),
            model: String::new(),
            root_media: MediaKind::Ssd,
            memory_cgroup: true,
            throttling: None,
            pressure: None,
            oom_kills: Some(0),
            watchdog: true,
            defaults_pause_c: 92.0,
        }
    }

    pub(crate) fn healthy_main() -> Facts {
        Facts {
            role: "main".into(),
            docker: Ok("27.3.1".into()),
            tailscale_state: "Running".into(),
            tailscale_name: "laptop.tail1234.ts.net".into(),
            key_expiry_days: None,
            system_disk: Some((40 << 30, 238 << 30)),
            storage_expected: true,
            storage_mounted: true,
            memory: (8 << 30, 3 << 30),
            max_temp_c: Some(52.0),
            on_battery: false,
            battery_pct: Some(80),
            power_guard: Some("ok".into()),
            firewall_active: true,
            power_guard_active: true,
            coolify_healthy: Some(true),
            platform: None,
            coolify_token_ok: Some(true),
            coolify_schema_missing: Some(vec![]),
            coolify_auto_update: Some(false),
            coolify_ssh_user_ok: Some(true),
            proxy_health: Some("healthy".into()),
            wildcard_set: Some(true),
            tunnel_active: None,
            lid_handled: Some(true),
            ollama_ready: None,
            autodeploy_errors: 0,
            hardware: Some(laptop_hw()),
            oom_recent_hours: None,
            clock_synced: Some(true),
            reboot_required: false,
            reboot_window: Some("04:30".into()),
            reboot_skipped: None,
            smart: vec![SmartFact {
                device: "/dev/sda".into(),
                passed: true,
                reallocated: Some(0),
            }],
            backup: Some(BackupFact {
                ok: true,
                finished_at: Some("2026-10-20T03:00:00Z".into()),
                error: None,
                age_hours: Some(5),
            }),
            backup_setup_hours: Some(24 * 30),
            thresholds: Thresholds {
                pause_c: 92.0,
                resume_c: 80.0,
                battery_shutdown_pct: 15,
            },
        }
    }

    fn status_of(checks: &[DoctorCheck], id: &str) -> Option<CheckStatus> {
        checks.iter().find(|c| c.id == id).map(|c| c.status)
    }

    #[test]
    fn healthy_machine_is_all_ok_without_fixes() {
        let c = evaluate(&healthy_main());
        assert!(c.iter().all(|c| c.status == Fine), "{c:#?}");
        assert!(c.iter().all(|c| c.fix.is_none()));
        assert_eq!(status_of(&c, "ollama"), None, "no AI check when Ollama isn't installed");
        assert_eq!(status_of(&c, "smart-sda"), Some(Fine));
    }

    #[test]
    fn platform_database_check() {
        let none = evaluate(&healthy_main());
        assert!(
            !none.iter().any(|c| c.id == "platform"),
            "not shown on the Coolify engine"
        );
        let ok = evaluate(&Facts {
            platform: Some(Ok(1)),
            ..healthy_main()
        });
        let c = ok.iter().find(|c| c.id == "platform").unwrap();
        assert_eq!(c.status, CheckStatus::Ok);
        let bad = evaluate(&Facts {
            platform: Some(Err("the platform database is damaged".into())),
            ..healthy_main()
        });
        let c = bad.iter().find(|c| c.id == "platform").unwrap();
        assert_eq!(c.status, CheckStatus::Bad);
        assert!(c.fix.as_deref().unwrap().contains("restore-state"));
    }

    #[test]
    fn problems_come_with_fixes() {
        let f = Facts {
            docker: Err("not running: Cannot connect to the Docker daemon".into()),
            tailscale_state: "NeedsLogin".into(),
            key_expiry_days: Some(3),
            storage_mounted: false,
            system_disk: Some((230 << 30, 238 << 30)),
            memory: (8 << 30, 300 << 20),
            max_temp_c: Some(85.0),
            on_battery: true,
            firewall_active: false,
            coolify_token_ok: None,
            coolify_schema_missing: Some(vec!["servers.user".into()]),
            coolify_auto_update: Some(true),
            coolify_ssh_user_ok: Some(false),
            proxy_health: Some("unhealthy".into()),
            wildcard_set: Some(false),
            tunnel_active: Some(false),
            lid_handled: Some(false),
            ollama_ready: Some(false),
            autodeploy_errors: 2,
            oom_recent_hours: Some(2),
            clock_synced: Some(false),
            reboot_required: true,
            reboot_window: None,
            reboot_skipped: None,
            smart: vec![SmartFact {
                device: "/dev/sdb".into(),
                passed: true,
                reallocated: Some(8),
            }],
            backup: Some(BackupFact {
                ok: false,
                error: Some("repository locked".into()),
                ..Default::default()
            }),
            ..healthy_main()
        };
        let c = evaluate(&f);
        for (id, want) in [
            ("docker", Bad),
            ("tailscale", Bad),
            ("tailscale-key", Warn),
            ("storage", Bad),
            ("disk", Bad),
            ("memory", Bad),
            ("temperature", Warn),
            ("power", Warn),
            ("firewall", Warn),
            ("coolify-token", Warn),
            ("coolify-schema", Bad),
            ("coolify-autoupdate", Warn),
            ("coolify-ssh", Bad),
            ("proxy", Bad),
            ("wildcard", Warn),
            ("tunnel", Bad),
            ("lid", Warn),
            ("ollama", Warn),
            ("oom", Warn),
            ("clock", Warn),
            ("reboot", Warn),
            ("smart-sdb", Warn),
            ("backup", Bad),
        ] {
            assert_eq!(status_of(&c, id), Some(want), "{id}");
        }
        assert!(c
            .iter()
            .find(|c| c.id == "autodeploy")
            .unwrap()
            .detail
            .starts_with("2 branch watches"));
        assert!(c.iter().filter(|c| c.status != Fine).all(|c| c.fix.is_some()));
        assert!(c
            .iter()
            .find(|c| c.id == "power")
            .unwrap()
            .fix
            .as_deref()
            .unwrap()
            .contains("15%"));
    }

    #[test]
    fn workers_skip_coolify_and_backups_and_single_disk_is_a_warning() {
        let f = Facts {
            role: "worker".into(),
            storage_expected: false,
            storage_mounted: false,
            coolify_healthy: None,
            backup: None,
            ..healthy_main()
        };
        let c = evaluate(&f);
        assert_eq!(status_of(&c, "coolify"), None);
        assert_eq!(status_of(&c, "coolify-token"), None);
        assert_eq!(status_of(&c, "backup"), None);
        assert_eq!(status_of(&c, "storage"), Some(Warn));
    }

    #[test]
    fn main_without_backups_is_warned() {
        let c = evaluate(&Facts {
            backup: None,
            backup_setup_hours: None,
            ..healthy_main()
        });
        assert_eq!(status_of(&c, "backup"), Some(Warn));
    }

    #[test]
    fn backups_set_up_by_the_installer_are_fine_until_the_first_run_is_overdue() {
        let fresh = evaluate(&Facts {
            backup: None,
            backup_setup_hours: Some(1),
            ..healthy_main()
        });
        assert_eq!(status_of(&fresh, "backup"), Some(Fine));
        let stale = evaluate(&Facts {
            backup: None,
            backup_setup_hours: Some(24 * 5),
            ..healthy_main()
        });
        assert_eq!(status_of(&stale, "backup"), Some(Warn));
        assert!(stale
            .iter()
            .any(|c| c.id == "backup" && c.detail.contains("5 days ago")));
    }

    #[test]
    fn raspberry_pi_checks() {
        let pi = HardwareInfo {
            profile: "pi4".into(),
            model: "Raspberry Pi 4 Model B".into(),
            root_media: MediaKind::SdCard,
            memory_cgroup: false,
            throttling: Some(Throttling {
                raw: 0x50005,
                under_voltage_now: true,
                throttled_now: true,
                soft_temp_limit_now: false,
                under_voltage_since_boot: true,
                throttled_since_boot: true,
            }),
            pressure: Some(Pressure {
                cpu_some_avg10: None,
                memory_some_avg10: Some(50.0),
                io_some_avg10: None,
            }),
            oom_kills: Some(1),
            watchdog: true,
            defaults_pause_c: 78.0,
        };
        let f = Facts {
            role: "worker".into(),
            hardware: Some(pi),
            max_temp_c: Some(80.0),
            thresholds: Thresholds {
                pause_c: 78.0,
                resume_c: 68.0,
                battery_shutdown_pct: 15,
            },
            ..healthy_main()
        };
        let c = evaluate(&f);
        assert_eq!(status_of(&c, "sd-card"), Some(Warn));
        assert_eq!(status_of(&c, "memory-limits"), Some(Bad));
        assert_eq!(status_of(&c, "power-supply"), Some(Bad));
        assert_eq!(status_of(&c, "memory"), Some(Bad), "memory pressure counts");
        assert_eq!(
            status_of(&c, "temperature"),
            Some(Bad),
            "Pi thresholds, not laptop ones"
        );
    }

    #[test]
    fn thresholds_from_power_guard_conf() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("pg.conf");
        std::fs::write(&conf, "THERMAL_PAUSE_C=70\nBATTERY_SHUTDOWN_PCT=25\n").unwrap();
        let cfg = Config {
            power_guard_conf: conf,
            ..Config::default()
        };
        let t = Thresholds::from_config(&cfg, "laptop");
        assert_eq!(t.pause_c, 70.0);
        assert_eq!(t.resume_c, 80.0, "missing keys fall back to the profile");
        assert_eq!(t.battery_shutdown_pct, 25);
        let pi = Thresholds::from_config(&Config::default(), "pi5");
        assert_eq!(pi.pause_c, 78.0);
    }

    #[test]
    fn key_expiry_days() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-20T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(days_until("2026-10-30T12:00:00Z", now), Some(10));
        assert_eq!(days_until("2026-10-19T12:00:00Z", now), Some(-1));
        assert_eq!(days_until("nonsense", now), None);
        let c = evaluate(&Facts {
            key_expiry_days: Some(-1),
            ..healthy_main()
        });
        assert_eq!(status_of(&c, "tailscale-key"), Some(Bad));
    }

    #[test]
    fn smart_json() {
        let j = r#"{"smart_status":{"passed":true},"ata_smart_attributes":{"table":[{"id":5,"name":"Reallocated_Sector_Ct","raw":{"value":12}}]}}"#;
        let f = parse_smart("/dev/sda", j).unwrap();
        assert!(f.passed);
        assert_eq!(f.reallocated, Some(12));
        let nvme = r#"{"smart_status":{"passed":false}}"#;
        assert!(!parse_smart("/dev/nvme0n1", nvme).unwrap().passed);
        assert!(parse_smart("/dev/sda", "{}").is_none());
    }

    #[test]
    fn oom_tracking() {
        let d = DoctorState::default();
        assert_eq!(d.observe_oom(Some(3)), None, "first reading is the baseline");
        assert_eq!(d.observe_oom(Some(3)), None);
        assert_eq!(d.observe_oom(Some(4)), Some(0), "went up just now");
    }

    #[test]
    fn fstab_marker() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("fstab");
        std::fs::write(
            &f,
            "UUID=1 / ext4 defaults 0 1\n# vercelify /mnt/storage\nUUID=2 /mnt/storage ext4 nofail 0 2\n",
        )
        .unwrap();
        assert!(fstab_has_storage(&f, "/mnt/storage"));
        assert!(!fstab_has_storage(&f, "/srv"));
        assert!(!fstab_has_storage(&dir.path().join("none"), "/mnt/storage"));
    }
}

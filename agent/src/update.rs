//! `vercelify update` started from the cockpit.
//!
//! The update re-runs the installer, which restarts this agent, so it runs
//! as its own transient systemd unit (outside the agent's sandbox) and
//! leaves its progress in files the restarted agent can read back.

use crate::config::Config;
use crate::error::AppError;
use crate::exec::run;
use std::path::PathBuf;
use std::time::Duration;
use vercelify_core::UpdateStatus;

pub const UNIT: &str = "vercelify-update";
const LOG_LINES: usize = 40;

struct Files {
    started: PathBuf,
    exit: PathBuf,
    log: PathBuf,
}

fn files(cfg: &Config) -> Files {
    Files {
        started: cfg.state_dir.join("update.started"),
        exit: cfg.state_dir.join("update.exit"),
        log: cfg.state_dir.join("update.log"),
    }
}

/// Last `n` non-empty lines, with terminal colour codes stripped.
pub fn tail(text: &str, n: usize) -> Vec<String> {
    let clean = |l: &str| {
        let mut out = String::with_capacity(l.len());
        let mut chars = l.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                // ESC [ ... final byte
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for d in chars.by_ref() {
                        if d.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                continue;
            }
            if c != '\r' {
                out.push(c);
            }
        }
        out
    };
    let lines: Vec<String> = text.lines().map(clean).filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

/// Empty the update log, readable by root only (it carries the installer's
/// output), whatever mode an older agent left it with.
fn reset_log(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

async fn unit_active(cfg: &Config) -> bool {
    run(
        &cfg.systemctl_bin,
        &["is-active", "--quiet", UNIT],
        Duration::from_secs(10),
    )
    .await
    .is_ok_and(|o| o.success())
}

pub async fn status(cfg: &Config) -> UpdateStatus {
    let f = files(cfg);
    UpdateStatus {
        running: unit_active(cfg).await,
        started_at: std::fs::read_to_string(&f.started).ok().map(|s| s.trim().to_string()),
        exit_code: std::fs::read_to_string(&f.exit)
            .ok()
            .and_then(|s| s.trim().parse().ok()),
        log_tail: tail(&std::fs::read_to_string(&f.log).unwrap_or_default(), LOG_LINES),
    }
}

pub async fn start(cfg: &Config) -> Result<UpdateStatus, AppError> {
    if unit_active(cfg).await {
        return Err(AppError::conflict("an update is already running on this server"));
    }
    if crate::exec::find_on_path(&cfg.cli_bin).is_none() {
        return Err(AppError::unavailable(format!(
            "{} is missing — re-run the installer on this server",
            cfg.cli_bin
        )));
    }
    let f = files(cfg);
    std::fs::create_dir_all(&cfg.state_dir).map_err(|e| AppError::internal(e.to_string()))?;
    let _ = std::fs::remove_file(&f.exit);
    reset_log(&f.log).map_err(|e| AppError::internal(e.to_string()))?;
    std::fs::write(
        &f.started,
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
    .map_err(|e| AppError::internal(e.to_string()))?;
    // A previous failed run would block the unit name.
    let _ = run(&cfg.systemctl_bin, &["reset-failed", UNIT], Duration::from_secs(10)).await;
    let script = format!(
        "{cli} update >>{log} 2>&1; echo $? >{exit}",
        cli = sh_quote(&cfg.cli_bin),
        log = sh_quote(&f.log.to_string_lossy()),
        exit = sh_quote(&f.exit.to_string_lossy()),
    );
    let unit = format!("--unit={UNIT}");
    let out = run(
        &cfg.systemd_run_bin,
        &[
            &unit,
            "--description=Vercelify update (started from the cockpit)",
            "--setenv=VERCELIFY_NONINTERACTIVE=1",
            "/bin/sh",
            "-c",
            &script,
        ],
        Duration::from_secs(20),
    )
    .await?;
    if !out.success() {
        return Err(AppError::internal(format!("systemd-run failed: {}", out.stderr.trim())));
    }
    Ok(status(cfg).await)
}

pub const BACKUP_UNIT: &str = "vercelify-backup.service";

/// Backup targets from backup.env, with credentials masked.
pub fn backup_targets(env: &str) -> Vec<String> {
    env.lines()
        .filter_map(|l| l.trim().strip_prefix("BACKUP_REPOS="))
        .flat_map(|v| {
            v.trim_matches(['"', '\''])
                .split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .map(|r| mask_repo(&r))
        .collect()
}

/// `sftp:root@pi:/mnt/storage/backups` stays readable; S3 keys and URL
/// passwords are hidden.
pub fn mask_repo(r: &str) -> String {
    match r.split_once("://") {
        Some((scheme, rest)) => match rest.split_once('@') {
            Some((_, host)) => format!("{scheme}://•••@{host}"),
            None => r.to_string(),
        },
        None => r.to_string(),
    }
}

pub async fn backup_status(cfg: &Config) -> vercelify_core::BackupStatus {
    let env = std::fs::read_to_string(&cfg.backup_env).ok();
    let last = crate::doctor::read_backup(cfg);
    vercelify_core::BackupStatus {
        configured: env.is_some(),
        targets: env.as_deref().map(backup_targets).unwrap_or_default(),
        last_ok: last.as_ref().map(|b| b.ok),
        last_finished_at: last.as_ref().and_then(|b| b.finished_at.clone()),
        last_error: last.and_then(|b| b.error),
        running: run(
            &cfg.systemctl_bin,
            &["is-active", "--quiet", BACKUP_UNIT],
            Duration::from_secs(10),
        )
        .await
        .is_ok_and(|o| o.success()),
        log_tail: tail(
            &std::fs::read_to_string(cfg.state_dir.join("backup.log")).unwrap_or_default(),
            LOG_LINES,
        ),
    }
}

pub async fn backup_start(cfg: &Config) -> Result<vercelify_core::BackupStatus, AppError> {
    if !cfg.backup_env.exists() || !cfg.systemd_dir.join(BACKUP_UNIT).exists() {
        return Err(AppError::conflict(
            "backups are not set up on this server — run: sudo vercelify backup setup",
        ));
    }
    let out = run(
        &cfg.systemctl_bin,
        &["start", "--no-block", BACKUP_UNIT],
        Duration::from_secs(20),
    )
    .await?;
    if !out.success() {
        return Err(AppError::internal(format!(
            "starting the backup: {}",
            out.stderr.trim()
        )));
    }
    Ok(backup_status(cfg).await)
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Follow a systemd unit started for a job until it stops, then close the
/// job with `result()` (read from the files the unit leaves behind).
pub async fn follow_unit(
    cfg: std::sync::Arc<Config>,
    unit: &'static str,
    job_id: String,
    jobs: std::sync::Arc<crate::events::Jobs>,
    result: fn(&Config, &str) -> Result<String, String>,
) {
    let started_at = jobs.get(&job_id).map(|j| j.started_at).unwrap_or_default();
    // Give systemd a moment to report the unit as active.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let started = std::time::Instant::now();
    loop {
        let active = run(
            &cfg.systemctl_bin,
            &["is-active", "--quiet", unit],
            Duration::from_secs(10),
        )
        .await
        .is_ok_and(|o| o.success());
        if !active {
            break;
        }
        if started.elapsed() > Duration::from_secs(6 * 3600) {
            jobs.finish_by_id(&job_id, Err("still running after 6 hours".into()));
            return;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    jobs.finish_by_id(&job_id, result(&cfg, &started_at));
}

/// The result `vercelify backup run` wrote, if it is from this run (started
/// at `started_at`, RFC 3339 like `finished_at`, so they compare as text).
pub fn backup_result(cfg: &Config, started_at: &str) -> Result<String, String> {
    match crate::doctor::read_backup(cfg) {
        // A result from an earlier run: this one wrote none.
        Some(b) if b.finished_at.as_deref().is_none_or(|f| f < started_at) => {
            Err("the backup left no result (see: sudo vercelify logs backup)".into())
        }
        Some(b) if b.ok => Ok("backed up".into()),
        Some(b) => Err(b.error.unwrap_or_else(|| "the backup failed".into())),
        None => Err("the backup left no result (see: sudo vercelify logs backup)".into()),
    }
}

pub fn update_result(cfg: &Config, _started_at: &str) -> Result<String, String> {
    let f = files(cfg);
    match std::fs::read_to_string(&f.exit)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
    {
        Some(0) => Ok(format!("now on {}", crate::VERSION)),
        Some(code) => Err(format!(
            "the update failed (exit {code}): {}",
            tail(&std::fs::read_to_string(&f.log).unwrap_or_default(), 1).join("")
        )),
        None => Err("the update left no result".into()),
    }
}

/// An update restarts this agent; pick its job up again after the restart.
pub async fn resume_update_job(cfg: std::sync::Arc<Config>, jobs: std::sync::Arc<crate::events::Jobs>) {
    if let Some(j) = jobs.running("update", None) {
        follow_unit(cfg, UNIT, j.id, jobs, update_result).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_update_log_is_emptied_and_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("update.log");
        std::fs::write(&log, "old run with a code").unwrap();
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
        reset_log(&log).unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
        assert_eq!(std::fs::metadata(&log).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_backup_job_only_counts_its_own_result() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            state_dir: dir.path().into(),
            ..Config::default()
        };
        let started = "2026-10-21T03:17:00Z";
        assert!(backup_result(&cfg, started).is_err(), "no result at all");
        let write = |json: &str| std::fs::write(dir.path().join("backup.json"), json).unwrap();
        write(r#"{"ok":true,"finished_at":"2026-10-20T03:20:00Z"}"#);
        assert!(
            backup_result(&cfg, started).unwrap_err().contains("no result"),
            "last night's"
        );
        write(r#"{"ok":true,"finished_at":"2026-10-21T03:21:09Z"}"#);
        assert_eq!(backup_result(&cfg, started).unwrap(), "backed up");
        write(r#"{"ok":false,"finished_at":"2026-10-21T03:21:09Z","error":"repository locked"}"#);
        assert_eq!(backup_result(&cfg, started).unwrap_err(), "repository locked");
    }

    #[test]
    fn tails_and_strips_colours() {
        let log = "a\n\u{1b}[32m✓\u{1b}[0m docker ready\r\n\n  \nlast\n";
        assert_eq!(tail(log, 2), vec!["✓ docker ready", "last"]);
        assert_eq!(tail(log, 10).len(), 3);
        assert!(tail("", 5).is_empty());
    }

    #[test]
    fn backup_targets_are_masked() {
        let env = "BACKUP_PASSWORD=x\nBACKUP_REPOS=\"/mnt/storage/backups/restic sftp:root@pi:/mnt/storage/backups/laptop s3:https://KEY:SECRET@s3.example.com/b\"\n";
        assert_eq!(
            backup_targets(env),
            vec![
                "/mnt/storage/backups/restic".to_string(),
                "sftp:root@pi:/mnt/storage/backups/laptop".to_string(),
                "s3:https://•••@s3.example.com/b".to_string()
            ]
        );
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("/usr/local/bin/vercelify"), "'/usr/local/bin/vercelify'");
        assert_eq!(sh_quote("a'b"), r"'a'\''b'");
    }
}

//! systemd unit status/control, restricted to an allowlist from config.

use crate::error::AppError;
use crate::exec::{run, ExecError};
use std::time::Duration;
use vercelify_core::ServiceStatus;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Sunshine runs as the desktop user's own unit (upstream's way: it needs
/// the session's display, audio and input), reached with `--user -M USER@`.
fn scope(desktop_user: Option<&str>, unit: &str) -> Vec<String> {
    match desktop_user {
        Some(u)
            if unit == "sunshine"
                && !u.is_empty()
                && u.len() <= 32
                && u.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
                && !u.starts_with('-') =>
        {
            vec!["--user".into(), "-M".into(), format!("{u}@")]
        }
        _ => vec![],
    }
}

pub async fn status(systemctl: &str, unit: &str) -> ServiceStatus {
    status_as(systemctl, None, unit).await
}

/// `status`, with the desktop user's units for those that are theirs.
pub async fn status_as(systemctl: &str, desktop_user: Option<&str>, unit: &str) -> ServiceStatus {
    let mut args = scope(desktop_user, unit);
    args.extend(["show", "-p", "ActiveState,LoadState,UnitFileState", "--", unit].map(String::from));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let active = match run(systemctl, &args, TIMEOUT).await {
        Ok(o) if o.success() => o.stdout,
        Ok(_) | Err(ExecError::Timeout(..)) | Err(ExecError::Io(..)) => String::new(),
        Err(ExecError::NotFound(_)) => {
            return ServiceStatus {
                name: unit.into(),
                active_state: "unknown".into(),
                enabled: None,
            }
        }
    };
    let field = |k: &str| {
        active
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or("")
            .to_string()
    };
    let load = field("LoadState");
    let active_state = if load == "not-found" {
        "not-found".to_string()
    } else {
        let s = field("ActiveState");
        if s.is_empty() {
            "unknown".into()
        } else {
            s
        }
    };
    let enabled = match field("UnitFileState").as_str() {
        "enabled" | "enabled-runtime" | "static" | "alias" => Some(true),
        "disabled" | "masked" => Some(false),
        _ => None,
    };
    ServiceStatus {
        name: unit.into(),
        active_state,
        enabled,
    }
}

pub async fn control(
    systemctl: &str,
    desktop_user: Option<&str>,
    allowed: &[String],
    unit: &str,
    action: &str,
) -> Result<(), AppError> {
    if !allowed.iter().any(|u| u == unit) {
        return Err(AppError::not_found(format!(
            "unit `{unit}` is not in the agent's service allowlist"
        )));
    }
    if !matches!(action, "start" | "stop" | "restart") {
        return Err(AppError::bad_request(format!("unsupported action `{action}`")));
    }
    if unit == "vercelify-agent" && action != "restart" {
        return Err(AppError::bad_request(
            "the agent can only be restarted, not stopped, from the cockpit",
        ));
    }
    if unit == "vercelify-agent" {
        // Respond before systemd kills us.
        let systemctl = systemctl.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = run(&systemctl, &["restart", "vercelify-agent"], Duration::from_secs(30)).await;
        });
        return Ok(());
    }
    let mut args = scope(desktop_user, unit);
    args.extend([action, "--", unit].map(String::from));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(systemctl, &args, Duration::from_secs(60)).await?;
    if out.success() {
        Ok(())
    } else {
        Err(AppError::conflict(format!(
            "systemctl {action} {unit}: {}",
            out.stderr.trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::scope;

    #[test]
    fn sunshine_is_the_desktop_users() {
        assert_eq!(scope(Some("ana"), "sunshine"), ["--user", "-M", "ana@"]);
        assert!(scope(Some("ana"), "docker").is_empty());
        assert!(scope(None, "sunshine").is_empty());
        for bad in ["", "-x", "Ana", "a b", "a@b", "a;b"] {
            assert!(scope(Some(bad), "sunshine").is_empty(), "{bad}");
        }
    }
}

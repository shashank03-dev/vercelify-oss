use crate::exec::{find_on_path, run, ExecError};
use std::time::Duration;
use vercelify_core::tailscale::{parse_status, unavailable, CLI_CANDIDATES};
use vercelify_core::TailscaleStatus;

pub fn cli(configured: Option<&str>) -> Option<String> {
    match configured {
        Some(c) => Some(c.to_string()),
        None => CLI_CANDIDATES
            .iter()
            .find_map(|c| find_on_path(c))
            .map(|p| p.to_string_lossy().into_owned()),
    }
}

pub async fn status(configured: Option<&str>) -> TailscaleStatus {
    let Some(bin) = cli(configured) else {
        return unavailable(false, "tailscale CLI not found");
    };
    match run(&bin, &["status", "--json"], Duration::from_secs(8)).await {
        Err(ExecError::NotFound(_)) => unavailable(false, "tailscale CLI not found"),
        Err(e) => unavailable(true, e.to_string()),
        // `tailscale status --json` prints valid JSON even when logged out
        // (exit code 1), so try to parse before looking at the status.
        Ok(o) => match parse_status(&o.stdout) {
            Ok(s) => s,
            Err(_) if !o.success() => unavailable(true, o.stderr.trim().to_string()),
            Err(e) => unavailable(true, format!("unexpected output: {e}")),
        },
    }
}

/// This node's Tailscale IPv4, if connected.
pub async fn ipv4(configured: Option<&str>) -> Option<String> {
    let bin = cli(configured)?;
    let o = run(&bin, &["ip", "-4"], Duration::from_secs(5)).await.ok()?;
    o.success()
        .then(|| o.stdout.lines().next().map(|l| l.trim().to_string()))
        .flatten()
        .filter(|s| !s.is_empty())
}

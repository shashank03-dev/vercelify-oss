//! Parsing of `tailscale status --json`.

use crate::model::{TailscaleNode, TailscaleStatus};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawStatus {
    version: String,
    backend_state: String,
    #[serde(rename = "AuthURL")]
    auth_url: String,
    #[serde(rename = "MagicDNSSuffix")]
    magic_dns_suffix: String,
    current_tailnet: Option<RawTailnet>,
    #[serde(rename = "Self")]
    self_node: Option<RawNode>,
    peer: Option<BTreeMap<String, RawNode>>,
    health: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawTailnet {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawNode {
    #[serde(rename = "ID")]
    id: String,
    host_name: String,
    #[serde(rename = "DNSName")]
    dns_name: String,
    #[serde(rename = "OS")]
    os: String,
    #[serde(rename = "TailscaleIPs")]
    tailscale_ips: Option<Vec<String>>,
    online: bool,
    active: bool,
    exit_node: bool,
    relay: String,
    cur_addr: String,
    rx_bytes: u64,
    tx_bytes: u64,
    last_seen: Option<String>,
    key_expiry: Option<String>,
}

impl From<RawNode> for TailscaleNode {
    fn from(n: RawNode) -> Self {
        TailscaleNode {
            id: n.id,
            hostname: n.host_name,
            dns_name: n.dns_name.trim_end_matches('.').to_string(),
            os: n.os,
            ips: n.tailscale_ips.unwrap_or_default(),
            online: n.online,
            active: n.active,
            exit_node: n.exit_node,
            relay: n.relay,
            cur_addr: n.cur_addr,
            rx_bytes: n.rx_bytes,
            tx_bytes: n.tx_bytes,
            // Go zero time means "never" / "currently connected".
            last_seen: n.last_seen.filter(|s| !s.starts_with("0001-")),
            key_expiry: n.key_expiry.filter(|s| !s.starts_with("0001-")),
        }
    }
}

pub fn parse_status(json: &str) -> Result<TailscaleStatus, serde_json::Error> {
    let raw: RawStatus = serde_json::from_str(json)?;
    let mut peers: Vec<TailscaleNode> = raw.peer.unwrap_or_default().into_values().map(Into::into).collect();
    // Online first, then alphabetical: what you want to see in a status list.
    peers.sort_by(|a, b| b.online.cmp(&a.online).then_with(|| a.hostname.cmp(&b.hostname)));
    Ok(TailscaleStatus {
        installed: true,
        backend_state: raw.backend_state,
        version: raw.version,
        tailnet: raw.current_tailnet.map(|t| t.name).unwrap_or_default(),
        magic_dns_suffix: raw.magic_dns_suffix,
        auth_url: raw.auth_url,
        self_node: raw.self_node.map(Into::into),
        peers,
        health: raw.health.unwrap_or_default(),
        error: None,
    })
}

/// Status object for when the CLI is missing or failed.
pub fn unavailable(installed: bool, error: impl Into<String>) -> TailscaleStatus {
    TailscaleStatus {
        installed,
        backend_state: if installed {
            "Unknown".into()
        } else {
            "NotInstalled".into()
        },
        error: Some(error.into()),
        ..Default::default()
    }
}

/// Well-known CLI locations, tried in order, for hosts where `tailscale` is
/// not on `PATH` (macOS app bundle).
pub const CLI_CANDIDATES: &[&str] = &[
    "tailscale",
    "/usr/bin/tailscale",
    "/usr/local/bin/tailscale",
    "/opt/homebrew/bin/tailscale",
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
];

/// First IPv4 address of this node, if any.
pub fn self_ipv4(status: &TailscaleStatus) -> Option<String> {
    status
        .self_node
        .as_ref()?
        .ips
        .iter()
        .find(|ip| ip.contains('.'))
        .cloned()
}

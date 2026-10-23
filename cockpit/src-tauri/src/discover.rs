//! Finding Vercelify hosts on the tailnet: every online Linux peer is asked
//! for `/v1/health` on the agent port (unauthenticated, tiny, 2 s timeout).

use std::time::Duration;
use vercelify_core::{DiscoveredServer, Health, TailscaleNode, TailscaleStatus, DEFAULT_AGENT_PORT};

/// Peers worth probing: online, not this machine, and not phones or Macs.
pub fn candidates(ts: &TailscaleStatus) -> Vec<&TailscaleNode> {
    ts.peers
        .iter()
        .filter(|p| p.online && !p.ips.is_empty())
        .filter(|p| {
            let os = p.os.to_ascii_lowercase();
            os.is_empty() || os == "linux"
        })
        .collect()
}

fn ipv4(node: &TailscaleNode) -> Option<&str> {
    node.ips
        .iter()
        .find(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok())
        .or_else(|| node.ips.first())
        .map(String::as_str)
}

pub async fn discover(client: &reqwest::Client, ts: TailscaleStatus) -> Vec<DiscoveredServer> {
    let mut set = tokio::task::JoinSet::new();
    for peer in candidates(&ts) {
        let Some(ip) = ipv4(peer).map(String::from) else {
            continue;
        };
        let host = if ip.contains(':') {
            format!("[{ip}]")
        } else {
            ip.clone()
        };
        let agent_url = format!("http://{host}:{DEFAULT_AGENT_PORT}");
        let client = client.clone();
        let peer = peer.clone();
        set.spawn(async move {
            let health: Health = client
                .get(format!("{agent_url}/v1/health"))
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()?;
            health.ok.then(|| DiscoveredServer {
                hostname: if health.hostname.is_empty() {
                    peer.hostname.clone()
                } else {
                    health.hostname
                },
                dns_name: peer.dns_name.clone(),
                ip,
                os: peer.os.clone(),
                agent_url,
                version: health.version,
                role: if health.role.is_empty() {
                    "main".into()
                } else {
                    health.role
                },
                pairing_open: health.pairing_open,
            })
        });
    }
    let mut found = Vec::new();
    while let Some(r) = set.join_next().await {
        if let Ok(Some(s)) = r {
            found.push(s);
        }
    }
    // Hosts waiting to be paired first, then by name.
    found.sort_by(|a, b| (!a.pairing_open, &a.hostname).cmp(&(!b.pairing_open, &b.hostname)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, os: &str, online: bool, ips: &[&str]) -> TailscaleNode {
        TailscaleNode {
            hostname: name.into(),
            os: os.into(),
            online,
            ips: ips.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn picks_online_linux_peers() {
        let ts = TailscaleStatus {
            peers: vec![
                node("laptop", "linux", true, &["fd7a:115c::1", "100.64.0.2"]),
                node("phone", "iOS", true, &["100.64.0.3"]),
                node("old", "linux", false, &["100.64.0.4"]),
                node("pi", "", true, &["100.64.0.5"]),
                node("mac", "macOS", true, &["100.64.0.6"]),
            ],
            ..Default::default()
        };
        let c = candidates(&ts);
        assert_eq!(
            c.iter().map(|n| n.hostname.as_str()).collect::<Vec<_>>(),
            ["laptop", "pi"]
        );
        assert_eq!(ipv4(c[0]), Some("100.64.0.2"), "IPv4 preferred");
    }
}

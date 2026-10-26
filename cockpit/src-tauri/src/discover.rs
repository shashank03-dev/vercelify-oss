//! Finding Vercelify servers among this computer's mesh peers: every server
//! peer is asked for `/v1/health` on the agent port (unauthenticated, tiny,
//! 2 s timeout).

use std::time::Duration;
use vercelify_core::{DiscoveredServer, Health, MeshLocal, MeshPeer, DEFAULT_AGENT_PORT};

/// Peers worth probing: servers with an IPv4 mesh address (devices such as
/// phones and other Macs run no agent).
pub fn candidates(local: &MeshLocal) -> Vec<(&MeshPeer, &str)> {
    local
        .peers
        .iter()
        .filter(|p| p.kind.as_deref().unwrap_or("server") == "server")
        .filter_map(|p| {
            p.addresses
                .iter()
                .find(|a| a.parse::<std::net::Ipv4Addr>().is_ok())
                .map(|ip| (p, ip.as_str()))
        })
        .collect()
}

pub async fn discover(client: &reqwest::Client, local: MeshLocal) -> Vec<DiscoveredServer> {
    let mut set = tokio::task::JoinSet::new();
    for (peer, ip) in candidates(&local) {
        let agent_url = format!("http://{ip}:{DEFAULT_AGENT_PORT}");
        let client = client.clone();
        let name = peer.name.clone();
        let ip = ip.to_string();
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
                    name
                } else {
                    health.hostname
                },
                ip,
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

    fn peer(name: &str, kind: Option<&str>, addresses: &[&str]) -> MeshPeer {
        MeshPeer {
            name: name.into(),
            kind: kind.map(String::from),
            addresses: addresses.iter().map(|s| s.to_string()).collect(),
            path: "lan".into(),
            endpoint: None,
            rtt_ms: None,
            handshake_secs: None,
        }
    }

    #[test]
    fn picks_server_peers() {
        let local = MeshLocal {
            peers: vec![
                peer("laptop", Some("server"), &["fd76:6679:6d65::2", "10.77.0.2"]),
                peer("phone", Some("device"), &["10.77.0.3"]),
                peer("pi", None, &["10.77.0.4"]),
                peer("v6only", Some("server"), &["fd76:6679:6d65::5"]),
            ],
            ..Default::default()
        };
        let c = candidates(&local);
        assert_eq!(
            c.iter().map(|(p, ip)| (p.name.as_str(), *ip)).collect::<Vec<_>>(),
            [("laptop", "10.77.0.2"), ("pi", "10.77.0.4")]
        );
    }
}

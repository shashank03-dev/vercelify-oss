//! The relay (design §13.7): when two nodes find no direct path, their
//! WireGuard (and disco) packets go through the coordinator over a
//! WebSocket, which also passes through a Cloudflare tunnel. The relay only
//! ever sees ciphertext.
//!
//! The connection is authenticated by the node's WireGuard key, not a
//! token: the relay sends a random challenge, the node answers it sealed to
//! the coordinator's key (`seal.rs`), which only the holder of the node's
//! private key can do. So a node can use the relay before it has any path
//! into the mesh, and a revoked or unknown key is refused.
//!
//! After `{"ok":true}`, every binary message is `peer key (32) ‖ packet`:
//! the destination from the node, the source from the relay.

use crate::cert::NodeKind;
use crate::keys::PublicKey;
use serde::{Deserialize, Serialize};

pub const RELAY_PATH: &str = "/v1/mesh/relay";
/// Packets waiting for one node; more are dropped (WireGuard retransmits
/// what matters, and a slow reader mustn't hold memory on the coordinator).
pub const QUEUE: usize = 256;
/// Largest message accepted: a key and a WireGuard packet for a 1280 MTU,
/// with room to spare.
pub const MAX_MESSAGE: usize = 32 + 4096;

/// The relay's first message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    pub challenge: String,
}

/// Sealed inside the node's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub challenge: String,
}

pub fn frame(key: &PublicKey, packet: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(32 + packet.len());
    f.extend_from_slice(&key.0);
    f.extend_from_slice(packet);
    f
}

pub fn split(frame: &[u8]) -> Option<(PublicKey, &[u8])> {
    if frame.len() <= 32 || frame.len() > MAX_MESSAGE {
        return None;
    }
    let mut k = [0u8; 32];
    k.copy_from_slice(&frame[..32]);
    Some((PublicKey(k), &frame[32..]))
}

/// Who may talk to whom through the relay: the same rule as the map, a
/// device sees servers only.
pub fn may_relay(from: NodeKind, to: NodeKind) -> bool {
    from == NodeKind::Server || to == NodeKind::Server
}

/// `ws://` URLs of the relay at the coordinator's control addresses
/// (`192.168.1.20:7443`), and an `https://` public URL as `wss://`.
pub fn relay_urls(control: &[String], public_url: Option<&str>) -> Vec<String> {
    let mut v: Vec<String> = control.iter().map(|c| format!("ws://{c}{RELAY_PATH}")).collect();
    if let Some(u) = public_url
        .map(|u| u.trim().trim_end_matches('/'))
        .filter(|u| !u.is_empty())
    {
        let u = if let Some(rest) = u.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = u.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            format!("wss://{u}")
        };
        v.push(format!("{u}{RELAY_PATH}"));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_and_rules() {
        let k = PublicKey([9; 32]);
        let f = frame(&k, b"packet");
        assert_eq!(split(&f), Some((k, &b"packet"[..])));
        assert!(split(&f[..32]).is_none(), "a key without a packet");
        assert!(split(&vec![0u8; MAX_MESSAGE + 1]).is_none());
        assert!(may_relay(NodeKind::Server, NodeKind::Device));
        assert!(may_relay(NodeKind::Device, NodeKind::Server));
        assert!(may_relay(NodeKind::Server, NodeKind::Server));
        assert!(!may_relay(NodeKind::Device, NodeKind::Device));
        assert_eq!(
            relay_urls(&["192.168.1.20:7443".into()], Some("https://mesh.example.com/")),
            vec![
                "ws://192.168.1.20:7443/v1/mesh/relay",
                "wss://mesh.example.com/v1/mesh/relay"
            ]
        );
        assert_eq!(
            relay_urls(&[], Some("mesh.example.com")),
            vec!["wss://mesh.example.com/v1/mesh/relay"]
        );
        assert!(relay_urls(&[], Some(" ")).is_empty());
    }
}

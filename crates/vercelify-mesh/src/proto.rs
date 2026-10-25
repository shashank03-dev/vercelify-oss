//! What nodes and the coordinator say to each other (design §13.2–13.4):
//! join tokens, the sealed enrollment, peer maps and their visibility
//! rules. Pure data and functions: the coordinator (in the agent) and the
//! daemon share them.

use crate::addr::Addressing;
use crate::cert::{self, NodeKind, MAP_PREFIX};
use crate::keys::{CoordinatorKey, VerifyingKey};
use crate::{MeshError, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64U;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// WireGuard port of servers.
pub const DEFAULT_WG_PORT: u16 = 51820;
/// The coordinator's enrollment listener.
pub const DEFAULT_CONTROL_PORT: u16 = 7443;
/// The agent on every node, reached over the mesh.
pub const AGENT_PORT: u16 = 7420;
pub const JOIN_PREFIX: &str = "vfyj1";
/// Zone of mesh names: `<node>.vfy`, `*.<node>.vfy`.
pub const DNS_ZONE: &str = "vfy";

/// What a device (a Mac) may reach on a server (design §3.5 of the plan):
/// the agent, Traefik, Ollama and the display stream. Database public ports
/// are added per server.
pub const DEVICE_PORTS: &[&str] = &["7420/tcp", "80/tcp", "443/tcp", "11434/tcp", "5600-5610/udp"];

/// Pasted on a new machine: `sudo vercelify mesh join vfyj1.…`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinToken {
    pub v: u8,
    /// Where the coordinator's enrollment listener is (`192.168.1.20:7443`, …).
    pub control: Vec<String>,
    /// The coordinator's WireGuard public key: the enrollment is sealed to it.
    pub coordinator_wg: String,
    /// The claim (single use, expires).
    pub claim: String,
    pub kind: NodeKind,
}

impl JoinToken {
    pub fn encode(&self) -> String {
        format!(
            "{JOIN_PREFIX}.{}",
            B64U.encode(serde_json::to_vec(self).expect("serializable"))
        )
    }

    pub fn decode(s: &str) -> Result<Self> {
        let rest = s
            .trim()
            .strip_prefix(&format!("{JOIN_PREFIX}."))
            .ok_or_else(|| MeshError::Invalid("not a join token (vfyj1.…)".into()))?;
        let raw = B64U
            .decode(rest)
            .map_err(|_| MeshError::Invalid("join token is damaged".into()))?;
        let t: JoinToken = serde_json::from_slice(&raw).map_err(|e| MeshError::Invalid(format!("join token: {e}")))?;
        if t.v != 1 || t.control.is_empty() {
            return Err(MeshError::Invalid("join token is damaged".into()));
        }
        Ok(t)
    }
}

/// Sealed inside the enrollment request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollRequest {
    pub claim: String,
    pub name: String,
    pub kind: NodeKind,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub arch: String,
    /// `ip:port` the node can be reached at (its LAN addresses).
    #[serde(default)]
    pub endpoints: Vec<String>,
    /// Unix seconds; old requests are refused.
    pub ts: i64,
}

/// The coordinator as a peer, so a new node can reach it right away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinatorInfo {
    pub public_key: String,
    pub ipv4: String,
    pub endpoints: Vec<String>,
}

/// Sealed inside the enrollment answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollResponse {
    pub node_id: String,
    pub name: String,
    pub ipv4: String,
    pub ipv6: String,
    /// Prefix length of the mesh's IPv4 network (16).
    pub ipv4_prefix_len: u8,
    pub certificate: String,
    /// `vfyn_…`: authenticates the node to the coordinator, inside the mesh.
    pub node_token: String,
    /// Ed25519 key that signs certificates and maps (base64).
    pub coordinator_key: String,
    pub coordinator: CoordinatorInfo,
    /// The agent of the coordinator, over the mesh (`http://10.77.0.1:7420`).
    pub api_url: String,
    /// The first map (signed).
    pub map: String,
}

/// A peer as a node sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub node_id: String,
    pub name: String,
    pub kind: NodeKind,
    pub role: String,
    pub public_key: String,
    pub ipv4: String,
    pub ipv6: String,
    pub endpoints: Vec<String>,
    /// For a server's view of a device: the ports it may reach. None: all.
    #[serde(default)]
    pub allowed_ports: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dns {
    pub zone: String,
    /// name → ipv4 (and `name` + "/6" → ipv6).
    pub records: BTreeMap<String, String>,
}

/// The signed payload of `vfym1.` (design §13.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerMap {
    pub v: u8,
    pub version: u64,
    pub epoch: u64,
    pub issued_at: i64,
    #[serde(rename = "self")]
    pub self_node: Peer,
    pub peers: Vec<Peer>,
    #[serde(default)]
    pub relays: Vec<String>,
    pub dns: Dns,
}

impl PeerMap {
    pub fn sign(&self, key: &CoordinatorKey) -> String {
        cert::sign(key, MAP_PREFIX, self)
    }

    pub fn verify(key: &VerifyingKey, s: &str) -> Result<Self> {
        let m: PeerMap = cert::verify(key, MAP_PREFIX, s)?;
        if m.v != 1 {
            return Err(MeshError::Invalid(format!("map version {}", m.v)));
        }
        Ok(m)
    }

    /// Whether `self` should replace `current` (newer, same or later epoch).
    pub fn supersedes(&self, current: Option<&PeerMap>) -> bool {
        match current {
            None => true,
            Some(c) => self.epoch > c.epoch || (self.epoch == c.epoch && self.version > c.version),
        }
    }
}

/// One line of `GET /v1/mesh/map`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MapEvent {
    Map {
        signed: String,
    },
    Ping,
    /// This node was taken off the mesh (sent before the others drop it).
    Revoked,
}

/// `POST /v1/mesh/endpoints`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointsReport {
    pub endpoints: Vec<String>,
}

/// `POST /v1/mesh/renew`: a new certificate, for a new key when given
/// (keys rotate with their certificate).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenewRequest {
    #[serde(default)]
    pub public_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenewResponse {
    pub certificate: String,
}

/// A node as the coordinator stores it (the map's input).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRecord {
    pub id: String,
    pub name: String,
    pub kind: NodeKind,
    pub role: String,
    pub public_key: String,
    pub ip_index: u16,
    pub endpoints: Vec<String>,
    /// Extra ports devices may reach on this server (databases made public).
    pub extra_device_ports: Vec<String>,
    pub revoked: bool,
}

fn peer_of(n: &NodeRecord, a: &Addressing) -> Peer {
    Peer {
        node_id: n.id.clone(),
        name: n.name.clone(),
        kind: n.kind,
        role: n.role.clone(),
        public_key: n.public_key.clone(),
        ipv4: a.ipv4(n.ip_index).to_string(),
        ipv6: a.ipv6(n.ip_index).to_string(),
        endpoints: n.endpoints.clone(),
        allowed_ports: None,
    }
}

/// The map `node` gets (design §13.4): servers see all servers and all
/// devices; devices see servers only; revoked nodes appear nowhere. A
/// server's view of a device carries the ports the device may reach on it.
pub fn build_map(
    nodes: &[NodeRecord],
    node: &NodeRecord,
    a: &Addressing,
    version: u64,
    epoch: u64,
    now: i64,
) -> PeerMap {
    let mut peers = vec![];
    let mut records = BTreeMap::new();
    for n in nodes.iter().filter(|n| !n.revoked) {
        let visible = n.id != node.id && (node.kind == NodeKind::Server || n.kind == NodeKind::Server);
        if n.kind == NodeKind::Server || n.id == node.id {
            records.insert(n.name.clone(), a.ipv4(n.ip_index).to_string());
            records.insert(format!("{}/6", n.name), a.ipv6(n.ip_index).to_string());
        }
        if !visible {
            continue;
        }
        let mut p = peer_of(n, a);
        if node.kind == NodeKind::Server && n.kind == NodeKind::Device {
            let mut ports: Vec<String> = DEVICE_PORTS.iter().map(|s| s.to_string()).collect();
            ports.extend(node.extra_device_ports.iter().cloned());
            p.allowed_ports = Some(ports);
        }
        peers.push(p);
    }
    PeerMap {
        v: 1,
        version,
        epoch,
        issued_at: now,
        self_node: peer_of(node, a),
        peers,
        relays: vec![],
        dns: Dns {
            zone: DNS_ZONE.into(),
            records,
        },
    }
}

/// A node name: DNS label, 1–40 characters.
pub fn valid_node_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
}

/// `host:port` with an IP host: what endpoints are.
pub fn valid_endpoint(s: &str) -> bool {
    s.parse::<std::net::SocketAddr>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::NodeKey;

    fn rec(id: &str, name: &str, kind: NodeKind, idx: u16) -> NodeRecord {
        NodeRecord {
            id: id.into(),
            name: name.into(),
            kind,
            role: "owner".into(),
            public_key: NodeKey::generate().public().to_base64(),
            ip_index: idx,
            endpoints: vec![format!("192.168.1.{idx}:51820")],
            extra_device_ports: vec![],
            revoked: false,
        }
    }

    #[test]
    fn join_tokens() {
        let t = JoinToken {
            v: 1,
            control: vec!["192.168.1.20:7443".into()],
            coordinator_wg: NodeKey::generate().public().to_base64(),
            claim: "abc".into(),
            kind: NodeKind::Server,
        };
        let s = t.encode();
        assert!(s.starts_with("vfyj1."));
        assert_eq!(JoinToken::decode(&s).unwrap(), t);
        assert_eq!(JoinToken::decode(&format!("  {s}\n")).unwrap(), t);
        assert!(JoinToken::decode("vfy1.abc").is_err());
        assert!(JoinToken::decode("vfyj1.!!!").is_err());
    }

    #[test]
    fn visibility() {
        let a = Addressing::default();
        let main = rec("mn_main", "main", NodeKind::Server, 1);
        let pi = rec("mn_pi", "pi", NodeKind::Server, 2);
        let mac = NodeRecord {
            role: "deployer".into(),
            ..rec("mn_mac", "mac", NodeKind::Device, 3)
        };
        let ipad = rec("mn_ipad", "ipad", NodeKind::Device, 4);
        let mut gone = rec("mn_gone", "gone", NodeKind::Server, 5);
        gone.revoked = true;
        let all = vec![main.clone(), pi.clone(), mac.clone(), ipad.clone(), gone];

        let m = build_map(&all, &pi, &a, 7, 1, 100);
        let names: Vec<&str> = m.peers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["main", "mac", "ipad"],
            "a server sees everyone but itself and the revoked"
        );
        assert_eq!(m.self_node.ipv4, "10.77.0.2");
        let mac_view = m.peers.iter().find(|p| p.name == "mac").unwrap();
        assert!(mac_view
            .allowed_ports
            .as_ref()
            .unwrap()
            .contains(&"7420/tcp".to_string()));
        assert!(m
            .peers
            .iter()
            .find(|p| p.name == "main")
            .unwrap()
            .allowed_ports
            .is_none());
        assert_eq!(m.dns.records.get("main").map(String::as_str), Some("10.77.0.1"));
        assert!(!m.dns.records.contains_key("mac"), "devices have no names in the zone");
        assert!(!m.dns.records.contains_key("gone"));

        let m = build_map(&all, &mac, &a, 7, 1, 100);
        let names: Vec<&str> = m.peers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["main", "pi"], "devices see servers only");
        assert_eq!(
            m.dns.records.get("mac").map(String::as_str),
            Some("10.77.0.3"),
            "but know themselves"
        );
    }

    #[test]
    fn signed_maps_and_ordering() {
        let a = Addressing::default();
        let k = CoordinatorKey::generate();
        let main = rec("mn_main", "main", NodeKind::Server, 1);
        let m1 = build_map(std::slice::from_ref(&main), &main, &a, 1, 1, 0);
        let s = m1.sign(&k);
        assert_eq!(PeerMap::verify(&k.verifying(), &s).unwrap(), m1);
        assert!(PeerMap::verify(&CoordinatorKey::generate().verifying(), &s).is_err());
        // A certificate is not a map.
        assert!(PeerMap::verify(&k.verifying(), &s.replacen("vfym1", "vfyc1", 1)).is_err());

        let m2 = PeerMap {
            version: 2,
            ..m1.clone()
        };
        assert!(m2.supersedes(Some(&m1)));
        assert!(!m1.supersedes(Some(&m2)));
        assert!(!m1.supersedes(Some(&m1)));
        let promoted = PeerMap {
            version: 1,
            epoch: 2,
            ..m1.clone()
        };
        assert!(
            promoted.supersedes(Some(&m2)),
            "a promoted standby starts again at a new epoch"
        );
        assert!(!m2.supersedes(Some(&promoted)));
    }

    #[test]
    fn names_and_endpoints() {
        assert!(valid_node_name("pi5-worker"));
        assert!(!valid_node_name("Pi5"));
        assert!(!valid_node_name("-a"));
        assert!(!valid_node_name(""));
        assert!(valid_endpoint("192.168.1.2:51820"));
        assert!(valid_endpoint("[fe80::1]:51820"));
        assert!(!valid_endpoint("host.example:51820"));
    }
}

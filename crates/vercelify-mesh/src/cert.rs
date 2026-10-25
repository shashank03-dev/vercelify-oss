//! Signed envelopes from the coordinator: node certificates (`vfyc1.`) and
//! peer maps (`vfym1.`). Both are `<prefix>.<base64url payload JSON>.<base64url
//! Ed25519 signature over the prefix and payload>`, so a signature can't be
//! moved from a map to a certificate, and verification never depends on how
//! JSON happens to be re-serialised.

use crate::keys::{CoordinatorKey, VerifyingKey};
use crate::{MeshError, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const CERT_PREFIX: &str = "vfyc1";
pub const MAP_PREFIX: &str = "vfym1";
/// Certificates last 30 days and are renewed after 20 (design §13.2).
pub const CERT_LIFETIME_SECS: i64 = 30 * 86_400;
pub const CERT_RENEW_AFTER_SECS: i64 = 20 * 86_400;
/// Tolerated clock difference between a node and the coordinator.
pub const CLOCK_SKEW_SECS: i64 = 300;

fn signed_input(prefix: &str, payload_b64: &str) -> Vec<u8> {
    format!("{prefix}.{payload_b64}").into_bytes()
}

pub fn sign<T: Serialize>(key: &CoordinatorKey, prefix: &str, payload: &T) -> String {
    let p = B64.encode(serde_json::to_vec(payload).expect("serializable"));
    let sig = key.sign(&signed_input(prefix, &p));
    format!("{prefix}.{p}.{}", B64.encode(sig))
}

pub fn verify<T: DeserializeOwned>(key: &VerifyingKey, prefix: &str, s: &str) -> Result<T> {
    let mut parts = s.trim().splitn(3, '.');
    let (pre, p, sig) = match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => return Err(MeshError::Invalid("not a signed envelope".into())),
    };
    if pre != prefix {
        return Err(MeshError::Invalid(format!("expected {prefix}, found {pre}")));
    }
    let sig = B64.decode(sig).map_err(|_| MeshError::BadSignature)?;
    key.verify(&signed_input(prefix, p), &sig)?;
    let json = B64
        .decode(p)
        .map_err(|_| MeshError::Invalid("payload is not base64url".into()))?;
    serde_json::from_slice(&json).map_err(|e| MeshError::Invalid(format!("payload: {e}")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Server,
    Device,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCert {
    pub v: u8,
    pub node_id: String,
    pub name: String,
    pub kind: NodeKind,
    /// owner / deployer / viewer
    pub role: String,
    /// WireGuard public key, base64.
    pub public_key: String,
    pub ipv4: String,
    pub ipv6: String,
    /// Unix seconds.
    pub not_before: i64,
    pub not_after: i64,
    /// Coordinator generation; increases when a standby is promoted.
    pub epoch: u64,
}

impl NodeCert {
    pub fn issue(key: &CoordinatorKey, mut cert: NodeCert, now: i64) -> String {
        cert.v = 1;
        cert.not_before = now - CLOCK_SKEW_SECS;
        cert.not_after = now + CERT_LIFETIME_SECS;
        sign(key, CERT_PREFIX, &cert)
    }

    /// Verify signature and validity window at `now` (unix seconds).
    pub fn verify(key: &VerifyingKey, s: &str, now: i64) -> Result<NodeCert> {
        let c: NodeCert = verify(key, CERT_PREFIX, s)?;
        if c.v != 1 {
            return Err(MeshError::Invalid(format!("certificate version {}", c.v)));
        }
        if now + CLOCK_SKEW_SECS < c.not_before || now - CLOCK_SKEW_SECS > c.not_after {
            return Err(MeshError::Expired);
        }
        Ok(c)
    }

    pub fn needs_renewal(&self, now: i64) -> bool {
        now >= self.not_before + CLOCK_SKEW_SECS + CERT_RENEW_AFTER_SECS
    }
}

pub fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cert() -> NodeCert {
        NodeCert {
            v: 0,
            node_id: "mn_1".into(),
            name: "pi5".into(),
            kind: NodeKind::Server,
            role: "owner".into(),
            public_key: "x".into(),
            ipv4: "10.77.0.3".into(),
            ipv6: "fd76:6679:6d65::3".into(),
            not_before: 0,
            not_after: 0,
            epoch: 1,
        }
    }

    #[test]
    fn issue_and_verify() {
        let k = CoordinatorKey::generate();
        let now = 1_800_000_000;
        let s = NodeCert::issue(&k, cert(), now);
        assert!(s.starts_with("vfyc1."));
        let c = NodeCert::verify(&k.verifying(), &s, now).unwrap();
        assert_eq!(c.name, "pi5");
        assert_eq!(c.v, 1);
        assert!(!c.needs_renewal(now));
        assert!(c.needs_renewal(now + CERT_RENEW_AFTER_SECS));
        assert_eq!(
            NodeCert::verify(&k.verifying(), &s, now + CERT_LIFETIME_SECS + CLOCK_SKEW_SECS + 1),
            Err(MeshError::Expired)
        );
        assert!(
            NodeCert::verify(&k.verifying(), &s, now + CERT_LIFETIME_SECS).is_ok(),
            "within skew"
        );
    }

    #[test]
    fn rejects_forgeries() {
        let k = CoordinatorKey::generate();
        let now = 1_800_000_000;
        let s = NodeCert::issue(&k, cert(), now);
        // Another coordinator.
        let other = CoordinatorKey::generate();
        assert_eq!(
            NodeCert::verify(&other.verifying(), &s, now),
            Err(MeshError::BadSignature)
        );
        // Changed payload.
        let mut forged = cert();
        forged.role = "owner".into();
        forged.name = "evil".into();
        let parts: Vec<&str> = s.split('.').collect();
        let fake = format!(
            "{}.{}.{}",
            parts[0],
            B64.encode(serde_json::to_vec(&forged).unwrap()),
            parts[2]
        );
        assert_eq!(
            NodeCert::verify(&k.verifying(), &fake, now),
            Err(MeshError::BadSignature)
        );
        // A map signature is not a certificate.
        let m = sign(&k, MAP_PREFIX, &cert());
        assert!(NodeCert::verify(&k.verifying(), &m, now).is_err());
        let relabeled = m.replacen(MAP_PREFIX, CERT_PREFIX, 1);
        assert_eq!(
            NodeCert::verify(&k.verifying(), &relabeled, now),
            Err(MeshError::BadSignature)
        );
        assert!(NodeCert::verify(&k.verifying(), "garbage", now).is_err());
    }
}

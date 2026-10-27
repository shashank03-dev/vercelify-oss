//! Where the coordinator can be reached, for nodes that lost it: a signed
//! note (`vfyb1.`) with its relay URLs, posted to a public message drop
//! (ntfy.sh by default) under a topic derived from the coordinator's key.
//!
//! A quick tunnel's address, or a home connection's public IP, changes when
//! the server restarts. A node whose only path was the relay at the old
//! address can't receive the map that carries the new one, so it reads the
//! note instead. The signature stops anyone else from moving nodes, and a
//! wrong relay could only drop packets anyway: WireGuard authenticates the
//! peers end to end.

use crate::cert;
use crate::keys::{CoordinatorKey, VerifyingKey};
use crate::{MeshError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const BEACON_PREFIX: &str = "vfyb1";
/// The public drop the server posts to and nodes read from.
pub const DEFAULT_BEACON_URL: &str = "https://ntfy.sh";
/// The server posts at least this often; ntfy keeps messages for 12 hours.
pub const REPOST_SECS: i64 = 20 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Beacon {
    pub v: u8,
    pub relays: Vec<String>,
    pub issued_at: i64,
}

/// The topic for a coordinator: unguessable without its public key, which
/// only joined nodes hold.
pub fn topic(key: &VerifyingKey) -> String {
    let mut h = Sha256::new();
    h.update(b"vercelify-beacon\0");
    h.update(key.to_base64().as_bytes());
    let d = h.finalize();
    let hex: String = d[..20].iter().map(|b| format!("{b:02x}")).collect();
    format!("vfy{hex}")
}

impl Beacon {
    pub fn sign(&self, key: &CoordinatorKey) -> String {
        cert::sign(key, BEACON_PREFIX, self)
    }

    pub fn verify(key: &VerifyingKey, s: &str, now: i64) -> Result<Self> {
        let b: Beacon = cert::verify(key, BEACON_PREFIX, s)?;
        if b.v != 1 {
            return Err(MeshError::Invalid(format!("beacon version {}", b.v)));
        }
        if b.issued_at > now + cert::CLOCK_SKEW_SECS {
            return Err(MeshError::Invalid("beacon from the future".into()));
        }
        Ok(b)
    }
}

/// The newest note that verifies, in an ntfy JSON stream
/// (`GET <drop>/<topic>/json?poll=1`): one JSON object per line, the note in
/// `message`. Anything else on the topic is ignored.
pub fn newest(key: &VerifyingKey, body: &str, now: i64) -> Option<Beacon> {
    body.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
        .filter_map(|m| Beacon::verify(key, &m, now).ok())
        .max_by_key(|b| b.issued_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_found_and_forgeries_ignored() {
        let ck = CoordinatorKey::generate();
        let other = CoordinatorKey::generate();
        let vk = ck.verifying();
        let t = topic(&vk);
        assert!(t.starts_with("vfy") && t.len() == 43);
        assert_ne!(t, topic(&other.verifying()));

        let old = Beacon {
            v: 1,
            relays: vec!["wss://a.trycloudflare.com/r".into()],
            issued_at: 100,
        };
        let new = Beacon {
            v: 1,
            relays: vec!["wss://b.trycloudflare.com/r".into()],
            issued_at: 200,
        };
        let forged = Beacon {
            v: 1,
            relays: vec!["wss://evil.example/r".into()],
            issued_at: 300,
        };
        let line = |s: String| serde_json::json!({"event": "message", "message": s}).to_string();
        let body = [
            r#"{"event":"open"}"#.to_string(),
            line(old.sign(&ck)),
            line(new.sign(&ck)),
            line(forged.sign(&other)),
            line("hello".into()),
        ]
        .join("\n");
        assert_eq!(newest(&vk, &body, 1000), Some(new.clone()));
        // A note dated well ahead of our clock is refused.
        assert_eq!(newest(&vk, &line(new.sign(&ck)), 200 - cert::CLOCK_SKEW_SECS - 1), None);
        // A map's signature doesn't pass for a note.
        let env = new.sign(&ck);
        assert!(Beacon::verify(&vk, &env.replacen("vfyb1", "vfym1", 1), 1000).is_err());
    }
}

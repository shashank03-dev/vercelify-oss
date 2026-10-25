//! Sealed enrollment (design §13.2): a joining node encrypts its request to
//! the coordinator's WireGuard public key, which it knows from the join
//! token, and the coordinator answers under the same shared secret. Nobody
//! on the path can read the claim, change the key being enrolled, or read
//! the node token in the answer, without TLS on the control listener.
//!
//! Key schedule: `X25519(node, coordinator)` → HKDF-SHA256 (salt
//! `vercelify-enroll-v1`, info = node key ‖ coordinator key) → 64 bytes: one
//! ChaCha20-Poly1305 key per direction, so a request can't be replayed back
//! as an answer. Nonces are random (each key seals few messages).

use crate::keys::{NodeKey, PublicKey};
use crate::{MeshError, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const SALT: &[u8] = b"vercelify-enroll-v1";

/// One sealed message: who sent it (the joining node's key), a nonce, and
/// the ciphertext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sealed {
    pub v: u8,
    /// The joining node's WireGuard public key (base64).
    pub public_key: String,
    pub nonce: String,
    pub data: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// Joining node → coordinator.
    Request,
    /// Coordinator → joining node.
    Response,
}

/// The two directional keys for (`node_public`, `coordinator_public`),
/// from the X25519 secret either side computes.
fn keys(shared: &[u8; 32], node_public: &PublicKey, coordinator_public: &PublicKey) -> Result<Zeroizing<[u8; 64]>> {
    if shared.iter().all(|b| *b == 0) {
        // A low-order point: the "shared" secret is known to everyone.
        return Err(MeshError::Invalid("unusable public key".into()));
    }
    let mut info = Vec::with_capacity(64);
    info.extend_from_slice(&node_public.0);
    info.extend_from_slice(&coordinator_public.0);
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(SALT), shared);
    let mut out = Zeroizing::new([0u8; 64]);
    hk.expand(&info, &mut *out)
        .map_err(|_| MeshError::Invalid("key derivation".into()))?;
    Ok(out)
}

fn cipher(keys: &[u8; 64], dir: Direction) -> ChaCha20Poly1305 {
    let k = match dir {
        Direction::Request => &keys[..32],
        Direction::Response => &keys[32..],
    };
    ChaCha20Poly1305::new(Key::from_slice(k))
}

fn nonce() -> [u8; 12] {
    let mut n = [0u8; 12];
    getrandom::fill(&mut n).expect("OS RNG available");
    n
}

fn seal_with(keys: &[u8; 64], dir: Direction, node_public: &PublicKey, plaintext: &[u8]) -> Sealed {
    let n = nonce();
    let data = cipher(keys, dir)
        .encrypt(
            Nonce::from_slice(&n),
            Payload {
                msg: plaintext,
                aad: &node_public.0,
            },
        )
        .expect("encryption cannot fail");
    Sealed {
        v: 1,
        public_key: node_public.to_base64(),
        nonce: B64.encode(n),
        data: B64.encode(data),
    }
}

fn open_with(keys: &[u8; 64], dir: Direction, node_public: &PublicKey, s: &Sealed) -> Result<Vec<u8>> {
    if s.v != 1 {
        return Err(MeshError::Invalid(format!("sealed message version {}", s.v)));
    }
    let n = B64
        .decode(&s.nonce)
        .map_err(|_| MeshError::Invalid("nonce is not base64".into()))?;
    if n.len() != 12 {
        return Err(MeshError::Invalid("nonce must be 12 bytes".into()));
    }
    let data = B64
        .decode(&s.data)
        .map_err(|_| MeshError::Invalid("data is not base64".into()))?;
    cipher(keys, dir)
        .decrypt(
            Nonce::from_slice(&n),
            Payload {
                msg: &data,
                aad: &node_public.0,
            },
        )
        .map_err(|_| MeshError::BadSignature)
}

/// The joining node's side.
pub struct NodeSession {
    keys: Zeroizing<[u8; 64]>,
    node_public: PublicKey,
}

impl NodeSession {
    pub fn new(node: &NodeKey, coordinator: &PublicKey) -> Result<Self> {
        let node_public = node.public();
        let shared = node.shared_secret(coordinator);
        Ok(Self {
            keys: keys(&shared, &node_public, coordinator)?,
            node_public,
        })
    }

    pub fn seal_request(&self, plaintext: &[u8]) -> Sealed {
        seal_with(&self.keys, Direction::Request, &self.node_public, plaintext)
    }

    pub fn open_response(&self, s: &Sealed) -> Result<Vec<u8>> {
        if s.public_key != self.node_public.to_base64() {
            return Err(MeshError::Invalid("an answer for another node".into()));
        }
        open_with(&self.keys, Direction::Response, &self.node_public, s)
    }
}

/// The coordinator's side, for one request.
pub struct CoordinatorSession {
    keys: Zeroizing<[u8; 64]>,
    pub node_public: PublicKey,
}

impl CoordinatorSession {
    /// Open a request sealed to `coordinator`'s key.
    pub fn open(coordinator: &NodeKey, s: &Sealed) -> Result<(Self, Vec<u8>)> {
        let node_public = PublicKey::from_base64(&s.public_key)?;
        let shared = coordinator.shared_secret(&node_public);
        let session = Self {
            keys: keys(&shared, &node_public, &coordinator.public())?,
            node_public,
        };
        let plain = open_with(&session.keys, Direction::Request, &node_public, s)?;
        Ok((session, plain))
    }

    pub fn seal_response(&self, plaintext: &[u8]) -> Sealed {
        seal_with(&self.keys, Direction::Response, &self.node_public, plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let coord = NodeKey::generate();
        let node = NodeKey::generate();
        let s = NodeSession::new(&node, &coord.public()).unwrap();
        let req = s.seal_request(b"claim=K7QF-M2XD");
        let (cs, plain) = CoordinatorSession::open(&coord, &req).unwrap();
        assert_eq!(plain, b"claim=K7QF-M2XD");
        assert_eq!(cs.node_public, node.public());
        let resp = cs.seal_response(b"token");
        assert_eq!(s.open_response(&resp).unwrap(), b"token");
    }

    #[test]
    fn tampering_and_reflection_fail() {
        let coord = NodeKey::generate();
        let node = NodeKey::generate();
        let s = NodeSession::new(&node, &coord.public()).unwrap();
        let req = s.seal_request(b"hello");

        // Another coordinator can't open it.
        assert!(CoordinatorSession::open(&NodeKey::generate(), &req).is_err());
        // A swapped sender key (enrolling someone else's key) fails.
        let mut swapped = req.clone();
        swapped.public_key = NodeKey::generate().public().to_base64();
        assert!(CoordinatorSession::open(&coord, &swapped).is_err());
        // A flipped byte fails.
        let mut bad = req.clone();
        let mut d = B64.decode(&bad.data).unwrap();
        d[0] ^= 1;
        bad.data = B64.encode(d);
        assert!(CoordinatorSession::open(&coord, &bad).is_err());
        // The node's own request isn't accepted as an answer.
        assert!(s.open_response(&req).is_err());
        // A low-order point is refused.
        let mut zero = req.clone();
        zero.public_key = B64.encode([0u8; 32]);
        assert!(CoordinatorSession::open(&coord, &zero).is_err());
    }
}

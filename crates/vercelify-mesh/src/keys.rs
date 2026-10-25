//! Keys. A node's WireGuard static key (X25519) is generated on the node and
//! its private half never leaves it. The coordinator's signing key (Ed25519)
//! signs node certificates and peer maps.

use crate::{MeshError, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use zeroize::Zeroizing;

fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("OS RNG available");
    b
}

/// Read a 32-byte key file (raw or base64), or create one with mode 0600.
fn load_or_create_32(path: &Path) -> std::io::Result<Zeroizing<[u8; 32]>> {
    match fs::read(path) {
        Ok(raw) => {
            let raw = Zeroizing::new(raw);
            let bytes: Zeroizing<Vec<u8>> = if raw.len() == 32 {
                Zeroizing::new(raw.to_vec())
            } else {
                Zeroizing::new(
                    STANDARD
                        .decode(String::from_utf8_lossy(&raw).trim())
                        .map_err(|_| std::io::Error::other(format!("{}: not a key", path.display())))?,
                )
            };
            let arr: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| std::io::Error::other(format!("{}: a key is 32 bytes", path.display())))?;
            Ok(Zeroizing::new(arr))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(d) = path.parent() {
                fs::create_dir_all(d)?;
            }
            let key = Zeroizing::new(random32());
            match OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
                Ok(mut f) => {
                    f.write_all(&*key)?;
                    f.sync_all()?;
                    Ok(key)
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => load_or_create_32(path),
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

/// A WireGuard public key, base64 like `wg` prints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey(pub [u8; 32]);

impl PublicKey {
    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }

    pub fn from_base64(s: &str) -> Result<Self> {
        let v = STANDARD
            .decode(s.trim())
            .map_err(|_| MeshError::Invalid("public key is not base64".into()))?;
        let arr: [u8; 32] = v
            .try_into()
            .map_err(|_| MeshError::Invalid("public key must be 32 bytes".into()))?;
        Ok(Self(arr))
    }
}

impl std::fmt::Display for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_base64())
    }
}

pub struct NodeKey {
    secret: x25519_dalek::StaticSecret,
}

impl NodeKey {
    pub fn generate() -> Self {
        Self {
            secret: x25519_dalek::StaticSecret::from(random32()),
        }
    }

    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self {
            secret: x25519_dalek::StaticSecret::from(b),
        }
    }

    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        Ok(Self::from_bytes(*load_or_create_32(path)?))
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(x25519_dalek::PublicKey::from(&self.secret).to_bytes())
    }

    /// Replace the key file (0600, atomically): a rotated key.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::state::write_private(path, &*self.secret_bytes())
    }

    /// The raw private key, for handing to the WireGuard implementation.
    pub fn secret_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.secret.to_bytes())
    }

    /// X25519 shared secret with a peer (used to seal disco messages).
    pub fn shared_secret(&self, peer: &PublicKey) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(
            self.secret
                .diffie_hellman(&x25519_dalek::PublicKey::from(peer.0))
                .to_bytes(),
        )
    }
}

pub struct CoordinatorKey {
    signing: ed25519_dalek::SigningKey,
}

impl CoordinatorKey {
    pub fn generate() -> Self {
        Self {
            signing: ed25519_dalek::SigningKey::from_bytes(&random32()),
        }
    }

    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            signing: ed25519_dalek::SigningKey::from_bytes(&*load_or_create_32(path)?),
        })
    }

    pub fn verifying(&self) -> VerifyingKey {
        VerifyingKey(self.signing.verifying_key())
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        self.signing.sign(msg).to_bytes()
    }
}

/// The coordinator's public signing key, pinned by every node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyingKey(pub ed25519_dalek::VerifyingKey);

impl VerifyingKey {
    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0.to_bytes())
    }

    pub fn from_base64(s: &str) -> Result<Self> {
        let v = STANDARD
            .decode(s.trim())
            .map_err(|_| MeshError::Invalid("coordinator key is not base64".into()))?;
        let arr: [u8; 32] = v
            .try_into()
            .map_err(|_| MeshError::Invalid("coordinator key must be 32 bytes".into()))?;
        ed25519_dalek::VerifyingKey::from_bytes(&arr)
            .map(Self)
            .map_err(|_| MeshError::Invalid("not an Ed25519 public key".into()))
    }

    pub fn verify(&self, msg: &[u8], sig: &[u8]) -> Result<()> {
        let sig: [u8; 64] = sig.try_into().map_err(|_| MeshError::BadSignature)?;
        self.0
            .verify_strict(msg, &ed25519_dalek::Signature::from_bytes(&sig))
            .map_err(|_| MeshError::BadSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn node_keys_persist_and_agree() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mesh/node.key");
        let a = NodeKey::load_or_create(&p).unwrap();
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let again = NodeKey::load_or_create(&p).unwrap();
        assert_eq!(a.public(), again.public());
        let b = NodeKey::generate();
        assert_eq!(*a.shared_secret(&b.public()), *b.shared_secret(&a.public()));
        let pk = a.public();
        assert_eq!(PublicKey::from_base64(&pk.to_base64()).unwrap(), pk);
        assert!(PublicKey::from_base64("AAAA").is_err());
    }

    #[test]
    fn base64_key_files_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("k");
        fs::write(&p, format!("{}\n", STANDARD.encode([3u8; 32]))).unwrap();
        let k = NodeKey::load_or_create(&p).unwrap();
        assert_eq!(
            *k.secret_bytes(),
            x25519_dalek::StaticSecret::from([3u8; 32]).to_bytes()
        );
    }

    #[test]
    fn signatures() {
        let dir = tempfile::tempdir().unwrap();
        let c = CoordinatorKey::load_or_create(&dir.path().join("c.key")).unwrap();
        let v = c.verifying();
        let sig = c.sign(b"map");
        v.verify(b"map", &sig).unwrap();
        assert_eq!(v.verify(b"other", &sig), Err(MeshError::BadSignature));
        let v2 = VerifyingKey::from_base64(&v.to_base64()).unwrap();
        assert_eq!(v, v2);
        assert_eq!(v.verify(b"map", &sig[..10]), Err(MeshError::BadSignature));
    }
}

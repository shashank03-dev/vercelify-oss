//! Secrets at rest: environment variables, database credentials, git
//! credentials.
//!
//! A sealed value is `0x01 ‖ nonce (24 bytes) ‖ XChaCha20-Poly1305(plaintext)`.
//! The associated data names the cell it belongs to (`table/column/row id`),
//! so a value copied into another row or column fails to open instead of
//! decrypting as something it isn't. The master key is 32 random bytes in a
//! root-only file (design §5).

use crate::{Error, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use zeroize::Zeroizing;

const VERSION: u8 = 1;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
pub const KEY_LEN: usize = 32;

pub struct MasterKey {
    cipher: XChaCha20Poly1305,
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(..)")
    }
}

/// Which cell a sealed value belongs to.
pub fn aad(table: &str, column: &str, id: &str) -> String {
    format!("{table}/{column}/{id}")
}

impl MasterKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != KEY_LEN {
            return Err(Error::Invalid(format!(
                "the master key must be {KEY_LEN} bytes, found {}",
                bytes.len()
            )));
        }
        Ok(Self {
            cipher: XChaCha20Poly1305::new_from_slice(bytes).expect("length checked"),
        })
    }

    /// Read the key file, or create it (0600) if it doesn't exist yet.
    /// A key file readable by others is tightened to 0600 with a warning.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                let mode = fs::metadata(path)?.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    tracing::warn!("{} was mode {mode:o}; setting it to 600", path.display());
                    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
                }
                Self::from_bytes(&bytes).map_err(|e| {
                    Error::Internal(format!(
                        "{}: {} — restore it from a backup; without it, stored secrets can't be read",
                        path.display(),
                        e.message()
                    ))
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(dir) = path.parent() {
                    fs::create_dir_all(dir)?;
                }
                let key = Zeroizing::new(random::<KEY_LEN>());
                // create_new: two processes starting at once can't both win
                // and leave values sealed with a key that was overwritten.
                let mut f = match OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
                    Ok(f) => f,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Self::load_or_create(path),
                    Err(e) => return Err(e.into()),
                };
                f.write_all(&*key)?;
                f.sync_all()?;
                tracing::info!("created the master key {}", path.display());
                Self::from_bytes(&*key)
            }
            Err(e) => Err(Error::Internal(format!("{}: {e}", path.display()))),
        }
    }

    pub fn seal(&self, aad: &str, plaintext: &[u8]) -> Vec<u8> {
        let nonce = random::<NONCE_LEN>();
        let ct = self
            .cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .expect("XChaCha20-Poly1305 encryption does not fail for in-memory buffers");
        let mut out = Vec::with_capacity(1 + NONCE_LEN + ct.len());
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    pub fn open(&self, aad: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        if sealed.len() < 1 + NONCE_LEN + TAG_LEN || sealed[0] != VERSION {
            return Err(Error::Internal(format!("{aad}: not a sealed value")));
        }
        let (nonce, ct) = sealed[1..].split_at(NONCE_LEN);
        self.cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ct,
                    aad: aad.as_bytes(),
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| {
                Error::Internal(format!(
                    "{aad}: can't decrypt (wrong master key, or the value was moved from another record)"
                ))
            })
    }

    pub fn seal_str(&self, aad: &str, plaintext: &str) -> Vec<u8> {
        self.seal(aad, plaintext.as_bytes())
    }

    pub fn open_string(&self, aad: &str, sealed: &[u8]) -> Result<Zeroizing<String>> {
        let bytes = self.open(aad, sealed)?;
        String::from_utf8(bytes.to_vec())
            .map(Zeroizing::new)
            .map_err(|_| Error::Internal(format!("{aad}: not UTF-8")))
    }
}

pub fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("OS RNG available");
    b
}

/// A random secret for people and URLs: `n` characters of base62.
pub fn random_token(n: usize) -> String {
    const B62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut out = String::with_capacity(n);
    while out.len() < n {
        // Rejection sampling: 62 * 4 = 248, so bytes >= 248 would bias.
        for b in random::<64>() {
            if b < 248 && out.len() < n {
                out.push(B62[(b % 62) as usize] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> MasterKey {
        MasterKey::from_bytes(&[7u8; 32]).unwrap()
    }

    #[test]
    fn round_trip() {
        let k = key();
        let a = aad("env_vars", "value", "var_1");
        let s = k.seal_str(&a, "postgres://u:p@db/x");
        assert_eq!(s[0], 1);
        assert_eq!(s.len(), 1 + 24 + 19 + 16);
        assert_eq!(&*k.open_string(&a, &s).unwrap(), "postgres://u:p@db/x");
        // Same plaintext, different nonce.
        assert_ne!(s, k.seal_str(&a, "postgres://u:p@db/x"));
    }

    #[test]
    fn bound_to_its_cell_and_key() {
        let k = key();
        let s = k.seal_str(&aad("env_vars", "value", "var_1"), "secret");
        assert!(
            k.open(&aad("env_vars", "value", "var_2"), &s).is_err(),
            "moved to another row"
        );
        assert!(
            k.open(&aad("databases", "credentials", "var_1"), &s).is_err(),
            "moved to another column"
        );
        let other = MasterKey::from_bytes(&[8u8; 32]).unwrap();
        assert!(other.open(&aad("env_vars", "value", "var_1"), &s).is_err(), "other key");
        let mut tampered = s.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(k.open(&aad("env_vars", "value", "var_1"), &tampered).is_err());
        assert!(k.open("x", &[1, 2, 3]).is_err());
    }

    #[test]
    fn key_file_is_created_private_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/master.key");
        let k1 = MasterKey::load_or_create(&path).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read(&path).unwrap().len(), 32);
        let sealed = k1.seal_str("a", "b");
        let k2 = MasterKey::load_or_create(&path).unwrap();
        assert_eq!(&*k2.open_string("a", &sealed).unwrap(), "b");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        MasterKey::load_or_create(&path).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

        fs::write(&path, b"short").unwrap();
        assert!(MasterKey::load_or_create(&path).is_err());
    }

    #[test]
    fn tokens() {
        let t = random_token(32);
        assert_eq!(t.len(), 32);
        assert!(t.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_ne!(t, random_token(32));
    }
}

//! Pairing by short code over the LAN (design §13.3). The app found a
//! server by mDNS (anyone can answer that) and the user typed the 8-symbol
//! code `sudo vercelify pair` printed. The code never travels: both sides
//! run SPAKE2 with it and derive a key only they share, and the server
//! seals the full pairing code under that key. A wrong code (or someone in
//! the middle) gets nothing it can open and learns nothing to test codes
//! against offline; every attempt is one online guess, and the server
//! allows five.

use crate::{MeshError, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};
use spake2::{Ed25519Group, Identity, Password, Spake2};

const ID_APP: &[u8] = b"vercelify-app";
const ID_SERVER: &[u8] = b"vercelify-server";
const INFO: &[u8] = b"vercelify-pair-v1";
pub const PAIR_PATH: &str = "/v1/pair/pake";

/// `POST /v1/pair/pake` from the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRequest {
    /// The app's SPAKE2 message (base64).
    pub spake: String,
}

/// The server's answer: its SPAKE2 message and the sealed pairing code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairAnswer {
    pub spake: String,
    pub nonce: String,
    pub data: String,
}

/// Codes are compared without dashes, spaces or case (`k7qf-m2xd`).
fn password(code: &str) -> Password {
    let c: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    Password::new(c.as_bytes())
}

fn cipher(key: &[u8]) -> Result<ChaCha20Poly1305> {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, key);
    let mut k = zeroize::Zeroizing::new([0u8; 32]);
    hk.expand(INFO, &mut *k)
        .map_err(|_| MeshError::Invalid("key derivation".into()))?;
    Ok(ChaCha20Poly1305::new(Key::from_slice(&*k)))
}

/// The app's first step.
pub struct AppSide(Spake2<Ed25519Group>);

pub fn app_start(code: &str) -> (AppSide, PairRequest) {
    let (s, msg) = Spake2::<Ed25519Group>::start_a(&password(code), &Identity::new(ID_APP), &Identity::new(ID_SERVER));
    (AppSide(s), PairRequest { spake: B64.encode(msg) })
}

impl AppSide {
    /// Open the server's answer: the pairing code, or an error when the
    /// code was wrong (or the answer isn't from the server that has it).
    pub fn finish(self, a: &PairAnswer) -> Result<Vec<u8>> {
        let bad = |w: &str| MeshError::Invalid(format!("pairing answer: {w}"));
        let msg = B64.decode(&a.spake).map_err(|_| bad("spake"))?;
        let key = self.0.finish(&msg).map_err(|_| bad("spake"))?;
        let nonce = B64.decode(&a.nonce).map_err(|_| bad("nonce"))?;
        if nonce.len() != 12 {
            return Err(bad("nonce"));
        }
        let data = B64.decode(&a.data).map_err(|_| bad("data"))?;
        cipher(&key)?
            .decrypt(Nonce::from_slice(&nonce), Payload { msg: &data, aad: INFO })
            .map_err(|_| MeshError::Invalid("wrong pairing code".into()))
    }
}

/// The server's step: answer `req` for `code`, sealing `pairing`.
pub fn server_answer(code: &str, req: &PairRequest, pairing: &[u8]) -> Result<PairAnswer> {
    let msg = B64
        .decode(&req.spake)
        .map_err(|_| MeshError::Invalid("spake is not base64".into()))?;
    let (s, out) = Spake2::<Ed25519Group>::start_b(&password(code), &Identity::new(ID_APP), &Identity::new(ID_SERVER));
    let key = s
        .finish(&msg)
        .map_err(|_| MeshError::Invalid("not a SPAKE2 message".into()))?;
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| MeshError::Invalid(e.to_string()))?;
    let data = cipher(&key)?
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: pairing,
                aad: INFO,
            },
        )
        .map_err(|_| MeshError::Invalid("sealing".into()))?;
    Ok(PairAnswer {
        spake: B64.encode(out),
        nonce: B64.encode(nonce),
        data: B64.encode(data),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_right_code_opens_it_and_only_that() {
        let (app, req) = app_start("k7qf-m2xd");
        let ans = server_answer("K7QF M2XD", &req, b"vfy2.pairing").unwrap();
        assert_eq!(app.finish(&ans).unwrap(), b"vfy2.pairing");

        let (app, req) = app_start("K7QF-M2XE");
        let ans = server_answer("K7QF-M2XD", &req, b"vfy2.pairing").unwrap();
        let err = app.finish(&ans).unwrap_err();
        assert!(err.to_string().contains("wrong pairing code"), "{err}");

        // The request carries nothing the code can be read from: two
        // requests for the same code differ.
        assert_ne!(app_start("K7QF-M2XD").1, app_start("K7QF-M2XD").1);
        assert!(server_answer("K7QF-M2XD", &PairRequest { spake: "!!".into() }, b"x").is_err());
    }
}

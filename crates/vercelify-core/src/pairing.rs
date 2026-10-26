//! Pairing codes: a single copy-pasteable string printed by the host
//! installer that carries everything the cockpit needs to connect.
//!
//! Format: `vfy1.` + base64url(JSON of [`Pairing`]) without padding;
//! `vfy2.` for a server on Vercelify's own mesh (the code then carries a
//! device join token, and `agent_url` is the server's mesh address).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

pub const PREFIX: &str = "vfy1.";
pub const PREFIX_MESH: &str = "vfy2.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct Pairing {
    /// e.g. `http://10.77.0.1:7420` (the server's mesh address).
    pub agent_url: String,
    pub agent_token: String,
    pub hostname: String,
    /// `main` or `worker` (absent in codes from 0.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub role: Option<String>,
    /// `vfyj1.…`: a single-use token that puts this device on the server's
    /// mesh (design §13.3). The app joins with it, then reaches `agent_url`
    /// over the mesh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mesh_join: Option<String>,
}

#[derive(Debug, Error, PartialEq)]
pub enum PairingError {
    #[error("pairing code must start with '{PREFIX}' or '{PREFIX_MESH}'")]
    Prefix,
    #[error("pairing code is not valid base64")]
    Encoding,
    #[error("pairing code payload is invalid: {0}")]
    Payload(String),
}

pub fn encode(p: &Pairing) -> String {
    format!(
        "{}{}",
        if p.mesh_join.is_some() { PREFIX_MESH } else { PREFIX },
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(p).expect("serializable"))
    )
}

pub fn decode(code: &str) -> Result<Pairing, PairingError> {
    let code = code.trim();
    let (body, mesh) = match code.strip_prefix(PREFIX) {
        Some(b) => (b, false),
        None => (code.strip_prefix(PREFIX_MESH).ok_or(PairingError::Prefix)?, true),
    };
    // Tolerate padding and whitespace introduced by copy/paste.
    let cleaned: String = body.chars().filter(|c| !c.is_whitespace() && *c != '=').collect();
    let bytes = URL_SAFE_NO_PAD.decode(cleaned).map_err(|_| PairingError::Encoding)?;
    let p: Pairing = serde_json::from_slice(&bytes).map_err(|e| PairingError::Payload(e.to_string()))?;
    if !(p.agent_url.starts_with("http://") || p.agent_url.starts_with("https://")) {
        return Err(PairingError::Payload("agent_url must be http(s)".into()));
    }
    if p.agent_token.len() < 16 {
        return Err(PairingError::Payload("agent_token too short".into()));
    }
    if mesh != p.mesh_join.is_some() {
        return Err(PairingError::Payload(
            "a vfy2. code carries a mesh join token, a vfy1. code doesn't".into(),
        ));
    }
    if p.mesh_join.as_deref().is_some_and(|j| !j.starts_with("vfyj1.")) {
        return Err(PairingError::Payload("mesh_join must be a vfyj1. token".into()));
    }
    Ok(p)
}

// --- short codes ----------------------------------------------------------------
//
// `sudo vercelify pair` also prints a short one-time code like `K7QF-M2XD`.
// The cockpit finds the host on the tailnet and trades the short code for
// the full pairing code at `POST /v1/pair/claim`. 8 symbols from a 31-symbol
// alphabet; together with a 15 minute lifetime and a 5-attempt
// limit that is far out of reach of guessing.

/// Alphabet without 0/O and 1/I/L: easy to read aloud and type (31 symbols).
pub const SHORT_ALPHABET: &[u8; 31] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
/// Seconds a short code stays valid.
pub const SHORT_CODE_TTL_SECS: u64 = 15 * 60;
/// Failed claims before the code is thrown away.
pub const SHORT_CODE_MAX_ATTEMPTS: u32 = 5;

/// Format 8 random bytes as `XXXX-XXXX`.
pub fn short_code(random: &[u8; 8]) -> String {
    // Base-31 digits of a 64-bit number: 31^8 is tiny next to 2^64, so the
    // digits are uniform for all practical purposes.
    let mut n = u64::from_le_bytes(*random);
    let mut out = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            out.push('-');
        }
        out.push(SHORT_ALPHABET[(n % 31) as usize] as char);
        n /= 31;
    }
    out
}

/// Canonical form for comparison: upper case, spaces and dashes dropped.
/// `None` when it cannot be a short code.
pub fn normalize_short_code(input: &str) -> Option<String> {
    let s: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (s.len() == 8 && s.bytes().all(|b| SHORT_ALPHABET.contains(&b))).then_some(s)
}

/// Contents of `/run/vercelify/pair-code.json` (written by the CLI, read by
/// the agent daemon).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShortCodeFile {
    pub code: String,
    /// Unix seconds.
    pub expires_at: u64,
    /// The full `vfy1.` code handed out on a successful claim.
    pub pairing: String,
}

impl ShortCodeFile {
    pub fn is_expired(&self, now_unix: u64) -> bool {
        now_unix >= self.expires_at
    }

    /// Constant-time comparison of normalised codes.
    pub fn matches(&self, presented: &str) -> bool {
        let (Some(a), Some(b)) = (normalize_short_code(&self.code), normalize_short_code(presented)) else {
            return false;
        };
        a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

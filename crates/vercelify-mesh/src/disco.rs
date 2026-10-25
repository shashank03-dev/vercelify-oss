//! Disco (design §13.6): small sealed messages between two nodes, on the
//! WireGuard socket or through the relay, that find and confirm direct
//! paths. A ping carries a transaction id; the pong echoes it with the
//! address the ping came from, so every peer works as a STUN server for
//! the others; `call_me` (through the relay) makes both sides ping at the
//! same moment, which opens most NATs.
//!
//! Frame: `VFYD ‖ sender public key (32) ‖ nonce (12) ‖ sealed body`, the
//! body sealed with ChaCha20-Poly1305 under
//! `HKDF-SHA256(X25519(mine, theirs), info "vercelify-disco-v1")`, with the
//! first 36 bytes as associated data. Only the two nodes can make or read
//! one; a forged sender key fails to open.

use crate::keys::{NodeKey, PublicKey};
use crate::{MeshError, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub const MAGIC: &[u8; 4] = b"VFYD";
const HEADER: usize = 4 + 32 + 12;
const INFO: &[u8] = b"vercelify-disco-v1";
/// Endpoints in one `call_me` at most.
pub const MAX_CALL_ME: usize = 16;

pub type TxId = [u8; 12];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Ping {
        tx: TxId,
    },
    /// `observed`: where the ping came from, as the answering node saw it.
    Pong {
        tx: TxId,
        observed: SocketAddr,
    },
    /// "Ping me now at these": the sender's candidate endpoints.
    CallMe {
        endpoints: Vec<SocketAddr>,
    },
}

pub fn is_disco(datagram: &[u8]) -> bool {
    datagram.len() >= HEADER && &datagram[..4] == MAGIC
}

/// The sender key of a frame (unauthenticated until it opens).
pub fn sender(datagram: &[u8]) -> Option<PublicKey> {
    if !is_disco(datagram) {
        return None;
    }
    let mut k = [0u8; 32];
    k.copy_from_slice(&datagram[4..36]);
    Some(PublicKey(k))
}

/// The key two nodes share for disco.
pub struct DiscoKey(ChaCha20Poly1305);

impl DiscoKey {
    pub fn new(mine: &NodeKey, theirs: &PublicKey) -> Result<Self> {
        let shared = mine.shared_secret(theirs);
        if shared.iter().all(|b| *b == 0) {
            return Err(MeshError::Invalid("unusable public key".into()));
        }
        let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, &shared[..]);
        let mut k = zeroize::Zeroizing::new([0u8; 32]);
        hk.expand(INFO, &mut *k)
            .map_err(|_| MeshError::Invalid("key derivation".into()))?;
        Ok(Self(ChaCha20Poly1305::new(Key::from_slice(&*k))))
    }

    pub fn seal(&self, me: &PublicKey, m: &Message) -> Vec<u8> {
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).expect("OS RNG available");
        let mut out = Vec::with_capacity(HEADER + 64);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&me.0);
        out.extend_from_slice(&nonce);
        let body = encode(m);
        let ct = self
            .0
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &body,
                    aad: &out[..36],
                },
            )
            .expect("sealing can't fail");
        out.extend_from_slice(&ct);
        out
    }

    pub fn open(&self, frame: &[u8]) -> Option<Message> {
        if !is_disco(frame) {
            return None;
        }
        let plain = self
            .0
            .decrypt(
                Nonce::from_slice(&frame[36..HEADER]),
                Payload {
                    msg: &frame[HEADER..],
                    aad: &frame[..36],
                },
            )
            .ok()?;
        decode(&plain)
    }
}

fn put_addr(out: &mut Vec<u8>, a: &SocketAddr) {
    match a.ip() {
        IpAddr::V4(ip) => {
            out.push(4);
            out.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            out.push(6);
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&a.port().to_be_bytes());
}

fn get_addr(b: &[u8]) -> Option<(SocketAddr, usize)> {
    match *b.first()? {
        4 => {
            let o: [u8; 4] = b.get(1..5)?.try_into().ok()?;
            let port = u16::from_be_bytes(b.get(5..7)?.try_into().ok()?);
            Some((SocketAddr::new(IpAddr::V4(Ipv4Addr::from(o)), port), 7))
        }
        6 => {
            let o: [u8; 16] = b.get(1..17)?.try_into().ok()?;
            let port = u16::from_be_bytes(b.get(17..19)?.try_into().ok()?);
            Some((SocketAddr::new(IpAddr::V6(Ipv6Addr::from(o)), port), 19))
        }
        _ => None,
    }
}

fn encode(m: &Message) -> Vec<u8> {
    let mut out = vec![];
    match m {
        Message::Ping { tx } => {
            out.push(1);
            out.extend_from_slice(tx);
        }
        Message::Pong { tx, observed } => {
            out.push(2);
            out.extend_from_slice(tx);
            put_addr(&mut out, observed);
        }
        Message::CallMe { endpoints } => {
            out.push(3);
            let n = endpoints.len().min(MAX_CALL_ME);
            out.push(n as u8);
            for e in &endpoints[..n] {
                put_addr(&mut out, e);
            }
        }
    }
    out
}

fn decode(b: &[u8]) -> Option<Message> {
    let tx = |b: &[u8]| -> Option<TxId> { b.get(1..13)?.try_into().ok() };
    match *b.first()? {
        1 => Some(Message::Ping { tx: tx(b)? }),
        2 => {
            let (observed, _) = get_addr(b.get(13..)?)?;
            Some(Message::Pong { tx: tx(b)?, observed })
        }
        3 => {
            let n = (*b.get(1)? as usize).min(MAX_CALL_ME);
            let mut pos = 2;
            let mut endpoints = Vec::with_capacity(n);
            for _ in 0..n {
                let (a, used) = get_addr(b.get(pos..)?)?;
                endpoints.push(a);
                pos += used;
            }
            Some(Message::CallMe { endpoints })
        }
        _ => None,
    }
}

pub fn new_tx() -> TxId {
    let mut tx = [0u8; 12];
    getrandom::fill(&mut tx).expect("OS RNG available");
    tx
}

/// Private (LAN) addresses: preferred over public ones at a similar delay.
pub fn is_private(a: &SocketAddr) -> bool {
    match a.ip() {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_link_local()
                || ip.is_loopback()
                || ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 64
        }
        IpAddr::V6(ip) => {
            (ip.segments()[0] & 0xfe00) == 0xfc00 || (ip.segments()[0] & 0xffc0) == 0xfe80 || ip.is_loopback()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_both_ways_and_only_for_the_pair() {
        let a = NodeKey::generate();
        let b = NodeKey::generate();
        let ab = DiscoKey::new(&a, &b.public()).unwrap();
        let ba = DiscoKey::new(&b, &a.public()).unwrap();
        let msgs = [
            Message::Ping { tx: new_tx() },
            Message::Pong {
                tx: new_tx(),
                observed: "81.2.3.4:41641".parse().unwrap(),
            },
            Message::Pong {
                tx: new_tx(),
                observed: "[2001:db8::1]:51820".parse().unwrap(),
            },
            Message::CallMe {
                endpoints: vec!["192.168.1.5:51820".parse().unwrap(), "[2001:db8::2]:9".parse().unwrap()],
            },
        ];
        for m in &msgs {
            let f = ab.seal(&a.public(), m);
            assert!(is_disco(&f));
            assert_eq!(sender(&f), Some(a.public()));
            assert_eq!(ba.open(&f).as_ref(), Some(m));
        }
        // A third node can't read or forge it; a changed byte fails.
        let c = NodeKey::generate();
        let f = ab.seal(&a.public(), &msgs[0]);
        assert!(DiscoKey::new(&c, &a.public()).unwrap().open(&f).is_none());
        let mut bad = f.clone();
        bad[10] ^= 1; // the claimed sender
        assert!(ba.open(&bad).is_none());
        let mut bad = f;
        let n = bad.len() - 1;
        bad[n] ^= 1;
        assert!(ba.open(&bad).is_none());
        // Not disco: WireGuard and STUN.
        assert!(!is_disco(&[1, 0, 0, 0, 9, 9]));
        assert!(!is_disco(&[0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42]));
    }

    #[test]
    fn junk_bodies_are_rejected() {
        assert!(decode(&[]).is_none());
        assert!(decode(&[1, 2, 3]).is_none());
        assert!(decode(&[9; 20]).is_none());
        assert!(decode(&[3, 2, 4, 1, 2, 3]).is_none());
        // A call_me claiming more endpoints than it holds.
        let mut b = vec![3, 200];
        put_addr(&mut b, &"1.2.3.4:5".parse().unwrap());
        assert!(decode(&b).is_none());
    }

    #[test]
    fn private_addresses() {
        for a in [
            "192.168.1.2:1",
            "10.0.0.1:1",
            "172.20.1.1:1",
            "100.100.1.1:1",
            "[fd00::1]:1",
            "[fe80::1]:1",
        ] {
            assert!(is_private(&a.parse().unwrap()), "{a}");
        }
        for a in ["81.2.3.4:1", "[2001:db8::1]:1", "100.200.1.1:1"] {
            assert!(!is_private(&a.parse().unwrap()), "{a}");
        }
    }
}

//! STUN binding requests (RFC 5389, the part a mesh needs), on the
//! WireGuard socket: a node learns its public address from STUN servers
//! before any peer answers it, and answers binding requests itself, so any
//! server on the mesh can be someone else's STUN server.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

const COOKIE: [u8; 4] = [0x21, 0x12, 0xa4, 0x42];
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const ATTR_MAPPED: u16 = 0x0001;
const ATTR_XOR_MAPPED: u16 = 0x0020;

pub type TxId = [u8; 12];

/// A STUN message: two zero bits, the magic cookie at offset 4.
pub fn is_stun(b: &[u8]) -> bool {
    b.len() >= 20 && b[0] & 0xc0 == 0 && b[4..8] == COOKIE
}

pub fn binding_request(tx: &TxId) -> [u8; 20] {
    let mut m = [0u8; 20];
    m[..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    m[4..8].copy_from_slice(&COOKIE);
    m[8..20].copy_from_slice(tx);
    m
}

fn tx_of(b: &[u8]) -> TxId {
    b[8..20].try_into().expect("20 bytes checked")
}

fn xor_addr(addr: &SocketAddr, tx: &TxId) -> Vec<u8> {
    let port = addr.port() ^ 0x2112;
    let mut v = vec![0u8];
    match addr.ip() {
        IpAddr::V4(ip) => {
            v.push(1);
            v.extend_from_slice(&port.to_be_bytes());
            for (i, o) in ip.octets().iter().enumerate() {
                v.push(o ^ COOKIE[i]);
            }
        }
        IpAddr::V6(ip) => {
            v.push(2);
            v.extend_from_slice(&port.to_be_bytes());
            let mut key = [0u8; 16];
            key[..4].copy_from_slice(&COOKIE);
            key[4..].copy_from_slice(tx);
            for (i, o) in ip.octets().iter().enumerate() {
                v.push(o ^ key[i]);
            }
        }
    }
    v
}

/// The success answer to a binding request from `from` (None for anything else).
pub fn answer(req: &[u8], from: SocketAddr) -> Option<Vec<u8>> {
    if !is_stun(req) || u16::from_be_bytes([req[0], req[1]]) != BINDING_REQUEST {
        return None;
    }
    let tx = tx_of(req);
    let value = xor_addr(&from, &tx);
    let mut m = Vec::with_capacity(20 + 4 + value.len());
    m.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
    m.extend_from_slice(&((4 + value.len()) as u16).to_be_bytes());
    m.extend_from_slice(&COOKIE);
    m.extend_from_slice(&tx);
    m.extend_from_slice(&ATTR_XOR_MAPPED.to_be_bytes());
    m.extend_from_slice(&(value.len() as u16).to_be_bytes());
    m.extend_from_slice(&value);
    Some(m)
}

fn parse_addr(v: &[u8], xor: bool, tx: &TxId) -> Option<SocketAddr> {
    let family = *v.get(1)?;
    let mut port = u16::from_be_bytes([*v.get(2)?, *v.get(3)?]);
    if xor {
        port ^= 0x2112;
    }
    match family {
        1 => {
            let mut o: [u8; 4] = v.get(4..8)?.try_into().ok()?;
            if xor {
                for (i, b) in o.iter_mut().enumerate() {
                    *b ^= COOKIE[i];
                }
            }
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(o)), port))
        }
        2 => {
            let mut o: [u8; 16] = v.get(4..20)?.try_into().ok()?;
            if xor {
                let mut key = [0u8; 16];
                key[..4].copy_from_slice(&COOKIE);
                key[4..].copy_from_slice(tx);
                for (i, b) in o.iter_mut().enumerate() {
                    *b ^= key[i];
                }
            }
            Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(o)), port))
        }
        _ => None,
    }
}

/// The address a binding success reports, with its transaction id.
pub fn parse_success(b: &[u8]) -> Option<(TxId, SocketAddr)> {
    if !is_stun(b) || u16::from_be_bytes([b[0], b[1]]) != BINDING_SUCCESS {
        return None;
    }
    let tx = tx_of(b);
    let len = u16::from_be_bytes([b[2], b[3]]) as usize;
    let attrs = b.get(20..20 + len)?;
    let mut pos = 0;
    let mut mapped = None;
    while pos + 4 <= attrs.len() {
        let t = u16::from_be_bytes([attrs[pos], attrs[pos + 1]]);
        let l = u16::from_be_bytes([attrs[pos + 2], attrs[pos + 3]]) as usize;
        let v = attrs.get(pos + 4..pos + 4 + l)?;
        match t {
            ATTR_XOR_MAPPED => return Some((tx, parse_addr(v, true, &tx)?)),
            ATTR_MAPPED => mapped = parse_addr(v, false, &tx),
            _ => {}
        }
        pos += 4 + l.div_ceil(4) * 4;
    }
    mapped.map(|a| (tx, a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_answer_round_trip() {
        let tx = [7u8; 12];
        let req = binding_request(&tx);
        assert!(is_stun(&req));
        for from in ["81.2.3.4:41641", "[2001:db8::7]:51820"] {
            let from: SocketAddr = from.parse().unwrap();
            let resp = answer(&req, from).unwrap();
            assert_eq!(parse_success(&resp), Some((tx, from)));
            // An answer isn't answered.
            assert!(answer(&resp, from).is_none());
        }
        assert!(!is_stun(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
    }

    #[test]
    fn parses_a_real_server_answer() {
        // A binding success as RFC 5769 §2.2 gives it (192.0.2.1:32853),
        // with its SOFTWARE attribute before XOR-MAPPED-ADDRESS.
        let resp: [u8; 60] = [
            0x01, 0x01, 0x00, 0x3c, 0x21, 0x12, 0xa4, 0x42, 0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87,
            0xdf, 0xae, 0x80, 0x22, 0x00, 0x0b, 0x74, 0x65, 0x73, 0x74, 0x20, 0x76, 0x65, 0x63, 0x74, 0x6f, 0x72, 0x20,
            0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xa1, 0x47, 0xe1, 0x12, 0xa6, 0x43, 0x00, 0x08, 0x00, 0x14, 0, 0, 0, 0,
            0, 0, 0, 0,
        ];
        // (Length trimmed to what we hold: the MESSAGE-INTEGRITY value is cut.)
        let mut r = resp.to_vec();
        r[2..4].copy_from_slice(&40u16.to_be_bytes());
        let (_, addr) = parse_success(&r).unwrap();
        assert_eq!(addr, "192.0.2.1:32853".parse().unwrap());
        assert!(parse_success(&r[..30]).is_none());
    }
}

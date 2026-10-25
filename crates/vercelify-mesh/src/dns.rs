//! The mesh's name server (design §13.8): `<node>.vfy` and `*.<node>.vfy`
//! answer the node's mesh addresses (A and AAAA); every other name is
//! refused, so the system resolver asks its usual servers. A pure
//! query → answer function; the daemon serves it on the mesh address.

use crate::proto::Dns;
use std::net::{Ipv4Addr, Ipv6Addr};

const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;
const TYPE_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
const TTL: u32 = 30;
const NOERROR: u8 = 0;
const FORMERR: u8 = 1;
const NXDOMAIN: u8 = 3;
const NOTIMP: u8 = 4;
const REFUSED: u8 = 5;

/// The node a name in the zone belongs to: `pi5.vfy` and `blog.pi5.vfy` →
/// `pi5`. None for names outside the zone.
pub fn node_of<'a>(name: &'a str, zone: &str) -> Option<&'a str> {
    let n = name.strip_suffix('.').unwrap_or(name);
    let rest = n.strip_suffix(zone)?.strip_suffix('.')?;
    rest.rsplit('.').next().filter(|s| !s.is_empty())
}

/// Read a (possibly compressed) name at `pos`; returns it lowercased and
/// the position after it in the original message.
fn read_name(msg: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = vec![];
    let mut end = None;
    let mut jumps = 0;
    loop {
        let len = *msg.get(pos)? as usize;
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xc0 == 0xc0 {
            let ptr = ((len & 0x3f) << 8) | *msg.get(pos + 1)? as usize;
            if end.is_none() {
                end = Some(pos + 2);
            }
            jumps += 1;
            if jumps > 16 {
                return None;
            }
            pos = ptr;
            continue;
        }
        if len > 63 {
            return None;
        }
        let label = msg.get(pos + 1..pos + 1 + len)?;
        labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
        pos += 1 + len;
        if labels.len() > 127 {
            return None;
        }
    }
    Some((labels.join("."), end.unwrap_or(pos)))
}

fn u16_at(msg: &[u8], pos: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*msg.get(pos)?, *msg.get(pos + 1)?]))
}

/// Answer one query. None for anything not worth an answer (not a query,
/// too short to carry an id).
pub fn answer(query: &[u8], zone: &Dns) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }
    let flags = u16_at(query, 2)?;
    if flags & 0x8000 != 0 {
        return None; // a response, not a query
    }
    let opcode = (flags >> 11) & 0xf;
    let qdcount = u16_at(query, 4)?;
    let rd = flags & 0x0100;
    let reply = |rcode: u8, question: &[u8], answers: &[Vec<u8>], aa: bool| {
        let mut out = Vec::with_capacity(512);
        out.extend_from_slice(&query[..2]);
        let f: u16 = 0x8000 | if aa { 0x0400 } else { 0 } | rd | rcode as u16;
        out.extend_from_slice(&f.to_be_bytes());
        out.extend_from_slice(&(if question.is_empty() { 0u16 } else { 1 }).to_be_bytes());
        out.extend_from_slice(&(answers.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(question);
        for a in answers {
            out.extend_from_slice(a);
        }
        out
    };
    if opcode != 0 {
        return Some(reply(NOTIMP, &[], &[], false));
    }
    if qdcount != 1 {
        return Some(reply(FORMERR, &[], &[], false));
    }
    let Some((name, after)) = read_name(query, 12) else {
        return Some(reply(FORMERR, &[], &[], false));
    };
    let (Some(qtype), Some(qclass)) = (u16_at(query, after), u16_at(query, after + 2)) else {
        return Some(reply(FORMERR, &[], &[], false));
    };
    let question = &query[12..after + 4];
    let Some(node) = node_of(&name, &zone.zone) else {
        return Some(reply(REFUSED, question, &[], false));
    };
    if qclass != CLASS_IN {
        return Some(reply(REFUSED, question, &[], false));
    }
    let v4 = zone.records.get(node).and_then(|s| s.parse::<Ipv4Addr>().ok());
    let v6 = zone
        .records
        .get(&format!("{node}/6"))
        .and_then(|s| s.parse::<Ipv6Addr>().ok());
    if v4.is_none() && v6.is_none() {
        return Some(reply(NXDOMAIN, question, &[], true));
    }
    // Answers point back at the question's name (offset 12).
    let rr = |rtype: u16, data: &[u8]| {
        let mut r = vec![0xc0, 12];
        r.extend_from_slice(&rtype.to_be_bytes());
        r.extend_from_slice(&CLASS_IN.to_be_bytes());
        r.extend_from_slice(&TTL.to_be_bytes());
        r.extend_from_slice(&(data.len() as u16).to_be_bytes());
        r.extend_from_slice(data);
        r
    };
    let mut answers = vec![];
    if matches!(qtype, TYPE_A | TYPE_ANY) {
        if let Some(ip) = v4 {
            answers.push(rr(TYPE_A, &ip.octets()));
        }
    }
    if matches!(qtype, TYPE_AAAA | TYPE_ANY) {
        if let Some(ip) = v6 {
            answers.push(rr(TYPE_AAAA, &ip.octets()));
        }
    }
    // A name that exists without the type asked: NOERROR, no answers.
    Some(reply(NOERROR, question, &answers, true))
}

#[cfg(feature = "daemon")]
pub mod server {
    use super::*;
    use std::sync::{Arc, RwLock};
    use tokio::net::UdpSocket;

    /// Serve the zone on `sock` until the task is dropped.
    pub async fn serve(sock: UdpSocket, zone: Arc<RwLock<Dns>>) {
        let mut buf = [0u8; 1500];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut buf).await else {
                continue;
            };
            let resp = {
                let z = zone.read().unwrap_or_else(|e| e.into_inner());
                answer(&buf[..n], &z)
            };
            if let Some(r) = resp {
                let _ = sock.send_to(&r, from).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn zone() -> Dns {
        let mut records = BTreeMap::new();
        records.insert("pi5".into(), "10.77.0.3".into());
        records.insert("pi5/6".into(), "fd76:6679:6d65::3".into());
        records.insert("main".into(), "10.77.0.1".into());
        Dns {
            zone: "vfy".into(),
            records,
        }
    }

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0xab, 0xcd, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for l in name.split('.') {
            q.push(l.len() as u8);
            q.extend_from_slice(l.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    fn rcode(r: &[u8]) -> u8 {
        r[3] & 0x0f
    }

    fn ancount(r: &[u8]) -> u16 {
        u16::from_be_bytes([r[6], r[7]])
    }

    #[test]
    fn names_of_the_zone() {
        assert_eq!(node_of("pi5.vfy", "vfy"), Some("pi5"));
        assert_eq!(node_of("blog.pi5.vfy.", "vfy"), Some("pi5"));
        assert_eq!(node_of("vfy", "vfy"), None);
        assert_eq!(node_of("pi5.vfyx", "vfy"), None);
        assert_eq!(node_of("example.com", "vfy"), None);
    }

    #[test]
    fn answers_a_and_aaaa() {
        let r = answer(&query("PI5.vfy", TYPE_A), &zone()).unwrap();
        assert_eq!(&r[..2], &[0xab, 0xcd], "same id");
        assert_eq!(rcode(&r), 0);
        assert_eq!(ancount(&r), 1);
        assert_eq!(&r[r.len() - 4..], &[10, 77, 0, 3]);
        assert!(r[2] & 0x04 != 0, "authoritative");
        assert!(r[2] & 0x01 != 0, "recursion desired is echoed");

        let r = answer(&query("blog.pi5.vfy", TYPE_AAAA), &zone()).unwrap();
        assert_eq!(ancount(&r), 1);
        assert_eq!(
            &r[r.len() - 16..],
            &"fd76:6679:6d65::3".parse::<Ipv6Addr>().unwrap().octets()
        );

        let r = answer(&query("main.vfy", TYPE_AAAA), &zone()).unwrap();
        assert_eq!((rcode(&r), ancount(&r)), (0, 0), "exists, no AAAA");
    }

    #[test]
    fn refuses_and_fails_cleanly() {
        let r = answer(&query("example.com", TYPE_A), &zone()).unwrap();
        assert_eq!(rcode(&r), 5, "other names: refused");
        let r = answer(&query("nope.vfy", TYPE_A), &zone()).unwrap();
        assert_eq!(rcode(&r), 3, "unknown node: NXDOMAIN");
        assert!(answer(&[1, 2, 3], &zone()).is_none());
        // A response is not answered.
        let mut resp = query("pi5.vfy", TYPE_A);
        resp[2] |= 0x80;
        assert!(answer(&resp, &zone()).is_none());
        // Truncated question.
        let q = query("pi5.vfy", TYPE_A);
        let r = answer(&q[..q.len() - 3], &zone()).unwrap();
        assert_eq!(rcode(&r), 1);
        // A compression loop is refused, not followed forever.
        let mut lp = vec![0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        lp.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1]);
        assert_eq!(rcode(&answer(&lp, &zone()).unwrap()), 1);
    }
}

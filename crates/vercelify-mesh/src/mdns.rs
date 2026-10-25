//! Finding a coordinator on the LAN (design §13.3): the agent of a main
//! server answers multicast DNS for `_vercelify._tcp.local`; the app (or
//! `vercelify-mesh discover`) asks and gets the server's name, LAN
//! addresses, control port and WireGuard key, so a short pairing code is
//! all the user types. Pure packet building and parsing; the sockets are
//! the callers'.
//!
//! Nothing here is trusted: anyone on the LAN can answer. Pairing over a
//! discovered address is protected by the code itself (a PAKE, `pake.rs`).

use std::net::{IpAddr, Ipv4Addr};

pub const SERVICE: &str = "_vercelify._tcp.local";
pub const MDNS_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const MDNS_PORT: u16 = 5353;

const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_SRV: u16 = 33;
const TYPE_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
/// "Cache flush" on unique records (RFC 6762 §10.2).
const FLUSH: u16 = 0x8000;
const TTL: u32 = 120;

/// What a coordinator says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    /// The server's name (`main`, a hostname).
    pub name: String,
    pub addresses: Vec<Ipv4Addr>,
    /// The mesh control listener (enrollment, pairing, relay).
    pub control_port: u16,
    /// The coordinator's WireGuard key (base64).
    pub coordinator_wg: String,
    /// Whether a short pairing code is waiting.
    pub pairing_open: bool,
}

fn put_name(out: &mut Vec<u8>, name: &str) {
    for label in name.trim_end_matches('.').split('.') {
        let l = &label.as_bytes()[..label.len().min(63)];
        out.push(l.len() as u8);
        out.extend_from_slice(l);
    }
    out.push(0);
}

fn header(id: u16, flags: u16, qd: u16, an: u16, ar: u16) -> Vec<u8> {
    let mut h = Vec::with_capacity(512);
    for v in [id, flags, qd, an, 0, ar] {
        h.extend_from_slice(&v.to_be_bytes());
    }
    h
}

/// A query for the service. `unicast`: ask for the answer to come back to
/// the sender (the QU bit), which a browser on an ephemeral port wants.
pub fn query(unicast: bool) -> Vec<u8> {
    let mut q = header(0, 0, 1, 0, 0);
    put_name(&mut q, SERVICE);
    q.extend_from_slice(&TYPE_PTR.to_be_bytes());
    q.extend_from_slice(&(CLASS_IN | if unicast { 0x8000 } else { 0 }).to_be_bytes());
    q
}

fn record(out: &mut Vec<u8>, name: &str, rtype: u16, class: u16, data: &[u8]) {
    put_name(out, name);
    out.extend_from_slice(&rtype.to_be_bytes());
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&TTL.to_be_bytes());
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
}

fn label_safe(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .take(40)
        .collect();
    if s.is_empty() {
        "vercelify".into()
    } else {
        s
    }
}

/// The answer to a query for the service (PTR, SRV, TXT and A records).
pub fn response(a: &Announcement, id: u16) -> Vec<u8> {
    let label = label_safe(&a.name);
    let instance = format!("{label}.{SERVICE}");
    let host = format!("{label}.local");
    let mut out = header(id, 0x8400, 0, 3 + a.addresses.len() as u16, 0);
    let mut ptr = vec![];
    put_name(&mut ptr, &instance);
    record(&mut out, SERVICE, TYPE_PTR, CLASS_IN, &ptr);
    let mut srv = vec![0, 0, 0, 0];
    srv.extend_from_slice(&a.control_port.to_be_bytes());
    put_name(&mut srv, &host);
    record(&mut out, &instance, TYPE_SRV, CLASS_IN | FLUSH, &srv);
    let mut txt = vec![];
    for kv in [
        "v=1".to_string(),
        format!("name={}", a.name),
        format!("wg={}", a.coordinator_wg),
        format!("pair={}", if a.pairing_open { 1 } else { 0 }),
    ] {
        let b = &kv.as_bytes()[..kv.len().min(255)];
        txt.push(b.len() as u8);
        txt.extend_from_slice(b);
    }
    record(&mut out, &instance, TYPE_TXT, CLASS_IN | FLUSH, &txt);
    for ip in &a.addresses {
        record(&mut out, &host, TYPE_A, CLASS_IN | FLUSH, &ip.octets());
    }
    out
}

fn u16_at(m: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*m.get(p)?, *m.get(p + 1)?]))
}

/// A (possibly compressed) name at `pos`, and the position after it.
fn read_name(m: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = vec![];
    let mut end = None;
    let mut jumps = 0;
    loop {
        let len = *m.get(pos)? as usize;
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xc0 == 0xc0 {
            end.get_or_insert(pos + 2);
            jumps += 1;
            if jumps > 16 {
                return None;
            }
            pos = ((len & 0x3f) << 8) | *m.get(pos + 1)? as usize;
            continue;
        }
        labels.push(String::from_utf8_lossy(m.get(pos + 1..pos + 1 + len)?).into_owned());
        pos += 1 + len;
        if labels.len() > 64 {
            return None;
        }
    }
    Some((labels.join("."), end.unwrap_or(pos)))
}

/// Whether a message is a query that asks for the service; and whether it
/// wants a unicast answer.
pub fn asks_for_service(m: &[u8]) -> Option<bool> {
    let flags = u16_at(m, 2)?;
    if flags & 0x8000 != 0 {
        return None;
    }
    let qd = u16_at(m, 4)?;
    let mut pos = 12;
    let mut unicast = None;
    for _ in 0..qd.min(16) {
        let (name, after) = read_name(m, pos)?;
        let qtype = u16_at(m, after)?;
        let qclass = u16_at(m, after + 2)?;
        pos = after + 4;
        if name.eq_ignore_ascii_case(SERVICE) && matches!(qtype, TYPE_PTR | TYPE_ANY) {
            unicast = Some(unicast.unwrap_or(false) || qclass & 0x8000 != 0);
        }
    }
    unicast
}

/// What a response announces (None when it isn't about the service).
pub fn parse_response(m: &[u8]) -> Option<Announcement> {
    let flags = u16_at(m, 2)?;
    if flags & 0x8000 == 0 {
        return None;
    }
    let (qd, an, ns, ar) = (u16_at(m, 4)?, u16_at(m, 6)?, u16_at(m, 8)?, u16_at(m, 10)?);
    let mut pos = 12;
    for _ in 0..qd {
        let (_, after) = read_name(m, pos)?;
        pos = after + 4;
    }
    let mut a = Announcement {
        name: String::new(),
        addresses: vec![],
        control_port: 0,
        coordinator_wg: String::new(),
        pairing_open: false,
    };
    let mut ours = false;
    for _ in 0..(an as usize + ns as usize + ar as usize).min(64) {
        let (name, after) = read_name(m, pos)?;
        let rtype = u16_at(m, after)?;
        let len = u16_at(m, after + 8)? as usize;
        let data_at = after + 10;
        let data = m.get(data_at..data_at + len)?;
        pos = data_at + len;
        let in_service = name.to_ascii_lowercase().ends_with(&SERVICE.to_ascii_lowercase());
        match rtype {
            TYPE_PTR if name.eq_ignore_ascii_case(SERVICE) => ours = true,
            TYPE_SRV if in_service && data.len() >= 7 => {
                a.control_port = u16::from_be_bytes([data[4], data[5]]);
            }
            TYPE_TXT if in_service => {
                let mut p = 0;
                while p < data.len() {
                    let l = data[p] as usize;
                    let kv = String::from_utf8_lossy(data.get(p + 1..p + 1 + l)?).into_owned();
                    p += 1 + l;
                    match kv.split_once('=') {
                        Some(("name", v)) => a.name = v.to_string(),
                        Some(("wg", v)) => a.coordinator_wg = v.to_string(),
                        Some(("pair", v)) => a.pairing_open = v == "1",
                        _ => {}
                    }
                }
            }
            TYPE_A if len == 4 => {
                let ip = Ipv4Addr::new(data[0], data[1], data[2], data[3]);
                if !a.addresses.contains(&ip) {
                    a.addresses.push(ip);
                }
            }
            _ => {}
        }
    }
    (ours && a.control_port != 0 && !a.coordinator_wg.is_empty()).then_some(a)
}

/// Where to reach a discovered server's control listener, preferring the
/// address the answer came from.
pub fn control_addresses(a: &Announcement, from: Option<IpAddr>) -> Vec<String> {
    let mut v = vec![];
    if let Some(IpAddr::V4(ip)) = from {
        if a.addresses.contains(&ip) {
            v.push(format!("{ip}:{}", a.control_port));
        }
    }
    for ip in &a.addresses {
        let s = format!("{ip}:{}", a.control_port);
        if !v.contains(&s) {
            v.push(s);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann() -> Announcement {
        Announcement {
            name: "old-thinkpad".into(),
            addresses: vec![Ipv4Addr::new(192, 168, 1, 20), Ipv4Addr::new(10, 0, 0, 5)],
            control_port: 7443,
            coordinator_wg: "a2V5a2V5a2V5a2V5a2V5a2V5a2V5a2V5a2V5a2V5a2U=".into(),
            pairing_open: true,
        }
    }

    #[test]
    fn query_and_answer_round_trip() {
        let q = query(true);
        assert_eq!(asks_for_service(&q), Some(true));
        assert_eq!(asks_for_service(&query(false)), Some(false));
        let r = response(&ann(), 0);
        assert_eq!(asks_for_service(&r), None, "an answer isn't a question");
        assert_eq!(parse_response(&r), Some(ann()));
        assert_eq!(
            control_addresses(&ann(), Some("10.0.0.5".parse().unwrap())),
            vec!["10.0.0.5:7443", "192.168.1.20:7443"]
        );
    }

    #[test]
    fn other_services_and_junk() {
        // A query for something else isn't ours to answer.
        let mut q = header(0, 0, 1, 0, 0);
        put_name(&mut q, "_airplay._tcp.local");
        q.extend_from_slice(&TYPE_PTR.to_be_bytes());
        q.extend_from_slice(&CLASS_IN.to_be_bytes());
        assert_eq!(asks_for_service(&q), None);
        assert!(parse_response(&q).is_none());
        assert!(parse_response(&[0x00, 0x00, 0x84, 0x00, 0, 0, 0, 9]).is_none());
        assert!(asks_for_service(&[1, 2]).is_none());
        // A truncated answer is dropped, not misread.
        let r = response(&ann(), 0);
        assert!(parse_response(&r[..r.len() - 3]).is_none());
        // A name that isn't a DNS label is made one.
        let mut a = ann();
        a.name = "Old ThinkPad!".into();
        assert_eq!(parse_response(&response(&a, 0)).unwrap().name, "Old ThinkPad!");
    }
}

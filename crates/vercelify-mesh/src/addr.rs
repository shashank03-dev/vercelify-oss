//! Mesh addresses. Every node gets an index n in 1..=65534; the coordinator
//! has index 1. The index maps onto an IPv4 /16 (default 10.77.0.0/16, configurable for
//! LANs that already use it) and onto a fixed IPv6 ULA /48.

use crate::{MeshError, Result};
use std::net::{Ipv4Addr, Ipv6Addr};

pub const DEFAULT_IPV4_PREFIX: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 0);
/// fd76:6679:6d65::/48 ("vfy" "me" in hex, a ULA prefix).
pub const IPV6_PREFIX: [u16; 3] = [0xfd76, 0x6679, 0x6d65];
pub const COORDINATOR_INDEX: u16 = 1;
pub const MAX_INDEX: u16 = 65534;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Addressing {
    /// Network address of the IPv4 /16 (low 16 bits zero).
    pub ipv4_prefix: Ipv4Addr,
}

impl Default for Addressing {
    fn default() -> Self {
        Self {
            ipv4_prefix: DEFAULT_IPV4_PREFIX,
        }
    }
}

impl Addressing {
    pub fn new(ipv4_prefix: Ipv4Addr) -> Result<Self> {
        let o = ipv4_prefix.octets();
        if o[2] != 0 || o[3] != 0 {
            return Err(MeshError::Invalid(format!(
                "{ipv4_prefix} is not a /16 network address (x.y.0.0)"
            )));
        }
        if !ipv4_prefix.is_private() && !(o[0] == 100 && (64..128).contains(&o[1])) {
            return Err(MeshError::Invalid(format!(
                "{ipv4_prefix}/16 is not a private range (use 10.x, 172.16–31.x, 192.168 or 100.64/10)"
            )));
        }
        Ok(Self { ipv4_prefix })
    }

    pub fn ipv4(&self, index: u16) -> Ipv4Addr {
        let o = self.ipv4_prefix.octets();
        Ipv4Addr::new(o[0], o[1], (index >> 8) as u8, (index & 0xff) as u8)
    }

    pub fn ipv6(&self, index: u16) -> Ipv6Addr {
        let [a, b, c] = IPV6_PREFIX;
        Ipv6Addr::new(a, b, c, 0, 0, 0, 0, index)
    }

    /// The index of a mesh address, if it is one.
    pub fn index_of_v4(&self, ip: Ipv4Addr) -> Option<u16> {
        let (p, o) = (self.ipv4_prefix.octets(), ip.octets());
        (p[0] == o[0] && p[1] == o[1])
            .then(|| u16::from_be_bytes([o[2], o[3]]))
            .filter(|i| valid_index(*i))
    }

    pub fn contains_v4(&self, ip: Ipv4Addr) -> bool {
        let (p, o) = (self.ipv4_prefix.octets(), ip.octets());
        p[0] == o[0] && p[1] == o[1]
    }

    pub fn cidr_v4(&self) -> String {
        format!("{}/16", self.ipv4_prefix)
    }

    pub fn cidr_v6(&self) -> String {
        let [a, b, c] = IPV6_PREFIX;
        format!("{a:x}:{b:x}:{c:x}::/48")
    }
}

pub fn valid_index(i: u16) -> bool {
    (1..=MAX_INDEX).contains(&i)
}

/// The lowest free index (> 1, the coordinator's) not in `used`.
pub fn next_free_index(used: &[u16]) -> Option<u16> {
    let mut taken = vec![false; MAX_INDEX as usize + 1];
    for &u in used {
        if let Some(t) = taken.get_mut(u as usize) {
            *t = true;
        }
    }
    (2..=MAX_INDEX).find(|i| !taken[*i as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_map_to_addresses() {
        let a = Addressing::default();
        assert_eq!(a.ipv4(1), Ipv4Addr::new(10, 77, 0, 1));
        assert_eq!(a.ipv4(258), Ipv4Addr::new(10, 77, 1, 2));
        assert_eq!(a.ipv6(258).to_string(), "fd76:6679:6d65::102");
        assert_eq!(a.index_of_v4(Ipv4Addr::new(10, 77, 1, 2)), Some(258));
        assert_eq!(a.index_of_v4(Ipv4Addr::new(10, 78, 1, 2)), None);
        assert_eq!(a.index_of_v4(Ipv4Addr::new(10, 77, 0, 0)), None, "network address");
        assert_eq!(a.index_of_v4(Ipv4Addr::new(10, 77, 255, 255)), None, "broadcast");
        assert_eq!(a.cidr_v4(), "10.77.0.0/16");
        assert_eq!(a.cidr_v6(), "fd76:6679:6d65::/48");
    }

    #[test]
    fn prefixes_must_be_private_slash_16() {
        assert!(Addressing::new(Ipv4Addr::new(10, 99, 0, 0)).is_ok());
        assert!(Addressing::new(Ipv4Addr::new(100, 100, 0, 0)).is_ok());
        assert!(Addressing::new(Ipv4Addr::new(10, 99, 1, 0)).is_err());
        assert!(Addressing::new(Ipv4Addr::new(8, 8, 0, 0)).is_err());
    }

    #[test]
    fn allocation() {
        assert_eq!(next_free_index(&[]), Some(2));
        assert_eq!(next_free_index(&[1, 2, 3, 5]), Some(4));
        let all: Vec<u16> = (1..=MAX_INDEX).collect();
        assert_eq!(next_free_index(&all), None);
    }
}

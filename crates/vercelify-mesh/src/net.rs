//! This machine's network interfaces: the addresses a node announces as
//! its endpoints (and the coordinator puts in join tokens).

use std::net::Ipv4Addr;

/// The node's own IPv4 addresses on other interfaces: what peers on the
/// same networks can reach it at. Loopback, link-local, the mesh itself and
/// container bridges are left out.
pub fn local_ipv4s(skip_prefix: Option<Ipv4Addr>) -> Vec<Ipv4Addr> {
    let mut out = vec![];
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return out;
    }
    let mut cur = head;
    while !cur.is_null() {
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_addr.is_null() || unsafe { (*ifa.ifa_addr).sa_family } as i32 != libc::AF_INET {
            continue;
        }
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();
        if !usable_interface(&name) || (ifa.ifa_flags & libc::IFF_UP as u32) == 0 {
            continue;
        }
        let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
        let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
        if ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() {
            continue;
        }
        if let Some(p) = skip_prefix {
            if p.octets()[..2] == ip.octets()[..2] {
                continue;
            }
        }
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    unsafe { libc::freeifaddrs(head) };
    out
}

/// Interfaces worth announcing: not loopback, not ours, not Docker's or
/// another VPN's.
pub fn usable_interface(name: &str) -> bool {
    !(name == "lo"
        || name.starts_with("vfy")
        || name.starts_with("docker")
        || name.starts_with("br-")
        || name.starts_with("veth")
        || name.starts_with("virbr")
        || name.starts_with("cni")
        || name.starts_with("flannel"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interfaces() {
        assert!(usable_interface("eth0") && usable_interface("wlan0") && usable_interface("enp3s0"));
        for n in ["lo", "vfy0", "docker0", "br-1a2b", "veth9"] {
            assert!(!usable_interface(n), "{n}");
        }
        assert!(local_ipv4s(None).iter().all(|ip| !ip.is_loopback()));
    }
}

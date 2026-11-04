//! This machine's network interfaces: the addresses a node announces as
//! its endpoints (and the coordinator puts in join tokens).

use std::net::Ipv4Addr;

/// The node's own IPv4 addresses on other interfaces: what peers on the
/// same networks can reach it at. Loopback, link-local, the mesh itself and
/// container bridges are left out. Addresses on another VPN's interface
/// (Tailscale, WireGuard, ZeroTier) come last: they are kept for join tokens,
/// where they may be the only way in, but never announced as WireGuard
/// endpoints (see [`endpoint_ipv4s`]).
pub fn local_ipv4s(skip_prefix: Option<Ipv4Addr>) -> Vec<Ipv4Addr> {
    let (mut own, overlay): (Vec<_>, Vec<_>) = interface_ipv4s(skip_prefix).into_iter().partition(|(_, o)| !o);
    own.extend(overlay);
    own.into_iter().map(|(ip, _)| ip).collect()
}

/// The addresses announced as this node's WireGuard endpoints: the
/// interface addresses without another VPN's. A mesh path through another
/// overlay stalls whenever that overlay changes its own path (Tailscale
/// moving between its relay and a direct route did for 40 s), while the LAN
/// address underneath reaches the same peers directly.
pub fn endpoint_ipv4s(skip_prefix: Option<Ipv4Addr>) -> Vec<Ipv4Addr> {
    interface_ipv4s(skip_prefix)
        .into_iter()
        .filter(|(_, overlay)| !overlay)
        .map(|(ip, _)| ip)
        .collect()
}

#[cfg(windows)]
fn interface_ipv4s(skip_prefix: Option<Ipv4Addr>) -> Vec<(Ipv4Addr, bool)> {
    // The address of the interface the default route uses: a UDP socket
    // "connected" to a public address learns it without sending anything.
    // Enough for LAN endpoints; peers elsewhere are found by STUN.
    let ip = std::net::UdpSocket::bind(("0.0.0.0", 0))
        .and_then(|s| s.connect(("1.1.1.1", 53)).map(|_| s))
        .and_then(|s| s.local_addr())
        .ok()
        .and_then(|a| match a.ip() {
            std::net::IpAddr::V4(v) => Some(v),
            _ => None,
        });
    ip.into_iter()
        .filter(|ip| !(ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()))
        .filter(|ip| skip_prefix.is_none_or(|p| p.octets()[..2] != ip.octets()[..2]))
        .map(|ip| (ip, false))
        .collect()
}

#[cfg(unix)]
fn interface_ipv4s(skip_prefix: Option<Ipv4Addr>) -> Vec<(Ipv4Addr, bool)> {
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
        if !out.iter().any(|(o, _)| *o == ip) {
            out.push((ip, overlay_interface(&name)));
        }
    }
    unsafe { libc::freeifaddrs(head) };
    out
}

/// Another VPN's interface: Tailscale (`tailscale0`, `utun*` on macOS),
/// WireGuard (`wg*`), ZeroTier (`zt*`) and other tunnels.
pub fn overlay_interface(name: &str) -> bool {
    ["tailscale", "utun", "wg", "zt", "tun", "nordlynx"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// Interfaces with reachable addresses: not loopback, not ours, not Docker's.
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
        for n in ["tailscale0", "utun4", "wg0", "ztabcdef", "tun0", "nordlynx"] {
            assert!(overlay_interface(n) && usable_interface(n), "{n}");
        }
        for n in ["eth0", "wlp1s0", "en0", "enp3s0"] {
            assert!(!overlay_interface(n), "{n}");
        }
        let all = local_ipv4s(None);
        let announced = endpoint_ipv4s(None);
        assert!(announced.iter().all(|ip| all.contains(ip)));
        // Own interfaces first: the announced addresses lead the full list.
        assert_eq!(&all[..announced.len()], &announced[..]);
    }
}

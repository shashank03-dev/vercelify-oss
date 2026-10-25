//! The Linux network device `vfy0` (design §13.5): `/dev/net/tun` in
//! `IFF_TUN | IFF_NO_PI` mode, configured with plain ioctls (address,
//! netmask, MTU, up; IPv6 through an `AF_INET6` socket) so no `ip` binary is
//! needed. The netmask makes the kernel route the whole mesh to it.

use crate::wg::Device;
use std::ffi::CString;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use tokio::io::unix::AsyncFd;

pub const DEFAULT_NAME: &str = "vfy0";
/// Small enough for any path (IPv6 minimum), WireGuard's overhead included.
pub const MTU: i32 = 1280;

const TUNSETIFF: libc::c_ulong = 0x4004_54ca;
const IFF_TUN: libc::c_short = 0x0001;
const IFF_NO_PI: libc::c_short = 0x1000;

#[repr(C)]
struct IfReqFlags {
    name: [libc::c_char; libc::IFNAMSIZ],
    flags: libc::c_short,
    _pad: [u8; 22],
}

#[repr(C)]
struct IfReqAddr {
    name: [libc::c_char; libc::IFNAMSIZ],
    addr: libc::sockaddr_in,
    _pad: [u8; 8],
}

#[repr(C)]
struct IfReqInt {
    name: [libc::c_char; libc::IFNAMSIZ],
    value: libc::c_int,
    _pad: [u8; 20],
}

#[repr(C)]
struct In6IfReq {
    addr: libc::in6_addr,
    prefixlen: u32,
    ifindex: libc::c_int,
}

fn ifname(name: &str) -> io::Result<[libc::c_char; libc::IFNAMSIZ]> {
    let c = CString::new(name).map_err(|_| io::Error::other("bad interface name"))?;
    let b = c.as_bytes_with_nul();
    if b.len() > libc::IFNAMSIZ {
        return Err(io::Error::other("interface name too long"));
    }
    let mut out = [0 as libc::c_char; libc::IFNAMSIZ];
    for (o, i) in out.iter_mut().zip(b) {
        *o = *i as libc::c_char;
    }
    Ok(out)
}

fn check(r: libc::c_int) -> io::Result<()> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn socket(family: libc::c_int) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::socket(family, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn sockaddr(ip: Ipv4Addr) -> libc::sockaddr_in {
    libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(ip.octets()),
        },
        sin_zero: [0; 8],
    }
}

fn prefix_mask(len: u8) -> Ipv4Addr {
    Ipv4Addr::from(if len == 0 { 0 } else { u32::MAX << (32 - len as u32) })
}

pub struct Tun {
    fd: AsyncFd<OwnedFd>,
    pub name: String,
}

impl Tun {
    /// Create (or attach to) the tun device `name`.
    pub fn open(name: &str) -> io::Result<Self> {
        let fd = unsafe {
            libc::open(
                c"/dev/net/tun".as_ptr(),
                libc::O_RDWR | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut req = IfReqFlags {
            name: ifname(name)?,
            flags: IFF_TUN | IFF_NO_PI,
            _pad: [0; 22],
        };
        check(unsafe { libc::ioctl(fd.as_raw_fd(), TUNSETIFF as _, &mut req) })?;
        // SAFETY: an OwnedFd keeps its descriptor open and unchanged for as
        // long as the AsyncFd owns it.
        let fd = unsafe { AsyncFd::register(fd) }.map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self {
            fd,
            name: name.to_string(),
        })
    }

    /// Address, netmask (routes the mesh here), MTU and up.
    pub fn configure(&self, ipv4: Ipv4Addr, prefix_len: u8, ipv6: Option<(Ipv6Addr, u8)>) -> io::Result<()> {
        let s = socket(libc::AF_INET)?;
        let name = ifname(&self.name)?;
        let mut a = IfReqAddr {
            name,
            addr: sockaddr(ipv4),
            _pad: [0; 8],
        };
        check(unsafe { libc::ioctl(s.as_raw_fd(), libc::SIOCSIFADDR as _, &mut a) })?;
        a.addr = sockaddr(prefix_mask(prefix_len));
        check(unsafe { libc::ioctl(s.as_raw_fd(), libc::SIOCSIFNETMASK as _, &mut a) })?;
        let mut m = IfReqInt {
            name,
            value: MTU,
            _pad: [0; 20],
        };
        check(unsafe { libc::ioctl(s.as_raw_fd(), libc::SIOCSIFMTU as _, &mut m) })?;
        let mut f = IfReqFlags {
            name,
            flags: 0,
            _pad: [0; 22],
        };
        check(unsafe { libc::ioctl(s.as_raw_fd(), libc::SIOCGIFFLAGS as _, &mut f) })?;
        f.flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
        check(unsafe { libc::ioctl(s.as_raw_fd(), libc::SIOCSIFFLAGS as _, &mut f) })?;
        if let Some((ip6, len)) = ipv6 {
            // IPv6 may be off on the host: a mesh without it still works.
            if let Err(e) = self.add_ipv6(ip6, len) {
                tracing::warn!("{}: no IPv6 address ({e})", self.name);
            }
        }
        Ok(())
    }

    fn add_ipv6(&self, ip: Ipv6Addr, prefix_len: u8) -> io::Result<()> {
        let s = socket(libc::AF_INET6)?;
        let mut idx = IfReqInt {
            name: ifname(&self.name)?,
            value: 0,
            _pad: [0; 20],
        };
        check(unsafe { libc::ioctl(s.as_raw_fd(), libc::SIOCGIFINDEX as _, &mut idx) })?;
        let mut r = In6IfReq {
            addr: libc::in6_addr { s6_addr: ip.octets() },
            prefixlen: prefix_len as u32,
            ifindex: idx.value,
        };
        match check(unsafe { libc::ioctl(s.as_raw_fd(), libc::SIOCSIFADDR as _, &mut r) }) {
            Err(e) if e.raw_os_error() == Some(libc::EEXIST) => Ok(()),
            other => other,
        }
    }

    pub fn raw_fd(&self) -> RawFd {
        self.fd.get_ref().as_raw_fd()
    }
}

impl Device for Tun {
    async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut g = self.fd.readable().await?;
            match g.try_io(|fd| {
                let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(r) => return r,
                Err(_would_block) => continue,
            }
        }
    }

    async fn send(&self, packet: &[u8]) -> io::Result<()> {
        loop {
            let mut g = self.fd.writable().await?;
            match g.try_io(|fd| {
                let n = unsafe { libc::write(fd.as_raw_fd(), packet.as_ptr().cast(), packet.len()) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }) {
                Ok(r) => return r,
                Err(_would_block) => continue,
            }
        }
    }
}

pub use crate::net::{local_ipv4s, usable_interface};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_and_names() {
        assert_eq!(prefix_mask(16), Ipv4Addr::new(255, 255, 0, 0));
        assert_eq!(prefix_mask(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(prefix_mask(0), Ipv4Addr::new(0, 0, 0, 0));
        assert!(ifname("vfy0").is_ok());
        assert!(ifname("a-very-long-interface-name").is_err());
    }

    /// Needs root (CAP_NET_ADMIN): `VERCELIFY_TUN_TESTS=1`.
    #[tokio::test]
    async fn a_real_tun_device() {
        if std::env::var("VERCELIFY_TUN_TESTS").as_deref() != Ok("1") {
            return;
        }
        let t = Tun::open("vfytest0").expect("tun");
        t.configure(
            Ipv4Addr::new(10, 199, 0, 1),
            16,
            Some(("fd76:6679:6d65::99".parse().unwrap(), 48)),
        )
        .unwrap();
        // A UDP datagram to another mesh address comes out of the device.
        let s = std::net::UdpSocket::bind("10.199.0.1:0").unwrap();
        s.send_to(b"through the tun", "10.199.0.2:9999").unwrap();
        let mut buf = [0u8; 1500];
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let n = t.recv(&mut buf).await.unwrap();
                if buf[..n].ends_with(b"through the tun") {
                    return n;
                }
            }
        })
        .await
        .expect("the packet");
        assert_eq!(buf[0] >> 4, 4);
        assert_eq!(&buf[16..20], &[10, 199, 0, 2]);
        assert!(n > 28);
    }
}

//! The macOS tun device for system mode (design §13.5): a `utun`
//! interface from the kernel control `com.apple.net.utun_control`, run as
//! root by the LaunchDaemon. Packets carry a 4-byte address family first.
//! Addresses, routes and the `vfy` resolver are set with the system's own
//! tools (`ifconfig`, `route`, `/etc/resolver/vfy`).

use crate::wg::Device;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Command;
use tokio::io::unix::AsyncFd;

const UTUN_CONTROL: &[u8] = b"com.apple.net.utun_control";
const UTUN_OPT_IFNAME: libc::c_int = 2;

pub struct Utun {
    fd: AsyncFd<OwnedFd>,
    pub name: String,
}

fn check(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

fn run(cmd: &str, args: &[&str]) -> io::Result<()> {
    let out = Command::new(cmd).args(args).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "{cmd} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

impl Utun {
    /// A new utun interface (the kernel picks its number).
    pub fn open() -> io::Result<Self> {
        unsafe {
            let fd = check(libc::socket(libc::PF_SYSTEM, libc::SOCK_DGRAM, libc::SYSPROTO_CONTROL))?;
            let owned = OwnedFd::from_raw_fd(fd);
            let mut info: libc::ctl_info = std::mem::zeroed();
            for (i, b) in UTUN_CONTROL.iter().enumerate() {
                info.ctl_name[i] = *b as libc::c_char;
            }
            check(libc::ioctl(fd, libc::CTLIOCGINFO, &mut info))?;
            let mut addr: libc::sockaddr_ctl = std::mem::zeroed();
            addr.sc_len = std::mem::size_of::<libc::sockaddr_ctl>() as u8;
            addr.sc_family = libc::AF_SYSTEM as u8;
            addr.ss_sysaddr = libc::AF_SYS_CONTROL as u16;
            addr.sc_id = info.ctl_id;
            addr.sc_unit = 0; // the next free utunN
            check(libc::connect(
                fd,
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_ctl>() as libc::socklen_t,
            ))?;
            let mut name = [0u8; 32];
            let mut len = name.len() as libc::socklen_t;
            check(libc::getsockopt(
                fd,
                libc::SYSPROTO_CONTROL,
                UTUN_OPT_IFNAME,
                name.as_mut_ptr() as *mut libc::c_void,
                &mut len,
            ))?;
            let name = String::from_utf8_lossy(&name[..(len as usize).saturating_sub(1)]).into_owned();
            let flags = check(libc::fcntl(fd, libc::F_GETFL))?;
            check(libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK))?;
            // SAFETY: an OwnedFd keeps its descriptor open and unchanged for
            // as long as the AsyncFd owns it.
            let fd = AsyncFd::register(owned).map_err(|e| io::Error::other(e.to_string()))?;
            Ok(Self { fd, name })
        }
    }

    /// Addresses and routes for the mesh's prefixes on this interface.
    pub fn configure(&self, ipv4: Ipv4Addr, prefix_len: u8, ipv6: Option<(Ipv6Addr, u8)>) -> io::Result<()> {
        let mask = Ipv4Addr::from(if prefix_len == 0 {
            0
        } else {
            u32::MAX << (32 - prefix_len as u32)
        });
        let a = ipv4.to_string();
        run(
            "ifconfig",
            &[
                &self.name,
                "inet",
                &a,
                &a,
                "netmask",
                &mask.to_string(),
                "mtu",
                "1280",
                "up",
            ],
        )?;
        let net = Ipv4Addr::from(u32::from(ipv4) & u32::from(mask));
        let _ = run(
            "route",
            &["-q", "-n", "delete", "-inet", &format!("{net}/{prefix_len}")],
        );
        run(
            "route",
            &[
                "-q",
                "-n",
                "add",
                "-inet",
                &format!("{net}/{prefix_len}"),
                "-interface",
                &self.name,
            ],
        )?;
        if let Some((ip, len)) = ipv6 {
            run(
                "ifconfig",
                &[&self.name, "inet6", &ip.to_string(), "prefixlen", &len.to_string()],
            )?;
            let mask = if len == 0 { 0 } else { u128::MAX << (128 - len as u32) };
            let net6 = Ipv6Addr::from(u128::from(ip) & mask);
            let _ = run("route", &["-q", "-n", "delete", "-inet6", &format!("{net6}/{len}")]);
            let _ = run(
                "route",
                &[
                    "-q",
                    "-n",
                    "add",
                    "-inet6",
                    &format!("{net6}/{len}"),
                    "-interface",
                    &self.name,
                ],
            );
        }
        Ok(())
    }
}

/// `*.vfy` through the mesh's name server (macOS reads /etc/resolver/<domain>).
pub fn configure_resolver(ip: &str, port: u16) -> io::Result<()> {
    write_resolver(std::path::Path::new("/etc/resolver"), ip, port)
}

fn write_resolver(dir: &std::path::Path, ip: &str, port: u16) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let existed = dir.exists();
    std::fs::create_dir_all(dir)?;
    if !existed {
        // This is public system configuration, even under a private umask.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    }
    let path = dir.join("vfy");
    std::fs::write(&path, format!("# vercelify-mesh\nnameserver {ip}\nport {port}\n"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
}

impl Device for Utun {
    async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut frame = vec![0u8; buf.len() + 4];
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|fd| {
                let n = unsafe {
                    libc::read(
                        fd.get_ref().as_raw_fd(),
                        frame.as_mut_ptr() as *mut libc::c_void,
                        frame.len(),
                    )
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(Ok(n)) if n > 4 => {
                    buf[..n - 4].copy_from_slice(&frame[4..n]);
                    return Ok(n - 4);
                }
                Ok(Ok(_)) => continue,
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }

    async fn send(&self, packet: &[u8]) -> io::Result<()> {
        let family: u32 = match packet.first().map(|b| b >> 4) {
            Some(6) => libc::AF_INET6 as u32,
            _ => libc::AF_INET as u32,
        };
        let mut frame = Vec::with_capacity(packet.len() + 4);
        frame.extend_from_slice(&family.to_be_bytes());
        frame.extend_from_slice(packet);
        loop {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|fd| {
                let n = unsafe {
                    libc::write(
                        fd.get_ref().as_raw_fd(),
                        frame.as_ptr() as *const libc::c_void,
                        frame.len(),
                    )
                };
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

#[cfg(test)]
mod resolver_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn resolver_uses_the_loopback_listener_port_and_is_system_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resolver");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("vfy"), "old").unwrap();
        std::fs::set_permissions(path.join("vfy"), std::fs::Permissions::from_mode(0o600)).unwrap();
        write_resolver(&path, "127.0.0.1", 53053).unwrap();
        let text = std::fs::read_to_string(path.join("vfy")).unwrap();
        assert!(text.contains("nameserver 127.0.0.1\nport 53053\n"), "{text}");
        assert_eq!(
            std::fs::metadata(path.join("vfy")).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}

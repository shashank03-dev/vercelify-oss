//! The coordinator answers multicast DNS for `_vercelify._tcp.local`
//! (design §13.3), so the Mac app finds it on the LAN and the user only
//! types a short pairing code. Shares port 5353 with Avahi (SO_REUSEPORT).

use crate::config::Config;
use crate::mesh::Coordinator;
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use vercelify_mesh::mdns::{self, Announcement, MDNS_ADDR, MDNS_PORT};

fn socket() -> std::io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    s.set_reuse_port(true)?;
    s.set_multicast_loop_v4(true)?;
    s.set_nonblocking(true)?;
    s.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, MDNS_PORT)).into())?;
    UdpSocket::from_std(s.into())
}

fn control_port(cfg: &Config) -> u16 {
    cfg.mesh_control_listen
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or(vercelify_mesh::proto::DEFAULT_CONTROL_PORT)
}

/// Answer until the task ends.
pub async fn respond(cfg: Arc<Config>, co: Arc<Coordinator>) {
    let sock = match socket() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("mDNS: port {MDNS_PORT}: {e} (the app can't find this server by itself)");
            return;
        }
    };
    let name = vercelify_mesh::state::State::load(&cfg.mesh_dir)
        .ok()
        .flatten()
        .map(|s| s.name)
        .unwrap_or_else(|| "vercelify".into());
    let mut joined: Vec<Ipv4Addr> = vec![];
    let mut last_multicast = Instant::now() - Duration::from_secs(10);
    let mut rejoin = tokio::time::interval(Duration::from_secs(60));
    let mut buf = vec![0u8; 9000];
    loop {
        tokio::select! {
            _ = rejoin.tick() => {
                // Interfaces come and go (DHCP, Wi-Fi): join on each LAN address.
                for ip in crate::lan_ipv4s() {
                    if !joined.contains(&ip) && sock.join_multicast_v4(MDNS_ADDR, ip).is_ok() {
                        joined.push(ip);
                    }
                }
            }
            r = sock.recv_from(&mut buf) => {
                let Ok((n, from)) = r else { continue };
                let Some(unicast) = mdns::asks_for_service(&buf[..n]) else { continue };
                let a = Announcement {
                    name: name.clone(),
                    addresses: crate::lan_ipv4s(),
                    control_port: control_port(&cfg),
                    coordinator_wg: co.wg_public().to_base64(),
                    pairing_open: crate::pairing::is_open(&cfg),
                };
                // Legacy unicast (a query from another port): answer there,
                // with its id (RFC 6762 §6.7).
                let legacy = from.port() != MDNS_PORT;
                let id = if legacy { u16::from_be_bytes([buf[0], buf[1]]) } else { 0 };
                let resp = mdns::response(&a, id);
                if unicast || legacy {
                    let _ = sock.send_to(&resp, from).await;
                } else if last_multicast.elapsed() >= Duration::from_secs(1) {
                    last_multicast = Instant::now();
                    let _ = sock.send_to(&resp, (MDNS_ADDR, MDNS_PORT)).await;
                }
            }
        }
    }
}

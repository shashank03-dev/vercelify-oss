//! The app's side of pairing on the LAN (design §13.3): find coordinators
//! by mDNS, then trade a short code for the full pairing code with SPAKE2.
//! `vercelify-mesh discover` and `vercelify-mesh pair CODE` use it, and so
//! does the Mac app (through the binary).

use crate::http;
use crate::mdns::{self, Announcement, MDNS_ADDR, MDNS_PORT};
use crate::pake::{self, PairAnswer};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;

/// A coordinator that answered, with where it answered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub announcement: Announcement,
    pub from: IpAddr,
}

impl Found {
    pub fn control(&self) -> Vec<String> {
        mdns::control_addresses(&self.announcement, Some(self.from))
    }
}

/// Ask the LAN for coordinators for `wait`; one entry per server key.
pub async fn discover(wait: Duration) -> std::io::Result<Vec<Found>> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let q = mdns::query(true);
    let deadline = tokio::time::Instant::now() + wait;
    let mut found: Vec<Found> = vec![];
    let mut buf = vec![0u8; 9000];
    // Ask a few times: multicast on Wi-Fi drops packets.
    let mut ask = tokio::time::interval(Duration::from_millis(700));
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = ask.tick() => {
                let _ = sock.send_to(&q, (MDNS_ADDR, MDNS_PORT)).await;
            }
            r = sock.recv_from(&mut buf) => {
                let Ok((n, from)) = r else { continue };
                if let Some(a) = mdns::parse_response(&buf[..n]) {
                    if !found.iter().any(|f| f.announcement.coordinator_wg == a.coordinator_wg) {
                        found.push(Found { announcement: a, from: from.ip() });
                    }
                }
            }
        }
    }
    Ok(found)
}

#[derive(Debug, thiserror::Error)]
pub enum PairError {
    #[error("{0}")]
    Msg(String),
    #[error(transparent)]
    Http(#[from] http::HttpError),
}

/// Trade `code` for the full pairing code at one of `control`'s addresses.
pub async fn claim(control: &[String], code: &str) -> Result<String, PairError> {
    let mut last = None;
    for c in control {
        let url = crate::daemon::control_url(c, pake::PAIR_PATH);
        let (app, req) = pake::app_start(code);
        let body = serde_json::to_value(&req).expect("serializable");
        match http::request("POST", &url, None, Some(&body), Duration::from_secs(15)).await {
            Ok(raw) => {
                let a: PairAnswer =
                    serde_json::from_slice(&raw).map_err(|e| PairError::Msg(format!("pairing answer: {e}")))?;
                let plain = app
                    .finish(&a)
                    .map_err(|_| PairError::Msg("wrong code (or not the server that printed it)".into()))?;
                return String::from_utf8(plain).map_err(|_| PairError::Msg("the pairing isn't text".into()));
            }
            // An answer from the server is final (expired, too many tries).
            Err(e @ http::HttpError::Status { .. }) => return Err(e.into()),
            Err(e) => last = Some(e),
        }
    }
    Err(last
        .map(PairError::from)
        .unwrap_or_else(|| PairError::Msg("no address to ask".into())))
}

/// `host[:port]` as a control address (default port 7443).
pub fn control_of(server: &str) -> String {
    let s = server.trim().trim_end_matches('/');
    if s.contains("://") || s.parse::<SocketAddr>().is_ok() {
        return s.to_string();
    }
    match s.rsplit_once(':') {
        Some((_, p)) if p.parse::<u16>().is_ok() && !s.contains("::") => s.to_string(),
        _ => format!("{s}:{}", crate::proto::DEFAULT_CONTROL_PORT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_addresses() {
        assert_eq!(control_of("192.168.1.20"), "192.168.1.20:7443");
        assert_eq!(control_of("192.168.1.20:9000"), "192.168.1.20:9000");
        assert_eq!(control_of("main.local"), "main.local:7443");
        assert_eq!(control_of("https://mesh.example.com/"), "https://mesh.example.com");
    }
}

//! What app mode offers the Mac (design §13.5): a SOCKS5 proxy on
//! loopback (CONNECT only, `*.vfy` names resolved from the map) and
//! forwards (`127.0.0.1:17420` → `main.vfy:7420`, TCP or UDP), both onto
//! the userspace stack. Forwards are given at start (`--forward`) or by the
//! app while the daemon runs (`<run-dir>/forwards`, for programs that can't
//! use SOCKS: the display stream, Moonlight). Only mesh destinations are
//! served: neither is a way out to the internet.

use crate::netstack::Stack;
use crate::proto::Dns;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// How names and addresses on the mesh are found.
#[derive(Clone)]
pub struct Resolver {
    pub zone: Arc<RwLock<Dns>>,
    /// The mesh's prefixes: anything else is refused.
    pub ipv4_prefix: (Ipv4Addr, u8),
    pub ipv6_prefix: (Ipv6Addr, u8),
}

impl Resolver {
    pub fn on_mesh(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(a) => {
                let (p, len) = self.ipv4_prefix;
                let mask = if len == 0 { 0 } else { u32::MAX << (32 - len as u32) };
                u32::from(a) & mask == u32::from(p) & mask
            }
            IpAddr::V6(a) => {
                let (p, len) = self.ipv6_prefix;
                let mask = if len == 0 { 0 } else { u128::MAX << (128 - len as u32) };
                u128::from(a) & mask == u128::from(p) & mask
            }
        }
    }

    /// `pi5.vfy`, `blog.pi5.vfy`, `pi5` (a bare node name), or an address on the mesh.
    pub fn resolve(&self, host: &str) -> Option<IpAddr> {
        if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
            return self.on_mesh(ip).then_some(ip);
        }
        let z = self.zone.read().unwrap_or_else(|e| e.into_inner());
        let host = host.to_ascii_lowercase();
        let node = crate::dns::node_of(&host, &z.zone).or_else(|| (!host.contains('.')).then_some(host.as_str()))?;
        z.records.get(node).and_then(|s| s.parse().ok())
    }
}

/// `HOST:PORT` → a mesh socket address.
pub fn resolve_target(r: &Resolver, target: &str) -> Option<SocketAddr> {
    let (host, port) = target.rsplit_once(':')?;
    Some(SocketAddr::new(r.resolve(host)?, port.parse().ok()?))
}

/// Copy both ways between a local stream and a stack connection; each
/// side's end closes the other's (a FIN, not a reset).
pub async fn bridge<S>(local: S, conn: crate::netstack::TcpConn)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send,
{
    let crate::netstack::TcpConn { tx, mut rx, wake } = conn;
    let (mut rd, mut wr) = tokio::io::split(local);
    let up_wake = wake.clone();
    let up = async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match rd.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                    up_wake.notify_one();
                }
            }
        }
        drop(tx);
        up_wake.notify_one();
    };
    let down = async move {
        while let Some(chunk) = rx.recv().await {
            wake.notify_one();
            if wr.write_all(&chunk).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
    };
    tokio::join!(up, down);
}

const SOCKS_VERSION: u8 = 5;
const REP_OK: u8 = 0;
const REP_FAILURE: u8 = 1;
const REP_NOT_ALLOWED: u8 = 2;
const REP_HOST_UNREACHABLE: u8 = 4;
const REP_REFUSED: u8 = 5;
const REP_COMMAND: u8 = 7;
const REP_ADDRESS_TYPE: u8 = 8;

async fn reply(s: &mut TcpStream, code: u8) -> std::io::Result<()> {
    s.write_all(&[SOCKS_VERSION, code, 0, 1, 0, 0, 0, 0, 0, 0]).await
}

/// Read a SOCKS5 CONNECT request; the target as `host:port`.
pub async fn read_request(s: &mut TcpStream) -> std::io::Result<Result<String, u8>> {
    let mut head = [0u8; 2];
    s.read_exact(&mut head).await?;
    if head[0] != SOCKS_VERSION {
        return Err(std::io::Error::other("not SOCKS5"));
    }
    let mut methods = vec![0u8; head[1] as usize];
    s.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        // Only "no authentication": it's on loopback.
        s.write_all(&[SOCKS_VERSION, 0xff]).await?;
        return Err(std::io::Error::other("no acceptable method"));
    }
    s.write_all(&[SOCKS_VERSION, 0]).await?;
    let mut req = [0u8; 4];
    s.read_exact(&mut req).await?;
    if req[1] != 1 {
        return Ok(Err(REP_COMMAND));
    }
    let host = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            s.read_exact(&mut a).await?;
            Ipv4Addr::from(a).to_string()
        }
        4 => {
            let mut a = [0u8; 16];
            s.read_exact(&mut a).await?;
            format!("[{}]", Ipv6Addr::from(a))
        }
        3 => {
            let mut len = [0u8; 1];
            s.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            s.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).into_owned()
        }
        _ => return Ok(Err(REP_ADDRESS_TYPE)),
    };
    let mut port = [0u8; 2];
    s.read_exact(&mut port).await?;
    Ok(Ok(format!("{host}:{}", u16::from_be_bytes(port))))
}

async fn socks_client(mut s: TcpStream, stack: Stack, r: Resolver) -> std::io::Result<()> {
    let target = match tokio::time::timeout(Duration::from_secs(10), read_request(&mut s)).await {
        Err(_) => return Ok(()),
        Ok(r) => r?,
    };
    let target = match target {
        Ok(t) => t,
        Err(code) => return reply(&mut s, code).await,
    };
    let Some(to) = resolve_target(&r, &target) else {
        tracing::debug!("socks: {target} isn't on the mesh");
        return reply(&mut s, REP_NOT_ALLOWED).await;
    };
    match stack.connect_tcp(to).await {
        Ok(conn) => {
            reply(&mut s, REP_OK).await?;
            bridge(s, conn).await;
            Ok(())
        }
        Err(e) => {
            tracing::debug!("socks: {target}: {e}");
            let code = match e.kind() {
                std::io::ErrorKind::ConnectionRefused => REP_REFUSED,
                std::io::ErrorKind::TimedOut => REP_HOST_UNREACHABLE,
                _ => REP_FAILURE,
            };
            reply(&mut s, code).await
        }
    }
}

/// Serve SOCKS5 on `listener` until the task is dropped.
pub async fn serve_socks(listener: TcpListener, stack: Stack, r: Resolver) {
    loop {
        let Ok((s, _)) = listener.accept().await else { continue };
        let (stack, r) = (stack.clone(), r.clone());
        tokio::spawn(async move {
            let _ = socks_client(s, stack, r).await;
        });
    }
}

/// `[tcp:|udp:]LISTEN=TARGET`: `127.0.0.1:17420=main.vfy:7420`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Forward {
    pub udp: bool,
    pub listen: SocketAddr,
    pub target: String,
}

impl std::str::FromStr for Forward {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let (udp, rest) = if let Some(r) = s.strip_prefix("udp:") {
            (true, r)
        } else {
            (false, s.strip_prefix("tcp:").unwrap_or(s))
        };
        let (listen, target) = rest
            .split_once('=')
            .ok_or_else(|| format!("`{s}`: expected LISTEN=TARGET (127.0.0.1:17420=main.vfy:7420)"))?;
        let listen: SocketAddr = listen
            .parse()
            .map_err(|_| format!("`{listen}`: expected an address with a port"))?;
        if !listen.ip().is_loopback() {
            return Err(format!("`{listen}`: forwards listen on loopback only"));
        }
        if target
            .rsplit_once(':')
            .and_then(|(host, p)| (!host.is_empty()).then(|| p.parse::<u16>().ok()).flatten())
            .is_none()
        {
            return Err(format!("`{target}`: expected HOST:PORT"));
        }
        Ok(Forward {
            udp,
            listen,
            target: target.to_string(),
        })
    }
}

/// The forwards file: one `[udp:]LISTEN=TARGET` per line, `#` starts a
/// comment (the app tags its lines with their owner). Bad lines are
/// returned separately, so one typo doesn't stop the others.
pub fn parse_forwards(raw: &str) -> (Vec<Forward>, Vec<String>) {
    let (mut ok, mut bad) = (vec![], vec![]);
    for line in raw.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        match line.parse::<Forward>() {
            Ok(f) if !ok.contains(&f) => ok.push(f),
            Ok(_) => {}
            Err(e) => bad.push(e),
        }
    }
    (ok, bad)
}

/// Keep the forwards in `path` running: checked every second, new lines
/// start, removed ones stop. The ones given at start (`fixed`) run on their
/// own and are left alone here.
pub async fn watch_forwards(path: PathBuf, fixed: Vec<Forward>, stack: Stack, r: Resolver) {
    let mut running: HashMap<Forward, tokio::task::JoinHandle<()>> = HashMap::new();
    let mut last: Option<String> = None;
    loop {
        let raw = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        if last.as_deref() != Some(raw.as_str()) {
            let (want, bad) = parse_forwards(&raw);
            for e in bad {
                tracing::warn!("{}: {e}", path.display());
            }
            let want: HashSet<Forward> = want.into_iter().filter(|f| !fixed.contains(f)).collect();
            running.retain(|f, task| {
                let keep = want.contains(f) && !task.is_finished();
                if !keep {
                    task.abort();
                    tracing::info!("forward {} stopped", f.listen);
                }
                keep
            });
            for f in want {
                if running.contains_key(&f) {
                    continue;
                }
                tracing::info!("forward {}{} → {}", if f.udp { "udp " } else { "" }, f.listen, f.target);
                let (stack, r, ff) = (stack.clone(), r.clone(), f.clone());
                running.insert(
                    f,
                    tokio::spawn(async move {
                        if let Err(e) = serve_forward(ff.clone(), stack, r).await {
                            tracing::warn!("forward {}: {e}", ff.listen);
                        }
                    }),
                );
            }
            last = Some(raw);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Run one forward until the task is dropped.
pub async fn serve_forward(f: Forward, stack: Stack, r: Resolver) -> std::io::Result<()> {
    if f.udp {
        return serve_udp_forward(f, stack, r).await;
    }
    let listener = TcpListener::bind(f.listen).await?;
    loop {
        let Ok((s, _)) = listener.accept().await else { continue };
        let (stack, r, target) = (stack.clone(), r.clone(), f.target.clone());
        tokio::spawn(async move {
            let Some(to) = resolve_target(&r, &target) else {
                tracing::debug!("forward: {target} isn't on the mesh (yet)");
                return;
            };
            match stack.connect_tcp(to).await {
                Ok(conn) => bridge(s, conn).await,
                Err(e) => tracing::debug!("forward to {target}: {e}"),
            }
        });
    }
}

/// UDP: datagrams from the last local sender go to the target; answers go
/// back to that sender (the display stream has one sender).
async fn serve_udp_forward(f: Forward, stack: Stack, r: Resolver) -> std::io::Result<()> {
    let sock = Arc::new(UdpSocket::bind(f.listen).await?);
    let mut buf = vec![0u8; 64 * 1024];
    let mut current: Option<(crate::netstack::UdpConn, SocketAddr)> = None;
    loop {
        tokio::select! {
            r_ = sock.recv_from(&mut buf) => {
                let Ok((n, from)) = r_ else { continue };
                let reopen = match &current {
                    Some((c, peer)) => *peer != from || c.tx.is_closed(),
                    None => true,
                };
                if reopen {
                    let Some(to) = resolve_target(&r, &f.target) else { continue };
                    match stack.open_udp(to).await {
                        Ok(c) => current = Some((c, from)),
                        Err(e) => {
                            tracing::debug!("udp forward to {}: {e}", f.target);
                            continue;
                        }
                    }
                }
                if let Some((c, _)) = &current {
                    c.send(buf[..n].to_vec()).await;
                }
            }
            d = async {
                match current.as_mut() {
                    Some((c, _)) => c.recv().await,
                    None => std::future::pending().await,
                }
            } => match d {
                Some(d) => {
                    if let Some((_, peer)) = &current {
                        let _ = sock.send_to(&d, peer).await;
                    }
                }
                None => current = None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn resolver() -> Resolver {
        let mut records = BTreeMap::new();
        records.insert("main".to_string(), "10.77.0.1".to_string());
        records.insert("pi5".to_string(), "10.77.0.3".to_string());
        Resolver {
            zone: Arc::new(RwLock::new(Dns {
                zone: "vfy".into(),
                records,
            })),
            ipv4_prefix: (Ipv4Addr::new(10, 77, 0, 0), 16),
            ipv6_prefix: ("fd76:6679:6d65::".parse().unwrap(), 48),
        }
    }

    #[test]
    fn only_mesh_destinations() {
        let r = resolver();
        assert_eq!(r.resolve("main.vfy"), Some("10.77.0.1".parse().unwrap()));
        assert_eq!(r.resolve("Blog.PI5.vfy"), Some("10.77.0.3".parse().unwrap()));
        assert_eq!(r.resolve("pi5"), Some("10.77.0.3".parse().unwrap()));
        assert_eq!(r.resolve("10.77.9.9"), Some("10.77.9.9".parse().unwrap()));
        assert_eq!(
            r.resolve("[fd76:6679:6d65::4]"),
            Some("fd76:6679:6d65::4".parse().unwrap())
        );
        assert_eq!(r.resolve("example.com"), None);
        assert_eq!(r.resolve("8.8.8.8"), None);
        assert_eq!(r.resolve("nope.vfy"), None);
        assert_eq!(
            resolve_target(&r, "main.vfy:7420"),
            Some("10.77.0.1:7420".parse().unwrap())
        );
        assert!(resolve_target(&r, "main.vfy").is_none());
    }

    #[test]
    fn forwards_parse() {
        let f: Forward = "127.0.0.1:17420=main.vfy:7420".parse().unwrap();
        assert!(!f.udp);
        assert_eq!(f.listen, "127.0.0.1:17420".parse().unwrap());
        let f: Forward = "udp:127.0.0.1:5600=main.vfy:5600".parse().unwrap();
        assert!(f.udp);
        assert!("0.0.0.0:80=main.vfy:80".parse::<Forward>().is_err(), "loopback only");
        assert!("127.0.0.1:80".parse::<Forward>().is_err());
        assert!("127.0.0.1:80=main.vfy".parse::<Forward>().is_err());
    }

    #[test]
    fn forwards_file_rejects_empty_target_host() {
        let (valid, invalid) = parse_forwards("127.0.0.1:17420=:7420\n127.0.0.1:17421=main.vfy:7420\n");
        assert_eq!(valid.len(), 1);
        assert_eq!(invalid.len(), 1);
    }

    #[test]
    fn forwards_file() {
        let (ok, bad) = parse_forwards(
            "# set by the app\nudp:127.0.0.1:15600=10.77.0.1:5600 # sender\n\n127.0.0.1:47989=10.77.0.1:47989 # moonlight\n\
             0.0.0.0:1=10.77.0.1:1\nudp:127.0.0.1:15600=10.77.0.1:5600\n",
        );
        assert_eq!(ok.len(), 2, "comments and repeats dropped: {ok:?}");
        assert!(ok[0].udp && ok[0].target == "10.77.0.1:5600");
        assert_eq!(ok[1].listen, "127.0.0.1:47989".parse().unwrap());
        assert_eq!(bad.len(), 1, "{bad:?}");
    }
}

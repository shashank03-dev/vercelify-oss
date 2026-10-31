//! Private apps in the Mac's normal browser (design §13.5). In app mode the
//! Mac has no route to the mesh, so `http://blog.10.77.0.1.sslip.io` can't
//! open. Browsers send every `*.localhost` name to 127.0.0.1 by themselves,
//! so the app opens `http://blog.10.77.0.1.sslip.io.localhost:7481/` instead:
//! this gateway takes the name back off `.localhost`, finds the server on
//! the mesh and hands the request to its proxy (port 80) with the original
//! Host, which is what the proxy routes on. No system proxy, DNS change or
//! admin rights.
//!
//! Plain HTTP only, one request per connection (`Connection: close`, so
//! each request's Host is rewritten); a WebSocket upgrade keeps its
//! connection. Only mesh destinations are served, as with SOCKS.

use crate::netstack::Stack;
use crate::socks::{bridge, Resolver};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The app port behind the mesh name: the server's reverse proxy.
pub const PROXY_PORT: u16 = 80;
/// Largest request head we read before giving up.
const MAX_HEAD: usize = 32 * 1024;

/// `blog.10.77.0.1.sslip.io.localhost:7481` → `blog.10.77.0.1.sslip.io`.
pub fn original_host(host_header: &str) -> Option<String> {
    let h = host_header.trim().to_ascii_lowercase();
    // Drop the port (no IPv6 literals here: they can't end in .localhost).
    let h = match h.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name.to_string(),
        _ => h,
    };
    let h = h.trim_end_matches('.');
    let name = h.strip_suffix(".localhost")?;
    (!name.is_empty() && name.split('.').all(valid_label)).then(|| name.to_string())
}

fn valid_label(l: &str) -> bool {
    !l.is_empty() && l.len() <= 63 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The address an sslip.io / nip.io name carries (`blog.10.77.0.1.sslip.io`,
/// `blog.10-77-0-1.sslip.io`), read off the name: no DNS needed.
pub fn embedded_ipv4(host: &str) -> Option<Ipv4Addr> {
    let rest = host
        .strip_suffix(".sslip.io")
        .or_else(|| host.strip_suffix(".nip.io"))?;
    let labels: Vec<&str> = rest.split('.').collect();
    // Dotted: the last four labels.
    if labels.len() >= 4 {
        let tail = labels[labels.len() - 4..].join(".");
        if let Ok(ip) = tail.parse() {
            return Some(ip);
        }
    }
    // Dashed: the last label (`10-77-0-1`, or `blog-10-77-0-1`).
    let last = labels.last()?;
    let parts: Vec<&str> = last.split('-').collect();
    (parts.len() >= 4).then(|| parts[parts.len() - 4..].join(".").parse().ok())?
}

/// The request head with the Host put back, and `Connection: close` unless
/// it's an upgrade (WebSocket).
pub fn rewrite_head(head: &str, host: &str) -> String {
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut out = vec![request_line.to_string()];
    let mut upgrade = false;
    let mut headers = vec![];
    for l in lines {
        if l.is_empty() {
            continue;
        }
        let name = l.split(':').next().unwrap_or_default().trim().to_ascii_lowercase();
        if name == "upgrade" {
            upgrade = true;
        }
        headers.push((name, l));
    }
    out.push(format!("Host: {host}"));
    for (name, l) in headers {
        match name.as_str() {
            "host" => {}
            "connection" | "keep-alive" | "proxy-connection" if !upgrade => {}
            _ => out.push(l.to_string()),
        }
    }
    if !upgrade {
        out.push("Connection: close".into());
    }
    out.join("\r\n") + "\r\n\r\n"
}

/// The value of the Host header in a request head.
fn host_of(head: &str) -> Option<&str> {
    head.split("\r\n")
        .skip(1)
        .find_map(|l| l.split_once(':').filter(|(n, _)| n.trim().eq_ignore_ascii_case("host")))
        .map(|(_, v)| v.trim())
}

/// Where the request goes: the mesh address of the server for `host`.
async fn destination(r: &Resolver, host: &str) -> Option<IpAddr> {
    if let Some(ip) = embedded_ipv4(host) {
        return r.on_mesh(IpAddr::V4(ip)).then_some(IpAddr::V4(ip));
    }
    if let Some(ip) = r.resolve(host) {
        return Some(ip);
    }
    // A private domain of your own (`blog.example.com` → a mesh address).
    let addrs = tokio::net::lookup_host((host, PROXY_PORT)).await.ok()?;
    addrs.map(|a| a.ip()).find(|ip| r.on_mesh(*ip))
}

async fn answer(s: &mut TcpStream, status: &str, text: &str) {
    let body = format!("{text}\n");
    let _ = s
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await;
}

async fn handle<C, F>(mut s: TcpStream, r: Resolver, connect: C)
where
    C: FnOnce(SocketAddr) -> F,
    F: std::future::Future<Output = std::io::Result<crate::netstack::TcpConn>>,
{
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_HEAD {
            return answer(&mut s, "431 Request Header Fields Too Large", "request head too large").await;
        }
        match s.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let Ok(head) = std::str::from_utf8(&buf[..end - 4]) else {
        return answer(&mut s, "400 Bad Request", "not an HTTP request").await;
    };
    let Some(host) = host_of(head).and_then(original_host) else {
        return answer(
            &mut s,
            "400 Bad Request",
            "open an app as http://<its address>.localhost:7481/ (the Vercelify app does this for you)",
        )
        .await;
    };
    let Some(ip) = destination(&r, &host).await else {
        return answer(
            &mut s,
            "502 Bad Gateway",
            &format!("{host} isn't on your Vercelify network"),
        )
        .await;
    };
    let conn = match connect(SocketAddr::new(ip, PROXY_PORT)).await {
        Ok(c) => c,
        Err(e) => {
            return answer(
                &mut s,
                "502 Bad Gateway",
                &format!("can't reach {host} over Vercelify's network ({e}): is the server up?"),
            )
            .await
        }
    };
    let mut first = rewrite_head(head, &host).into_bytes();
    first.extend_from_slice(&buf[end..]);
    if conn.tx.send(first).await.is_err() {
        return;
    }
    conn.wake.notify_one();
    bridge(s, conn).await;
}

/// Serve the gateway on `listener` (loopback) until the daemon stops.
pub async fn serve(listener: TcpListener, stack: Stack, r: Resolver) {
    loop {
        let Ok((s, from)) = listener.accept().await else {
            continue;
        };
        // Loopback only: this is a door into the mesh.
        if !from.ip().is_loopback() {
            continue;
        }
        let stack = stack.clone();
        tokio::spawn(handle(s, r.clone(), move |addr| async move {
            stack.connect_tcp(addr).await
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn exchange(upgrade: bool) {
        use std::sync::{Arc, RwLock};
        use tokio::sync::{mpsc, Notify};
        let r = Resolver {
            zone: Arc::new(RwLock::new(crate::proto::Dns::default())),
            ipv4_prefix: (Ipv4Addr::new(10, 77, 0, 0), 16),
            ipv6_prefix: ("fd76:6679:6d65::".parse().unwrap(), 48),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut browser = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let (tx, mut upstream) = mpsc::channel(4);
        let (reply, rx) = mpsc::channel(4);
        let task = tokio::spawn(handle(socket, r, move |addr| async move {
            assert_eq!(addr, "10.77.0.1:80".parse().unwrap());
            Ok(crate::netstack::TcpConn {
                tx,
                rx,
                wake: Arc::new(Notify::new()),
            })
        }));
        let headers = if upgrade {
            "Connection: Upgrade\r\nUpgrade: websocket\r\n"
        } else {
            ""
        };
        let request =
            format!("GET /path?q=1 HTTP/1.1\r\nHost: blog.10.77.0.1.sslip.io.localhost:7481\r\n{headers}\r\n");
        browser.write_all(request.as_bytes()).await.unwrap();
        let first = String::from_utf8(upstream.recv().await.unwrap()).unwrap();
        assert!(first.contains("Host: blog.10.77.0.1.sslip.io\r\n"));
        assert!(!first.contains(".localhost"));
        assert!(first.starts_with("GET /path?q=1 HTTP/1.1"));
        if upgrade {
            assert!(first.contains("Connection: Upgrade\r\n"));
            reply
                .send(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n".to_vec())
                .await
                .unwrap();
        } else {
            assert!(first.contains("Connection: close\r\n"));
            reply
                .send(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec())
                .await
                .unwrap();
        }
        let mut response = vec![0; 1024];
        let n = browser.read(&mut response).await.unwrap();
        assert!(response[..n].starts_with(if upgrade { b"HTTP/1.1 101" } else { b"HTTP/1.1 200" }));
        if upgrade {
            // Bytes after the upgrade are opaque, including binary frames.
            let frame = vec![0x82, 0x03, 0x00, 0xff, 0x01];
            browser.write_all(&frame).await.unwrap();
            assert_eq!(upstream.recv().await.unwrap(), frame);
            reply.send(frame.clone()).await.unwrap();
            let mut received = vec![0; frame.len()];
            browser.read_exact(&mut received).await.unwrap();
            assert_eq!(received, frame);
        }
        drop(reply);
        drop(browser);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn http_round_trip_preserves_host_and_response() {
        tokio::time::timeout(std::time::Duration::from_secs(3), exchange(false))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn websocket_round_trip_passes_binary_frames_both_ways() {
        tokio::time::timeout(std::time::Duration::from_secs(3), exchange(true))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn zone_records_outside_the_mesh_are_refused() {
        let r = Resolver {
            zone: std::sync::Arc::new(std::sync::RwLock::new(crate::proto::Dns {
                zone: "vfy".into(),
                records: [("outside".into(), "8.8.8.8".into())].into(),
            })),
            ipv4_prefix: (Ipv4Addr::new(10, 77, 0, 0), 16),
            ipv6_prefix: ("fd76:6679:6d65::".parse().unwrap(), 48),
        };
        assert_eq!(destination(&r, "outside.vfy").await, None);
    }

    #[test]
    fn names_come_back_off_localhost() {
        assert_eq!(
            original_host("blog.10.77.0.1.sslip.io.localhost:7481").as_deref(),
            Some("blog.10.77.0.1.sslip.io")
        );
        assert_eq!(
            original_host("Blog.Example.com.localhost").as_deref(),
            Some("blog.example.com")
        );
        assert_eq!(original_host("localhost:7481"), None, "nothing to open");
        assert_eq!(original_host("blog.example.com"), None, "not ours");
        assert_eq!(original_host("bad_name.localhost"), None);
    }

    #[test]
    fn sslip_names_carry_their_address() {
        assert_eq!(
            embedded_ipv4("blog.10.77.0.1.sslip.io"),
            Some(Ipv4Addr::new(10, 77, 0, 1))
        );
        assert_eq!(
            embedded_ipv4("blog.10-77-0-1.sslip.io"),
            Some(Ipv4Addr::new(10, 77, 0, 1))
        );
        assert_eq!(
            embedded_ipv4("blog-10-77-0-4.nip.io"),
            Some(Ipv4Addr::new(10, 77, 0, 4))
        );
        assert_eq!(
            embedded_ipv4("pr-3.blog.10.77.0.1.sslip.io"),
            Some(Ipv4Addr::new(10, 77, 0, 1))
        );
        assert_eq!(embedded_ipv4("blog.example.com"), None);
    }

    #[test]
    fn the_host_is_put_back_and_each_request_gets_its_own_connection() {
        let head = "GET /a?b=1 HTTP/1.1\r\nHost: blog.10.77.0.1.sslip.io.localhost:7481\r\nConnection: keep-alive\r\nAccept: */*";
        let out = rewrite_head(head, "blog.10.77.0.1.sslip.io");
        assert!(
            out.starts_with("GET /a?b=1 HTTP/1.1\r\nHost: blog.10.77.0.1.sslip.io\r\n"),
            "{out}"
        );
        assert!(out.contains("Accept: */*\r\n"));
        assert!(out.contains("Connection: close\r\n") && !out.contains("keep-alive"));
        assert!(out.ends_with("\r\n\r\n"));
        assert_eq!(out.matches("Host:").count(), 1);
    }

    #[test]
    fn a_websocket_upgrade_keeps_its_connection() {
        let head = "GET /ws HTTP/1.1\r\nHost: x.localhost:7481\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: k";
        let out = rewrite_head(head, "x");
        assert!(out.contains("Connection: Upgrade") && out.contains("Upgrade: websocket"));
        assert!(!out.contains("Connection: close"));
    }

    #[test]
    fn the_host_header_is_found_case_insensitively() {
        assert_eq!(host_of("GET / HTTP/1.1\r\nhOsT:  a.localhost\r\n"), Some("a.localhost"));
        assert_eq!(host_of("GET / HTTP/1.1\r\nAccept: x"), None);
    }
}

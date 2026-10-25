//! The daemon's HTTP client: HTTP/1.1, plain inside the mesh (the
//! coordinator's agent at its mesh address, or on 127.0.0.1) and for the
//! sealed enrollment on a LAN; TLS (webpki roots) for a public control URL
//! behind a Cloudflare tunnel. `connect_io` is shared with the relay client.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::rustls;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("cannot reach {0}: {1}")]
    Connect(String, String),
    #[error("{0}")]
    Io(String),
    #[error("HTTP {status}: {message}")]
    Status { status: u16, message: String },
}

pub struct Url {
    pub tls: bool,
    /// For TLS server names.
    pub host: String,
    pub host_port: String,
    pub path: String,
}

/// `http(s)://host[:port]/path` (and `ws(s)://`) → parts.
pub fn parse_url(url: &str) -> Result<Url, HttpError> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("http://").or_else(|| url.strip_prefix("ws://")) {
        (false, r)
    } else if let Some(r) = url.strip_prefix("https://").or_else(|| url.strip_prefix("wss://")) {
        (true, r)
    } else {
        return Err(HttpError::Io(format!("{url}: not an http(s):// URL")));
    };
    let (hp, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if hp.is_empty() {
        return Err(HttpError::Io(format!("{url}: no host")));
    }
    // `[v6]:port`, `host:port`, or a bare host (default port).
    let has_port = match hp.rfind(']') {
        Some(b) => hp[b..].contains(':'),
        None => hp.contains(':'),
    };
    let host_port = if has_port {
        hp.to_string()
    } else {
        format!("{hp}:{}", if tls { 443 } else { 80 })
    };
    let host = match host_port.rsplit_once(':') {
        Some((h, _)) => h.trim_matches(['[', ']']).to_string(),
        None => hp.to_string(),
    };
    Ok(Url {
        tls,
        host,
        host_port,
        path: path.to_string(),
    })
}

/// A byte stream: TCP, or TLS over TCP.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("ring supports the default versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            Arc::new(cfg)
        })
        .clone()
}

/// In app mode the kernel has no route into the mesh: the daemon's own
/// requests to mesh addresses (the map stream, endpoint reports, renewals)
/// go over the userspace stack instead.
static MESH_DIALER: OnceLock<(crate::netstack::Stack, crate::socks::Resolver)> = OnceLock::new();

pub fn dial_mesh_with(stack: crate::netstack::Stack, resolver: crate::socks::Resolver) {
    let _ = MESH_DIALER.set((stack, resolver));
}

async fn connect_mesh(u: &Url, timeout: Duration) -> Option<Result<Box<dyn Io>, HttpError>> {
    let (stack, resolver) = MESH_DIALER.get()?;
    let ip: std::net::IpAddr = u.host.parse().ok()?;
    if !resolver.on_mesh(ip) {
        return None;
    }
    let port = u.host_port.rsplit_once(':').and_then(|(_, p)| p.parse().ok())?;
    let r = match tokio::time::timeout(timeout, stack.connect_tcp(std::net::SocketAddr::new(ip, port))).await {
        Err(_) => Err(HttpError::Connect(u.host_port.clone(), "timed out".into())),
        Ok(Err(e)) => Err(HttpError::Connect(u.host_port.clone(), e.to_string())),
        Ok(Ok(conn)) => {
            let (ours, theirs) = tokio::io::duplex(64 * 1024);
            tokio::spawn(crate::socks::bridge(theirs, conn));
            Ok(Box::new(ours) as Box<dyn Io>)
        }
    };
    Some(r)
}

/// Connect to `u`, with TLS for https/wss.
pub async fn connect_io(u: &Url, timeout: Duration) -> Result<Box<dyn Io>, HttpError> {
    if !u.tls {
        if let Some(r) = connect_mesh(u, timeout).await {
            return r;
        }
    }
    let err = |e: String| HttpError::Connect(u.host_port.clone(), e);
    let stream = tokio::time::timeout(timeout, TcpStream::connect(&u.host_port))
        .await
        .map_err(|_| err("timed out".into()))?
        .map_err(|e| err(e.to_string()))?;
    let _ = stream.set_nodelay(true);
    if !u.tls {
        return Ok(Box::new(stream));
    }
    let name = rustls::pki_types::ServerName::try_from(u.host.clone()).map_err(|e| err(e.to_string()))?;
    let tls = tokio::time::timeout(
        timeout,
        tokio_rustls::TlsConnector::from(tls_config()).connect(name, stream),
    )
    .await
    .map_err(|_| err("TLS handshake timed out".into()))?
    .map_err(|e| err(format!("TLS: {e}")))?;
    Ok(Box::new(tls))
}

async fn connect(
    u: &Url,
    timeout: Duration,
) -> Result<hyper::client::conn::http1::SendRequest<Full<Bytes>>, HttpError> {
    let io = connect_io(u, timeout).await?;
    let (send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|e| HttpError::Connect(u.host_port.clone(), e.to_string()))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok(send)
}

fn error_message(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(200).collect())
}

/// One request with a JSON body (or none); the answer's body on 2xx.
pub async fn request(
    method: &str,
    url: &str,
    token: Option<&str>,
    body: Option<&serde_json::Value>,
    timeout: Duration,
) -> Result<Vec<u8>, HttpError> {
    let u = parse_url(url)?;
    let fut = async {
        let mut send = connect(&u, timeout).await?;
        let mut b = Request::builder()
            .method(method)
            .uri(&u.path)
            .header("host", &u.host_port)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let data = body
            .map(|v| serde_json::to_vec(v).unwrap_or_default())
            .unwrap_or_default();
        let req = b
            .body(Full::new(Bytes::from(data)))
            .map_err(|e| HttpError::Io(e.to_string()))?;
        let resp = send.send_request(req).await.map_err(|e| HttpError::Io(e.to_string()))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| HttpError::Io(e.to_string()))?
            .to_bytes();
        if !(200..300).contains(&status) {
            return Err(HttpError::Status {
                status,
                message: error_message(&bytes),
            });
        }
        Ok(bytes.to_vec())
    };
    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| HttpError::Io(format!("{url}: timed out")))?
}

/// Follow a JSON-lines stream, calling `on_line` for each line, until it
/// ends, `idle` passes without a byte (the server pings), or on_line says stop.
pub async fn stream_lines(
    url: &str,
    token: &str,
    idle: Duration,
    mut on_line: impl FnMut(&str) -> bool,
) -> Result<(), HttpError> {
    let u = parse_url(url)?;
    let mut send = connect(&u, Duration::from_secs(10)).await?;
    let req = Request::builder()
        .method("GET")
        .uri(&u.path)
        .header("host", &u.host_port)
        .header("authorization", format!("Bearer {token}"))
        .body(Full::new(Bytes::new()))
        .map_err(|e| HttpError::Io(e.to_string()))?;
    let resp = tokio::time::timeout(Duration::from_secs(15), send.send_request(req))
        .await
        .map_err(|_| HttpError::Io(format!("{url}: no answer")))?
        .map_err(|e| HttpError::Io(e.to_string()))?;
    let status = resp.status().as_u16();
    let mut body = resp.into_body();
    if !(200..300).contains(&status) {
        let bytes = body.collect().await.map(|b| b.to_bytes()).unwrap_or_default();
        return Err(HttpError::Status {
            status,
            message: error_message(&bytes),
        });
    }
    let mut buf: Vec<u8> = vec![];
    loop {
        let frame = match tokio::time::timeout(idle, body.frame()).await {
            Err(_) => return Err(HttpError::Io("the stream went quiet".into())),
            Ok(None) => return Ok(()),
            Ok(Some(Err(e))) => return Err(HttpError::Io(e.to_string())),
            Ok(Some(Ok(f))) => f,
        };
        let Ok(data) = frame.into_data() else { continue };
        buf.extend_from_slice(&data);
        while let Some(i) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if !line.is_empty() && !on_line(line) {
                return Ok(());
            }
        }
        if buf.len() > 4 * 1024 * 1024 {
            return Err(HttpError::Io("a line longer than 4 MiB".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let u = parse_url("http://10.77.0.1:7420/v1/mesh/map").unwrap();
        assert_eq!(u.host_port, "10.77.0.1:7420");
        assert_eq!(u.path, "/v1/mesh/map");
        assert!(!u.tls);
        assert_eq!(parse_url("http://127.0.0.1:7443").unwrap().path, "/");
        let t = parse_url("https://mesh.example.com/v1/mesh/enroll").unwrap();
        assert!(t.tls);
        assert_eq!(
            (t.host.as_str(), t.host_port.as_str()),
            ("mesh.example.com", "mesh.example.com:443")
        );
        let w = parse_url("ws://[fd76::1]:7443/v1/mesh/relay").unwrap();
        assert_eq!((w.host.as_str(), w.host_port.as_str()), ("fd76::1", "[fd76::1]:7443"));
        assert_eq!(parse_url("wss://m.example.com").unwrap().host_port, "m.example.com:443");
        assert!(parse_url("ftp://x").is_err());
        assert!(parse_url("http:///x").is_err());
        assert_eq!(error_message(br#"{"error":"revoked"}"#), "revoked");
    }
}

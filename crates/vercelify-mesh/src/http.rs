//! The daemon's HTTP client: plain HTTP/1.1, because everything it talks to
//! is either sealed (enrollment) or inside the mesh already (the
//! coordinator's agent at its mesh address, or on 127.0.0.1).

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use std::time::Duration;
use tokio::net::TcpStream;

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
    pub host_port: String,
    pub path: String,
}

/// `http://host:port/path` → parts (only plain http).
pub fn parse_url(url: &str) -> Result<Url, HttpError> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| HttpError::Io(format!("{url}: only http:// URLs (the mesh encrypts)")))?;
    let (hp, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    Ok(Url {
        host_port: hp.to_string(),
        path: path.to_string(),
    })
}

async fn connect(
    host_port: &str,
    timeout: Duration,
) -> Result<hyper::client::conn::http1::SendRequest<Full<Bytes>>, HttpError> {
    let stream = tokio::time::timeout(timeout, TcpStream::connect(host_port))
        .await
        .map_err(|_| HttpError::Connect(host_port.into(), "timed out".into()))?
        .map_err(|e| HttpError::Connect(host_port.into(), e.to_string()))?;
    let (send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| HttpError::Connect(host_port.into(), e.to_string()))?;
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
        let mut send = connect(&u.host_port, timeout).await?;
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
    let mut send = connect(&u.host_port, Duration::from_secs(10)).await?;
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
        assert_eq!(parse_url("http://127.0.0.1:7443").unwrap().path, "/");
        assert!(parse_url("https://x").is_err());
        assert_eq!(error_message(br#"{"error":"revoked"}"#), "revoked");
    }
}

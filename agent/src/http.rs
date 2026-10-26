//! Minimal HTTP/1.1 client for services on this machine and the mesh (the
//! proxy's metrics, Ollama, other agents). Plain `http://` only; built on
//! the hyper that axum already links, so it adds almost nothing to the
//! binary.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::time::Duration;
use tokio::net::TcpStream;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("invalid URL {0} (only http://host:port/path is supported)")]
    Url(String),
    #[error("cannot connect to {0}: {1}")]
    Connect(String, std::io::Error),
    #[error("request to {0} timed out after {1:?}")]
    Timeout(String, Duration),
    #[error("HTTP error talking to {0}: {1}")]
    Protocol(String, hyper::Error),
}

/// `http://host:port/path?q` -> ("host:port", "/path?q")
pub fn split_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let authority = if authority
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
        authority.to_string()
    } else if authority.starts_with('[') && authority.ends_with(']') || !authority.contains(':') {
        format!("{authority}:80")
    } else {
        return None;
    };
    Some((authority, path.to_string()))
}

pub struct Req<'a> {
    pub method: Method,
    pub url: &'a str,
    pub bearer: Option<&'a str>,
    pub json: Option<Vec<u8>>,
    pub timeout: Duration,
}

impl<'a> Req<'a> {
    pub fn get(url: &'a str) -> Self {
        Self {
            method: Method::GET,
            url,
            bearer: None,
            json: None,
            timeout: Duration::from_secs(15),
        }
    }
    pub fn post(url: &'a str, body: &serde_json::Value) -> Self {
        Self {
            method: Method::POST,
            json: Some(serde_json::to_vec(body).expect("serializable")),
            ..Self::get(url)
        }
    }
    pub fn delete(url: &'a str, body: &serde_json::Value) -> Self {
        Self {
            method: Method::DELETE,
            ..Self::post(url, body)
        }
    }
    pub fn bearer(mut self, token: &'a str) -> Self {
        self.bearer = Some(token);
        self
    }
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
}

/// Send a request and return the response with its body still streaming.
/// The timeout covers connecting and receiving the response headers.
pub async fn stream(req: Req<'_>) -> Result<Response<Incoming>, HttpError> {
    let url = req.url.to_string();
    let timeout = req.timeout;
    tokio::time::timeout(timeout, send(req))
        .await
        .map_err(|_| HttpError::Timeout(url, timeout))?
}

async fn send(req: Req<'_>) -> Result<Response<Incoming>, HttpError> {
    let (authority, path) = split_url(req.url).ok_or_else(|| HttpError::Url(req.url.to_string()))?;
    let tcp = TcpStream::connect(&authority)
        .await
        .map_err(|e| HttpError::Connect(authority.clone(), e))?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
        .await
        .map_err(|e| HttpError::Protocol(authority.clone(), e))?;
    // Drives the connection; ends when the response body is consumed or dropped.
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let mut b = Request::builder()
        .method(req.method)
        .uri(path)
        .header(hyper::header::HOST, &authority)
        .header(hyper::header::ACCEPT, "application/json")
        .header(
            hyper::header::USER_AGENT,
            concat!("vercelify-agent/", env!("CARGO_PKG_VERSION")),
        );
    if let Some(t) = req.bearer {
        b = b.header(hyper::header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let body = match req.json {
        Some(j) => {
            b = b.header(hyper::header::CONTENT_TYPE, "application/json");
            Full::new(Bytes::from(j))
        }
        None => Full::new(Bytes::new()),
    };
    let request = b.body(body).map_err(|_| HttpError::Url(req.url.to_string()))?;
    sender
        .send_request(request)
        .await
        .map_err(|e| HttpError::Protocol(authority, e))
}

/// Send a request and read the whole body (within the same timeout).
pub async fn call(req: Req<'_>) -> Result<(StatusCode, Bytes), HttpError> {
    let url = req.url.to_string();
    let timeout = req.timeout;
    tokio::time::timeout(timeout, async {
        let resp = send(req).await?;
        let status = resp.status();
        let body = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| HttpError::Protocol(url.clone(), e))?
            .to_bytes();
        Ok((status, body))
    })
    .await
    .map_err(|_| HttpError::Timeout(url, timeout))?
}

/// Error text from a JSON body (`{"error": …}` or `{"message": …}`) or the raw body.
pub fn error_text(status: StatusCode, body: &[u8]) -> String {
    let v: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let msg = v
        .as_ref()
        .and_then(|v| v.get("error").or_else(|| v.get("message")))
        .and_then(|m| m.as_str())
        .map(String::from)
        .unwrap_or_else(|| String::from_utf8_lossy(body).trim().chars().take(300).collect());
    format!("HTTP {}: {msg}", status.as_u16())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            split_url("http://127.0.0.1:8000/api/v1/deploy?uuid=x"),
            Some(("127.0.0.1:8000".into(), "/api/v1/deploy?uuid=x".into()))
        );
        assert_eq!(split_url("http://localhost"), Some(("localhost:80".into(), "/".into())));
        assert_eq!(
            split_url("http://[::1]:11434/api"),
            Some(("[::1]:11434".into(), "/api".into()))
        );
        assert_eq!(split_url("https://x"), None);
        assert_eq!(split_url("http://user@x/"), None);
        assert_eq!(split_url("http:///x"), None);
    }

    #[test]
    fn errors() {
        assert_eq!(
            error_text(StatusCode::NOT_FOUND, br#"{"error":"model not found"}"#),
            "HTTP 404: model not found"
        );
        assert_eq!(error_text(StatusCode::BAD_GATEWAY, b"oops"), "HTTP 502: oops");
    }

    #[tokio::test]
    async fn round_trip() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(req.starts_with("POST /x HTTP/1.1"));
            assert!(req.contains("authorization: Bearer tok"));
            s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 11\r\n\r\n{\"ok\":true}")
                .await
                .unwrap();
        });
        let url = format!("http://127.0.0.1:{port}/x");
        let (status, body) = call(Req::post(&url, &serde_json::json!({"a": 1})).bearer("tok"))
            .await
            .unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"{\"ok\":true}");
    }

    #[tokio::test]
    async fn connect_refused() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let url = format!("http://127.0.0.1:{port}/");
        assert!(matches!(call(Req::get(&url)).await, Err(HttpError::Connect(..))));
    }
}

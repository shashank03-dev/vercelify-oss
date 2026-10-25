//! HTTP from Rust to the host agent and the Coolify API. Doing this in
//! Rust (not the webview) avoids CORS and keeps tokens out of page JS fetches.

use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct Request {
    pub method: String,
    /// Path including query, starting with `/`.
    pub path: String,
    #[serde(default)]
    pub body: Option<Value>,
    /// Seconds; defaults to 30.
    #[serde(default)]
    pub timeout: Option<u64>,
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .user_agent(concat!("vercelify-cockpit/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

/// The same, through `vercelify-mesh`'s SOCKS proxy (app mode on the Mac).
pub fn mesh_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .user_agent(concat!("vercelify-cockpit/", env!("CARGO_PKG_VERSION")))
        .proxy(reqwest::Proxy::all(crate::mesh::SOCKS).expect("a valid proxy URL"))
        .build()
        .expect("http client")
}

fn build(
    client: &reqwest::Client,
    base: &str,
    token: Option<&str>,
    req: Request,
) -> Result<reqwest::RequestBuilder, String> {
    if !req.path.starts_with('/') || req.path.contains("://") {
        return Err("invalid request path".into());
    }
    let method: reqwest::Method = req
        .method
        .to_uppercase()
        .parse()
        .map_err(|_| "invalid method".to_string())?;
    let url = format!("{}{}", base.trim_end_matches('/'), req.path);
    let mut rb = client
        .request(method, &url)
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(req.timeout.unwrap_or(30).clamp(1, 900)));
    if let Some(t) = token {
        rb = rb.bearer_auth(t);
    }
    if let Some(body) = req.body {
        rb = rb.json(&body);
    }
    Ok(rb)
}

fn unreachable(base: &str, e: reqwest::Error) -> String {
    if (e.is_connect() || e.is_timeout()) && crate::mesh::on_mesh(base) {
        format!("HTTP 0: cannot reach {base} over Vercelify's network — is vercelify-mesh running on this Mac?")
    } else if e.is_connect() || e.is_timeout() {
        format!("HTTP 0: cannot reach {base} — is Tailscale connected on both machines?")
    } else {
        format!("HTTP 0: {e}")
    }
}

fn parse_body(text: String) -> Value {
    if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }
}

fn error_message(status: reqwest::StatusCode, value: &Value) -> String {
    let msg = value
        .get("error")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| value.to_string());
    format!("HTTP {}: {msg}", status.as_u16())
}

/// Error string format understood by the frontend: `HTTP <status>: <message>`.
pub async fn send(client: &reqwest::Client, base: &str, token: Option<&str>, req: Request) -> Result<Value, String> {
    let resp = build(client, base, token, req)?
        .send()
        .await
        .map_err(|e| unreachable(base, e))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("HTTP {}: {e}", status.as_u16()))?;
    let value = parse_body(text);
    if status.is_success() {
        Ok(value)
    } else {
        Err(error_message(status, &value))
    }
}

/// Split a byte stream into lines; returns complete lines and keeps the rest.
pub fn take_lines(buf: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(i) = buf.iter().position(|b| *b == b'\n') {
        let line: Vec<u8> = buf.drain(..=i).collect();
        let line = String::from_utf8_lossy(&line).trim().to_string();
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

/// Send a request whose response is NDJSON and hand each line to `on_line`
/// as it arrives. Stops early when `cancel` fires or `on_line` returns false.
pub async fn send_lines(
    client: &reqwest::Client,
    base: &str,
    token: Option<&str>,
    req: Request,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
    mut on_line: impl FnMut(String) -> bool,
) -> Result<(), String> {
    let request = build(client, base, token, req)?;
    // A model may take minutes to load before it sends response headers.
    let mut resp = tokio::select! {
        biased;
        _ = &mut cancel => return Ok(()),
        response = request.send() => response.map_err(|e| unreachable(base, e))?,
    };
    let status = resp.status();
    if !status.is_success() {
        let text = tokio::select! {
            biased;
            _ = &mut cancel => return Ok(()),
            text = resp.text() => text.unwrap_or_default(),
        };
        return Err(error_message(status, &parse_body(text)));
    }
    let mut buf = Vec::new();
    loop {
        tokio::select! {
            _ = &mut cancel => return Ok(()),
            chunk = resp.chunk() => match chunk.map_err(|e| format!("HTTP 0: stream interrupted: {e}"))? {
                None => break,
                Some(bytes) => {
                    buf.extend_from_slice(&bytes);
                    for line in take_lines(&mut buf) {
                        if !on_line(line) {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    let rest = String::from_utf8_lossy(&buf).trim().to_string();
    if !rest.is_empty() {
        on_line(rest);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_interrupts_a_server_that_never_sends_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            let _ = accepted_tx.send(());
            std::future::pending::<()>().await;
        });
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let request = tokio::spawn(async move {
            send_lines(
                &client(),
                &base,
                None,
                Request {
                    method: "GET".into(),
                    path: "/stream".into(),
                    body: None,
                    timeout: Some(30),
                },
                cancel_rx,
                |_| true,
            )
            .await
        });
        accepted_rx.await.unwrap();
        cancel_tx.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_millis(300), request).await;
        server.abort();
        assert!(result.is_ok(), "Stop must cancel before response headers arrive");
        assert!(result.unwrap().unwrap().is_ok());
    }

    #[test]
    fn lines() {
        let mut buf = b"{\"a\":1}\n\n{\"b\"".to_vec();
        assert_eq!(take_lines(&mut buf), vec!["{\"a\":1}"]);
        assert_eq!(buf, b"{\"b\"");
        buf.extend_from_slice(b":2}\n");
        assert_eq!(take_lines(&mut buf), vec!["{\"b\":2}"]);
        assert!(buf.is_empty());
    }
}

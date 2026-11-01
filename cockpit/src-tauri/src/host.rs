//! The host manager (`vercelify-host`) on this computer: the Vercelify
//! environment that lets a Mac or Windows PC host apps. Desktop reads the
//! owner token the host manager left for this account and talks to it on
//! the loopback; only `/v1/host/…` is reachable through here.

use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use vercelify_core::host::{HostLocal, ManagedEnvironment};

const API: &str = "http://127.0.0.1:7425";

pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("VERCELIFY_HOST_HOME") {
        return h.into();
    }
    if cfg!(windows) {
        let p = std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into());
        return PathBuf::from(p).join("Vercelify").join("host");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Application Support/Vercelify/host")
}

fn token() -> Option<String> {
    let t = std::fs::read_to_string(home().join("owner.token")).ok()?;
    let t = t.trim().to_string();
    (t.len() >= 32).then_some(t)
}

/// Paths Desktop may call (pure, tested).
pub fn allowed(method: &str, path: &str) -> bool {
    let ok_path = path.starts_with("/v1/host/")
        && !path.contains("..")
        && path.chars().all(|c| c.is_ascii_alphanumeric() || "/_-".contains(c));
    ok_path && matches!(method, "GET" | "POST" | "PATCH" | "DELETE")
}

pub async fn request(method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
    if !allowed(method, path) {
        return Err(format!("{method} {path} isn't a host manager request"));
    }
    let token = token().ok_or("Vercelify isn't set up to host on this computer")?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let mut req = client.request(m, format!("{API}{path}")).bearer_auth(token);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let res = req
        .send()
        .await
        .map_err(|_| "the Vercelify host manager isn't running on this computer".to_string())?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        Ok(v)
    } else {
        Err(format!(
            "HTTP {}: {}",
            status.as_u16(),
            v["error"].as_str().unwrap_or("the host manager refused this")
        ))
    }
}

pub async fn local() -> HostLocal {
    if token().is_none() {
        return HostLocal::default();
    }
    match request("GET", "/v1/host/environment", None).await {
        Ok(v) => HostLocal {
            installed: true,
            running: true,
            environment: serde_json::from_value::<ManagedEnvironment>(v).ok(),
            error: None,
        },
        Err(e) => HostLocal {
            installed: true,
            running: false,
            environment: None,
            error: Some(e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_host_paths() {
        assert!(allowed("GET", "/v1/host/environment"));
        assert!(allowed("POST", "/v1/host/environment/start"));
        assert!(allowed("GET", "/v1/host/operations/op_ab12"));
        assert!(!allowed("GET", "/v1/tokens"));
        assert!(!allowed("GET", "/v1/host/../tokens"));
        assert!(!allowed("GET", "/v1/host/environment?x=1"));
        assert!(!allowed("PUT", "/v1/host/environment"));
    }
}

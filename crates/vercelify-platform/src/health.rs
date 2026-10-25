//! Is a new container ready for traffic? (design §7.2 step 3)
//!
//! HTTP: `GET <path>` must answer below 500 (redirects and 404s still mean
//! the server is up; a health path should answer 2xx). TCP: the port accepts
//! a connection. Command: exits 0 inside the container. Two passes in a row
//! are needed, so an app that answers once and then crashes isn't promoted.

use crate::docker::Docker;
use crate::{Error, Result};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Debug, Clone, PartialEq)]
pub enum Probe {
    Http { path: String },
    Tcp,
    Command(Vec<String>),
}

/// One HTTP probe: the status code, or why there was none.
pub async fn http_status(ip: &str, port: u16, path: &str, timeout: Duration) -> std::result::Result<u16, String> {
    let fut = async {
        let mut s = TcpStream::connect((ip, port))
            .await
            .map_err(|e| format!("connect: {e}"))?;
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nUser-Agent: vercelify-health/1\r\nConnection: close\r\n\r\n"
        );
        s.write_all(req.as_bytes()).await.map_err(|e| format!("write: {e}"))?;
        let mut buf = [0u8; 64];
        let mut n = 0;
        while n < 12 {
            let r = s.read(&mut buf[n..]).await.map_err(|e| format!("read: {e}"))?;
            if r == 0 {
                break;
            }
            n += r;
        }
        let head = String::from_utf8_lossy(&buf[..n]);
        head.split_whitespace()
            .nth(1)
            .and_then(|c| c.parse::<u16>().ok())
            .filter(|_| head.starts_with("HTTP/"))
            .ok_or_else(|| "not an HTTP response".to_string())
    };
    tokio::time::timeout(timeout, fut)
        .await
        .unwrap_or_else(|_| Err("timed out".into()))
}

pub async fn tcp_open(ip: &str, port: u16, timeout: Duration) -> bool {
    tokio::time::timeout(timeout, TcpStream::connect((ip, port)))
        .await
        .is_ok_and(|r| r.is_ok())
}

pub struct Wait<'a> {
    pub docker: &'a Docker,
    pub container: &'a str,
    /// Network whose address to probe (the proxy network).
    pub network: &'a str,
    pub port: u16,
    pub probe: Probe,
    pub timeout: Duration,
    pub interval: Duration,
}

/// Wait until healthy twice in a row. Fails early when the container exits.
pub async fn wait_healthy(w: Wait<'_>, log: &mut (dyn FnMut(&str) + Send)) -> Result<()> {
    let deadline = Instant::now() + w.timeout;
    let mut passes = 0;
    let mut last = String::from("not checked yet");
    let mut told = false;
    loop {
        let Some(i) = w.docker.inspect(w.container).await? else {
            return Err(Error::Invalid("the new container disappeared".into()));
        };
        if !i.running {
            let why = if i.oom_killed {
                " (out of memory: raise the app's memory limit)".to_string()
            } else {
                String::new()
            };
            return Err(Error::Invalid(format!("the app exited with code {}{why}", i.exit_code)));
        }
        let ip = i.ips.get(w.network).cloned().unwrap_or_default();
        let ok = if ip.is_empty() {
            last = format!("no address on {}", w.network);
            false
        } else {
            match &w.probe {
                Probe::Http { path } => match http_status(&ip, w.port, path, Duration::from_secs(5)).await {
                    Ok(code) if code < 500 => true,
                    Ok(code) => {
                        last = format!("GET {path} answered {code}");
                        false
                    }
                    Err(e) => {
                        last = format!("GET {path} on port {}: {e}", w.port);
                        false
                    }
                },
                Probe::Tcp => {
                    let open = tcp_open(&ip, w.port, Duration::from_secs(3)).await;
                    if !open {
                        last = format!("nothing listens on port {}", w.port);
                    }
                    open
                }
                Probe::Command(cmd) => {
                    let o = w.docker.exec(w.container, cmd, Duration::from_secs(10)).await?;
                    if !o.success() {
                        last = format!("health command exited {}: {}", o.code.unwrap_or(-1), o.tail(1));
                    }
                    o.success()
                }
            }
        };
        if ok {
            passes += 1;
            if passes >= 2 {
                return Ok(());
            }
        } else {
            passes = 0;
            if !told && Instant::now() + w.timeout / 2 > deadline {
                log(&format!("still waiting: {last}"));
                told = true;
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::Invalid(format!(
                "not healthy after {}s: {last}",
                w.timeout.as_secs()
            )));
        }
        tokio::time::sleep(w.interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    async fn server(status: &'static str) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                let mut b = [0u8; 512];
                let _ = s.read(&mut b).await;
                let _ = s
                    .write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes())
                    .await;
            }
        });
        port
    }

    #[tokio::test]
    async fn probes() {
        let ok = server("200 OK").await;
        assert_eq!(http_status("127.0.0.1", ok, "/", Duration::from_secs(2)).await, Ok(200));
        let bad = server("503 Service Unavailable").await;
        assert_eq!(
            http_status("127.0.0.1", bad, "/healthz", Duration::from_secs(2)).await,
            Ok(503)
        );
        assert!(tcp_open("127.0.0.1", ok, Duration::from_secs(1)).await);
        let closed = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(http_status("127.0.0.1", closed, "/", Duration::from_secs(1))
            .await
            .is_err());
    }
}

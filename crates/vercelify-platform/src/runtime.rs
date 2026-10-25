//! Running containers on a server (design §8.2): this one, through Docker
//! directly, or a worker, through its agent's runtime API over the mesh.
//! The engine and the reconciler use [`NodeRuntime`] and don't care which.
//!
//! The worker side is the same code: its agent answers `/v1/runtime/*` with
//! [`Deployer::start_instance`] and [`apply_local`], so a deployment does on
//! a worker exactly what it does on the main server.

use crate::deploy::{DeployInput, Deployer};
use crate::docker::{ContainerState, Inspect, PROXY_NETWORK};
use crate::health::{self, Probe};
use crate::proxy::{self, Route};
use crate::{Error, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use vercelify_core::platform::{App, AppVolume, Deployment};
use zeroize::Zeroizing;

/// Start one instance of a deployment (`POST /v1/runtime/instances`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartSpec {
    pub app: App,
    pub deployment: Deployment,
    pub image: String,
    /// Runtime variables, system ones included. They cross the mesh
    /// (WireGuard) and reach the container through Docker's environment.
    pub env: Vec<(String, String)>,
    pub volumes: Vec<AppVolume>,
    pub port: u16,
    pub replica: u8,
}

/// An idempotent batch for one server (`POST /v1/runtime/apply`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Apply {
    pub start: Vec<String>,
    pub stop: Vec<String>,
    pub restart: Vec<String>,
    pub remove: Vec<String>,
    /// Grace before a stop kills (default 20 s).
    pub grace_secs: Option<u32>,
    pub write_routes: Vec<Route>,
    pub remove_routes: Vec<String>,
    pub remove_volumes: Vec<String>,
    /// `vfy/<app>` images to prune: keep the 5 newest and these.
    pub prune: Option<(String, Vec<String>)>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Applied {
    pub routes_written: usize,
    pub errors: Vec<String>,
}

/// `POST /v1/runtime/health`: wait until a container answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthWait {
    pub container: String,
    pub port: u16,
    pub probe: Probe,
    pub timeout_secs: u64,
}

/// Do a batch on this server's Docker and proxy. Every item is tried; the
/// failures come back as messages.
pub async fn apply_local(d: &Deployer, a: &Apply) -> Applied {
    let mut out = Applied::default();
    let grace = a.grace_secs.unwrap_or(20);
    let docker = &d.docker;
    for n in &a.start {
        if let Err(e) = docker.start(n).await {
            out.errors.push(format!("start {n}: {e}"));
        }
    }
    for n in &a.restart {
        if let Err(e) = docker.restart(n, grace).await {
            out.errors.push(format!("restart {n}: {e}"));
        }
    }
    for n in &a.stop {
        if let Err(e) = docker.stop(n, grace).await {
            out.errors.push(format!("stop {n}: {e}"));
        }
    }
    for n in &a.remove {
        if let Err(e) = docker.remove(n).await {
            out.errors.push(format!("remove {n}: {e}"));
        }
    }
    for r in &a.write_routes {
        match proxy::write_route(&d.proxy, r) {
            Ok(true) => out.routes_written += 1,
            Ok(false) => {}
            Err(e) => out.errors.push(format!("route {}: {e}", r.key)),
        }
    }
    for k in &a.remove_routes {
        if let Err(e) = proxy::remove_route(&d.proxy, k) {
            out.errors.push(format!("route {k}: {e}"));
        }
    }
    for v in &a.remove_volumes {
        let _ = docker.volume_remove(v).await;
        if !v.contains('/') && !v.is_empty() {
            let _ = std::fs::remove_dir_all(d.hdd_root.join(v));
        }
    }
    if let Some((app, keep)) = &a.prune {
        if let Err(e) = d.prune_images(app, keep).await {
            out.errors.push(format!("prune {app}: {e}"));
        }
    }
    out
}

impl Deployer {
    /// One instance of a deployment, from a spec (the worker's side of
    /// `POST /v1/runtime/instances`, and the main server's own).
    pub async fn start_instance(&self, s: &StartSpec, log: &mut (dyn FnMut(&str) + Send)) -> Result<String> {
        self.start(
            &DeployInput {
                app: &s.app,
                deployment: &s.deployment,
                image: &s.image,
                env: s.env.clone(),
                volumes: &s.volumes,
                port: s.port,
                hosts: vec![],
                basic_auth: vec![],
                dns_challenge: false,
                replica: s.replica,
            },
            log,
        )
        .await
    }

    pub async fn wait(&self, w: &HealthWait) -> Result<()> {
        health::wait_healthy(
            health::Wait {
                docker: &self.docker,
                container: &w.container,
                network: PROXY_NETWORK,
                port: w.port,
                probe: w.probe.clone(),
                timeout: Duration::from_secs(w.timeout_secs),
                interval: Duration::from_millis(250),
            },
            &mut |_| {},
        )
        .await
    }
}

/// A worker's agent, reached over the mesh.
#[derive(Clone)]
pub struct Remote {
    pub node: String,
    /// `http://10.77.0.5:7420`
    pub base: String,
    pub token: Zeroizing<String>,
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote")
            .field("node", &self.node)
            .field("base", &self.base)
            .finish()
    }
}

fn map_status(node: &str, status: StatusCode, body: &[u8]) -> Error {
    let msg = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(String::from))
        .unwrap_or_else(|| String::from_utf8_lossy(body).trim().chars().take(300).collect());
    let msg = format!("{node}: {msg}");
    match status.as_u16() {
        400 => Error::Invalid(msg),
        404 => Error::NotFound(msg),
        409 => Error::Conflict(msg),
        401 | 403 | 502..=504 => Error::Unavailable(msg),
        _ => Error::Internal(msg),
    }
}

impl Remote {
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<hyper::Response<hyper::body::Incoming>> {
        let unreachable = |e: &dyn std::fmt::Display| {
            Error::Unavailable(format!("{} can't be reached over the mesh: {e}", self.node))
        };
        let authority = self
            .base
            .strip_prefix("http://")
            .ok_or_else(|| Error::Internal(format!("{}: not an http:// address", self.base)))?
            .trim_end_matches('/')
            .to_string();
        let tcp = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(&authority))
            .await
            .map_err(|_| unreachable(&"timed out"))?
            .map_err(|e| unreachable(&e))?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
            .await
            .map_err(|e| unreachable(&e))?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, &authority)
            .header(hyper::header::AUTHORIZATION, format!("Bearer {}", self.token.as_str()))
            .header(
                hyper::header::USER_AGENT,
                concat!("vercelify-main/", env!("CARGO_PKG_VERSION")),
            );
        if body.is_some() {
            b = b.header(hyper::header::CONTENT_TYPE, "application/json");
        }
        let req = b
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .map_err(|e| Error::Internal(e.to_string()))?;
        sender.send_request(req).await.map_err(|e| unreachable(&e))
    }

    /// A request and its whole answer (not generic: compiled once).
    async fn request(&self, method: Method, path: &str, body: Option<Vec<u8>>, timeout: Duration) -> Result<Bytes> {
        let fut = async {
            let resp = self.send(method, path, body).await?;
            let status = resp.status();
            let bytes = resp
                .into_body()
                .collect()
                .await
                .map_err(|e| Error::Unavailable(format!("{}: {e}", self.node)))?
                .to_bytes();
            if !status.is_success() {
                return Err(map_status(&self.node, status, &bytes));
            }
            Ok(bytes)
        };
        tokio::time::timeout(timeout, Box::pin(fut))
            .await
            .map_err(|_| Error::Unavailable(format!("{} didn't answer within {}s", self.node, timeout.as_secs())))?
    }

    /// A request and its whole JSON answer.
    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
        timeout: Duration,
    ) -> Result<T> {
        let body = body.map(serde_json::to_vec).transpose()?;
        let bytes = self.request(method, path, body, timeout).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// A request whose answer is JSON lines; each goes to `line`.
    pub async fn lines(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Duration,
        line: &mut (dyn FnMut(serde_json::Value) + Send),
    ) -> Result<()> {
        let fut = async {
            let resp = self.send(method, path, body).await?;
            let status = resp.status();
            let mut body = resp.into_body();
            if !status.is_success() {
                let bytes = body.collect().await.map(|b| b.to_bytes()).unwrap_or_default();
                return Err(map_status(&self.node, status, &bytes));
            }
            let mut buf: Vec<u8> = vec![];
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|e| Error::Unavailable(format!("{}: {e}", self.node)))?;
                let Some(data) = frame.data_ref() else { continue };
                buf.extend_from_slice(data);
                while let Some(i) = buf.iter().position(|b| *b == b'\n') {
                    let l: Vec<u8> = buf.drain(..=i).collect();
                    if let Ok(v) = serde_json::from_slice(&l[..l.len() - 1]) {
                        line(v);
                    }
                }
            }
            Ok(())
        };
        tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| Error::Unavailable(format!("{} didn't finish within {}s", self.node, timeout.as_secs())))?
    }

    /// The raw streaming response (logs, terminals), for passing through.
    pub async fn stream(&self, path: &str) -> Result<hyper::Response<hyper::body::Incoming>> {
        let resp = self.send(Method::GET, path, None).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let bytes = resp
                .into_body()
                .collect()
                .await
                .map(|b| b.to_bytes())
                .unwrap_or_default();
            return Err(map_status(&self.node, status, &bytes));
        }
        Ok(resp)
    }

    pub async fn post<T: serde::de::DeserializeOwned>(&self, path: &str, body: &impl Serialize) -> Result<T> {
        self.json(Method::POST, path, Some(body), Duration::from_secs(60)).await
    }

    pub async fn delete(&self, path: &str) -> Result<serde_json::Value> {
        self.json(Method::DELETE, path, None::<&()>, Duration::from_secs(30))
            .await
    }
}

/// Container names are DNS labels with dashes; nothing else reaches a URL.
fn segment(name: &str) -> Result<&str> {
    if name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        Ok(name)
    } else {
        Err(Error::Invalid(format!("not a container name: {name}")))
    }
}

/// One server's containers and proxy.
#[derive(Debug, Clone)]
pub enum NodeRuntime {
    Local(Deployer),
    Remote(Remote),
}

impl NodeRuntime {
    pub fn is_local(&self) -> bool {
        matches!(self, NodeRuntime::Local(_))
    }

    pub fn name(&self) -> &str {
        match self {
            NodeRuntime::Local(_) => "this server",
            NodeRuntime::Remote(r) => &r.node,
        }
    }

    pub async fn list(&self) -> Result<Vec<ContainerState>> {
        match self {
            NodeRuntime::Local(d) => d.docker.list_managed().await,
            NodeRuntime::Remote(r) => {
                r.json(
                    Method::GET,
                    "/v1/runtime/containers",
                    None::<&()>,
                    Duration::from_secs(30),
                )
                .await
            }
        }
    }

    pub async fn inspect(&self, name: &str) -> Result<Option<Inspect>> {
        match self {
            NodeRuntime::Local(d) => d.docker.inspect(name).await,
            NodeRuntime::Remote(r) => {
                let path = format!("/v1/runtime/containers/{}", segment(name)?);
                match r.json(Method::GET, &path, None::<&()>, Duration::from_secs(30)).await {
                    Ok(i) => Ok(Some(i)),
                    Err(Error::NotFound(_)) => Ok(None),
                    Err(e) => Err(e),
                }
            }
        }
    }

    pub async fn apply(&self, a: &Apply) -> Result<Applied> {
        match self {
            NodeRuntime::Local(d) => Ok(apply_local(d, a).await),
            NodeRuntime::Remote(r) => {
                r.json(Method::POST, "/v1/runtime/apply", Some(a), Duration::from_secs(300))
                    .await
            }
        }
    }

    /// Like [`apply`](Self::apply), failing on the first error.
    pub async fn apply_ok(&self, a: &Apply) -> Result<Applied> {
        let out = self.apply(a).await?;
        match out.errors.first() {
            Some(e) => Err(Error::Internal(format!("{}: {e}", self.name()))),
            None => Ok(out),
        }
    }

    pub async fn route_keys(&self) -> Result<Vec<String>> {
        match self {
            NodeRuntime::Local(d) => Ok(proxy::route_keys(&d.proxy)),
            NodeRuntime::Remote(r) => {
                r.json(Method::GET, "/v1/runtime/routes", None::<&()>, Duration::from_secs(30))
                    .await
            }
        }
    }

    /// Start an instance and wait until it is healthy; its progress goes to `log`.
    pub async fn start_instance(&self, s: &StartSpec, log: &mut (dyn FnMut(&str) + Send)) -> Result<String> {
        match self {
            NodeRuntime::Local(d) => d.start_instance(s, log).await,
            NodeRuntime::Remote(r) => {
                let timeout = Duration::from_secs(s.app.runtime.health_timeout_secs as u64 + 1200);
                let mut container: Option<String> = None;
                let mut failure: Option<Error> = None;
                let node = r.node.clone();
                r.lines(
                    Method::POST,
                    "/v1/runtime/instances",
                    Some(serde_json::to_vec(s)?),
                    timeout,
                    &mut |v| {
                        if let Some(l) = v["log"].as_str() {
                            log(&format!("[{node}] {l}"));
                        } else if let Some(c) = v["container"].as_str() {
                            container = Some(c.to_string());
                        } else if let Some(e) = v["error"].as_str() {
                            let msg = format!("on {node}: {e}");
                            failure = Some(match v["status"].as_u64() {
                                Some(400) => Error::Invalid(msg),
                                Some(409) => Error::Conflict(msg),
                                Some(503) => Error::Unavailable(msg),
                                _ => Error::Internal(msg),
                            });
                        }
                    },
                )
                .await?;
                if let Some(e) = failure {
                    return Err(e);
                }
                container.ok_or_else(|| Error::Unavailable(format!("{}: the start ended without an answer", r.node)))
            }
        }
    }

    pub async fn wait_healthy(&self, w: &HealthWait) -> Result<()> {
        match self {
            NodeRuntime::Local(d) => d.wait(w).await,
            NodeRuntime::Remote(r) => r
                .json::<serde_json::Value>(
                    Method::POST,
                    "/v1/runtime/health",
                    Some(w),
                    Duration::from_secs(w.timeout_secs + 30),
                )
                .await
                .map(|_| ()),
        }
    }

    /// The last `tail` lines a container wrote (for failure messages).
    pub async fn logs(&self, name: &str, tail: u32) -> Result<String> {
        match self {
            NodeRuntime::Local(d) => d.docker.logs(name, tail).await,
            NodeRuntime::Remote(r) => {
                let mut out = String::new();
                let path = format!("/v1/runtime/containers/{}/logs?tail={tail}", segment(name)?);
                r.lines(Method::GET, &path, None, Duration::from_secs(30), &mut |v| {
                    if let Some(t) = v["text"].as_str() {
                        out.push_str(t);
                        out.push('\n');
                    }
                })
                .await?;
                Ok(out)
            }
        }
    }

    /// Stop and remove an old container after it drained.
    pub async fn retire(&self, container: &str, drain: Duration) {
        tokio::time::sleep(drain).await;
        let a = Apply {
            stop: vec![container.to_string()],
            remove: vec![container.to_string()],
            ..Apply::default()
        };
        match self.apply(&a).await {
            Ok(out) => {
                for e in out.errors {
                    tracing::warn!("retiring {container} on {}: {e}", self.name());
                }
            }
            Err(e) => tracing::warn!("retiring {container}: {e}"),
        }
    }

    pub async fn remove(&self, container: &str) -> Result<()> {
        self.apply_ok(&Apply {
            remove: vec![container.to_string()],
            ..Apply::default()
        })
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_map_to_errors() {
        let e = map_status("pi", StatusCode::CONFLICT, br#"{"error":"busy"}"#);
        assert_eq!(e, Error::Conflict("pi: busy".into()));
        assert!(matches!(
            map_status("pi", StatusCode::UNAUTHORIZED, b"no"),
            Error::Unavailable(_)
        ));
        assert!(matches!(
            map_status("pi", StatusCode::INTERNAL_SERVER_ERROR, b"x"),
            Error::Internal(_)
        ));
        assert!(segment("vfy-blog-1x8z3c5v").is_ok());
        assert!(
            segment("../etc").is_err() && segment("..").is_err() && segment("a b").is_err() && segment("").is_err()
        );
    }

    #[test]
    fn apply_round_trips() {
        let a = Apply {
            stop: vec!["vfy-a-1".into()],
            prune: Some(("blog".into(), vec!["vfy/blog:1".into()])),
            ..Apply::default()
        };
        let back: Apply = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(back, a);
        let empty: Apply = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Apply::default());
    }
}

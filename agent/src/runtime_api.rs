//! A worker's runtime API (design §8.2), called by the main server over
//! the mesh with the worker's runtime token (auth.rs accepts it only from
//! the coordinator's mesh address, and only here). Everything is done by
//! the same platform code the main server runs for itself.
//!
//! One axum handler dispatches on the path: each handler axum generates
//! costs several KiB of the agent's size budget (design §16), and these
//! routes only ever have one caller.

use crate::error::AppError;
use crate::json::Json;
use crate::routes::AppState;
use crate::tokens::Actor;
use crate::worker::Worker;
use axum::extract::{Path, RawQuery, State};
use axum::http::Method;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{Extension, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use vercelify_core::platform::{TerminalRequest, TerminalSession};
use vercelify_platform::runtime::{apply_local, Apply, HealthWait, StartSpec};

fn worker(s: &AppState) -> Result<Arc<Worker>, AppError> {
    s.worker
        .clone()
        .ok_or_else(|| AppError::unavailable("this server isn't a worker of the platform engine"))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/runtime/{*rest}", any(dispatch))
}

/// Bodies are parsed with serde_json directly: the main server sends them.
fn body<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T, AppError> {
    serde_json::from_slice(b).map_err(|e| AppError::bad_request(e.to_string()))
}

fn json<T: serde::Serialize>(v: T) -> Response {
    Json(v).into_response()
}

fn query_value<'a>(q: &'a str, key: &str) -> Option<&'a str> {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

async fn dispatch(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    method: Method,
    Path(rest): Path<String>,
    RawQuery(query): RawQuery,
    b: bytes::Bytes,
) -> Result<Response, AppError> {
    let seg: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    let q = query.unwrap_or_default();
    let w = worker(&s)?;
    let d = &w.deployer;
    let get = method == Method::GET;
    let post = method == Method::POST;
    let missing = || AppError::not_found(format!("no runtime route {method} /{rest}"));
    Ok(match seg.as_slice() {
        ["containers"] if get => json(d.docker.list_managed().await?),
        ["containers", name] if get => match d.docker.inspect(name).await? {
            Some(i) => json(i),
            None => return Err(AppError::not_found(format!("no container {name}"))),
        },
        ["containers", name, "logs"] if get => {
            let tail = query_value(&q, "tail").and_then(|t| t.parse().ok()).unwrap_or(200);
            crate::platform_api::docker_logs(&d.docker.bin, name, tail, query_value(&q, "follow") == Some("true"))?
        }
        ["containers", name, "exec"] if post => {
            json(vercelify_platform::runtime::exec_local(d, name, &body(&b)?).await?)
        }
        ["apply"] if post => json(apply_local(d, &body::<Apply>(&b)?).await),
        ["routes"] if get => json(vercelify_platform::proxy::route_keys(&d.proxy)),
        ["instances" | "instances-v2"] if post => start_instance(w.clone(), body(&b)?),
        ["health"] if post => {
            d.wait(&body::<HealthWait>(&b)?).await?;
            json(json!({ "healthy": true }))
        }
        ["standby"] if get => json(crate::standby::held(&s.config)),
        ["standby"] if post => {
            let sealed: vercelify_mesh::seal::Sealed = body(&b)?;
            let cfg = s.config.clone();
            let held = tokio::task::spawn_blocking(move || crate::standby::receive(&cfg, &sealed))
                .await
                .map_err(|e| AppError::internal(e.to_string()))?
                .map_err(AppError::bad_request)?;
            json(held)
        }
        ["terminals"] if post => json(terminal_open(&s, &w, &actor, body(&b)?)?),
        ["terminals", id, rest @ ..] if rest.len() <= 1 => {
            let (st, ex, id) = (State(s.clone()), Extension(actor), Path(id.to_string()));
            match (rest.first().copied(), &method) {
                (None, &Method::DELETE) => crate::platform_api::terminal_close(st, ex, id).await?.into_response(),
                (Some("output"), &Method::GET) => crate::platform_api::terminal_output(st, ex, id).await?,
                (Some("input"), &Method::POST) => crate::platform_api::terminal_input(st, ex, id, Json(body(&b)?))
                    .await?
                    .into_response(),
                (Some("resize"), &Method::POST) => crate::platform_api::terminal_resize(st, ex, id, Json(body(&b)?))
                    .await?
                    .into_response(),
                _ => return Err(missing()),
            }
        }
        _ => return Err(missing()),
    })
}

/// Start an instance; its progress as `{"log": …}` lines, then
/// `{"container": name}` or `{"error": …, "status": code}`.
fn start_instance(w: Arc<Worker>, spec: StartSpec) -> Response {
    let (tx_, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(256);
    let line = |v: Value| {
        let mut b = serde_json::to_vec(&v).unwrap_or_default();
        b.push(b'\n');
        bytes::Bytes::from(b)
    };
    tokio::spawn(async move {
        let logs = tx_.clone();
        let mut log = move |m: &str| {
            let _ = logs.try_send(line(json!({ "log": m })));
        };
        let end = match w.deployer.start_instance(&spec, &mut log).await {
            Ok(c) => json!({ "container": c }),
            Err(e) => json!({ "error": e.message(), "status": e.status() }),
        };
        let _ = tx_.send(line(end)).await;
    });
    let headers = [
        (axum::http::header::CONTENT_TYPE, "application/x-ndjson"),
        (axum::http::header::CACHE_CONTROL, "no-store"),
    ];
    (headers, axum::body::Body::new(crate::routes::LineBody(rx))).into_response()
}

#[derive(Deserialize)]
struct OpenTerminal {
    container: String,
    #[serde(default)]
    command: Option<Vec<String>>,
    #[serde(flatten)]
    size: TerminalRequest,
}

fn terminal_open(s: &AppState, w: &Worker, actor: &Actor, req: OpenTerminal) -> Result<TerminalSession, AppError> {
    let argv = crate::terminal::exec_argv(&w.deployer.docker.bin, &req.container, req.command.as_deref());
    let t = s
        .terminals
        .open(
            &actor.id,
            &req.container,
            &argv,
            req.size.cols.unwrap_or(100),
            req.size.rows.unwrap_or(30),
        )
        .map_err(AppError::conflict)?;
    Ok(TerminalSession {
        id: t.id.clone(),
        target: t.target.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries() {
        assert_eq!(query_value("tail=20&follow=true", "tail"), Some("20"));
        assert_eq!(query_value("tail=20&follow=true", "follow"), Some("true"));
        assert_eq!(query_value("", "tail"), None);
    }
}

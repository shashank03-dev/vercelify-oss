//! A worker's runtime API (design §8.2), called by the main server over
//! the mesh with the worker's runtime token (auth.rs accepts it only from
//! the coordinator's mesh address, and only here). Everything is done by
//! the same platform code the main server runs for itself.

use crate::error::{ApiResult, AppError};
use crate::json::Json;
use crate::routes::AppState;
use crate::worker::Worker;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use vercelify_core::platform::{TerminalRequest, TerminalSession};
use vercelify_platform::docker::{ContainerState, Inspect};
use vercelify_platform::runtime::{apply_local, Applied, Apply, HealthWait, StartSpec};

fn worker(s: &AppState) -> Result<Arc<Worker>, AppError> {
    s.worker
        .clone()
        .ok_or_else(|| AppError::unavailable("this server isn't a worker of the platform engine"))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/runtime/containers", get(containers))
        .route("/runtime/containers/{name}", get(inspect))
        .route("/runtime/containers/{name}/logs", get(logs))
        .route("/runtime/apply", post(apply))
        .route("/runtime/routes", get(routes))
        .route("/runtime/instances", post(start_instance))
        .route("/runtime/health", post(health))
        .route("/runtime/terminals", post(terminal_open))
        .route("/runtime/terminals/{id}", delete(crate::platform_api::terminal_close))
        .route(
            "/runtime/terminals/{id}/output",
            get(crate::platform_api::terminal_output),
        )
        .route(
            "/runtime/terminals/{id}/input",
            post(crate::platform_api::terminal_input),
        )
        .route(
            "/runtime/terminals/{id}/resize",
            post(crate::platform_api::terminal_resize),
        )
}

async fn containers(State(s): State<AppState>) -> ApiResult<Vec<ContainerState>> {
    Ok(Json(worker(&s)?.deployer.docker.list_managed().await?))
}

async fn inspect(State(s): State<AppState>, Path(name): Path<String>) -> ApiResult<Inspect> {
    worker(&s)?
        .deployer
        .docker
        .inspect(&name)
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found(format!("no container {name}")))
}

#[derive(Deserialize, Default)]
struct LogQuery {
    #[serde(default)]
    follow: bool,
    tail: Option<u32>,
}

async fn logs(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<LogQuery>,
) -> Result<Response, AppError> {
    let w = worker(&s)?;
    crate::platform_api::docker_logs(&w.deployer.docker.bin, &name, q.tail.unwrap_or(200), q.follow)
}

/// Bodies are parsed with serde_json directly: the main server sends them,
/// and axum's extractor would add a second copy of every deserializer.
fn body<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T, AppError> {
    serde_json::from_slice(b).map_err(|e| AppError::bad_request(e.to_string()))
}

async fn apply(State(s): State<AppState>, b: bytes::Bytes) -> ApiResult<Applied> {
    let a: Apply = body(&b)?;
    Ok(Json(apply_local(&worker(&s)?.deployer, &a).await))
}

async fn routes(State(s): State<AppState>) -> ApiResult<Vec<String>> {
    Ok(Json(vercelify_platform::proxy::route_keys(&worker(&s)?.deployer.proxy)))
}

/// Start an instance; its progress as `{"log": …}` lines, then
/// `{"container": name}` or `{"error": …, "status": code}`.
async fn start_instance(State(s): State<AppState>, b: bytes::Bytes) -> Result<Response, AppError> {
    let spec: StartSpec = body(&b)?;
    let w = worker(&s)?;
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
    Ok((headers, axum::body::Body::new(crate::routes::LineBody(rx))).into_response())
}

async fn health(State(s): State<AppState>, b: bytes::Bytes) -> ApiResult<Value> {
    let h: HealthWait = body(&b)?;
    worker(&s)?.deployer.wait(&h).await?;
    Ok(Json(json!({ "healthy": true })))
}

#[derive(Deserialize)]
struct OpenTerminal {
    container: String,
    #[serde(default)]
    command: Option<Vec<String>>,
    #[serde(flatten)]
    size: TerminalRequest,
}

async fn terminal_open(
    State(s): State<AppState>,
    Extension(actor): Extension<crate::tokens::Actor>,
    b: bytes::Bytes,
) -> ApiResult<TerminalSession> {
    let req: OpenTerminal = body(&b)?;
    let w = worker(&s)?;
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
    Ok(Json(TerminalSession {
        id: t.id.clone(),
        target: t.target.clone(),
    }))
}

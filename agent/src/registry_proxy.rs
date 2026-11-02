//! Read-only registry streams. Device tokens cannot access this path, and raw
//! registry writes are loopback-only. Revoked servers fail each new request.
use crate::{config::Config, error::AppError, http, routes::AppState};
use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    response::Response,
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use vercelify_mesh::{cert::NodeKind, state::State as NodeState};

async fn read(request: Request, url: &str, bearer: Option<&str>) -> Result<Response, AppError> {
    if !matches!(*request.method(), hyper::Method::GET | hyper::Method::HEAD) {
        return Err(AppError::forbidden("registry reads only"));
    }
    let (parts, _body) = request.into_parts();
    let headers = parts.headers;
    let mut req = http::Req::get(url).timeout(Duration::from_secs(30));
    req.method = parts.method;
    req.bearer = bearer;
    req.accept = headers.get("accept").and_then(|v| v.to_str().ok());
    req.range = headers.get("range").and_then(|v| v.to_str().ok());
    let response = http::stream(req)
        .await
        .map_err(|_| AppError::unavailable("image registry is unavailable"))?;
    let (mut parts, body) = response.into_parts();
    parts.headers.remove("connection");
    Ok(Response::from_parts(parts, Body::new(body)))
}

pub async fn server_read(
    State(s): State<AppState>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    request: Request,
) -> Result<Response, AppError> {
    let co = crate::mesh::coordinator(&s)?;
    let node = co.authenticate(request.headers(), from.ip())?;
    if node.kind != NodeKind::Server {
        return Err(AppError::forbidden("only enrolled servers may read the image registry"));
    }
    let p = crate::platform_api::platform(&s)?;
    let registered = p.store.with(|c| {
        Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM nodes WHERE mesh_node_id=?1 AND role='worker')",
            [&node.id],
            |r| r.get::<_, bool>(0),
        )?)
    })?;
    if !registered {
        return Err(AppError::forbidden("server is not an active hosting worker"));
    }
    let path = request
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("")
        .strip_prefix("/v1/mesh/registry/")
        .filter(|p| p.starts_with("v2/"))
        .ok_or_else(|| AppError::bad_request("registry v2 path required"))?;
    let url = format!("http://127.0.0.1:5055/{path}");
    read(request, &url, None).await
}

pub async fn worker_read(State(cfg): State<Arc<Config>>, request: Request) -> Result<Response, AppError> {
    let state = NodeState::load(&cfg.mesh_dir)
        .map_err(|_| AppError::unavailable("mesh state unavailable"))?
        .ok_or_else(|| AppError::unavailable("worker is not enrolled"))?;
    let path = request
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .filter(|p| p.starts_with("/v2/"))
        .ok_or_else(|| AppError::bad_request("registry v2 path required"))?;
    let url = format!("{}/v1/mesh/registry{path}", state.api_url.trim_end_matches('/'));
    read(request, &url, Some(&state.node_token)).await
}

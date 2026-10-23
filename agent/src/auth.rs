use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use vercelify_core::ApiError;

use crate::routes::AppState;
use crate::tokens;

/// Bearer auth: the host's own token or a device token. Every change
/// (anything but GET) is written to the audit log with who made it.
pub async fn require_token(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_string();
    let Some(actor) = state.tokens.verify(&state.config, &presented) else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(ApiError {
                error: "missing or invalid agent token".into(),
            }),
        )
            .into_response();
    };
    let method = req.method().clone();
    // Inside the nested /v1 router the URI is stripped; log the real one.
    let path = req
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|u| u.0.path().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    req.extensions_mut().insert(actor.clone());
    let resp = next.run(req).await;
    if method != Method::GET && method != Method::HEAD {
        let cfg = state.config.clone();
        let status = resp.status().as_u16();
        let actor = actor.0;
        let method = method.to_string();
        tokio::task::spawn_blocking(move || tokens::audit(&cfg, &actor, &method, &path, status));
    }
    resp
}

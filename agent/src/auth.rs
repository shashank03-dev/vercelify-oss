use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use vercelify_core::ApiError;

use crate::routes::AppState;
use crate::tokens;
use vercelify_core::platform::Role;

/// What a role may change (design §12). Everyone may read. Owners may do
/// everything. Deployers may ship code: apps (not deleting them),
/// deployments, variables, domains, previews, databases (not deleting them),
/// project rules, auto-deploy, sleep, and starting/stopping containers.
/// Infrastructure (tokens, updates, backups, servers, storage moves, the
/// mesh, the power guard) stays with owners.
pub fn allowed(role: Role, method: &Method, path: &str) -> bool {
    if role == Role::Owner || method == Method::GET || method == Method::HEAD {
        return true;
    }
    if role != Role::Deployer {
        return false;
    }
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    let delete = method == Method::DELETE;
    match segs.as_slice() {
        ["v1", "platform", "apps"] => !delete,
        // Deleting an app or database (or a volume's data) is an owner's call;
        // their parts (variables, domains, previews) are a deployer's.
        ["v1", "platform", "apps", _] | ["v1", "platform", "databases", _] => !delete,
        ["v1", "platform", "apps", _, "volumes", ..] | ["v1", "platform", "databases", _, "volumes", ..] => !delete,
        ["v1", "platform", "apps", _, ..] | ["v1", "platform", "databases", _, ..] => true,
        ["v1", "platform", "databases"] => !delete,
        ["v1", "platform", "deployments", _, ..] => !delete,
        ["v1", "autodeploy", ..] | ["v1", "projects", ..] | ["v1", "sleep", ..] => true,
        ["v1", "containers", _, action] => matches!(*action, "start" | "stop" | "restart"),
        ["v1", "llm", "chat"] => true,
        _ => false,
    }
}

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
    if !allowed(actor.role, &method, &path) {
        tokens_audit(&state, &actor.id, &method, &path, 403);
        return (
            StatusCode::FORBIDDEN,
            Json(ApiError {
                error: format!(
                    "this token is a {} token; ask an owner (or use an owner token) for this",
                    actor.role.as_str()
                ),
            }),
        )
            .into_response();
    }
    req.extensions_mut().insert(actor.clone());
    let resp = next.run(req).await;
    if method != Method::GET && method != Method::HEAD {
        tokens_audit(&state, &actor.id, &method, &path, resp.status().as_u16());
    }
    resp
}

fn tokens_audit(state: &AppState, actor: &str, method: &Method, path: &str, status: u16) {
    let cfg = state.config.clone();
    let (actor, method, path) = (actor.to_string(), method.to_string(), path.to_string());
    tokio::task::spawn_blocking(move || tokens::audit(&cfg, &actor, &method, &path, status));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_policy() {
        let can = |role, m: Method, p: &str| allowed(role, &m, p);
        for p in ["/v1/system", "/v1/platform/apps", "/v1/tokens"] {
            assert!(can(Role::Viewer, Method::GET, p));
        }
        assert!(!can(Role::Viewer, Method::POST, "/v1/platform/apps"));
        assert!(!can(Role::Viewer, Method::POST, "/v1/llm/chat"));

        let d = Role::Deployer;
        assert!(can(d, Method::POST, "/v1/platform/apps"));
        assert!(can(d, Method::PATCH, "/v1/platform/apps/blog"));
        assert!(
            !can(d, Method::DELETE, "/v1/platform/apps/blog"),
            "deleting an app: owner"
        );
        assert!(can(d, Method::POST, "/v1/platform/apps/blog/deploy"));
        assert!(can(d, Method::PUT, "/v1/platform/apps/blog/env"));
        assert!(can(d, Method::DELETE, "/v1/platform/apps/blog/env/KEY"));
        assert!(can(d, Method::DELETE, "/v1/platform/apps/blog/domains/x.example.com"));
        assert!(can(d, Method::POST, "/v1/platform/apps/blog/volumes"));
        assert!(
            !can(d, Method::DELETE, "/v1/platform/apps/blog/volumes/data"),
            "data: owner"
        );
        assert!(can(d, Method::POST, "/v1/platform/deployments/dpl_x/cancel"));
        assert!(!can(d, Method::DELETE, "/v1/platform/databases/pg"));
        assert!(can(d, Method::POST, "/v1/containers/abc/restart"));
        assert!(!can(d, Method::POST, "/v1/containers/abc/remove"));
        for p in [
            "/v1/tokens",
            "/v1/update",
            "/v1/backup/run",
            "/v1/services/docker/restart",
            "/v1/platform/projects",
            "/v1/coolify/volumes/3/move",
        ] {
            assert!(!can(d, Method::POST, p), "{p}");
        }
        assert!(can(Role::Owner, Method::DELETE, "/v1/platform/apps/blog"));
    }
}

//! The host manager's two listeners, both on the computer's loopback:
//!
//! - the management API (`127.0.0.1:7425`, design §9): every route, reads
//!   included, needs the owner token; repeated bad tokens are slowed down;
//! - the guest's status listener (`127.0.0.1:7426`): the real computer's
//!   battery and temperature, and cloud-init's seed. Read-only, nothing
//!   secret, and the only thing the guest is told about.

use crate::manager::Manager;
use crate::Error;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vercelify_core::host::{
    ChangeEnvironment, ConfirmedChange, CreateEnvironment, EnvironmentPairing, EnvironmentRecommendation,
    HostCapabilities, HostOperation, HostStatus, ManagedEnvironment, StartEnvironment,
};

/// Bad tokens allowed per minute before every unauthenticated request waits
/// out the minute.
pub const AUTH_FAILURES_PER_MINUTE: u32 = 20;

#[derive(Clone)]
pub struct Api {
    pub manager: Arc<Manager>,
    failures: Arc<Mutex<(Instant, u32)>>,
}

impl Api {
    pub fn new(manager: Arc<Manager>) -> Api {
        Api {
            manager,
            failures: Arc::new(Mutex::new((Instant::now(), 0))),
        }
    }
}

pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = StatusCode::from_u16(self.0.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (code, Json(serde_json::json!({ "error": self.0.message() }))).into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

fn authorize(api: &Api, headers: &HeaderMap) -> Result<(), ApiError> {
    use subtle::ConstantTimeEq;
    let mut f = api.failures.lock().unwrap_or_else(|p| p.into_inner());
    if f.0.elapsed() > Duration::from_secs(60) {
        *f = (Instant::now(), 0);
    }
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let ok: bool = crate::state::sha256_hex(presented)
        .as_bytes()
        .ct_eq(api.manager.owner_token_sha256().as_bytes())
        .into();
    if ok && !presented.is_empty() {
        return Ok(());
    }
    f.1 += 1;
    if f.1 > AUTH_FAILURES_PER_MINUTE {
        return Err(ApiError(Error::Unavailable(
            "too many failed attempts: wait a minute".into(),
        )));
    }
    Err(ApiError(Error::Unauthorized))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> crate::Result<T> + Send + 'static) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError(Error::Internal(e.to_string())))?
        .map_err(ApiError)
}

macro_rules! owner {
    ($api:expr, $h:expr) => {
        authorize(&$api, &$h)?
    };
}

async fn capabilities(State(api): State<Api>, h: HeaderMap) -> ApiResult<HostCapabilities> {
    owner!(api, h);
    let m = api.manager.clone();
    Ok(Json(blocking(move || Ok(m.capabilities())).await?))
}

async fn recommendation(State(api): State<Api>, h: HeaderMap) -> ApiResult<EnvironmentRecommendation> {
    owner!(api, h);
    let m = api.manager.clone();
    Ok(Json(blocking(move || Ok(m.recommendation())).await?))
}

async fn environment(State(api): State<Api>, h: HeaderMap) -> ApiResult<ManagedEnvironment> {
    owner!(api, h);
    Ok(Json(api.manager.environment()))
}

async fn create(State(api): State<Api>, h: HeaderMap, Json(req): Json<CreateEnvironment>) -> ApiResult<HostOperation> {
    owner!(api, h);
    let m = api.manager.clone();
    Ok(Json(blocking(move || m.create(&req)).await?))
}

async fn change(State(api): State<Api>, h: HeaderMap, Json(req): Json<ChangeEnvironment>) -> ApiResult<HostOperation> {
    owner!(api, h);
    let m = api.manager.clone();
    Ok(Json(blocking(move || m.change(&req)).await?))
}

async fn remove(State(api): State<Api>, h: HeaderMap, Json(req): Json<ConfirmedChange>) -> ApiResult<HostOperation> {
    owner!(api, h);
    let m = api.manager.clone();
    Ok(Json(blocking(move || m.remove(&req)).await?))
}

async fn start(State(api): State<Api>, h: HeaderMap, Json(req): Json<StartEnvironment>) -> ApiResult<HostOperation> {
    owner!(api, h);
    let m = api.manager.clone();
    Ok(Json(blocking(move || m.start(&req)).await?))
}

async fn stop(State(api): State<Api>, h: HeaderMap, Json(req): Json<ConfirmedChange>) -> ApiResult<HostOperation> {
    owner!(api, h);
    let m = api.manager.clone();
    Ok(Json(blocking(move || m.stop(&req)).await?))
}

async fn pairing(State(api): State<Api>, h: HeaderMap) -> ApiResult<EnvironmentPairing> {
    owner!(api, h);
    let m = api.manager.clone();
    Ok(Json(blocking(move || m.pairing()).await?))
}

async fn operation(State(api): State<Api>, h: HeaderMap, Path(id): Path<String>) -> ApiResult<HostOperation> {
    owner!(api, h);
    api.manager
        .operation(&id)
        .map(Json)
        .ok_or_else(|| ApiError(Error::NotFound(format!("no operation {id}"))))
}

pub fn router(api: Api) -> Router {
    Router::new()
        .route("/v1/host/capabilities", get(capabilities))
        .route(
            "/v1/host/environment/recommendation",
            axum::routing::post(recommendation),
        )
        .route(
            "/v1/host/environment",
            get(environment).post(create).patch(change).delete(remove),
        )
        .route("/v1/host/environment/start", axum::routing::post(start))
        .route("/v1/host/environment/stop", axum::routing::post(stop))
        .route("/v1/host/environment/pairing", axum::routing::post(pairing))
        .route("/v1/host/operations/{id}", get(operation))
        .with_state(api)
}

// ----- the guest's listener ------------------------------------------------------------

#[derive(Clone)]
pub struct Guest {
    pub manager: Arc<Manager>,
    pub status: Arc<Mutex<HostStatus>>,
}

#[derive(Serialize)]
struct Nothing {}

async fn host_status(State(g): State<Guest>) -> Json<HostStatus> {
    Json(g.status.lock().unwrap_or_else(|p| p.into_inner()).clone())
}

async fn seed(State(g): State<Guest>, Path(file): Path<String>) -> Response {
    match g.manager.provider.seed(&file) {
        Some(body) => (StatusCode::OK, body).into_response(),
        None => (StatusCode::NOT_FOUND, Json(Nothing {})).into_response(),
    }
}

pub fn guest_router(g: Guest) -> Router {
    Router::new()
        .route("/v1/host/status", get(host_status))
        .route("/seed/{file}", get(seed))
        .with_state(g)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Fake;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    use vercelify_core::host::{HostOs, Virtualization, GIB};

    fn api() -> (tempfile::TempDir, Router) {
        let d = tempfile::tempdir().unwrap();
        let m = Manager::open(
            Arc::new(Fake::default()),
            d.path().join("state.json"),
            crate::state::sha256_hex("owner-token"),
            Box::new(|| HostCapabilities {
                os: HostOs::Macos,
                os_version: "15".into(),
                arch: "aarch64".into(),
                logical_cpus: Some(8),
                memory_total_bytes: Some(16 * GIB),
                memory_available_bytes: Some(10 * GIB),
                disk_free_bytes: Some(200 * GIB),
                disk_rotational: None,
                on_battery: None,
                virtualization: Virtualization::Ready,
                provider: None,
                measured_at: 1,
            }),
            Box::new(|| 1),
            "mac".into(),
            "v1".into(),
        )
        .unwrap()
        .for_tests();
        (d, router(Api::new(m)))
    }

    async fn call(r: &Router, method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, String) {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let res = r
            .clone()
            .oneshot(b.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let code = res.status().as_u16();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (code, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn every_route_needs_the_owner_token() {
        let (_d, r) = api();
        for (m, p) in [
            ("GET", "/v1/host/capabilities"),
            ("POST", "/v1/host/environment/recommendation"),
            ("GET", "/v1/host/environment"),
            ("GET", "/v1/host/operations/op_x"),
        ] {
            assert_eq!(call(&r, m, p, None, "").await.0, 401, "{m} {p}");
            assert_eq!(call(&r, m, p, Some("wrong"), "").await.0, 401, "{m} {p}");
        }
        let (code, body) = call(&r, "GET", "/v1/host/environment", Some("owner-token"), "").await;
        assert_eq!(code, 200);
        assert!(body.contains("\"not_created\""), "{body}");
        let (code, body) = call(
            &r,
            "POST",
            "/v1/host/environment/recommendation",
            Some("owner-token"),
            "",
        )
        .await;
        assert_eq!(code, 200, "{body}");
        assert!(body.contains("\"grant\":{"));
    }

    #[tokio::test]
    async fn bad_tokens_are_slowed_down() {
        let (_d, r) = api();
        for _ in 0..AUTH_FAILURES_PER_MINUTE {
            assert_eq!(call(&r, "GET", "/v1/host/environment", Some("guess"), "").await.0, 401);
        }
        assert_eq!(call(&r, "GET", "/v1/host/environment", Some("guess"), "").await.0, 503);
    }

    #[tokio::test]
    async fn mutations_check_their_body() {
        let (_d, r) = api();
        let (code, _) = call(&r, "POST", "/v1/host/environment", Some("owner-token"), "{}").await;
        assert_eq!(code, 422, "a malformed request is refused");
        let (code, body) = call(
            &r,
            "POST",
            "/v1/host/environment/stop",
            Some("owner-token"),
            r#"{"idempotency_key":"k","revision":0,"acknowledged":false}"#,
        )
        .await;
        assert_eq!(code, 400, "{body}");
    }
}

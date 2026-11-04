//! Paired-owner management; invitation secrets are returned only on creation.
use crate::{error::AppError, json::Json, platform_api::platform, routes::AppState, tokens::Actor};
use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    routing::{delete, get},
    Extension, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use vercelify_core::platform::Role;
use vercelify_platform::{apps, guest_shares as shares};
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/platform/settings/guest-links", get(config).put(configure))
        .route("/platform/settings/guest-links/rotate", axum::routing::post(rotate))
        .route("/platform/apps/{app}/previews/{pr}/guest-links", get(list).post(create))
        .route("/platform/guest-links/{id}", delete(revoke))
}
fn owner(actor: &Actor) -> Result<(), AppError> {
    if actor.role != Role::Owner {
        return Err(AppError::forbidden(
            "only an owner may configure or issue guest invitations",
        ));
    }
    Ok(())
}
fn output(value: Value) -> Response {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(axum::http::header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    response
}
async fn config(State(s): State<AppState>) -> Result<Response, AppError> {
    let cfg = platform(&s)?.store.call(|c| shares::config(c)).await?;
    Ok(output(
        json!({"config":cfg,"gateway_available":s.guest.as_ref().is_some_and(|g|g.is_ready()),"session_seconds":shares::SESSION_SECONDS,"dns_tls_verified":false,"scope":"main-node private previews; operator must route the guest HTTPS domain here"}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configure {
    domain: Option<String>,
    #[serde(default = "port")]
    public_port: u16,
}
fn port() -> u16 {
    443
}
async fn configure(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(req): Json<Configure>,
) -> Result<Response, AppError> {
    owner(&actor)?;
    let gateway = s
        .guest
        .as_ref()
        .ok_or_else(|| AppError::unavailable("guest links need the platform and a working wake listener"))?;
    if !gateway.is_ready() {
        return Err(AppError::unavailable("guest listener is not running"));
    }
    let cfg = platform(&s)?
        .store
        .call(move |c| shares::configure_at(c, req.domain.as_deref(), req.public_port))
        .await?;
    gateway.refresh()?;
    s.events.changed("guest-links");
    Ok(output(json!({"config":cfg,"dns_tls_verified":false})))
}
async fn list(State(s): State<AppState>, Path((app, pr)): Path<(String, u32)>) -> Result<Response, AppError> {
    let rows = platform(&s)?
        .store
        .call(move |c| {
            let app = apps::find(c, &app)?;
            shares::list(c, &app.id, pr)
        })
        .await?;
    Ok(output(json!(rows)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    #[serde(default = "ttl")]
    expires_in: i64,
    #[serde(default = "single")]
    single_use: bool,
}
fn ttl() -> i64 {
    3600
}
fn single() -> bool {
    true
}
async fn create(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path((app, pr)): Path<(String, u32)>,
    Json(req): Json<Create>,
) -> Result<Response, AppError> {
    owner(&actor)?;
    let gateway = s
        .guest
        .as_ref()
        .ok_or_else(|| AppError::unavailable("guest gateway unavailable"))?;
    if !gateway.is_ready() {
        return Err(AppError::unavailable("guest listener is not running"));
    }
    let p = platform(&s)?;
    let issued = p
        .store
        .call(move |c| {
            shares::create(
                c,
                &app,
                pr,
                &actor.id,
                req.expires_in,
                req.single_use,
                chrono::Utc::now().timestamp(),
            )
        })
        .await?;
    gateway.refresh()?;
    let report = p.reconciler.once().await?;
    if !report.errors.is_empty() {
        let id = issued.grant.id.clone();
        p.store
            .call(move |c| shares::revoke(c, &id, chrono::Utc::now().timestamp()))
            .await?;
        return Err(AppError::unavailable(
            "preview routing needs repair; invitation was revoked",
        ));
    }
    s.projects.tick().await;
    let asleep = p
        .store
        .with(|c| Ok(vercelify_platform::previews::get(c, &issued.grant.app_id, pr)?.is_some_and(|p| p.asleep)))?;
    if !gateway.wait_route(&issued.grant.host, false).await
        || !asleep && !gateway.wait_route(&issued.grant.host, true).await
    {
        let id = issued.grant.id.clone();
        p.store
            .call(move |c| shares::revoke(c, &id, chrono::Utc::now().timestamp()))
            .await?;
        return Err(AppError::unavailable(
            "guest routing did not become ready; invitation was revoked",
        ));
    }
    let port = p.store.with(shares::config)?.public_port;
    let authority = if port == 443 {
        issued.grant.host.clone()
    } else {
        format!("{}:{port}", issued.grant.host)
    };
    let url = format!("https://{authority}/__vfy/invite#vfy={}", issued.secret.as_str());
    s.events.changed("guest-links");
    Ok(output(
        json!({"grant":issued.grant,"url":url,"secret_shown_once":true,"audience":"anyone with the complete link"}),
    ))
}
async fn revoke(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    owner(&actor)?;
    platform(&s)?
        .store
        .call(move |c| shares::revoke(c, &id, chrono::Utc::now().timestamp()))
        .await?;
    if let Some(gateway) = &s.guest {
        gateway.refresh()?;
    }
    s.events.changed("guest-links");
    Ok(output(json!({"revoked":true})))
}
async fn rotate(State(s): State<AppState>, Extension(actor): Extension<Actor>) -> Result<Response, AppError> {
    owner(&actor)?;
    platform(&s)?
        .store
        .call(move |c| shares::rotate(c, chrono::Utc::now().timestamp()))
        .await?;
    if let Some(gateway) = &s.guest {
        gateway.refresh()?;
    }
    s.events.changed("guest-links");
    Ok(output(json!({"revoked_all":true,"clock_reset":true})))
}

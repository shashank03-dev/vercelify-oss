//! HTTP API of the platform engine: `/v1/platform/…` (design doc §12).
//!
//! Handlers validate nothing themselves: the platform crate's repositories
//! own every rule, so the API, the CLI and the migration share them. Writes
//! nudge the cockpit through a `changed` event.

use crate::error::{ApiResult, AppError};
use crate::platform::Platform;
use crate::routes::AppState;
use crate::tokens::Actor;
use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use vercelify_core::platform::*;
use vercelify_platform::engine::{EngineEvent, Enqueue};
use vercelify_platform::{apps, deployments, env, nodes, previews, projects, volumes};

impl From<vercelify_platform::Error> for AppError {
    fn from(e: vercelify_platform::Error) -> Self {
        use vercelify_platform::Error as E;
        match e {
            E::NotFound(m) => AppError::not_found(m),
            E::Conflict(m) => AppError::conflict(m),
            E::Invalid(m) => AppError::bad_request(m),
            E::Unavailable(m) => AppError::unavailable(m),
            E::Internal(m) => AppError::internal(m),
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/platform/projects", get(project_list).post(project_create))
        .route(
            "/platform/projects/{id}",
            get(project_get).patch(project_update).delete(project_delete),
        )
        .route("/platform/projects/{id}/environments", post(environment_create))
        .route("/platform/projects/{id}/env", get(project_env).put(project_env_put))
        .route("/platform/projects/{id}/env/{key}", delete(project_env_delete))
        .route("/platform/environments/{id}", delete(environment_delete))
        .route(
            "/platform/environments/{id}/env",
            get(environment_env).put(environment_env_put),
        )
        .route("/platform/environments/{id}/env/{key}", delete(environment_env_delete))
        .route("/platform/apps", get(app_list).post(app_create))
        .route("/platform/apps/{app}", get(app_get).patch(app_patch).delete(app_delete))
        .route("/platform/apps/{app}/env", get(app_env).put(app_env_put))
        .route("/platform/apps/{app}/env/import", post(app_env_import))
        .route("/platform/apps/{app}/env/{key}", delete(app_env_delete))
        .route("/platform/apps/{app}/domains", get(app_domains).post(app_domain_add))
        .route("/platform/apps/{app}/domains/{host}", delete(app_domain_remove))
        .route("/platform/apps/{app}/volumes", get(app_volumes).post(app_volume_create))
        .route("/platform/apps/{app}/volumes/{name}", delete(app_volume_delete))
        .route("/platform/apps/{app}/deployments", get(app_deployments))
        .route("/platform/apps/{app}/deploy", post(app_deploy))
        .route("/platform/apps/{app}/rollback", post(app_rollback))
        .route("/platform/apps/{app}/previews", get(app_previews))
        .route("/platform/deployments/{id}", get(deployment_get))
        .route("/platform/deployments/{id}/cancel", post(deployment_cancel))
        .route("/platform/deployments/{id}/logs", get(deployment_logs))
        .route("/platform/nodes", get(node_list))
}

/// The platform of this server, or why there is none.
pub fn platform(s: &AppState) -> Result<Arc<Platform>, AppError> {
    if let Some(p) = &s.platform {
        return Ok(p.clone());
    }
    Err(match &s.platform_error {
        Some(e) => AppError::unavailable(format!("the platform engine failed to start: {}", e.0)),
        None if s.config.role() == "worker" => {
            AppError::unavailable("this is a worker: manage apps on its main server")
        }
        None => AppError::unavailable(
            "this server runs the Coolify engine (agent.toml: engine = \"coolify\"); the platform API is off",
        ),
    })
}

/// Run `f` in a transaction on the blocking pool.
async fn tx<T, F>(s: &AppState, f: F) -> Result<T, AppError>
where
    T: Send + 'static,
    F: FnOnce(
            &vercelify_platform::rusqlite::Transaction,
            &vercelify_platform::secrets::MasterKey,
        ) -> vercelify_platform::Result<T>
        + Send
        + 'static,
{
    let p = platform(s)?;
    let key = p.key.clone();
    Ok(p.store.call(move |c| f(c, &key)).await?)
}

fn changed(s: &AppState, topic: &str) {
    s.events.changed(topic);
}

// ----- projects -------------------------------------------------------------

async fn project_list(State(s): State<AppState>) -> ApiResult<Vec<Project>> {
    Ok(Json(tx(&s, |c, _| projects::list(c)).await?))
}

async fn project_create(State(s): State<AppState>, Json(req): Json<ProjectRequest>) -> ApiResult<Project> {
    let p = tx(&s, move |c, _| projects::create(c, &req)).await?;
    changed(&s, "apps");
    Ok(Json(p))
}

async fn project_get(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Project> {
    Ok(Json(tx(&s, move |c, _| projects::get(c, &id)).await?))
}

async fn project_update(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ProjectRequest>,
) -> ApiResult<Project> {
    let p = tx(&s, move |c, _| projects::update(c, &id, &req)).await?;
    changed(&s, "apps");
    Ok(Json(p))
}

async fn project_delete(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Value> {
    tx(&s, move |c, _| projects::delete(c, &id)).await?;
    changed(&s, "apps");
    Ok(Json(json!({ "deleted": true })))
}

async fn environment_create(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<EnvironmentRequest>,
) -> ApiResult<Environment> {
    let e = tx(&s, move |c, _| projects::add_environment(c, &id, &req.name)).await?;
    changed(&s, "apps");
    Ok(Json(e))
}

async fn environment_delete(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Value> {
    tx(&s, move |c, _| projects::delete_environment(c, &id)).await?;
    changed(&s, "apps");
    Ok(Json(json!({ "deleted": true })))
}

// ----- variables --------------------------------------------------------------

#[derive(Deserialize, Default)]
struct EnvQuery {
    #[serde(default)]
    reveal: bool,
    #[serde(default)]
    preview_only: bool,
}

fn check_reveal(actor: &Actor, q: &EnvQuery) -> Result<(), AppError> {
    if q.reveal && actor.role < Role::Deployer {
        return Err(AppError {
            status: axum::http::StatusCode::FORBIDDEN,
            message: "a viewer token can't reveal secret values".into(),
        });
    }
    Ok(())
}

async fn list_vars(s: &AppState, scope: VarScope, id: String, reveal: bool) -> ApiResult<Vec<EnvVar>> {
    Ok(Json(tx(s, move |c, k| env::list(c, k, scope, &id, reveal)).await?))
}

async fn put_vars(s: &AppState, scope: VarScope, id: String, req: EnvVarsPut) -> ApiResult<Value> {
    let n = tx(s, move |c, k| env::put(c, k, scope, &id, &req)).await?;
    changed(s, "apps");
    Ok(Json(json!({ "saved": n })))
}

async fn delete_var(s: &AppState, scope: VarScope, id: String, key: String, preview_only: bool) -> ApiResult<Value> {
    tx(s, move |c, _| env::delete(c, scope, &id, &key, preview_only)).await?;
    changed(s, "apps");
    Ok(Json(json!({ "deleted": true })))
}

async fn project_env(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Query(q): Query<EnvQuery>,
) -> ApiResult<Vec<EnvVar>> {
    check_reveal(&actor, &q)?;
    list_vars(&s, VarScope::Project, id, q.reveal).await
}

async fn project_env_put(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<EnvVarsPut>,
) -> ApiResult<Value> {
    put_vars(&s, VarScope::Project, id, req).await
}

async fn project_env_delete(
    State(s): State<AppState>,
    Path((id, key)): Path<(String, String)>,
    Query(q): Query<EnvQuery>,
) -> ApiResult<Value> {
    delete_var(&s, VarScope::Project, id, key, q.preview_only).await
}

async fn environment_env(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Query(q): Query<EnvQuery>,
) -> ApiResult<Vec<EnvVar>> {
    check_reveal(&actor, &q)?;
    list_vars(&s, VarScope::Environment, id, q.reveal).await
}

async fn environment_env_put(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<EnvVarsPut>,
) -> ApiResult<Value> {
    put_vars(&s, VarScope::Environment, id, req).await
}

async fn environment_env_delete(
    State(s): State<AppState>,
    Path((id, key)): Path<(String, String)>,
    Query(q): Query<EnvQuery>,
) -> ApiResult<Value> {
    delete_var(&s, VarScope::Environment, id, key, q.preview_only).await
}

/// Apps are addressed by id or name; resolve to the id.
async fn app_id(s: &AppState, app: String) -> Result<String, AppError> {
    tx(s, move |c, _| Ok(apps::find(c, &app)?.id)).await
}

async fn app_env(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(app): Path<String>,
    Query(q): Query<EnvQuery>,
) -> ApiResult<Vec<EnvVar>> {
    check_reveal(&actor, &q)?;
    let id = app_id(&s, app).await?;
    list_vars(&s, VarScope::App, id, q.reveal).await
}

async fn app_env_put(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Json(req): Json<EnvVarsPut>,
) -> ApiResult<Value> {
    let id = app_id(&s, app).await?;
    put_vars(&s, VarScope::App, id, req).await
}

#[derive(Deserialize)]
struct EnvImport {
    /// Contents of a `.env` file.
    content: String,
    #[serde(default)]
    replace: bool,
    #[serde(default)]
    preview_only: bool,
    #[serde(default)]
    build: bool,
}

async fn app_env_import(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Json(req): Json<EnvImport>,
) -> ApiResult<Value> {
    let parsed = env::parse_dotenv(&req.content)?;
    let vars = parsed
        .into_iter()
        .map(|(key, value)| EnvVarInput {
            key,
            value,
            build: Some(req.build),
            preview_only: Some(req.preview_only),
            secret: None,
        })
        .collect();
    let id = app_id(&s, app).await?;
    put_vars(
        &s,
        VarScope::App,
        id,
        EnvVarsPut {
            vars,
            replace: req.replace,
        },
    )
    .await
}

async fn app_env_delete(
    State(s): State<AppState>,
    Path((app, key)): Path<(String, String)>,
    Query(q): Query<EnvQuery>,
) -> ApiResult<Value> {
    let id = app_id(&s, app).await?;
    delete_var(&s, VarScope::App, id, key, q.preview_only).await
}

// ----- apps -------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct AppListQuery {
    project: Option<String>,
}

async fn app_list(State(s): State<AppState>, Query(q): Query<AppListQuery>) -> ApiResult<Vec<App>> {
    Ok(Json(tx(&s, move |c, _| apps::list(c, q.project.as_deref())).await?))
}

async fn app_create(State(s): State<AppState>, Json(req): Json<AppCreate>) -> ApiResult<App> {
    let a = tx(&s, move |c, _| apps::create(c, &req)).await?;
    changed(&s, "apps");
    Ok(Json(a))
}

async fn app_get(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<App> {
    Ok(Json(tx(&s, move |c, _| apps::find(c, &app)).await?))
}

async fn app_patch(State(s): State<AppState>, Path(app): Path<String>, Json(req): Json<AppPatch>) -> ApiResult<App> {
    let a = tx(&s, move |c, _| {
        let id = apps::find(c, &app)?.id;
        apps::patch(c, &id, &req)
    })
    .await?;
    changed(&s, "apps");
    Ok(Json(a))
}

async fn app_delete(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<Value> {
    let a = tx(&s, move |c, _| {
        let id = apps::find(c, &app)?.id;
        apps::delete(c, &id)
    })
    .await?;
    changed(&s, "apps");
    Ok(Json(json!({ "deleted": a.name })))
}

async fn app_domains(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<Vec<AppDomain>> {
    Ok(Json(tx(&s, move |c, _| Ok(apps::find(c, &app)?.domains)).await?))
}

async fn app_domain_add(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Json(req): Json<DomainRequest>,
) -> ApiResult<AppDomain> {
    let d = tx(&s, move |c, _| {
        let id = apps::find(c, &app)?.id;
        apps::add_domain(c, &id, &req)
    })
    .await?;
    changed(&s, "apps");
    Ok(Json(d))
}

async fn app_domain_remove(State(s): State<AppState>, Path((app, host)): Path<(String, String)>) -> ApiResult<Value> {
    tx(&s, move |c, _| {
        let id = apps::find(c, &app)?.id;
        apps::remove_domain(c, &id, &host)
    })
    .await?;
    changed(&s, "apps");
    Ok(Json(json!({ "deleted": true })))
}

async fn app_volumes(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<Vec<AppVolume>> {
    Ok(Json(
        tx(&s, move |c, _| {
            let id = apps::find(c, &app)?.id;
            volumes::list(c, VolumeOwner::App, &id)
        })
        .await?,
    ))
}

async fn app_volume_create(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Json(req): Json<AppVolumeRequest>,
) -> ApiResult<AppVolume> {
    let v = tx(&s, move |c, _| {
        let id = apps::find(c, &app)?.id;
        volumes::create(c, VolumeOwner::App, &id, &req)
    })
    .await?;
    changed(&s, "volumes");
    Ok(Json(v))
}

async fn app_volume_delete(State(s): State<AppState>, Path((app, name)): Path<(String, String)>) -> ApiResult<Value> {
    tx(&s, move |c, _| {
        let id = apps::find(c, &app)?.id;
        volumes::delete(c, VolumeOwner::App, &id, &name)
    })
    .await?;
    changed(&s, "volumes");
    Ok(Json(json!({ "deleted": true })))
}

// ----- deployments and servers --------------------------------------------------

#[derive(Deserialize, Default)]
struct Limit {
    limit: Option<u32>,
}

async fn app_deployments(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Query(q): Query<Limit>,
) -> ApiResult<Vec<Deployment>> {
    Ok(Json(
        tx(&s, move |c, _| {
            let id = apps::find(c, &app)?.id;
            deployments::list(c, &id, q.limit.unwrap_or(30))
        })
        .await?,
    ))
}

async fn deployment_get(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Deployment> {
    Ok(Json(tx(&s, move |c, _| deployments::get(c, &id)).await?))
}

async fn node_list(State(s): State<AppState>) -> ApiResult<Vec<PlatformNode>> {
    Ok(Json(tx(&s, |c, _| nodes::list(c)).await?))
}

// ----- deploying ------------------------------------------------------------------

/// Engine events → the cockpit's event stream and the alerts.
pub fn on_engine_event(events: &Arc<crate::events::Events>, watcher: &Arc<crate::watch::Watcher>, e: EngineEvent) {
    events.changed("deployments");
    let (source, status, title, detail) = match e {
        EngineEvent::Changed => return,
        EngineEvent::Succeeded { app, deployment, url } => (
            format!("deploy:{app}"),
            vercelify_core::CheckStatus::Ok,
            format!("{app} deployed"),
            format!(
                "deployment #{} is live{}",
                deployment.number,
                url.map(|u| format!(" at {u}")).unwrap_or_default()
            ),
        ),
        EngineEvent::Failed { app, deployment, error } => (
            format!("deploy:{app}"),
            vercelify_core::CheckStatus::Bad,
            format!("{app}: deployment #{} failed", deployment.number),
            error.lines().next().unwrap_or("").chars().take(300).collect(),
        ),
    };
    let w = watcher.clone();
    tokio::spawn(async move {
        w.raise(crate::watch::Change {
            resolved: status == vercelify_core::CheckStatus::Ok,
            source,
            status,
            title,
            detail,
        })
        .await;
    });
}

async fn engine_call<T: Send + 'static>(
    s: &AppState,
    f: impl FnOnce(&vercelify_platform::engine::Engine) -> vercelify_platform::Result<T> + Send + 'static,
) -> Result<T, AppError> {
    let p = platform(s)?;
    let engine = p.engine.clone();
    tokio::task::spawn_blocking(move || f(&engine))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .map_err(AppError::from)
}

async fn app_deploy(
    State(s): State<AppState>,
    Path(app): Path<String>,
    body: Option<Json<DeployRequest>>,
) -> ApiResult<Deployment> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let d = engine_call(&s, move |e| {
        e.enqueue(
            &app,
            Enqueue {
                trigger: Some(DeploymentTrigger::Manual),
                commit_sha: req.commit_sha,
                force_rebuild: req.force_rebuild,
                ..Enqueue::default()
            },
        )
    })
    .await?;
    Ok(Json(d))
}

async fn app_rollback(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Json(req): Json<RollbackRequest>,
) -> ApiResult<Deployment> {
    let d = engine_call(&s, move |e| {
        e.enqueue(
            &app,
            Enqueue {
                trigger: Some(DeploymentTrigger::Rollback),
                rollback_of: Some(req.deployment_id),
                ..Enqueue::default()
            },
        )
    })
    .await?;
    Ok(Json(d))
}

async fn deployment_cancel(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Deployment> {
    Ok(Json(engine_call(&s, move |e| e.cancel(&id)).await?))
}

async fn app_previews(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<Vec<previews::Preview>> {
    Ok(Json(
        tx(&s, move |c, _| {
            let id = apps::find(c, &app)?.id;
            previews::list(c, &id, false)
        })
        .await?,
    ))
}

#[derive(Deserialize, Default)]
struct LogQuery {
    #[serde(default)]
    follow: bool,
    tail: Option<usize>,
}

/// A deployment's log: JSON lines. With `follow=true` the response stays
/// open and streams new lines until the deployment's log is complete.
async fn deployment_logs(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<LogQuery>,
) -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;
    let p = platform(&s)?;
    let (d, app_name) = {
        let id = id.clone();
        tx(&s, move |c, _| {
            let d = deployments::get(c, &id)?;
            let name = apps::get(c, &d.app_id)?.name;
            Ok((d, name))
        })
        .await?
    };
    let logs = p.engine.logs.clone();
    // Subscribe before reading the file, so no line falls between the two.
    let live = if q.follow { logs.follow(&d.id) } else { None };
    let lines = logs.read(&app_name, &d.id, q.tail);
    let headers = [
        (axum::http::header::CONTENT_TYPE, "application/x-ndjson"),
        (axum::http::header::CACHE_CONTROL, "no-store"),
    ];
    let encode = |l: &vercelify_platform::logs::LogLine| {
        let mut v = serde_json::to_vec(l).unwrap_or_default();
        v.push(b'\n');
        bytes::Bytes::from(v)
    };
    let Some(mut live) = live else {
        let body: Vec<u8> = lines.iter().flat_map(|l| encode(l).to_vec()).collect();
        return Ok((headers, body).into_response());
    };
    let (tx_, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(256);
    tokio::spawn(async move {
        // The file may end with lines that also arrive live: skip by position.
        let next = lines.last().map(|l| l.seq + 1).unwrap_or(0);
        for l in &lines {
            if tx_.send(encode(l)).await.is_err() {
                return;
            }
        }
        loop {
            match live.recv().await {
                Ok(l) => {
                    if l.seq < next {
                        continue;
                    }
                    if tx_.send(encode(&l)).await.is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            }
        }
    });
    Ok((headers, axum::body::Body::new(crate::routes::LineBody(rx))).into_response())
}

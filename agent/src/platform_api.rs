//! HTTP API of the platform engine: `/v1/platform/…` (design doc §12).
//!
//! Handlers validate nothing themselves: the platform crate's repositories
//! own every rule, so the API, the CLI and the migration share them. Writes
//! nudge the cockpit through a `changed` event.

use crate::error::{ApiResult, AppError};
use crate::json::Json;
use crate::platform::Platform;
use crate::routes::AppState;
use crate::tokens::Actor;
use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Extension, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use vercelify_core::platform::*;
use vercelify_platform::engine::{EngineEvent, Enqueue};
use vercelify_platform::{
    apps, databases, dbengine, deployments, env, git_sources, git_watch, hooks, nodes, previews, projects, tasks,
    volumes,
};

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
        .route("/platform/settings/standby", get(standby_get).put(standby_set))
        .route(
            "/platform/nodes/{id}",
            axum::routing::patch(node_patch).delete(node_remove),
        )
        .route("/platform/apps/{app}/instances", get(app_instances))
        .route("/platform/apps/{app}/previews/{pr}", delete(app_preview_remove))
        .route("/platform/apps/{app}/git", get(app_git).put(app_git_set))
        .route("/platform/apps/{app}/git/check", post(app_git_check))
        .route("/platform/apps/{app}/basic-auth", axum::routing::put(app_basic_auth))
        .route("/platform/apps/{app}/hooks", get(app_hooks).post(app_hook_create))
        .route("/platform/apps/{app}/hooks/{id}", delete(app_hook_delete))
        .route("/platform/apps/{app}/tasks", get(app_tasks).post(app_task_create))
        .route("/platform/apps/{app}/tasks/{id}", delete(app_task_delete))
        .route("/platform/apps/{app}/tasks/{id}/run", post(app_task_run))
        .route("/platform/git-sources", get(git_source_list).post(git_source_create))
        .route("/platform/git-sources/{id}", delete(git_source_delete))
        .route("/platform/settings/domain", get(domain_get).put(domain_set))
        .route("/platform/databases", get(db_list).post(db_create))
        .route(
            "/platform/databases/{db}",
            get(db_get).patch(db_patch).delete(db_delete),
        )
        .route("/platform/databases/{db}/link", post(db_link))
        .route("/platform/databases/{db}/move", post(db_move))
        .route("/platform/databases/{db}/backups", get(db_backups).post(db_backup_run))
        .route("/platform/databases/{db}/restore", post(db_restore))
        .route("/platform/databases/{db}/terminal", post(db_terminal))
        .route("/platform/apps/{app}/logs", get(app_runtime_logs))
        .route("/platform/apps/{app}/restart", post(app_restart))
        .route("/platform/apps/{app}/terminal", post(app_terminal))
        .route("/platform/terminals/{id}", delete(terminal_close))
        .route("/platform/terminals/{id}/output", get(terminal_output))
        .route("/platform/terminals/{id}/input", post(terminal_input))
        .route("/platform/terminals/{id}/resize", post(terminal_resize))
        .route("/platform/templates", get(template_list))
        .route("/platform/templates/{id}", post(template_create))
        .route("/platform/volumes", get(volume_list))
        .route("/platform/volumes/{id}/move", post(volume_move))
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
    // Databases' public ports are in the mesh maps (who may reach them).
    if topic == "databases" {
        if let Some(co) = &s.mesh {
            if co.store.tx(|c| vercelify_platform::mesh_registry::bump(c)).is_ok() {
                co.changed();
            }
        }
    }
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

#[derive(Deserialize, Default)]
struct DeleteQuery {
    /// Also delete the app's volumes (its data).
    #[serde(default)]
    purge_volumes: bool,
}

async fn app_delete(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<Value> {
    let (a, vols) = tx(&s, move |c, _| {
        let id = apps::find(c, &app)?.id;
        let vols = volumes::list(c, VolumeOwner::App, &id)?;
        Ok((apps::delete(c, &id)?, vols))
    })
    .await?;
    let p = platform(&s)?;
    let projects = s.projects.clone();
    let name = a.name.clone();
    // Containers, routes, images and logs go in the background.
    tokio::spawn(async move {
        p.engine.cleanup_app(&a, q.purge_volumes, &vols).await;
        projects.remove_app(&a.id).await;
    });
    changed(&s, "apps");
    Ok(Json(json!({ "deleted": name })))
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

/// A worker's heartbeat (`vercelify-agent` on it, every minute): over the
/// mesh, with its node token, from its own mesh address.
pub async fn node_heartbeat(
    State(s): State<AppState>,
    axum::extract::ConnectInfo(from): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    b: bytes::Bytes,
) -> ApiResult<NodeHeartbeatReply> {
    let hb: NodeHeartbeat = serde_json::from_slice(&b).map_err(|e| AppError::bad_request(e.to_string()))?;
    let co = crate::mesh::coordinator(&s)?;
    let node = co.authenticate(&headers, from.ip())?;
    if node.kind != vercelify_mesh::cert::NodeKind::Server {
        return Err(AppError::forbidden("only servers run apps"));
    }
    let mesh_ip = co.addressing.ipv4(node.ip_index).to_string();
    let main_ip = co.addressing.ipv4(vercelify_mesh::addr::COORDINATOR_INDEX).to_string();
    let first = tx(&s, {
        let id = node.id.clone();
        move |c, _| {
            Ok(
                c.query_row("SELECT count(*) FROM nodes WHERE mesh_node_id = ?1", [&id], |r| {
                    r.get::<_, i64>(0)
                })? == 0,
            )
        }
    })
    .await?;
    let n = tx(&s, move |c, k| {
        nodes::heartbeat(
            c,
            k,
            &nodes::Heartbeat {
                mesh_node_id: &node.id,
                name: &node.name,
                mesh_ip: &mesh_ip,
                agent_port: hb.agent_port,
                arch: &hb.arch,
                runtime_token: &hb.runtime_token,
                status: &hb.status,
            },
        )
    })
    .await?;
    if first {
        tracing::info!("{} joined the platform as a worker ({})", n.name, n.arch);
        changed(&s, "nodes");
    }
    Ok(Json(NodeHeartbeatReply {
        node_id: n.id,
        registry: format!("{main_ip}:{}", vercelify_platform::registry::PORT),
    }))
}

/// The main server's side of the fleet: its own condition every minute
/// (placement compares it with the workers'), and the image registry while
/// it has workers (its layers collected once a day).
pub async fn run_fleet(
    p: Arc<crate::platform::Platform>,
    cfg: Arc<crate::config::Config>,
    collector: Arc<vercelify_core::collect::Collector>,
) {
    let mut last_gc = std::time::Instant::now();
    loop {
        let status = crate::worker::node_status(&cfg, &collector).await;
        let ip = crate::mesh::own_ipv4(&cfg).map(|i| i.to_string()).unwrap_or_default();
        let workers = p
            .store
            .with(|c| {
                nodes::set_main_status(c, &ip, &status)?;
                Ok(nodes::list(c)?.iter().filter(|n| n.role == "worker").count())
            })
            .unwrap_or(0);
        if !ip.is_empty() {
            p.engine.set_registry(cfg.state_dir.join("registry"), vec![ip.clone()]);
        }
        if workers > 0 && !ip.is_empty() {
            let docker = &p.engine.deployer.docker;
            match vercelify_platform::registry::ensure(docker, &cfg.state_dir.join("registry"), &[ip]).await {
                Ok(()) if last_gc.elapsed() > std::time::Duration::from_secs(86_400) => {
                    last_gc = std::time::Instant::now();
                    if let Err(e) = vercelify_platform::registry::collect_garbage(docker).await {
                        tracing::warn!("{e}");
                    }
                }
                Ok(()) => {}
                Err(e) => tracing::warn!("image registry: {e}"),
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

async fn standby_get(State(s): State<AppState>) -> ApiResult<StandbyStatus> {
    Ok(Json(tx(&s, |c, _| vercelify_platform::standby::status(c)).await?))
}

/// Choose the standby (or none); the first copy goes right away.
async fn standby_set(State(s): State<AppState>, Json(r): Json<StandbyRequest>) -> ApiResult<StandbyStatus> {
    let p = platform(&s)?;
    let st = tx(&s, move |c, _| {
        vercelify_platform::standby::choose(c, r.node.as_deref())
    })
    .await?;
    if st.node_id.is_some() {
        let cfg = s.config.clone();
        let events = s.events.clone();
        tokio::spawn(async move {
            crate::standby::once(&p, &cfg).await;
            events.changed("nodes");
        });
    }
    changed(&s, "nodes");
    Ok(Json(st))
}

async fn node_patch(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(p): Json<NodePatch>,
) -> ApiResult<PlatformNode> {
    let n = tx(&s, move |c, _| nodes::patch(c, &id, &p)).await?;
    changed(&s, "nodes");
    Ok(Json(n))
}

async fn node_remove(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Value> {
    tx(&s, move |c, _| nodes::remove(c, &id)).await?;
    changed(&s, "nodes");
    Ok(Json(json!({ "removed": true })))
}

/// Where the live deployment's containers run, and their state there.
async fn app_instances(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<Vec<AppInstance>> {
    let p = platform(&s)?;
    let dep = tx(&s, move |c, _| Ok(apps::find(c, &app)?.current_deployment_id)).await?;
    let Some(dep) = dep else { return Ok(Json(vec![])) };
    let mut out = vec![];
    for pl in p.engine.placed(&dep)? {
        let state = match p.engine.node_runtime(&pl.node) {
            Ok(rt) => match rt.inspect(&pl.instance.container).await {
                Ok(Some(i)) if i.paused => "paused".to_string(),
                Ok(Some(i)) => i.status,
                Ok(None) => "missing".into(),
                Err(_) => "unknown".into(),
            },
            Err(_) => "unknown".into(),
        };
        out.push(AppInstance {
            node_id: pl.node.id.clone(),
            node: pl.node.name.clone(),
            container: pl.instance.container.clone(),
            state,
        });
    }
    Ok(Json(out))
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

// ----- previews, auto-deploy, protection, hooks, tasks ------------------------------

async fn app_preview_remove(State(s): State<AppState>, Path((app, pr)): Path<(String, u32)>) -> ApiResult<Value> {
    let p = platform(&s)?;
    let id = app_id(&s, app).await?;
    p.engine.remove_preview(&id, pr).await?;
    s.projects.remove(&crate::projects::key_for(&id, u64::from(pr))).await;
    Ok(Json(json!({ "removed": pr })))
}

async fn app_git(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<GitWatchStatus> {
    Ok(Json(
        tx(&s, move |c, _| {
            let id = apps::find(c, &app)?.id;
            git_watch::get(c, &id)
        })
        .await?,
    ))
}

#[derive(Deserialize)]
struct GitWatchSet {
    enabled: bool,
}

async fn app_git_set(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Json(req): Json<GitWatchSet>,
) -> ApiResult<GitWatchStatus> {
    let w = tx(&s, move |c, _| {
        let id = apps::find(c, &app)?.id;
        git_watch::set_enabled(c, &id, req.enabled)
    })
    .await?;
    changed(&s, "apps");
    Ok(Json(w))
}

async fn app_git_check(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<GitWatchStatus> {
    let git = s
        .platform_git
        .clone()
        .ok_or_else(|| AppError::unavailable("the platform engine is off"))?;
    let a = tx(&s, move |c, _| apps::find(c, &app)).await?;
    if a.source.repository.is_none() {
        return Err(AppError::bad_request(format!(
            "{} isn't deployed from a repository",
            a.name
        )));
    }
    git.check(&a).await.map(Json).map_err(AppError::unavailable)
}

async fn app_basic_auth(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Json(req): Json<BasicAuthRequest>,
) -> ApiResult<Value> {
    let on = !req.user.trim().is_empty();
    tx(&s, move |c, _| {
        let id = apps::find(c, &app)?.id;
        let cred = (!req.user.trim().is_empty()).then_some((req.user.as_str(), req.password.as_str()));
        apps::set_basic_auth(c, &id, cred)
    })
    .await?;
    // The routes change now, not at the next reconcile.
    let p = platform(&s)?;
    tokio::spawn(async move {
        if let Err(e) = p.reconciler.once().await {
            tracing::warn!("reconcile after a password change: {e}");
        }
    });
    changed(&s, "apps");
    Ok(Json(json!({ "protected": on })))
}

async fn app_hooks(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<Vec<DeployHook>> {
    Ok(Json(
        tx(&s, move |c, _| {
            let id = apps::find(c, &app)?.id;
            hooks::list(c, &id)
        })
        .await?,
    ))
}

async fn app_hook_create(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<DeployHook> {
    Ok(Json(
        tx(&s, move |c, _| {
            let id = apps::find(c, &app)?.id;
            hooks::create(c, &id)
        })
        .await?,
    ))
}

async fn app_hook_delete(State(s): State<AppState>, Path((app, id)): Path<(String, String)>) -> ApiResult<Value> {
    tx(&s, move |c, _| {
        let app_id = apps::find(c, &app)?.id;
        hooks::delete(c, &app_id, &id)
    })
    .await?;
    Ok(Json(json!({ "deleted": true })))
}

/// Calls per hook in the last minute (deploy hooks are public URLs).
static HOOK_CALLS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Vec<std::time::Instant>>>> =
    std::sync::LazyLock::new(Default::default);
pub const HOOK_CALLS_PER_MINUTE: usize = 6;

fn hook_rate_ok(hook: &str) -> bool {
    let mut m = HOOK_CALLS.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    let calls = m.entry(hook.to_string()).or_default();
    calls.retain(|t| now.duration_since(*t) < std::time::Duration::from_secs(60));
    if calls.len() >= HOOK_CALLS_PER_MINUTE {
        return false;
    }
    calls.push(now);
    true
}

/// `POST /v1/hooks/deploy/{secret}` (no token: the secret is the credential).
pub async fn deploy_hook(State(s): State<AppState>, Path(secret): Path<String>) -> ApiResult<Value> {
    let p = platform(&s).map_err(|_| AppError::not_found("no such hook"))?;
    let found = {
        let store = p.store.clone();
        tokio::task::spawn_blocking(move || store.tx(|c| hooks::resolve(c, &secret)))
            .await
            .map_err(|e| AppError::internal(e.to_string()))??
    };
    let Some((hook, app_id)) = found else {
        // The same answer for every unknown secret.
        return Err(AppError::not_found("no such hook"));
    };
    if !hook_rate_ok(&hook) {
        return Err(AppError {
            status: axum::http::StatusCode::TOO_MANY_REQUESTS,
            message: format!("at most {HOOK_CALLS_PER_MINUTE} deploys a minute per hook"),
        });
    }
    let d = engine_call(&s, move |e| {
        e.enqueue(
            &app_id,
            Enqueue {
                trigger: Some(DeploymentTrigger::Hook),
                ..Enqueue::default()
            },
        )
    })
    .await?;
    crate::tokens::audit(&s.config, &format!("hook:{hook}"), "POST", "/v1/hooks/deploy", 200);
    Ok(Json(
        json!({ "deployment": d.id, "number": d.number, "status": d.status }),
    ))
}

async fn app_tasks(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<Vec<ScheduledTask>> {
    Ok(Json(
        tx(&s, move |c, _| {
            let id = apps::find(c, &app)?.id;
            tasks::list(c, Some(&id))
        })
        .await?,
    ))
}

async fn app_task_create(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Json(req): Json<ScheduledTaskRequest>,
) -> ApiResult<ScheduledTask> {
    let t = tx(&s, move |c, _| {
        let id = apps::find(c, &app)?.id;
        tasks::create(c, &id, &req, |sch| crate::cron::parse(sch).map(|_| ()))
    })
    .await?;
    changed(&s, "apps");
    Ok(Json(t))
}

async fn app_task_delete(State(s): State<AppState>, Path((app, id)): Path<(String, String)>) -> ApiResult<Value> {
    tx(&s, move |c, _| {
        let app_id = apps::find(c, &app)?.id;
        tasks::delete(c, &app_id, &id)
    })
    .await?;
    changed(&s, "apps");
    Ok(Json(json!({ "deleted": true })))
}

async fn app_task_run(State(s): State<AppState>, Path((app, id)): Path<(String, String)>) -> ApiResult<Value> {
    let p = platform(&s)?;
    let t = tx(&s, move |c, _| {
        let app_id = apps::find(c, &app)?.id;
        tasks::get(c, &app_id, &id)
    })
    .await?;
    let status = run_task(&p, &t).await;
    Ok(Json(json!({ "status": status })))
}

/// Run a scheduled task in the app's running container; records the result.
pub async fn run_task(p: &crate::platform::Platform, t: &ScheduledTask) -> String {
    let container = p
        .store
        .with(|c| {
            let a = apps::get(c, &t.app_id)?;
            match a.current_deployment_id {
                Some(d) => deployments::container(c, &d),
                None => Ok(String::new()),
            }
        })
        .unwrap_or_default();
    let status = if container.is_empty() {
        "skipped: the app isn't deployed".to_string()
    } else {
        let cmd = vec!["sh".to_string(), "-c".to_string(), t.command.clone()];
        match p
            .engine
            .deployer
            .docker
            .exec(&container, &cmd, std::time::Duration::from_secs(15 * 60))
            .await
        {
            Ok(o) if o.success() => "ok".to_string(),
            Ok(o) => format!("exit {}: {}", o.code.unwrap_or(-1), o.tail(2)),
            Err(e) => format!("failed: {e}"),
        }
    };
    let (store, id, st) = (p.store.clone(), t.id.clone(), status.clone());
    let _ = tokio::task::spawn_blocking(move || store.with(|c| tasks::record_run(c, &id, &st))).await;
    status
}

/// The minute loop for scheduled tasks.
pub async fn run_tasks(p: Arc<crate::platform::Platform>, cfg: Arc<crate::config::Config>) {
    use chrono::Timelike;
    let mut last = None;
    loop {
        let now = chrono::Utc::now();
        tokio::time::sleep(std::time::Duration::from_secs(60 - u64::from(now.second()) + 1)).await;
        let now = chrono::Utc::now();
        let minute = now.timestamp() / 60;
        if last == Some(minute) {
            continue;
        }
        last = Some(minute);
        let due: Vec<ScheduledTask> = p
            .store
            .with(|c| tasks::list(c, None))
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.enabled && crate::cron::parse(&t.schedule).is_ok_and(|s| s.matches(now)))
            .collect();
        for t in due {
            let p = p.clone();
            tokio::spawn(async move {
                let st = run_task(&p, &t).await;
                if st != "ok" {
                    tracing::info!("scheduled task {} of {}: {st}", t.name, t.app_id);
                }
            });
        }
        let (p2, cfg2) = (p.clone(), cfg.clone());
        tokio::spawn(async move { run_database_backups(&p2, &cfg2, now).await });
    }
}

// ----- git sources and settings ----------------------------------------------------

async fn git_source_list(State(s): State<AppState>) -> ApiResult<Vec<GitSource>> {
    Ok(Json(tx(&s, |c, _| git_sources::list(c)).await?))
}

async fn git_source_create(State(s): State<AppState>, Json(req): Json<GitSourceRequest>) -> ApiResult<GitSource> {
    let pair = if req.auth == GitAuthKind::DeployKey {
        let tmp = s.config.state_dir.join("builds/.keys");
        Some(git_sources::generate_deploy_key(&s.config.ssh_keygen_bin, &tmp).await?)
    } else {
        None
    };
    Ok(Json(tx(&s, move |c, k| git_sources::create(c, k, &req, pair)).await?))
}

async fn git_source_delete(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Value> {
    tx(&s, move |c, _| git_sources::delete(c, &id)).await?;
    Ok(Json(json!({ "deleted": true })))
}

async fn domain_get(State(s): State<AppState>) -> ApiResult<PlatformDomain> {
    let d = tx(&s, |c, _| apps::domain_settings(c)).await?;
    Ok(Json(PlatformDomain {
        wildcard: d.wildcard,
        https: d.https,
        edge_tls: d.edge_tls,
    }))
}

/// The wildcard for new apps' and previews' names. Existing apps keep their
/// domains (change them per app), as with Coolify.
async fn domain_set(State(s): State<AppState>, Json(req): Json<PlatformDomain>) -> ApiResult<PlatformDomain> {
    let wildcard = match req.wildcard.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
        Some(w) => Some(vercelify_platform::validate::host(w.trim_start_matches("*."))?),
        None => None,
    };
    let settings = apps::DomainSettings {
        wildcard: wildcard.clone(),
        https: req.https && !req.edge_tls,
        edge_tls: req.edge_tls,
    };
    tx(&s, move |c, _| {
        vercelify_platform::store::put_setting(c, apps::DOMAIN_SETTING, &settings)
    })
    .await?;
    let p = platform(&s)?;
    // Previews' hosts follow the wildcard.
    tokio::spawn(async move {
        let _ = p.reconciler.once().await;
    });
    changed(&s, "apps");
    Ok(Json(PlatformDomain {
        wildcard,
        https: req.https && !req.edge_tls,
        edge_tls: req.edge_tls,
    }))
}

// ----- databases -----------------------------------------------------------------------

/// restic settings from `backup.env` (the installer's backup setup).
pub fn restic(cfg: &crate::config::Config) -> dbengine::Restic {
    dbengine::Restic {
        bin: "restic".into(),
        env: crate::config::env_file_values(&cfg.backup_env).into_iter().collect(),
    }
}

#[derive(Deserialize, Default)]
struct DbListQuery {
    project: Option<String>,
    #[serde(default)]
    reveal: bool,
}

async fn db_list(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Query(q): Query<DbListQuery>,
) -> ApiResult<Vec<Database>> {
    if q.reveal && actor.role < Role::Deployer {
        return Err(AppError {
            status: axum::http::StatusCode::FORBIDDEN,
            message: "a viewer token can't reveal passwords".into(),
        });
    }
    Ok(Json(
        tx(&s, move |c, k| databases::list(c, k, q.project.as_deref(), q.reveal)).await?,
    ))
}

async fn db_get(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(db): Path<String>,
    Query(q): Query<EnvQuery>,
) -> ApiResult<Database> {
    check_reveal(&actor, &q)?;
    Ok(Json(tx(&s, move |c, k| databases::get(c, k, &db, q.reveal)).await?))
}

/// Start (or restart) a database in the background; the API answers at once.
fn ensure_in_background(s: &AppState, id: String, title: String) {
    let Ok(p) = platform(s) else { return };
    let job = s.jobs.start("database", title, Some(&id));
    tokio::spawn(async move {
        match p.engine.ensure_database(&id).await {
            Ok(d) => job.done(format!(
                "{} is {}",
                d.name,
                if d.desired == Desired::Stopped {
                    "stopped"
                } else {
                    "running"
                }
            )),
            Err(e) => job.fail(e.to_string()),
        }
    });
}

async fn db_create(State(s): State<AppState>, Json(req): Json<DatabaseCreate>) -> ApiResult<Database> {
    let d = tx(&s, move |c, k| databases::create(c, k, &req)).await?;
    ensure_in_background(&s, d.id.clone(), format!("Start {}", d.name));
    changed(&s, "databases");
    Ok(Json(d))
}

async fn db_patch(
    State(s): State<AppState>,
    Path(db): Path<String>,
    Json(req): Json<DatabasePatch>,
) -> ApiResult<Database> {
    if let Some(sched) = req.backup_schedule.as_deref().filter(|x| !x.trim().is_empty()) {
        crate::cron::parse(sched.trim()).map_err(|e| AppError::bad_request(format!("backup schedule: {e}")))?;
    }
    let d = tx(&s, move |c, k| {
        let id = databases::get(c, k, &db, false)?.id;
        databases::patch(c, k, &id, &req)
    })
    .await?;
    ensure_in_background(&s, d.id.clone(), format!("Apply the settings of {}", d.name));
    changed(&s, "databases");
    Ok(Json(d))
}

#[derive(Deserialize, Default)]
struct PurgeQuery {
    /// Also delete its data (otherwise the volume stays, for a later restore).
    #[serde(default)]
    purge: bool,
}

async fn db_delete(State(s): State<AppState>, Path(db): Path<String>, Query(q): Query<PurgeQuery>) -> ApiResult<Value> {
    let (d, hdd) = tx(&s, move |c, k| {
        let d = databases::get(c, k, &db, false)?;
        let hdd = volumes::list(c, VolumeOwner::Database, &d.id)?
            .into_iter()
            .next()
            .and_then(|v| v.host_path);
        Ok((databases::delete(c, k, &d.id)?, hdd))
    })
    .await?;
    let p = platform(&s)?;
    let name = d.name.clone();
    tokio::spawn(async move {
        if let Err(e) = p.engine.remove_database(&d, q.purge, hdd).await {
            tracing::warn!("removing the database {}: {e}", d.name);
        }
    });
    changed(&s, "databases");
    Ok(Json(json!({ "deleted": name, "data_kept": !q.purge })))
}

#[derive(Deserialize)]
struct LinkRequest {
    app: String,
    /// Default: DATABASE_URL (REDIS_URL for Redis-likes…).
    var: Option<String>,
}

async fn db_link(State(s): State<AppState>, Path(db): Path<String>, Json(req): Json<LinkRequest>) -> ApiResult<Value> {
    let var = tx(&s, move |c, k| {
        let d = databases::get(c, k, &db, false)?;
        let var = req
            .var
            .clone()
            .unwrap_or_else(|| databases::template(d.kind).url_var.to_string());
        vercelify_platform::validate::env_key(&var)?;
        databases::link(c, k, &req.app, &d.name, &var)?;
        Ok(var)
    })
    .await?;
    changed(&s, "apps");
    Ok(Json(json!({ "variable": var, "note": "redeploy the app to use it" })))
}

#[derive(Deserialize)]
struct MoveRequest {
    tier: vercelify_core::StorageTier,
}

async fn db_move(
    State(s): State<AppState>,
    Path(db): Path<String>,
    Json(req): Json<MoveRequest>,
) -> ApiResult<vercelify_core::Job> {
    let p = platform(&s)?;
    let d = tx(&s, move |c, k| databases::get(c, k, &db, false)).await?;
    if req.tier == vercelify_core::StorageTier::Hdd
        && !crate::volumes::is_mount_point(std::path::Path::new(&s.config.storage_mount))
        && s.config.require_storage_mount
    {
        return Err(AppError::conflict("the storage disk isn't mounted"));
    }
    let job = s
        .jobs
        .start("move", format!("Move {} to the {:?}", d.name, req.tier), Some(&d.id));
    let snapshot = job.snapshot();
    let root = s.config.docker_volumes_root.clone();
    let events = s.events.clone();
    tokio::spawn(async move {
        match p.engine.move_database(&d.id, req.tier, &root).await {
            Ok(_) => job.done(format!("{} runs from the new disk", d.name)),
            Err(e) => job.fail(e.to_string()),
        }
        events.changed("databases");
    });
    snapshot.map(Json).ok_or_else(|| AppError::internal("job vanished"))
}

async fn db_backups(State(s): State<AppState>, Path(db): Path<String>) -> ApiResult<Vec<DatabaseBackup>> {
    Ok(Json(
        tx(&s, move |c, k| {
            let id = databases::get(c, k, &db, false)?.id;
            dbengine::backups(c, &id)
        })
        .await?,
    ))
}

async fn db_backup_run(State(s): State<AppState>, Path(db): Path<String>) -> ApiResult<vercelify_core::Job> {
    let p = platform(&s)?;
    let restic = restic(&s.config);
    if !restic.configured() {
        return Err(AppError::conflict(
            "backups aren't set up on this server: sudo vercelify backup setup",
        ));
    }
    let d = tx(&s, move |c, k| databases::get(c, k, &db, false)).await?;
    let job = s.jobs.start("backup", format!("Back up {}", d.name), Some(&d.id));
    let snapshot = job.snapshot();
    let events = s.events.clone();
    tokio::spawn(async move {
        match p.engine.backup_database(&d.id, &restic).await {
            Ok(b) => job.done(format!(
                "snapshot {} ({} bytes)",
                &b.snapshot[..b.snapshot.len().min(8)],
                b.bytes
            )),
            Err(e) => job.fail(e.to_string()),
        }
        events.changed("databases");
    });
    snapshot.map(Json).ok_or_else(|| AppError::internal("job vanished"))
}

#[derive(Deserialize)]
struct RestoreRequest {
    snapshot: String,
}

async fn db_restore(
    State(s): State<AppState>,
    Path(db): Path<String>,
    Json(req): Json<RestoreRequest>,
) -> ApiResult<vercelify_core::Job> {
    let p = platform(&s)?;
    let restic = restic(&s.config);
    let d = tx(&s, move |c, k| databases::get(c, k, &db, false)).await?;
    let job = s.jobs.start("restore", format!("Restore {}", d.name), Some(&d.id));
    let snapshot = job.snapshot();
    let events = s.events.clone();
    tokio::spawn(async move {
        match p.engine.restore_database(&d.id, &req.snapshot, &restic).await {
            Ok(()) => job.done(format!("{} restored from {}", d.name, req.snapshot)),
            Err(e) => job.fail(e.to_string()),
        }
        events.changed("databases");
    });
    snapshot.map(Json).ok_or_else(|| AppError::internal("job vanished"))
}

/// Scheduled backups of databases (their `backup_schedule`), from the minute loop.
pub async fn run_database_backups(
    p: &Arc<crate::platform::Platform>,
    cfg: &crate::config::Config,
    now: chrono::DateTime<chrono::Utc>,
) {
    let due: Vec<Database> = p
        .store
        .with(|c| databases::list(c, &p.key, None, false))
        .unwrap_or_default()
        .into_iter()
        .filter(|d| {
            !d.backup_schedule.is_empty() && crate::cron::parse(&d.backup_schedule).is_ok_and(|s| s.matches(now))
        })
        .collect();
    if due.is_empty() {
        return;
    }
    let restic = restic(cfg);
    for d in due {
        if let Err(e) = p.engine.backup_database(&d.id, &restic).await {
            tracing::warn!("scheduled backup of {}: {e}", d.name);
        }
    }
}

// ----- runtime: logs, restart, terminals, volumes ---------------------------------

#[derive(Deserialize, Default)]
struct RuntimeLogQuery {
    #[serde(default)]
    follow: bool,
    tail: Option<u32>,
}

/// The live container's output (`docker logs`) as JSON lines
/// `{at, stream, text}`; with `follow=true` it stays open for new lines.
/// From another server's container it comes through that server's agent.
async fn app_runtime_logs(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Query(q): Query<RuntimeLogQuery>,
) -> Result<axum::response::Response, AppError> {
    let p = platform(&s)?;
    let id = tx(&s, move |c, _| Ok(apps::find(c, &app)?.id)).await?;
    let (rt, container) = p.engine.live_instance(&id)?;
    let tail = q.tail.unwrap_or(200).clamp(1, 5000);
    match rt {
        vercelify_platform::runtime::NodeRuntime::Remote(r) => {
            let path = format!(
                "/v1/runtime/containers/{container}/logs?tail={tail}&follow={}",
                q.follow
            );
            passthrough(r.stream(&path).await?)
        }
        vercelify_platform::runtime::NodeRuntime::Local(_) => {
            docker_logs(&p.engine.deployer.docker.bin, &container, tail, q.follow)
        }
    }
}

/// A streamed answer from another server's agent, passed on as it is.
fn passthrough(resp: hyper::Response<hyper::body::Incoming>) -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;
    let headers = [
        (axum::http::header::CONTENT_TYPE, "application/x-ndjson"),
        (axum::http::header::CACHE_CONTROL, "no-store"),
    ];
    Ok((headers, axum::body::Body::new(resp.into_body())).into_response())
}

/// `docker logs` of a container as JSON lines `{at, stream, text}`.
pub(crate) fn docker_logs(
    docker_bin: &str,
    container: &str,
    tail: u32,
    follow: bool,
) -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;
    let tail = tail.clamp(1, 5000).to_string();
    let mut args = vec!["logs", "--timestamps", "--tail", &tail];
    if follow {
        args.push("--follow");
    }
    args.push(container);
    let mut child = tokio::process::Command::new(docker_bin)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AppError::internal(format!("docker logs: {e}")))?;
    let (out, err) = (child.stdout.take(), child.stderr.take());
    let (tx_, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(256);
    if let Some(o) = out {
        tokio::spawn(forward_lines(o, "stdout", tx_.clone()));
    }
    if let Some(e) = err {
        tokio::spawn(forward_lines(e, "stderr", tx_.clone()));
    }
    drop(tx_);
    // The process lives as long as the response: dropped (and killed) when
    // the reader goes away or both pipes end.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    let headers = [
        (axum::http::header::CONTENT_TYPE, "application/x-ndjson"),
        (axum::http::header::CACHE_CONTROL, "no-store"),
    ];
    Ok((headers, axum::body::Body::new(crate::routes::LineBody(rx))).into_response())
}

/// Each line of `pipe` as `{at, stream, text}` JSON until it ends or the
/// reader goes away.
async fn forward_lines<R: tokio::io::AsyncRead + Unpin>(
    pipe: R,
    stream: &'static str,
    tx_: tokio::sync::mpsc::Sender<bytes::Bytes>,
) {
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(pipe).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let (at, text) = split_timestamp(&line);
        let mut v = serde_json::to_vec(&json!({ "at": at, "stream": stream, "text": text })).unwrap_or_default();
        v.push(b'\n');
        if tx_.send(bytes::Bytes::from(v)).await.is_err() {
            return;
        }
    }
}

/// `2025-01-02T03:04:05.123456789Z text` → (`2025-01-02T03:04:05Z`, `text`).
pub fn split_timestamp(line: &str) -> (String, &str) {
    match line.split_once(' ') {
        Some((ts, rest)) if ts.len() >= 20 && ts.as_bytes()[4] == b'-' && ts.ends_with('Z') => {
            let secs = ts.split('.').next().unwrap_or(ts);
            let at = if secs.ends_with('Z') {
                secs.to_string()
            } else {
                format!("{secs}Z")
            };
            (at, rest)
        }
        _ => (String::new(), line),
    }
}

async fn app_restart(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<Value> {
    let p = platform(&s)?;
    let id = tx(&s, move |c, _| Ok(apps::find(c, &app)?.id)).await?;
    let n = p.engine.restart_app(&id).await?;
    changed(&s, "apps");
    Ok(Json(json!({ "restarted": n })))
}

fn open_terminal(
    s: &AppState,
    actor: &Actor,
    container: String,
    command: Option<Vec<String>>,
    req: &TerminalRequest,
) -> ApiResult<TerminalSession> {
    let p = platform(s)?;
    let argv = crate::terminal::exec_argv(&p.engine.deployer.docker.bin, &container, command.as_deref());
    let t = s
        .terminals
        .open(
            &actor.id,
            &container,
            &argv,
            req.cols.unwrap_or(100),
            req.rows.unwrap_or(30),
        )
        .map_err(AppError::conflict)?;
    Ok(Json(TerminalSession {
        id: t.id.clone(),
        target: t.target.clone(),
    }))
}

async fn app_terminal(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(app): Path<String>,
    body: Option<Json<TerminalRequest>>,
) -> ApiResult<TerminalSession> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let p = platform(&s)?;
    let id = tx(&s, move |c, _| Ok(apps::find(c, &app)?.id)).await?;
    let (rt, container) = p.engine.live_instance(&id)?;
    match rt {
        vercelify_platform::runtime::NodeRuntime::Remote(r) => {
            // Opened on that server; this one relays it under its own id,
            // owned by the token that opened it.
            let mut body = serde_json::to_value(&req).unwrap_or(json!({}));
            body["container"] = json!(container);
            let t: TerminalSession = r.post("/v1/runtime/terminals", &body).await?;
            let id = format!("term_r{}", vercelify_platform::secrets::random_token(20).to_lowercase());
            s.remote_terminals.lock().unwrap_or_else(|e| e.into_inner()).insert(
                id.clone(),
                RemoteTerminal {
                    owner: actor.id.clone(),
                    remote: r,
                    id: t.id,
                },
            );
            Ok(Json(TerminalSession { id, target: t.target }))
        }
        vercelify_platform::runtime::NodeRuntime::Local(_) => open_terminal(&s, &actor, container, None, &req),
    }
}

/// A terminal on another server, relayed.
#[derive(Clone)]
pub struct RemoteTerminal {
    owner: String,
    remote: vercelify_platform::runtime::Remote,
    /// Its id there.
    id: String,
}

fn remote_terminal(s: &AppState, actor: &Actor, id: &str) -> Option<RemoteTerminal> {
    s.remote_terminals
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .filter(|t| t.owner == actor.id)
        .cloned()
}

async fn db_terminal(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(db): Path<String>,
    body: Option<Json<TerminalRequest>>,
) -> ApiResult<TerminalSession> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let d = tx(&s, move |c, k| databases::get(c, k, &db, false)).await?;
    let command = req.console.unwrap_or(true).then(|| databases::console_command(d.kind));
    open_terminal(&s, &actor, d.container, command, &req)
}

fn session(s: &AppState, actor: &Actor, id: &str) -> Result<Arc<crate::terminal::Session>, AppError> {
    s.terminals
        .get(id, &actor.id)
        .ok_or_else(|| AppError::not_found(format!("no terminal {id} (it ended, or another token opened it)")))
}

/// The terminal's output as JSON lines: `{"data": "<base64>"}`, then
/// `{"exit": code}` when its shell ends. Starts with recent output.
pub(crate) async fn terminal_output(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
) -> Result<axum::response::Response, AppError> {
    use crate::terminal::Chunk;
    use axum::response::IntoResponse;
    use base64::Engine as _;
    if let Some(r) = remote_terminal(&s, &actor, &id) {
        return passthrough(
            r.remote
                .stream(&format!("/v1/runtime/terminals/{}/output", r.id))
                .await?,
        );
    }
    let t = session(&s, &actor, &id)?;
    let (backlog, mut live, exit, reader) = t.attach();
    let (tx_, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);
    let line = |v: Value| {
        let mut b = serde_json::to_vec(&v).unwrap_or_default();
        b.push(b'\n');
        bytes::Bytes::from(b)
    };
    let b64 = |d: &[u8]| base64::engine::general_purpose::STANDARD.encode(d);
    tokio::spawn(async move {
        let _reader = reader;
        if !backlog.is_empty() && tx_.send(line(json!({ "data": b64(&backlog) }))).await.is_err() {
            return;
        }
        if let Some(code) = exit {
            let _ = tx_.send(line(json!({ "exit": code }))).await;
            return;
        }
        loop {
            match live.recv().await {
                Ok(Chunk::Data(d)) => {
                    if tx_.send(line(json!({ "data": b64(&d) }))).await.is_err() {
                        return;
                    }
                }
                Ok(Chunk::Exit(code)) => {
                    let _ = tx_.send(line(json!({ "exit": code }))).await;
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            }
        }
    });
    let headers = [
        (axum::http::header::CONTENT_TYPE, "application/x-ndjson"),
        (axum::http::header::CACHE_CONTROL, "no-store"),
    ];
    Ok((headers, axum::body::Body::new(crate::routes::LineBody(rx))).into_response())
}

#[derive(Deserialize)]
pub(crate) struct TerminalInput {
    /// Base64 of the bytes typed.
    data: String,
}

pub(crate) async fn terminal_input(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(req): Json<TerminalInput>,
) -> ApiResult<Value> {
    use base64::Engine as _;
    if let Some(r) = remote_terminal(&s, &actor, &id) {
        let v: Value = r
            .remote
            .post(
                &format!("/v1/runtime/terminals/{}/input", r.id),
                &json!({ "data": req.data }),
            )
            .await?;
        return Ok(Json(v));
    }
    let t = session(&s, &actor, &id)?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(req.data.as_bytes())
        .map_err(|_| AppError::bad_request("data must be base64"))?;
    if data.len() > 64 * 1024 {
        return Err(AppError::bad_request("at most 64 KiB per write"));
    }
    tokio::task::spawn_blocking(move || t.write(&data))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .map_err(|e| AppError::conflict(format!("the terminal is closed: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub(crate) struct TerminalSize {
    cols: u16,
    rows: u16,
}

pub(crate) async fn terminal_resize(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(req): Json<TerminalSize>,
) -> ApiResult<Value> {
    if let Some(r) = remote_terminal(&s, &actor, &id) {
        let v: Value = r
            .remote
            .post(
                &format!("/v1/runtime/terminals/{}/resize", r.id),
                &json!({ "cols": req.cols, "rows": req.rows }),
            )
            .await?;
        return Ok(Json(v));
    }
    let t = session(&s, &actor, &id)?;
    t.resize(req.cols.min(1000), req.rows.min(500))
        .map_err(|e| AppError::conflict(e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

pub(crate) async fn terminal_close(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    if let Some(r) = remote_terminal(&s, &actor, &id) {
        s.remote_terminals.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        let _ = r.remote.delete(&format!("/v1/runtime/terminals/{}", r.id)).await;
        return Ok(Json(json!({ "closed": true })));
    }
    if !s.terminals.close(&id, &actor.id) {
        return Err(AppError::not_found(format!("no terminal {id}")));
    }
    Ok(Json(json!({ "closed": true })))
}

async fn template_list() -> Json<Vec<ServiceTemplate>> {
    Json(vercelify_platform::templates::list())
}

/// Create the app of a catalogue service and deploy it.
async fn template_create(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<TemplateCreate>>,
) -> ApiResult<App> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let p = platform(&s)?;
    let app = tx(&s, move |c, k| vercelify_platform::templates::create(c, k, &id, &req)).await?;
    p.engine.enqueue(
        &app.id,
        Enqueue {
            trigger: Some(DeploymentTrigger::Manual),
            ..Enqueue::default()
        },
    )?;
    changed(&s, "apps");
    Ok(Json(app))
}

async fn volume_list(State(s): State<AppState>) -> ApiResult<Vec<VolumeInfo>> {
    Ok(Json(
        tx(&s, |c, k| {
            let mut out = vec![];
            for v in volumes::all(c)? {
                let owner_name = match v.owner_kind {
                    VolumeOwner::App => apps::get(c, &v.owner_id).map(|a| a.name),
                    VolumeOwner::Database => databases::get(c, k, &v.owner_id, false).map(|d| d.name),
                }
                .unwrap_or_default();
                out.push(VolumeInfo { volume: v, owner_name });
            }
            Ok(out)
        })
        .await?,
    ))
}

/// Move a volume to the other disk. Database volumes move with their
/// database (stop, copy, start); app volumes recreate the app's container.
async fn volume_move(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<MoveRequest>,
) -> ApiResult<vercelify_core::Job> {
    let p = platform(&s)?;
    let vid = id.clone();
    let (vol, owner) = tx(&s, move |c, k| {
        let v = volumes::get(c, &vid)?;
        let name = match v.owner_kind {
            VolumeOwner::App => apps::get(c, &v.owner_id)?.name,
            VolumeOwner::Database => databases::get(c, k, &v.owner_id, false)?.name,
        };
        Ok((v, name))
    })
    .await?;
    if req.tier == vercelify_core::StorageTier::Hdd
        && !crate::volumes::is_mount_point(std::path::Path::new(&s.config.storage_mount))
        && s.config.require_storage_mount
    {
        return Err(AppError::conflict("the storage disk isn't mounted"));
    }
    let tier = if req.tier == vercelify_core::StorageTier::Hdd {
        "HDD"
    } else {
        "SSD"
    };
    let job = s.jobs.start(
        "volume-move",
        format!("Move {owner}/{} to the {tier}", vol.name),
        Some(&vol.id),
    );
    let snapshot = job.snapshot();
    let root = s.config.docker_volumes_root.clone();
    let events = s.events.clone();
    tokio::spawn(async move {
        let res = match vol.owner_kind {
            VolumeOwner::Database => p.engine.move_database(&vol.owner_id, req.tier, &root).await.map(|_| ()),
            VolumeOwner::App => p.engine.move_app_volume(&vol.id, req.tier, &root).await.map(|_| ()),
        };
        match res {
            Ok(()) => job.done(format!("{owner}/{} is on the {tier} now", vol.name)),
            Err(e) => job.fail(e.to_string()),
        }
        events.changed("volumes");
        events.changed("apps");
    });
    snapshot.map(Json).ok_or_else(|| AppError::internal("job vanished"))
}

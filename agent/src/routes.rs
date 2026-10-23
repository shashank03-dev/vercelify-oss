use crate::autodeploy::AutoDeploy;
use crate::config::Config;
use crate::display::{self, Receiver};
use crate::error::{ApiResult, AppError};
use crate::llm::{self, Llm};
use crate::pairing::{self, Claims};
use crate::tokens::Actor;
use crate::{auth, coolify, docker, doctor, services, ssh, tailscale, update, volumes};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{middleware, Extension, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use vercelify_core::collect::{Collector, SysPaths};
use vercelify_core::*;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub collector: Arc<Collector>,
    pub receiver: Receiver,
    pub autodeploy: Arc<AutoDeploy>,
    pub llm: Arc<Llm>,
    pub claims: Arc<Claims>,
    pub doctor: Arc<doctor::DoctorState>,
    pub watcher: Arc<crate::watch::Watcher>,
    pub tokens: Arc<crate::tokens::Tokens>,
    pub events: Arc<crate::events::Events>,
    pub jobs: Arc<crate::events::Jobs>,
    pub sleep: Arc<crate::sleep::Sleep>,
    pub projects: Arc<crate::projects::Projects>,
}

impl AppState {
    pub fn new(config: Config, token: String) -> Self {
        let collector = Collector::new(
            SysPaths {
                sys_class: config.sys_class.clone(),
                sys_block: config.sys_block.clone(),
                storage_mount: config.storage_mount.clone(),
                hw: vercelify_core::hardware::HwPaths {
                    power_supply: config.sys_class.join("power_supply"),
                    ..Default::default()
                },
            },
            env!("CARGO_PKG_VERSION"),
        );
        let config = Arc::new(config);
        let events = Arc::new(crate::events::Events::default());
        let jobs = Arc::new(crate::events::Jobs::load(&config, events.clone()));
        let watcher = Arc::new(crate::watch::Watcher::load(config.clone(), events.clone()));
        let autodeploy = Arc::new(AutoDeploy::load(config.clone()));
        autodeploy.set_watcher(watcher.clone());
        autodeploy.set_jobs(jobs.clone());
        let projects = Arc::new(crate::projects::Projects::load(
            config.clone(),
            events.clone(),
            jobs.clone(),
        ));
        projects.set_watcher(watcher.clone());
        autodeploy.set_projects(projects.clone());
        let llm = Arc::new(Llm::new(config.clone()));
        llm.set_jobs(jobs.clone());
        Self {
            tokens: Arc::new(crate::tokens::Tokens::new(token.clone())),
            autodeploy,
            llm,
            sleep: Arc::new(crate::sleep::Sleep::load(config.clone(), events.clone(), jobs.clone())),
            projects,
            events,
            jobs,
            claims: Arc::new(Claims::default()),
            doctor: Arc::new(doctor::DoctorState::default()),
            watcher,
            config,
            collector: Arc::new(collector),
            receiver: Receiver::default(),
        }
    }
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/system", get(system))
        .route("/tailscale", get(tailscale_status))
        .route("/containers", get(containers))
        .route("/containers/{id}/logs", get(container_logs))
        .route("/containers/{id}/{action}", post(container_action))
        .route("/volumes", get(list_volumes).post(create_volume))
        .route("/display", get(display_status))
        .route("/display/receiver/start", post(receiver_start))
        .route("/display/receiver/stop", post(receiver_stop))
        .route("/coolify/volumes", get(coolify_volumes))
        .route("/coolify/volumes/{id}/move", post(coolify_move_volume))
        .route("/services", get(list_services))
        .route("/services/{name}/{action}", post(service_action))
        .route("/autodeploy", get(autodeploy_status).post(autodeploy_upsert))
        .route("/autodeploy/{app}", delete(autodeploy_remove))
        .route("/autodeploy/{app}/check", post(autodeploy_check))
        .route("/llm", get(llm_status))
        .route("/llm/install", post(llm_install))
        .route("/llm/models/pull", post(llm_pull))
        .route("/llm/models/delete", post(llm_delete_model))
        .route("/llm/{action}", post(llm_action))
        .route("/coolify/worker-key", get(coolify_worker_key))
        .route("/ssh/authorized-keys", post(authorize_key))
        .route("/doctor", get(doctor_report))
        .route("/tokens", get(tokens_list).post(tokens_create))
        .route("/tokens/{id}", delete(tokens_revoke))
        .route("/audit", get(audit_log))
        .route("/alerts", get(alerts_status))
        .route("/alerts/notify", post(alerts_notify))
        .route("/alerts/test", post(alerts_test))
        .route("/alerts/check", post(alerts_check))
        .route("/metrics", get(metrics))
        .route("/uptime", get(uptime))
        .route("/images/{repository}", get(app_images))
        .route("/backup", get(backup_status))
        .route("/backup/run", post(backup_run))
        .route("/update", get(update_status).post(update_start))
        .route("/jobs", get(jobs_list))
        .route("/jobs/{id}", get(job_get))
        .route("/sleep", get(sleep_status))
        .route("/sleep/proxy", post(sleep_proxy))
        .route("/sleep/{app}", put(sleep_set))
        .route(
            "/power-guard",
            get(power_guard_status).put(power_guard_set).delete(power_guard_reset),
        )
        .route("/domain", get(domain_info))
        .route("/projects", get(projects_status))
        .route("/projects/check", post(projects_check))
        .route("/projects/{app}", put(projects_upload).delete(projects_remove))
        .route("/projects/{app}/sync", post(projects_sync))
        .route("/projects/{app}/crons/run", post(projects_cron_run))
        .layer(timeout(Duration::from_secs(120)));
    // Kept open: the live event stream.
    let stream = Router::new().route("/events", get(events_stream));
    // Chats wait for a model to load (slow from an HDD) before the first byte.
    let long = Router::new()
        .route("/llm/chat", post(llm_chat))
        .layer(timeout(llm::CHAT_TIMEOUT + Duration::from_secs(30)));
    let api = api
        .merge(long)
        .merge(stream)
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_token));

    let public = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/pair/claim", post(pair_claim))
        .layer(timeout(Duration::from_secs(30)));

    let mut app = public.nest("/v1", api).layer(TraceLayer::new_for_http());

    if !state.config.cors_origins.is_empty() {
        let origins: Vec<HeaderValue> = state
            .config
            .cors_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        app = app.layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
                .allow_headers([axum::http::header::AUTHORIZATION, axum::http::header::CONTENT_TYPE]),
        );
    }
    app.with_state(state)
}

fn timeout(d: Duration) -> TimeoutLayer {
    TimeoutLayer::with_status_code(axum::http::StatusCode::GATEWAY_TIMEOUT, d)
}

async fn health(State(s): State<AppState>) -> Json<Health> {
    Json(Health {
        ok: true,
        version: env!("CARGO_PKG_VERSION").into(),
        hostname: s.collector.host().hostname,
        role: s.config.role(),
        pairing_open: pairing::is_open(&s.config),
        api_version: API_VERSION,
        capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
    })
}

/// Bump on breaking changes to existing endpoints.
pub const API_VERSION: u32 = 2;
/// Features newer than the first release, so cockpits can adapt per server.
pub const CAPABILITIES: &[&str] = &[
    "doctor",
    "update",
    "rollback",
    "hardware",
    "alerts",
    "metrics",
    "uptime",
    "tokens",
    "audit",
    "backup",
    "previews",
    "auto-rollback",
    "images",
    "events",
    "jobs",
    "sleep",
    "power-guard",
    "domain",
    "projects",
];

async fn tokens_list(State(s): State<AppState>, Extension(actor): Extension<Actor>) -> Json<Vec<DeviceToken>> {
    Json(s.tokens.list(&s.config, &actor))
}

async fn tokens_create(
    State(s): State<AppState>,
    Json(req): Json<DeviceTokenRequest>,
) -> ApiResult<DeviceTokenCreated> {
    let cfg = s.config.clone();
    let (token, secret) = tokio::task::spawn_blocking(move || crate::tokens::create(&cfg, &req.name))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .map_err(|e| AppError::internal(format!("writing the token file: {e}")))?;
    Ok(Json(DeviceTokenCreated { token, secret }))
}

async fn tokens_revoke(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Value> {
    let cfg = s.config.clone();
    let removed = tokio::task::spawn_blocking(move || crate::tokens::revoke(&cfg, &id))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .map_err(|e| AppError::internal(e.to_string()))?;
    if removed {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(AppError::not_found("no such token"))
    }
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
    minutes: Option<usize>,
}

async fn audit_log(State(s): State<AppState>, Query(q): Query<LimitQuery>) -> Json<Vec<AuditEntry>> {
    Json(crate::tokens::audit_tail(&s.config, q.limit.unwrap_or(100).min(1000)))
}

async fn alerts_status(State(s): State<AppState>) -> Json<AlertsStatus> {
    Json(s.watcher.status().await)
}

async fn alerts_notify(State(s): State<AppState>, Json(req): Json<NotifyConfigRequest>) -> ApiResult<AlertsStatus> {
    s.watcher
        .set_notify_url(&req.url)
        .await
        .map_err(AppError::bad_request)?;
    Ok(Json(s.watcher.status().await))
}

async fn alerts_test(State(s): State<AppState>) -> ApiResult<Alert> {
    let a = s
        .watcher
        .raise(crate::watch::Change {
            source: format!("test:{}", chrono::Utc::now().timestamp()),
            status: CheckStatus::Ok,
            title: "Test notification".into(),
            detail: "Alerts from this server reach you.".into(),
            resolved: true,
        })
        .await;
    match a.notified {
        Some(false) => Err(AppError::unavailable(
            "the notification could not be delivered — check the URL",
        )),
        None => Err(AppError::conflict("no notification URL set")),
        Some(true) => Ok(Json(a)),
    }
}

/// Run one pass of the background loop now (doctor, uptime, repairs).
async fn alerts_check(State(s): State<AppState>) -> Json<AlertsStatus> {
    s.watcher.tick(&s).await;
    Json(s.watcher.status().await)
}

async fn metrics(State(s): State<AppState>, Query(q): Query<LimitQuery>) -> Json<MetricsResponse> {
    Json(MetricsResponse {
        interval_secs: s.config.watch_interval_secs,
        points: s.watcher.metrics(q.minutes.unwrap_or(60).clamp(1, 24 * 60)).await,
    })
}

async fn uptime(State(s): State<AppState>) -> Json<Vec<UptimeCheck>> {
    Json(s.watcher.uptime().await)
}

/// Image tags kept for an app (Coolify names images after the app uuid).
async fn app_images(State(s): State<AppState>, Path(repository): Path<String>) -> ApiResult<AppImages> {
    if !crate::autodeploy::valid_app_uuid(&repository) {
        return Err(AppError::bad_request("invalid repository name"));
    }
    let out = crate::exec::run(
        &s.config.docker_bin,
        &["images", "--format", "{{.Tag}}", &repository],
        Duration::from_secs(20),
    )
    .await?;
    if !out.success() {
        return Err(AppError::unavailable(format!(
            "docker images failed: {}",
            out.stderr.trim()
        )));
    }
    Ok(Json(AppImages {
        repository,
        tags: out
            .stdout
            .lines()
            .map(str::trim)
            .filter(|t| !t.is_empty() && *t != "<none>")
            .map(String::from)
            .collect(),
    }))
}

async fn backup_status(State(s): State<AppState>) -> Json<BackupStatus> {
    Json(crate::update::backup_status(&s.config).await)
}

async fn backup_run(State(s): State<AppState>) -> ApiResult<BackupStatus> {
    let st = crate::update::backup_start(&s.config).await?;
    if s.jobs.running("backup", None).is_none() {
        let id = s.jobs.start("backup", "Backup", None).detach();
        tokio::spawn(crate::update::follow_unit(
            s.config.clone(),
            crate::update::BACKUP_UNIT,
            id,
            s.jobs.clone(),
            crate::update::backup_result,
        ));
    }
    Ok(Json(st))
}

async fn pair_claim(State(s): State<AppState>, Json(req): Json<PairClaimRequest>) -> ApiResult<PairClaimResponse> {
    let pairing = s.claims.claim(&s.config, &req.code).await?;
    Ok(Json(PairClaimResponse { pairing }))
}

fn read_power_guard(path: &std::path::Path) -> Option<PowerGuardState> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

async fn system(State(s): State<AppState>) -> ApiResult<SystemSnapshot> {
    let collector = s.collector.clone();
    let pg_path = s.config.power_guard_state.clone();
    let snap = tokio::task::spawn_blocking(move || collector.snapshot(read_power_guard(&pg_path)))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(snap))
}

async fn tailscale_status(State(s): State<AppState>) -> Json<TailscaleStatus> {
    Json(tailscale::status(s.config.tailscale_bin.as_deref()).await)
}

#[derive(Deserialize)]
struct ContainersQuery {
    #[serde(default)]
    stats: bool,
}

async fn containers(State(s): State<AppState>, Query(q): Query<ContainersQuery>) -> ApiResult<Vec<ContainerInfo>> {
    Ok(Json(docker::list(&s.config.docker_bin, q.stats).await?))
}

#[derive(Deserialize)]
struct LogsQuery {
    tail: Option<u32>,
}

async fn container_logs(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<LogsQuery>,
) -> ApiResult<Value> {
    let lines = docker::logs(&s.config.docker_bin, &id, q.tail.unwrap_or(300)).await?;
    Ok(Json(json!({ "lines": lines })))
}

async fn container_action(State(s): State<AppState>, Path((id, action)): Path<(String, String)>) -> ApiResult<Value> {
    docker::action(&s.config.docker_bin, &id, &action).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_volumes(State(s): State<AppState>) -> ApiResult<Vec<VolumeEntry>> {
    Ok(Json(volumes::list(&s.config).await?))
}

async fn create_volume(State(s): State<AppState>, Json(req): Json<VolumeRequest>) -> ApiResult<VolumeResult> {
    Ok(Json(volumes::create(&s.config, req).await?))
}

async fn build_display_status(s: &AppState) -> DisplayStatus {
    let (receiver, receiver_config) = s.receiver.status().await;
    let host = tailscale::ipv4(s.config.tailscale_bin.as_deref()).await;
    let receiver_url = match (&receiver_config, host) {
        (Some(c), Some(h)) => Some(display::sender_url(c, &h)),
        _ => None,
    };
    DisplayStatus {
        receiver,
        receiver_config,
        receiver_url,
        mpv_installed: crate::exec::find_on_path(&s.config.mpv_bin).is_some(),
        sunshine: services::status(&s.config.systemctl_bin, "sunshine").await,
    }
}

async fn display_status(State(s): State<AppState>) -> Json<DisplayStatus> {
    Json(build_display_status(&s).await)
}

async fn receiver_start(State(s): State<AppState>, Json(cfg): Json<ReceiverConfig>) -> ApiResult<DisplayStatus> {
    s.receiver.start(&s.config, cfg).await.map_err(AppError::unavailable)?;
    // Give mpv a moment so an immediate failure (bad args, DRM busy) is reported.
    tokio::time::sleep(Duration::from_millis(400)).await;
    Ok(Json(build_display_status(&s).await))
}

async fn receiver_stop(State(s): State<AppState>) -> Json<DisplayStatus> {
    s.receiver.stop().await;
    Json(build_display_status(&s).await)
}

async fn list_services(State(s): State<AppState>) -> Json<Vec<ServiceStatus>> {
    let mut out = Vec::new();
    for unit in &s.config.services {
        out.push(services::status(&s.config.systemctl_bin, unit).await);
    }
    Json(out)
}

async fn service_action(State(s): State<AppState>, Path((name, action)): Path<(String, String)>) -> ApiResult<Value> {
    services::control(&s.config.systemctl_bin, &s.config.services, &name, &action).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn coolify_volumes(State(s): State<AppState>) -> ApiResult<Vec<CoolifyVolume>> {
    Ok(Json(coolify::list(&s.config).await?))
}

#[derive(Deserialize)]
struct BackgroundQuery {
    #[serde(default)]
    background: bool,
}

/// `?background=true`: answer with the job right away (large volumes on an
/// HDD take longer than a request may).
async fn coolify_move_volume(
    State(s): State<AppState>,
    Path(id): Path<u64>,
    Query(q): Query<BackgroundQuery>,
    Json(req): Json<MoveVolumeRequest>,
) -> Result<Response, AppError> {
    if !q.background {
        return Ok(Json(coolify::move_volume(&s.config, id, req.tier).await?).into_response());
    }
    let target = id.to_string();
    if s.jobs.running("volume-move", Some(&target)).is_some() {
        return Err(AppError::conflict("this volume is already being moved"));
    }
    let tier = req.tier;
    let job = s.jobs.start(
        "volume-move",
        format!(
            "Move volume {id} to the {}",
            if tier == StorageTier::Hdd { "HDD" } else { "SSD" }
        ),
        Some(&target),
    );
    job.progress("stopping the app and copying", None);
    let snapshot = job.snapshot();
    let cfg = s.config.clone();
    let events = s.events.clone();
    tokio::spawn(async move {
        let r = coolify::move_volume(&cfg, id, tier).await;
        job.finish(r.map(|m| m.note).map_err(|e| e.message));
        events.changed("volumes");
    });
    Ok((axum::http::StatusCode::ACCEPTED, Json(snapshot)).into_response())
}

async fn autodeploy_status(State(s): State<AppState>) -> Json<AutoDeployStatus> {
    Json(s.autodeploy.status().await)
}

async fn autodeploy_upsert(
    State(s): State<AppState>,
    Json(req): Json<AutoDeployRequest>,
) -> ApiResult<AutoDeployWatch> {
    Ok(Json(s.autodeploy.upsert(req).await?))
}

async fn autodeploy_remove(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<Value> {
    s.autodeploy.remove(&app).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn autodeploy_check(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<AutoDeployWatch> {
    Ok(Json(s.autodeploy.check_now(&app).await?))
}

async fn llm_status(State(s): State<AppState>) -> Json<LlmStatus> {
    Json(s.llm.status().await)
}

async fn llm_install(State(s): State<AppState>, Json(req): Json<LlmInstallRequest>) -> ApiResult<LlmStatus> {
    s.llm.install(req.tier).await?;
    Ok(Json(s.llm.status().await))
}

async fn llm_action(State(s): State<AppState>, Path(action): Path<String>) -> ApiResult<LlmStatus> {
    s.llm.container_action(&action).await?;
    Ok(Json(s.llm.status().await))
}

async fn llm_pull(State(s): State<AppState>, Json(req): Json<LlmModelRequest>) -> ApiResult<LlmStatus> {
    s.llm.pull(req.model.trim()).await?;
    Ok(Json(s.llm.status().await))
}

async fn llm_delete_model(State(s): State<AppState>, Json(req): Json<LlmModelRequest>) -> ApiResult<Value> {
    s.llm.delete_model(req.model.trim()).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn llm_chat(State(s): State<AppState>, Json(req): Json<LlmChatRequest>) -> Result<Response, AppError> {
    let streaming = req.stream.unwrap_or(false);
    let resp = s.llm.chat(req).await?;
    let content_type = if streaming {
        "application/x-ndjson"
    } else {
        "application/json"
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
        ],
        axum::body::Body::new(resp.into_body()),
    )
        .into_response())
}

async fn coolify_worker_key(State(s): State<AppState>) -> ApiResult<CoolifyWorkerKey> {
    if s.config.role() == "worker" {
        return Err(AppError::conflict(
            "this machine is a worker; ask the main host for its key",
        ));
    }
    Ok(Json(coolify::worker_key(&s.config).await?))
}

async fn authorize_key(
    State(s): State<AppState>,
    Json(req): Json<AuthorizeKeyRequest>,
) -> ApiResult<AuthorizeKeyResult> {
    let file = s.config.authorized_keys_file.clone();
    let added = tokio::task::spawn_blocking(move || ssh::authorize(&file, &req.public_key))
        .await
        .map_err(|e| AppError::internal(e.to_string()))??;
    Ok(Json(AuthorizeKeyResult { added }))
}

#[derive(Deserialize)]
struct DoctorQuery {
    #[serde(default)]
    refresh: bool,
}

async fn doctor_report(State(s): State<AppState>, Query(q): Query<DoctorQuery>) -> Json<DoctorReport> {
    if q.refresh {
        doctor::invalidate(&s);
    }
    Json(doctor::report(&s).await)
}

async fn update_status(State(s): State<AppState>) -> Json<UpdateStatus> {
    Json(update::status(&s.config).await)
}

async fn update_start(State(s): State<AppState>) -> ApiResult<UpdateStatus> {
    let st = update::start(&s.config).await?;
    let id = s
        .jobs
        .start("update", format!("Update from {}", env!("CARGO_PKG_VERSION")), None)
        .detach();
    tokio::spawn(update::follow_unit(
        s.config.clone(),
        update::UNIT,
        id,
        s.jobs.clone(),
        update::update_result,
    ));
    Ok(Json(st))
}

async fn jobs_list(State(s): State<AppState>) -> Json<Vec<Job>> {
    Json(s.jobs.list())
}

async fn job_get(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Job> {
    s.jobs
        .get(&id)
        .map(Json)
        .ok_or_else(|| AppError::not_found("no such job"))
}

/// Newline-delimited JSON: a `hello`, then every alert, job change and
/// "fetch again" hint as it happens, and a `ping` every 20 s.
async fn events_stream(State(s): State<AppState>) -> Response {
    use tokio::sync::broadcast::error::RecvError;
    let (tx, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);
    let mut sub = s.events.subscribe();
    let events = s.events.clone();
    let line = |e: &AgentEvent| {
        let mut v = serde_json::to_vec(e).unwrap_or_default();
        v.push(b'\n');
        bytes::Bytes::from(v)
    };
    tokio::spawn(async move {
        if tx.send(line(&events.event("hello"))).await.is_err() {
            return;
        }
        loop {
            let e = tokio::select! {
                r = sub.recv() => match r {
                    Ok(e) => e,
                    // Too slow a reader: tell it to fetch everything again.
                    Err(RecvError::Lagged(_)) => {
                        let mut e = events.event("changed");
                        e.topic = Some("all".into());
                        e
                    }
                    Err(RecvError::Closed) => return,
                },
                _ = tokio::time::sleep(Duration::from_secs(20)) => events.event("ping"),
            };
            if tx.send(line(&e)).await.is_err() {
                return; // the cockpit went away
            }
        }
    });
    (
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        axum::body::Body::new(LineBody(rx)),
    )
        .into_response()
}

/// A response body fed line by line from a channel.
struct LineBody(tokio::sync::mpsc::Receiver<bytes::Bytes>);

impl http_body::Body for LineBody {
    type Data = bytes::Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        self.0.poll_recv(cx).map(|o| o.map(|b| Ok(http_body::Frame::data(b))))
    }
}

fn upload_files(req: ProjectUpload) -> Vec<(String, String)> {
    req.files.into_iter().map(|f| (f.name, f.content)).collect()
}

async fn projects_status(State(s): State<AppState>) -> Json<ProjectsStatus> {
    Json(s.projects.status().await)
}

/// What a configuration would do, without applying it (`vfy config check`).
async fn projects_check(State(s): State<AppState>, Json(req): Json<ProjectUpload>) -> ApiResult<ProjectSummary> {
    let files = crate::projects::check_upload(&upload_files(req)).map_err(AppError::bad_request)?;
    let refs: Vec<(&str, &str)> = files.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
    let mut plan = crate::project::parse(&refs).map_err(AppError::bad_request)?;
    if !s.config.rewrite_private_targets {
        crate::project::drop_private_rewrites(&mut plan);
    }
    Ok(Json(crate::projects::summary(&plan)))
}

async fn projects_upload(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Json(req): Json<ProjectUpload>,
) -> ApiResult<ProjectInfo> {
    Ok(Json(s.projects.upload(&app, upload_files(req)).await?))
}

async fn projects_remove(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<Value> {
    if !s.projects.remove_app(&app).await {
        return Err(AppError::not_found("this app has no project configuration"));
    }
    Ok(Json(json!({ "removed": true })))
}

/// Read vercel.json from the app's repository now (GitHub), at the branch
/// auto-deploy follows or the one Coolify builds.
async fn projects_sync(State(s): State<AppState>, Path(app): Path<String>) -> ApiResult<ProjectInfo> {
    if !crate::autodeploy::valid_app_uuid(&app) {
        return Err(AppError::bad_request("invalid app uuid"));
    }
    let coolify = s.autodeploy.coolify_app(&app).await;
    let field = |k: &str| {
        coolify
            .as_ref()
            .and_then(|a| a.get(k)?.as_str().map(String::from))
            .unwrap_or_default()
    };
    let (repo, branch, token) = match s.autodeploy.source_of(&app).await {
        Some(src) => src,
        None => (field("git_repository"), field("git_branch"), None),
    };
    let slug = crate::github::repo_slug(&repo)
        .or_else(|| crate::github::repo_slug(&format!("https://github.com/{}", repo.trim_matches('/'))))
        .ok_or_else(|| AppError::conflict("only GitHub repositories can be read; upload the file instead"))?;
    // No branch known: the repository's default branch.
    if !branch.is_empty() && !crate::autodeploy::valid_branch(&branch) {
        return Err(AppError::bad_request("invalid branch"));
    }
    let on = if branch.is_empty() {
        "default branch"
    } else {
        branch.as_str()
    };
    let files = crate::projects::fetch_github(&s.config, &slug, &field("base_directory"), &branch, token.as_deref())
        .await
        .map_err(AppError::unavailable)?;
    if files.is_empty() {
        return Err(AppError::not_found(format!(
            "no vercel.json or vercelify.json on {on} of {slug}"
        )));
    }
    s.projects.set_from_repo(&app, 0, files, on).await;
    s.projects
        .get(&app)
        .await
        .filter(|p| p.source == "repo")
        .map(Json)
        .ok_or_else(|| AppError::conflict("an uploaded configuration is in use; remove it to follow the repository"))
}

async fn projects_cron_run(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Json(req): Json<CronRunRequest>,
) -> ApiResult<Job> {
    Ok(Json(s.projects.run_now(&app, &req.path).await?))
}

async fn sleep_status(State(s): State<AppState>) -> Json<SleepStatus> {
    Json(s.sleep.status().await)
}

async fn sleep_set(
    State(s): State<AppState>,
    Path(app): Path<String>,
    Json(req): Json<SleepRequest>,
) -> ApiResult<SleepStatus> {
    let enabling = req.enabled;
    s.sleep.set(&app, req).await?;
    if enabling {
        // Requests must be counted before anything can be called idle.
        if let Err(e) = s.sleep.ensure_proxy_metrics().await {
            tracing::warn!("sleep: proxy request counters: {}", e.message);
        }
    }
    s.sleep.tick().await;
    Ok(Json(s.sleep.status().await))
}

async fn sleep_proxy(State(s): State<AppState>) -> ApiResult<ProxyMetricsResult> {
    Ok(Json(s.sleep.ensure_proxy_metrics().await?))
}

async fn power_guard_status(State(s): State<AppState>) -> Json<PowerGuardStatus> {
    Json(crate::powerguard::status(&s.config).await)
}

async fn power_guard_set(
    State(s): State<AppState>,
    Json(req): Json<PowerGuardSettings>,
) -> ApiResult<PowerGuardStatus> {
    let st = crate::powerguard::set(&s.config, Some(req)).await?;
    doctor::invalidate(&s);
    s.events.changed("power-guard");
    Ok(Json(st))
}

async fn power_guard_reset(State(s): State<AppState>) -> ApiResult<PowerGuardStatus> {
    let st = crate::powerguard::set(&s.config, None).await?;
    doctor::invalidate(&s);
    s.events.changed("power-guard");
    Ok(Json(st))
}

async fn domain_info(State(s): State<AppState>) -> Json<DomainInfo> {
    let cfg = &s.config;
    let domain = cfg.install_value("domain");
    let mode = match (domain.as_deref(), cfg.install_value("domain_mode").as_deref()) {
        (None, _) => "private",
        (Some(_), Some("cloudflare")) => "cloudflare",
        (Some(_), _) => "direct",
    };
    let wildcard = if cfg.role() == "worker" {
        None
    } else {
        coolify::query(
            cfg,
            "select coalesce(wildcard_domain, '') from server_settings where server_id = 0",
        )
        .await
        .ok()
        .and_then(|r| r.first()?.first().cloned())
        .filter(|w| !w.is_empty())
    };
    let tunnel_active = if mode == "cloudflare" {
        Some(services::status(&cfg.systemctl_bin, "cloudflared").await.active_state == "active")
    } else {
        None
    };
    Json(DomainInfo {
        mode: mode.into(),
        domain,
        wildcard,
        tunnel_active,
    })
}

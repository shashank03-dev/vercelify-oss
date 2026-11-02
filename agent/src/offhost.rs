//! Explicit computer/CI image handoff. Only a live lease may load its unique tag.
use crate::{
    error::{ApiResult, AppError},
    json::Json,
    platform_api::platform,
    routes::AppState,
    tokens::Actor,
};
use axum::{
    extract::{Path, Query, Request, State},
    Extension,
};
use http_body_util::BodyExt;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use vercelify_core::platform::{BuildJob, LocalBuildRequest};
use vercelify_platform::{artifact, deployments, offhost, proc};

#[derive(Deserialize)]
pub struct Lease {
    pub generation: u32,
}
#[derive(Deserialize)]
pub struct Upload {
    pub generation: u32,
    pub digest: String,
}

pub async fn create(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(app): Path<String>,
    Json(request): Json<LocalBuildRequest>,
) -> ApiResult<serde_json::Value> {
    let p = platform(&s)?;
    let engine = p.engine.clone();
    let key = p.key.clone();
    let store = p.store.clone();
    let owner = actor.id;
    let result = tokio::task::spawn_blocking(move || -> vercelify_platform::Result<_> {
        let job = engine.enqueue_external(&app, &owner, &request)?;
        let d = store.with(|c| deployments::get(c, &job.deployment_id))?;
        let vars = store.with(|c| offhost::build_variables(c, &key, &d.app_id, d.pr_number.is_some()))?;
        let vars: Vec<_> = vars
            .into_iter()
            .map(|(name, value, secret)| json!({"key":name,"value":value,"secret":secret}))
            .collect();
        Ok(json!({"job":job,"variables":vars}))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))??;
    s.events.changed("deployments");
    Ok(Json(result))
}

pub async fn get(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<BuildJob> {
    let p = platform(&s)?;
    Ok(Json(p.store.call(move |c| offhost::get(c, &id)).await?))
}

pub async fn renew(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(lease): Json<Lease>,
) -> ApiResult<BuildJob> {
    let p = platform(&s)?;
    Ok(Json(
        p.store
            .call(move |c| offhost::renew(c, &id, &actor.id, lease.generation, offhost::timestamp()))
            .await?,
    ))
}

pub async fn upload(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Query(query): Query<Upload>,
    request: Request,
) -> ApiResult<BuildJob> {
    let p = platform(&s)?;
    if !artifact::digest(&query.digest) {
        return Err(AppError::bad_request("image digest must be a SHA256 config digest"));
    }
    let bytes = request
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0 && *n <= artifact::MAX_ARCHIVE_BYTES)
        .ok_or_else(|| AppError::bad_request("declare a nonzero Content-Length, at most 3 GiB"))?;
    let token = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_string();
    let dir = s.config.state_dir.join("build-uploads");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let free = proc::run(proc::Spec {
        bin: "df",
        args: &proc::args(&["-Pk", &dir.to_string_lossy()]),
        env: &[],
        cwd: None,
        timeout: Duration::from_secs(10),
    })
    .await?;
    let available = free
        .stdout
        .lines()
        .last()
        .and_then(|l| l.split_whitespace().nth(3))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_mul(1024);
    if available < bytes.saturating_mul(2).saturating_add(512 * 1024 * 1024) {
        return Err(AppError::conflict(
            "not enough disk headroom for the uploaded image and its validated copy",
        ));
    }
    let job = p
        .store
        .tx(|c| offhost::begin_upload(c, &id, &actor.id, query.generation, offhost::timestamp()))?;
    let result = async {
        let raw = artifact::private_upload(&dir)?;
        let safe = artifact::private_upload(&dir)?;
        let mut output = tokio::fs::File::from_std(raw.reopen().map_err(|e| AppError::internal(e.to_string()))?);
        let mut body = request.into_body();
        let mut received = 0u64;
        let mut checked = std::time::Instant::now();
        while let Some(frame) = tokio::time::timeout(Duration::from_secs(30), body.frame())
            .await
            .map_err(|_| AppError::bad_request("image upload stalled"))?
        {
            let frame = frame.map_err(|_| AppError::bad_request("image upload interrupted"))?;
            if let Ok(data) = frame.into_data() {
                received = received.saturating_add(data.len() as u64);
                if received > bytes {
                    return Err(AppError::bad_request("image upload exceeded its declared size"));
                }
                output
                    .write_all(&data)
                    .await
                    .map_err(|e| AppError::unavailable(format!("image upload storage: {e}")))?;
            }
            if checked.elapsed() > Duration::from_secs(1) {
                p.store
                    .with(|c| offhost::authorize(c, &id, &actor.id, query.generation, offhost::timestamp()))?;
                checked = std::time::Instant::now();
            }
        }
        if received != bytes {
            return Err(AppError::bad_request("incomplete image upload"));
        }
        output
            .flush()
            .await
            .map_err(|e| AppError::unavailable(format!("image upload storage: {e}")))?;
        drop(output);
        let tag = job.image.clone();
        let digest = query.digest.clone();
        let platform = job.platform.clone();
        // Move the guards too: a disconnected HTTP future cannot leave a
        // background sanitizer creating a new file after its guard was dropped.
        let (_raw, safe, identity) = tokio::task::spawn_blocking(move || -> vercelify_platform::Result<_> {
            let identity = artifact::sanitize(raw.path(), safe.path(), &tag, &digest, &platform)?;
            Ok((raw, safe, identity))
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))??;
        p.store
            .with(|c| offhost::authorize(c, &id, &actor.id, query.generation, offhost::timestamp()))?;
        let loaded = p
            .engine
            .deployer
            .docker
            .run(
                &proc::args(&["load", "--input", &safe.path().to_string_lossy()]),
                Duration::from_secs(900),
            )
            .await?;
        if !loaded.success() {
            return Err(AppError::bad_request(
                "Docker could not load the validated image; retry the handoff",
            ));
        }
        let inspect = p
            .engine
            .deployer
            .docker
            .run(
                &proc::args(&["image", "inspect", "--format", "{{json .}}", &job.image]),
                Duration::from_secs(30),
            )
            .await?;
        let loaded_identity: serde_json::Value = serde_json::from_str(inspect.stdout.trim())
            .map_err(|_| AppError::bad_request("could not inspect loaded image identity"))?;
        if !inspect.success() || !artifact::matches_loaded(&identity, &loaded_identity, &job.platform) {
            return Err(AppError::bad_request(
                "loaded image digest or architecture differs from the build job",
            ));
        }
        if !s.tokens.verify(&s.config, &token).is_some_and(|a| a.id == actor.id) {
            return Err(AppError::conflict("build device was revoked during upload"));
        }
        let main = p.store.with(vercelify_platform::nodes::main)?;
        let arch = job.platform.trim_start_matches("linux/");
        let variant = vercelify_platform::registry::arch_tag(&job.image, arch, &main.arch);
        if variant != job.image {
            let tagged = p
                .engine
                .deployer
                .docker
                .run(&proc::args(&["tag", &job.image, &variant]), Duration::from_secs(30))
                .await?;
            if !tagged.success() {
                return Err(AppError::internal("could not stage the target image architecture"));
            }
        }
        let published = p.store.tx(|c| {
            offhost::publish(
                c,
                &p.key,
                &id,
                &actor.id,
                query.generation,
                &query.digest,
                offhost::timestamp(),
            )
        })?;
        s.events.changed("deployments");
        Ok(Json(published))
    }
    .await;
    if result.is_err() {
        let _ = p.store.tx(|c| {
            offhost::fail(
                c,
                &id,
                "off-host image handoff failed; retry from the selected computer or CI",
            )
        });
    }
    result
}

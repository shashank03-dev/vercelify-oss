//! Local LLMs: Ollama in a container the agent manages.
//!
//! Low-end defaults: one model in memory at a time, unloaded after 5 idle
//! minutes, one request at a time, and half the CPU weight of other
//! containers so the web apps on the same machine stay responsive. The API
//! is published on port 11434; the Vercelify firewall keeps it private to
//! the mesh, so a Mac (or any other server) can use it directly, and apps
//! on the same host reach it at `http://vercelify-ollama:11434` on the
//! proxy's network.

use crate::config::Config;
use crate::error::AppError;
use crate::exec::run;
use crate::http::{self, Req};
use crate::{services, volumes};
use http_body_util::BodyExt;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use vercelify_core::{LlmChatRequest, LlmLoaded, LlmModel, LlmStatus, LlmTask, StorageTier, VolumeRequest};

const DOCKER_TIMEOUT: Duration = Duration::from_secs(30);
/// Pulling the image (~1-3 GB) on a slow line.
const IMAGE_PULL_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// No progress from Ollama for this long = the model download stalled.
const PULL_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Loading a model from an HDD can take a while before the first token.
pub const CHAT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_TASKS: usize = 12;
/// Answer length cap unless the request asks for another one.
pub const DEFAULT_MAX_TOKENS: u32 = 1024;
/// The Ollama image is ~4 GB compressed, ~9.5 GB unpacked (GPU libraries).
const IMAGE_SPACE_BYTES: u64 = 12 * 1024 * 1024 * 1024;

pub fn valid_model(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/'))
}

/// (MemTotal, MemAvailable) in bytes from /proc/meminfo.
pub fn parse_meminfo(raw: &str) -> (u64, u64) {
    let field = |k: &str| {
        raw.lines()
            .find_map(|l| l.strip_prefix(k))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    (field("MemTotal:"), field("MemAvailable:"))
}

#[derive(Deserialize)]
struct Tags {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    modified_at: Option<String>,
    #[serde(default)]
    details: Option<TagDetails>,
}

#[derive(Deserialize, Default)]
struct TagDetails {
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    parameter_size: Option<String>,
    #[serde(default)]
    quantization_level: Option<String>,
}

#[derive(Deserialize)]
struct Ps {
    #[serde(default)]
    models: Vec<PsModel>,
}

#[derive(Deserialize)]
struct PsModel {
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    expires_at: Option<String>,
}

pub fn parse_tags(body: &[u8]) -> Vec<LlmModel> {
    let mut models: Vec<LlmModel> = serde_json::from_slice::<Tags>(body)
        .map(|t| t.models)
        .unwrap_or_default()
        .into_iter()
        .map(|m| {
            let d = m.details.unwrap_or_default();
            LlmModel {
                name: m.name,
                size_bytes: m.size,
                modified_at: m.modified_at,
                family: d.family.filter(|s| !s.is_empty()),
                parameter_size: d.parameter_size.filter(|s| !s.is_empty()),
                quantization: d.quantization_level.filter(|s| !s.is_empty()),
            }
        })
        .collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    models
}

pub fn parse_ps(body: &[u8]) -> Vec<LlmLoaded> {
    serde_json::from_slice::<Ps>(body)
        .map(|p| p.models)
        .unwrap_or_default()
        .into_iter()
        .map(|m| LlmLoaded {
            name: m.name,
            size_bytes: m.size,
            expires_at: m.expires_at,
        })
        .collect()
}

/// Apply one NDJSON line of Ollama's `/api/pull` stream to a task.
pub fn apply_pull_line(task: &mut LlmTask, line: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
        task.error = Some(e.to_string());
        task.done = true;
        task.status = "failed".into();
        return;
    }
    if let Some(s) = v.get("status").and_then(|s| s.as_str()) {
        task.status = s.to_string();
        if s == "success" {
            task.done = true;
        }
    }
    match (
        v.get("total").and_then(|t| t.as_u64()),
        v.get("completed").and_then(|c| c.as_u64()),
    ) {
        (Some(t), c) => {
            task.total = Some(t);
            task.completed = Some(c.unwrap_or(0));
        }
        (None, _) if !task.done => {
            // manifest/verify steps carry no byte counts
            task.total = None;
            task.completed = None;
        }
        _ => {}
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A task's job in the activity list, and what was last reported on it.
struct TaskJob {
    handle: crate::events::JobHandle,
    status: String,
    pct: Option<u32>,
}

pub struct Llm {
    cfg: Arc<Config>,
    tasks: Mutex<Vec<LlmTask>>,
    jobs: std::sync::OnceLock<Arc<crate::events::Jobs>>,
    task_jobs: Mutex<std::collections::HashMap<(String, String), TaskJob>>,
}

impl Llm {
    pub fn new(cfg: Arc<Config>) -> Self {
        Self {
            cfg,
            tasks: Mutex::new(Vec::new()),
            jobs: std::sync::OnceLock::new(),
            task_jobs: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Installs and downloads show up in the activity list.
    pub fn set_jobs(&self, j: Arc<crate::events::Jobs>) {
        let _ = self.jobs.set(j);
    }

    fn api(&self, path: &str) -> String {
        format!("{}{path}", self.cfg.ollama_url.trim_end_matches('/'))
    }

    async fn container_state(&self) -> Option<String> {
        let out = run(
            &self.cfg.docker_bin,
            &["inspect", "-f", "{{.State.Status}}", &self.cfg.ollama_container],
            DOCKER_TIMEOUT,
        )
        .await
        .ok()?;
        out.success()
            .then(|| out.stdout.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    async fn data_path(&self) -> Option<String> {
        let out = run(
            &self.cfg.docker_bin,
            &[
                "inspect",
                "-f",
                r#"{{range .Mounts}}{{if eq .Destination "/root/.ollama"}}{{.Source}}{{end}}{{end}}"#,
                &self.cfg.ollama_container,
            ],
            DOCKER_TIMEOUT,
        )
        .await
        .ok()?;
        out.success()
            .then(|| out.stdout.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    async fn get_json(&self, path: &str) -> Result<bytes::Bytes, String> {
        let url = self.api(path);
        let (status, body) = http::call(Req::get(&url).timeout(Duration::from_secs(5)))
            .await
            .map_err(|e| e.to_string())?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(http::error_text(status, &body))
        }
    }

    pub async fn status(&self) -> LlmStatus {
        let state = self.container_state().await;
        let running = state.as_deref() == Some("running");
        let (version, models, loaded, error) = if running {
            match self.get_json("/api/version").await {
                Ok(v) => {
                    let version = serde_json::from_slice::<serde_json::Value>(&v)
                        .ok()
                        .and_then(|v| v.get("version")?.as_str().map(String::from));
                    let models = self
                        .get_json("/api/tags")
                        .await
                        .map(|b| parse_tags(&b))
                        .unwrap_or_default();
                    let loaded = self.get_json("/api/ps").await.map(|b| parse_ps(&b)).unwrap_or_default();
                    (version, models, loaded, None)
                }
                Err(e) => (
                    None,
                    vec![],
                    vec![],
                    Some(format!("Ollama is starting or not answering: {e}")),
                ),
            }
        } else {
            (None, vec![], vec![], None)
        };
        let port = self.cfg.ollama_port;
        let endpoints: Vec<String> = crate::mesh::private_ipv4(&self.cfg)
            .await
            .map(|ip| format!("http://{ip}:{port}"))
            .into_iter()
            .collect();
        let (memory_total_bytes, memory_available_bytes) =
            parse_meminfo(&std::fs::read_to_string("/proc/meminfo").unwrap_or_default());
        let firewall_active = services::status(&self.cfg.systemctl_bin, "vercelify-firewall")
            .await
            .active_state
            == "active";
        LlmStatus {
            installed: state.is_some(),
            running,
            ready: version.is_some(),
            container_state: state,
            image: self.cfg.ollama_image.clone(),
            version,
            port,
            endpoints,
            data_path: self.data_path().await,
            models,
            loaded,
            tasks: self.tasks.lock().await.clone(),
            firewall_active,
            memory_total_bytes,
            memory_available_bytes,
            error,
        }
    }

    /// Register a task, refusing a duplicate that is still running.
    async fn begin(&self, kind: &str, target: &str, status: &str) -> Result<(), AppError> {
        let mut tasks = self.tasks.lock().await;
        if tasks.iter().any(|t| !t.done && t.kind == kind && t.target == target) {
            return Err(AppError::conflict(format!(
                "{target} is already being {}",
                if kind == "pull" { "downloaded" } else { "installed" }
            )));
        }
        tasks.retain(|t| !(t.kind == kind && t.target == target));
        tasks.push(LlmTask {
            kind: kind.into(),
            target: target.into(),
            status: status.into(),
            completed: None,
            total: None,
            done: false,
            error: None,
            started_at: now(),
        });
        if let Some(jobs) = self.jobs.get() {
            let title = if kind == "pull" {
                format!("Download AI model {target}")
            } else {
                "Install the local AI server".to_string()
            };
            let handle = jobs.start(&format!("llm-{kind}"), title, Some(target));
            handle.progress(status, None);
            self.task_jobs.lock().await.insert(
                (kind.to_string(), target.to_string()),
                TaskJob {
                    handle,
                    status: status.into(),
                    pct: None,
                },
            );
        }
        // Keep the list short: drop the oldest finished tasks.
        while tasks.len() > MAX_TASKS {
            match tasks.iter().position(|t| t.done) {
                Some(i) => {
                    tasks.remove(i);
                }
                None => break,
            }
        }
        Ok(())
    }

    async fn update(&self, kind: &str, target: &str, f: impl FnOnce(&mut LlmTask)) {
        let snapshot = {
            let mut tasks = self.tasks.lock().await;
            let Some(t) = tasks.iter_mut().find(|t| t.kind == kind && t.target == target) else {
                return;
            };
            f(t);
            t.clone()
        };
        // Pull progress arrives many times a second: pass on whole percents
        // and status changes only.
        let mut jobs = self.task_jobs.lock().await;
        if let Some(tj) = jobs.get_mut(&(kind.to_string(), target.to_string())) {
            let pct = match (snapshot.completed, snapshot.total) {
                (Some(c), Some(t)) if t > 0 => Some((c.saturating_mul(100) / t).min(100) as u32),
                _ => None,
            };
            if !snapshot.done && (pct != tj.pct || snapshot.status != tj.status) {
                tj.pct = pct;
                tj.status = snapshot.status.clone();
                tj.handle.progress(snapshot.status, pct.map(|p| p as f32));
            }
        }
    }

    async fn finish(&self, kind: &str, target: &str, result: Result<(), String>) {
        if let Some(tj) = self
            .task_jobs
            .lock()
            .await
            .remove(&(kind.to_string(), target.to_string()))
        {
            tj.handle.finish(result.clone().map(|_| "done".to_string()));
        }
        self.update(kind, target, |t| {
            t.done = true;
            match result {
                Ok(()) => t.status = "success".into(),
                Err(e) => {
                    t.status = "failed".into();
                    t.error = Some(e);
                }
            }
        })
        .await;
    }

    /// Start installing Ollama in the background.
    pub async fn install(self: &Arc<Self>, tier: StorageTier) -> Result<(), AppError> {
        // Docker keeps images on the system disk, next to its volumes.
        let docker_root = self
            .cfg
            .docker_volumes_root
            .parent()
            .unwrap_or(&self.cfg.docker_volumes_root);
        if let Some(free) = free_bytes(docker_root) {
            let have_image = run(
                &self.cfg.docker_bin,
                &["image", "inspect", &self.cfg.ollama_image],
                DOCKER_TIMEOUT,
            )
            .await
            .is_ok_and(|o| o.success());
            if !have_image && free < IMAGE_SPACE_BYTES {
                return Err(AppError::conflict(format!(
                    "Ollama needs about 12 GB free on the system disk ({} has {:.1} GB) — free some space (docker system prune) and try again",
                    docker_root.display(),
                    free as f64 / 1024f64.powi(3)
                )));
            }
        }
        // Resolve the storage first so a missing HDD fails the request itself.
        let vol = volumes::create(
            &self.cfg,
            VolumeRequest {
                service: "ollama".into(),
                name: Some("models".into()),
                tier,
                uid: None,
                gid: None,
            },
        )
        .await?;
        let image = self.cfg.ollama_image.clone();
        self.begin("install", &image, "downloading the Ollama image").await?;
        let me = self.clone();
        tokio::spawn(async move {
            let r = me.do_install(&vol.source).await;
            if let Err(e) = &r {
                tracing::warn!("ollama install failed: {e}");
            }
            me.finish("install", &image, r).await;
        });
        Ok(())
    }

    async fn docker(&self, args: &[&str], timeout: Duration) -> Result<String, String> {
        let out = run(&self.cfg.docker_bin, args, timeout)
            .await
            .map_err(|e| e.to_string())?;
        if out.success() {
            Ok(out.stdout)
        } else {
            Err(format!("docker {}: {}", args.first().unwrap_or(&""), out.stderr.trim()))
        }
    }

    async fn do_install(&self, source: &str) -> Result<(), String> {
        let c = &self.cfg;
        let image = &c.ollama_image;
        self.docker(&["pull", "-q", image], IMAGE_PULL_TIMEOUT).await?;
        self.update("install", image, |t| t.status = "starting Ollama".into())
            .await;
        if self.container_state().await.is_some() {
            self.docker(&["rm", "-f", &c.ollama_container], Duration::from_secs(60))
                .await?;
        }
        let publish = format!("{}:11434", c.ollama_port);
        let mount = format!("{source}:/root/.ollama");
        self.docker(
            &[
                "run",
                "-d",
                "--name",
                &c.ollama_container,
                "--restart",
                "unless-stopped",
                "--label",
                "vercelify.managed=llm",
                "--cpu-shares",
                "512",
                "-p",
                &publish,
                "-v",
                &mount,
                "-e",
                "OLLAMA_HOST=0.0.0.0:11434",
                "-e",
                "OLLAMA_KEEP_ALIVE=5m",
                "-e",
                "OLLAMA_MAX_LOADED_MODELS=1",
                "-e",
                "OLLAMA_NUM_PARALLEL=1",
                image,
            ],
            Duration::from_secs(120),
        )
        .await?;
        // Let the platform's apps call http://vercelify-ollama:11434.
        let net = vercelify_platform::docker::PROXY_NETWORK;
        if self.docker(&["network", "inspect", net], DOCKER_TIMEOUT).await.is_ok() {
            let _ = self
                .docker(&["network", "connect", net, &c.ollama_container], DOCKER_TIMEOUT)
                .await;
        }
        for _ in 0..60 {
            if self.get_json("/api/version").await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err("Ollama started but its API did not answer within a minute — check `docker logs vercelify-ollama`".into())
    }

    pub async fn container_action(&self, action: &str) -> Result<(), AppError> {
        let args: Vec<&str> = match action {
            "start" | "stop" | "restart" => vec![action, &self.cfg.ollama_container],
            // Models stay on disk; reinstalling picks them up again.
            "remove" => vec!["rm", "-f", &self.cfg.ollama_container],
            _ => return Err(AppError::bad_request(format!("unsupported action `{action}`"))),
        };
        if self.container_state().await.is_none() {
            return Err(AppError::not_found("Ollama is not installed on this server"));
        }
        self.docker(&args, Duration::from_secs(90))
            .await
            .map_err(AppError::conflict)?;
        Ok(())
    }

    async fn require_ready(&self) -> Result<(), AppError> {
        self.get_json("/api/version")
            .await
            .map(|_| ())
            .map_err(|_| AppError::unavailable("Ollama is not running on this server — install or start it first"))
    }

    /// Start downloading a model in the background.
    pub async fn pull(self: &Arc<Self>, model: &str) -> Result<(), AppError> {
        if !valid_model(model) {
            return Err(AppError::bad_request(
                "model names look like `llama3.2:3b` or `qwen2.5-coder:1.5b`",
            ));
        }
        self.require_ready().await?;
        self.begin("pull", model, "starting download").await?;
        let me = self.clone();
        let model = model.to_string();
        tokio::spawn(async move {
            let r = me.do_pull(&model).await;
            me.update("pull", &model, |t| {
                if t.done {
                    return; // Ollama already reported success or an error
                }
                t.done = true;
                t.status = "failed".into();
                t.error = Some(match r {
                    Err(e) => e,
                    Ok(()) => "the download ended before Ollama reported success".into(),
                });
            })
            .await;
        });
        Ok(())
    }

    async fn do_pull(&self, model: &str) -> Result<(), String> {
        let url = self.api("/api/pull");
        let resp = http::stream(
            Req::post(&url, &serde_json::json!({ "model": model, "stream": true })).timeout(Duration::from_secs(60)),
        )
        .await
        .map_err(|e| e.to_string())?;
        let status = resp.status();
        let mut body = resp.into_body();
        if !status.is_success() {
            let bytes = body.collect().await.map(|c| c.to_bytes()).unwrap_or_default();
            return Err(http::error_text(status, &bytes));
        }
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let frame = match tokio::time::timeout(PULL_IDLE_TIMEOUT, body.frame()).await {
                Err(_) => return Err("download stalled (no progress for 5 minutes)".into()),
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(format!("download interrupted: {e}")),
                Ok(Some(Ok(f))) => f,
            };
            let Ok(data) = frame.into_data() else { continue };
            buf.extend_from_slice(&data);
            while let Some(i) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=i).collect();
                let line = String::from_utf8_lossy(&line).to_string();
                self.update("pull", model, |t| apply_pull_line(t, line.trim())).await;
            }
        }
        if !buf.is_empty() {
            let line = String::from_utf8_lossy(&buf).to_string();
            self.update("pull", model, |t| apply_pull_line(t, line.trim())).await;
        }
        Ok(())
    }

    pub async fn delete_model(&self, model: &str) -> Result<(), AppError> {
        if !valid_model(model) {
            return Err(AppError::bad_request("invalid model name"));
        }
        self.require_ready().await?;
        let url = self.api("/api/delete");
        let (status, body) = http::call(Req::delete(&url, &serde_json::json!({ "model": model })))
            .await
            .map_err(|e| AppError::unavailable(e.to_string()))?;
        match status.as_u16() {
            200..=299 => Ok(()),
            404 => Err(AppError::not_found(format!("{model} is not downloaded"))),
            _ => Err(AppError::internal(http::error_text(status, &body))),
        }
    }

    /// Forward a chat to Ollama; the response body streams through untouched.
    pub async fn chat(&self, req: LlmChatRequest) -> Result<hyper::Response<hyper::body::Incoming>, AppError> {
        if !valid_model(&req.model) {
            return Err(AppError::bad_request("invalid model name"));
        }
        if req.messages.is_empty() || req.messages.len() > 200 {
            return Err(AppError::bad_request("send between 1 and 200 messages"));
        }
        if let Some(m) = req
            .messages
            .iter()
            .find(|m| !matches!(m.role.as_str(), "system" | "user" | "assistant"))
        {
            return Err(AppError::bad_request(format!("unknown message role `{}`", m.role)));
        }
        let mut options = serde_json::json!({
            "num_predict": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS).clamp(16, 8192),
        });
        if let Some(t) = req.temperature {
            options["temperature"] = serde_json::json!(t.clamp(0.0, 2.0));
        }
        let body = serde_json::json!({
            "model": req.model,
            "messages": req.messages,
            "stream": req.stream.unwrap_or(false),
            "options": options,
        });
        let url = self.api("/api/chat");
        let resp = http::stream(Req::post(&url, &body).timeout(CHAT_TIMEOUT))
            .await
            .map_err(|e| match e {
                http::HttpError::Connect(..) => {
                    AppError::unavailable("Ollama is not running on this server — install or start it first")
                }
                e => AppError::unavailable(e.to_string()),
            })?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .map(|c| c.to_bytes())
            .unwrap_or_default();
        let text = http::error_text(status, &bytes);
        Err(if status == hyper::StatusCode::NOT_FOUND {
            AppError::not_found(format!("{text} — download it first"))
        } else {
            AppError::unavailable(text)
        })
    }
}

/// Free bytes (as root sees them) on the filesystem holding `path`.
fn free_bytes(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs is plain old data; zeroed is a valid initial value.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` a writable statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    // The field types differ between targets (u32 on some 32-bit libcs).
    #[allow(clippy::unnecessary_cast)]
    Some(st.f_bfree as u64 * st.f_frsize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space() {
        assert!(free_bytes(std::path::Path::new("/")).unwrap() > 0);
        assert_eq!(free_bytes(std::path::Path::new("/definitely/not/here")), None);
    }

    #[test]
    fn model_names() {
        for ok in [
            "llama3.2:3b",
            "qwen2.5-coder:1.5b",
            "hf.co/bartowski/Llama-3.2-1B-Instruct-GGUF:Q4_K_M",
            "phi4-mini",
        ] {
            assert!(valid_model(ok), "{ok}");
        }
        for bad in ["", "-x", "../x", "a b", "a;b", "x\n"] {
            assert!(!valid_model(bad), "{bad:?}");
        }
    }

    #[test]
    fn meminfo() {
        let raw = "MemTotal:        8058920 kB\nMemFree:          212340 kB\nMemAvailable:    3120000 kB\n";
        assert_eq!(parse_meminfo(raw), (8058920 * 1024, 3120000 * 1024));
        assert_eq!(parse_meminfo(""), (0, 0));
    }

    #[test]
    fn tags_and_ps() {
        let tags = br#"{"models":[{"name":"qwen2.5:3b","model":"qwen2.5:3b","modified_at":"2026-10-18T10:00:00Z","size":1929912432,"details":{"family":"qwen2","parameter_size":"3.1B","quantization_level":"Q4_K_M"}},{"name":"gemma3:1b","size":815319791}]}"#;
        let m = parse_tags(tags);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, "gemma3:1b");
        assert_eq!(m[1].parameter_size.as_deref(), Some("3.1B"));
        assert_eq!(m[1].quantization.as_deref(), Some("Q4_K_M"));
        assert!(parse_tags(b"nope").is_empty());

        let ps = br#"{"models":[{"name":"qwen2.5:3b","size":2500000000,"size_vram":0,"expires_at":"2026-10-18T10:05:00Z"}]}"#;
        assert_eq!(parse_ps(ps)[0].size_bytes, 2_500_000_000);
    }

    #[test]
    fn pull_progress() {
        let mut t = LlmTask {
            kind: "pull".into(),
            target: "gemma3:1b".into(),
            status: String::new(),
            completed: None,
            total: None,
            done: false,
            error: None,
            started_at: String::new(),
        };
        apply_pull_line(&mut t, r#"{"status":"pulling manifest"}"#);
        assert_eq!(t.status, "pulling manifest");
        apply_pull_line(
            &mut t,
            r#"{"status":"pulling 7cd4","digest":"sha256:7cd4","total":815310496,"completed":1000}"#,
        );
        assert_eq!((t.completed, t.total), (Some(1000), Some(815310496)));
        apply_pull_line(&mut t, "not json");
        assert_eq!(t.completed, Some(1000));
        apply_pull_line(&mut t, r#"{"status":"success"}"#);
        assert!(t.done && t.error.is_none());

        let mut f = t.clone();
        f.done = false;
        apply_pull_line(&mut f, r#"{"error":"pull model manifest: file does not exist"}"#);
        assert!(f.done);
        assert_eq!(f.status, "failed");
    }
}

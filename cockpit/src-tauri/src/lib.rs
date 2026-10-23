//! Vercelify cockpit — Tauri backend.
//!
//! The React UI calls these commands; everything that touches the network,
//! the filesystem or child processes happens here.

mod discover;
mod http;
mod local;
mod procs;
mod tray;

use procs::Processes;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder};
use vercelify_core::collect::{Collector, SysPaths};
use vercelify_core::pairing::{self, Pairing};
use vercelify_core::stream::{moonlight_args, sender_args, MoonlightLaunch, SenderConfig};
use vercelify_core::{
    avfoundation::CaptureScreen, CockpitConfig, CockpitSettings, DiscoveredServer, ProcessStatus, SystemSnapshot,
    TailscaleStatus, ToolStatus,
};

const SENDER: &str = "sender";
const MOONLIGHT: &str = "moonlight";

struct AppState {
    /// Every paired server; one is selected.
    config: Mutex<CockpitConfig>,
    /// Open NDJSON streams (LLM chats), by id, so the UI can cancel them.
    streams: Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>,
    http: reqwest::Client,
    procs: Processes,
    collector: Collector,
}

type CmdResult<T> = Result<T, String>;

// --- settings ---------------------------------------------------------------

fn settings_path(app: &AppHandle) -> CmdResult<PathBuf> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}

fn read_config(app: &AppHandle) -> CockpitConfig {
    settings_path(app)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| CockpitConfig::from_json(&raw))
        .unwrap_or_default()
}

fn write_private(path: &PathBuf, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(contents.as_bytes())
}

/// Change the config under the lock and persist it (0600).
fn update_config<T>(
    app: &AppHandle,
    state: &State<'_, AppState>,
    f: impl FnOnce(&mut CockpitConfig) -> T,
) -> CmdResult<(T, CockpitConfig)> {
    let mut cfg = state.config.lock().map_err(|e| e.to_string())?;
    let out = f(&mut cfg);
    let json = serde_json::to_string_pretty(&*cfg).map_err(|e| e.to_string())?;
    write_private(&settings_path(app)?, &json).map_err(|e| e.to_string())?;
    Ok((out, cfg.clone()))
}

/// The server a request is for: the given agent URL, or the selected one.
fn server(state: &State<'_, AppState>, agent_url: Option<&str>) -> CmdResult<CockpitSettings> {
    let cfg = state.config.lock().map_err(|e| e.to_string())?;
    match agent_url {
        Some(u) => cfg
            .find(u)
            .cloned()
            .ok_or_else(|| format!("{u} is not a paired server")),
        None => cfg
            .active_server()
            .cloned()
            .ok_or_else(|| "Not paired yet — run `sudo vercelify pair` on your server".into()),
    }
}

/// The selected server (compatible with the single-server API of 0.1).
#[tauri::command]
fn settings_load(state: State<'_, AppState>) -> Option<CockpitSettings> {
    state.config.lock().ok()?.active_server().cloned()
}

/// Add (or update) a server and select it.
#[tauri::command]
fn settings_save(app: AppHandle, state: State<'_, AppState>, settings: CockpitSettings) -> CmdResult<()> {
    update_config(&app, &state, |c| c.upsert(settings)).map(|_| ())
}

/// Forget the selected server.
#[tauri::command]
fn settings_clear(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    update_config(&app, &state, |c| {
        if let Some(a) = c.active_server().map(|s| s.agent_url.clone()) {
            c.remove(&a);
        }
    })
    .map(|_| ())
}

#[tauri::command]
fn servers_load(state: State<'_, AppState>) -> CmdResult<CockpitConfig> {
    Ok(state.config.lock().map_err(|e| e.to_string())?.clone())
}

#[tauri::command]
fn server_select(app: AppHandle, state: State<'_, AppState>, agent_url: String) -> CmdResult<CockpitConfig> {
    let (found, cfg) = update_config(&app, &state, |c| {
        let found = c.find(&agent_url).is_some();
        if found {
            c.active = Some(agent_url.clone());
        }
        found
    })?;
    if found {
        Ok(cfg)
    } else {
        Err(format!("{agent_url} is not a paired server"))
    }
}

#[tauri::command]
fn server_remove(app: AppHandle, state: State<'_, AppState>, agent_url: String) -> CmdResult<CockpitConfig> {
    update_config(&app, &state, |c| c.remove(&agent_url)).map(|(_, c)| c)
}

#[tauri::command]
fn pair_decode(code: String) -> CmdResult<Pairing> {
    pairing::decode(&code).map_err(|e| e.to_string())
}

/// Trade a short code (`K7QF-M2XD`) for the full pairing at a discovered host.
#[tauri::command]
async fn pair_claim(state: State<'_, AppState>, agent_url: String, code: String) -> CmdResult<Pairing> {
    if !(agent_url.starts_with("http://") || agent_url.starts_with("https://")) {
        return Err("invalid agent URL".into());
    }
    let v = http::send(
        &state.http,
        &agent_url,
        None,
        http::Request {
            method: "POST".into(),
            path: "/v1/pair/claim".into(),
            body: Some(serde_json::json!({ "code": code })),
            timeout: Some(15),
        },
    )
    .await?;
    let full = v["pairing"].as_str().ok_or("the server sent no pairing code")?;
    pairing::decode(full).map_err(|e| e.to_string())
}

/// Vercelify hosts on the tailnet.
#[tauri::command]
async fn discover_servers(state: State<'_, AppState>) -> CmdResult<Vec<DiscoveredServer>> {
    let ts = tauri::async_runtime::spawn_blocking(local::tailscale_status)
        .await
        .map_err(|e| e.to_string())?;
    if ts.backend_state != "Running" {
        return Err(ts
            .error
            .clone()
            .unwrap_or_else(|| format!("Tailscale is not connected on this computer ({})", ts.backend_state)));
    }
    Ok(discover::discover(&state.http, ts).await)
}

// --- remote APIs ------------------------------------------------------------

#[tauri::command]
async fn agent_request(state: State<'_, AppState>, request: http::Request, server: Option<String>) -> CmdResult<Value> {
    let s = self::server(&state, server.as_deref())?;
    http::send(&state.http, &s.agent_url, Some(&s.agent_token), request).await
}

fn coolify_base(s: &CockpitSettings) -> CmdResult<(String, String)> {
    if s.coolify_url.trim().is_empty() {
        return Err(format!(
            "{} is a worker without its own Coolify — select your main server to deploy",
            s.hostname
        ));
    }
    let token = s
        .coolify_token
        .clone()
        .filter(|t| !t.is_empty())
        .ok_or("No Coolify API token — run `sudo vercelify pair --refresh` on the host and pair again")?;
    Ok((format!("{}/api/v1", s.coolify_url.trim_end_matches('/')), token))
}

#[tauri::command]
async fn coolify_request(
    state: State<'_, AppState>,
    request: http::Request,
    server: Option<String>,
) -> CmdResult<Value> {
    let s = self::server(&state, server.as_deref())?;
    let (base, token) = coolify_base(&s)?;
    http::send(&state.http, &base, Some(&token), request).await
}

/// An agent request whose NDJSON response is delivered line by line.
#[tauri::command]
async fn agent_stream(
    state: State<'_, AppState>,
    request: http::Request,
    server: Option<String>,
    stream_id: String,
    on_line: Channel<String>,
) -> CmdResult<()> {
    let s = self::server(&state, server.as_deref())?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .streams
        .lock()
        .map_err(|e| e.to_string())?
        .insert(stream_id.clone(), tx);
    let result = http::send_lines(&state.http, &s.agent_url, Some(&s.agent_token), request, rx, |line| {
        on_line.send(line).is_ok()
    })
    .await;
    if let Ok(mut m) = state.streams.lock() {
        m.remove(&stream_id);
    }
    result
}

#[tauri::command]
fn stream_cancel(state: State<'_, AppState>, stream_id: String) {
    if let Some(tx) = state.streams.lock().ok().and_then(|mut m| m.remove(&stream_id)) {
        let _ = tx.send(());
    }
}

/// Probe arbitrary connection details before saving them (onboarding).
#[tauri::command]
async fn test_connection(state: State<'_, AppState>, settings: CockpitSettings) -> CmdResult<Value> {
    let health = http::send(
        &state.http,
        &settings.agent_url,
        Some(&settings.agent_token),
        http::Request {
            method: "GET".into(),
            path: "/v1/system".into(),
            body: None,
            timeout: Some(10),
        },
    )
    .await?;
    let coolify = if settings.coolify_url.trim().is_empty() {
        serde_json::json!({ "ok": false, "error": "worker (no Coolify)" })
    } else {
        match coolify_base(&settings) {
            Err(e) => serde_json::json!({ "ok": false, "error": e }),
            Ok((base, token)) => http::send(
                &state.http,
                &base,
                Some(&token),
                http::Request {
                    method: "GET".into(),
                    path: "/version".into(),
                    body: None,
                    timeout: Some(10),
                },
            )
            .await
            .map(|v| serde_json::json!({ "ok": true, "version": v }))
            .unwrap_or_else(|e| serde_json::json!({ "ok": false, "error": e })),
        }
    };
    Ok(serde_json::json!({ "hostname": health["host"]["hostname"], "coolify": coolify }))
}

// --- this machine -----------------------------------------------------------

#[tauri::command]
async fn local_system(state: State<'_, AppState>) -> CmdResult<SystemSnapshot> {
    Ok(state.collector.snapshot(None))
}

#[tauri::command]
async fn local_tailscale() -> TailscaleStatus {
    tauri::async_runtime::spawn_blocking(local::tailscale_status)
        .await
        .unwrap_or_default()
}

#[tauri::command]
fn tools_status() -> Vec<ToolStatus> {
    local::tools()
}

#[tauri::command]
async fn capture_screens() -> CmdResult<Vec<CaptureScreen>> {
    tauri::async_runtime::spawn_blocking(local::capture_screens)
        .await
        .map_err(|e| e.to_string())?
}

// --- display streaming --------------------------------------------------------

#[tauri::command]
fn sender_start(state: State<'_, AppState>, config: SenderConfig) -> CmdResult<ProcessStatus> {
    let args = sender_args(&config).map_err(|e| e.to_string())?;
    let bin = local::ffmpeg().ok_or("ffmpeg is not installed — on macOS: brew install ffmpeg")?;
    state.procs.start(SENDER, &bin.to_string_lossy(), &args, true)
}

#[tauri::command]
fn sender_stop(state: State<'_, AppState>) -> ProcessStatus {
    state.procs.stop(SENDER);
    state.procs.status(SENDER)
}

#[tauri::command]
fn sender_status(state: State<'_, AppState>) -> ProcessStatus {
    state.procs.status(SENDER)
}

#[tauri::command]
fn moonlight_start(state: State<'_, AppState>, launch: MoonlightLaunch) -> CmdResult<ProcessStatus> {
    let args = moonlight_args(&launch).map_err(|e| e.to_string())?;
    let bin = local::moonlight().ok_or("Moonlight is not installed — get it from moonlight-stream.org")?;
    // Moonlight is a visible window the user can close; let it outlive us.
    state.procs.start(MOONLIGHT, &bin.to_string_lossy(), &args, false)
}

#[tauri::command]
fn moonlight_stop(state: State<'_, AppState>) -> ProcessStatus {
    state.procs.stop(MOONLIGHT);
    state.procs.status(MOONLIGHT)
}

#[tauri::command]
fn moonlight_status(state: State<'_, AppState>) -> ProcessStatus {
    state.procs.status(MOONLIGHT)
}

// --- embedded Coolify ---------------------------------------------------------

/// Styles injected into the Coolify dashboard so it matches the cockpit.
const COOLIFY_THEME_JS: &str = r#"
(() => {
  const css = `
    :root { color-scheme: dark; }
    html, body { background: #07080c !important; }
    body { font-family: -apple-system, BlinkMacSystemFont, "Inter", "Segoe UI", sans-serif !important; }
    ::-webkit-scrollbar { width: 10px; height: 10px; }
    ::-webkit-scrollbar-thumb { background: rgba(255,255,255,.12); border-radius: 8px; border: 2px solid transparent; background-clip: padding-box; }
    ::-webkit-scrollbar-track { background: transparent; }
    .bg-coolgray-100, .dark\:bg-coolgray-100 { background-color: rgba(255,255,255,.04) !important; backdrop-filter: blur(12px); }
    .bg-coolgray-200, .dark\:bg-coolgray-200 { background-color: rgba(255,255,255,.06) !important; }
    a, .text-warning { transition: color .15s ease; }
  `;
  const apply = () => {
    if (document.getElementById("vercelify-theme")) return;
    const s = document.createElement("style");
    s.id = "vercelify-theme";
    s.textContent = css;
    document.head.appendChild(s);
    document.documentElement.classList.add("dark");
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", apply); else apply();
})();
"#;

#[tauri::command]
async fn open_coolify(app: AppHandle, state: State<'_, AppState>, path: Option<String>) -> CmdResult<()> {
    let s = server(&state, None)?;
    if s.coolify_url.trim().is_empty() {
        return Err(format!(
            "{} is a worker — open Coolify from your main server",
            s.hostname
        ));
    }
    let mut url = s.coolify_url.trim_end_matches('/').to_string();
    if let Some(p) = path.filter(|p| p.starts_with('/') && !p.contains("://")) {
        url.push_str(&p);
    }
    let parsed: tauri::Url = url.parse().map_err(|_| format!("invalid Coolify URL: {url}"))?;
    if let Some(w) = app.get_webview_window("coolify") {
        w.navigate(parsed).map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }
    // Separate window with its own (persistent) cookie store, no IPC access.
    WebviewWindowBuilder::new(&app, "coolify", WebviewUrl::External(parsed))
        .title("Coolify — Vercelify")
        .inner_size(1280.0, 860.0)
        .min_inner_size(900.0, 600.0)
        .initialization_script(COOLIFY_THEME_JS)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// --- app ----------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// The menu-bar item's status line (set by the UI from its live data).
#[tauri::command]
fn tray_status(app: AppHandle, text: String) {
    if let Some(t) = app.try_state::<tray::Tray>() {
        t.set_status(&text);
    }
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            // A menu-bar item is a nicety: never fail to start without one.
            match tray::setup(app) {
                Ok(t) => {
                    app.manage(t);
                }
                Err(e) => tracing::warn!("menu-bar item unavailable: {e}"),
            }
            let config = read_config(app.handle());
            app.manage(AppState {
                config: Mutex::new(config),
                streams: Mutex::new(HashMap::new()),
                http: http::client(),
                procs: Processes::default(),
                collector: Collector::new(SysPaths::default(), env!("CARGO_PKG_VERSION")),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            settings_load,
            settings_save,
            settings_clear,
            servers_load,
            server_select,
            server_remove,
            pair_decode,
            pair_claim,
            discover_servers,
            agent_request,
            coolify_request,
            agent_stream,
            stream_cancel,
            test_connection,
            local_system,
            local_tailscale,
            tools_status,
            capture_screens,
            sender_start,
            sender_stop,
            sender_status,
            moonlight_start,
            moonlight_stop,
            moonlight_status,
            open_coolify,
            tray_status,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the Vercelify cockpit");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(state) = handle.try_state::<AppState>() {
                state.procs.stop_all();
            }
        }
    });
}

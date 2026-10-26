//! Vercelify cockpit — Tauri backend.
//!
//! The React UI calls these commands; everything that touches the network,
//! the filesystem or child processes happens here.

mod discover;
mod http;
mod local;
mod mesh;
mod procs;
mod tray;

use procs::Processes;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, RunEvent, State};
use vercelify_core::collect::{Collector, SysPaths};
use vercelify_core::pairing::{self, Pairing};
use vercelify_core::stream::{moonlight_args, sender_args, MoonlightLaunch, SenderConfig};
use vercelify_core::{
    avfoundation::CaptureScreen, CockpitConfig, CockpitSettings, DiscoveredServer, LanServer, MeshLocal, ProcessStatus,
    SystemSnapshot, ToolStatus,
};

const SENDER: &str = "sender";
const MOONLIGHT: &str = "moonlight";

struct AppState {
    /// Every paired server; one is selected.
    config: Mutex<CockpitConfig>,
    /// Open NDJSON streams (LLM chats), by id, so the UI can cancel them.
    streams: Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>,
    http: reqwest::Client,
    /// Through vercelify-mesh's SOCKS proxy (servers on the mesh, app mode).
    mesh_http: reqwest::Client,
    procs: Processes,
    collector: Collector,
}

type CmdResult<T> = Result<T, String>;

impl AppState {
    /// The client for `url`: through the mesh daemon's proxy when the server
    /// is on the mesh and the daemon runs in app mode (as root or on Linux
    /// the mesh is routed and needs no proxy).
    fn client_for(&self, url: &str) -> &reqwest::Client {
        if mesh::on_mesh(url) && mesh::status().userspace {
            &self.mesh_http
        } else {
            &self.http
        }
    }
}

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
    // Write beside the destination, then replace it atomically. Existing files
    // (including symlinks) never receive a partial write or retain public modes.
    let mut file = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "settings path has no parent"))?,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file().set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(contents.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Change the config under the lock and persist it (0600).
fn update_config<T>(
    app: &AppHandle,
    state: &State<'_, AppState>,
    f: impl FnOnce(&mut CockpitConfig) -> T,
) -> CmdResult<(T, CockpitConfig)> {
    let mut cfg = state.config.lock().map_err(|e| e.to_string())?;
    let mut next = cfg.clone();
    let out = f(&mut next);
    let json = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
    write_private(&settings_path(app)?, &json).map_err(|e| e.to_string())?;
    *cfg = next;
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

/// Join the mesh with a pasted `vfy2.` code (a `vfy1.` code needs nothing).
#[tauri::command]
async fn pair_join(code: String) -> CmdResult<Pairing> {
    let p = pairing::decode(&code).map_err(|e| e.to_string())?;
    if let Some(token) = p.mesh_join.clone() {
        tauri::async_runtime::spawn_blocking(move || {
            mesh::join(&token)?;
            mesh::wait_connected(std::time::Duration::from_secs(30))
        })
        .await
        .map_err(|e| e.to_string())??;
    }
    Ok(p)
}

/// Vercelify servers on this LAN (mDNS), to pair with a short code.
#[tauri::command]
async fn mesh_discover() -> CmdResult<Vec<LanServer>> {
    tauri::async_runtime::spawn_blocking(mesh::discover)
        .await
        .map_err(|e| e.to_string())?
}

/// Short code → pairing, over the LAN (SPAKE2), then join the mesh.
#[tauri::command]
async fn mesh_pair(code: String, server: Option<String>) -> CmdResult<Pairing> {
    let full = tauri::async_runtime::spawn_blocking(move || {
        let full = mesh::pair(&code, server.as_deref())?;
        mesh::wait_connected(std::time::Duration::from_secs(30))?;
        Ok::<_, String>(full)
    })
    .await
    .map_err(|e| e.to_string())??;
    pairing::decode(&full).map_err(|e| e.to_string())
}

/// This computer on the mesh (the network panel).
#[tauri::command]
async fn mesh_local() -> MeshLocal {
    tauri::async_runtime::spawn_blocking(mesh::status)
        .await
        .unwrap_or_default()
}

/// Vercelify servers among this computer's mesh peers.
#[tauri::command]
async fn discover_servers(state: State<'_, AppState>) -> CmdResult<Vec<DiscoveredServer>> {
    let local = tauri::async_runtime::spawn_blocking(mesh::status)
        .await
        .map_err(|e| e.to_string())?;
    if !local.running {
        return Err(if local.installed {
            "Vercelify's network isn't running on this computer yet: connect to a server below".into()
        } else {
            "This computer isn't on a Vercelify network yet: connect to a server below".into()
        });
    }
    let client = if local.userspace { &state.mesh_http } else { &state.http };
    Ok(discover::discover(client, local).await)
}

// --- remote APIs ------------------------------------------------------------

#[tauri::command]
async fn agent_request(state: State<'_, AppState>, request: http::Request, server: Option<String>) -> CmdResult<Value> {
    let s = self::server(&state, server.as_deref())?;
    http::send(
        state.client_for(&s.agent_url),
        &s.agent_url,
        Some(&s.agent_token),
        request,
    )
    .await
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
    let result = http::send_lines(
        state.client_for(&s.agent_url),
        &s.agent_url,
        Some(&s.agent_token),
        request,
        rx,
        |line| on_line.send(line).is_ok(),
    )
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
        state.client_for(&settings.agent_url),
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
    Ok(serde_json::json!({ "hostname": health["host"]["hostname"] }))
}
// --- this machine -----------------------------------------------------------

#[tauri::command]
async fn local_system(state: State<'_, AppState>) -> CmdResult<SystemSnapshot> {
    Ok(state.collector.snapshot(None))
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
                mesh_http: http::mesh_client(),
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
            pair_join,
            mesh_discover,
            mesh_pair,
            mesh_local,
            discover_servers,
            agent_request,
            agent_stream,
            stream_cancel,
            test_connection,
            local_system,
            tools_status,
            capture_screens,
            sender_start,
            sender_stop,
            sender_status,
            moonlight_start,
            moonlight_stop,
            moonlight_status,
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

#[cfg(all(test, unix))]
mod settings_tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn saving_credentials_replaces_a_public_file_with_a_private_one() {
        let dir = std::env::temp_dir().join(format!("vfy-settings-perms-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, "secret-token").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(mode, 0o600, "saved agent credentials must be private");
    }

    #[test]
    fn saving_credentials_replaces_symlinks_without_overwriting_the_target() {
        let dir = std::env::temp_dir().join(format!("vfy-settings-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("other-file");
        let path = dir.join("settings.json");
        std::fs::write(&target, "keep me").unwrap();
        symlink(&target, &path).unwrap();
        write_private(&path, "new settings").unwrap();
        let original = std::fs::read_to_string(&target).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(original, "keep me");
        assert_eq!(saved, "new settings");
    }
}

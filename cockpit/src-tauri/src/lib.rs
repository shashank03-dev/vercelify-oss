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
use vercelify_core::stream::{moonlight_args, moonlight_pair_args, sender_args, MoonlightLaunch, SenderConfig};
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
    /// Said once: the settings file couldn't be read and was put aside.
    config_notice: Mutex<Option<String>>,
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

fn read_config(app: &AppHandle) -> (CockpitConfig, Option<String>) {
    match settings_path(app) {
        Ok(p) => load_config_file(&p),
        Err(_) => (CockpitConfig::default(), None),
    }
}

/// The saved servers. A file that can't be read is moved aside (never
/// overwritten by the next save, which would lose every pairing), and the
/// note says where it went.
fn load_config_file(path: &std::path::Path) -> (CockpitConfig, Option<String>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return (CockpitConfig::default(), None);
    };
    if raw.trim().is_empty() {
        return (CockpitConfig::default(), None);
    }
    if let Some(c) = CockpitConfig::from_json(&raw) {
        return (c, None);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let aside = path.with_extension(format!("json.corrupt-{stamp}"));
    let note = match std::fs::rename(path, &aside) {
        Ok(()) => format!(
            "This app's saved servers couldn't be read, so it starts fresh: pair again. The old file is kept as {}.",
            aside.display()
        ),
        Err(e) => format!("This app's saved servers couldn't be read ({e}): pair again."),
    };
    tracing::warn!("{note}");
    (CockpitConfig::default(), Some(note))
}

/// The note about an unreadable settings file, once.
#[tauri::command]
fn config_notice(state: State<'_, AppState>) -> Option<String> {
    state.config_notice.lock().ok()?.take()
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
    // A server found on the mesh (10.77.x) is reached through the mesh
    // daemon's proxy in app mode, like every other request to it.
    let v = http::send(
        state.client_for(&agent_url),
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
            // The code is spent now: a tunnel that's slow to come up is no
            // reason to lose it. The app saves the server and keeps trying.
            if let Err(e) = mesh::wait_connected(std::time::Duration::from_secs(30)) {
                eprintln!("pair_join: joined, tunnel not up yet: {e}");
            }
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
    }
    Ok(p)
}

/// A pairing code `install-mac.sh --pair CODE` left for the app: read once
/// and removed, so the first start pairs by itself. Older than a day (the
/// code's own lifetime) it is dropped.
#[tauri::command]
fn pair_pending(app: AppHandle) -> CmdResult<Option<String>> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(take_pending(&dir.join("pending-pairing")))
}

fn take_pending(path: &std::path::Path) -> Option<String> {
    let fresh = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < std::time::Duration::from_secs(24 * 3600));
    let code = std::fs::read_to_string(path).ok();
    let _ = std::fs::remove_file(path);
    let code = code?.trim().to_string();
    (fresh && pairing::decode(&code).is_ok()).then_some(code)
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

/// Start this computer's network daemon (the network panel's button).
#[tauri::command]
async fn mesh_start() -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(mesh::start)
        .await
        .map_err(|e| e.to_string())?
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
// --- releases ---------------------------------------------------------------

/// Where releases are published (the installers' `VERCELIFY_REPO`).
const RELEASES_REPO: &str = match option_env!("VERCELIFY_REPO") {
    Some(r) => r,
    None => "shashank03-dev/vercelify-oss",
};

/// The newest published release, which `vercelify update` and install-mac.sh install.
#[derive(Debug, PartialEq, serde::Serialize)]
struct LatestRelease {
    version: String,
    url: String,
    /// This app's version.
    current: &'static str,
}

fn parse_release(v: &Value) -> Option<LatestRelease> {
    let version = v["tag_name"].as_str()?.trim_start_matches('v');
    let url = v["html_url"].as_str()?;
    (!version.is_empty() && url.starts_with("https://")).then(|| LatestRelease {
        version: version.into(),
        url: url.into(),
        current: env!("CARGO_PKG_VERSION"),
    })
}

#[tauri::command]
async fn latest_release(state: State<'_, AppState>) -> CmdResult<LatestRelease> {
    let res = state
        .http
        .get(format!("https://api.github.com/repos/{RELEASES_REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("checking for updates: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("checking for updates: GitHub answered {}", res.status()));
    }
    let v: Value = res.json().await.map_err(|e| format!("checking for updates: {e}"))?;
    parse_release(&v).ok_or_else(|| "checking for updates: unexpected answer from GitHub".into())
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

/// A server on the mesh while this computer's daemon runs in app mode has no
/// route here: hand the daemon forwards for `owner` and wait until they
/// listen. Whether it did (false: the host is reachable as it is).
fn forward_to(owner: &str, host: &str, ports: &[(bool, u16, u16)]) -> CmdResult<bool> {
    if !mesh::needs_forward(&format!("http://{host}")) {
        mesh::set_forwards(owner, &[]).map_err(|e| e.to_string())?;
        return Ok(false);
    }
    let lines: Vec<String> = ports
        .iter()
        .map(|&(udp, local, remote)| format!("{}127.0.0.1:{local}={host}:{remote}", if udp { "udp:" } else { "" }))
        .collect();
    mesh::set_forwards(owner, &lines).map_err(|e| format!("telling vercelify-mesh about the forward: {e}"))?;
    let local: Vec<(bool, u16)> = ports.iter().map(|&(udp, l, _)| (udp, l)).collect();
    mesh::wait_forwarded(&local, std::time::Duration::from_secs(5))?;
    Ok(true)
}

#[tauri::command]
async fn sender_start(state: State<'_, AppState>, mut config: SenderConfig) -> CmdResult<ProcessStatus> {
    sender_args(&config).map_err(|e| e.to_string())?;
    let bin = local::ffmpeg().ok_or("ffmpeg is not installed — on macOS: brew install ffmpeg")?;
    local::screen_recording_allowed()?;
    // The stream (UDP, or SRT over UDP) to a server on the mesh in app mode
    // goes through a forward: 127.0.0.1:<port + 10000>.
    let url = config.url.clone();
    let forwarded = tauri::async_runtime::spawn_blocking(move || -> CmdResult<Option<String>> {
        let Some((target, _)) = mesh::via_forward(&url, 0) else {
            return Ok(None);
        };
        let (host, port) = target.rsplit_once(':').ok_or("bad stream address")?;
        let port: u16 = port.parse().map_err(|_| "bad stream port")?;
        let local = port.checked_add(10_000).ok_or("bad stream port")?;
        Ok(forward_to("sender", host, &[(true, local, port)])?
            .then(|| mesh::via_forward(&url, local).map(|(_, u)| u))
            .flatten())
    })
    .await
    .map_err(|e| e.to_string())??;
    if let Some(u) = forwarded {
        config.url = u;
    }
    let args = sender_args(&config).map_err(|e| e.to_string())?;
    state.procs.start(SENDER, &bin.to_string_lossy(), &args, true)
}

#[tauri::command]
fn sender_stop(state: State<'_, AppState>) -> ProcessStatus {
    state.procs.stop(SENDER);
    let _ = mesh::set_forwards("sender", &[]);
    state.procs.status(SENDER)
}

#[tauri::command]
fn sender_status(state: State<'_, AppState>) -> ProcessStatus {
    state.procs.status(SENDER)
}

/// Moonlight reaches Sunshine on fixed ports: forward each one, same number.
/// True when they are forwarded (Moonlight then connects to 127.0.0.1).
async fn forward_sunshine(host: &str) -> CmdResult<bool> {
    let host = host.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let ports: Vec<(bool, u16, u16)> = mesh::SUNSHINE_PORTS.iter().map(|&(udp, p)| (udp, p, p)).collect();
        forward_to("moonlight", &host, &ports)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn moonlight_start(state: State<'_, AppState>, mut launch: MoonlightLaunch) -> CmdResult<ProcessStatus> {
    moonlight_args(&launch).map_err(|e| e.to_string())?;
    let bin = local::moonlight().ok_or("Moonlight is not installed — get it from moonlight-stream.org")?;
    if forward_sunshine(&launch.host).await? {
        launch.host = "127.0.0.1".into();
    }
    let args = moonlight_args(&launch).map_err(|e| e.to_string())?;
    // Moonlight is a visible window the user can close; let it outlive us.
    state.procs.start(MOONLIGHT, &bin.to_string_lossy(), &args, false)
}

/// `moonlight pair` with a PIN the cockpit chose; the agent hands the same
/// PIN to Sunshine.
#[tauri::command]
async fn moonlight_pair(state: State<'_, AppState>, host: String, pin: String) -> CmdResult<ProcessStatus> {
    moonlight_pair_args(&host, &pin).map_err(|e| e.to_string())?;
    let bin = local::moonlight().ok_or("Moonlight is not installed — get it from moonlight-stream.org")?;
    let target = if forward_sunshine(&host).await? {
        "127.0.0.1".to_string()
    } else {
        host
    };
    let args = moonlight_pair_args(&target, &pin).map_err(|e| e.to_string())?;
    state.procs.start(MOONLIGHT, &bin.to_string_lossy(), &args, false)
}

/// Establish the same private forwards used by Moonlight before opening setup.
#[tauri::command]
async fn moonlight_setup(host: String) -> CmdResult<String> {
    moonlight_args(&MoonlightLaunch {
        host: host.clone(),
        app: "Desktop".into(),
        resolution: None,
        fps: None,
        bitrate_kbps: None,
    })
    .map_err(|e| e.to_string())?;
    let forwarded = forward_sunshine(&host).await?;
    let address = if forwarded {
        "127.0.0.1".into()
    } else if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host
    };
    Ok(format!("https://{address}:47990"))
}

#[tauri::command]
fn moonlight_stop(state: State<'_, AppState>) -> ProcessStatus {
    state.procs.stop(MOONLIGHT);
    let _ = mesh::set_forwards("moonlight", &[]);
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
            let (config, notice) = read_config(app.handle());
            app.manage(AppState {
                config: Mutex::new(config),
                config_notice: Mutex::new(notice),
                streams: Mutex::new(HashMap::new()),
                http: http::client(),
                mesh_http: http::mesh_client(),
                procs: Processes::default(),
                collector: Collector::new(SysPaths::default(), env!("CARGO_PKG_VERSION")),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            config_notice,
            mesh_start,
            settings_load,
            settings_save,
            settings_clear,
            servers_load,
            server_select,
            server_remove,
            pair_decode,
            pair_claim,
            pair_join,
            pair_pending,
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
            moonlight_pair,
            moonlight_setup,
            moonlight_stop,
            moonlight_status,
            tray_status,
            latest_release,
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

#[cfg(test)]
mod release_tests {
    use super::*;

    #[test]
    fn a_release_is_its_version_without_the_v_and_its_page() {
        let v = serde_json::json!({ "tag_name": "v0.3.0", "html_url": "https://github.com/o/r/releases/tag/v0.3.0" });
        assert_eq!(
            parse_release(&v),
            Some(LatestRelease {
                version: "0.3.0".into(),
                url: "https://github.com/o/r/releases/tag/v0.3.0".into(),
                current: env!("CARGO_PKG_VERSION"),
            })
        );
    }

    #[test]
    fn anything_else_is_not_a_release() {
        for v in [
            serde_json::json!({ "message": "Not Found" }),
            serde_json::json!({ "tag_name": "v", "html_url": "https://github.com/o/r" }),
            serde_json::json!({ "tag_name": "v1.0.0", "html_url": "javascript:alert(1)" }),
        ] {
            assert_eq!(parse_release(&v), None, "{v}");
        }
    }
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

#[cfg(test)]
mod tests {
    use super::{load_config_file, take_pending};

    #[test]
    fn an_unreadable_settings_file_is_kept_aside_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(load_config_file(&path).1.is_none(), "no file: a first start");
        std::fs::write(&path, "{ \"servers\": [ truncated").unwrap();
        let (c, note) = load_config_file(&path);
        assert!(c.servers.is_empty());
        let note = note.expect("the user is told");
        assert!(note.contains("pair again") && note.contains(".corrupt-"), "{note}");
        assert!(!path.exists(), "moved aside, so a save can't overwrite it");
        let kept: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(
            std::fs::read_to_string(kept[0].path()).unwrap(),
            "{ \"servers\": [ truncated"
        );
    }

    /// Every request to an agent picks its client by URL: a server on the
    /// mesh is reachable only through the mesh daemon's proxy in app mode
    /// (the short code once went direct and could never pair, F76).
    #[test]
    fn agent_requests_choose_their_client_by_url() {
        let src = include_str!("lib.rs");
        // Split so this test doesn't find itself.
        let needle = concat!("http::", "send(");
        for (i, call) in src.split(needle).skip(1).enumerate() {
            let first = call.trim_start().split(',').next().unwrap_or("");
            assert!(
                first.starts_with("state.client_for("),
                "http::send call {i} uses `{first}`"
            );
        }
    }

    #[test]
    fn a_pending_code_is_taken_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending-pairing");
        let code = include_str!("../../../crates/vercelify-core/tests/fixtures/pairing-code.txt").trim();
        std::fs::write(&path, format!("{code}\n")).unwrap();
        assert_eq!(take_pending(&path).as_deref(), Some(code));
        assert!(!path.exists());
        assert_eq!(take_pending(&path), None);
        std::fs::write(&path, "not a code").unwrap();
        assert_eq!(take_pending(&path), None);
        assert!(!path.exists());
    }
}

//! This computer on Vercelify's own network (design §13.3, §13.5): the app
//! drives the `vercelify-mesh` binary that `install-mac.sh` put next to it
//! (a LaunchAgent runs it in app mode) to find servers on the LAN, pair by
//! short code and join; and it reads the daemon's status file for the
//! network panel. In app mode servers are reached through the daemon's
//! SOCKS proxy (`client_for` in lib.rs).

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vercelify_core::{LanServer, MeshLocal, MeshPeer};

/// The daemon's SOCKS proxy in app mode.
pub const SOCKS: &str = "socks5h://127.0.0.1:7480";

/// Where the app-mode daemon keeps its key, state and status.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("VERCELIFY_MESH_HOME") {
        return h.into();
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Vercelify")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("vercelify")
    }
}

/// The installed `vercelify-mesh`.
pub fn bin() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![];
    if let Some(b) = std::env::var_os("VERCELIFY_MESH_BIN") {
        candidates.push(b.into());
    }
    candidates.push(home().join("bin/vercelify-mesh"));
    candidates.push("/usr/local/bin/vercelify-mesh".into());
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join("vercelify-mesh")));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// A host on the mesh: `10.77.x.x`, `fd76:6679:6d65::…` or `*.vfy`.
pub fn on_mesh(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = if let Some(r) = rest.strip_prefix('[') {
        r.split(']').next().unwrap_or("")
    } else {
        rest.split(['/', ':']).next().unwrap_or("")
    };
    host.starts_with("10.77.") || host.to_ascii_lowercase().starts_with("fd76:6679:6d65:") || host.ends_with(".vfy")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The daemon's view, from its status file.
pub fn status_from(installed: bool, raw: Option<&str>, now: u64) -> MeshLocal {
    let v: Value = raw.and_then(|r| serde_json::from_str(r).ok()).unwrap_or(Value::Null);
    let fresh = v["updated_at"].as_u64().is_some_and(|t| now.saturating_sub(t) < 30);
    MeshLocal {
        installed,
        running: fresh,
        userspace: v["interface"].as_str() == Some("userspace"),
        name: v["name"].as_str().map(String::from),
        ipv4: v["ipv4"].as_str().map(String::from),
        revoked: v["revoked"].as_bool().unwrap_or(false),
        coordinator_seen_secs: v["coordinator_seen_secs"].as_u64(),
        peers: v["peers"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| serde_json::from_value::<MeshPeer>(p.clone()).ok())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub fn status() -> MeshLocal {
    let raw = std::fs::read_to_string(home().join("run/mesh-status.json")).ok();
    status_from(bin().is_some(), raw.as_deref(), now())
}

fn run(args: &[&str], timeout: Duration) -> Result<String, String> {
    let bin = bin().ok_or("vercelify-mesh isn't installed on this computer — run install-mac.sh again")?;
    let mut cmd = Command::new(&bin);
    cmd.arg("--dir").arg(home().join("mesh")).args(args);
    let child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", bin.display()))?;
    let id = child.id();
    // A hung child (no network) must not hang the app: stop it after
    // `timeout`, unless it ended (then `done` arrives first).
    let (done, finished) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if finished.recv_timeout(timeout) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
            stop(id);
        }
    });
    let out = child.wait_with_output().map_err(|e| e.to_string());
    let _ = done.send(());
    let out = out?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("failed");
        Err(line.trim_start_matches("vercelify-mesh: ").to_string())
    }
}

#[cfg(unix)]
fn stop(pid: u32) {
    let _ = Command::new("kill").arg(pid.to_string()).status();
}

#[cfg(not(unix))]
fn stop(_pid: u32) {}

/// Servers answering mDNS on this network.
pub fn discover() -> Result<Vec<LanServer>, String> {
    let out = run(&["discover", "--json", "--wait", "2"], Duration::from_secs(10))?;
    let v: Vec<Value> = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    Ok(v.iter()
        .map(|s| LanServer {
            name: s["name"].as_str().unwrap_or("").to_string(),
            control: s["control"]
                .as_array()
                .map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            pairing_open: s["pairing_open"].as_bool().unwrap_or(false),
        })
        .collect())
}

fn device_name() -> String {
    let raw = Command::new("scutil")
        .args(["--get", "LocalHostName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_else(|| "mac".into());
    let n: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let n = n.trim_matches('-').chars().take(40).collect::<String>();
    if n.is_empty() {
        "mac".into()
    } else {
        n
    }
}

/// Trade a short code for the full pairing code and join with it.
pub fn pair(code: &str, server: Option<&str>) -> Result<String, String> {
    let name = device_name();
    let mut args = vec!["pair", code, "--join", "--name", &name];
    if let Some(s) = server {
        args.extend(["--server", s]);
    }
    run(&args, Duration::from_secs(40))
}

/// Join with the token of a pasted `vfy2.` code.
pub fn join(token: &str) -> Result<(), String> {
    let name = device_name();
    run(
        &["join", token, "--name", &name, "--port", "0"],
        Duration::from_secs(30),
    )
    .map(|_| ())
}

/// After joining: wait until the daemon has a tunnel to the coordinator,
/// so the first request doesn't fail.
pub fn wait_connected(timeout: Duration) -> Result<(), String> {
    let until = std::time::Instant::now() + timeout;
    loop {
        let s = status();
        if s.peers.iter().any(|p| p.handshake_secs.is_some()) {
            return Ok(());
        }
        if std::time::Instant::now() > until {
            return Err(if s.running {
                "joined, but no tunnel to the server yet — check that this Mac and the server share a network, then try again".into()
            } else {
                "joined, but vercelify-mesh isn't running (launchctl kickstart gui/$UID/dev.vercelify.mesh)".into()
            });
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_hosts() {
        assert!(on_mesh("http://10.77.0.1:7420"));
        assert!(on_mesh("http://main.vfy:7420/v1"));
        assert!(on_mesh("http://[fd76:6679:6d65::1]:7420"));
        assert!(!on_mesh("http://100.101.102.103:7420"));
        assert!(!on_mesh("http://old-thinkpad.tail1234.ts.net:7420"));
        assert!(!on_mesh("http://10.7.0.1:7420"));
    }

    #[test]
    fn status_file() {
        let raw = r#"{"name":"mac","ipv4":"10.77.0.5","interface":"userspace","revoked":false,
          "coordinator_seen_secs":3,"updated_at":1000,
          "peers":[{"name":"main","kind":"server","addresses":["10.77.0.1"],"path":"lan",
                    "endpoint":"192.168.1.20:51820","rtt_ms":1.2,"handshake_secs":4,"rx_bytes":1,"tx_bytes":2}]}"#;
        let s = status_from(true, Some(raw), 1010);
        assert!(s.running && s.userspace && s.installed);
        assert_eq!(s.ipv4.as_deref(), Some("10.77.0.5"));
        assert_eq!(s.peers[0].path, "lan");
        assert_eq!(s.peers[0].rtt_ms, Some(1.2));
        assert!(
            !status_from(true, Some(raw), 2000).running,
            "a stale file isn't a running daemon"
        );
        let none = status_from(false, None, 0);
        assert!(!none.installed && !none.running && none.peers.is_empty());
    }
}

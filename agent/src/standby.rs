//! The warm standby, the agent's side (design §8.4).
//!
//! - On the main server: every 5 minutes, a sealed snapshot to the chosen
//!   worker (`POST /v1/runtime/standby`), and a watch for having been
//!   replaced (a promoted standby answering with our coordinator key at a
//!   newer epoch), after which this server stops being the main server.
//! - On the standby: the snapshot is checked (only the coordinator can have
//!   sealed it) and kept sealed; `vercelify-agent promote` opens it and
//!   takes over.

use crate::config::Config;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use vercelify_core::platform::StandbyStatus;
use vercelify_mesh::keys::{NodeKey, PublicKey};
use vercelify_mesh::seal::{CoordinatorSession, NodeSession, Sealed};
use vercelify_mesh::state::{key_path, write_private, State as NodeState};
use vercelify_platform::standby::{self, Snapshot};
use vercelify_platform::{mesh_registry, nodes, Store};

pub fn dir(cfg: &Config) -> PathBuf {
    cfg.state_dir.join("standby")
}

/// Marker of a main server that was replaced by a promoted standby.
pub fn superseded_file(cfg: &Config) -> PathBuf {
    cfg.mesh_dir.join("superseded.json")
}

pub fn superseded(cfg: &Config) -> Option<String> {
    let raw = std::fs::read_to_string(superseded_file(cfg)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(format!(
        "this server was replaced as the main server by {} (epoch {}): sudo vercelify demote, then join it as a worker",
        v["by"].as_str().unwrap_or("the standby"),
        v["epoch"].as_u64().unwrap_or(0)
    ))
}

/// Meta of the snapshot a standby holds (no secrets).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Held {
    pub taken_at: i64,
    pub from: String,
    pub epoch: u64,
    pub bytes: u64,
    pub received_at: i64,
}

fn files(cfg: &Config) -> Vec<(&'static str, PathBuf, bool)> {
    let etc = cfg
        .token_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| "/etc/vercelify".into());
    vec![
        ("master.key", cfg.master_key_file.clone(), true),
        ("mesh/coordinator.key", cfg.mesh_dir.join("coordinator.key"), true),
        ("mesh/node.key", key_path(&cfg.mesh_dir), true),
        ("tokens.json", cfg.tokens_file.clone(), false),
        ("agent.token", cfg.token_file.clone(), false),
        ("cloudflare.env", etc.join("cloudflare.env"), false),
        ("cloudflared.env", etc.join("cloudflared.env"), false),
        ("agent.toml", etc.join("agent.toml"), false),
    ]
}

/// Build, seal and send one snapshot. Returns its size.
pub async fn send(p: &crate::platform::Platform, cfg: &Config) -> Result<Option<u64>, String> {
    let status = p.store.with(standby::status).map_err(|e| e.to_string())?;
    let Some(node_id) = status.node_id.clone() else {
        return Ok(None);
    };
    let (node, pubkey) = p
        .store
        .with(|c| {
            let n = nodes::get(c, &node_id)?;
            let mn: Option<String> =
                c.query_row("SELECT mesh_node_id FROM nodes WHERE id = ?1", [&n.id], |r| r.get(0))?;
            let pk = match mn {
                Some(m) => Some(mesh_registry::get(c, &m)?.public_key),
                None => None,
            };
            Ok((n, pk))
        })
        .map_err(|e| e.to_string())?;
    let pubkey = pubkey.ok_or_else(|| format!("{} isn't on the mesh (revoked?)", node.name))?;
    let to = PublicKey::from_base64(&pubkey).map_err(|e| e.to_string())?;
    let own = NodeKey::load_or_create(&key_path(&cfg.mesh_dir)).map_err(|e| e.to_string())?;
    let store = p.store.clone();
    let list = files(cfg);
    let from = nodes_main_name(&store);
    let sealed = tokio::task::spawn_blocking(move || -> Result<(Sealed, u64), String> {
        let refs: Vec<(&str, &Path, bool)> = list.iter().map(|(n, p, r)| (*n, p.as_path(), *r)).collect();
        let snap = standby::build(&store, &from, &refs).map_err(|e| e.to_string())?;
        let plain = zeroize::Zeroizing::new(serde_json::to_vec(&snap).map_err(|e| e.to_string())?);
        let session = NodeSession::new(&own, &to).map_err(|e| e.to_string())?;
        Ok((session.seal_request(&plain), plain.len() as u64))
    })
    .await
    .map_err(|e| e.to_string())??;
    let rt = p.engine.node_runtime(&node).map_err(|e| e.to_string())?;
    let vercelify_platform::runtime::NodeRuntime::Remote(r) = rt else {
        return Err("the standby is this server".into());
    };
    r.post::<serde_json::Value>("/v1/runtime/standby", &sealed.0)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Some(sealed.1))
}

fn nodes_main_name(store: &Store) -> String {
    store
        .with(nodes::main)
        .map(|n| n.name)
        .unwrap_or_else(|_| "main".into())
}

/// Every 5 minutes while a standby is chosen; the outcome is the status.
pub async fn run(p: Arc<crate::platform::Platform>, cfg: Arc<Config>) {
    loop {
        once(&p, &cfg).await;
        tokio::time::sleep(Duration::from_secs(300)).await;
    }
}

pub async fn once(p: &crate::platform::Platform, cfg: &Config) {
    let res = send(p, cfg).await;
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let _ = p.store.tx(|c| {
        let mut s: StandbyStatus = standby::status(c)?;
        if s.node_id.is_none() {
            return Ok(());
        }
        match &res {
            Ok(Some(bytes)) => {
                s.last_sent_at = Some(now.clone());
                s.bytes = *bytes;
                s.last_error = None;
            }
            Ok(None) => {}
            Err(e) => s.last_error = Some(e.clone()),
        }
        standby::set_status(c, &s)
    });
    if let Err(e) = res {
        tracing::warn!("standby copy: {e}");
    }
}

/// On the standby: check a snapshot came from the coordinator and keep it.
pub fn receive(cfg: &Config, sealed: &Sealed) -> Result<Held, String> {
    let st = NodeState::load(&cfg.mesh_dir)
        .map_err(|e| e.to_string())?
        .ok_or("this server isn't on the mesh")?;
    if sealed.public_key != st.coordinator.public_key {
        return Err("not sealed by the coordinator".into());
    }
    let own = NodeKey::load_or_create(&key_path(&cfg.mesh_dir)).map_err(|e| e.to_string())?;
    let (_, plain) =
        CoordinatorSession::open(&own, sealed).map_err(|_| "the snapshot doesn't open with this server's key")?;
    let plain = zeroize::Zeroizing::new(plain);
    let snap: Snapshot = serde_json::from_slice(&plain).map_err(|e| format!("not a snapshot: {e}"))?;
    let held = Held {
        taken_at: snap.taken_at,
        from: snap.from.clone(),
        epoch: snap.epoch,
        bytes: plain.len() as u64,
        received_at: chrono::Utc::now().timestamp(),
    };
    let d = dir(cfg);
    write_private(
        &d.join("snapshot.sealed"),
        &serde_json::to_vec(sealed).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    write_private(
        &d.join("snapshot.json"),
        &serde_json::to_vec(&held).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(held)
}

pub fn held(cfg: &Config) -> Option<Held> {
    serde_json::from_slice(&std::fs::read(dir(cfg).join("snapshot.json")).ok()?).ok()
}

fn put(path: &Path, data: &[u8], keep_old: bool) -> Result<(), String> {
    if keep_old && path.exists() {
        let mut old = path.as_os_str().to_owned();
        old.push(".before-promote");
        std::fs::rename(path, PathBuf::from(old)).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    write_private(path, data).map_err(|e| format!("{}: {e}", path.display()))
}

/// Take over as the main server from the snapshot this standby holds.
/// The host's `vercelify promote` stops the agent and the mesh daemon
/// first and starts them again as the main server after.
pub async fn promote(cfg: &Config, force: bool) -> Result<String, String> {
    if cfg.role() != "worker" {
        return Err("this is the main server already (promote runs on its standby)".into());
    }
    let st = NodeState::load(&cfg.mesh_dir)
        .map_err(|e| e.to_string())?
        .ok_or("this server isn't on the mesh")?;
    let raw = std::fs::read(dir(cfg).join("snapshot.sealed"))
        .map_err(|_| "no copy of the main server here: choose this server as the standby first (sudo vercelify standby NAME on the main server)")?;
    let sealed: Sealed = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if sealed.public_key != st.coordinator.public_key {
        return Err("the copy here wasn't sealed by this mesh's coordinator".into());
    }
    let own = NodeKey::load_or_create(&key_path(&cfg.mesh_dir)).map_err(|e| e.to_string())?;
    let (_, plain) =
        CoordinatorSession::open(&own, &sealed).map_err(|_| "the copy doesn't open with this server's key")?;
    let plain = zeroize::Zeroizing::new(plain);
    let snap: Snapshot = serde_json::from_slice(&plain).map_err(|e| e.to_string())?;
    let age = chrono::Utc::now().timestamp() - snap.taken_at;
    if age > standby::MAX_AGE_SECS && !force {
        return Err(format!(
            "the copy is {} minutes old: changes after it are lost (use --force to promote anyway)",
            age / 60
        ));
    }
    // The main server must be gone: two coordinators would fight.
    if !force {
        let api = st.api_url.trim_start_matches("http://").to_string();
        if tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(&api))
            .await
            .is_ok_and(|r| r.is_ok())
        {
            return Err(format!(
                "the main server still answers at {api}: promote only when it is gone (or --force)"
            ));
        }
    }
    let need = |name: &str| -> Result<zeroize::Zeroizing<Vec<u8>>, String> {
        snap.file(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{name} is missing from the copy"))
    };
    // The database first: everything else is useless without it.
    let db = need("platform.db")?;
    for suffix in ["-wal", "-shm"] {
        let mut p = cfg.platform_db.as_os_str().to_owned();
        p.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(p));
    }
    put(&cfg.platform_db, &db, true)?;
    let epoch = {
        let store = Store::open(&cfg.platform_db).map_err(|e| e.to_string())?;
        store
            .tx(|c| standby::promote_db(c, &st.node_id))
            .map_err(|e| e.to_string())?
    };
    put(&cfg.master_key_file, &need("master.key")?, true)?;
    put(
        &cfg.mesh_dir.join("coordinator.key"),
        &need("mesh/coordinator.key")?,
        true,
    )?;
    put(&key_path(&cfg.mesh_dir), &need("mesh/node.key")?, true)?;
    // The agent writes the coordinator's own mesh state at start.
    let state = vercelify_mesh::state::state_path(&cfg.mesh_dir);
    if state.exists() {
        let _ = std::fs::rename(&state, state.with_extension("json.before-promote"));
    }
    if let Some(t) = snap.file("tokens.json").map_err(|e| e.to_string())? {
        put(&cfg.tokens_file, &t, true)?;
    }
    if let Some(t) = snap.file("agent.token").map_err(|e| e.to_string())? {
        put(&cfg.token_file, &t, true)?;
    }
    // For the host script: the main server's settings and tunnel.
    for (name, out) in [
        ("agent.toml", "main-agent.toml"),
        ("cloudflare.env", "main-cloudflare.env"),
        ("cloudflared.env", "main-cloudflared.env"),
    ] {
        if let Some(b) = snap.file(name).map_err(|e| e.to_string())? {
            put(&dir(cfg).join(out), &b, false)?;
        }
    }
    let note = serde_json::json!({ "from": snap.from, "epoch": epoch, "at": chrono::Utc::now().timestamp(), "copy_age_secs": age });
    put(&dir(cfg).join("promoted.json"), note.to_string().as_bytes(), false)?;
    Ok(format!(
        "this server is now the main server in {}'s place (map epoch {epoch}; the copy was {} min old)",
        snap.from,
        age / 60
    ))
}

/// On the main server: has a promoted standby taken our place? It answers
/// on its control port with our coordinator key at a newer epoch.
pub async fn check_replaced(cfg: &Config, co: &crate::mesh::Coordinator) -> Option<(String, u64)> {
    let (standby_node, epoch) = co
        .store
        .with(|c| Ok((standby::status(c)?.node_id, mesh_registry::versions(c)?.1)))
        .ok()?;
    let node_id = standby_node?;
    let (name, endpoints) = co
        .store
        .with(|c| {
            let n = nodes::get(c, &node_id)?;
            let mn: Option<String> =
                c.query_row("SELECT mesh_node_id FROM nodes WHERE id = ?1", [&n.id], |r| r.get(0))?;
            let eps = match mn {
                Some(m) => mesh_registry::get(c, &m)?.endpoints,
                None => vec![],
            };
            Ok((n.name, eps))
        })
        .ok()?;
    let port = cfg
        .mesh_control_listen
        .rsplit_once(':')
        .map(|(_, p)| p.to_string())
        .unwrap_or_else(|| "7443".into());
    let own = co.wg_public().to_base64();
    for ep in endpoints {
        let Some((host, _)) = ep.rsplit_once(':') else { continue };
        let url = format!("http://{host}:{port}/v1/mesh/hello");
        let Ok(Ok((code, body))) = tokio::time::timeout(
            Duration::from_secs(4),
            crate::http::call(crate::http::Req::get(&url).timeout(Duration::from_secs(4))),
        )
        .await
        else {
            continue;
        };
        if !code.is_success() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        if v["coordinator_wg"].as_str() == Some(own.as_str()) && v["epoch"].as_u64().is_some_and(|e| e > epoch) {
            return Some((name, v["epoch"].as_u64().unwrap_or(0)));
        }
    }
    None
}

/// Give up being the main server: remember it, stop the mesh daemon (two
/// daemons with one key would fight) and exit; systemd starts the agent
/// again, which then runs neither the coordinator nor the platform.
pub async fn step_down(cfg: &Config, by: &str, epoch: u64) -> ! {
    let note = serde_json::json!({ "by": by, "epoch": epoch, "at": chrono::Utc::now().timestamp() });
    let _ = write_private(&superseded_file(cfg), note.to_string().as_bytes());
    tracing::error!("{by} was promoted to main server (epoch {epoch}): this server steps down");
    let _ = tokio::process::Command::new(&cfg.systemctl_bin)
        .args(["stop", "vercelify-mesh"])
        .status()
        .await;
    std::process::exit(3)
}

pub async fn watch_replaced(cfg: Arc<Config>, co: Arc<crate::mesh::Coordinator>) {
    loop {
        if let Some((by, epoch)) = check_replaced(&cfg, &co).await {
            step_down(&cfg, &by, epoch).await;
        }
        tokio::time::sleep(Duration::from_secs(120)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superseded_marker() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            mesh_dir: dir.path().into(),
            ..Config::default()
        };
        assert!(superseded(&cfg).is_none());
        std::fs::write(superseded_file(&cfg), r#"{"by":"pi","epoch":2}"#).unwrap();
        let m = superseded(&cfg).unwrap();
        assert!(m.contains("replaced as the main server by pi (epoch 2)"), "{m}");
    }
}

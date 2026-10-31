//! The `vercelify-mesh` daemon (design §13): joining a mesh, then keeping
//! the node on it. `up` creates `vfy0`, runs WireGuard, follows the signed
//! map from the coordinator (peers, names, ACLs), reports the node's own
//! endpoints, renews its certificate (rotating its key) and writes a status
//! file for the agent and the cockpit. It keeps working on the last map
//! while the coordinator is away.

use crate::cert::{unix_now, NodeCert, NodeKind};
use crate::http::{self, HttpError};
use crate::keys::{NodeKey, PublicKey, VerifyingKey};
use crate::proto::*;
use crate::seal::{NodeSession, Sealed};
use crate::state::{key_path, write_private, State};
use crate::wg::{Control, Engine, PeerSpec, PeerStatus};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("{0}")]
    Msg(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error(transparent)]
    Mesh(#[from] crate::MeshError),
}

type Result<T> = std::result::Result<T, DaemonError>;

fn msg(s: impl Into<String>) -> DaemonError {
    DaemonError::Msg(s.into())
}

/// This machine's OS and CPU, as enrollment reports them.
pub fn platform() -> (String, String) {
    (std::env::consts::OS.to_string(), std::env::consts::ARCH.to_string())
}

/// Join the mesh a token points at: generate (or reuse) this node's key,
/// enroll sealed to the coordinator, check what comes back, save it.
pub async fn join(dir: &Path, token: &str, name: &str, endpoints: Vec<String>) -> Result<State> {
    let t = JoinToken::decode(token)?;
    if !valid_node_name(name) {
        return Err(msg(format!(
            "`{name}` is not a valid node name (lowercase letters, digits, dashes)"
        )));
    }
    let key = NodeKey::load_or_create(&key_path(dir))?;
    let coordinator = PublicKey::from_base64(&t.coordinator_wg)?;
    let session = NodeSession::new(&key, &coordinator)?;
    let (os, arch) = platform();
    let req = EnrollRequest {
        claim: t.claim.clone(),
        name: name.to_string(),
        kind: t.kind,
        os,
        arch,
        endpoints,
        ts: unix_now(),
    };
    let sealed = session.seal_request(&serde_json::to_vec(&req).expect("serializable"));
    let body = serde_json::to_value(&sealed).expect("serializable");
    let mut last = None;
    for control in &t.control {
        let url = control_url(control, "/v1/mesh/enroll");
        // An address that doesn't answer (the LAN, seen from elsewhere) is
        // given up on quickly; a tunnel gets longer for its TLS handshake.
        let connect = Duration::from_secs(if url.starts_with("https://") { 10 } else { 4 });
        match http::request_within("POST", &url, None, Some(&body), connect, Duration::from_secs(20)).await {
            Ok(raw) => {
                let answer: Sealed = serde_json::from_slice(&raw).map_err(|e| msg(format!("enroll answer: {e}")))?;
                let plain = session.open_response(&answer)?;
                let r: EnrollResponse =
                    serde_json::from_slice(&plain).map_err(|e| msg(format!("enroll answer: {e}")))?;
                let state = accept(&key, &r)?;
                state.save(dir)?;
                return Ok(state);
            }
            // A refused claim is final; an unreachable address tries the next.
            Err(e @ HttpError::Status { .. }) => return Err(e.into()),
            Err(e) => last = Some(e),
        }
    }
    Err(last
        .map(DaemonError::from)
        .unwrap_or_else(|| msg("the token names no coordinator")))
}

/// A control address of a join token as a URL: `192.168.1.20:7443` is
/// plain HTTP on the LAN; `https://mesh.example.com` a tunnel.
pub fn control_url(control: &str, path: &str) -> String {
    let c = control.trim_end_matches('/');
    if c.contains("://") {
        format!("{c}{path}")
    } else {
        format!("http://{c}{path}")
    }
}

/// Check an enrollment answer: certificate and map signed by the
/// coordinator key it names, for this node's key.
fn accept(key: &NodeKey, r: &EnrollResponse) -> Result<State> {
    let vk = VerifyingKey::from_base64(&r.coordinator_key)?;
    let cert = NodeCert::verify(&vk, &r.certificate, unix_now())?;
    if cert.public_key != key.public().to_base64() {
        return Err(msg("the coordinator certified another key"));
    }
    let map = PeerMap::verify(&vk, &r.map)?;
    if map.self_node.node_id != r.node_id {
        return Err(msg("the first map is for another node"));
    }
    Ok(State {
        node_id: r.node_id.clone(),
        name: cert.name,
        kind: cert.kind,
        ipv4: r.ipv4.clone(),
        ipv6: r.ipv6.clone(),
        ipv4_prefix_len: r.ipv4_prefix_len,
        certificate: r.certificate.clone(),
        node_token: r.node_token.clone(),
        coordinator_key: r.coordinator_key.clone(),
        coordinator: r.coordinator.clone(),
        api_url: r.api_url.clone(),
        map: Some(r.map.clone()),
    })
}

/// Peers of a map as the engine wants them. The coordinator's endpoints
/// from enrollment fill in while the map has none for it yet.
pub fn peer_specs(map: &PeerMap, coordinator: &CoordinatorInfo) -> Vec<PeerSpec> {
    map.peers
        .iter()
        .filter_map(|p| {
            let public_key = PublicKey::from_base64(&p.public_key).ok()?;
            let mut addresses = vec![];
            if let Ok(ip) = p.ipv4.parse::<IpAddr>() {
                addresses.push(ip);
            }
            if let Ok(ip) = p.ipv6.parse::<IpAddr>() {
                addresses.push(ip);
            }
            let mut endpoints: Vec<SocketAddr> = p.endpoints.iter().filter_map(|e| e.parse().ok()).collect();
            if p.public_key == coordinator.public_key {
                for e in coordinator.endpoints.iter().filter_map(|e| e.parse().ok()) {
                    if !endpoints.contains(&e) {
                        endpoints.push(e);
                    }
                }
            }
            Some(PeerSpec {
                public_key,
                name: p.name.clone(),
                addresses,
                endpoints,
            })
        })
        .collect()
}

/// What the host firewall needs (design §13.8): servers may reach this node
/// on any port; each device only on its listed ports.
pub fn acl_json(map: &PeerMap, interface: &str) -> serde_json::Value {
    let servers: Vec<&str> = map
        .peers
        .iter()
        .filter(|p| p.kind == NodeKind::Server)
        .flat_map(|p| [p.ipv4.as_str(), p.ipv6.as_str()])
        .collect();
    let devices: Vec<serde_json::Value> = map
        .peers
        .iter()
        .filter(|p| p.kind == NodeKind::Device)
        .map(|p| {
            serde_json::json!({
                "name": p.name, "ipv4": p.ipv4, "ipv6": p.ipv6,
                "ports": p.allowed_ports.clone().unwrap_or_default(),
            })
        })
        .collect();
    // What this server's containers may reach on other servers.
    let container: Vec<serde_json::Value> = map
        .peers
        .iter()
        .filter_map(|p| {
            let ports = p.container_ports.as_ref().filter(|c| !c.is_empty())?;
            Some(serde_json::json!({ "name": p.name, "ipv4": p.ipv4, "ipv6": p.ipv6, "ports": ports }))
        })
        .collect();
    serde_json::json!({
        "interface": interface, "version": map.version, "servers": servers, "devices": devices,
        "container": container,
    })
}

/// The path to a peer as the status file shows it: `relay`, `none`, or
/// for a direct one `lan` when the endpoint shares a /24 with one of this
/// node's addresses, else `direct`.
pub fn path_of(engine_path: &str, endpoint: Option<&str>, local: &[Ipv4Addr]) -> &'static str {
    if engine_path != "direct" {
        return if engine_path == "relay" { "relay" } else { "none" };
    }
    let Some(ep) = endpoint.and_then(|e| e.parse::<SocketAddr>().ok()) else {
        return "none";
    };
    match ep.ip() {
        IpAddr::V4(ip) if local.iter().any(|l| l.octets()[..3] == ip.octets()[..3]) => "lan",
        _ => "direct",
    }
}

pub struct UpOptions {
    pub dir: PathBuf,
    pub run_dir: PathBuf,
    pub interface: String,
    pub port: u16,
    pub dns: bool,
    /// Run after the ACL file changes (`vercelify-firewall mesh`).
    pub acl_hook: Option<String>,
    /// STUN servers (`host:port`) to learn the public address from.
    pub stun_servers: Vec<String>,
    /// App mode (the Mac without root): a userspace stack instead of a tun
    /// device, reached through SOCKS5 and forwards.
    pub userspace: Option<Userspace>,
    /// The public drop holding the coordinator's signed address (ntfy.sh),
    /// read after the coordinator has been out of reach for a while.
    pub beacon: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Userspace {
    /// SOCKS5 on loopback (`127.0.0.1:7480`).
    pub socks: Option<SocketAddr>,
    /// The browser gateway for private apps (`127.0.0.1:7481`, `*.localhost`).
    pub gateway: Option<SocketAddr>,
    pub forwards: Vec<crate::socks::Forward>,
}

/// Shared between the daemon's tasks.
struct Live {
    state: RwLock<State>,
    map: RwLock<Option<PeerMap>>,
    zone: Arc<RwLock<Dns>>,
    coordinator_seen: RwLock<Option<Instant>>,
    revoked: RwLock<bool>,
    /// The node's key, for the relay (it reconnects when it changes).
    key: watch::Sender<Arc<NodeKey>>,
    /// Relay URLs from the map.
    relays: watch::Sender<Vec<String>>,
    /// `vfy0`, `utun5`, or `userspace`.
    interface: String,
    /// The WireGuard socket's port.
    port: u16,
}

impl Live {
    fn verifying(&self) -> Result<VerifyingKey> {
        Ok(VerifyingKey::from_base64(&self.state.read().unwrap().coordinator_key)?)
    }

    /// Switch the tunnels (and the relay) to `key`.
    async fn use_key(&self, ctl: &mpsc::Sender<Control>, key: &NodeKey) {
        let _ = ctl.send(Control::Rekey(NodeKey::from_bytes(*key.secret_bytes()))).await;
        self.key
            .send_replace(Arc::new(NodeKey::from_bytes(*key.secret_bytes())));
    }
}

fn apply_map(live: &Live, opts: &UpOptions, ctl: &mpsc::Sender<Control>, map: PeerMap, signed: &str) -> bool {
    {
        let cur = live.map.read().unwrap();
        if !map.supersedes(cur.as_ref()) {
            return false;
        }
    }
    let coordinator = live.state.read().unwrap().coordinator.clone();
    let _ = ctl.try_send(Control::SetPeers(peer_specs(&map, &coordinator)));
    *live.zone.write().unwrap() = map.dns.clone();
    live.relays.send_if_modified(|r| {
        let changed = *r != map.relays;
        r.clone_from(&map.relays);
        changed
    });
    let acl = acl_json(&map, &opts.interface);
    let acl_path = opts.run_dir.join("mesh-acl.json");
    if write_private(
        &acl_path,
        serde_json::to_string_pretty(&acl).unwrap_or_default().as_bytes(),
    )
    .is_ok()
    {
        if let Some(hook) = &opts.acl_hook {
            let hook = hook.clone();
            tokio::spawn(async move {
                match tokio::process::Command::new("sh").arg("-c").arg(&hook).status().await {
                    Ok(s) if s.success() => {}
                    other => tracing::warn!("acl hook `{hook}`: {other:?}"),
                }
            });
        }
    }
    {
        let mut st = live.state.write().unwrap();
        st.map = Some(signed.to_string());
        let _ = st.save(&opts.dir);
    }
    tracing::info!("map {} (epoch {}): {} peers", map.version, map.epoch, map.peers.len());
    *live.map.write().unwrap() = Some(map);
    true
}

/// Out of touch with the coordinator: its address may have moved (a new
/// quick tunnel, a new public IP). Read its signed note and follow it.
async fn check_beacon(live: &Live, base: &str) {
    let Ok(vk) = live.verifying() else { return };
    let url = format!("{base}/{}/json?poll=1&since=12h", crate::beacon::topic(&vk));
    let body = match http::request("GET", &url, None, None, Duration::from_secs(15)).await {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!("beacon: {e}");
            return;
        }
    };
    let Some(b) = crate::beacon::newest(&vk, &String::from_utf8_lossy(&body), unix_now()) else {
        return;
    };
    // The map in hand is as new: nothing moved.
    if live
        .map
        .read()
        .unwrap()
        .as_ref()
        .is_some_and(|m| m.issued_at >= b.issued_at)
    {
        return;
    }
    let moved = live.relays.send_if_modified(|r| {
        let changed = *r != b.relays;
        r.clone_from(&b.relays);
        changed
    });
    if moved {
        tracing::info!("the main server moved: relay at {}", b.relays.join(", "));
    }
}

/// Follow the coordinator's map stream for as long as it lasts.
async fn follow_map(live: &Arc<Live>, opts: &Arc<UpOptions>, ctl: &mpsc::Sender<Control>) -> Result<()> {
    let (url, token) = {
        let s = live.state.read().unwrap();
        (
            format!("{}/v1/mesh/map", s.api_url.trim_end_matches('/')),
            s.node_token.clone(),
        )
    };
    let vk = live.verifying()?;
    let res = http::stream_lines(&url, &token, Duration::from_secs(70), |line| {
        *live.coordinator_seen.write().unwrap() = Some(Instant::now());
        match serde_json::from_str::<MapEvent>(line) {
            Ok(MapEvent::Map { signed }) => match PeerMap::verify(&vk, &signed) {
                Ok(m) => {
                    apply_map(live, opts, ctl, m, &signed);
                }
                Err(e) => tracing::warn!("a map that doesn't verify: {e}"),
            },
            Ok(MapEvent::Ping) => {}
            Ok(MapEvent::Revoked) => {
                tracing::warn!("this node was revoked from the mesh");
                *live.revoked.write().unwrap() = true;
                let _ = ctl.try_send(Control::SetPeers(vec![]));
                return false;
            }
            Err(e) => tracing::debug!("map stream: {e}"),
        }
        true
    })
    .await;
    if *live.revoked.read().unwrap() {
        return Err(msg("revoked"));
    }
    match res {
        Err(HttpError::Status {
            status: 401 | 403,
            message,
        }) => {
            // Revoked: forget every peer (they forget this node too).
            tracing::warn!("the coordinator refused this node: {message}");
            *live.revoked.write().unwrap() = true;
            let _ = ctl.try_send(Control::SetPeers(vec![]));
            Err(msg(message))
        }
        other => Ok(other?),
    }
}

/// Tell the coordinator where this node can be reached: its interface
/// addresses, then what peers and STUN servers saw (its public mapping).
async fn report_endpoints(live: &Live, ctl: &mpsc::Sender<Control>, port: u16, last: &mut Vec<String>) {
    let mesh_prefix = live.state.read().unwrap().ipv4.parse::<Ipv4Addr>().ok();
    let local: Vec<SocketAddr> = crate::net::local_ipv4s(mesh_prefix)
        .into_iter()
        .map(|ip| SocketAddr::new(IpAddr::V4(ip), port))
        .collect();
    let _ = ctl.send(Control::SetLocal(local.clone())).await;
    let (tx, rx) = oneshot::channel();
    let observed = if ctl.send(Control::Observed(tx)).await.is_ok() {
        rx.await.unwrap_or_default()
    } else {
        vec![]
    };
    let mut eps: Vec<String> = local.iter().map(|a| a.to_string()).collect();
    for a in observed {
        let a = a.to_string();
        if !eps.contains(&a) {
            eps.push(a);
        }
    }
    if eps == *last || eps.is_empty() {
        return;
    }
    let (url, token) = {
        let s = live.state.read().unwrap();
        (
            format!("{}/v1/mesh/endpoints", s.api_url.trim_end_matches('/')),
            s.node_token.clone(),
        )
    };
    let body = serde_json::to_value(EndpointsReport { endpoints: eps.clone() }).expect("serializable");
    match http::request("POST", &url, Some(&token), Some(&body), Duration::from_secs(10)).await {
        Ok(_) => *last = eps,
        Err(e) => tracing::debug!("reporting endpoints: {e}"),
    }
}

fn next_key_path(dir: &Path) -> PathBuf {
    dir.join("node.key.next")
}

async fn ask_renew(url: &str, token: &str, key: &NodeKey, vk: &VerifyingKey) -> Result<(String, i64)> {
    let body = serde_json::to_value(RenewRequest {
        public_key: Some(key.public().to_base64()),
    })
    .expect("serializable");
    let raw = http::request("POST", url, Some(token), Some(&body), Duration::from_secs(15)).await?;
    let r: RenewResponse = serde_json::from_slice(&raw).map_err(|e| msg(format!("renew answer: {e}")))?;
    let c = NodeCert::verify(vk, &r.certificate, unix_now())?;
    if c.public_key != key.public().to_base64() {
        return Err(msg("the renewed certificate is for another key"));
    }
    Ok((r.certificate, c.not_after))
}

/// Renew the certificate when due (or `force`), with a new key: keys rotate
/// with their certificates (design §13.2).
///
/// The coordinator may switch to the new key and lose its answer on the
/// way (peers drop the old key as soon as the map changes). So the next key
/// is saved before asking; when the answer doesn't come, the node tries the
/// next key itself and asks again through the mesh with it, and goes back
/// to the old one only if that fails too. Asking twice for the same key is
/// harmless.
async fn renew(live: &Live, opts: &UpOptions, ctl: &mpsc::Sender<Control>, force: bool) -> Result<bool> {
    let vk = live.verifying()?;
    let (cert, url, token) = {
        let s = live.state.read().unwrap();
        (
            s.certificate.clone(),
            format!("{}/v1/mesh/renew", s.api_url.trim_end_matches('/')),
            s.node_token.clone(),
        )
    };
    let now = unix_now();
    let due = match NodeCert::verify(&vk, &cert, now) {
        Ok(c) => c.needs_renewal(now),
        Err(_) => true,
    };
    let pending = next_key_path(&opts.dir);
    if !due && !force && !pending.exists() {
        return Ok(false);
    }
    let next = NodeKey::load_or_create(&pending)?;
    let result = match ask_renew(&url, &token, &next, &vk).await {
        Ok(r) => Ok(r),
        Err(first) => {
            // Maybe the coordinator has the next key already: use it and ask again.
            tracing::info!("no answer to the renewal ({first}); trying the next key");
            live.use_key(ctl, &next).await;
            let mut r = Err(first);
            for _ in 0..4 {
                tokio::time::sleep(Duration::from_secs(2)).await;
                r = ask_renew(&url, &token, &next, &vk).await;
                if r.is_ok() {
                    break;
                }
            }
            if r.is_err() {
                // Not taken: back to the key the mesh knows; the next key
                // stays pending for the next attempt.
                let old = NodeKey::load_or_create(&key_path(&opts.dir))?;
                live.use_key(ctl, &old).await;
            }
            r
        }
    };
    let (certificate, until) = result?;
    next.save(&key_path(&opts.dir))?;
    let _ = std::fs::remove_file(&pending);
    {
        let mut s = live.state.write().unwrap();
        s.certificate = certificate;
        s.save(&opts.dir)?;
    }
    live.use_key(ctl, &next).await;
    tracing::info!("certificate renewed until {until} with a new key");
    Ok(true)
}

async fn write_status(live: &Live, opts: &UpOptions, ctl: &mpsc::Sender<Control>) {
    let (tx, rx) = oneshot::channel::<Vec<PeerStatus>>();
    if ctl.send(Control::Status(tx)).await.is_err() {
        return;
    }
    let Ok(peers) = rx.await else { return };
    let local = crate::net::local_ipv4s(None);
    let st = live.state.read().unwrap().clone();
    let map = live.map.read().unwrap().clone();
    let vk = live.verifying().ok();
    let cert_expires = vk
        .and_then(|k| NodeCert::verify(&k, &st.certificate, unix_now()).ok())
        .map(|c| c.not_after);
    let seen = live.coordinator_seen.read().unwrap().map(|t| t.elapsed().as_secs());
    let peers: Vec<serde_json::Value> = peers
        .iter()
        .map(|p| {
            let kind = map
                .as_ref()
                .and_then(|m| m.peers.iter().find(|x| x.public_key == p.public_key))
                .map(|x| x.kind);
            serde_json::json!({
                "name": p.name, "kind": kind, "addresses": p.addresses, "endpoint": p.endpoint,
                "path": path_of(p.path, p.endpoint.as_deref(), &local),
                "rtt_ms": p.rtt_ms, "handshake_secs": p.handshake_secs, "rx_bytes": p.rx_bytes, "tx_bytes": p.tx_bytes,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "node_id": st.node_id, "name": st.name, "kind": st.kind, "ipv4": st.ipv4, "ipv6": st.ipv6,
        "interface": live.interface, "port": live.port,
        "map_version": map.as_ref().map(|m| m.version), "epoch": map.as_ref().map(|m| m.epoch),
        "coordinator_seen_secs": seen, "revoked": *live.revoked.read().unwrap(),
        "cert_expires_at": cert_expires, "updated_at": unix_now(), "peers": peers,
    });
    let _ = write_private(
        &opts.run_dir.join("mesh-status.json"),
        serde_json::to_string_pretty(&doc).unwrap_or_default().as_bytes(),
    );
}

/// Resolve the STUN servers now and every hour (their addresses move).
async fn resolve_stun(servers: Vec<String>, ctl: mpsc::Sender<Control>) {
    if servers.is_empty() {
        return;
    }
    loop {
        let mut addrs = vec![];
        for s in &servers {
            if let Ok(Ok(found)) =
                tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host(s.as_str())).await
            {
                // One per server, IPv4 first (the socket is IPv4).
                if let Some(a) = found.into_iter().find(|a| a.is_ipv4()) {
                    addrs.push(a);
                }
            }
        }
        if ctl.send(Control::SetStun(addrs)).await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

/// Point `*.vfy` at the mesh name server (systemd-resolved, when present).
async fn configure_resolver(interface: &str, ip: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = interface;
        if let Err(e) = crate::utun::configure_resolver(ip) {
            tracing::warn!("/etc/resolver/vfy: {e}");
        }
    }
    if !Path::new("/run/systemd/resolve").exists() {
        return;
    }
    for args in [vec!["dns", interface, ip], vec!["domain", interface, "~vfy"]] {
        let _ = tokio::process::Command::new("resolvectl")
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }
}

/// Why the daemon should restart: `state.json` now names another node or
/// another coordinator, or is gone. Its own updates (the last map, a new
/// certificate) change neither.
fn identity_changed(running: &State, now: Option<&State>) -> Option<&'static str> {
    match now {
        None => Some("left the mesh"),
        Some(n) if n.node_id != running.node_id => Some("joined as another node"),
        // The same node paired again (F77): the old token no longer works.
        Some(n) if n.node_token != running.node_token => Some("paired again"),
        Some(n)
            if n.coordinator_key != running.coordinator_key
                || n.coordinator.public_key != running.coordinator.public_key =>
        {
            Some("joined another mesh")
        }
        Some(_) => None,
    }
}

fn beacon_due(silence: Option<Duration>, ticks: u64) -> bool {
    silence.is_none_or(|age| age >= Duration::from_secs(30)) && ticks >= 6 && ticks.is_multiple_of(6)
}

/// Run the node on the mesh until a signal ends it.
pub async fn up(opts: UpOptions) -> Result<()> {
    let opts = Arc::new(opts);
    std::fs::create_dir_all(&opts.run_dir)?;
    // Not joined yet (a fresh LaunchAgent on a Mac): wait, and start as soon
    // as the app joins, so pairing doesn't wait on us.
    let mut said = false;
    let state = loop {
        match State::load(&opts.dir)? {
            Some(s) => break s,
            None => {
                if !said {
                    tracing::info!(
                        "not on a mesh yet ({}): waiting for `vercelify-mesh join`",
                        opts.dir.display()
                    );
                    said = true;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
    let key = NodeKey::load_or_create(&key_path(&opts.dir))?;
    let ipv4: Ipv4Addr = state.ipv4.parse().map_err(|_| msg("bad mesh address in state"))?;
    let ipv6: Option<std::net::Ipv6Addr> = state.ipv6.parse().ok();
    let socket = UdpSocket::bind(("0.0.0.0", opts.port)).await?;
    // With `--port 0` (devices) the kernel picked one: that's what peers need.
    let port = socket.local_addr()?.port();
    let coordinator_wg = PublicKey::from_base64(&state.coordinator.public_key)?;
    let (key_tx, key_rx) = watch::channel(Arc::new(NodeKey::from_bytes(*key.secret_bytes())));
    let (relays_tx, relays_rx) = watch::channel(vec![]);
    // The coordinator's own daemon needs no relay to itself; it still
    // connects, so nodes reach it through the relay when nothing else works.
    let relay = crate::relay_client::spawn(crate::relay_client::RelayConfig {
        urls: relays_rx,
        key: key_rx,
        coordinator: coordinator_wg,
    });
    // Names from the last map right away: SOCKS may be asked before the
    // map is applied below.
    let cached = state
        .map
        .as_deref()
        .and_then(|m| PeerMap::verify(&VerifyingKey::from_base64(&state.coordinator_key).ok()?, m).ok());
    let zone = Arc::new(RwLock::new(cached.map(|m| m.dns).unwrap_or_default()));
    let (ctl, ctl_rx) = mpsc::channel(32);
    let interface = match &opts.userspace {
        Some(us) => {
            let mut addrs = vec![(IpAddr::V4(ipv4), state.ipv4_prefix_len)];
            if let Some(ip) = ipv6 {
                addrs.push((IpAddr::V6(ip), 48));
            }
            let (stack, dev) = crate::netstack::Stack::start(addrs);
            tokio::spawn(Engine::new(&key, socket, Arc::new(dev)).with_relay(relay).run(ctl_rx));
            let resolver = crate::socks::Resolver {
                zone: zone.clone(),
                ipv4_prefix: (ipv4, state.ipv4_prefix_len),
                ipv6_prefix: (ipv6.unwrap_or(std::net::Ipv6Addr::UNSPECIFIED), 48),
            };
            crate::http::dial_mesh_with(stack.clone(), resolver.clone());
            if let Some(addr) = us.socks {
                let l = tokio::net::TcpListener::bind(addr)
                    .await
                    .map_err(|e| msg(format!("SOCKS on {addr}: {e}")))?;
                tokio::spawn(crate::socks::serve_socks(l, stack.clone(), resolver.clone()));
                tracing::info!("SOCKS5 to the mesh on {addr}");
            }
            if let Some(addr) = us.gateway {
                // Not fatal: the app works without it, only browser links don't.
                match tokio::net::TcpListener::bind(addr).await {
                    Ok(l) => {
                        tokio::spawn(crate::gateway::serve(l, stack.clone(), resolver.clone()));
                        tracing::info!(
                            "private apps in the browser: http://<app address>.localhost:{}/",
                            addr.port()
                        );
                    }
                    Err(e) => tracing::warn!("browser gateway on {addr}: {e}"),
                }
            }
            for f in us.forwards.clone() {
                let (stack, resolver) = (stack.clone(), resolver.clone());
                tracing::info!("forward {}{} → {}", if f.udp { "udp " } else { "" }, f.listen, f.target);
                tokio::spawn(async move {
                    if let Err(e) = crate::socks::serve_forward(f.clone(), stack, resolver).await {
                        tracing::warn!("forward {}: {e}", f.listen);
                    }
                });
            }
            // The ones the app adds while it runs (the display stream, Moonlight).
            tokio::spawn(crate::socks::watch_forwards(
                opts.run_dir.join("forwards"),
                us.forwards.clone(),
                stack.clone(),
                resolver.clone(),
            ));
            tracing::info!("{} is {} (userspace)", state.name, state.ipv4);
            "userspace".to_string()
        }
        None => {
            #[cfg(target_os = "linux")]
            let dev = {
                let tun = crate::tun::Tun::open(&opts.interface)?;
                tun.configure(ipv4, state.ipv4_prefix_len, ipv6.map(|ip| (ip, 48)))?;
                tun
            };
            #[cfg(target_os = "macos")]
            let dev = {
                let tun = crate::utun::Utun::open()?;
                tun.configure(ipv4, state.ipv4_prefix_len, ipv6.map(|ip| (ip, 48)))?;
                tun
            };
            #[cfg(target_os = "linux")]
            let interface = opts.interface.clone();
            #[cfg(target_os = "macos")]
            let interface = dev.name.clone();
            tokio::spawn(Engine::new(&key, socket, Arc::new(dev)).with_relay(relay).run(ctl_rx));
            tracing::info!("{} is {} on {}", state.name, state.ipv4, interface);
            interface
        }
    };
    {
        let (servers, ctl) = (opts.stun_servers.clone(), ctl.clone());
        tokio::spawn(async move { resolve_stun(servers, ctl).await });
    }
    let _ = write_private(
        &opts.run_dir.join("mesh.pid"),
        std::process::id().to_string().as_bytes(),
    );

    let live = Arc::new(Live {
        state: RwLock::new(state.clone()),
        map: RwLock::new(None),
        zone,
        coordinator_seen: RwLock::new(None),
        revoked: RwLock::new(false),
        key: key_tx,
        relays: relays_tx,
        interface: interface.clone(),
        port,
    });
    // The last map first: the mesh works before the coordinator answers.
    if let Some(signed) = &state.map {
        if let Ok(m) = PeerMap::verify(&live.verifying()?, signed) {
            apply_map(&live, &opts, &ctl, m, signed);
        }
    }
    if opts.dns && opts.userspace.is_none() {
        match UdpSocket::bind((ipv4, 53)).await {
            Ok(sock) => {
                tokio::spawn(crate::dns::server::serve(sock, live.zone.clone()));
                configure_resolver(&interface, &state.ipv4).await;
            }
            Err(e) => tracing::warn!("mesh DNS on {ipv4}:53: {e}"),
        }
    }

    // The map stream, reconnecting with a growing pause.
    {
        let (live, opts, ctl) = (live.clone(), opts.clone(), ctl.clone());
        tokio::spawn(async move {
            let mut wait = 1;
            loop {
                let started = Instant::now();
                if let Err(e) = follow_map(&live, &opts, &ctl).await {
                    tracing::debug!("map stream: {e}");
                }
                if started.elapsed() > Duration::from_secs(60) {
                    wait = 1;
                }
                tokio::time::sleep(Duration::from_secs(wait)).await;
                wait = (wait * 2).min(30);
            }
        });
    }
    // Endpoints, renewal and status.
    let mut sigusr1 = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut every5 = tokio::time::interval(Duration::from_secs(5));
    let mut last_endpoints = vec![];
    let mut ticks: u64 = 0;
    loop {
        tokio::select! {
            _ = every5.tick() => {
                ticks += 1;
                // Joined again (a re-pairing, a new main server) or left:
                // this process runs the old identity until it restarts, so
                // it ends and launchd / systemd start it on the new one.
                if ticks.is_multiple_of(2) {
                    // An unreadable file (being written) is no change.
                    let why = State::load(&opts.dir).ok().and_then(|now| identity_changed(&state, now.as_ref()));
                    if let Some(why) = why {
                        tracing::info!("{why}: restarting on the new state");
                        let _ = std::fs::remove_file(opts.run_dir.join("mesh.pid"));
                        return Ok(());
                    }
                }
                write_status(&live, &opts, &ctl).await;
                // After 30 seconds of silence, check its signed address every
                // 30 seconds so a new quick tunnel does not strand devices.
                let lost = beacon_due(live.coordinator_seen.read().unwrap().map(|t| t.elapsed()), ticks);
                if let (true, Some(base)) = (lost, opts.beacon.clone()) {
                    let live = live.clone();
                    tokio::spawn(async move { check_beacon(&live, &base).await });
                }
                // Endpoints every minute (sooner at start); renewal hourly.
                if ticks == 2 || ticks == 6 || ticks.is_multiple_of(12) {
                    report_endpoints(&live, &ctl, live.port, &mut last_endpoints).await;
                }
                if ticks % 720 == 3 {
                    if let Err(e) = renew(&live, &opts, &ctl, false).await {
                        tracing::warn!("renewing the certificate: {e}");
                    }
                }
            }
            _ = sigusr1.recv() => {
                // On request: a few tries, the coordinator may be busy or a
                // firewall reload may have dropped the first connection.
                for attempt in 1..=4 {
                    match renew(&live, &opts, &ctl, true).await {
                        Ok(_) => {
                            tracing::info!("key rotated on request");
                            break;
                        }
                        Err(e) => {
                            tracing::warn!("rotating the key (try {attempt}): {e}");
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                    }
                }
            }
            _ = sigterm.recv() => {
                tracing::info!("stopping");
                let _ = std::fs::remove_file(opts.run_dir.join("mesh.pid"));
                return Ok(());
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addr::Addressing;
    use crate::keys::CoordinatorKey;

    #[test]
    fn a_lost_coordinator_checks_its_signed_address_within_a_minute() {
        assert!(beacon_due(Some(Duration::from_secs(30)), 6));
        assert!(beacon_due(Some(Duration::from_secs(60)), 12));
        assert!(!beacon_due(Some(Duration::from_secs(25)), 6));
        assert!(!beacon_due(Some(Duration::from_secs(60)), 7));
        assert!(beacon_due(None, 6));
    }

    #[test]
    fn a_new_pairing_restarts_the_daemon_but_its_own_updates_dont() {
        let s: State = serde_json::from_value(serde_json::json!({
            "node_id": "n1", "name": "mac", "kind": "device", "ipv4": "10.77.0.2", "ipv6": "",
            "ipv4_prefix_len": 16, "certificate": "c", "node_token": "t", "coordinator_key": "ck",
            "coordinator": {"public_key": "wg", "ipv4": "10.77.0.1", "endpoints": []},
            "api_url": "http://10.77.0.1:7420"
        }))
        .unwrap();
        let mut own = s.clone();
        own.map = Some("signed map".into());
        own.certificate = "renewed".into();
        assert_eq!(identity_changed(&s, Some(&own)), None);
        let mut rejoined = s.clone();
        rejoined.node_id = "n7".into();
        assert!(identity_changed(&s, Some(&rejoined)).is_some());
        let mut repaired = s.clone();
        repaired.node_token = "t2".into();
        assert!(identity_changed(&s, Some(&repaired)).is_some());
        let mut other = s.clone();
        other.coordinator_key = "another".into();
        assert!(identity_changed(&s, Some(&other)).is_some());
        let mut moved = s.clone();
        moved.coordinator.public_key = "wg2".into();
        assert!(identity_changed(&s, Some(&moved)).is_some());
        assert!(identity_changed(&s, None).is_some(), "left");
    }

    fn rec(id: &str, name: &str, kind: NodeKind, idx: u16) -> NodeRecord {
        NodeRecord {
            id: id.into(),
            name: name.into(),
            kind,
            role: "owner".into(),
            public_key: NodeKey::generate().public().to_base64(),
            ip_index: idx,
            endpoints: vec![format!("192.168.1.{idx}:51820"), "bad endpoint".into()],
            extra_device_ports: vec!["5432/tcp".into()],
            container_ports: vec!["80/tcp".into(), "5432/tcp".into()],
            revoked: false,
        }
    }

    #[test]
    fn specs_acl_and_paths() {
        let a = Addressing::default();
        let main = rec("mn_1", "main", NodeKind::Server, 1);
        let pi = rec("mn_2", "pi", NodeKind::Server, 2);
        let mac = rec("mn_3", "mac", NodeKind::Device, 3);
        let map = build_map(&[main.clone(), pi.clone(), mac], &pi, &a, 1, 1, 0);
        let coord = CoordinatorInfo {
            public_key: main.public_key.clone(),
            ipv4: "10.77.0.1".into(),
            endpoints: vec!["203.0.113.7:51820".into()],
        };
        let specs = peer_specs(&map, &coord);
        let m = specs.iter().find(|s| s.name == "main").unwrap();
        assert_eq!(
            m.endpoints,
            vec![
                "192.168.1.1:51820".parse::<SocketAddr>().unwrap(),
                "203.0.113.7:51820".parse().unwrap()
            ],
            "map endpoints, then the coordinator's from enrollment; junk skipped"
        );
        assert_eq!(m.addresses.len(), 2);

        let acl = acl_json(&map, "vfy0");
        assert_eq!(acl["servers"], serde_json::json!(["10.77.0.1", "fd76:6679:6d65::1"]));
        assert_eq!(acl["devices"][0]["ipv4"], "10.77.0.3");
        let ports = acl["devices"][0]["ports"].as_array().unwrap();
        assert!(ports.contains(&serde_json::json!("7420/tcp")) && ports.contains(&serde_json::json!("5432/tcp")));
        assert_eq!(acl["container"][0]["ipv4"], "10.77.0.1");
        assert_eq!(acl["container"][0]["ports"], serde_json::json!(["80/tcp", "5432/tcp"]));

        let local = [Ipv4Addr::new(192, 168, 1, 9)];
        assert_eq!(path_of("direct", Some("192.168.1.31:51820"), &local), "lan");
        assert_eq!(path_of("direct", Some("81.2.3.4:51820"), &local), "direct");
        assert_eq!(path_of("direct", None, &local), "none");
        assert_eq!(path_of("relay", None, &local), "relay");
        assert_eq!(path_of("none", Some("81.2.3.4:51820"), &local), "none");
        assert_eq!(
            control_url("192.168.1.20:7443", "/v1/mesh/enroll"),
            "http://192.168.1.20:7443/v1/mesh/enroll"
        );
        assert_eq!(
            control_url("https://mesh.example.com/", "/x"),
            "https://mesh.example.com/x"
        );
    }

    // A full pairing code lists the server's LAN addresses before its
    // tunnel. From another network a LAN address never answers: it must
    // cost seconds, not the whole time the app gives a join (field test F96).
    #[tokio::test]
    async fn an_unreachable_address_doesnt_hold_up_the_next() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut c, _) = server.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf).await;
            let body = r#"{"error":"this code was already used"}"#;
            let head = format!("HTTP/1.1 403 Forbidden\r\ncontent-length: {}\r\n\r\n", body.len());
            let _ = c.write_all(format!("{head}{body}").as_bytes()).await;
        });
        let token = JoinToken {
            v: 1,
            // TEST-NET-2: routed to the default gateway and never answered.
            control: vec!["198.51.100.1:7443".into(), format!("127.0.0.1:{port}")],
            coordinator_wg: NodeKey::generate().public().to_base64(),
            claim: "c".into(),
            kind: NodeKind::Device,
        }
        .encode();
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let err = join(dir.path(), &token, "mac", vec![]).await.unwrap_err();
        assert!(err.to_string().contains("already used"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn enrollment_answers_are_checked() {
        let a = Addressing::default();
        let ck = CoordinatorKey::generate();
        let key = NodeKey::generate();
        let mut me = rec("mn_2", "pi", NodeKind::Server, 2);
        me.public_key = key.public().to_base64();
        let cert = NodeCert::issue(
            &ck,
            NodeCert {
                v: 1,
                node_id: me.id.clone(),
                name: "pi".into(),
                kind: NodeKind::Server,
                role: "owner".into(),
                public_key: me.public_key.clone(),
                ipv4: "10.77.0.2".into(),
                ipv6: "fd76:6679:6d65::2".into(),
                not_before: 0,
                not_after: 0,
                epoch: 1,
            },
            unix_now(),
        );
        let map = build_map(std::slice::from_ref(&me), &me, &a, 1, 1, 0).sign(&ck);
        let r = EnrollResponse {
            node_id: me.id.clone(),
            name: "pi".into(),
            ipv4: "10.77.0.2".into(),
            ipv6: "fd76:6679:6d65::2".into(),
            ipv4_prefix_len: 16,
            certificate: cert,
            node_token: "vfyn_x".into(),
            coordinator_key: ck.verifying().to_base64(),
            coordinator: CoordinatorInfo {
                public_key: "c".into(),
                ipv4: "10.77.0.1".into(),
                endpoints: vec![],
            },
            api_url: "http://10.77.0.1:7420".into(),
            map,
        };
        let s = accept(&key, &r).unwrap();
        assert_eq!((s.name.as_str(), s.ipv4.as_str()), ("pi", "10.77.0.2"));
        // Certified for another key: refused.
        assert!(accept(&NodeKey::generate(), &r).is_err());
        // Signed by someone else: refused.
        let mut forged = r.clone();
        forged.coordinator_key = CoordinatorKey::generate().verifying().to_base64();
        assert!(accept(&key, &forged).is_err());
    }
}

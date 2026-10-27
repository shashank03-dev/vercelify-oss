//! The mesh coordinator (design §13), on the main server: the node
//! registry, sealed enrollment on the control listener, and signed peer
//! maps streamed to every node. After enrollment a node talks to it only
//! inside the mesh: its node token is accepted from its own mesh address.

use crate::config::Config;
use crate::error::{ApiResult, AppError};
use crate::json::Json;
use crate::routes::AppState;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use vercelify_core::platform::{MeshInvite, MeshInviteRequest, MeshNode};
use vercelify_mesh::addr::{Addressing, COORDINATOR_INDEX};
use vercelify_mesh::cert::{unix_now, NodeCert, NodeKind};
use vercelify_mesh::keys::{CoordinatorKey, NodeKey, PublicKey};
use vercelify_mesh::proto::*;
use vercelify_mesh::seal::{CoordinatorSession, Sealed};
use vercelify_mesh::state::State as NodeState;
use vercelify_platform::mesh_registry::{self as reg, Enrollment, Node};
use vercelify_platform::Store;

pub struct Coordinator {
    pub store: Arc<Store>,
    key: CoordinatorKey,
    wg: NodeKey,
    pub addressing: Addressing,
    version: watch::Sender<u64>,
    /// Node ids just revoked: their streams say so before the maps change.
    kick: tokio::sync::broadcast::Sender<String>,
    cfg: Arc<Config>,
    pub self_id: String,
    /// Enrollment attempts per source address (a minute's window).
    attempts: Mutex<HashMap<IpAddr, (Instant, u32)>>,
    pub relay: crate::mesh_relay::RelayHub,
}

pub fn hostname() -> String {
    let h = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    let h: String = h
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let h = h.trim_matches('-').chars().take(40).collect::<String>();
    if valid_node_name(&h) {
        h
    } else {
        "main".into()
    }
}

impl Coordinator {
    /// The coordinator, when this server is one (`mesh_coordinator`).
    pub fn open(cfg: &Arc<Config>, platform_store: Option<Arc<Store>>) -> Result<Option<Arc<Self>>, String> {
        if !cfg.mesh_coordinator || cfg.role() == "worker" {
            return Ok(None);
        }
        if let Some(m) = crate::standby::superseded(cfg) {
            return Err(m);
        }
        let store = match platform_store {
            Some(s) => s,
            None => Arc::new(Store::open(&cfg.platform_db).map_err(|e| e.to_string())?),
        };
        let key = CoordinatorKey::load_or_create(&cfg.mesh_dir.join("coordinator.key")).map_err(|e| e.to_string())?;
        let wg = NodeKey::load_or_create(&vercelify_mesh::state::key_path(&cfg.mesh_dir)).map_err(|e| e.to_string())?;
        let name = match &cfg.mesh_name {
            Some(n) if valid_node_name(n) => n.clone(),
            Some(n) => {
                return Err(format!(
                    "mesh_name `{n}` is not a valid node name (lowercase letters, digits, dashes)"
                ))
            }
            None => hostname(),
        };
        let me = store
            .tx(|c| reg::ensure_coordinator(c, &name, &wg.public()))
            .map_err(|e| e.to_string())?;
        let v = store.with(reg::versions).map_err(|e| e.to_string())?.0;
        let (version, _) = watch::channel(v);
        let co = Arc::new(Self {
            store,
            key,
            wg,
            addressing: Addressing::default(),
            version,
            kick: tokio::sync::broadcast::channel(16).0,
            cfg: cfg.clone(),
            self_id: me.id.clone(),
            attempts: Mutex::new(HashMap::new()),
            relay: Default::default(),
        });
        co.write_own_state(&me)?;
        co.write_beacon();
        Ok(Some(co))
    }

    pub fn wg_public(&self) -> PublicKey {
        self.wg.public()
    }

    /// Open a message a node sealed to this coordinator's WireGuard key.
    pub fn open_sealed(&self, s: &Sealed) -> vercelify_mesh::Result<(CoordinatorSession, Vec<u8>)> {
        CoordinatorSession::open(&self.wg, s)
    }

    /// Ids of nodes as they are revoked.
    pub fn kicked(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.kick.subscribe()
    }

    /// Where the relay is: the control addresses, and the public URL.
    fn relays(&self) -> Vec<String> {
        vercelify_mesh::relay::relay_urls(&control_addresses(&self.cfg), self.cfg.mesh_public_url.as_deref())
    }

    /// With a public address, the signed note nodes look for when they lose
    /// this server (`<topic> <note>` in mesh_dir/beacon; `vercelify remote`
    /// posts it). Signed at startup: the address only changes with a restart.
    fn write_beacon(&self) {
        let path = self.cfg.mesh_dir.join("beacon");
        let public = self
            .cfg
            .mesh_public_url
            .as_deref()
            .is_some_and(|u| !u.trim().is_empty())
            || !self.cfg.mesh_public_endpoints.is_empty();
        if !public {
            let _ = std::fs::remove_file(&path);
            return;
        }
        let note = vercelify_mesh::beacon::Beacon {
            v: 1,
            relays: self.relays(),
            issued_at: unix_now(),
        };
        let line = format!(
            "{} {}\n",
            vercelify_mesh::beacon::topic(&self.key.verifying()),
            note.sign(&self.key)
        );
        if let Err(e) = std::fs::write(&path, line) {
            tracing::warn!("writing {}: {e}", path.display());
        }
    }

    /// The coordinator's own daemon joins without a token: the agent writes
    /// its state. Its token is kept while it still works, so a running
    /// daemon isn't locked out by an agent restart.
    fn write_own_state(&self, me: &Node) -> Result<(), String> {
        let dir = &self.cfg.mesh_dir;
        let existing = NodeState::load(dir).ok().flatten();
        let token = match existing.as_ref().map(|s| s.node_token.clone()) {
            Some(t)
                if self
                    .store
                    .with(|c| reg::by_token(c, &t))
                    .ok()
                    .flatten()
                    .is_some_and(|n| n.id == me.id) =>
            {
                t
            }
            _ => self
                .store
                .tx(|c| reg::issue_token(c, &me.id))
                .map_err(|e| e.to_string())?,
        };
        let certificate = self.certify(me).map_err(|e| e.message)?;
        let state = NodeState {
            node_id: me.id.clone(),
            name: me.name.clone(),
            kind: NodeKind::Server,
            ipv4: self.addressing.ipv4(me.ip_index).to_string(),
            ipv6: self.addressing.ipv6(me.ip_index).to_string(),
            ipv4_prefix_len: 16,
            certificate,
            node_token: token,
            coordinator_key: self.key.verifying().to_base64(),
            coordinator: CoordinatorInfo {
                public_key: me.public_key.clone(),
                ipv4: self.addressing.ipv4(COORDINATOR_INDEX).to_string(),
                endpoints: vec![],
            },
            api_url: format!("http://127.0.0.1:{}", self.cfg.port),
            map: Some(self.map_for(me).map_err(|e| e.message)?),
        };
        state.save(dir).map_err(|e| e.to_string())
    }

    fn certify(&self, n: &Node) -> Result<String, AppError> {
        let epoch = self.store.with(reg::versions)?.1;
        let now = unix_now();
        let cert = NodeCert::issue(
            &self.key,
            NodeCert {
                v: 1,
                node_id: n.id.clone(),
                name: n.name.clone(),
                kind: n.kind,
                role: n.role.clone(),
                public_key: n.public_key.clone(),
                ipv4: self.addressing.ipv4(n.ip_index).to_string(),
                ipv6: self.addressing.ipv6(n.ip_index).to_string(),
                not_before: 0,
                not_after: 0,
                epoch,
            },
            now,
        );
        let expires = now + vercelify_mesh::cert::CERT_LIFETIME_SECS;
        let id = n.id.clone();
        self.store.with(|c| reg::set_cert_expiry(c, &id, expires))?;
        Ok(cert)
    }

    /// `node`'s current map, signed.
    pub fn map_for(&self, node: &Node) -> Result<String, AppError> {
        let (version, epoch) = self.store.with(reg::versions)?;
        let mut records = self.store.with(reg::records)?;
        // The coordinator is also reachable where the admin says it is.
        for r in records.iter_mut().filter(|r| r.ip_index == COORDINATOR_INDEX) {
            for e in &self.cfg.mesh_public_endpoints {
                if valid_endpoint(e) && !r.endpoints.contains(e) {
                    r.endpoints.push(e.clone());
                }
            }
        }
        let me = records
            .iter()
            .find(|r| r.id == node.id)
            .cloned()
            .ok_or_else(|| AppError::not_found("node"))?;
        let mut map = build_map(&records, &me, &self.addressing, version, epoch, unix_now());
        map.relays = self.relays();
        Ok(map.sign(&self.key))
    }

    /// Something changed: every map stream sends a new map.
    pub fn changed(&self) {
        if let Ok((v, _)) = self.store.with(reg::versions) {
            self.version.send_replace(v);
        }
    }

    fn allow_attempt(&self, ip: IpAddr) -> bool {
        let mut m = self.attempts.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        m.retain(|_, (t, _)| now.duration_since(*t) < Duration::from_secs(60));
        let e = m.entry(ip).or_insert((now, 0));
        e.1 += 1;
        e.1 <= 10
    }

    /// The node behind a token, if the request comes from its own mesh
    /// address (or, for the coordinator's own daemon, from this machine).
    pub(crate) fn authenticate(&self, headers: &HeaderMap, from: IpAddr) -> Result<Node, AppError> {
        let token = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("")
            .to_string();
        let node = self.store.with(|c| reg::by_token(c, &token))?.ok_or_else(|| AppError {
            status: StatusCode::UNAUTHORIZED,
            message: "not a mesh node token (revoked?)".into(),
        })?;
        let own = [
            IpAddr::V4(self.addressing.ipv4(node.ip_index)),
            IpAddr::V6(self.addressing.ipv6(node.ip_index)),
        ];
        let local = node.ip_index == COORDINATOR_INDEX && from.is_loopback();
        if !own.contains(&from) && !local {
            return Err(AppError {
                status: StatusCode::FORBIDDEN,
                message: format!("a node token is accepted only from the node's own mesh address, not {from}"),
            });
        }
        Ok(node)
    }
}

pub fn coordinator(s: &AppState) -> Result<Arc<Coordinator>, AppError> {
    s.mesh.clone().ok_or_else(|| {
        AppError::unavailable("this server doesn't coordinate a mesh (agent.toml: mesh_coordinator = true)")
    })
}

// ----- the control listener ----------------------------------------------------------

/// What answers on `mesh_control_listen`: enrollment, nothing else.
pub fn control_router(co: Arc<Coordinator>, claims: Arc<crate::pairing::Claims>) -> Router {
    let cfg = co.cfg.clone();
    Router::new()
        .route(
            vercelify_mesh::pake::PAIR_PATH,
            post(move |Json(req): Json<vercelify_mesh::pake::PairRequest>| {
                let (claims, cfg) = (claims.clone(), cfg.clone());
                async move { claims.pake(&cfg, &req).await.map(Json) }
            }),
        )
        .route("/v1/mesh/enroll", post(enroll))
        .route("/v1/mesh/hello", get(hello))
        .route(vercelify_mesh::relay::RELAY_PATH, get(crate::mesh_relay::relay))
        .with_state(co)
}

async fn hello(State(co): State<Arc<Coordinator>>) -> Json<Value> {
    // The epoch tells a returning main server that a standby replaced it.
    let epoch = co.store.with(reg::versions).map(|v| v.1).unwrap_or(1);
    Json(json!({ "coordinator_wg": co.wg_public().to_base64(), "service": "vercelify-mesh", "epoch": epoch }))
}

async fn enroll(
    State(co): State<Arc<Coordinator>>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(sealed): Json<Sealed>,
) -> ApiResult<Sealed> {
    if !co.allow_attempt(from.ip()) {
        return Err(AppError {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: "too many attempts; wait a minute".into(),
        });
    }
    let (session, plain) = CoordinatorSession::open(&co.wg, &sealed)
        .map_err(|_| AppError::bad_request("the request isn't sealed to this coordinator"))?;
    let req: EnrollRequest =
        serde_json::from_slice(&plain).map_err(|e| AppError::bad_request(format!("enrollment: {e}")))?;
    let pk = session.node_public;
    // Where the node saw the coordinator, and where the coordinator saw it.
    let host_ip = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.rsplit_once(':').map(|(a, _)| a.trim_matches(['[', ']']).to_string()))
        .and_then(|h| h.parse::<IpAddr>().ok());
    let mut endpoints = req.endpoints.clone();
    if !from.ip().is_loopback() {
        let seen = SocketAddr::new(from.ip(), DEFAULT_WG_PORT).to_string();
        if !endpoints.contains(&seen) {
            endpoints.push(seen);
        }
    }
    let (node, token) = {
        let req = req.clone();
        co.store
            .tx(move |c| {
                reg::enroll(
                    c,
                    &Enrollment {
                        claim: &req.claim,
                        name: &req.name,
                        kind: req.kind,
                        os: &req.os,
                        arch: &req.arch,
                        public_key: &pk,
                        endpoints: &endpoints,
                        ts: req.ts,
                    },
                )
            })
            .map_err(AppError::from)?
    };
    tracing::info!(
        "mesh: {} joined as {} ({})",
        from.ip(),
        node.name,
        co.addressing.ipv4(node.ip_index)
    );
    let me = co.store.with(|c| reg::get(c, &co.self_id))?;
    let mut coord_eps: Vec<String> = me.endpoints.clone();
    if let Some(ip) = host_ip {
        let e = SocketAddr::new(ip, co.cfg.mesh_port).to_string();
        if !coord_eps.contains(&e) {
            coord_eps.insert(0, e);
        }
    }
    coord_eps.extend(
        co.cfg
            .mesh_public_endpoints
            .iter()
            .filter(|e| valid_endpoint(e))
            .cloned(),
    );
    let resp = EnrollResponse {
        node_id: node.id.clone(),
        name: node.name.clone(),
        ipv4: co.addressing.ipv4(node.ip_index).to_string(),
        ipv6: co.addressing.ipv6(node.ip_index).to_string(),
        ipv4_prefix_len: 16,
        certificate: co.certify(&node)?,
        node_token: token,
        coordinator_key: co.key.verifying().to_base64(),
        coordinator: CoordinatorInfo {
            public_key: me.public_key.clone(),
            ipv4: co.addressing.ipv4(COORDINATOR_INDEX).to_string(),
            endpoints: coord_eps,
        },
        api_url: format!("http://{}:{}", co.addressing.ipv4(COORDINATOR_INDEX), co.cfg.port),
        map: co.map_for(&node)?,
    };
    co.changed();
    Ok(Json(
        session.seal_response(&serde_json::to_vec(&resp).expect("serializable")),
    ))
}

// ----- node routes (inside the mesh, node tokens) --------------------------------------

/// Routes nodes call with their own token; mounted outside the device-token auth.
pub fn node_router() -> Router<AppState> {
    Router::new()
        .route("/v1/mesh/map", get(map_stream))
        .route("/v1/mesh/endpoints", post(endpoints))
        .route("/v1/mesh/renew", post(renew))
        .route(
            "/v1/platform/nodes/heartbeat",
            post(crate::platform_api::node_heartbeat),
        )
}

/// The node's map now and after every change; a ping every 25 s keeps NAT
/// mappings and the client's idle timer alive.
async fn map_stream(
    State(s): State<AppState>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let co = coordinator(&s)?;
    let node = co.authenticate(&headers, from.ip())?;
    let id = node.id.clone();
    let _ = co.store.with(|c| reg::touch(c, &id));
    let mut rx = co.version.subscribe();
    let mut kicked = co.kick.subscribe();
    let (tx, body_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(8);
    let line = |e: MapEvent| {
        let mut v = serde_json::to_vec(&e).unwrap_or_default();
        v.push(b'\n');
        bytes::Bytes::from(v)
    };
    tokio::spawn(async move {
        let mut sent = None;
        loop {
            // Gone (revoked) or the coordinator can't build a map: end the
            // stream; the node reconnects and learns why.
            let node = match co.store.with(|c| reg::get(c, &id)) {
                Ok(n) if n.revoked_at.is_none() => n,
                _ => return,
            };
            let v = *rx.borrow_and_update();
            if sent != Some(v) {
                let Ok(signed) = co.map_for(&node) else { return };
                if tx.send(line(MapEvent::Map { signed })).await.is_err() {
                    return;
                }
                sent = Some(v);
            }
            tokio::select! {
                r = rx.changed() => if r.is_err() { return },
                k = kicked.recv() => {
                    if matches!(&k, Ok(n) if *n == id) {
                        let _ = tx.send(line(MapEvent::Revoked)).await;
                        return;
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(25)) => {
                    if tx.send(line(MapEvent::Ping)).await.is_err() {
                        return;
                    }
                    let _ = co.store.with(|c| reg::touch(c, &id));
                }
            }
        }
    });
    let headers = [
        (axum::http::header::CONTENT_TYPE, "application/x-ndjson"),
        (axum::http::header::CACHE_CONTROL, "no-store"),
    ];
    Ok((headers, axum::body::Body::new(crate::routes::LineBody(body_rx))).into_response())
}

async fn endpoints(
    State(s): State<AppState>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<EndpointsReport>,
) -> ApiResult<Value> {
    let co = coordinator(&s)?;
    let node = co.authenticate(&headers, from.ip())?;
    let changed = co.store.tx(|c| reg::set_endpoints(c, &node.id, &req.endpoints))?;
    if changed {
        co.changed();
    }
    Ok(Json(json!({ "changed": changed })))
}

/// A new certificate, for a new key when given (keys rotate with it).
async fn renew(
    State(s): State<AppState>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RenewRequest>,
) -> ApiResult<RenewResponse> {
    let co = coordinator(&s)?;
    let mut node = co.authenticate(&headers, from.ip())?;
    if node.ip_index == COORDINATOR_INDEX && req.public_key.is_some() {
        // The coordinator's key is also what enrollments are sealed to;
        // rotating it would invalidate every outstanding join token.
        return Err(AppError::bad_request(
            "the coordinator keeps its key; renew without one",
        ));
    }
    if let Some(pk) = &req.public_key {
        let pk = PublicKey::from_base64(pk).map_err(|e| AppError::bad_request(e.to_string()))?;
        if pk.to_base64() != node.public_key {
            node = co.store.tx(|c| reg::rekey(c, &node.id, &pk))?;
            // Answer first: the peers (the coordinator's own daemon too) drop
            // the old key with the next map, and this answer travels on it.
            let co2 = co.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(800)).await;
                co2.changed();
            });
            tracing::info!("mesh: {} rotated its key", node.name);
        }
    }
    Ok(Json(RenewResponse {
        certificate: co.certify(&node)?,
    }))
}

// ----- owner routes ---------------------------------------------------------------------

pub fn owner_routes() -> Router<AppState> {
    Router::new()
        .route("/mesh/nodes", get(node_list))
        .route("/mesh/nodes/{id}", delete(node_revoke))
        .route("/mesh/invites", post(invite))
        .route("/mesh/status", get(status))
}

async fn node_list(State(s): State<AppState>) -> ApiResult<Vec<MeshNode>> {
    let co = coordinator(&s)?;
    let nodes = co.store.with(reg::list)?;
    Ok(Json(
        nodes
            .iter()
            .map(|n| {
                let mut w = n.wire(&co.addressing);
                (w.relay_connected, w.relayed_bytes) = co.relay.stats(&n.public_key, &n.id);
                w
            })
            .collect(),
    ))
}

/// Revoke (and with `?forget=true`, then remove) a node.
async fn node_revoke(
    State(s): State<AppState>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let co = coordinator(&s)?;
    let n = co.store.tx(|c| reg::revoke(c, &id))?;
    // The node hears it first, over the mesh it is still on; then the
    // others drop it.
    if co.kick.send(n.id.clone()).is_ok() {
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    co.changed();
    if q.get("forget").map(String::as_str) == Some("true") {
        co.store.tx(|c| reg::delete(c, &n.id))?;
    }
    s.events.changed("mesh");
    tracing::info!("mesh: revoked {}", n.name);
    Ok(Json(json!({ "revoked": n.name })))
}

/// Where joining nodes can reach the control listener: the configured
/// address, or every LAN address of this machine.
fn control_addresses(cfg: &Config) -> Vec<String> {
    let port = cfg
        .mesh_control_listen
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .unwrap_or(DEFAULT_CONTROL_PORT);
    let host = cfg.mesh_control_listen.rsplit_once(':').map(|(h, _)| h).unwrap_or("");
    if !host.is_empty() && host != "0.0.0.0" && host != "[::]" {
        return vec![cfg.mesh_control_listen.clone()];
    }
    crate::lan_ipv4s()
        .into_iter()
        .map(|ip| format!("{ip}:{port}"))
        .collect()
}

/// A single-use join token: where the control listener is (LAN addresses,
/// then the tunnel), the coordinator's WireGuard key, and the claim. Also
/// used by `vercelify-agent pair` for the Mac app (outside the server).
pub fn make_invite(
    cfg: &Config,
    store: &Store,
    coordinator_wg: &PublicKey,
    req: &MeshInviteRequest,
) -> Result<MeshInvite, AppError> {
    let kind = match req.kind.as_deref().unwrap_or("server") {
        "server" => NodeKind::Server,
        "device" => NodeKind::Device,
        k => return Err(AppError::bad_request(format!("unknown kind `{k}` (server or device)"))),
    };
    let role = req.role.clone().unwrap_or_else(|| "owner".into());
    let ttl = req.ttl_minutes.unwrap_or(60);
    let mut control = control_addresses(cfg);
    // Outside the LAN: through the tunnel.
    if let Some(u) = cfg.mesh_public_url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        let u = u.trim_end_matches('/');
        control.push(if u.contains("://") {
            u.to_string()
        } else {
            format!("https://{u}")
        });
    }
    if control.is_empty() {
        return Err(AppError::unavailable("this server has no LAN address to be reached at"));
    }
    let (claim, expires_at) = store.tx(|c| reg::invite(c, kind, &role, ttl))?;
    let token = JoinToken {
        v: 1,
        control,
        coordinator_wg: coordinator_wg.to_base64(),
        claim,
        kind,
    }
    .encode();
    Ok(MeshInvite { token, expires_at })
}

async fn invite(State(s): State<AppState>, body: Option<Json<MeshInviteRequest>>) -> ApiResult<MeshInvite> {
    let co = coordinator(&s)?;
    let req = body.map(|b| b.0).unwrap_or_default();
    Ok(Json(make_invite(&co.cfg, &co.store, &co.wg_public(), &req)?))
}

/// This node's view (the daemon's status file) and, on the coordinator, the
/// registry's size.
async fn status(State(s): State<AppState>) -> ApiResult<Value> {
    let path = s.config.run_dir.join("mesh-status.json");
    let daemon: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let coordinator = match &s.mesh {
        Some(co) => {
            let nodes = co.store.with(reg::list)?;
            json!({
                "nodes": nodes.iter().filter(|n| n.revoked_at.is_none()).count(),
                "revoked": nodes.iter().filter(|n| n.revoked_at.is_some()).count(),
                "map_version": co.store.with(reg::versions)?.0,
                "control": control_addresses(&s.config),
            })
        }
        None => Value::Null,
    };
    Ok(Json(json!({ "daemon": daemon, "coordinator": coordinator })))
}

/// Where this server is reached privately: its mesh address.
pub fn private_ipv4(cfg: &Config) -> Option<String> {
    own_ipv4(cfg).map(|ip| ip.to_string())
}

/// This node's mesh address, once it has joined (for `listen = ["mesh"]`).
pub fn own_ipv4(cfg: &Config) -> Option<IpAddr> {
    NodeState::load(&cfg.mesh_dir)
        .ok()
        .flatten()
        .and_then(|s| s.ipv4.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostnames_become_node_names() {
        assert!(valid_node_name(&hostname()));
    }

    #[test]
    fn control_address_from_config() {
        let cfg = Config {
            mesh_control_listen: "192.168.1.20:7443".into(),
            ..Config::default()
        };
        assert_eq!(control_addresses(&cfg), vec!["192.168.1.20:7443"]);
    }
}

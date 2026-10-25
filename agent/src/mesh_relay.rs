//! The mesh relay (design §13.7), on the coordinator's control listener:
//! nodes without a direct path send each other WireGuard and disco packets
//! through it over a WebSocket. A connection is authenticated by the node's
//! WireGuard key (a challenge sealed to the coordinator), forwards only
//! between nodes the map lets talk, and keeps no packets beyond a short
//! queue per node.

use crate::mesh::Coordinator;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::response::Response;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use vercelify_mesh::cert::NodeKind;
use vercelify_mesh::keys::PublicKey;
use vercelify_mesh::relay::{self, Answer, Challenge};
use vercelify_mesh::seal::Sealed;
use vercelify_platform::mesh_registry as reg;

struct Conn {
    tx: mpsc::Sender<Vec<u8>>,
    kind: NodeKind,
    serial: u64,
}

/// Who is connected, and how much each node sent through the relay.
#[derive(Default)]
pub struct RelayHub {
    conns: Mutex<HashMap<PublicKey, Conn>>,
    bytes: Mutex<HashMap<String, u64>>,
    serial: AtomicU64,
}

impl RelayHub {
    /// (connected now, bytes relayed) for a node.
    pub fn stats(&self, public_key: &str, node_id: &str) -> (bool, u64) {
        let connected = PublicKey::from_base64(public_key)
            .ok()
            .is_some_and(|k| self.conns.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&k));
        let bytes = self
            .bytes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(node_id)
            .copied()
            .unwrap_or(0);
        (connected, bytes)
    }

    fn register(&self, key: PublicKey, kind: NodeKind, tx: mpsc::Sender<Vec<u8>>) -> u64 {
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        // A newer connection replaces the old one (whose channel then closes).
        self.conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Conn { tx, kind, serial });
        serial
    }

    fn unregister(&self, key: &PublicKey, serial: u64) {
        let mut m = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        if m.get(key).is_some_and(|c| c.serial == serial) {
            m.remove(key);
        }
    }

    /// Forward one packet from `src`; false when it can't go (unknown
    /// destination, not allowed, or its queue is full).
    fn forward(&self, src: &PublicKey, src_kind: NodeKind, dst: &PublicKey, packet: &[u8]) -> bool {
        let m = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        let Some(c) = m.get(dst) else { return false };
        if !relay::may_relay(src_kind, c.kind) {
            return false;
        }
        c.tx.try_send(relay::frame(src, packet)).is_ok()
    }

    fn count(&self, node_id: &str, n: usize) {
        *self
            .bytes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(node_id.to_string())
            .or_insert(0) += n as u64;
    }
}

pub async fn relay(
    State(co): State<Arc<Coordinator>>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.max_message_size(relay::MAX_MESSAGE)
        .max_frame_size(relay::MAX_MESSAGE)
        .on_upgrade(move |socket| async move {
            if let Err(e) = session(co, socket).await {
                tracing::debug!("mesh relay from {from}: {e}");
            }
        })
}

async fn recv_text(ws: &mut WebSocket) -> Result<String, String> {
    match tokio::time::timeout(Duration::from_secs(10), ws.recv()).await {
        Err(_) => Err("no answer to the challenge".into()),
        Ok(Some(Ok(Message::Text(t)))) => Ok(t.to_string()),
        Ok(other) => Err(format!("unexpected {other:?}")),
    }
}

/// Which node holds the key that sealed `answer` to `challenge`.
fn authenticate(co: &Coordinator, challenge: &str, answer: &str) -> Result<(PublicKey, reg::Node), String> {
    let sealed: Sealed = serde_json::from_str(answer).map_err(|e| format!("answer: {e}"))?;
    let (session, plain) = co
        .open_sealed(&sealed)
        .map_err(|_| "not sealed to this coordinator".to_string())?;
    let a: Answer = serde_json::from_slice(&plain).map_err(|e| format!("answer: {e}"))?;
    if a.challenge != challenge {
        return Err("wrong challenge".into());
    }
    let key = session.node_public;
    let node = co
        .store
        .with(|c| reg::by_public_key(c, &key))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "not a node of this mesh (revoked?)".to_string())?;
    Ok((key, node))
}

async fn session(co: Arc<Coordinator>, mut ws: WebSocket) -> Result<(), String> {
    let mut c = [0u8; 32];
    getrandom::fill(&mut c).map_err(|e| e.to_string())?;
    let challenge = B64.encode(c);
    let hello = serde_json::to_string(&Challenge {
        challenge: challenge.clone(),
    })
    .expect("serializable");
    ws.send(Message::Text(hello.into())).await.map_err(|e| e.to_string())?;
    let answer = recv_text(&mut ws).await?;
    let (key, node) = match authenticate(&co, &challenge, &answer) {
        Ok(x) => x,
        Err(e) => {
            let _ = ws
                .send(Message::Text(serde_json::json!({ "error": e }).to_string().into()))
                .await;
            return Err(e);
        }
    };
    ws.send(Message::Text(r#"{"ok":true}"#.into()))
        .await
        .map_err(|e| e.to_string())?;
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(relay::QUEUE);
    let hub = &co.relay;
    let serial = hub.register(key, node.kind, tx);
    let mut kicked = co.kicked();
    let mut ping = tokio::time::interval(Duration::from_secs(30));
    ping.tick().await;
    tracing::info!("mesh relay: {} connected", node.name);
    let result = loop {
        tokio::select! {
            m = ws.recv() => match m {
                Some(Ok(Message::Binary(b))) => {
                    if let Some((dst, packet)) = relay::split(&b) {
                        if hub.forward(&key, node.kind, &dst, packet) {
                            hub.count(&node.id, packet.len());
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None => break Ok(()),
                Some(Ok(_)) => {}
                Some(Err(e)) => break Err(e.to_string()),
            },
            out = rx.recv() => match out {
                Some(f) => {
                    if let Err(e) = ws.send(Message::Binary(f.into())).await {
                        break Err(e.to_string());
                    }
                }
                // Replaced by a newer connection of the same node.
                None => break Ok(()),
            },
            k = kicked.recv() => {
                if matches!(&k, Ok(id) if *id == node.id) {
                    break Ok(());
                }
            }
            _ = ping.tick() => {
                if ws.send(Message::Ping(Default::default())).await.is_err() {
                    break Ok(());
                }
            }
        }
    };
    hub.unregister(&key, serial);
    tracing::info!("mesh relay: {} left", node.name);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_only_what_the_map_allows() {
        let hub = RelayHub::default();
        let (server, device, other) = (PublicKey([1; 32]), PublicKey([2; 32]), PublicKey([3; 32]));
        let (stx, mut srx) = mpsc::channel(2);
        let (dtx, mut drx) = mpsc::channel(2);
        let s1 = hub.register(server, NodeKind::Server, stx);
        hub.register(device, NodeKind::Device, dtx);
        assert!(hub.forward(&device, NodeKind::Device, &server, b"to the server"));
        let f = srx.try_recv().unwrap();
        assert_eq!(relay::split(&f), Some((device, &b"to the server"[..])));
        assert!(hub.forward(&server, NodeKind::Server, &device, b"to the device"));
        assert!(drx.try_recv().is_ok());
        // Device to device: refused; unknown destination: dropped.
        let (otx, _orx) = mpsc::channel(2);
        hub.register(other, NodeKind::Device, otx);
        assert!(!hub.forward(&device, NodeKind::Device, &other, b"x"));
        assert!(!hub.forward(&server, NodeKind::Server, &PublicKey([9; 32]), b"x"));
        // A full queue drops instead of waiting.
        assert!(hub.forward(&device, NodeKind::Device, &server, b"1"));
        assert!(hub.forward(&device, NodeKind::Device, &server, b"2"));
        assert!(!hub.forward(&device, NodeKind::Device, &server, b"3"));
        // An old connection's exit doesn't remove its replacement.
        let (stx2, _srx2) = mpsc::channel(2);
        let s2 = hub.register(server, NodeKind::Server, stx2);
        hub.unregister(&server, s1);
        assert!(hub.stats(&server.to_base64(), "x").0);
        hub.unregister(&server, s2);
        assert!(!hub.stats(&server.to_base64(), "x").0);
        hub.count("mn_1", 100);
        hub.count("mn_1", 20);
        assert_eq!(hub.stats(&device.to_base64(), "mn_1"), (true, 120));
    }
}

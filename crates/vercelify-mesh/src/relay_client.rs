//! The daemon's side of the relay (design §13.7): one WebSocket to the
//! coordinator, kept up while the node runs, carrying the WireGuard and
//! disco packets of peers without a direct path. The URLs come from the map
//! (LAN addresses, then the public tunnel URL); the connection is
//! authenticated with the node's key, so it follows key rotation.

use crate::http;
use crate::keys::{NodeKey, PublicKey};
use crate::relay::{self, Answer, Challenge};
use crate::seal::NodeSession;
use crate::wg::RelayLink;
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::protocol::{Message, WebSocketConfig};

/// What the relay task follows.
pub struct RelayConfig {
    /// Relay URLs from the map, in order of preference.
    pub urls: watch::Receiver<Vec<String>>,
    /// The node's key (it changes with rotation: the task reconnects).
    pub key: watch::Receiver<Arc<NodeKey>>,
    /// The coordinator's WireGuard key: the challenge is sealed to it.
    pub coordinator: PublicKey,
}

/// Start the relay task; returns the engine's end.
pub fn spawn(cfg: RelayConfig) -> RelayLink {
    let (to_relay, from_engine) = mpsc::channel(relay::QUEUE);
    let (to_engine, from_relay) = mpsc::channel(relay::QUEUE);
    let up = Arc::new(AtomicBool::new(false));
    tokio::spawn(run(cfg, to_engine, from_engine, up.clone()));
    RelayLink {
        tx: to_relay,
        rx: from_relay,
        up,
    }
}

async fn run(
    mut cfg: RelayConfig,
    to_engine: mpsc::Sender<(PublicKey, Vec<u8>)>,
    mut from_engine: mpsc::Receiver<(PublicKey, Vec<u8>)>,
    up: Arc<AtomicBool>,
) {
    let mut wait = 1;
    loop {
        let urls = cfg.urls.borrow_and_update().clone();
        if urls.is_empty() {
            if cfg.urls.changed().await.is_err() {
                return;
            }
            continue;
        }
        let mut connected = false;
        for url in &urls {
            match session(url, &mut cfg, &to_engine, &mut from_engine, &up).await {
                Ok(ran) => {
                    connected |= ran;
                    break;
                }
                Err(e) => tracing::debug!("relay {url}: {e}"),
            }
        }
        up.store(false, Ordering::Relaxed);
        if to_engine.is_closed() {
            return;
        }
        wait = if connected { 1 } else { (wait * 2).min(30) };
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
            // The daemon is gone with its map: so is the relay.
            r = cfg.urls.changed() => if r.is_err() { return },
        }
    }
}

/// One connection, until it drops or the URLs or key change. Ok(true) when
/// it got as far as being connected.
async fn session(
    url: &str,
    cfg: &mut RelayConfig,
    to_engine: &mpsc::Sender<(PublicKey, Vec<u8>)>,
    from_engine: &mut mpsc::Receiver<(PublicKey, Vec<u8>)>,
    up: &AtomicBool,
) -> Result<bool, String> {
    let u = http::parse_url(url).map_err(|e| e.to_string())?;
    let io = http::connect_io(&u, Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    let mut wsc = WebSocketConfig::default();
    wsc.max_message_size = Some(relay::MAX_MESSAGE);
    wsc.max_frame_size = Some(relay::MAX_MESSAGE);
    let (mut ws, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::client_async_with_config(url, io, Some(wsc)),
    )
    .await
    .map_err(|_| "handshake timed out".to_string())?
    .map_err(|e| e.to_string())?;

    // The challenge, answered sealed with this node's key.
    let key = cfg.key.borrow_and_update().clone();
    let text = next_text(&mut ws).await?;
    let ch: Challenge = serde_json::from_str(&text).map_err(|e| format!("challenge: {e}"))?;
    let session = NodeSession::new(&key, &cfg.coordinator).map_err(|e| e.to_string())?;
    let answer = session.seal_request(
        &serde_json::to_vec(&Answer {
            challenge: ch.challenge,
        })
        .expect("serializable"),
    );
    ws.send(Message::Text(
        serde_json::to_string(&answer).expect("serializable").into(),
    ))
    .await
    .map_err(|e| e.to_string())?;
    let verdict = next_text(&mut ws).await?;
    if !verdict.contains("\"ok\":true") {
        return Err(format!("refused: {verdict}"));
    }
    // What queued while down is stale: WireGuard resends what matters.
    while from_engine.try_recv().is_ok() {}
    up.store(true, Ordering::Relaxed);
    tracing::info!("relay connected: {url}");
    let mut ping = tokio::time::interval(Duration::from_secs(25));
    ping.tick().await;
    loop {
        tokio::select! {
            m = ws.next() => match m {
                Some(Ok(Message::Binary(b))) => {
                    if let Some((src, packet)) = relay::split(&b) {
                        let _ = to_engine.try_send((src, packet.to_vec()));
                    }
                }
                Some(Ok(Message::Close(_))) | None => return Ok(true),
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    tracing::info!("relay {url}: {e}");
                    return Ok(true);
                }
            },
            out = from_engine.recv() => match out {
                Some((dst, packet)) => {
                    if ws.send(Message::Binary(relay::frame(&dst, &packet).into())).await.is_err() {
                        return Ok(true);
                    }
                }
                None => return Ok(true),
            },
            _ = ping.tick() => {
                if ws.send(Message::Ping(Default::default())).await.is_err() {
                    return Ok(true);
                }
            }
            // (A dropped sender: the key stays as it is.)
            r = cfg.key.changed(), if cfg.key.has_changed().is_ok() => {
                if r.is_ok() {
                    tracing::debug!("relay: the key changed; reconnecting");
                    let _ = ws.close(None).await;
                    return Ok(true);
                }
            }
            r = cfg.urls.changed() => {
                if r.is_err() || !cfg.urls.borrow().iter().any(|x| x == url) {
                    let _ = ws.close(None).await;
                    return Ok(true);
                }
            }
        }
    }
}

async fn next_text<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>) -> Result<String, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
            Err(_) => return Err("no answer".into()),
            Ok(Some(Ok(Message::Text(t)))) => return Ok(t.to_string()),
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(other) => return Err(format!("unexpected {other:?}")),
        }
    }
}

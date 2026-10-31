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
        let key = cfg.key.borrow_and_update().clone();
        let dialed = tokio::select! {
            d = dial_first(&urls, &key, &cfg.coordinator) => d,
            // A new map mid-round (the server moved): start over with it.
            r = cfg.urls.changed() => if r.is_err() { return } else { continue },
        };
        let connected = match dialed {
            Some((url, ws)) => {
                serve(&url, ws, &mut cfg, &to_engine, &mut from_engine, &up).await;
                true
            }
            None => false,
        };
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

/// Pings every 25 s (the relay pings every 30): hearing nothing for longer
/// than this means the connection died without a reset.
const PING_EVERY: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 25 });
const SILENCE: Duration = Duration::from_secs(if cfg!(test) { 3 } else { 40 });

type Ws = tokio_tungstenite::WebSocketStream<Box<dyn http::Io>>;

/// How long each URL waits for the ones before it: the preferred one wins
/// when it answers quickly, and one that never answers (a LAN address seen
/// from another network) doesn't hold up the rest.
const HEAD_START: Duration = Duration::from_millis(300);

/// Open the URLs side by side, earlier ones a little ahead, and answer the
/// challenge on the first that opens (only one: the relay keeps the newest
/// connection of a key, so a second would replace it). If that one refuses,
/// the next to open gets its turn.
async fn dial_first(urls: &[String], key: &NodeKey, coordinator: &PublicKey) -> Option<(String, Ws)> {
    let mut dials: futures_util::stream::FuturesUnordered<_> = urls
        .iter()
        .enumerate()
        .map(|(i, url)| async move {
            tokio::time::sleep(HEAD_START * i as u32).await;
            (url, open(url).await)
        })
        .collect();
    while let Some((url, r)) = dials.next().await {
        match r {
            Ok(ws) => match answer(ws, key, coordinator).await {
                Ok(ws) => return Some((url.clone(), ws)),
                Err(e) => tracing::debug!("relay {url}: {e}"),
            },
            Err(e) => tracing::debug!("relay {url}: {e}"),
        }
    }
    None
}

/// Connect and open the WebSocket.
async fn open(url: &str) -> Result<Ws, String> {
    let u = http::parse_url(url).map_err(|e| e.to_string())?;
    let io = http::connect_io(&u, Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    let mut wsc = WebSocketConfig::default();
    wsc.max_message_size = Some(relay::MAX_MESSAGE);
    wsc.max_frame_size = Some(relay::MAX_MESSAGE);
    let (ws, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::client_async_with_config(url, io, Some(wsc)),
    )
    .await
    .map_err(|_| "handshake timed out".to_string())?
    .map_err(|e| e.to_string())?;
    Ok(ws)
}

/// The challenge, answered sealed with this node's key.
async fn answer(mut ws: Ws, key: &NodeKey, coordinator: &PublicKey) -> Result<Ws, String> {
    let text = next_text(&mut ws).await?;
    let ch: Challenge = serde_json::from_str(&text).map_err(|e| format!("challenge: {e}"))?;
    let session = NodeSession::new(key, coordinator).map_err(|e| e.to_string())?;
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
    Ok(ws)
}

/// One connection, until it drops or the URLs or key change.
async fn serve(
    url: &str,
    mut ws: Ws,
    cfg: &mut RelayConfig,
    to_engine: &mpsc::Sender<(PublicKey, Vec<u8>)>,
    from_engine: &mut mpsc::Receiver<(PublicKey, Vec<u8>)>,
    up: &AtomicBool,
) {
    // What queued while down is stale: WireGuard resends what matters.
    while from_engine.try_recv().is_ok() {}
    up.store(true, Ordering::Relaxed);
    tracing::info!("relay connected: {url}");
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut heard = tokio::time::Instant::now();
    loop {
        tokio::select! {
            m = ws.next() => {
                heard = tokio::time::Instant::now();
                match m {
                    Some(Ok(Message::Binary(b))) => {
                        if let Some((src, packet)) = relay::split(&b) {
                            let _ = to_engine.try_send((src, packet.to_vec()));
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::info!("relay {url}: {e}");
                        return;
                    }
                }
            }
            out = from_engine.recv() => match out {
                Some((dst, packet)) => {
                    if ws.send(Message::Binary(relay::frame(&dst, &packet).into())).await.is_err() {
                        return;
                    }
                }
                None => return,
            },
            _ = ping.tick() => {
                if heard.elapsed() > SILENCE {
                    tracing::info!("relay {url}: silent for {}s; reconnecting", heard.elapsed().as_secs());
                    return;
                }
                if ws.send(Message::Ping(Default::default())).await.is_err() {
                    return;
                }
            }
            // (A dropped sender: the key stays as it is.)
            r = cfg.key.changed(), if cfg.key.has_changed().is_ok() => {
                if r.is_ok() {
                    tracing::debug!("relay: the key changed; reconnecting");
                    let _ = ws.close(None).await;
                    return;
                }
            }
            r = cfg.urls.changed() => {
                if r.is_err() || !cfg.urls.borrow().iter().any(|x| x == url) {
                    let _ = ws.close(None).await;
                    return;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::NodeKey;

    /// A relay that greets with a challenge and accepts any answer.
    async fn relay() -> String {
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((c, _)) = server.accept().await {
                tokio::spawn(async move {
                    let mut ws = tokio_tungstenite::accept_async(c).await.unwrap();
                    let hello = r#"{"challenge":"c"}"#;
                    ws.send(Message::Text(hello.into())).await.unwrap();
                    let _ = ws.next().await;
                    ws.send(Message::Text(r#"{"ok":true}"#.into())).await.unwrap();
                    while ws.next().await.is_some() {}
                });
            }
        });
        format!("ws://127.0.0.1:{port}{}", relay::RELAY_PATH)
    }

    #[tokio::test]
    async fn a_relay_gone_silent_is_dropped() {
        // Accepts, then neither reads nor answers: a connection that died
        // without a reset.
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = vec![];
            while let Ok((c, _)) = server.accept().await {
                let mut ws = tokio_tungstenite::accept_async(c).await.unwrap();
                ws.send(Message::Text(r#"{"challenge":"c"}"#.into())).await.unwrap();
                let _ = ws.next().await;
                ws.send(Message::Text(r#"{"ok":true}"#.into())).await.unwrap();
                held.push(ws);
            }
        });
        let (_urls, rx) = watch::channel(vec![format!("ws://127.0.0.1:{port}{}", relay::RELAY_PATH)]);
        let (_key, key) = watch::channel(Arc::new(NodeKey::generate()));
        let link = spawn(RelayConfig {
            urls: rx,
            key,
            coordinator: NodeKey::generate().public(),
        });
        let started = std::time::Instant::now();
        while !link.up.load(Ordering::Relaxed) {
            assert!(started.elapsed() < Duration::from_secs(3), "never connected");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let up_at = std::time::Instant::now();
        while link.up.load(Ordering::Relaxed) {
            assert!(
                up_at.elapsed() < SILENCE + Duration::from_secs(2),
                "still up {:?} after going silent",
                up_at.elapsed()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn an_unreachable_address_doesnt_hold_up_the_next() {
        let good = relay().await;
        // TEST-NET-2: routed to the default gateway and never answered, like
        // the server's LAN address seen from another network.
        let (_urls, rx) = watch::channel(vec![format!("ws://198.51.100.1:7443{}", relay::RELAY_PATH), good]);
        let (_key, key) = watch::channel(Arc::new(NodeKey::generate()));
        let link = spawn(RelayConfig {
            urls: rx,
            key,
            coordinator: NodeKey::generate().public(),
        });
        let started = std::time::Instant::now();
        while !link.up.load(Ordering::Relaxed) {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "still not connected after {:?}",
                started.elapsed()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

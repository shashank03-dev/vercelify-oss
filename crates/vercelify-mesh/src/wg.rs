//! The WireGuard data plane (design §13.5–13.7): one UDP socket, one
//! `boringtun::noise::Tunn` per peer, driven by one task. Packets from the
//! node's network device go to the peer owning their destination address;
//! datagrams from the network are matched to a peer (by the key in a
//! handshake, by our index in the others), decrypted, checked against the
//! peer's own addresses, and handed to the device.
//!
//! The same socket carries disco (`disco.rs`: path discovery between
//! peers) and STUN (`stun.rs`: this node's public address, and answers for
//! others). Each peer's packets take its best confirmed direct path; with
//! none, they go through the relay when one is connected, and otherwise to
//! its candidate endpoints in turn.
//!
//! The device is a trait: a tun interface on Linux (`tun.rs`), a channel in
//! tests, a userspace TCP/IP stack on the Mac.

use crate::disco::{self, DiscoKey, Message, TxId};
use crate::keys::{NodeKey, PublicKey};
use crate::stun;
use boringtun::noise::{handshake::parse_handshake_anon, Packet, Tunn, TunnResult};
use boringtun::x25519;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

/// Largest packet we carry (the device MTU is 1280; leave room).
const MAX_PACKET: usize = 65_535;
/// WireGuard adds 32 bytes; handshakes are 148.
const OVERHEAD: usize = 32;
/// Seconds between keepalives (behind NAT a mapping lives about 30 s).
pub const KEEPALIVE_SECS: u16 = 25;
/// Try the next candidate endpoint when one hasn't answered for this long
/// (only without a relay: with one, packets wait there for a path).
const CANDIDATE_TIMEOUT: Duration = Duration::from_secs(4);
/// At most this many anonymous handshake parses and STUN answers per
/// second (a handshake parse is an X25519 operation; this bounds what a
/// flood of junk can cost).
const ANON_PER_SEC: u32 = 200;
/// Disco pings to every candidate: this often while no direct path is
/// confirmed, and this often to keep a confirmed one.
const PING_SEARCHING: Duration = Duration::from_secs(5);
const PING_CONFIRMED: Duration = Duration::from_secs(12);
/// A path without a pong (or a packet) for this long is gone: three
/// missed pings.
const PATH_STALE: Duration = Duration::from_secs(40);
/// Within this, a private (LAN) path wins over a faster public one.
const LAN_PREFERENCE: Duration = Duration::from_millis(2);
/// Pings waiting for a pong are forgotten after this.
const PING_TIMEOUT: Duration = Duration::from_secs(10);
/// `call_me` through the relay at most this often per peer.
const CALL_ME_EVERY: Duration = Duration::from_secs(15);
/// Candidates per peer at most (map endpoints first, then learned ones).
const MAX_CANDIDATES: usize = 16;
/// STUN servers are asked this often.
const STUN_EVERY: Duration = Duration::from_secs(60);
/// What this node learned about its own address is kept this long.
const OBSERVED_TTL: Duration = Duration::from_secs(300);

/// A packet device: whole IP packets in and out.
#[allow(async_fn_in_trait)]
pub trait Device: Send + Sync + 'static {
    async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize>;
    async fn send(&self, packet: &[u8]) -> std::io::Result<()>;
}

/// What a peer is, from the map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSpec {
    pub public_key: PublicKey,
    pub name: String,
    /// Its mesh addresses: the only sources accepted from it, and the
    /// destinations routed to it.
    pub addresses: Vec<IpAddr>,
    /// Where it may be reached, best first.
    pub endpoints: Vec<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PeerStatus {
    pub name: String,
    pub public_key: String,
    pub addresses: Vec<String>,
    /// Where its packets come from / go to now (None through the relay).
    pub endpoint: Option<String>,
    /// `direct` (a confirmed or working endpoint), `relay`, or `none`.
    pub path: &'static str,
    /// Round trip of the direct path, from disco.
    pub rtt_ms: Option<f64>,
    /// Seconds since the last completed handshake.
    pub handshake_secs: Option<u64>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

pub enum Control {
    SetPeers(Vec<PeerSpec>),
    /// The node's key changed (rotation): every tunnel re-handshakes.
    Rekey(NodeKey),
    Status(oneshot::Sender<Vec<PeerStatus>>),
    /// This node's own addresses (`ip:port` on its interfaces), sent to
    /// peers in `call_me`.
    SetLocal(Vec<SocketAddr>),
    /// STUN servers to learn this node's public address from.
    SetStun(Vec<SocketAddr>),
    /// The addresses peers and STUN servers saw this node at.
    Observed(oneshot::Sender<Vec<SocketAddr>>),
}

/// The engine's side of a relay connection (`relay_client.rs`).
pub struct RelayLink {
    /// (destination, packet) to send through the relay.
    pub tx: mpsc::Sender<(PublicKey, Vec<u8>)>,
    /// (source, packet) the relay delivered.
    pub rx: mpsc::Receiver<(PublicKey, Vec<u8>)>,
    /// Whether the connection is up now.
    pub up: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Udp(SocketAddr),
    Relay,
    Nowhere,
}

struct DirectPath {
    rtt: Duration,
    /// Last pong, or last packet from it.
    alive: Instant,
}

struct PeerState {
    spec: PeerSpec,
    tunn: Box<Tunn>,
    index: u32,
    disco: Option<DiscoKey>,
    /// Current endpoint: the best path, learned from authenticated
    /// packets, or a candidate.
    endpoint: Option<SocketAddr>,
    /// Learned from the peer itself (roaming) rather than the map.
    roamed: bool,
    candidate: usize,
    candidate_since: Instant,
    last_rx: Option<Instant>,
    /// Endpoints to ping: the map's, then what call_me and pings taught us.
    learned: Vec<SocketAddr>,
    paths: HashMap<SocketAddr, DirectPath>,
    pings: Vec<(TxId, SocketAddr, Instant)>,
    best: Option<SocketAddr>,
    next_ping: Instant,
    last_call_me: Option<Instant>,
    last_relay_rx: Option<Instant>,
    /// The way the last packet to it went.
    route: Route,
}

impl PeerState {
    fn new(key: &NodeKey, secret: &x25519::StaticSecret, spec: PeerSpec, index: u32) -> Self {
        let tunn = Tunn::new(
            secret.clone(),
            x25519::PublicKey::from(spec.public_key.0),
            None,
            Some(KEEPALIVE_SECS),
            index,
            None,
        );
        let endpoint = spec.endpoints.first().copied();
        let disco = DiscoKey::new(key, &spec.public_key).ok();
        Self {
            spec,
            tunn: Box::new(tunn),
            index,
            disco,
            endpoint,
            roamed: false,
            candidate: 0,
            candidate_since: Instant::now(),
            last_rx: None,
            learned: vec![],
            paths: HashMap::new(),
            pings: vec![],
            best: None,
            next_ping: Instant::now(),
            last_call_me: None,
            last_relay_rx: None,
            route: Route::Nowhere,
        }
    }

    fn handshake_age(&self) -> Option<Duration> {
        self.tunn.stats().0
    }

    fn candidates(&self) -> Vec<SocketAddr> {
        let mut v: Vec<SocketAddr> = vec![];
        for e in self.spec.endpoints.iter().chain(self.learned.iter()) {
            if !v.contains(e) {
                v.push(*e);
            }
        }
        v.truncate(MAX_CANDIDATES);
        v
    }

    fn learn(&mut self, a: SocketAddr) {
        if a.port() == 0 || a.ip().is_unspecified() || self.spec.endpoints.contains(&a) || self.learned.contains(&a) {
            return;
        }
        if self.learned.len() >= MAX_CANDIDATES {
            self.learned.remove(0);
        }
        self.learned.push(a);
    }

    fn best_valid(&self, now: Instant) -> bool {
        self.best
            .and_then(|b| self.paths.get(&b))
            .is_some_and(|p| now.duration_since(p.alive) < PATH_STALE)
    }

    /// Pick the best live direct path: the lowest round trip, a LAN path
    /// winning within 2 ms.
    fn select_best(&mut self, now: Instant) {
        self.paths.retain(|_, p| now.duration_since(p.alive) < PATH_STALE);
        let score = |a: &SocketAddr, p: &DirectPath| {
            let bonus = if disco::is_private(a) {
                LAN_PREFERENCE
            } else {
                Duration::ZERO
            };
            p.rtt.saturating_sub(bonus)
        };
        let best = self.paths.iter().min_by_key(|(a, p)| score(a, p)).map(|(a, _)| *a);
        if best != self.best {
            if let Some(b) = best {
                tracing::debug!("{}: direct path {b}", self.spec.name);
                self.endpoint = Some(b);
                self.roamed = false;
            }
            self.best = best;
        }
    }

    /// Where the next packet to this peer goes.
    fn route(&self, now: Instant, relay_up: bool) -> Route {
        if self.best_valid(now) {
            return Route::Udp(self.best.expect("valid"));
        }
        // An endpoint the peer's packets arrive from works both ways.
        let heard = self.roamed
            && self
                .last_rx
                .is_some_and(|t| now.duration_since(t) < Duration::from_secs(10));
        match self.endpoint {
            Some(e) if heard => Route::Udp(e),
            _ if relay_up => Route::Relay,
            Some(e) => Route::Udp(e),
            None => Route::Nowhere,
        }
    }

    /// Without a recent handshake, move on to the next candidate endpoint.
    /// Returns whether it moved (the caller handshakes there at once).
    fn maybe_next_candidate(&mut self, now: Instant) -> bool {
        let fresh = self.handshake_age().is_some_and(|a| a < Duration::from_secs(180));
        let heard = self
            .last_rx
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(30));
        if fresh || heard || self.best_valid(now) || self.spec.endpoints.len() < 2 {
            return false;
        }
        if now.duration_since(self.candidate_since) < CANDIDATE_TIMEOUT {
            return false;
        }
        self.candidate = (self.candidate + 1) % self.spec.endpoints.len();
        self.endpoint = Some(self.spec.endpoints[self.candidate]);
        self.roamed = false;
        self.candidate_since = now;
        true
    }
}

pub struct Engine<D: Device> {
    device: Arc<D>,
    socket: UdpSocket,
    key: NodeKey,
    secret: x25519::StaticSecret,
    public: x25519::PublicKey,
    peers: Vec<Option<PeerState>>,
    by_key: HashMap<PublicKey, usize>,
    by_addr: HashMap<IpAddr, usize>,
    next_index: u32,
    anon_budget: u32,
    anon_window: Instant,
    relay_tx: Option<mpsc::Sender<(PublicKey, Vec<u8>)>>,
    relay_rx: Option<mpsc::Receiver<(PublicKey, Vec<u8>)>>,
    relay_up: Arc<AtomicBool>,
    local: Vec<SocketAddr>,
    stun_servers: Vec<SocketAddr>,
    stun_pending: Vec<(stun::TxId, Instant)>,
    next_stun: Instant,
    observed: HashMap<SocketAddr, Instant>,
}

fn copy_key(k: &NodeKey) -> NodeKey {
    NodeKey::from_bytes(*k.secret_bytes())
}

impl<D: Device> Engine<D> {
    pub fn new(key: &NodeKey, socket: UdpSocket, device: Arc<D>) -> Self {
        let secret = x25519::StaticSecret::from(*key.secret_bytes());
        let public = x25519::PublicKey::from(&secret);
        Self {
            device,
            socket,
            key: copy_key(key),
            secret,
            public,
            peers: vec![],
            by_key: HashMap::new(),
            by_addr: HashMap::new(),
            next_index: 1,
            anon_budget: ANON_PER_SEC,
            anon_window: Instant::now(),
            relay_tx: None,
            relay_rx: None,
            relay_up: Arc::new(AtomicBool::new(false)),
            local: vec![],
            stun_servers: vec![],
            stun_pending: vec![],
            next_stun: Instant::now() + Duration::from_secs(1),
            observed: HashMap::new(),
        }
    }

    /// Send through `relay` when no direct path works.
    pub fn with_relay(mut self, relay: RelayLink) -> Self {
        self.relay_tx = Some(relay.tx);
        self.relay_rx = Some(relay.rx);
        self.relay_up = relay.up;
        self
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    fn relay_up(&self) -> bool {
        self.relay_tx.is_some() && self.relay_up.load(Ordering::Relaxed)
    }

    /// Replace the peer set: unchanged peers keep their sessions; changed
    /// endpoints are tried next; removed peers are forgotten at once.
    pub fn set_peers(&mut self, specs: Vec<PeerSpec>) {
        let wanted: HashMap<PublicKey, PeerSpec> = specs.into_iter().map(|s| (s.public_key, s)).collect();
        for slot in self.peers.iter_mut() {
            if let Some(p) = slot {
                if !wanted.contains_key(&p.spec.public_key) {
                    *slot = None;
                }
            }
        }
        for (key, spec) in wanted {
            match self.by_key.get(&key).and_then(|i| self.peers[*i].as_mut()) {
                Some(p) => {
                    if p.spec.endpoints != spec.endpoints {
                        if !p.roamed && !p.best_valid(Instant::now()) {
                            p.candidate = 0;
                            p.endpoint = spec.endpoints.first().copied().or(p.endpoint);
                            p.candidate_since = Instant::now();
                        }
                        // New endpoints: look for a path there now.
                        p.next_ping = Instant::now();
                    }
                    p.spec = spec;
                }
                None => {
                    let index = self.next_index & 0x00ff_ffff;
                    self.next_index = self.next_index.wrapping_add(1).max(1);
                    self.peers
                        .push(Some(PeerState::new(&self.key, &self.secret, spec, index)));
                }
            }
        }
        self.reindex();
    }

    fn reindex(&mut self) {
        self.by_key.clear();
        self.by_addr.clear();
        // Compact: drop removed slots, keep indexes stable in the tunnels.
        self.peers.retain(|p| p.is_some());
        for (i, p) in self.peers.iter().enumerate() {
            let p = p.as_ref().expect("retained");
            self.by_key.insert(p.spec.public_key, i);
            for a in &p.spec.addresses {
                self.by_addr.insert(*a, i);
            }
        }
    }

    pub fn rekey(&mut self, key: &NodeKey) {
        self.key = copy_key(key);
        self.secret = x25519::StaticSecret::from(*key.secret_bytes());
        self.public = x25519::PublicKey::from(&self.secret);
        for p in self.peers.iter_mut().flatten() {
            p.tunn.set_static_private(self.secret.clone(), self.public, None);
            p.disco = DiscoKey::new(key, &p.spec.public_key).ok();
            p.next_ping = Instant::now();
        }
    }

    pub fn status(&self) -> Vec<PeerStatus> {
        let now = Instant::now();
        let relay_up = self.relay_up();
        self.peers
            .iter()
            .flatten()
            .map(|p| {
                let (hs, tx, rx, _, _) = p.tunn.stats();
                let route = p.route(now, relay_up);
                let live = hs.is_some_and(|d| d < Duration::from_secs(180));
                let (endpoint, path) = match route {
                    Route::Udp(e) => (
                        Some(e.to_string()),
                        if live || p.best_valid(now) { "direct" } else { "none" },
                    ),
                    Route::Relay => (None, if live { "relay" } else { "none" }),
                    Route::Nowhere => (None, "none"),
                };
                PeerStatus {
                    name: p.spec.name.clone(),
                    public_key: p.spec.public_key.to_base64(),
                    addresses: p.spec.addresses.iter().map(|a| a.to_string()).collect(),
                    endpoint,
                    path,
                    rtt_ms: p
                        .best
                        .filter(|_| p.best_valid(now))
                        .and_then(|b| p.paths.get(&b))
                        .map(|d| (d.rtt.as_secs_f64() * 10_000.0).round() / 10.0),
                    handshake_secs: hs.map(|d| d.as_secs()),
                    rx_bytes: rx as u64,
                    tx_bytes: tx as u64,
                }
            })
            .collect()
    }

    fn observed(&self) -> Vec<SocketAddr> {
        let now = Instant::now();
        let mut v: Vec<(SocketAddr, Instant)> = self
            .observed
            .iter()
            .filter(|(_, t)| now.duration_since(**t) < OBSERVED_TTL)
            .map(|(a, t)| (*a, *t))
            .collect();
        v.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
        v.into_iter().map(|(a, _)| a).take(8).collect()
    }

    async fn send_udp(&self, data: &[u8], to: SocketAddr) {
        let _ = self.socket.send_to(data, to).await;
    }

    /// A WireGuard message for peer `i`, the way its packets go now.
    async fn send_wg(&mut self, i: usize, data: &[u8]) {
        let now = Instant::now();
        let relay_up = self.relay_up();
        let Some(p) = self.peers[i].as_mut() else { return };
        let route = p.route(now, relay_up);
        p.route = route;
        let key = p.spec.public_key;
        match route {
            Route::Udp(to) => self.send_udp(data, to).await,
            Route::Relay => self.send_relay(key, data.to_vec()),
            Route::Nowhere => {}
        }
    }

    fn send_relay(&self, to: PublicKey, data: Vec<u8>) {
        if let Some(tx) = &self.relay_tx {
            // Full: drop, as a congested link would.
            let _ = tx.try_send((to, data));
        }
    }

    /// A packet from the device: encrypt it for the peer owning its destination.
    async fn on_device_packet(&mut self, packet: &[u8], out: &mut [u8]) {
        let Some(dst) = Tunn::dst_address(packet) else { return };
        let Some(&i) = self.by_addr.get(&dst) else { return };
        let Some(p) = self.peers[i].as_mut() else { return };
        match p.tunn.encapsulate(packet, out) {
            TunnResult::WriteToNetwork(b) => {
                let data = b.to_vec();
                self.send_wg(i, &data).await;
            }
            TunnResult::Err(e) => tracing::debug!("encapsulate for {}: {e:?}", p.spec.name),
            _ => {}
        }
    }

    fn anon_allowed(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.anon_window) >= Duration::from_secs(1) {
            self.anon_window = now;
            self.anon_budget = ANON_PER_SEC;
        }
        if self.anon_budget == 0 {
            return false;
        }
        self.anon_budget -= 1;
        true
    }

    /// Which peer a WireGuard datagram is for.
    fn peer_of(&mut self, datagram: &[u8]) -> Option<usize> {
        match Tunn::parse_incoming_packet(datagram).ok()? {
            Packet::HandshakeInit(h) => {
                if !self.anon_allowed() {
                    return None;
                }
                let half = parse_handshake_anon(&self.secret, &self.public, &h).ok()?;
                self.by_key.get(&PublicKey(half.peer_static_public)).copied()
            }
            Packet::HandshakeResponse(h) => self.by_index(h.receiver_idx),
            Packet::PacketCookieReply(c) => self.by_index(c.receiver_idx),
            Packet::PacketData(d) => self.by_index(d.receiver_idx),
        }
    }

    fn by_index(&self, receiver_idx: u32) -> Option<usize> {
        let idx = receiver_idx >> 8;
        self.peers
            .iter()
            .position(|p| p.as_ref().is_some_and(|p| p.index == idx))
    }

    /// A datagram from the network: STUN, disco or WireGuard.
    async fn on_datagram(&mut self, datagram: &[u8], from: SocketAddr, out: &mut [u8]) {
        if stun::is_stun(datagram) {
            self.on_stun(datagram, from).await;
        } else if disco::is_disco(datagram) {
            self.on_disco(datagram, Some(from)).await;
        } else if let Some(i) = self.peer_of(datagram) {
            self.on_wg(i, datagram, Some(from), out).await;
        }
    }

    /// A message the relay delivered from `src`.
    async fn on_relayed(&mut self, src: PublicKey, data: &[u8], out: &mut [u8]) {
        if disco::is_disco(data) {
            // Only its own sender may speak for a key.
            if disco::sender(data) == Some(src) {
                self.on_disco(data, None).await;
            }
            return;
        }
        let Some(&i) = self.by_key.get(&src) else { return };
        if let Some(p) = self.peers[i].as_mut() {
            p.last_relay_rx = Some(Instant::now());
        }
        self.on_wg(i, data, None, out).await;
    }

    async fn on_stun(&mut self, datagram: &[u8], from: SocketAddr) {
        if let Some((tx, addr)) = stun::parse_success(datagram) {
            if let Some(pos) = self.stun_pending.iter().position(|(t, _)| *t == tx) {
                self.stun_pending.remove(pos);
                self.observed.insert(addr, Instant::now());
            }
        } else if self.anon_allowed() {
            if let Some(answer) = stun::answer(datagram, from) {
                self.send_udp(&answer, from).await;
            }
        }
    }

    async fn on_disco(&mut self, frame: &[u8], from: Option<SocketAddr>) {
        let Some(sender) = disco::sender(frame) else { return };
        let Some(&i) = self.by_key.get(&sender) else { return };
        let me = PublicKey(self.public.to_bytes());
        let now = Instant::now();
        let Some(p) = self.peers[i].as_mut() else { return };
        let Some(msg) = p.disco.as_ref().and_then(|k| k.open(frame)) else {
            return;
        };
        match (msg, from) {
            (Message::Ping { tx }, Some(from)) => {
                // It reached us from there, so we can likely reach it there.
                p.learn(from);
                let reply = p
                    .disco
                    .as_ref()
                    .expect("opened")
                    .seal(&me, &Message::Pong { tx, observed: from });
                self.send_udp(&reply, from).await;
            }
            (Message::Pong { tx, observed }, Some(from)) => {
                let Some(pos) = p.pings.iter().position(|(t, _, _)| *t == tx) else {
                    return;
                };
                let (_, to, sent) = p.pings.swap_remove(pos);
                if to != from {
                    return;
                }
                p.paths.insert(
                    to,
                    DirectPath {
                        rtt: now.duration_since(sent),
                        alive: now,
                    },
                );
                p.select_best(now);
                self.observed.insert(observed, now);
            }
            (Message::CallMe { endpoints }, _) => {
                for e in endpoints {
                    p.learn(e);
                }
                // Ping now: the peer is pinging us at this moment too.
                p.next_ping = now;
            }
            _ => {}
        }
    }

    async fn on_wg(&mut self, i: usize, datagram: &[u8], from: Option<SocketAddr>, out: &mut [u8]) {
        let mut first = true;
        loop {
            let input: &[u8] = if first { datagram } else { &[] };
            let src = if first { from.map(|f| f.ip()) } else { None };
            first = false;
            let p = match self.peers[i].as_mut() {
                Some(p) => p,
                None => return,
            };
            let res = p.tunn.decapsulate(src, input, out);
            let ok = !matches!(res, TunnResult::Done | TunnResult::Err(_));
            if ok {
                p.last_rx = Some(Instant::now());
                if let Some(from) = from {
                    // Authenticated: the peer is at `from` now (roaming).
                    let now = Instant::now();
                    match p.paths.get_mut(&from) {
                        // A path disco confirmed: it's alive; the best stays the best.
                        Some(path) if now.duration_since(path.alive) < PATH_STALE => path.alive = now,
                        // Somewhere new (a restart, a NAT rebinding): follow it
                        // at once, and let disco confirm the best path again.
                        _ => {
                            if p.endpoint != Some(from) {
                                tracing::debug!("{}: now at {from}", p.spec.name);
                                p.best = None;
                                p.paths.remove(&from);
                                p.next_ping = now;
                            }
                            p.endpoint = Some(from);
                            p.roamed = true;
                        }
                    }
                    p.learn(from);
                }
            }
            match res {
                TunnResult::Done => break,
                TunnResult::Err(e) => {
                    tracing::debug!("from {from:?} for {}: {e:?}", p.spec.name);
                    break;
                }
                TunnResult::WriteToNetwork(b) => {
                    // A handshake answer or queued packets: back the way it came.
                    let data = b.to_vec();
                    let key = p.spec.public_key;
                    match from {
                        Some(f) => self.send_udp(&data, f).await,
                        None => self.send_relay(key, data),
                    }
                    // Repeat with empty input to flush what was queued.
                }
                TunnResult::WriteToTunnelV4(b, src_ip) => {
                    // Only the peer's own addresses (WireGuard's allowed IPs).
                    if p.spec.addresses.contains(&IpAddr::V4(src_ip)) {
                        let len = b.len();
                        let _ = self.device.send(&out[..len]).await;
                    } else {
                        tracing::debug!("{} sent a packet from {src_ip}: dropped", p.spec.name);
                    }
                    break;
                }
                TunnResult::WriteToTunnelV6(b, src_ip) => {
                    if p.spec.addresses.contains(&IpAddr::V6(src_ip)) {
                        let len = b.len();
                        let _ = self.device.send(&out[..len]).await;
                    }
                    break;
                }
            }
        }
    }

    /// Disco for peer `i` when due: pings to every candidate, and
    /// `call_me` through the relay while no direct path is confirmed.
    async fn disco_round(&mut self, i: usize, now: Instant) {
        let me = PublicKey(self.public.to_bytes());
        let relay_up = self.relay_up();
        let mut ours: Vec<SocketAddr> = self.local.clone();
        ours.extend(self.observed());
        let Some(p) = self.peers[i].as_mut() else { return };
        if now < p.next_ping || p.disco.is_none() {
            return;
        }
        p.pings.retain(|(_, _, t)| now.duration_since(*t) < PING_TIMEOUT);
        p.select_best(now);
        let confirmed = p.best_valid(now);
        p.next_ping = now + if confirmed { PING_CONFIRMED } else { PING_SEARCHING };
        let mut sends: Vec<(SocketAddr, Vec<u8>)> = vec![];
        for to in p.candidates() {
            let tx = disco::new_tx();
            if p.pings.len() >= 64 {
                p.pings.remove(0);
            }
            p.pings.push((tx, to, now));
            let frame = p.disco.as_ref().expect("checked").seal(&me, &Message::Ping { tx });
            sends.push((to, frame));
        }
        let call_me = (!confirmed && relay_up && p.last_call_me.is_none_or(|t| now.duration_since(t) >= CALL_ME_EVERY))
            .then(|| {
                p.last_call_me = Some(now);
                ours.truncate(disco::MAX_CALL_ME);
                (
                    p.spec.public_key,
                    p.disco
                        .as_ref()
                        .expect("checked")
                        .seal(&me, &Message::CallMe { endpoints: ours }),
                )
            });
        for (to, frame) in sends {
            self.send_udp(&frame, to).await;
        }
        if let Some((key, frame)) = call_me {
            self.send_relay(key, frame);
        }
    }

    async fn stun_round(&mut self, now: Instant) {
        if now < self.next_stun || self.stun_servers.is_empty() {
            return;
        }
        self.next_stun = now + STUN_EVERY;
        self.stun_pending.retain(|(_, t)| now.duration_since(*t) < PING_TIMEOUT);
        for server in self.stun_servers.clone() {
            let tx = disco::new_tx();
            self.stun_pending.push((tx, now));
            self.send_udp(&stun::binding_request(&tx), server).await;
        }
    }

    /// Handshake retries, keepalives, session expiry; disco, STUN and
    /// candidate rotation.
    async fn timers(&mut self, out: &mut [u8]) {
        let now = Instant::now();
        let relay_up = self.relay_up();
        self.stun_round(now).await;
        for i in 0..self.peers.len() {
            self.disco_round(i, now).await;
            let Some(p) = self.peers[i].as_mut() else { continue };
            // Without a relay, step through the candidates for a handshake.
            if !relay_up && p.maybe_next_candidate(now) {
                // Don't wait for the 5 s handshake retry: try the new candidate now.
                if let TunnResult::WriteToNetwork(b) = p.tunn.format_handshake_initiation(out, true) {
                    let data = b.to_vec();
                    self.send_wg(i, &data).await;
                }
                continue;
            }
            let Some(p) = self.peers[i].as_mut() else { continue };
            match p.tunn.update_timers(out) {
                TunnResult::WriteToNetwork(b) => {
                    let data = b.to_vec();
                    self.send_wg(i, &data).await;
                }
                TunnResult::Err(e) => tracing::trace!("timers: {e:?}"),
                _ => {}
            }
        }
    }

    /// Run until the control channel closes.
    pub async fn run(mut self, mut control: mpsc::Receiver<Control>) {
        let mut net_buf = vec![0u8; MAX_PACKET];
        let mut dev_buf = vec![0u8; MAX_PACKET];
        let mut out = vec![0u8; MAX_PACKET + OVERHEAD];
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let device = self.device.clone();
        let mut relay_rx = self.relay_rx.take();
        loop {
            tokio::select! {
                r = self.socket.recv_from(&mut net_buf) => match r {
                    Ok((n, from)) => {
                        let d = net_buf[..n].to_vec();
                        self.on_datagram(&d, from, &mut out).await;
                    }
                    Err(e) => tracing::debug!("udp: {e}"),
                },
                r = device.recv(&mut dev_buf) => match r {
                    Ok(n) if n > 0 => {
                        let pkt = dev_buf[..n].to_vec();
                        self.on_device_packet(&pkt, &mut out).await;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("device: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                },
                m = recv_relay(&mut relay_rx) => match m {
                    Some((src, data)) => self.on_relayed(src, &data, &mut out).await,
                    None => relay_rx = None,
                },
                _ = tick.tick() => self.timers(&mut out).await,
                c = control.recv() => match c {
                    Some(Control::SetPeers(p)) => self.set_peers(p),
                    Some(Control::Rekey(k)) => self.rekey(&k),
                    Some(Control::Status(tx)) => { let _ = tx.send(self.status()); }
                    Some(Control::SetLocal(l)) => self.local = l,
                    Some(Control::SetStun(s)) => {
                        if s != self.stun_servers {
                            self.stun_servers = s;
                            self.next_stun = Instant::now();
                        }
                    }
                    Some(Control::Observed(tx)) => { let _ = tx.send(self.observed()); }
                    None => return,
                },
            }
        }
    }
}

async fn recv_relay(rx: &mut Option<mpsc::Receiver<(PublicKey, Vec<u8>)>>) -> Option<(PublicKey, Vec<u8>)> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// A device made of two channels: what the "kernel" sends, and what
    /// the engine delivered.
    struct ChanDevice {
        rx: Mutex<mpsc::Receiver<Vec<u8>>>,
        delivered: mpsc::Sender<Vec<u8>>,
    }

    impl Device for ChanDevice {
        async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.rx.lock().await.recv().await {
                Some(p) => {
                    buf[..p.len()].copy_from_slice(&p);
                    Ok(p.len())
                }
                None => std::future::pending().await,
            }
        }
        async fn send(&self, packet: &[u8]) -> std::io::Result<()> {
            let _ = self.delivered.send(packet.to_vec()).await;
            Ok(())
        }
    }

    /// A minimal IPv4/UDP packet from `src` to `dst` carrying `payload`.
    fn ipv4(src: Ipv4Addr, dst: Ipv4Addr, payload: &[u8]) -> Vec<u8> {
        let total = 20 + 8 + payload.len();
        let mut p = vec![0u8; total];
        p[0] = 0x45;
        p[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        p[8] = 64;
        p[9] = 17;
        p[12..16].copy_from_slice(&src.octets());
        p[16..20].copy_from_slice(&dst.octets());
        let mut sum: u32 = 0;
        for c in p[..20].chunks(2) {
            sum += u16::from_be_bytes([c[0], c[1]]) as u32;
        }
        while sum > 0xffff {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        p[10..12].copy_from_slice(&(!(sum as u16)).to_be_bytes());
        p[20..22].copy_from_slice(&1000u16.to_be_bytes());
        p[22..24].copy_from_slice(&2000u16.to_be_bytes());
        p[24..26].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        p[28..].copy_from_slice(payload);
        p
    }

    struct Node {
        key: NodeKey,
        ip: Ipv4Addr,
        addr: SocketAddr,
        inject: mpsc::Sender<Vec<u8>>,
        delivered: mpsc::Receiver<Vec<u8>>,
        control: mpsc::Sender<Control>,
    }

    async fn node(ip: Ipv4Addr) -> (Node, Engine<ChanDevice>, mpsc::Receiver<Control>) {
        let key = NodeKey::generate();
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        let (inject, rx) = mpsc::channel(64);
        let (dtx, delivered) = mpsc::channel(64);
        let dev = Arc::new(ChanDevice {
            rx: Mutex::new(rx),
            delivered: dtx,
        });
        let engine = Engine::new(&key, sock, dev);
        let (control, crx) = mpsc::channel(8);
        (
            Node {
                key,
                ip,
                addr,
                inject,
                delivered,
                control,
            },
            engine,
            crx,
        )
    }

    fn spec(n: &Node, endpoints: Vec<SocketAddr>) -> PeerSpec {
        PeerSpec {
            public_key: n.key.public(),
            name: n.ip.to_string(),
            addresses: vec![IpAddr::V4(n.ip)],
            endpoints,
        }
    }

    async fn expect_packet(rx: &mut mpsc::Receiver<Vec<u8>>, payload: &[u8]) -> Vec<u8> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let p = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("a packet in time")
                .expect("open");
            if p.ends_with(payload) {
                return p;
            }
        }
    }

    #[tokio::test]
    async fn two_nodes_talk_and_only_with_their_own_addresses() {
        let (mut a, mut ea, ca) = node(Ipv4Addr::new(10, 77, 0, 1)).await;
        let (mut b, mut eb, cb) = node(Ipv4Addr::new(10, 77, 0, 2)).await;
        // A knows a wrong endpoint first, then the right one: the engine
        // moves to the next candidate when the first stays silent.
        let dead: SocketAddr = "127.0.0.1:9".parse().unwrap();
        ea.set_peers(vec![spec(&b, vec![dead, b.addr])]);
        eb.set_peers(vec![spec(&a, vec![])]); // B learns A's endpoint from A.
        tokio::spawn(ea.run(ca));
        tokio::spawn(eb.run(cb));

        a.inject.send(ipv4(a.ip, b.ip, b"hello b")).await.unwrap();
        let got = expect_packet(&mut b.delivered, b"hello b").await;
        assert_eq!(&got[12..16], &a.ip.octets());
        // And back, to the endpoint B learned from A's packets.
        b.inject.send(ipv4(b.ip, a.ip, b"hello a")).await.unwrap();
        expect_packet(&mut a.delivered, b"hello a").await;

        // A packet for an address nobody owns goes nowhere; one claiming a
        // source that isn't the sender's is dropped by the receiver.
        a.inject
            .send(ipv4(a.ip, Ipv4Addr::new(10, 77, 0, 9), b"lost"))
            .await
            .unwrap();
        a.inject
            .send(ipv4(Ipv4Addr::new(10, 77, 0, 5), b.ip, b"spoofed"))
            .await
            .unwrap();
        a.inject.send(ipv4(a.ip, b.ip, b"after")).await.unwrap();
        let got = expect_packet(&mut b.delivered, b"after").await;
        assert!(!got.ends_with(b"spoofed"));
        assert!(b.delivered.try_recv().is_err(), "nothing else arrived");

        let (tx, rx) = oneshot::channel();
        a.control.send(Control::Status(tx)).await.unwrap();
        let st = rx.await.unwrap();
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].endpoint.as_deref(), Some(b.addr.to_string().as_str()));
        assert!(st[0].handshake_secs.is_some());
        assert!(st[0].tx_bytes > 0 && st[0].rx_bytes > 0);

        // Removing a peer stops traffic to it.
        a.control.send(Control::SetPeers(vec![])).await.unwrap();
        // Control messages are handled in order: once this answers, the
        // peer is gone.
        let (tx, rx) = oneshot::channel();
        a.control.send(Control::Status(tx)).await.unwrap();
        assert!(rx.await.unwrap().is_empty());
        a.inject.send(ipv4(a.ip, b.ip, b"revoked")).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(800), expect_packet(&mut b.delivered, b"revoked"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn key_rotation_keeps_the_link() {
        let (mut a, mut ea, ca) = node(Ipv4Addr::new(10, 77, 0, 1)).await;
        let (mut b, mut eb, cb) = node(Ipv4Addr::new(10, 77, 0, 2)).await;
        ea.set_peers(vec![spec(&b, vec![b.addr])]);
        eb.set_peers(vec![spec(&a, vec![a.addr])]);
        tokio::spawn(ea.run(ca));
        tokio::spawn(eb.run(cb));
        a.inject.send(ipv4(a.ip, b.ip, b"one")).await.unwrap();
        expect_packet(&mut b.delivered, b"one").await;

        // A rotates its key; B learns the new one from the next map.
        let new = NodeKey::generate();
        let new_public = new.public();
        a.control.send(Control::Rekey(new)).await.unwrap();
        let mut a_spec = spec(&a, vec![a.addr]);
        a_spec.public_key = new_public;
        b.control.send(Control::SetPeers(vec![a_spec])).await.unwrap();
        // A packet still sealed under the old session while the two switch
        // is lost, as on any WireGuard rekey: resend until one arrives, as
        // TCP would.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            a.inject.send(ipv4(a.ip, b.ip, b"two")).await.unwrap();
            match tokio::time::timeout(Duration::from_millis(500), expect_packet(&mut b.delivered, b"two")).await {
                Ok(_) => break,
                Err(_) => assert!(tokio::time::Instant::now() < deadline, "no link after the rotation"),
            }
        }
        b.inject.send(ipv4(b.ip, a.ip, b"three")).await.unwrap();
        expect_packet(&mut a.delivered, b"three").await;
    }

    #[tokio::test]
    async fn strangers_are_ignored() {
        let (a, mut ea, ca) = node(Ipv4Addr::new(10, 77, 0, 1)).await;
        let (mut b, eb, cb) = node(Ipv4Addr::new(10, 77, 0, 2)).await;
        // A knows B, B doesn't know A: A's handshakes are dropped.
        ea.set_peers(vec![spec(&b, vec![b.addr])]);
        tokio::spawn(ea.run(ca));
        tokio::spawn(eb.run(cb));
        a.inject.send(ipv4(a.ip, b.ip, b"knock")).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(1500), expect_packet(&mut b.delivered, b"knock"))
                .await
                .is_err()
        );
    }

    async fn status_of(n: &Node) -> Vec<PeerStatus> {
        let (tx, rx) = oneshot::channel();
        n.control.send(Control::Status(tx)).await.unwrap();
        rx.await.unwrap()
    }

    async fn until<F: Fn(&[PeerStatus]) -> bool>(n: &Node, what: &str, f: F) -> Vec<PeerStatus> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let st = status_of(n).await;
            if f(&st) {
                return st;
            }
            assert!(tokio::time::Instant::now() < deadline, "{what}: {st:?}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[tokio::test]
    async fn disco_confirms_the_direct_path_and_teaches_our_address() {
        let (mut a, mut ea, ca) = node(Ipv4Addr::new(10, 77, 0, 1)).await;
        let (b, mut eb, cb) = node(Ipv4Addr::new(10, 77, 0, 2)).await;
        ea.set_peers(vec![spec(&b, vec![b.addr])]);
        eb.set_peers(vec![spec(&a, vec![a.addr])]);
        tokio::spawn(ea.run(ca));
        tokio::spawn(eb.run(cb));
        b.inject.send(ipv4(b.ip, a.ip, b"hi")).await.unwrap();
        expect_packet(&mut a.delivered, b"hi").await;
        let st = until(&a, "a confirmed path", |s| s[0].rtt_ms.is_some()).await;
        assert_eq!(st[0].path, "direct");
        assert_eq!(st[0].endpoint.as_deref(), Some(b.addr.to_string().as_str()));
        // B's pongs told A where B sees it.
        let (tx, rx) = oneshot::channel();
        a.control.send(Control::Observed(tx)).await.unwrap();
        assert_eq!(rx.await.unwrap(), vec![a.addr]);
    }

    /// Two relay links joined by a task, as the coordinator's hub joins them.
    fn relay_pair(a: PublicKey, b: PublicKey) -> (RelayLink, RelayLink) {
        let (a_out, mut a_hub) = mpsc::channel::<(PublicKey, Vec<u8>)>(64);
        let (b_out, mut b_hub) = mpsc::channel::<(PublicKey, Vec<u8>)>(64);
        let (a_in_tx, a_in) = mpsc::channel(64);
        let (b_in_tx, b_in) = mpsc::channel(64);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some((dst, p)) = a_hub.recv() => if dst == b { let _ = b_in_tx.send((a, p)).await; },
                    Some((dst, p)) = b_hub.recv() => if dst == a { let _ = a_in_tx.send((b, p)).await; },
                    else => return,
                }
            }
        });
        let up = || Arc::new(AtomicBool::new(true));
        (
            RelayLink {
                tx: a_out,
                rx: a_in,
                up: up(),
            },
            RelayLink {
                tx: b_out,
                rx: b_in,
                up: up(),
            },
        )
    }

    #[tokio::test]
    async fn the_relay_carries_traffic_when_no_path_exists() {
        let (mut a, ea, ca) = node(Ipv4Addr::new(10, 77, 0, 1)).await;
        let (mut b, eb, cb) = node(Ipv4Addr::new(10, 77, 0, 2)).await;
        let (la, lb) = relay_pair(a.key.public(), b.key.public());
        let (mut ea, mut eb) = (ea.with_relay(la), eb.with_relay(lb));
        // Neither knows where the other is.
        ea.set_peers(vec![spec(&b, vec![])]);
        eb.set_peers(vec![spec(&a, vec![])]);
        tokio::spawn(ea.run(ca));
        tokio::spawn(eb.run(cb));
        a.inject.send(ipv4(a.ip, b.ip, b"via relay")).await.unwrap();
        expect_packet(&mut b.delivered, b"via relay").await;
        b.inject.send(ipv4(b.ip, a.ip, b"and back")).await.unwrap();
        expect_packet(&mut a.delivered, b"and back").await;
        let st = until(&a, "relay path", |s| s[0].path == "relay").await;
        assert!(st[0].endpoint.is_none());
    }

    #[tokio::test]
    async fn call_me_through_the_relay_finds_a_direct_path() {
        let (mut a, ea, ca) = node(Ipv4Addr::new(10, 77, 0, 1)).await;
        let (b, eb, cb) = node(Ipv4Addr::new(10, 77, 0, 2)).await;
        let (la, lb) = relay_pair(a.key.public(), b.key.public());
        let (mut ea, mut eb) = (ea.with_relay(la), eb.with_relay(lb));
        ea.set_peers(vec![spec(&b, vec![])]);
        eb.set_peers(vec![spec(&a, vec![])]);
        // Each knows its own address (as `local_ipv4s` gives the daemon).
        a.control.send(Control::SetLocal(vec![a.addr])).await.unwrap();
        b.control.send(Control::SetLocal(vec![b.addr])).await.unwrap();
        tokio::spawn(ea.run(ca));
        tokio::spawn(eb.run(cb));
        let st = until(&a, "a direct path after call_me", |s| s[0].rtt_ms.is_some()).await;
        assert_eq!(st[0].endpoint.as_deref(), Some(b.addr.to_string().as_str()));
        until(&b, "b direct path after call_me", |s| s[0].rtt_ms.is_some()).await;
        b.inject.send(ipv4(b.ip, a.ip, b"direct now")).await.unwrap();
        expect_packet(&mut a.delivered, b"direct now").await;
        let st = until(&a, "direct", |s| s[0].path == "direct").await;
        assert!(st[0].endpoint.is_some());
    }

    #[tokio::test]
    async fn follows_a_peer_that_moved() {
        let (mut a, mut ea, ca) = node(Ipv4Addr::new(10, 77, 0, 1)).await;
        let (b, mut eb, cb) = node(Ipv4Addr::new(10, 77, 0, 2)).await;
        ea.set_peers(vec![spec(&b, vec![b.addr])]);
        eb.set_peers(vec![spec(&a, vec![a.addr])]);
        tokio::spawn(ea.run(ca));
        let b_task = tokio::spawn(eb.run(cb));
        b.inject.send(ipv4(b.ip, a.ip, b"before")).await.unwrap();
        expect_packet(&mut a.delivered, b"before").await;
        until(&a, "a confirmed path to b", |s| s[0].rtt_ms.is_some()).await;

        // B restarts with the same key on another port (a new socket, a
        // new session); A's map still has the old address.
        b_task.abort();
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let moved = sock.local_addr().unwrap();
        let (inject, rx) = mpsc::channel(64);
        let (dtx, mut delivered) = mpsc::channel(64);
        let dev = Arc::new(ChanDevice {
            rx: Mutex::new(rx),
            delivered: dtx,
        });
        let mut eb2 = Engine::new(&b.key, sock, dev);
        eb2.set_peers(vec![spec(&a, vec![a.addr])]);
        let (_c2, cb2) = mpsc::channel(8);
        tokio::spawn(eb2.run(cb2));
        let started = tokio::time::Instant::now();
        inject.send(ipv4(b.ip, a.ip, b"after the move")).await.unwrap();
        expect_packet(&mut a.delivered, b"after the move").await;
        // A answers at the new address right away, not after the old path expires.
        a.inject.send(ipv4(a.ip, b.ip, b"answer")).await.unwrap();
        expect_packet(&mut delivered, b"answer").await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        let st = until(&a, "a follows b", |s| {
            s[0].endpoint.as_deref() == Some(moved.to_string().as_str())
        })
        .await;
        assert_eq!(st[0].path, "direct");
    }

    #[tokio::test]
    async fn answers_stun_and_learns_from_it() {
        let (a, mut ea, ca) = node(Ipv4Addr::new(10, 77, 0, 1)).await;
        let (b, eb, cb) = node(Ipv4Addr::new(10, 77, 0, 2)).await;
        ea.set_peers(vec![]);
        tokio::spawn(ea.run(ca));
        tokio::spawn(eb.run(cb));
        // A asks B (any node is a STUN server).
        a.control.send(Control::SetStun(vec![b.addr])).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let (tx, rx) = oneshot::channel();
            a.control.send(Control::Observed(tx)).await.unwrap();
            if rx.await.unwrap() == vec![a.addr] {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "no STUN answer");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        drop(b);
    }
}

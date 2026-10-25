//! The WireGuard data plane (design §13.5): one UDP socket, one
//! `boringtun::noise::Tunn` per peer, driven by one task. Packets from the
//! node's network device go to the peer owning their destination address;
//! datagrams from the network are matched to a peer (by the key in a
//! handshake, by our index in the others), decrypted, checked against the
//! peer's own addresses, and handed to the device.
//!
//! The device is a trait: a tun interface on Linux (`tun.rs`), a channel in
//! tests, a userspace TCP/IP stack on the Mac later.

use crate::keys::{NodeKey, PublicKey};
use boringtun::noise::{handshake::parse_handshake_anon, Packet, Tunn, TunnResult};
use boringtun::x25519;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

/// Largest packet we carry (the device MTU is 1280; leave room).
const MAX_PACKET: usize = 65_535;
/// WireGuard adds 32 bytes; handshakes are 148.
const OVERHEAD: usize = 32;
/// Seconds between keepalives (behind NAT a mapping lives about 30 s).
pub const KEEPALIVE_SECS: u16 = 25;
/// Try the next candidate endpoint when one hasn't answered for this long.
const CANDIDATE_TIMEOUT: Duration = Duration::from_secs(4);
/// At most this many anonymous handshake parses per second (each is an
/// X25519 operation; this bounds what a flood of junk can cost).
const ANON_PER_SEC: u32 = 200;

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
    /// Where its packets come from / go to now.
    pub endpoint: Option<String>,
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
}

struct PeerState {
    spec: PeerSpec,
    tunn: Box<Tunn>,
    index: u32,
    /// Current endpoint: learned from authenticated packets, or a candidate.
    endpoint: Option<SocketAddr>,
    /// Learned from the peer itself (roaming) rather than the map.
    roamed: bool,
    candidate: usize,
    candidate_since: Instant,
    last_rx: Option<Instant>,
}

impl PeerState {
    fn new(key: &x25519::StaticSecret, spec: PeerSpec, index: u32) -> Self {
        let tunn = Tunn::new(
            key.clone(),
            x25519::PublicKey::from(spec.public_key.0),
            None,
            Some(KEEPALIVE_SECS),
            index,
            None,
        );
        let endpoint = spec.endpoints.first().copied();
        Self {
            spec,
            tunn: Box::new(tunn),
            index,
            endpoint,
            roamed: false,
            candidate: 0,
            candidate_since: Instant::now(),
            last_rx: None,
        }
    }

    fn handshake_age(&self) -> Option<Duration> {
        self.tunn.stats().0
    }

    /// Without a recent handshake, move on to the next candidate endpoint.
    /// Returns whether it moved (the caller handshakes there at once).
    fn maybe_next_candidate(&mut self, now: Instant) -> bool {
        let fresh = self.handshake_age().is_some_and(|a| a < Duration::from_secs(180));
        let heard = self
            .last_rx
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(30));
        if fresh || heard || self.spec.endpoints.len() < 2 {
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
    device: std::sync::Arc<D>,
    socket: UdpSocket,
    secret: x25519::StaticSecret,
    public: x25519::PublicKey,
    peers: Vec<Option<PeerState>>,
    by_key: HashMap<PublicKey, usize>,
    by_addr: HashMap<IpAddr, usize>,
    next_index: u32,
    anon_budget: u32,
    anon_window: Instant,
}

impl<D: Device> Engine<D> {
    pub fn new(key: &NodeKey, socket: UdpSocket, device: std::sync::Arc<D>) -> Self {
        let secret = x25519::StaticSecret::from(*key.secret_bytes());
        let public = x25519::PublicKey::from(&secret);
        Self {
            device,
            socket,
            secret,
            public,
            peers: vec![],
            by_key: HashMap::new(),
            by_addr: HashMap::new(),
            next_index: 1,
            anon_budget: ANON_PER_SEC,
            anon_window: Instant::now(),
        }
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
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
                    if p.spec.endpoints != spec.endpoints && !p.roamed {
                        p.candidate = 0;
                        p.endpoint = spec.endpoints.first().copied().or(p.endpoint);
                        p.candidate_since = Instant::now();
                    }
                    p.spec = spec;
                }
                None => {
                    let index = self.next_index & 0x00ff_ffff;
                    self.next_index = self.next_index.wrapping_add(1).max(1);
                    let slot = self.peers.len();
                    self.peers.push(Some(PeerState::new(&self.secret, spec, index)));
                    let _ = slot;
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
        self.secret = x25519::StaticSecret::from(*key.secret_bytes());
        self.public = x25519::PublicKey::from(&self.secret);
        for p in self.peers.iter_mut().flatten() {
            p.tunn.set_static_private(self.secret.clone(), self.public, None);
        }
    }

    pub fn status(&self) -> Vec<PeerStatus> {
        self.peers
            .iter()
            .flatten()
            .map(|p| {
                let (hs, tx, rx, _, _) = p.tunn.stats();
                PeerStatus {
                    name: p.spec.name.clone(),
                    public_key: p.spec.public_key.to_base64(),
                    addresses: p.spec.addresses.iter().map(|a| a.to_string()).collect(),
                    endpoint: p.endpoint.map(|e| e.to_string()),
                    handshake_secs: hs.map(|d| d.as_secs()),
                    rx_bytes: rx as u64,
                    tx_bytes: tx as u64,
                }
            })
            .collect()
    }

    async fn send_to(&self, data: &[u8], to: Option<SocketAddr>) {
        if let Some(to) = to {
            let _ = self.socket.send_to(data, to).await;
        }
    }

    /// A packet from the device: encrypt it for the peer owning its destination.
    async fn on_device_packet(&mut self, packet: &[u8], out: &mut [u8]) {
        let Some(dst) = Tunn::dst_address(packet) else { return };
        let Some(&i) = self.by_addr.get(&dst) else { return };
        let Some(p) = self.peers[i].as_mut() else { return };
        match p.tunn.encapsulate(packet, out) {
            TunnResult::WriteToNetwork(b) => {
                let to = p.endpoint;
                let len = b.len();
                self.send_to(&out[..len], to).await;
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

    /// Which peer a datagram is for.
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

    /// A datagram from the network.
    async fn on_datagram(&mut self, datagram: &[u8], from: SocketAddr, out: &mut [u8]) {
        let Some(i) = self.peer_of(datagram) else { return };
        let mut first = true;
        loop {
            let input: &[u8] = if first { datagram } else { &[] };
            let src = first.then_some(from.ip());
            first = false;
            let p = match self.peers[i].as_mut() {
                Some(p) => p,
                None => return,
            };
            match p.tunn.decapsulate(src, input, out) {
                TunnResult::Done => break,
                TunnResult::Err(e) => {
                    tracing::debug!("from {from} for {}: {e:?}", p.spec.name);
                    break;
                }
                TunnResult::WriteToNetwork(b) => {
                    // A handshake answer or queued packets: back where it came from.
                    let len = b.len();
                    p.endpoint = Some(from);
                    p.roamed = true;
                    p.last_rx = Some(Instant::now());
                    let _ = self.socket.send_to(&out[..len], from).await;
                    // Repeat with empty input to flush what was queued.
                }
                TunnResult::WriteToTunnelV4(b, src_ip) => {
                    let len = b.len();
                    p.endpoint = Some(from);
                    p.roamed = true;
                    p.last_rx = Some(Instant::now());
                    // Only the peer's own addresses (WireGuard's allowed IPs).
                    if p.spec.addresses.contains(&IpAddr::V4(src_ip)) {
                        let _ = self.device.send(&out[..len]).await;
                    } else {
                        tracing::debug!("{} sent a packet from {src_ip}: dropped", p.spec.name);
                    }
                    break;
                }
                TunnResult::WriteToTunnelV6(b, src_ip) => {
                    let len = b.len();
                    p.endpoint = Some(from);
                    p.roamed = true;
                    p.last_rx = Some(Instant::now());
                    if p.spec.addresses.contains(&IpAddr::V6(src_ip)) {
                        let _ = self.device.send(&out[..len]).await;
                    }
                    break;
                }
            }
        }
    }

    /// Handshake retries, keepalives, session expiry; candidate rotation.
    async fn timers(&mut self, out: &mut [u8]) {
        let now = Instant::now();
        for i in 0..self.peers.len() {
            let Some(p) = self.peers[i].as_mut() else { continue };
            let moved = p.maybe_next_candidate(now);
            let to = p.endpoint;
            if moved {
                // Don't wait for the 5 s handshake retry: try the new candidate now.
                if let TunnResult::WriteToNetwork(b) = p.tunn.format_handshake_initiation(out, true) {
                    let len = b.len();
                    self.send_to(&out[..len], to).await;
                }
                continue;
            }
            let Some(p) = self.peers[i].as_mut() else { continue };
            match p.tunn.update_timers(out) {
                TunnResult::WriteToNetwork(b) => {
                    let len = b.len();
                    self.send_to(&out[..len], to).await;
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
                _ = tick.tick() => self.timers(&mut out).await,
                c = control.recv() => match c {
                    Some(Control::SetPeers(p)) => self.set_peers(p),
                    Some(Control::Rekey(k)) => self.rekey(&k),
                    Some(Control::Status(tx)) => { let _ = tx.send(self.status()); }
                    None => return,
                },
            }
        }
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
}

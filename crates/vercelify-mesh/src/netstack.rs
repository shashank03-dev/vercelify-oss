//! A userspace TCP/IP stack (smoltcp) behind the WireGuard engine, for app
//! mode on the Mac (design §13.5): no tun device, no root. The daemon's
//! SOCKS5 proxy and port forwards (`socks.rs`) open TCP connections and UDP
//! sockets on it to mesh addresses; packets go through the same tunnels as
//! on a server.
//!
//! One task owns the stack. Connections are a pair of channels plus a
//! shared `Notify` that wakes the task whenever a side has moved data, so
//! nothing polls on a timer except smoltcp's own retransmit clock.

use crate::wg::Device as WgDevice;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::socket::{tcp, udp};
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, IpEndpoint};
use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex, Notify};

const MTU: usize = 1280;
const TCP_BUFFER: usize = 64 * 1024;
/// What a UDP socket holds each way: a burst of video (a keyframe is
/// hundreds of ~1.1–1.4 kB packets) at once, from the server (remote
/// desktop) or to it (Share screen).
const UDP_PACKETS: usize = 1024;
const UDP_BUFFER: usize = UDP_PACKETS * 1500;
/// Chunks waiting on either side of a connection.
const CHUNKS: usize = 16;
/// TCP connections open at once at most.
const MAX_CONNECTIONS: usize = 256;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A UDP socket nothing used for this long is closed.
const UDP_IDLE: Duration = Duration::from_secs(120);

/// The engine's side: whole IP packets in and out.
pub struct StackDevice {
    to_stack: mpsc::Sender<Vec<u8>>,
    from_stack: Mutex<mpsc::Receiver<Vec<u8>>>,
    wake: Arc<Notify>,
}

impl WgDevice for StackDevice {
    async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.from_stack.lock().await.recv().await {
            Some(p) => {
                let n = p.len().min(buf.len());
                buf[..n].copy_from_slice(&p[..n]);
                Ok(n)
            }
            None => std::future::pending().await,
        }
    }

    async fn send(&self, packet: &[u8]) -> std::io::Result<()> {
        // Full: drop it, as a congested link would (TCP resends).
        if self.to_stack.try_send(packet.to_vec()).is_ok() {
            self.wake.notify_one();
        }
        Ok(())
    }
}

/// A TCP connection on the stack. Dropping `tx` closes our side (FIN);
/// `rx` ends when the remote side closed.
pub struct TcpConn {
    pub tx: mpsc::Sender<Vec<u8>>,
    pub rx: mpsc::Receiver<Vec<u8>>,
    pub wake: Arc<Notify>,
}

impl TcpConn {
    pub async fn send(&self, data: Vec<u8>) -> bool {
        let ok = self.tx.send(data).await.is_ok();
        self.wake.notify_one();
        ok
    }

    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        let r = self.rx.recv().await;
        // Room again: the stack may read more from the window.
        self.wake.notify_one();
        r
    }
}

/// A UDP socket on the stack, bound to an ephemeral port, talking to one
/// remote endpoint.
pub struct UdpConn {
    pub tx: mpsc::Sender<Vec<u8>>,
    pub rx: mpsc::Receiver<Vec<u8>>,
    pub wake: Arc<Notify>,
}

impl UdpConn {
    pub async fn send(&self, data: Vec<u8>) -> bool {
        let ok = self.tx.send(data).await.is_ok();
        self.wake.notify_one();
        ok
    }

    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        let r = self.rx.recv().await;
        self.wake.notify_one();
        r
    }
}

enum Cmd {
    Tcp(SocketAddr, oneshot::Sender<std::io::Result<TcpConn>>),
    Udp(SocketAddr, oneshot::Sender<std::io::Result<UdpConn>>),
}

/// Opens connections on the stack.
#[derive(Clone)]
pub struct Stack {
    cmds: mpsc::Sender<Cmd>,
    wake: Arc<Notify>,
}

impl Stack {
    /// A stack with these addresses (the node's mesh addresses and the
    /// mesh prefixes, so every peer is on-link); returns the engine's device.
    pub fn start(addrs: Vec<(IpAddr, u8)>) -> (Stack, StackDevice) {
        let (to_stack, from_engine) = mpsc::channel(512);
        let (to_engine, from_stack) = mpsc::channel(512);
        let (cmds, cmd_rx) = mpsc::channel(64);
        let wake = Arc::new(Notify::new());
        tokio::spawn(run(addrs, from_engine, to_engine, cmd_rx, wake.clone()));
        (
            Stack {
                cmds,
                wake: wake.clone(),
            },
            StackDevice {
                to_stack,
                from_stack: Mutex::new(from_stack),
                wake,
            },
        )
    }

    pub async fn connect_tcp(&self, to: SocketAddr) -> std::io::Result<TcpConn> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(Cmd::Tcp(to, tx)).await.map_err(|_| gone())?;
        self.wake.notify_one();
        tokio::time::timeout(CONNECT_TIMEOUT + Duration::from_secs(1), rx)
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))?
            .map_err(|_| gone())?
    }

    pub async fn open_udp(&self, to: SocketAddr) -> std::io::Result<UdpConn> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(Cmd::Udp(to, tx)).await.map_err(|_| gone())?;
        self.wake.notify_one();
        rx.await.map_err(|_| gone())?
    }
}

fn gone() -> std::io::Error {
    std::io::Error::other("the network stack stopped")
}

/// smoltcp's device: queues the task fills and drains.
struct Phy {
    rx: VecDeque<Vec<u8>>,
    tx: Vec<Vec<u8>>,
}

struct Rx(Vec<u8>);
struct Tx<'a>(&'a mut Vec<Vec<u8>>);

impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        self.0.push(buf);
        r
    }
}

impl phy::Device for Phy {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn receive(&mut self, _: smoltcp::time::Instant) -> Option<(Rx, Tx<'_>)> {
        let p = self.rx.pop_front()?;
        Some((Rx(p), Tx(&mut self.tx)))
    }

    fn transmit(&mut self, _: smoltcp::time::Instant) -> Option<Tx<'_>> {
        Some(Tx(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ip;
        // smoltcp 0.12 uses the full MTU for non-final IPv4 fragments.
        // Their payload must be a multiple of 8, so leave four bytes of
        // headroom under the WireGuard interface's 1280-byte MTU.
        c.max_transmission_unit = MTU - (MTU - 20) % 8;
        c
    }
}

struct TcpEntry {
    handle: SocketHandle,
    /// Until established: the caller waiting for the connection.
    pending: Option<(oneshot::Sender<std::io::Result<TcpConn>>, TcpConn)>,
    started: std::time::Instant,
    /// Data to send, and what of the current chunk is left.
    out: mpsc::Receiver<Vec<u8>>,
    unsent: Vec<u8>,
    out_closed: bool,
    /// Data received, to the caller (None once the remote side closed).
    inbound: Option<mpsc::Sender<Vec<u8>>>,
}

struct UdpEntry {
    handle: SocketHandle,
    remote: IpEndpoint,
    out: mpsc::Receiver<Vec<u8>>,
    /// Taken from `out` but refused by a full socket: goes first next time.
    waiting: Option<Vec<u8>>,
    inbound: mpsc::Sender<Vec<u8>>,
    used: std::time::Instant,
}

fn now() -> smoltcp::time::Instant {
    smoltcp::time::Instant::now()
}

fn endpoint(a: SocketAddr) -> IpEndpoint {
    IpEndpoint::new(IpAddress::from(a.ip()), a.port())
}

async fn run(
    addrs: Vec<(IpAddr, u8)>,
    mut from_engine: mpsc::Receiver<Vec<u8>>,
    to_engine: mpsc::Sender<Vec<u8>>,
    mut cmds: mpsc::Receiver<Cmd>,
    wake: Arc<Notify>,
) {
    let mut phy = Phy {
        rx: VecDeque::new(),
        tx: vec![],
    };
    let mut cfg = Config::new(HardwareAddress::Ip);
    let mut seed = [0u8; 8];
    let _ = getrandom::fill(&mut seed);
    cfg.random_seed = u64::from_le_bytes(seed);
    let mut iface = Interface::new(cfg, &mut phy, now());
    iface.update_ip_addrs(|a| {
        for (ip, len) in &addrs {
            let _ = a.push(IpCidr::new(IpAddress::from(*ip), *len));
        }
    });
    let mut sockets = SocketSet::new(vec![]);
    let mut tcps: Vec<TcpEntry> = vec![];
    let mut udps: Vec<UdpEntry> = vec![];
    let mut taken: VecDeque<Cmd> = VecDeque::new();
    let mut next_port: u16 = 49152 + (seed[0] as u16) * 32;
    let mut port = || {
        next_port = if next_port >= 65000 { 49152 } else { next_port + 1 };
        next_port
    };
    loop {
        // Work everything that's ready, then wait for something new.
        while let Ok(p) = from_engine.try_recv() {
            phy.rx.push_back(p);
        }
        while let Ok(cmd) = cmds.try_recv() {
            taken.push_back(cmd);
        }
        while let Some(cmd) = taken.pop_front() {
            match cmd {
                Cmd::Tcp(to, reply) => {
                    if tcps.len() >= MAX_CONNECTIONS {
                        let _ = reply.send(Err(std::io::Error::other("too many connections")));
                        continue;
                    }
                    let mut s = tcp::Socket::new(
                        tcp::SocketBuffer::new(vec![0; TCP_BUFFER]),
                        tcp::SocketBuffer::new(vec![0; TCP_BUFFER]),
                    );
                    s.set_nagle_enabled(false);
                    s.set_keep_alive(Some(smoltcp::time::Duration::from_secs(30)));
                    if let Err(e) = s.connect(iface.context(), endpoint(to), port()) {
                        let _ = reply.send(Err(std::io::Error::other(format!("connect {to}: {e}"))));
                        continue;
                    }
                    let handle = sockets.add(s);
                    let (out_tx, out) = mpsc::channel(CHUNKS);
                    let (in_tx, in_rx) = mpsc::channel(CHUNKS);
                    tcps.push(TcpEntry {
                        handle,
                        pending: Some((
                            reply,
                            TcpConn {
                                tx: out_tx,
                                rx: in_rx,
                                wake: wake.clone(),
                            },
                        )),
                        started: std::time::Instant::now(),
                        out,
                        unsent: vec![],
                        out_closed: false,
                        inbound: Some(in_tx),
                    });
                }
                Cmd::Udp(to, reply) => {
                    let mut s = udp::Socket::new(
                        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; UDP_PACKETS], vec![0; UDP_BUFFER]),
                        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; UDP_PACKETS], vec![0; UDP_BUFFER]),
                    );
                    if let Err(e) = s.bind(port()) {
                        let _ = reply.send(Err(std::io::Error::other(format!("bind: {e}"))));
                        continue;
                    }
                    let handle = sockets.add(s);
                    let (out_tx, out) = mpsc::channel(UDP_PACKETS);
                    let (in_tx, in_rx) = mpsc::channel(UDP_PACKETS);
                    udps.push(UdpEntry {
                        handle,
                        remote: endpoint(to),
                        out,
                        waiting: None,
                        inbound: in_tx,
                        used: std::time::Instant::now(),
                    });
                    let _ = reply.send(Ok(UdpConn {
                        tx: out_tx,
                        rx: in_rx,
                        wake: wake.clone(),
                    }));
                }
            }
        }

        iface.poll(now(), &mut phy, &mut sockets);
        let mut moved = service_tcp(&mut tcps, &mut sockets);
        moved |= service_udp(&mut udps, &mut sockets);
        if moved {
            // What the sockets queued goes out now, not at the next wake.
            iface.poll(now(), &mut phy, &mut sockets);
        }
        // Wait for the engine rather than drop: it never waits on the stack,
        // and a dropped video packet spoils every frame until the next keyframe.
        for p in phy.tx.drain(..) {
            if to_engine.send(p).await.is_err() {
                return;
            }
        }
        if !phy.rx.is_empty() {
            continue;
        }
        let delay = iface
            .poll_delay(now(), &sockets)
            .map(|d| Duration::from_micros(d.total_micros()))
            .unwrap_or(Duration::from_secs(1))
            .min(Duration::from_secs(1));
        if delay.is_zero() {
            tokio::task::yield_now().await;
            continue;
        }
        tokio::select! {
            p = from_engine.recv() => match p {
                Some(p) => phy.rx.push_back(p),
                None => return,
            },
            c = cmds.recv() => match c {
                // Handled at the top of the loop.
                Some(c) => taken.push_back(c),
                None => return,
            },
            _ = wake.notified() => {}
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// Move data between TCP sockets and their channels; drop finished ones.
/// Returns whether anything was handed to a socket.
fn service_tcp(tcps: &mut Vec<TcpEntry>, sockets: &mut SocketSet<'_>) -> bool {
    let mut queued = false;
    tcps.retain_mut(|e| {
        let s = sockets.get_mut::<tcp::Socket>(e.handle);
        if let Some((reply, conn)) = e.pending.take() {
            match s.state() {
                tcp::State::Established => {
                    let _ = reply.send(Ok(conn));
                }
                tcp::State::Closed | tcp::State::TimeWait => {
                    let _ = reply.send(Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "connection refused",
                    )));
                    sockets.remove(e.handle);
                    return false;
                }
                _ if e.started.elapsed() > CONNECT_TIMEOUT => {
                    let _ = reply.send(Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "no answer (is the peer up, and the port allowed?)",
                    )));
                    s.abort();
                    sockets.remove(e.handle);
                    return false;
                }
                _ => {
                    e.pending = Some((reply, conn));
                    return true;
                }
            }
        }
        // Remote → caller, while the caller has room.
        if let Some(tx) = &e.inbound {
            while s.can_recv() {
                let Ok(permit) = tx.try_reserve() else { break };
                let mut buf = vec![0u8; 16 * 1024];
                match s.recv_slice(&mut buf) {
                    Ok(n) if n > 0 => {
                        buf.truncate(n);
                        permit.send(buf);
                    }
                    _ => break,
                }
            }
            if !s.may_recv() && !s.can_recv() {
                e.inbound = None; // EOF for the caller
            }
        }
        // Caller → remote.
        loop {
            if e.unsent.is_empty() {
                if e.out_closed {
                    break;
                }
                match e.out.try_recv() {
                    Ok(chunk) => e.unsent = chunk,
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        e.out_closed = true;
                        s.close();
                        queued = true;
                        break;
                    }
                }
            }
            if !s.can_send() {
                break;
            }
            match s.send_slice(&e.unsent) {
                Ok(n) if n > 0 => {
                    e.unsent.drain(..n);
                    queued = true;
                }
                _ => break,
            }
        }
        // The caller dropped both ends: abort what's left.
        if e.inbound.as_ref().is_some_and(|t| t.is_closed()) && e.out_closed {
            s.abort();
        }
        let finished = matches!(s.state(), tcp::State::Closed | tcp::State::TimeWait)
            || (!s.is_active() && e.out_closed && e.inbound.is_none());
        if finished {
            sockets.remove(e.handle);
            return false;
        }
        true
    });
    queued
}

fn service_udp(udps: &mut Vec<UdpEntry>, sockets: &mut SocketSet<'_>) -> bool {
    let mut queued = false;
    udps.retain_mut(|e| {
        let s = sockets.get_mut::<udp::Socket>(e.handle);
        let mut buf = vec![0u8; 64 * 1024];
        while s.can_recv() {
            match s.recv_slice(&mut buf) {
                Ok((n, meta)) => {
                    if meta.endpoint == e.remote {
                        let _ = e.inbound.try_send(buf[..n].to_vec());
                        e.used = std::time::Instant::now();
                    }
                }
                Err(_) => break,
            }
        }
        loop {
            if !s.can_send() {
                break;
            }
            let d = match e.waiting.take().map(Ok).unwrap_or_else(|| e.out.try_recv()) {
                Ok(d) => d,
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    sockets.remove(e.handle);
                    return false;
                }
            };
            // A free slot doesn't mean room for the payload: keep it, don't drop it.
            if s.send_slice(&d, e.remote).is_err() {
                e.waiting = Some(d);
                break;
            }
            queued = true;
            e.used = std::time::Instant::now();
        }
        if e.used.elapsed() > UDP_IDLE || e.inbound.is_closed() {
            sockets.remove(e.handle);
            return false;
        }
        true
    });
    queued
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    const ME: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 2);
    const SERVER: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 1);

    fn checksum(b: &[u8]) -> u16 {
        let mut s: u32 = b
            .chunks(2)
            .map(|c| u32::from(c[0]) << 8 | u32::from(*c.get(1).unwrap_or(&0)))
            .sum();
        while s > 0xffff {
            s = (s & 0xffff) + (s >> 16);
        }
        !(s as u16)
    }

    /// An IPv4 packet from the server; `frag` = (more fragments, offset in bytes).
    fn ipv4(id: u16, frag: (bool, usize), body: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 20];
        p[0] = 0x45;
        p[2..4].copy_from_slice(&((20 + body.len()) as u16).to_be_bytes());
        p[4..6].copy_from_slice(&id.to_be_bytes());
        let flags = (u16::from(frag.0) << 13) | (frag.1 / 8) as u16;
        p[6..8].copy_from_slice(&flags.to_be_bytes());
        p[8] = 64;
        p[9] = 17;
        p[12..16].copy_from_slice(&SERVER.octets());
        p[16..20].copy_from_slice(&ME.octets());
        let c = checksum(&p);
        p[10..12].copy_from_slice(&c.to_be_bytes());
        p.extend_from_slice(body);
        p
    }

    /// A UDP datagram from the server's `from` port to ours.
    fn udp(from: u16, to: u16, payload: &[u8]) -> Vec<u8> {
        let len = (8 + payload.len()) as u16;
        let mut u = vec![];
        u.extend_from_slice(&from.to_be_bytes());
        u.extend_from_slice(&to.to_be_bytes());
        u.extend_from_slice(&len.to_be_bytes());
        u.extend_from_slice(&[0, 0]);
        u.extend_from_slice(payload);
        let mut pseudo = vec![];
        pseudo.extend_from_slice(&SERVER.octets());
        pseudo.extend_from_slice(&ME.octets());
        pseudo.extend_from_slice(&[0, 17]);
        pseudo.extend_from_slice(&len.to_be_bytes());
        pseudo.extend_from_slice(&u);
        let c = checksum(&pseudo);
        u[6..8].copy_from_slice(&c.to_be_bytes());
        u
    }

    /// A UDP socket to the server's `port`, and the local port it uses.
    async fn open(port: u16) -> (UdpConn, StackDevice, u16, Stack) {
        let (stack, dev) = Stack::start(vec![(IpAddr::V4(ME), 16)]);
        let conn = stack.open_udp(SocketAddr::from((SERVER, port))).await.unwrap();
        conn.send(b"ping".to_vec()).await;
        let mut buf = vec![0u8; 2048];
        let n = tokio::time::timeout(Duration::from_secs(5), dev.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(buf[9], 17, "the ping goes out as UDP");
        let local = u16::from_be_bytes([buf[20], buf[21]]);
        assert!(n > 28);
        (conn, dev, local, stack)
    }

    async fn next(conn: &mut UdpConn) -> Option<Vec<u8>> {
        tokio::time::timeout(Duration::from_secs(5), conn.recv())
            .await
            .ok()
            .flatten()
    }

    // Moonlight asks Sunshine for ~1400-byte video packets on what it takes
    // for a LAN (127.0.0.1); over the mesh's 1280 MTU they arrive in pieces.
    #[tokio::test]
    async fn a_fragmented_datagram_arrives_whole() {
        let (mut conn, dev, local, _stack) = open(47998).await;
        let payload: Vec<u8> = (0..1400u32).map(|i| i as u8).collect();
        let d = udp(47998, local, &payload);
        dev.send(&ipv4(7, (true, 0), &d[..1200])).await.unwrap();
        dev.send(&ipv4(7, (false, 1200), &d[1200..])).await.unwrap();
        assert_eq!(next(&mut conn).await, Some(payload));
    }

    #[tokio::test]
    async fn oversized_udp_egress_is_fragmented_and_arrives_whole() {
        let (mut conn, dev, _local, _stack) = open(5600).await;
        let payload: Vec<u8> = (0..1316u32).map(|i| i as u8).collect();
        conn.send(payload.clone()).await;
        let mut buf = vec![0; 2048];
        let mut fragments = 0;
        loop {
            let n = tokio::time::timeout(Duration::from_secs(3), dev.recv(&mut buf))
                .await
                .unwrap()
                .unwrap();
            assert!(n <= 1280, "packet exceeds the mesh MTU: {n}");
            let flags = u16::from_be_bytes([buf[6], buf[7]]);
            let offset = usize::from(flags & 0x1fff) * 8;
            let header = usize::from(buf[0] & 15) * 4;
            // Echo the peer's exact fragments back. Swapping IPs and UDP ports
            // preserves both checksums (the pseudo-header sum is unchanged).
            for i in 0..4 {
                buf.swap(12 + i, 16 + i);
            }
            if offset == 0 {
                buf.swap(header, header + 2);
                buf.swap(header + 1, header + 3);
            }
            dev.send(&buf[..n]).await.unwrap();
            fragments += 1;
            if flags & 0x2000 == 0 {
                break;
            }
        }
        assert!(fragments > 1);
        assert_eq!(next(&mut conn).await, Some(payload));
    }

    // Share screen: ffmpeg's keyframe, hundreds of 1128-byte datagrams in a
    // few milliseconds, sent from this Mac to the server. All of it must leave.
    #[tokio::test]
    async fn an_outgoing_video_burst_leaves_whole() {
        let (conn, dev, _local, _stack) = open(5600).await;
        let burst = 1000u16;
        let sender = async {
            for i in 0..burst {
                let mut payload = vec![0u8; 1128];
                payload[..2].copy_from_slice(&i.to_be_bytes());
                conn.send(payload).await;
            }
        };
        let receiver = async {
            let mut seen = std::collections::BTreeSet::new();
            let mut buf = vec![0u8; 2048];
            while let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(500), dev.recv(&mut buf)).await {
                if n >= 30 && buf[9] == 17 {
                    seen.insert(u16::from_be_bytes([buf[28], buf[29]]));
                }
            }
            seen
        };
        let ((), seen) = tokio::join!(sender, receiver);
        assert_eq!(
            seen.len(),
            usize::from(burst),
            "lost {} of {burst}",
            usize::from(burst) - seen.len()
        );
    }

    // A keyframe is a burst of such packets, sent faster than the reader drains them.
    #[tokio::test]
    async fn a_video_burst_fits() {
        let (mut conn, dev, local, _stack) = open(47998).await;
        let burst = 400u16;
        for i in 0..burst {
            let mut payload = vec![0u8; 1024];
            payload[..2].copy_from_slice(&i.to_be_bytes());
            dev.send(&ipv4(i, (false, 0), &udp(47998, local, &payload)))
                .await
                .unwrap();
        }
        let mut got = 0u16;
        while let Some(p) = next(&mut conn).await {
            assert_eq!(u16::from_be_bytes([p[0], p[1]]), got, "in order, none missing");
            got += 1;
            if got == burst {
                break;
            }
        }
        assert_eq!(got, burst);
    }
}

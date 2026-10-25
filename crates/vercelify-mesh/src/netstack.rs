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
const UDP_PACKETS: usize = 64;
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
        c.max_transmission_unit = MTU;
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
                        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; UDP_PACKETS], vec![0; 64 * 1024]),
                        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; UDP_PACKETS], vec![0; 64 * 1024]),
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
        for p in phy.tx.drain(..) {
            let _ = to_engine.try_send(p);
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
            match e.out.try_recv() {
                Ok(d) => {
                    if s.send_slice(&d, e.remote).is_ok() {
                        queued = true;
                        e.used = std::time::Instant::now();
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    sockets.remove(e.handle);
                    return false;
                }
            }
        }
        if e.used.elapsed() > UDP_IDLE || e.inbound.is_closed() {
            sockets.remove(e.handle);
            return false;
        }
        true
    });
    queued
}

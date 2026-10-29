//! Low-latency display receiver: turns the Linux laptop panel into a
//! secondary monitor for the Mac by playing an MPEG-TS stream with mpv.
//!
//! The Mac side (the cockpit) captures a screen with ffmpeg + VideoToolbox
//! and sends it over the mesh; see `cockpit/src-tauri/src/display.rs`.

use crate::config::Config;
use chrono::Utc;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use vercelify_core::{ProcessStatus, ReceiverConfig, StreamTransport};

const OUTPUT_LINES: usize = 40;
/// mpv keeps the last position when frames stop arriving (sender gone, the
/// network down): playback counts only while the position moves.
const STALL: Duration = Duration::from_secs(2);
const SRT_LATENCY_MS: std::ops::RangeInclusive<u32> = 20..=5000;

/// URL mpv (via libavformat) listens on.
pub fn listen_url(cfg: &ReceiverConfig) -> String {
    match cfg.transport {
        // overrun_nonfatal: drop instead of dying when the decoder falls behind.
        StreamTransport::Udp => format!("udp://0.0.0.0:{}?overrun_nonfatal=1&fifo_size=50000", cfg.port),
        // libsrt latency is in microseconds. A listener serves one caller:
        // drop one that went silent (the Mac slept, quit or roamed) after 2 s
        // so mpv listens again, rather than after SRT's own idle timeout.
        StreamTransport::Srt => format!(
            "srt://0.0.0.0:{}?mode=listener&latency={}&timeout=2000000",
            cfg.port,
            u64::from(cfg.latency_ms.unwrap_or(120)) * 1000
        ),
    }
}

/// URL the sender should push to, given this host's reachable address.
pub fn sender_url(cfg: &ReceiverConfig, host: &str) -> String {
    match cfg.transport {
        StreamTransport::Udp => format!("udp://{host}:{}?pkt_size=1316", cfg.port),
        // Keep calling while the receiver starts or listens again (ms;
        // ffmpeg gives up after 3 s by default).
        StreamTransport::Srt => format!(
            "srt://{host}:{}?mode=caller&latency={}&connect_timeout=10000&pkt_size=1316",
            cfg.port,
            u64::from(cfg.latency_ms.unwrap_or(120)) * 1000
        ),
    }
}

/// Resolve `auto` into a concrete backend based on the configured env.
pub fn effective_backend(config: &Config) -> &str {
    match config.display_backend.as_str() {
        "auto" => {
            if config.display_env.contains_key("WAYLAND_DISPLAY") {
                "wayland"
            } else if config.display_env.contains_key("DISPLAY") {
                "x11"
            } else {
                "drm"
            }
        }
        other => other,
    }
}

pub fn mpv_args(config: &Config, cfg: &ReceiverConfig) -> Vec<String> {
    let mut a: Vec<String> = [
        "--no-config",
        "--profile=low-latency",
        "--untimed",
        "--no-cache",
        "--demuxer-lavf-format=mpegts",
        "--demuxer-lavf-o=fflags=+nobuffer",
        "--demuxer-lavf-probesize=32",
        "--demuxer-lavf-analyzeduration=0.1",
        "--framedrop=vo",
        "--hwdec=auto-safe",
        "--fullscreen",
        "--osc=no",
        "--osd-level=0",
        "--audio=no",
        // Keep listening when the Mac stops/restarts the stream.
        "--loop-playlist=inf",
        "--idle=yes",
        "--force-window=no",
        "--title=Vercelify Display",
        "--msg-level=all=warn",
        "--term-status-msg=",
    ]
    .map(String::from)
    .to_vec();
    match effective_backend(config) {
        "drm" => {
            a.push("--vo=gpu".into());
            a.push("--gpu-context=drm".into());
        }
        "wayland" => a.push("--gpu-context=wayland".into()),
        "x11" => a.push("--gpu-context=x11egl".into()),
        _ => {}
    }
    a.extend(config.mpv_extra_args.iter().cloned());
    a.push(listen_url(cfg));
    a
}

struct Running {
    child: Child,
    pid: Option<u32>,
    command: String,
    started_at: String,
    config: ReceiverConfig,
    ipc: PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        // Also clean up mpv when the agent shuts down or an in-flight start
        // replaces a receiver, rather than leaving a runuser child behind.
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    output: VecDeque<String>,
    last_exit: Option<i32>,
    last_config: Option<ReceiverConfig>,
    /// The last position mpv reported and when it last changed.
    progress: Option<(f64, Instant)>,
}

#[derive(Clone, Default)]
pub struct Receiver {
    inner: Arc<Mutex<Inner>>,
    lifecycle: Arc<Mutex<()>>,
}

impl Receiver {
    pub async fn start(&self, config: &Config, cfg: ReceiverConfig) -> Result<(), String> {
        if cfg.port < 1024 {
            return Err("port must be >= 1024".into());
        }
        if cfg.transport == StreamTransport::Srt && cfg.latency_ms.is_some_and(|l| !SRT_LATENCY_MS.contains(&l)) {
            return Err(format!(
                "SRT latency must be {}-{} ms",
                SRT_LATENCY_MS.start(),
                SRT_LATENCY_MS.end()
            ));
        }
        let _operation = self.lifecycle.lock().await;
        self.stop_inner().await;
        // mpv only logs a port it cannot bind and then idles forever.
        port_free(cfg.port).await?;
        let session = crate::display_session::resolve(config).await?;
        let mut resolved = config.clone();
        resolved.display_env.extend(session.env);
        let ipc_dir = config.run_dir.join("display");
        tokio::fs::create_dir_all(&ipc_dir)
            .await
            .map_err(|e| format!("cannot create display runtime directory: {e}"))?;
        // A previous session may have owned this directory. Never leave a
        // root mpv control socket writable by a logged-out desktop user.
        if unsafe { libc::geteuid() } == 0 {
            let uid = if session.user.is_some() {
                resolved.display_env["XDG_RUNTIME_DIR"]
                    .rsplit('/')
                    .next()
                    .unwrap_or("0")
            } else {
                "0"
            };
            let result = crate::exec::run("chown", &[uid, &ipc_dir.to_string_lossy()], Duration::from_secs(2))
                .await
                .map_err(|e| e.to_string())?;
            if !result.success() {
                return Err("Could not prepare the Linux desktop receiver directory".into());
            }
        }
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&ipc_dir, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|e| e.to_string())?;
        // Each receiver has its own socket, including simultaneous API calls.
        let ipc = ipc_dir.join(format!(
            "mpv-{}.sock",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut args = mpv_args(&resolved, &cfg);
        args.insert(0, format!("--input-ipc-server={}", ipc.display()));
        let mut cmd = if let Some(user) = session.user {
            let mut cmd = Command::new("runuser");
            cmd.args(["--preserve-environment", "--user", &user, "--", &config.mpv_bin]);
            cmd
        } else {
            Command::new(&config.mpv_bin)
        };
        // Kill the process group on stop, including runuser's mpv child.
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
        cmd.args(&args)
            .envs(&resolved.display_env)
            .stdin(Stdio::null())
            // mpv prints status/info on stdout and warnings on stderr; keep both.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                format!(
                    "`{}` not found — install it with `sudo apt install mpv`",
                    config.mpv_bin
                )
            }
            _ => format!("failed to start mpv: {e}"),
        })?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let mut inner = self.inner.lock().await;
        inner.output.clear();
        inner.last_exit = None;
        inner.last_config = Some(cfg.clone());
        inner.progress = None;
        inner.running = Some(Running {
            child,
            pid,
            command: format!("{} {}", config.mpv_bin, args.join(" ")),
            started_at: Utc::now().to_rfc3339(),
            config: cfg.clone(),
            ipc: ipc.clone(),
        });
        drop(inner);
        keep_listening(self.inner.clone(), ipc, listen_url(&cfg));

        if let Some(out) = stdout {
            self.collect(out, pid);
        }
        if let Some(err) = stderr {
            self.collect(err, pid);
        }
        Ok(())
    }

    /// Keep the last lines of a child stream in the shared ring buffer.
    fn collect<R: tokio::io::AsyncRead + Unpin + Send + 'static>(&self, stream: R, pid: Option<u32>) {
        let state = self.inner.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim_end().to_string();
                if line.is_empty() {
                    continue;
                }
                let mut g = state.lock().await;
                if g.running.as_ref().and_then(|r| r.pid) != pid {
                    continue;
                }
                if g.output.len() >= OUTPUT_LINES {
                    g.output.pop_front();
                }
                g.output.push_back(line);
            }
        });
    }

    pub async fn stop(&self) -> bool {
        let _operation = self.lifecycle.lock().await;
        self.stop_inner().await
    }

    async fn stop_inner(&self) -> bool {
        let mut inner = self.inner.lock().await;
        if let Some(mut r) = inner.running.take() {
            if let Some(pid) = r.pid {
                // This group was created for this receiver, never the agent's.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = r.child.start_kill();
            let status = r.child.wait().await.ok();
            inner.last_exit = status.and_then(|s| s.code());
            let _ = tokio::fs::remove_file(&r.ipc).await;
            true
        } else {
            false
        }
    }

    pub async fn playback_active(&self) -> bool {
        let Some(ipc) = self.inner.lock().await.running.as_ref().map(|r| r.ipc.clone()) else {
            return false;
        };
        let position = position_at(&ipc).await;
        let mut inner = self.inner.lock().await;
        // A receiver replaced meanwhile has its own progress.
        if inner.running.as_ref().map(|r| &r.ipc) != Some(&ipc) {
            return false;
        }
        moving(&mut inner.progress, position, Instant::now())
    }

    pub async fn status(&self) -> (ProcessStatus, Option<ReceiverConfig>) {
        let mut inner = self.inner.lock().await;
        // Reap a process that exited on its own.
        if let Some(r) = inner.running.as_mut() {
            if let Ok(Some(status)) = r.child.try_wait() {
                inner.last_exit = status.code().or(Some(-1));
                if let Some(r) = inner.running.take() {
                    let _ = tokio::fs::remove_file(&r.ipc).await;
                }
            }
        }
        let output: Vec<String> = inner.output.iter().cloned().collect();
        match &inner.running {
            Some(r) => (
                ProcessStatus {
                    running: true,
                    pid: r.pid,
                    command: Some(r.command.clone()),
                    started_at: Some(r.started_at.clone()),
                    last_output: output,
                    exit_code: None,
                },
                Some(r.config.clone()),
            ),
            None => (
                ProcessStatus {
                    running: false,
                    last_output: output,
                    exit_code: inner.last_exit,
                    ..Default::default()
                },
                inner.last_config.clone(),
            ),
        }
    }
}

/// Whether playback moved within `STALL`, given the position just read.
fn moving(progress: &mut Option<(f64, Instant)>, position: Option<f64>, now: Instant) -> bool {
    let Some(position) = position else {
        *progress = None;
        return false;
    };
    match progress {
        Some((last, since)) if *last == position => now.duration_since(*since) < STALL,
        _ => {
            *progress = Some((position, now));
            true
        }
    }
}

/// Fail clearly when another program holds the UDP port (SRT is UDP too).
/// The receiver just stopped may take a moment to release it.
async fn port_free(port: u16) -> Result<(), String> {
    for _ in 0..20 {
        match std::net::UdpSocket::bind(("0.0.0.0", port)) {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => tokio::time::sleep(Duration::from_millis(100)).await,
            Err(e) => return Err(format!("cannot listen on UDP port {port}: {e}")),
        }
    }
    Err(format!(
        "UDP port {port} is in use by another program on the server (see: sudo ss -ulpn 'sport = :{port}')"
    ))
}

/// time-pos is unavailable until mpv has opened and decoded the media.
async fn position_at(path: &Path) -> Option<f64> {
    ask(path, serde_json::json!(["get_property", "time-pos"]))
        .await?
        .as_f64()
}

/// Run an mpv IPC command; its `data` on success (`null` for commands).
async fn ask(path: &Path, command: serde_json::Value) -> Option<serde_json::Value> {
    tokio::time::timeout(Duration::from_millis(150), async {
        let mut socket = UnixStream::connect(path).await.ok()?;
        let request = serde_json::json!({ "command": command, "request_id": 1 });
        socket.write_all(format!("{request}\n").as_bytes()).await.ok()?;
        let mut lines = BufReader::new(socket).lines();
        for _ in 0..8 {
            let line = lines.next_line().await.ok()??;
            let mut response: serde_json::Value = serde_json::from_str(&line).ok()?;
            if response["request_id"] == 1 {
                return (response["error"] == "success").then(|| response["data"].take());
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// mpv stops looping a stream whose every open failed (an SRT caller whose
/// handshake half-completed sends nothing; mpv gives up on it) and idles
/// with the receiver still "running". Load the stream again once it stays
/// idle; `--loop-playlist=force` would instead spin on an instant failure.
fn keep_listening(inner: Arc<Mutex<Inner>>, ipc: PathBuf, url: String) {
    tokio::spawn(async move {
        let mut idle_before = false;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if inner.lock().await.running.as_ref().map(|r| &r.ipc) != Some(&ipc) {
                return;
            }
            let idle = ask(&ipc, serde_json::json!(["get_property", "idle-active"])).await
                == Some(serde_json::Value::Bool(true));
            // Twice in a row: mpv is briefly idle between its own reopens.
            if idle && idle_before {
                tracing::info!("display receiver idle; listening again");
                let _ = ask(&ipc, serde_json::json!(["loadfile", url])).await;
                idle_before = false;
            } else {
                idle_before = idle;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn playback_needs_media_and_handles_unresponsive_receiver() {
        for (response, expected) in [
            ("{\"request_id\":1,\"error\":\"success\",\"data\":0.0}\n", Some(0.0)),
            ("{\"request_id\":1,\"error\":\"property unavailable\"}\n", None),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("mpv.sock");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let server = tokio::spawn(async move {
                let (mut conn, _) = listener.accept().await.unwrap();
                use tokio::io::AsyncReadExt;
                let mut buffer = [0; 256];
                assert!(conn.read(&mut buffer).await.unwrap() > 0);
                conn.write_all(response.as_bytes()).await.unwrap();
            });
            assert_eq!(position_at(&path).await, expected);
            server.await.unwrap();
        }
        assert_eq!(position_at(Path::new("/nonexistent/mpv.sock")).await, None);
    }

    /// A fake mpv that is idle: the watchdog asks twice, then reloads.
    #[tokio::test]
    async fn an_idle_receiver_listens_again() {
        let dir = tempfile::tempdir().unwrap();
        let ipc = dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&ipc).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let (conn, _) = listener.accept().await.unwrap();
                let (read, mut write) = conn.into_split();
                let line = BufReader::new(read).lines().next_line().await.unwrap().unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                tx.send(request["command"].clone()).unwrap();
                let reply = if request["command"][0] == "get_property" {
                    "{\"request_id\":1,\"error\":\"success\",\"data\":true}\n"
                } else {
                    "{\"request_id\":1,\"error\":\"success\",\"data\":null}\n"
                };
                write.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        let child = Command::new("sleep").arg("30").kill_on_drop(true).spawn().unwrap();
        let inner = Arc::new(Mutex::new(Inner {
            running: Some(Running {
                child,
                pid: None,
                command: String::new(),
                started_at: String::new(),
                config: rc(StreamTransport::Srt),
                ipc: ipc.clone(),
            }),
            ..Default::default()
        }));
        keep_listening(inner.clone(), ipc, "srt://0.0.0.0:5600?mode=listener".into());
        let mut seen = vec![];
        while seen.len() < 3 {
            seen.push(
                tokio::time::timeout(Duration::from_secs(5), rx.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        assert_eq!(seen[0], serde_json::json!(["get_property", "idle-active"]));
        assert_eq!(seen[1], seen[0], "one idle look is not enough");
        assert_eq!(
            seen[2],
            serde_json::json!(["loadfile", "srt://0.0.0.0:5600?mode=listener"])
        );
        // Stopped receivers are left alone.
        inner.lock().await.running = None;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        while let Ok(command) = rx.try_recv() {
            assert_ne!(command[0], "loadfile", "reloaded a stopped receiver");
        }
    }

    #[test]
    fn playback_stops_counting_when_the_picture_freezes() {
        let t0 = Instant::now();
        let mut p = None;
        assert!(!moving(&mut p, None, t0), "no media yet");
        assert!(moving(&mut p, Some(1.0), t0), "first frame");
        assert!(moving(&mut p, Some(1.0), t0 + STALL / 2), "between frames");
        assert!(!moving(&mut p, Some(1.0), t0 + STALL), "frozen: sender or network gone");
        assert!(moving(&mut p, Some(4.0), t0 + STALL * 3), "frames again");
        // A restarted sender starts its timestamps over.
        assert!(moving(&mut p, Some(0.2), t0 + STALL * 4));
        assert!(!moving(&mut p, None, t0 + STALL * 4) && p.is_none());
    }

    #[tokio::test]
    async fn a_taken_port_is_reported_not_waited_on_forever() {
        let held = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        let port = held.local_addr().unwrap().port();
        let err = port_free(port).await.unwrap_err();
        assert!(err.contains(&port.to_string()) && err.contains("in use"), "{err}");
        drop(held);
        port_free(port).await.unwrap();
    }

    #[tokio::test]
    async fn srt_latency_is_bounded() {
        let r = Receiver::default();
        for latency in [0, 19, 5001, 600_000] {
            let cfg = ReceiverConfig {
                latency_ms: Some(latency),
                ..rc(StreamTransport::Srt)
            };
            let err = r.start(&Config::default(), cfg).await.unwrap_err();
            assert!(err.contains("SRT latency"), "{latency}: {err}");
        }
    }

    fn rc(t: StreamTransport) -> ReceiverConfig {
        ReceiverConfig {
            transport: t,
            port: 5600,
            latency_ms: Some(80),
        }
    }

    #[test]
    fn urls() {
        assert_eq!(
            listen_url(&rc(StreamTransport::Udp)),
            "udp://0.0.0.0:5600?overrun_nonfatal=1&fifo_size=50000"
        );
        assert_eq!(
            listen_url(&rc(StreamTransport::Srt)),
            "srt://0.0.0.0:5600?mode=listener&latency=80000&timeout=2000000"
        );
        assert_eq!(
            sender_url(&rc(StreamTransport::Udp), "10.77.0.1"),
            "udp://10.77.0.1:5600?pkt_size=1316"
        );
        // host/tests/contract/screencast.py streams to this exact URL.
        assert_eq!(
            sender_url(&rc(StreamTransport::Srt), "10.77.0.1"),
            "srt://10.77.0.1:5600?mode=caller&latency=80000&connect_timeout=10000&pkt_size=1316"
        );
    }

    #[test]
    fn backend_selection() {
        let mut c = Config::default();
        assert_eq!(effective_backend(&c), "drm");
        assert!(mpv_args(&c, &rc(StreamTransport::Udp)).contains(&"--gpu-context=drm".to_string()));
        c.display_env.insert("DISPLAY".into(), ":0".into());
        assert_eq!(effective_backend(&c), "x11");
        c.display_backend = "wayland".into();
        assert_eq!(effective_backend(&c), "wayland");
    }

    #[test]
    fn extra_args_before_url() {
        let c = Config {
            mpv_extra_args: vec!["--vo=null".into()],
            ..Default::default()
        };
        let a = mpv_args(&c, &rc(StreamTransport::Udp));
        assert_eq!(a[a.len() - 2], "--vo=null");
        assert!(a.last().unwrap().starts_with("udp://"));
    }
}

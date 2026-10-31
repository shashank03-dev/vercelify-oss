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
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use vercelify_core::{ProcessStatus, ReceiverConfig, StreamTransport};

const OUTPUT_LINES: usize = 40;

/// URL mpv (via libavformat) listens on.
pub fn listen_url(cfg: &ReceiverConfig) -> String {
    match cfg.transport {
        // overrun_nonfatal: drop instead of dying when the decoder falls behind.
        StreamTransport::Udp => format!("udp://0.0.0.0:{}?overrun_nonfatal=1&fifo_size=50000", cfg.port),
        // libsrt latency is in microseconds.
        StreamTransport::Srt => format!(
            "srt://0.0.0.0:{}?mode=listener&latency={}",
            cfg.port,
            u64::from(cfg.latency_ms.unwrap_or(120)) * 1000
        ),
    }
}

/// URL the sender should push to, given this host's reachable address.
pub fn sender_url(cfg: &ReceiverConfig, host: &str) -> String {
    match cfg.transport {
        StreamTransport::Udp => format!("udp://{host}:{}?pkt_size=1128", cfg.port),
        StreamTransport::Srt => format!(
            "srt://{host}:{}?mode=caller&latency={}&pkt_size=1128",
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
        let _operation = self.lifecycle.lock().await;
        self.stop_inner().await;
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
        inner.running = Some(Running {
            child,
            pid,
            command: format!("{} {}", config.mpv_bin, args.join(" ")),
            started_at: Utc::now().to_rfc3339(),
            config: cfg,
            ipc,
        });
        drop(inner);

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
        let ipc = self.inner.lock().await.running.as_ref().map(|r| r.ipc.clone());
        match ipc {
            Some(path) => playback_at(&path).await,
            None => false,
        }
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

/// time-pos is unavailable until mpv has opened and decoded the media.
async fn playback_at(path: &Path) -> bool {
    tokio::time::timeout(Duration::from_millis(150), async {
        let mut socket = UnixStream::connect(path).await.ok()?;
        socket
            .write_all(b"{\"command\":[\"get_property\",\"vo-configured\"],\"request_id\":1}\n{\"command\":[\"get_property\",\"core-idle\"],\"request_id\":2}\n")
            .await
            .ok()?;
        let mut lines = BufReader::new(socket).lines();
        let mut video = None;
        let mut idle = None;
        for _ in 0..8 {
            let line = lines.next_line().await.ok()??;
            let response: serde_json::Value = serde_json::from_str(&line).ok()?;
            if response["error"] != "success" { continue; }
            match response["request_id"].as_u64() {
                Some(1) => video = response["data"].as_bool(),
                Some(2) => idle = response["data"].as_bool(),
                _ => {}
            }
            if let (Some(video), Some(idle)) = (video, idle) { return Some(video && !idle); }
        }
        None
    })
    .await
    .ok()
    .flatten()
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn playback_requires_configured_video_and_an_active_decoder() {
        for (video, idle, expected) in [(false, false, false), (true, true, false), (true, false, true)] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("mpv.sock");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let server = tokio::spawn(async move {
                let (conn, _) = listener.accept().await.unwrap();
                let (rd, mut wr) = conn.into_split();
                let mut lines = BufReader::new(rd).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    let data = match request["command"][1].as_str().unwrap() {
                        "vo-configured" => serde_json::json!(video),
                        "core-idle" => serde_json::json!(idle),
                        "time-pos" => serde_json::json!(0.0),
                        other => panic!("unexpected mpv property: {other}"),
                    };
                    let reply =
                        serde_json::json!({ "request_id": request["request_id"], "error": "success", "data": data });
                    if wr.write_all(format!("{reply}\n").as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
            assert_eq!(playback_at(&path).await, expected, "video={video}, idle={idle}");
            server.abort();
        }
        assert!(!playback_at(Path::new("/nonexistent/mpv.sock")).await);
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
            "srt://0.0.0.0:5600?mode=listener&latency=80000"
        );
        assert_eq!(
            sender_url(&rc(StreamTransport::Udp), "10.77.0.1"),
            "udp://10.77.0.1:5600?pkt_size=1128"
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

//! Low-latency display receiver: turns the Linux laptop panel into a
//! secondary monitor for the Mac by playing an MPEG-TS stream with mpv.
//!
//! The Mac side (the cockpit) captures a screen with ffmpeg + VideoToolbox
//! and sends it over the mesh; see `cockpit/src-tauri/src/display.rs`.

use crate::config::Config;
use chrono::Utc;
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
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
        StreamTransport::Udp => format!("udp://{host}:{}?pkt_size=1316", cfg.port),
        StreamTransport::Srt => format!(
            "srt://{host}:{}?mode=caller&latency={}&pkt_size=1316",
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
}

impl Receiver {
    pub async fn start(&self, config: &Config, cfg: ReceiverConfig) -> Result<(), String> {
        if cfg.port < 1024 {
            return Err("port must be >= 1024".into());
        }
        self.stop().await;
        let args = mpv_args(config, &cfg);
        let mut cmd = Command::new(&config.mpv_bin);
        cmd.args(&args)
            .envs(&config.display_env)
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
        });
        drop(inner);

        if let Some(out) = stdout {
            self.collect(out);
        }
        if let Some(err) = stderr {
            self.collect(err);
        }
        Ok(())
    }

    /// Keep the last lines of a child stream in the shared ring buffer.
    fn collect<R: tokio::io::AsyncRead + Unpin + Send + 'static>(&self, stream: R) {
        let state = self.inner.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim_end().to_string();
                if line.is_empty() {
                    continue;
                }
                let mut g = state.lock().await;
                if g.output.len() >= OUTPUT_LINES {
                    g.output.pop_front();
                }
                g.output.push_back(line);
            }
        });
    }

    pub async fn stop(&self) -> bool {
        let mut inner = self.inner.lock().await;
        if let Some(mut r) = inner.running.take() {
            let _ = r.child.start_kill();
            let status = r.child.wait().await.ok();
            inner.last_exit = status.and_then(|s| s.code());
            true
        } else {
            false
        }
    }

    pub async fn status(&self) -> (ProcessStatus, Option<ReceiverConfig>) {
        let mut inner = self.inner.lock().await;
        // Reap a process that exited on its own.
        if let Some(r) = inner.running.as_mut() {
            if let Ok(Some(status)) = r.child.try_wait() {
                inner.last_exit = status.code().or(Some(-1));
                inner.running = None;
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

#[cfg(test)]
mod tests {
    use super::*;

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
            sender_url(&rc(StreamTransport::Udp), "100.1.2.3"),
            "udp://100.1.2.3:5600?pkt_size=1316"
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

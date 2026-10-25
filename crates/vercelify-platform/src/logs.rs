//! Deployment logs: one file per deployment (`<logs>/<app>/<deployment>.log`,
//! lines of `<time> <stream> <text>`), plus live followers. The last
//! [`KEEP_PER_APP`] logs of each app are kept.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub const KEEP_PER_APP: usize = 20;
/// A runaway build can't fill the disk with its log.
pub const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LogLine {
    /// Position in the deployment's log (0, 1, 2…), so a follower can tell
    /// which live lines it already read from the file.
    pub seq: u64,
    pub at: String,
    /// stdout / stderr / info / error
    pub stream: String,
    pub text: String,
}

#[derive(Default)]
pub struct LogHub {
    dir: PathBuf,
    live: Mutex<HashMap<String, broadcast::Sender<LogLine>>>,
}

impl LogHub {
    pub fn new(dir: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            dir: dir.into(),
            live: Mutex::default(),
        })
    }

    pub fn path(&self, app: &str, deployment: &str) -> PathBuf {
        self.dir.join(app).join(format!("{deployment}.log"))
    }

    /// Open the log of a deployment for writing.
    pub fn writer(self: &Arc<Self>, app: &str, deployment: &str) -> LogWriter {
        let path = self.path(app, deployment);
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let existing = std::fs::read_to_string(&path)
            .map(|r| r.lines().count() as u64)
            .unwrap_or(0);
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
        let tx = self
            .live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(deployment.to_string())
            .or_insert_with(|| broadcast::channel(1024).0)
            .clone();
        LogWriter {
            hub: self.clone(),
            deployment: deployment.to_string(),
            file,
            written: 0,
            tx,
            truncated: false,
            seq: existing,
        }
    }

    /// Follow a deployment that is still being written; None when finished.
    pub fn follow(&self, deployment: &str) -> Option<broadcast::Receiver<LogLine>> {
        self.live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(deployment)
            .map(|t| t.subscribe())
    }

    /// Lines written so far (the whole file, or the last `tail`).
    pub fn read(&self, app: &str, deployment: &str, tail: Option<usize>) -> Vec<LogLine> {
        let raw = std::fs::read_to_string(self.path(app, deployment)).unwrap_or_default();
        let lines: Vec<LogLine> = raw
            .lines()
            .enumerate()
            .filter_map(|(i, l)| parse_line(i as u64, l))
            .collect();
        match tail {
            Some(n) if lines.len() > n => lines[lines.len() - n..].to_vec(),
            _ => lines,
        }
    }

    /// Delete all but the newest `keep` logs of an app.
    pub fn prune(&self, app: &str, keep: usize) {
        prune_dir(&self.dir.join(app), keep);
    }

    pub fn remove_app(&self, app: &str) {
        let _ = std::fs::remove_dir_all(self.dir.join(app));
    }
}

fn prune_dir(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort();
    let n = files.len().saturating_sub(keep);
    for (_, p) in files.into_iter().take(n) {
        let _ = std::fs::remove_file(p);
    }
}

fn parse_line(seq: u64, l: &str) -> Option<LogLine> {
    let mut it = l.splitn(3, ' ');
    Some(LogLine {
        seq,
        at: it.next()?.to_string(),
        stream: it.next()?.to_string(),
        text: it.next().unwrap_or("").to_string(),
    })
}

pub struct LogWriter {
    hub: Arc<LogHub>,
    deployment: String,
    file: Option<std::fs::File>,
    written: u64,
    tx: broadcast::Sender<LogLine>,
    truncated: bool,
    seq: u64,
}

impl LogWriter {
    pub fn line(&mut self, stream: &str, text: &str) {
        // BuildKit and ANSI colours: keep the text, drop the escapes.
        let text = strip_ansi(text);
        let line = LogLine {
            seq: self.seq,
            at: crate::now(),
            stream: stream.to_string(),
            text,
        };
        if self.written < MAX_LOG_BYTES {
            self.seq += 1;
            if let Some(f) = self.file.as_mut() {
                let s = format!("{} {} {}\n", line.at, line.stream, line.text);
                self.written += s.len() as u64;
                let _ = f.write_all(s.as_bytes());
            }
            let _ = self.tx.send(line);
        } else if !self.truncated {
            self.truncated = true;
            self.seq += 1;
            if let Some(f) = self.file.as_mut() {
                let _ = writeln!(f, "{} info (log truncated at 16 MiB)", line.at);
            }
        }
    }

    pub fn info(&mut self, text: &str) {
        self.line("info", text);
    }

    pub fn error(&mut self, text: &str) {
        self.line("error", text);
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        // Followers see the channel close: the deployment's log is complete.
        self.hub
            .live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.deployment);
    }
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_follow_read_prune() {
        let dir = tempfile::tempdir().unwrap();
        let hub = LogHub::new(dir.path());
        let mut w = hub.writer("blog", "dpl_1");
        let mut rx = hub.follow("dpl_1").unwrap();
        w.info("fetching");
        w.line("stderr", "\u{1b}[32m#1 DONE\u{1b}[0m 0.1s");
        assert_eq!(rx.recv().await.unwrap().text, "fetching");
        assert_eq!(rx.recv().await.unwrap().text, "#1 DONE 0.1s");
        drop(w);
        assert!(rx.recv().await.is_err(), "closed when the writer is done");
        assert!(hub.follow("dpl_1").is_none());
        let lines = hub.read("blog", "dpl_1", None);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].stream, "stderr");
        assert_eq!((lines[0].seq, lines[1].seq), (0, 1));
        let mut again = hub.writer("blog", "dpl_1");
        let mut rx = hub.follow("dpl_1").unwrap();
        again.info("more");
        assert_eq!(rx.recv().await.unwrap().seq, 2, "numbering continues after the file");
        drop(again);
        assert_eq!(hub.read("blog", "dpl_1", Some(1))[0].text, "more");

        for i in 2..6 {
            hub.writer("blog", &format!("dpl_{i}")).info("x");
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        hub.prune("blog", 2);
        let left: Vec<_> = std::fs::read_dir(dir.path().join("blog")).unwrap().flatten().collect();
        assert_eq!(left.len(), 2);
        assert!(hub.path("blog", "dpl_5").exists());
    }
}

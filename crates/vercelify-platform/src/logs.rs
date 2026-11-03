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
            redactor: Redactor::default(),
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
    redactor: Redactor,
}

impl LogWriter {
    /// Values to hide in every line from now on (the deployment's secret
    /// variables), shown as `[redacted]`.
    pub fn redact(&mut self, r: Redactor) {
        self.redactor = r;
    }

    /// The same secret policy for persisted errors and emitted events.
    pub fn redact_text(&self, text: &str) -> String {
        self.redactor.apply(text)
    }

    /// Add newly provisioned runtime secrets without dropping build/env masks.
    pub fn hide<S: AsRef<str>>(&mut self, values: impl IntoIterator<Item = S>) {
        self.redactor = Redactor::new(
            self.redactor
                .values
                .iter()
                .cloned()
                .chain(values.into_iter().map(|v| v.as_ref().to_string())),
        );
    }

    /// One message: a multi-line one becomes several log lines (the file has
    /// one line per entry, so an embedded newline used to start a line with
    /// no time or stream and shift every number after it).
    pub fn line(&mut self, stream: &str, text: &str) {
        for text in clean_lines(text) {
            let text = self.redactor.apply(&text);
            self.one(stream, text);
        }
    }

    fn one(&mut self, stream: &str, text: String) {
        let line = LogLine {
            seq: self.seq,
            at: crate::now(),
            stream: stream.to_string(),
            text,
        };
        // Past the cap only the build's own output goes: the engine's lines
        // (why it failed, that it is live) are what the log is read for.
        let output = matches!(line.stream.as_str(), "stdout" | "stderr");
        if self.written >= MAX_LOG_BYTES && output {
            if !self.truncated {
                self.truncated = true;
                self.emit(LogLine {
                    stream: "info".into(),
                    text: "(build output truncated at 16 MiB)".into(),
                    ..line
                });
            }
            return;
        }
        self.emit(line);
    }

    fn emit(&mut self, mut line: LogLine) {
        line.seq = self.seq;
        self.seq += 1;
        if let Some(f) = self.file.as_mut() {
            let s = format!("{} {} {}\n", line.at, line.stream, line.text);
            self.written += s.len() as u64;
            let _ = f.write_all(s.as_bytes());
        }
        let _ = self.tx.send(line);
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

/// Terminal output as plain text, the way a terminal would have shown it: a
/// `\r` returns to the start of the line, so of `50%\r100%` (a progress
/// bar) only `100%` is left instead of `50%100%`; escape sequences (colours,
/// cursor moves, window titles) and other control characters go. Newlines
/// and tabs stay.
pub fn strip_ansi(s: &str) -> String {
    s.split('\n')
        .map(|l| strip_escapes(l.split('\r').rfind(|p| !p.is_empty()).unwrap_or("")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// [`strip_ansi`], one entry per line (a final newline ends the last line,
/// it doesn't start an empty one).
pub fn clean_lines(s: &str) -> Vec<String> {
    let t = strip_ansi(s);
    t.strip_suffix('\n')
        .unwrap_or(&t)
        .split('\n')
        .map(String::from)
        .collect()
}

fn strip_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters up to a final byte (@ to ~).
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC/DCS/APC/PM strings, ended by BEL or ESC \.
                Some(']' | 'P' | '_' | '^' | 'X') => {
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // Character set designations take one more character.
                Some('(' | ')' | '*' | '+') => {
                    chars.next();
                }
                _ => {}
            },
            '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Hides every nonempty secret value in log text, including short values.
/// Each line of a multi-line value (a PEM key) is hidden on its own.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    values: Vec<String>,
}

impl Redactor {
    pub const MASK: &'static str = "[redacted]";

    pub fn new<S: AsRef<str>>(values: impl IntoIterator<Item = S>) -> Self {
        let mut v: Vec<String> = values
            .into_iter()
            .flat_map(|s| {
                let s = s.as_ref();
                std::iter::once(s.trim().to_string())
                    .chain(s.lines().map(|l| l.trim().to_string()))
                    .collect::<Vec<_>>()
            })
            .filter(|s| !s.is_empty())
            .collect();
        // Longest first: a value that contains another is hidden whole.
        v.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        v.dedup();
        Self { values: v }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn apply(&self, s: &str) -> String {
        if self.values.is_empty() {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut remaining = s;
        while !remaining.is_empty() {
            // Match only the original text: a later secret must not rewrite
            // an inserted mask. Values are longest first for overlaps.
            if let Some(v) = self.values.iter().find(|v| remaining.starts_with(v.as_str())) {
                out.push_str(Self::MASK);
                remaining = &remaining[v.len()..];
            } else {
                let c = remaining.chars().next().expect("nonempty text");
                out.push(c);
                remaining = &remaining[c.len_utf8()..];
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_secrets_are_redacted_too() {
        let redactor = Redactor::new(["1234", "x", "", "long-secret-value"]);
        assert_eq!(
            redactor.apply("pin=1234 token=x key=long-secret-value"),
            "pin=[redacted] token=[redacted] key=[redacted]"
        );
        assert_eq!(Redactor::new([""]).apply("unchanged"), "unchanged");
        assert_eq!(Redactor::new(["a", "abc"]).apply("abc a"), "[redacted] [redacted]");
    }

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

    /// What a real build writes: BuildKit progress with `\r`, colours, a
    /// window title, an error message of several lines, a secret echoed by
    /// a build script. Each entry stays one line of the file with its time
    /// and stream, so reading it back gives the same lines and numbers.
    #[test]
    fn terminal_output_is_kept_line_by_line() {
        let dir = tempfile::tempdir().unwrap();
        let hub = LogHub::new(dir.path());
        let mut w = hub.writer("blog", "dpl_1");
        w.redact(Redactor::new([
            "s3cr3t-token-value",
            "-----BEGIN KEY-----\nAAAABBBBCCCC\n-----END KEY-----",
            "short",
        ]));
        w.line("stderr", "#5 [2/4] RUN npm ci\r#5 10.2 added 312 packages\r\n");
        w.line(
            "stdout",
            "\u{1b}]0;npm install\u{7}\u{1b}[1m\u{1b}[32mok\u{1b}[0m\u{1b}(B done\u{8}",
        );
        w.error("docker run failed:\nError response from daemon:\n  port is already allocated");
        w.line("stdout", "token=s3cr3t-token-value key=AAAABBBBCCCC user=short");
        w.line("stdout", "\tindented\twith tabs");
        w.line("stdout", "");
        drop(w);
        let lines = hub.read("blog", "dpl_1", None);
        let got: Vec<(&str, &str)> = lines.iter().map(|l| (l.stream.as_str(), l.text.as_str())).collect();
        assert_eq!(
            got,
            [
                ("stderr", "#5 10.2 added 312 packages"),
                ("stdout", "ok done"),
                ("error", "docker run failed:"),
                ("error", "Error response from daemon:"),
                ("error", "  port is already allocated"),
                ("stdout", "token=[redacted] key=[redacted] user=[redacted]"),
                ("stdout", "\tindented\twith tabs"),
                ("stdout", ""),
            ]
        );
        assert!(lines.iter().enumerate().all(|(i, l)| l.seq == i as u64));
        assert!(lines.iter().all(|l| l.at.ends_with('Z')), "every line has its time");
    }

    #[test]
    fn past_the_cap_the_engines_lines_still_go() {
        let dir = tempfile::tempdir().unwrap();
        let hub = LogHub::new(dir.path());
        let mut w = hub.writer("blog", "dpl_1");
        let chunk = "x".repeat(64 * 1024);
        for _ in 0..(MAX_LOG_BYTES / chunk.len() as u64 + 2) {
            w.line("stdout", &chunk);
        }
        w.line("stderr", "dropped");
        w.error("build failed: exit status 1");
        drop(w);
        let lines = hub.read("blog", "dpl_1", Some(3));
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts[1..],
            ["(build output truncated at 16 MiB)", "build failed: exit status 1"]
        );
        assert!(
            lines.windows(2).all(|p| p[1].seq == p[0].seq + 1),
            "numbering has no gaps"
        );
    }
}

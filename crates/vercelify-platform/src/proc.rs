//! Running host tools (git, docker) with timeouts, cancellation and output
//! streamed line by line into a build log. Secrets go in the environment,
//! never in arguments (`ps` shows arguments to every user).

use crate::{Error, Result};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;

#[derive(Debug, Clone)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// The last non-empty lines of stderr (else stdout), for error messages.
    pub fn tail(&self, n: usize) -> String {
        let src = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        let lines: Vec<&str> = src.lines().filter(|l| !l.trim().is_empty()).collect();
        lines[lines.len().saturating_sub(n)..].join("\n")
    }
}

/// A cancellation signal shared by everything one deployment runs.
#[derive(Clone)]
pub struct Cancel(watch::Receiver<bool>);

pub struct CancelHandle(watch::Sender<bool>);

pub fn cancel_pair() -> (CancelHandle, Cancel) {
    let (tx, rx) = watch::channel(false);
    (CancelHandle(tx), Cancel(rx))
}

impl CancelHandle {
    pub fn cancel(&self) {
        let _ = self.0.send(true);
    }
}

impl Cancel {
    pub fn never() -> Self {
        // A sender that lives forever: never cancelled.
        let (tx, rx) = watch::channel(false);
        std::mem::forget(tx);
        Cancel(rx)
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    pub async fn cancelled(&mut self) {
        while !*self.0.borrow() {
            if self.0.changed().await.is_err() {
                // Sender gone without cancelling: wait forever.
                std::future::pending::<()>().await;
            }
        }
    }
}

pub struct Spec<'a> {
    pub bin: &'a str,
    pub args: &'a [String],
    pub env: &'a [(String, String)],
    pub cwd: Option<&'a Path>,
    pub timeout: Duration,
}

fn command(spec: &Spec) -> Command {
    let mut cmd = Command::new(spec.bin);
    cmd.args(spec.args)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in spec.env {
        cmd.env(k, v);
    }
    if let Some(d) = spec.cwd {
        cmd.current_dir(d);
    }
    cmd
}

fn spawn_error(bin: &str, e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        Error::Unavailable(format!("{bin} is not installed"))
    } else {
        Error::Internal(format!("{bin}: {e}"))
    }
}

/// Run to completion and capture output.
pub async fn run(spec: Spec<'_>) -> Result<Output> {
    let child = command(&spec).spawn().map_err(|e| spawn_error(spec.bin, e))?;
    match tokio::time::timeout(spec.timeout, child.wait_with_output()).await {
        Err(_) => Err(Error::Unavailable(format!(
            "{} timed out after {}s",
            spec.bin,
            spec.timeout.as_secs()
        ))),
        Ok(Err(e)) => Err(Error::Internal(format!("{}: {e}", spec.bin))),
        Ok(Ok(o)) => Ok(Output {
            code: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }),
    }
}

/// Run, calling `line(stream, text)` for every output line as it arrives;
/// stops (and kills the process) on timeout or cancellation. The last 200
/// lines are also returned.
pub async fn stream(spec: Spec<'_>, mut cancel: Cancel, line: &mut (dyn FnMut(&str, &str) + Send)) -> Result<Output> {
    let mut child = command(&spec).spawn().map_err(|e| spawn_error(spec.bin, e))?;
    let mut out = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut err = BufReader::new(child.stderr.take().expect("piped")).lines();
    let deadline = tokio::time::sleep(spec.timeout);
    tokio::pin!(deadline);
    let (mut out_done, mut err_done) = (false, false);
    let mut tail: std::collections::VecDeque<String> = Default::default();
    let keep = |s: &str, tail: &mut std::collections::VecDeque<String>| {
        if tail.len() == 200 {
            tail.pop_front();
        }
        tail.push_back(s.to_string());
    };
    while !(out_done && err_done) {
        tokio::select! {
            l = out.next_line(), if !out_done => match l {
                Ok(Some(l)) => { line("stdout", &l); keep(&l, &mut tail); }
                _ => out_done = true,
            },
            l = err.next_line(), if !err_done => match l {
                Ok(Some(l)) => { line("stderr", &l); keep(&l, &mut tail); }
                _ => err_done = true,
            },
            _ = &mut deadline => {
                let _ = child.kill().await;
                return Err(Error::Unavailable(format!("{} timed out after {}s", spec.bin, spec.timeout.as_secs())));
            }
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(Error::Conflict("cancelled".into()));
            }
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|e| Error::Internal(format!("{}: {e}", spec.bin)))?;
    Ok(Output {
        code: status.code(),
        stdout: String::new(),
        stderr: tail.into_iter().collect::<Vec<_>>().join("\n"),
    })
}

/// Convenience for tests and simple calls.
pub fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_and_streams() {
        let a = args(&["-c", "echo out; echo err >&2; exit 3"]);
        let o = run(Spec {
            bin: "sh",
            args: &a,
            env: &[],
            cwd: None,
            timeout: Duration::from_secs(5),
        })
        .await
        .unwrap();
        assert_eq!(o.code, Some(3));
        assert_eq!(o.stdout.trim(), "out");
        assert_eq!(o.tail(5), "err");

        let mut seen = vec![];
        let env = vec![("X".to_string(), "secret".to_string())];
        let a = args(&["-c", "echo $X; echo two >&2"]);
        let o = stream(
            Spec {
                bin: "sh",
                args: &a,
                env: &env,
                cwd: None,
                timeout: Duration::from_secs(5),
            },
            Cancel::never(),
            &mut |s, l| seen.push(format!("{s}:{l}")),
        )
        .await
        .unwrap();
        assert!(o.success());
        seen.sort();
        assert_eq!(seen, ["stderr:two", "stdout:secret"]);
    }

    #[tokio::test]
    async fn timeouts_cancellation_and_missing_tools() {
        let a = args(&["-c", "sleep 30"]);
        let spec = || Spec {
            bin: "sh",
            args: &a,
            env: &[],
            cwd: None,
            timeout: Duration::from_millis(200),
        };
        assert!(matches!(run(spec()).await, Err(Error::Unavailable(m)) if m.contains("timed out")));
        let (h, c) = cancel_pair();
        let long = Spec {
            timeout: Duration::from_secs(30),
            ..spec()
        };
        let t = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            h.cancel();
        });
        let r = stream(long, c, &mut |_, _| {}).await;
        assert!(matches!(r, Err(Error::Conflict(m)) if m == "cancelled"));
        t.await.unwrap();
        let none: Vec<String> = vec![];
        let r = run(Spec {
            bin: "definitely-not-installed-xyz",
            args: &none,
            env: &[],
            cwd: None,
            timeout: Duration::from_secs(1),
        })
        .await;
        assert!(matches!(r, Err(Error::Unavailable(m)) if m.contains("not installed")));
    }
}

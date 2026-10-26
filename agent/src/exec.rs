//! Running external tools (docker, git, systemctl) with timeouts.

use std::time::Duration;
use tokio::process::Command;

#[derive(Debug)]
pub struct Output {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("`{0}` is not installed or not on PATH")]
    NotFound(String),
    #[error("`{0}` timed out after {1:?}")]
    Timeout(String, Duration),
    #[error("failed to run `{0}`: {1}")]
    Io(String, std::io::Error),
}

pub async fn run(bin: &str, args: &[&str], timeout: Duration) -> Result<Output, ExecError> {
    run_env(bin, args, &[], timeout).await
}

/// Like [`run`], with extra environment variables (kept out of the argument
/// list so secrets don't show up in `ps`).
pub async fn run_env(bin: &str, args: &[&str], env: &[(&str, &str)], timeout: Duration) -> Result<Output, ExecError> {
    let mut cmd = Command::new(bin);
    cmd.args(args).kill_on_drop(true).stdin(std::process::Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let fut = cmd.output();
    match tokio::time::timeout(timeout, fut).await {
        Err(_) => Err(ExecError::Timeout(format!("{bin} {}", args.join(" ")), timeout)),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Err(ExecError::NotFound(bin.into())),
        Ok(Err(e)) => Err(ExecError::Io(bin.into(), e)),
        Ok(Ok(o)) => Ok(Output {
            status: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }),
    }
}

/// `which`-style lookup.
pub fn find_on_path(bin: &str) -> Option<std::path::PathBuf> {
    if bin.contains('/') {
        let p = std::path::PathBuf::from(bin);
        return p.is_file().then_some(p);
    }
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(bin)).find(|p| p.is_file()))
}

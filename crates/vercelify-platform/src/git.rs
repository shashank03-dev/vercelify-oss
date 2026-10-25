//! Fetching source code (design §6.2 "fetch").
//!
//! git runs with every inherited configuration ignored, no prompts, no hooks,
//! no submodules, and only https or ssh transports. Credentials travel in the
//! environment (`GIT_CONFIG_*`, `GIT_SSH_COMMAND`), never in arguments, and
//! never in the repository's own config.

use crate::proc::{self, Cancel, Spec};
use crate::{validate, Error, Result};
use base64::Engine;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub enum GitAuth {
    #[default]
    None,
    /// HTTPS token (GitHub: user `x-access-token`; GitLab: `oauth2`).
    Token { user: String, token: String },
    /// SSH deploy key, with a known_hosts file of its own.
    DeployKey { key: PathBuf, known_hosts: PathBuf },
}

pub struct Git {
    pub bin: String,
    pub timeout: Duration,
}

impl Default for Git {
    fn default() -> Self {
        Self {
            bin: "git".into(),
            timeout: Duration::from_secs(600),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    pub sha: String,
    pub message: String,
}

fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"))
}

pub fn env(auth: &GitAuth) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = [
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_ASKPASS", "/bin/false"),
        ("SSH_ASKPASS", "/bin/false"),
        ("GIT_ALLOW_PROTOCOL", "https:ssh"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_LFS_SKIP_SMUDGE", "1"),
        ("HOME", "/nonexistent"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    // Hardening that must not depend on the repository: hooks off, no
    // filesystem monitor, no submodule recursion, no protocol downgrade.
    let mut cfg: Vec<(String, String)> = vec![
        ("core.hooksPath".into(), "/dev/null".into()),
        ("core.fsmonitor".into(), "false".into()),
        ("submodule.recurse".into(), "false".into()),
        ("protocol.file.allow".into(), "never".into()),
        ("protocol.ext.allow".into(), "never".into()),
        ("http.followRedirects".into(), "initial".into()),
    ];
    match auth {
        GitAuth::None => {}
        GitAuth::Token { user, token } => {
            let basic = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{token}"));
            cfg.push(("http.extraHeader".into(), format!("Authorization: Basic {basic}")));
        }
        GitAuth::DeployKey { key, known_hosts } => {
            env.push((
                "GIT_SSH_COMMAND".into(),
                format!(
                    "ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new \
                     -o UserKnownHostsFile={} -o ConnectTimeout=20",
                    shell_quote(key),
                    shell_quote(known_hosts)
                ),
            ));
        }
    }
    env.push(("GIT_CONFIG_COUNT".into(), cfg.len().to_string()));
    for (i, (k, v)) in cfg.into_iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), k));
        env.push((format!("GIT_CONFIG_VALUE_{i}"), v));
    }
    env
}

/// Turn git's error into something a person can act on.
fn explain(out: &proc::Output, repo: &str) -> String {
    let tail = out.tail(3);
    let lower = tail.to_ascii_lowercase();
    let hint = if lower.contains("authentication failed")
        || lower.contains("could not read username")
        || lower.contains("permission denied (publickey)")
        || lower.contains("repository not found")
    {
        " — is the repository private? Add a token or deploy key for it"
    } else if lower.contains("could not resolve host") || lower.contains("timed out") {
        " — this server can't reach the git host"
    } else if lower.contains("not our ref") || lower.contains("couldn't find remote ref") {
        " — that commit or branch doesn't exist (force-pushed?)"
    } else {
        ""
    };
    format!("git could not fetch {repo}: {tail}{hint}")
}

impl Git {
    async fn git(
        &self,
        args: &[String],
        auth: &GitAuth,
        cwd: Option<&Path>,
        timeout: Duration,
    ) -> Result<proc::Output> {
        let env = env(auth);
        proc::run(Spec {
            bin: &self.bin,
            args,
            env: &env,
            cwd,
            timeout,
        })
        .await
    }

    /// The commit a branch points to, without cloning (`git ls-remote`).
    pub async fn resolve(&self, repo: &str, branch: &str, auth: &GitAuth) -> Result<String> {
        validate::repository(repo)?;
        validate::branch(branch)?;
        let args = proc::args(&["ls-remote", "--heads", "--", repo, &format!("refs/heads/{branch}")]);
        let out = self.git(&args, auth, None, Duration::from_secs(30)).await?;
        if !out.success() {
            return Err(Error::Unavailable(explain(&out, repo)));
        }
        parse_ls_remote(&out.stdout, branch).ok_or_else(|| Error::NotFound(format!("{repo} has no branch {branch}")))
    }

    /// Fetch one commit (or a branch head) into `dest` (created, must not
    /// exist yet) and check it out. Shallow: one commit of history.
    pub async fn fetch(
        &self,
        repo: &str,
        rev: &str,
        dest: &Path,
        auth: &GitAuth,
        cancel: Cancel,
        log: &mut (dyn FnMut(&str, &str) + Send),
    ) -> Result<Fetched> {
        validate::repository(repo)?;
        if validate::commit_sha(rev).is_err() {
            validate::branch(rev)?;
        }
        if dest.exists() {
            return Err(Error::Conflict(format!("{} already exists", dest.display())));
        }
        std::fs::create_dir_all(dest)?;
        let d = Some(dest);
        let quick = Duration::from_secs(30);
        let init = self.git(&proc::args(&["init", "--quiet", "."]), auth, d, quick).await?;
        if !init.success() {
            return Err(Error::Internal(format!("git init failed: {}", init.tail(2))));
        }
        let remote = self
            .git(&proc::args(&["remote", "add", "origin", "--", repo]), auth, d, quick)
            .await?;
        if !remote.success() {
            return Err(Error::Internal(format!("git remote add failed: {}", remote.tail(2))));
        }
        log("info", &format!("fetching {repo} at {rev}"));
        let args = proc::args(&[
            "fetch",
            "--depth",
            "1",
            "--no-tags",
            "--recurse-submodules=no",
            "--progress",
            "origin",
            "--",
            rev,
        ]);
        let env = env(auth);
        let out = proc::stream(
            Spec {
                bin: &self.bin,
                args: &args,
                env: &env,
                cwd: d,
                timeout: self.timeout,
            },
            cancel,
            &mut |s, l| {
                // git's progress lines repeat with \r; keep the log readable.
                if !l.contains('\r') {
                    log(s, l)
                }
            },
        )
        .await?;
        if !out.success() {
            return Err(Error::Unavailable(explain(&out, repo)));
        }
        let co = self
            .git(
                &proc::args(&[
                    "-c",
                    "advice.detachedHead=false",
                    "checkout",
                    "--quiet",
                    "--detach",
                    "FETCH_HEAD",
                ]),
                auth,
                d,
                Duration::from_secs(120),
            )
            .await?;
        if !co.success() {
            return Err(Error::Internal(format!("git checkout failed: {}", co.tail(3))));
        }
        let info = self
            .git(&proc::args(&["log", "-1", "--format=%H%n%s"]), auth, d, quick)
            .await?;
        let mut lines = info.stdout.lines();
        let sha = lines.next().unwrap_or("").trim().to_string();
        validate::commit_sha(&sha).map_err(|_| Error::Internal("git: no commit after checkout".into()))?;
        let message = lines.next().unwrap_or("").trim().to_string();
        // The source is only read from here on: drop .git so a build can't
        // see credentials or history it wasn't meant to.
        let _ = std::fs::remove_dir_all(dest.join(".git"));
        Ok(Fetched { sha, message })
    }
}

/// The sha of `refs/heads/<branch>` in `git ls-remote` output.
pub fn parse_ls_remote(out: &str, branch: &str) -> Option<String> {
    let want = format!("refs/heads/{branch}");
    out.lines().find_map(|l| {
        let (sha, r) = l.split_once('\t')?;
        (r.trim() == want && validate::commit_sha(sha.trim()).is_ok()).then(|| sha.trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn have_git() -> bool {
        std::process::Command::new("git").arg("--version").output().is_ok()
    }

    #[test]
    fn credentials_stay_out_of_arguments() {
        let e = env(&GitAuth::Token {
            user: "x-access-token".into(),
            token: "ghp_secret".into(),
        });
        let get = |k: &str| e.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        assert_eq!(get("GIT_CONFIG_GLOBAL").as_deref(), Some("/dev/null"));
        let n: usize = get("GIT_CONFIG_COUNT").unwrap().parse().unwrap();
        let header = (0..n)
            .find(|i| get(&format!("GIT_CONFIG_KEY_{i}")).as_deref() == Some("http.extraHeader"))
            .and_then(|i| get(&format!("GIT_CONFIG_VALUE_{i}")))
            .unwrap();
        let b64 = header.strip_prefix("Authorization: Basic ").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        assert_eq!(decoded, b"x-access-token:ghp_secret");
        assert!((0..n).any(|i| get(&format!("GIT_CONFIG_KEY_{i}")).as_deref() == Some("core.hooksPath")));

        let e = env(&GitAuth::DeployKey {
            key: "/var/lib/vercelify/keys/it's.key".into(),
            known_hosts: "/var/lib/vercelify/known_hosts".into(),
        });
        let ssh = e.iter().find(|(k, _)| k == "GIT_SSH_COMMAND").unwrap();
        assert!(ssh.1.contains(r"-i '/var/lib/vercelify/keys/it'\''s.key'"));
        assert!(ssh.1.contains("IdentitiesOnly=yes"));
    }

    #[test]
    fn ls_remote_output() {
        let out = "0123456789abcdef0123456789abcdef01234567\trefs/heads/main\n\
                   1111111111111111111111111111111111111111\trefs/heads/main-old\n";
        assert_eq!(
            parse_ls_remote(out, "main").as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(parse_ls_remote(out, "dev"), None);
    }

    /// Local paths and option-like input are refused before git runs. (A
    /// real network fetch is covered by the platform contract test.)
    #[tokio::test]
    async fn rejects_local_and_option_like_inputs() {
        let g = Git::default();
        let dir = tempfile::tempdir().unwrap();
        let mut log = |_: &str, _: &str| {};
        for repo in ["/srv/repo", "file:///srv/repo", "--upload-pack=touch /tmp/x"] {
            let r = g
                .fetch(
                    repo,
                    "main",
                    &dir.path().join("src"),
                    &GitAuth::None,
                    Cancel::never(),
                    &mut log,
                )
                .await;
            assert!(matches!(r, Err(Error::Invalid(_))), "{repo}: {r:?}");
        }
        let r = g
            .fetch(
                "https://github.com/me/app",
                "--upload-pack=x",
                &dir.path().join("src"),
                &GitAuth::None,
                Cancel::never(),
                &mut log,
            )
            .await;
        assert!(matches!(r, Err(Error::Invalid(_))));
        assert!(!dir.path().join("src").exists(), "nothing created for refused input");
    }

    /// The hardened environment still lets the local steps (init, checkout,
    /// log) run.
    #[tokio::test]
    async fn hardened_env_still_runs_git() {
        if !have_git() {
            return;
        }
        let g = Git::default();
        let dir = tempfile::tempdir().unwrap();
        let out = g
            .git(
                &proc::args(&["init", "--quiet", "."]),
                &GitAuth::None,
                Some(dir.path()),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert!(out.success(), "{}", out.tail(3));
    }
}

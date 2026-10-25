//! Credentials for private repositories (design §4.10): a token, or a deploy
//! key generated here whose public half the user adds to the repository.
//! Secrets are sealed; at build time a deploy key is written to a 0600 file
//! that lives only as long as the fetch.

use crate::git::GitAuth;
use crate::ids::new_id;
use crate::proc::{self, Spec};
use crate::secrets::{aad, MasterKey};
use crate::{now, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use vercelify_core::platform::{GitAuthKind, GitProvider, GitSource, GitSourceRequest};

fn provider(s: &str) -> GitProvider {
    match s {
        "github" => GitProvider::Github,
        "gitlab" => GitProvider::Gitlab,
        "gitea" => GitProvider::Gitea,
        _ => GitProvider::Generic,
    }
}

fn auth_kind(s: &str) -> GitAuthKind {
    match s {
        "token" => GitAuthKind::Token,
        "deploy_key" => GitAuthKind::DeployKey,
        _ => GitAuthKind::None,
    }
}

fn row(r: &Row) -> rusqlite::Result<GitSource> {
    Ok(GitSource {
        id: r.get("id")?,
        name: r.get("name")?,
        provider: provider(&r.get::<_, String>("provider")?),
        auth: auth_kind(&r.get::<_, String>("auth")?),
        public_key: r.get("public_key")?,
        created_at: r.get("created_at")?,
    })
}

pub fn list(c: &Connection) -> Result<Vec<GitSource>> {
    let mut st = c.prepare("SELECT * FROM git_sources ORDER BY name")?;
    let rows = st.query_map([], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(c: &Connection, id: &str) -> Result<GitSource> {
    c.query_row("SELECT * FROM git_sources WHERE id = ?1 OR name = ?1", [id], row)
        .optional()?
        .ok_or_else(|| Error::not_found(format!("git source {id}")))
}

/// A new Ed25519 key pair from ssh-keygen: (private key in OpenSSH format, public key line).
pub async fn generate_deploy_key(ssh_keygen: &str, tmp: &Path) -> Result<(String, String)> {
    std::fs::create_dir_all(tmp)?;
    let path = tmp.join(format!("key-{}", new_id("git")));
    let args = proc::args(&[
        "-q",
        "-t",
        "ed25519",
        "-N",
        "",
        "-C",
        "vercelify deploy key",
        "-f",
        &path.to_string_lossy(),
    ]);
    let out = proc::run(Spec {
        bin: ssh_keygen,
        args: &args,
        env: &[],
        cwd: None,
        timeout: Duration::from_secs(20),
    })
    .await?;
    let res = if out.success() {
        let private = std::fs::read_to_string(&path)?;
        let public = std::fs::read_to_string(path.with_extension("pub"))?;
        Ok((private, public.trim().to_string()))
    } else {
        Err(Error::Internal(format!("ssh-keygen failed: {}", out.tail(2))))
    };
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("pub"));
    res
}

/// Store a git source. For a deploy key, pass the generated pair.
pub fn create(
    c: &Connection,
    key: &MasterKey,
    req: &GitSourceRequest,
    deploy_key: Option<(String, String)>,
) -> Result<GitSource> {
    let name = req.name.trim();
    if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
        return Err(Error::Invalid("a git source name is 1–80 characters".into()));
    }
    let id = new_id("git");
    let (secret, public) = match req.auth {
        GitAuthKind::None => (None, String::new()),
        GitAuthKind::Token => {
            let t = req
                .token
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| Error::Invalid("a token is required".into()))?;
            if t.len() > 512 || t.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(Error::Invalid("that doesn't look like a token".into()));
            }
            (Some(key.seal_str(&aad("git_sources", "secret", &id), t)), String::new())
        }
        GitAuthKind::DeployKey => {
            let (private, public) =
                deploy_key.ok_or_else(|| Error::Internal("deploy key requested but none generated".into()))?;
            (Some(key.seal_str(&aad("git_sources", "secret", &id), &private)), public)
        }
    };
    let auth = match req.auth {
        GitAuthKind::None => "none",
        GitAuthKind::Token => "token",
        GitAuthKind::DeployKey => "deploy_key",
    };
    c.execute(
        "INSERT INTO git_sources (id, name, provider, auth, secret, public_key, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, name, req.provider.as_str(), auth, secret, public, now()],
    )
    .map_err(|e| match Error::from(e) {
        Error::Conflict(_) => Error::Conflict(format!("a git source named {name} already exists")),
        e => e,
    })?;
    get(c, &id)
}

/// Refused while apps use it.
pub fn delete(c: &Connection, id: &str) -> Result<()> {
    let g = get(c, id)?;
    let used: i64 = c.query_row(
        "SELECT count(*) FROM apps WHERE json_extract(source, '$.git_source_id') = ?1",
        [&g.id],
        |r| r.get(0),
    )?;
    if used > 0 {
        return Err(Error::Conflict(format!("{} is used by {used} app(s)", g.name)));
    }
    c.execute("DELETE FROM git_sources WHERE id = ?1", [&g.id])?;
    Ok(())
}

/// The token of a source (for the GitHub API: statuses, previews).
pub fn token(c: &Connection, key: &MasterKey, id: &str) -> Result<Option<String>> {
    let (auth, sealed): (String, Option<Vec<u8>>) = c
        .query_row("SELECT auth, secret FROM git_sources WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?
        .ok_or_else(|| Error::not_found(format!("git source {id}")))?;
    match (auth.as_str(), sealed) {
        ("token", Some(s)) => Ok(Some(
            key.open_string(&aad("git_sources", "secret", id), &s)?.to_string(),
        )),
        _ => Ok(None),
    }
}

/// A deploy key written for one fetch; removed when dropped.
pub struct KeyFile(PathBuf);

impl Drop for KeyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// How to authenticate a fetch with this source. `tmp` holds the key file,
/// `known_hosts` is kept across fetches (trust on first use per server).
pub fn auth(
    c: &Connection,
    key: &MasterKey,
    id: Option<&str>,
    tmp: &Path,
    known_hosts: &Path,
) -> Result<(GitAuth, Option<KeyFile>)> {
    let Some(id) = id.filter(|i| !i.is_empty()) else {
        return Ok((GitAuth::None, None));
    };
    let (prov, auth, sealed): (String, String, Option<Vec<u8>>) = c
        .query_row(
            "SELECT provider, auth, secret FROM git_sources WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| Error::not_found(format!("git source {id}")))?;
    let Some(sealed) = sealed else {
        return Ok((GitAuth::None, None));
    };
    let secret = key.open_string(&aad("git_sources", "secret", id), &sealed)?;
    match auth.as_str() {
        "token" => {
            let user = match provider(&prov) {
                GitProvider::Github => "x-access-token",
                GitProvider::Gitlab => "oauth2",
                _ => "vercelify",
            };
            Ok((
                GitAuth::Token {
                    user: user.into(),
                    token: secret.to_string(),
                },
                None,
            ))
        }
        "deploy_key" => {
            std::fs::create_dir_all(tmp)?;
            let path = tmp.join(format!("{}.key", new_id("git")));
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            f.write_all(secret.as_bytes())?;
            if !secret.ends_with('\n') {
                f.write_all(b"\n")?;
            }
            Ok((
                GitAuth::DeployKey {
                    key: path.clone(),
                    known_hosts: known_hosts.to_path_buf(),
                },
                Some(KeyFile(path)),
            ))
        }
        _ => Ok((GitAuth::None, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn key() -> MasterKey {
        MasterKey::from_bytes(&[3u8; 32]).unwrap()
    }

    #[test]
    fn tokens_are_sealed_and_resolved() {
        let s = Store::open_in_memory().unwrap();
        let k = key();
        let g = s
            .tx(|c| {
                create(
                    c,
                    &k,
                    &GitSourceRequest {
                        name: "github".into(),
                        provider: GitProvider::Github,
                        auth: GitAuthKind::Token,
                        token: Some("ghp_abc".into()),
                    },
                    None,
                )
            })
            .unwrap();
        assert_eq!(g.auth, GitAuthKind::Token);
        let raw: Vec<u8> = s
            .with(|c| Ok(c.query_row("SELECT secret FROM git_sources", [], |r| r.get(0))?))
            .unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("ghp_abc"));
        assert_eq!(s.with(|c| token(c, &k, &g.id)).unwrap().as_deref(), Some("ghp_abc"));
        let dir = tempfile::tempdir().unwrap();
        let (a, file) = s
            .with(|c| auth(c, &k, Some(&g.id), dir.path(), &dir.path().join("kh")))
            .unwrap();
        assert!(file.is_none());
        assert!(matches!(a, GitAuth::Token { user, token } if user == "x-access-token" && token == "ghp_abc"));
        assert!(s
            .tx(|c| create(
                c,
                &k,
                &GitSourceRequest {
                    name: "bad".into(),
                    provider: GitProvider::Github,
                    auth: GitAuthKind::Token,
                    token: None
                },
                None
            ))
            .is_err());
    }

    #[tokio::test]
    async fn deploy_keys() {
        if std::process::Command::new("ssh-keygen").arg("-?").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pair = generate_deploy_key("ssh-keygen", dir.path()).await.unwrap();
        assert!(pair.0.contains("OPENSSH PRIVATE KEY"));
        assert!(pair.1.starts_with("ssh-ed25519 "));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "no key left on disk");
        let s = Store::open_in_memory().unwrap();
        let k = key();
        let g = s
            .tx(|c| {
                create(
                    c,
                    &k,
                    &GitSourceRequest {
                        name: "deploy".into(),
                        provider: GitProvider::Generic,
                        auth: GitAuthKind::DeployKey,
                        token: None,
                    },
                    Some(pair.clone()),
                )
            })
            .unwrap();
        assert_eq!(g.public_key, pair.1);
        let (a, file) = s
            .with(|c| auth(c, &k, Some(&g.id), dir.path(), &dir.path().join("kh")))
            .unwrap();
        let GitAuth::DeployKey { key: path, .. } = a else {
            panic!("deploy key")
        };
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        drop(file);
        assert!(!path.exists(), "removed after the fetch");
        assert_eq!(s.with(|c| token(c, &k, &g.id)).unwrap(), None);
        s.tx(|c| delete(c, &g.id)).unwrap();
    }
}

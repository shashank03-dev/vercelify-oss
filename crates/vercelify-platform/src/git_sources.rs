//! Credentials for private repositories (design §4.10): a token, a deploy
//! key generated here whose public half the user adds to the repository, or
//! a GitHub App created for this server (its private key mints short-lived
//! installation tokens, design §11). Secrets are sealed; at build time a
//! deploy key is written to a 0600 file that lives only as long as the
//! fetch. A source may also have a webhook secret: providers sign pushes and
//! pull requests with it, and they only make the poller look sooner.

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
        "github_app" => GitAuthKind::GithubApp,
        _ => GitAuthKind::None,
    }
}

fn row(r: &Row) -> rusqlite::Result<GitSource> {
    let auth = auth_kind(&r.get::<_, String>("auth")?);
    // A GitHub App keeps its page where a deploy key keeps its public half.
    let public: String = r.get("public_key")?;
    let (public_key, app_url) = if auth == GitAuthKind::GithubApp {
        (String::new(), Some(public).filter(|u| !u.is_empty()))
    } else {
        (public, None)
    };
    Ok(GitSource {
        id: r.get("id")?,
        name: r.get("name")?,
        provider: provider(&r.get::<_, String>("provider")?),
        auth,
        public_key,
        created_at: r.get("created_at")?,
        webhook: r.get::<_, Option<Vec<u8>>>("webhook_secret")?.is_some(),
        app_url,
    })
}

/// What a GitHub App created by the manifest flow gives (sealed as JSON).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct GithubAppCreds {
    pub app_id: u64,
    pub slug: String,
    /// PKCS#1 PEM, RS256.
    pub pem: String,
    pub client_id: String,
    pub client_secret: String,
    pub html_url: String,
    pub owner: String,
}

impl std::fmt::Debug for GithubAppCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubAppCreds")
            .field("app_id", &self.app_id)
            .field("slug", &self.slug)
            .finish_non_exhaustive()
    }
}

/// Store a GitHub App (its webhook secret too).
pub fn create_github_app(
    c: &Connection,
    key: &MasterKey,
    app: &GithubAppCreds,
    webhook_secret: &str,
) -> Result<GitSource> {
    let id = new_id("git");
    let json = zeroize::Zeroizing::new(serde_json::to_string(app)?);
    let secret = key.seal_str(&aad("git_sources", "secret", &id), &json);
    let hook =
        (!webhook_secret.is_empty()).then(|| key.seal_str(&aad("git_sources", "webhook_secret", &id), webhook_secret));
    let name = format!("GitHub App {}", app.slug);
    c.execute(
        "INSERT INTO git_sources (id, name, provider, auth, secret, public_key, webhook_secret, created_at)
         VALUES (?1, ?2, 'github', 'github_app', ?3, ?4, ?5, ?6)",
        params![id, name, secret, app.html_url, hook, now()],
    )
    .map_err(|e| match Error::from(e) {
        Error::Conflict(_) => Error::Conflict(format!("{name} is set up already")),
        e => e,
    })?;
    get(c, &id)
}

pub fn github_app(c: &Connection, key: &MasterKey, id: &str) -> Result<Option<GithubAppCreds>> {
    let (auth, sealed): (String, Option<Vec<u8>>) = c
        .query_row("SELECT auth, secret FROM git_sources WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?
        .ok_or_else(|| Error::not_found(format!("git source {id}")))?;
    match (auth.as_str(), sealed) {
        ("github_app", Some(s)) => {
            let json = key.open_string(&aad("git_sources", "secret", id), &s)?;
            Ok(Some(serde_json::from_str(&json)?))
        }
        _ => Ok(None),
    }
}

/// A new webhook secret for a source (replacing any); returned once.
pub fn new_webhook_secret(c: &Connection, key: &MasterKey, id: &str) -> Result<String> {
    let g = get(c, id)?;
    let secret = crate::secrets::random_token(40);
    let sealed = key.seal_str(&aad("git_sources", "webhook_secret", &g.id), &secret);
    c.execute(
        "UPDATE git_sources SET webhook_secret = ?2 WHERE id = ?1",
        params![g.id, sealed],
    )?;
    Ok(secret)
}

pub fn webhook_secret(c: &Connection, key: &MasterKey, id: &str) -> Result<Option<zeroize::Zeroizing<String>>> {
    let sealed: Option<Vec<u8>> = c
        .query_row("SELECT webhook_secret FROM git_sources WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| Error::not_found(format!("git source {id}")))?;
    sealed
        .map(|s| key.open_string(&aad("git_sources", "webhook_secret", id), &s))
        .transpose()
}

/// `https://github.com/Me/App.git`, `git@github.com:me/app`,
/// `ssh://git@host:22/me/app.git` → `github.com/me/app` (for matching).
pub fn repo_key(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u);
    let rest = if let Some((_, r)) = u.split_once("://") {
        let r = r.rsplit_once('@').map(|(_, h)| h).unwrap_or(r);
        // ssh://host:22/path: drop the port
        match r.split_once('/') {
            Some((host, path)) => format!("{}/{}", host.split(':').next().unwrap_or(host), path),
            None => r.to_string(),
        }
    } else if let Some((user_host, path)) = u.split_once(':') {
        let host = user_host.rsplit_once('@').map(|(_, h)| h).unwrap_or(user_host);
        format!("{host}/{path}")
    } else {
        u.to_string()
    };
    rest.to_ascii_lowercase()
}

/// Apps a webhook from `source_id` about any of `repo_urls` concerns: apps
/// of that source, and apps without one (public repositories).
pub fn apps_for_repo(c: &Connection, source_id: &str, repo_urls: &[String]) -> Result<Vec<String>> {
    let keys: std::collections::HashSet<String> = repo_urls.iter().map(|u| repo_key(u)).collect();
    let mut st = c.prepare(
        "SELECT id, json_extract(source, '$.repository'), json_extract(source, '$.git_source_id') FROM apps
         WHERE kind IN ('git', 'compose') AND json_extract(source, '$.repository') IS NOT NULL",
    )?;
    let rows: Vec<(String, String, Option<String>)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .filter(|(_, repo, src)| {
            keys.contains(&repo_key(repo)) && src.as_deref().filter(|s| !s.is_empty()).is_none_or(|s| s == source_id)
        })
        .map(|(id, _, _)| id)
        .collect())
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
        GitAuthKind::GithubApp => {
            return Err(Error::Invalid(
                "a GitHub App is created with POST /v1/platform/github-app (GitHub's manifest flow)".into(),
            ))
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
        GitAuthKind::GithubApp => "github_app",
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
        // Tokens of an app are minted per repository (Engine::git_auth).
        "github_app" => Err(Error::Internal(
            "a GitHub App source needs an installation token".into(),
        )),
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

    #[test]
    fn repositories_match_across_url_forms() {
        for u in [
            "https://github.com/Me/App.git",
            "https://github.com/me/app",
            "git@github.com:me/app.git",
            "ssh://git@github.com:22/me/app.git",
            "https://x-access-token@github.com/me/app/",
        ] {
            assert_eq!(repo_key(u), "github.com/me/app", "{u}");
        }
        assert_ne!(repo_key("https://github.com/me/app2"), "github.com/me/app");
    }

    #[test]
    fn github_apps_and_webhooks() {
        let s = Store::open_in_memory().unwrap();
        let k = key();
        let creds = GithubAppCreds {
            app_id: 42,
            slug: "vercelify-laptop".into(),
            pem: "-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----\n".into(),
            client_id: "Iv1.x".into(),
            client_secret: "cs".into(),
            html_url: "https://github.com/apps/vercelify-laptop".into(),
            owner: "me".into(),
        };
        let g = s.tx(|c| create_github_app(c, &k, &creds, "hooksecret")).unwrap();
        assert_eq!(g.auth, GitAuthKind::GithubApp);
        assert!(g.webhook && g.public_key.is_empty());
        assert_eq!(g.app_url.as_deref(), Some("https://github.com/apps/vercelify-laptop"));
        assert!(format!("{creds:?}").find("BEGIN").is_none(), "no key in debug output");
        let back = s.with(|c| github_app(c, &k, &g.id)).unwrap().unwrap();
        assert_eq!((back.app_id, back.pem), (42, creds.pem.clone()));
        let raw: Vec<u8> = s
            .with(|c| Ok(c.query_row("SELECT secret FROM git_sources", [], |r| r.get(0))?))
            .unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("PRIVATE"));
        assert_eq!(
            s.with(|c| webhook_secret(c, &k, &g.id)).unwrap().unwrap().as_str(),
            "hooksecret"
        );
        let fresh = s.tx(|c| new_webhook_secret(c, &k, &g.id)).unwrap();
        assert_eq!(fresh.len(), 40);
        assert_eq!(
            s.with(|c| webhook_secret(c, &k, &g.id)).unwrap().unwrap().as_str(),
            fresh
        );
        assert!(s.with(|c| github_app(c, &k, "git_missing")).is_err());

        // Which apps a webhook concerns.
        let mut mine = crate::apps::tests::git_app("mine");
        mine.source.repository = Some("https://github.com/me/app".into());
        mine.source.git_source_id = Some(g.id.clone());
        let mut public = crate::apps::tests::git_app("public");
        public.source.repository = Some("https://github.com/me/app.git".into());
        let mut other = crate::apps::tests::git_app("other");
        other.source.repository = Some("https://github.com/me/app".into());
        let ids: Vec<String> = s
            .tx(|c| {
                let second = create(
                    c,
                    &k,
                    &GitSourceRequest {
                        name: "second".into(),
                        provider: GitProvider::Github,
                        auth: GitAuthKind::Token,
                        token: Some("ghp_x".into()),
                    },
                    None,
                )?;
                other.source.git_source_id = Some(second.id);
                let a = crate::apps::create(c, &mine)?.id;
                let b = crate::apps::create(c, &public)?.id;
                crate::apps::create(c, &other)?;
                Ok(vec![a, b])
            })
            .unwrap();
        let mut found = s
            .with(|c| apps_for_repo(c, &g.id, &["git@github.com:me/app.git".to_string()]))
            .unwrap();
        found.sort();
        let mut want = ids.clone();
        want.sort();
        assert_eq!(found, want, "the source's apps and public ones, not another source's");
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

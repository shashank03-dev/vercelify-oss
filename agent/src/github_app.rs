//! A GitHub App for this server (design §11), made with GitHub's manifest
//! flow so nobody types keys:
//!
//! 1. `POST /v1/platform/github-app` (an owner) returns a start URL with a
//!    one-time state, on this server's control listener (its public URL, or
//!    its LAN address).
//! 2. That page posts the manifest to GitHub (name, permissions, events,
//!    webhook, where to come back); the person confirms there.
//! 3. GitHub sends the browser back to `/v1/github-app/callback` with a code
//!    (or the person pastes that address into the cockpit); the code buys
//!    the app's id, private key and webhook secret, stored sealed as a git
//!    source.
//!
//! Installation tokens (1 hour, per repository) are minted with a JWT the
//! `openssl` command signs (RS256): the agent carries no RSA code.

use crate::config::Config;
use crate::github;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine as _;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use vercelify_core::platform::{GitAuthKind, GitSource, GithubAppStart};
use vercelify_platform::git_sources::{self, GithubAppCreds};
use vercelify_platform::secrets::MasterKey;
use vercelify_platform::Store;

/// One-time states of flows in progress: state → (org, started).
type Pending = HashMap<String, (Option<String>, Instant)>;
/// Installation tokens: (source, repo) → (token, valid until).
type Tokens = HashMap<(String, String), (String, Instant)>;

fn pending() -> &'static Mutex<Pending> {
    static P: OnceLock<Mutex<Pending>> = OnceLock::new();
    P.get_or_init(Default::default)
}

const STATE_TTL: Duration = Duration::from_secs(30 * 60);

fn cache() -> &'static Mutex<Tokens> {
    static C: OnceLock<Mutex<Tokens>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Where a browser (and GitHub) reaches this server's control listener:
/// its public URL, else its LAN addresses.
pub fn public_bases(cfg: &Config) -> (Option<String>, Vec<String>) {
    let public = cfg
        .mesh_public_url
        .as_deref()
        .map(|u| u.trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty());
    let port = cfg
        .mesh_control_listen
        .rsplit_once(':')
        .map(|(_, p)| p.to_string())
        .unwrap_or_else(|| "7443".into());
    let lan = crate::lan_ipv4s()
        .into_iter()
        .map(|ip| format!("http://{ip}:{port}"))
        .collect();
    (public, lan)
}

fn random(n: usize) -> String {
    vercelify_platform::secrets::random_token(n)
}

/// Begin the flow: a one-time state and the page that posts the manifest.
pub fn start(cfg: &Config, org: Option<String>) -> Result<GithubAppStart, String> {
    if let Some(o) = &org {
        if o.is_empty() || o.len() > 39 || !o.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(format!("`{o}` is not a GitHub organization name"));
        }
    }
    let (public, lan) = public_bases(cfg);
    let base = public
        .or_else(|| lan.into_iter().next())
        .ok_or("this server has no address a browser can reach (set up the mesh: sudo vercelify mesh init)")?;
    let state = random(32);
    let mut p = pending().lock().unwrap_or_else(|e| e.into_inner());
    p.retain(|_, (_, t)| t.elapsed() < STATE_TTL);
    if p.len() > 20 {
        return Err("too many GitHub App setups in progress; finish or wait for one".into());
    }
    p.insert(state.clone(), (org, Instant::now()));
    Ok(GithubAppStart {
        start_url: format!("{base}/v1/github-app/start?state={state}"),
        state,
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(30))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    })
}

fn org_of(state: &str) -> Result<Option<String>, String> {
    let p = pending().lock().unwrap_or_else(|e| e.into_inner());
    match p.get(state) {
        Some((org, t)) if t.elapsed() < STATE_TTL => Ok(org.clone()),
        _ => Err("this GitHub App setup expired or was used already: start it again".into()),
    }
}

/// The manifest (design §11): read code and metadata, write commit
/// statuses and pull request comments; push and pull_request events.
pub fn manifest(cfg: &Config, host: &str) -> serde_json::Value {
    let (public, lan) = public_bases(cfg);
    let base = public.clone().or_else(|| lan.first().cloned()).unwrap_or_default();
    let hook = match &public {
        Some(p) => serde_json::json!({ "url": format!("{p}/v1/webhooks/github"), "active": true }),
        // GitHub can't reach a LAN address: the poller does the work.
        None => serde_json::json!({ "url": format!("{base}/v1/webhooks/github"), "active": false }),
    };
    let host: String = host
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(20)
        .collect();
    serde_json::json!({
        "name": format!("vercelify-{host}-{}", random(4).to_ascii_lowercase()),
        "url": env!("CARGO_PKG_REPOSITORY"),
        "description": "Deploys your repositories to your own servers with Vercelify.",
        "hook_attributes": hook,
        "redirect_url": format!("{base}/v1/github-app/callback"),
        "public": false,
        "default_permissions": {
            "contents": "read",
            "metadata": "read",
            "statuses": "write",
            "pull_requests": "write",
        },
        "default_events": ["push", "pull_request"],
    })
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&#39;")
}

/// The page that sends the manifest to GitHub (a form posts itself).
pub fn start_page(cfg: &Config, host: &str, state: &str) -> Result<String, String> {
    let org = org_of(state)?;
    let action = match &org {
        Some(o) => format!("https://github.com/organizations/{o}/settings/apps/new?state={state}"),
        None => format!("https://github.com/settings/apps/new?state={state}"),
    };
    let manifest = manifest(cfg, host).to_string();
    Ok(format!(
        "<!doctype html><meta charset=utf-8><title>Vercelify → GitHub</title>\
         <body style=\"font:15px system-ui;margin:3em\">\
         <form id=f method=post action=\"{}\"><input type=hidden name=manifest value=\"{}\">\
         <p>Taking you to GitHub to create the app…</p><button>Continue to GitHub</button></form>\
         <script>document.getElementById('f').submit()</script>",
        html_escape(&action),
        html_escape(&manifest)
    ))
}

/// Trade GitHub's code for the app's credentials and store them.
pub async fn convert(
    cfg: &Config,
    store: &Store,
    key: &MasterKey,
    code: &str,
    state: &str,
) -> Result<GitSource, String> {
    org_of(state)?;
    if code.is_empty() || code.len() > 100 || !code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("that isn't a code from GitHub".into());
    }
    let (status, body) = github::api(cfg, "POST", &format!("/app-manifests/{code}/conversions"), None, None).await?;
    if status != 201 && status != 200 {
        return Err(format!(
            "GitHub didn't accept the code (HTTP {status}): it works once, within an hour"
        ));
    }
    pending().lock().unwrap_or_else(|e| e.into_inner()).remove(state);
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let creds = GithubAppCreds {
        app_id: v["id"].as_u64().ok_or("GitHub's answer has no app id")?,
        slug: s("slug"),
        pem: s("pem"),
        client_id: s("client_id"),
        client_secret: s("client_secret"),
        html_url: s("html_url"),
        owner: v["owner"]["login"].as_str().unwrap_or("").to_string(),
    };
    if creds.pem.is_empty() || creds.slug.is_empty() {
        return Err("GitHub's answer has no private key".into());
    }
    let hook = s("webhook_secret");
    store
        .tx(|c| git_sources::create_github_app(c, key, &creds, &hook))
        .map_err(|e| e.to_string())
}

/// The JWT of the app: RS256 over `header.claims`, signed by openssl.
pub async fn jwt(cfg: &Config, app_id: u64, pem: &str) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    let now = chrono::Utc::now().timestamp();
    let header = B64URL.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    // Issued a minute back (clock drift), valid 9 minutes (GitHub allows 10).
    let claims = B64URL.encode(format!(
        r#"{{"iat":{},"exp":{},"iss":"{app_id}"}}"#,
        now - 60,
        now + 540
    ));
    let signing = format!("{header}.{claims}");
    let key = cfg.run_dir.join(format!("gh-app-{}.pem", random(12)));
    vercelify_mesh::state::write_private(&key, pem.as_bytes()).map_err(|e| format!("{}: {e}", key.display()))?;
    let out = async {
        let mut child = tokio::process::Command::new(&cfg.openssl_bin)
            .args(["dgst", "-sha256", "-sign"])
            .arg(&key)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("running openssl (needed for GitHub Apps): {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(signing.as_bytes()).await.map_err(|e| e.to_string())?;
        }
        tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
            .await
            .map_err(|_| "openssl didn't finish".to_string())?
            .map_err(|e| e.to_string())
    }
    .await;
    let _ = std::fs::remove_file(&key);
    let out = out?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(format!(
            "signing the GitHub App token: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(format!("{signing}.{}", B64URL.encode(&out.stdout)))
}

/// Point each GitHub App's webhook at this server's public URL. A quick
/// tunnel's address changes with every restart; pushes still deploy by
/// polling, the webhook only makes them instant. Returns how many changed.
pub async fn sync_webhooks(cfg: &Config, store: &Store, key: &MasterKey) -> Result<usize, String> {
    let Some(public) = public_bases(cfg).0 else {
        return Ok(0);
    };
    let want = format!("{public}/v1/webhooks/github");
    let sources = store.with(git_sources::list).map_err(|e| e.to_string())?;
    let mut changed = 0;
    for s in sources.iter().filter(|s| s.auth == GitAuthKind::GithubApp) {
        let Some(creds) = store
            .with(|c| git_sources::github_app(c, key, &s.id))
            .map_err(|e| e.to_string())?
        else {
            continue;
        };
        let jwt = jwt(cfg, creds.app_id, &creds.pem).await?;
        let (status, body) = github::api(cfg, "GET", "/app/hook/config", Some(&jwt), None).await?;
        let current = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["url"].as_str().map(String::from));
        if status == 200 && current.as_deref() == Some(want.as_str()) {
            continue;
        }
        let body = serde_json::json!({ "url": want, "content_type": "json" });
        let (status, _) = github::api(cfg, "PATCH", "/app/hook/config", Some(&jwt), Some(&body)).await?;
        if status != 200 {
            return Err(format!(
                "GitHub refused the webhook address for {} (HTTP {status})",
                creds.slug
            ));
        }
        tracing::info!("GitHub App {}: webhook now {want}", creds.slug);
        changed += 1;
    }
    Ok(changed)
}

/// An installation token for `repo` (owner/name) from the app on `source`.
pub async fn installation_token(
    cfg: &Config,
    store: &Store,
    key: &MasterKey,
    source: &str,
    repo: &str,
) -> Result<String, String> {
    let slug = crate::platform_git::github_slug(repo).ok_or_else(|| format!("{repo} isn't a GitHub repository"))?;
    let k = (source.to_string(), slug.clone());
    if let Some((t, until)) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&k) {
        if Instant::now() < *until {
            return Ok(t.clone());
        }
    }
    let creds = store
        .with(|c| git_sources::github_app(c, key, source))
        .map_err(|e| e.to_string())?
        .ok_or("that git source isn't a GitHub App")?;
    let jwt = jwt(cfg, creds.app_id, &creds.pem).await?;
    let (status, body) = github::api(cfg, "GET", &format!("/repos/{slug}/installation"), Some(&jwt), None).await?;
    if status == 404 {
        return Err(format!(
            "the GitHub App {} isn't installed on {slug}: install it at {}/installations/new",
            creds.slug, creds.html_url
        ));
    }
    if status != 200 {
        return Err(format!("GitHub: installation of {} (HTTP {status})", creds.slug));
    }
    let installation = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["id"].as_u64())
        .ok_or("GitHub gave no installation id")?;
    let (status, body) = github::api(
        cfg,
        "POST",
        &format!("/app/installations/{installation}/access_tokens"),
        Some(&jwt),
        None,
    )
    .await?;
    if status != 201 {
        return Err(format!("GitHub refused an installation token (HTTP {status})"));
    }
    let token = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["token"].as_str().map(String::from))
        .ok_or("GitHub gave no token")?;
    // Valid an hour; used for 50 minutes.
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(k, (token.clone(), Instant::now() + Duration::from_secs(50 * 60)));
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_and_page() {
        let cfg = Config {
            mesh_public_url: Some("https://mesh.example.com/".into()),
            ..Config::default()
        };
        let m = manifest(&cfg, "Old ThinkPad");
        assert!(m["name"].as_str().unwrap().starts_with("vercelify-old-thinkpad-"));
        assert_eq!(
            m["hook_attributes"]["url"],
            "https://mesh.example.com/v1/webhooks/github"
        );
        assert_eq!(m["hook_attributes"]["active"], true);
        assert_eq!(m["redirect_url"], "https://mesh.example.com/v1/github-app/callback");
        assert_eq!(m["default_permissions"]["contents"], "read");
        let st = start(&cfg, Some("acme".into())).unwrap();
        assert!(st
            .start_url
            .starts_with("https://mesh.example.com/v1/github-app/start?state="));
        let page = start_page(&cfg, "laptop", &st.state).unwrap();
        assert!(page.contains("https://github.com/organizations/acme/settings/apps/new?state="));
        assert!(
            page.contains("name=manifest value=\"{&quot;"),
            "the manifest is escaped: {page}"
        );
        assert!(start_page(&cfg, "laptop", "bogus").is_err());
        assert!(start(&cfg, Some("bad org!".into())).is_err());
        let private = Config::default();
        let m = manifest(&private, "laptop");
        assert_eq!(
            m["hook_attributes"]["active"], false,
            "GitHub can't reach a LAN address"
        );
    }

    /// A real RS256 signature with openssl (when it is installed).
    #[tokio::test]
    async fn jwt_is_signed() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("k.pem");
        let ok = std::process::Command::new("openssl")
            .args(["genrsa", "-traditional", "-out"])
            .arg(&key)
            .arg("2048")
            .output()
            .is_ok_and(|o| o.status.success())
            || std::process::Command::new("openssl")
                .args(["genrsa", "-out"])
                .arg(&key)
                .arg("2048")
                .output()
                .is_ok_and(|o| o.status.success());
        if !ok {
            return;
        }
        let pem = std::fs::read_to_string(&key).unwrap();
        let cfg = Config {
            run_dir: dir.path().into(),
            ..Config::default()
        };
        let t = jwt(&cfg, 42, &pem).await.unwrap();
        let parts: Vec<&str> = t.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: serde_json::Value = serde_json::from_slice(&B64URL.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "42");
        assert_eq!(B64URL.decode(parts[2]).unwrap().len(), 256, "a 2048-bit RSA signature");
        // Verify it with openssl too.
        let sig = dir.path().join("sig");
        std::fs::write(&sig, B64URL.decode(parts[2]).unwrap()).unwrap();
        let pubkey = dir.path().join("pub.pem");
        std::process::Command::new("openssl")
            .args(["rsa", "-pubout", "-in"])
            .arg(&key)
            .arg("-out")
            .arg(&pubkey)
            .output()
            .unwrap();
        let mut child = std::process::Command::new("openssl")
            .args(["dgst", "-sha256", "-verify"])
            .arg(&pubkey)
            .arg("-signature")
            .arg(&sig)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{}.{}", parts[0], parts[1]).as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("Verified OK"));
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            3,
            "no key left in run_dir"
        );
    }
}

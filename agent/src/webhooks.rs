//! What git providers and GitHub's app flow call on the control listener
//! (design §11), reachable through the tunnel's public URL or on the LAN:
//!
//! - `POST /v1/webhooks/{github|gitlab|gitea}[/{source}]`: a push or pull
//!   request, signed with a git source's webhook secret (GitHub and Gitea:
//!   HMAC-SHA256 of the body; GitLab: the secret as a token). It only makes
//!   the poller look at the apps of that repository now: the payload's
//!   commit is never trusted, `git ls-remote` decides what to deploy.
//! - `GET /v1/github-app/start?state=…` and `…/callback?code=…&state=…`:
//!   the GitHub App manifest flow (github_app.rs), gated by its one-time
//!   state.

use crate::routes::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use vercelify_core::platform::{GitAuthKind, GitProvider};
use vercelify_platform::{apps, git_sources};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/webhooks/{*rest}", post(webhook))
        .route("/v1/github-app/{step}", get(github_app_step))
        .with_state(state)
}

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let (mut ipad, mut opad) = ([0x36u8; 64], [0x5cu8; 64]);
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(msg).finalize();
    Sha256::new().chain_update(opad).chain_update(inner).finalize().into()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn same(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// Whether the request is signed with `secret` the way `provider` signs.
pub fn verify(provider: GitProvider, headers: &HeaderMap, body: &[u8], secret: &str) -> bool {
    let h = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("");
    match provider {
        GitProvider::Github => {
            let sig = h("x-hub-signature-256");
            sig.strip_prefix("sha256=")
                .is_some_and(|s| same(s.as_bytes(), hex(&hmac_sha256(secret.as_bytes(), body)).as_bytes()))
        }
        GitProvider::Gitea => {
            let sig = [h("x-gitea-signature"), h("x-forgejo-signature"), h("x-gogs-signature")]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or("");
            same(sig.as_bytes(), hex(&hmac_sha256(secret.as_bytes(), body)).as_bytes())
        }
        GitProvider::Gitlab => same(h("x-gitlab-token").as_bytes(), secret.as_bytes()),
        GitProvider::Generic => false,
    }
}

/// The event kind and the repository's URLs from a payload.
pub fn event(provider: GitProvider, headers: &HeaderMap, body: &[u8]) -> (String, Vec<String>) {
    let h = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let kind = match provider {
        GitProvider::Github => h("x-github-event"),
        GitProvider::Gitea => [h("x-gitea-event"), h("x-forgejo-event"), h("x-gogs-event")]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or_default(),
        GitProvider::Gitlab => match h("x-gitlab-event").as_str() {
            "Push Hook" => "push".into(),
            "Merge Request Hook" => "pull_request".into(),
            other => other.to_ascii_lowercase(),
        },
        GitProvider::Generic => String::new(),
    };
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let repo = if provider == GitProvider::Gitlab {
        &v["project"]
    } else {
        &v["repository"]
    };
    let urls = [
        "clone_url",
        "html_url",
        "ssh_url",
        "git_http_url",
        "git_ssh_url",
        "web_url",
    ]
    .iter()
    .filter_map(|k| repo[k].as_str().map(String::from))
    .collect();
    (kind, urls)
}

/// At most 60 events a minute per source.
fn allow(source: &str) -> bool {
    static SEEN: OnceLock<Mutex<HashMap<String, (Instant, u32)>>> = OnceLock::new();
    let mut m = SEEN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    m.retain(|_, (t, _)| now.duration_since(*t) < Duration::from_secs(60));
    let e = m.entry(source.to_string()).or_insert((now, 0));
    e.1 += 1;
    e.1 <= 60
}

/// Unsigned or wrongly signed requests: at most 120 a minute in all. Each
/// one opens every webhook secret and hashes the body once per source, and
/// the tunnel puts this endpoint on the internet.
fn allow_failure(record: bool) -> bool {
    static FAILED: Mutex<Option<(Instant, u32)>> = Mutex::new(None);
    let mut f = FAILED.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    let (start, n) = f.get_or_insert((now, 0));
    if now.duration_since(*start) >= Duration::from_secs(60) {
        (*start, *n) = (now, 0);
    }
    if record {
        *n += 1;
    }
    *n < 120
}

fn reply(status: StatusCode, msg: &str) -> Response {
    (status, crate::json::Json(serde_json::json!({ "message": msg }))).into_response()
}

async fn webhook(
    State(s): State<AppState>,
    Path(rest): Path<String>,
    headers: HeaderMap,
    body: bytes::Bytes,
) -> Response {
    let (Some(p), Some(git)) = (&s.platform, &s.platform_git) else {
        return reply(
            StatusCode::SERVICE_UNAVAILABLE,
            "this server doesn't run the platform engine",
        );
    };
    let mut parts = rest.split('/');
    let provider = match parts.next() {
        Some("github") => GitProvider::Github,
        Some("gitlab") => GitProvider::Gitlab,
        Some("gitea") | Some("forgejo") => GitProvider::Gitea,
        _ => return reply(StatusCode::NOT_FOUND, "unknown provider"),
    };
    let wanted = parts.next().filter(|s| !s.is_empty()).map(String::from);
    if body.len() > 2 * 1024 * 1024 {
        return reply(StatusCode::PAYLOAD_TOO_LARGE, "payload too large");
    }
    if !allow_failure(false) {
        return reply(StatusCode::TOO_MANY_REQUESTS, "too many bad signatures; slow down");
    }
    // The sources this could be from: the one named, or every source of
    // that provider with a webhook secret (a GitHub App's URL names none).
    let candidates = p
        .store
        .with(|c| {
            let mut out = vec![];
            for g in git_sources::list(c)? {
                let fits = match &wanted {
                    Some(w) => &g.id == w,
                    None => {
                        g.provider == provider || (provider == GitProvider::Github && g.auth == GitAuthKind::GithubApp)
                    }
                };
                if fits && g.webhook {
                    if let Some(secret) = git_sources::webhook_secret(c, &p.key, &g.id)? {
                        out.push((g.id, secret));
                    }
                }
            }
            Ok(out)
        })
        .unwrap_or_default();
    let Some(source) = candidates
        .iter()
        .find(|(_, secret)| verify(provider, &headers, &body, secret))
        .map(|(id, _)| id.clone())
    else {
        allow_failure(true);
        return reply(
            StatusCode::UNAUTHORIZED,
            "bad signature (or no git source has this webhook)",
        );
    };
    if !allow(&source) {
        return reply(StatusCode::TOO_MANY_REQUESTS, "slow down");
    }
    let (kind, urls) = event(provider, &headers, &body);
    match kind.as_str() {
        "ping" => return reply(StatusCode::OK, "pong"),
        "push" | "pull_request" => {}
        other => return reply(StatusCode::ACCEPTED, &format!("ignored: {other}")),
    }
    let ids = p
        .store
        .with(|c| git_sources::apps_for_repo(c, &source, &urls))
        .unwrap_or_default();
    for id in &ids {
        let (git, store) = (git.clone(), p.store.clone());
        let id = id.clone();
        tokio::spawn(async move {
            if let Ok(app) = store.with(|c| apps::get(c, &id)) {
                if let Err(e) = git.check(&app).await {
                    tracing::info!("webhook: {}: {e}", app.name);
                }
            }
        });
    }
    tracing::info!("webhook {kind} from {source}: checking {} app(s)", ids.len());
    reply(StatusCode::ACCEPTED, &format!("checking {} app(s)", ids.len()))
}

#[derive(Deserialize)]
struct Flow {
    #[serde(default)]
    state: String,
    #[serde(default)]
    code: String,
}

fn page(title: &str, body: &str) -> Response {
    Html(format!(
        "<!doctype html><meta charset=utf-8><title>{title}</title>\
         <body style=\"font:15px system-ui;margin:3em;max-width:40em\"><h2>{title}</h2>{body}"
    ))
    .into_response()
}

async fn github_app_step(State(s): State<AppState>, Path(step): Path<String>, Query(q): Query<Flow>) -> Response {
    let Some(p) = &s.platform else {
        return page("Not available", "<p>This server doesn't run the platform engine.</p>");
    };
    let host = s.collector.host().hostname;
    match step.as_str() {
        "start" => match crate::github_app::start_page(&s.config, &host, &q.state) {
            Ok(html) => Html(html).into_response(),
            Err(e) => page("Can't continue", &format!("<p>{}</p>", html_text(&e))),
        },
        "callback" => match crate::github_app::convert(&s.config, &p.store, &p.key, &q.code, &q.state).await {
            Ok(g) => {
                s.events.changed("git-sources");
                let install = g.app_url.map(|u| format!("{u}/installations/new")).unwrap_or_default();
                page(
                    "GitHub App created",
                    &format!(
                        "<p>{} is ready. Install it on the repositories Vercelify should deploy:</p>\
                         <p><a href=\"{}\">{}</a></p><p>Then pick it as the git source of your apps.</p>",
                        html_text(&g.name),
                        html_text(&install),
                        html_text(&install)
                    ),
                )
            }
            Err(e) => page("GitHub App not created", &format!("<p>{}</p>", html_text(&e))),
        },
        _ => page("Not found", ""),
    }
}

fn html_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: a key longer than the block.
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn signatures_per_provider() {
        let body = br#"{"repository":{"clone_url":"https://github.com/me/app.git"}}"#;
        let sig = hex(&hmac_sha256(b"s3cret", body));
        let mut h = HeaderMap::new();
        h.insert(
            "x-hub-signature-256",
            HeaderValue::from_str(&format!("sha256={sig}")).unwrap(),
        );
        assert!(verify(GitProvider::Github, &h, body, "s3cret"));
        assert!(!verify(GitProvider::Github, &h, body, "other"));
        assert!(!verify(GitProvider::Github, &h, b"tampered", "s3cret"));
        let mut g = HeaderMap::new();
        g.insert("x-gitea-signature", HeaderValue::from_str(&sig).unwrap());
        assert!(verify(GitProvider::Gitea, &g, body, "s3cret"));
        let mut l = HeaderMap::new();
        l.insert("x-gitlab-token", HeaderValue::from_static("s3cret"));
        assert!(verify(GitProvider::Gitlab, &l, body, "s3cret"));
        assert!(!verify(GitProvider::Gitlab, &l, body, "s3cre"));
        assert!(!verify(GitProvider::Generic, &l, body, "s3cret"));
    }

    #[test]
    fn events_name_their_repository() {
        let mut h = HeaderMap::new();
        h.insert("x-github-event", HeaderValue::from_static("push"));
        let (k, urls) = event(
            GitProvider::Github,
            &h,
            br#"{"repository":{"clone_url":"https://github.com/me/app.git","ssh_url":"git@github.com:me/app.git"}}"#,
        );
        assert_eq!(k, "push");
        assert_eq!(urls.len(), 2);
        let mut l = HeaderMap::new();
        l.insert("x-gitlab-event", HeaderValue::from_static("Merge Request Hook"));
        let (k, urls) = event(
            GitProvider::Gitlab,
            &l,
            br#"{"project":{"git_http_url":"https://gitlab.com/me/app.git"}}"#,
        );
        assert_eq!(
            (k.as_str(), urls[0].as_str()),
            ("pull_request", "https://gitlab.com/me/app.git")
        );
    }
}

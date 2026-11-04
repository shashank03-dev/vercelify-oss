//! Guest access at the wake listener; the control API remains bearer-only.
use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::Response,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use vercelify_platform::{guest_shares as shares, proxy, secrets};
use zeroize::Zeroizing;
const COOKIE: &str = "__Host-vfy_guest";
const INVITE: &str = "/__vfy/invite";
const REDEEM: &str = "/__vfy/redeem";
const INTERNAL: &str = "/__vfy/internal-check";
const SESSION_HEADER: &str = "X-Vfy-Session";
const APP_AUTH_HEADER: &str = "X-Vfy-App-Authorization";
const TARGET_HEADER: &str = "X-Vfy-Target";
const MAX_REQUEST: u64 = 16 * 1024 * 1024;
const MAX_TRANSFER: u64 = 64 * 1024 * 1024;

fn host(headers: &HeaderMap) -> Option<String> {
    if headers.get_all(header::HOST).iter().count() != 1 {
        return None;
    }
    let raw = headers.get(header::HOST)?.to_str().ok()?;
    let (raw, port) = raw
        .split_once(':')
        .map_or((raw, None), |(host, port)| (host, Some(port)));
    if port.is_some_and(|p| p.parse::<u16>().ok().is_none_or(|p| p == 0))
        || raw.len() > 253
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return None;
    }
    vercelify_platform::validate::host(raw).ok()
}
fn cookie(headers: &HeaderMap) -> Option<String> {
    let mut found = None;
    for header in headers.get_all(header::COOKIE) {
        for item in header.to_str().ok()?.split(';') {
            let Some((key, value)) = item.trim().split_once('=') else {
                continue;
            };
            if key == COOKIE {
                if found.is_some() {
                    return None;
                }
                found = Some(value.to_string());
            }
        }
    }
    found
}
fn secure(response: &mut Response) {
    let headers = response.headers_mut();
    let cookies: Vec<_> = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|value| {
            if value.split(';').next()?.split_once('=')?.0.trim() == COOKIE {
                return None;
            }
            let value = value
                .split(';')
                .filter(|part| !part.trim().to_ascii_lowercase().starts_with("domain="))
                .collect::<Vec<_>>()
                .join(";");
            HeaderValue::from_str(&value).ok()
        })
        .collect();
    headers.remove(header::SET_COOKIE);
    for cookie in cookies {
        headers.append(header::SET_COOKIE, cookie);
    }
    for key in [
        proxy::GUEST_BRIDGE_HEADER,
        SESSION_HEADER,
        TARGET_HEADER,
        APP_AUTH_HEADER,
        "service-worker-allowed",
    ] {
        headers.remove(key);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store, max-age=0"),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(header::EXPIRES, HeaderValue::from_static("0"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert("x-robots-tag", HeaderValue::from_static("noindex, nofollow, noarchive"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.append(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("worker-src 'none'; frame-ancestors 'none'"),
    );
    headers.append(header::VARY, HeaderValue::from_static("Cookie, Authorization"));
    headers.remove(header::ETAG);
    headers.remove(header::LAST_MODIFIED);
}
fn response(status: StatusCode, text: impl Into<Body>) -> Response {
    let mut response = Response::new(text.into());
    *response.status_mut() = status;
    secure(&mut response);
    response
}
fn invitation_page(nonce: &str, existing: bool) -> String {
    format!(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta name="referrer" content="no-referrer"><title>Open preview · Vercelify</title><style nonce="{nonce}">body{{font:16px system-ui;background:#171717;color:#eee;margin:12vh auto;padding:24px;max-width:480px}}button{{padding:12px 20px;border:0;border-radius:6px;font:inherit;cursor:pointer}}p{{line-height:1.6;color:#bbb}}</style><h1>Open this preview</h1><p>Only continue if you trust the invitation. Anyone with the complete link can use it. A session lasts up to 15 minutes.</p><button id="open" disabled>Open preview</button><p id="status">Use the complete invitation link to continue.</p><script nonce="{nonce}">(()=>{{let token=new URLSearchParams(location.hash.slice(1)).get('vfy');history.replaceState(null,'',location.pathname);const button=document.getElementById('open'),status=document.getElementById('status');button.disabled=!token&&!{existing};button.addEventListener('click',async()=>{{button.disabled=true;if({existing}){{token=null;location.replace('/');return}}try{{const result=await fetch('/__vfy/redeem',{{method:'POST',credentials:'same-origin',cache:'no-store',headers:{{'Content-Type':'application/json'}},body:JSON.stringify({{token}})}});token=null;if(result.ok){{location.replace('/')}}else{{status.textContent='This invitation is expired, used or unavailable. Ask its owner for another.'}}}}catch{{status.textContent='The preview could not be reached. Open the invitation again to retry.'}}}})}})();</script></html>"#
    )
}
fn origin(host: &str, port: u16) -> String {
    if port == 443 {
        format!("https://{host}")
    } else {
        format!("https://{host}:{port}")
    }
}
struct Bucket {
    tokens: f64,
    last: Instant,
    rate: f64,
    burst: f64,
}
impl Bucket {
    fn new(rate: f64, burst: f64) -> Self {
        Self {
            tokens: burst,
            last: Instant::now(),
            rate,
            burst,
        }
    }
    fn take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * self.rate).min(self.burst);
        self.last = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}
pub struct Gateway {
    platform: Arc<crate::platform::Platform>,
    cfg: Arc<crate::config::Config>,
    sleep: Arc<crate::sleep::Sleep>,
    bridge: Zeroizing<String>,
    bridge_user: String,
    ready: std::sync::atomic::AtomicBool,
    connections: Arc<tokio::sync::Semaphore>,
    checks: Arc<tokio::sync::Semaphore>,
    rate: Mutex<Bucket>,
    redemptions: Mutex<Bucket>,
}
impl Gateway {
    pub fn new(
        platform: Arc<crate::platform::Platform>,
        cfg: Arc<crate::config::Config>,
        sleep: Arc<crate::sleep::Sleep>,
    ) -> vercelify_platform::Result<Arc<Self>> {
        platform
            .store
            .tx(|c| shares::ensure_key(c, &platform.key, chrono::Utc::now().timestamp()))?;
        let bridge = Zeroizing::new(secrets::random_token(44));
        let bridge_user = vercelify_platform::guest_routes::bridge_user(&bridge)?;
        let gateway = Arc::new(Self {
            platform,
            cfg,
            sleep,
            bridge,
            bridge_user,
            ready: std::sync::atomic::AtomicBool::new(false),
            connections: Arc::new(tokio::sync::Semaphore::new(32)),
            checks: Arc::new(tokio::sync::Semaphore::new(32)),
            rate: Mutex::new(Bucket::new(64.0, 128.0)),
            redemptions: Mutex::new(Bucket::new(1.0, 20.0)),
        });
        gateway.refresh()?;
        Ok(gateway)
    }
    pub fn is_ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn mark_ready(&self, value: bool) {
        self.ready.store(value, std::sync::atomic::Ordering::Release);
    }
    pub fn handles(&self, host: &str) -> bool {
        self.platform.store.with(shares::domains).is_ok_and(|domains| {
            domains.iter().any(|d| {
                host.strip_suffix(&format!(".{d}"))
                    .is_some_and(|label| !label.is_empty() && !label.contains('.'))
            })
        })
    }
    pub fn refresh(&self) -> vercelify_platform::Result<()> {
        let (domains, hosts, port) = self.platform.store.tx(|c| {
            let config = shares::config(c)?;
            if config.domain.is_some() {
                shares::observe_clock(c, chrono::Utc::now().timestamp())?;
            }
            let hosts = shares::routing_hosts(c)?.into_values().flatten().collect::<Vec<_>>();
            Ok((shares::domains(c)?, hosts, config.public_port))
        })?;
        let wake = self
            .cfg
            .wake_listen
            .parse::<std::net::SocketAddr>()
            .map_err(|_| vercelify_platform::Error::Invalid("guest links need a valid wake listener".into()))?;
        let yaml = vercelify_platform::guest_routes::render(
            &domains,
            &hosts,
            &format!("http://host.docker.internal:{}", wake.port()),
            port,
            &self.bridge_user,
        );
        proxy::write_atomic(
            &self
                .platform
                .engine
                .deployer
                .proxy
                .dynamic_dir()
                .join("vercelify-guest.yaml"),
            &yaml,
        )?;
        Ok(())
    }
    pub async fn wait_route(&self, host: &str, bridge: bool) -> bool {
        let until = tokio::time::Instant::now() + Duration::from_secs(8);
        loop {
            let ready = tokio::time::timeout(Duration::from_millis(750), self.probe_route(host, bridge))
                .await
                .unwrap_or(false);
            if ready {
                return true;
            }
            if tokio::time::Instant::now() >= until {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    async fn probe_route(&self, host: &str, bridge: bool) -> bool {
        use hyper_util::rt::TokioIo;
        let Ok(stream) =
            tokio::net::TcpStream::connect(("127.0.0.1", self.platform.engine.deployer.proxy.http_port)).await
        else {
            return false;
        };
        let Ok((mut sender, connection)) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await else {
            return false;
        };
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        struct Close(tokio::task::JoinHandle<()>);
        impl Drop for Close {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _connection = Close(connection);
        let public_port = self
            .platform
            .store
            .with(shares::config)
            .map(|c| c.public_port)
            .unwrap_or(443);
        let authority = if public_port == 443 {
            host.to_string()
        } else {
            format!("{host}:{public_port}")
        };
        let mut request = Request::builder()
            .method(Method::HEAD)
            .uri("/__vfy/probe")
            .header(header::HOST, authority)
            .body(Body::empty())
            .expect("probe request");
        if bridge {
            use base64::Engine as _;
            let authorization = format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("bridge:{}", self.bridge.as_str()))
            );
            request.headers_mut().insert(
                header::AUTHORIZATION,
                HeaderValue::from_str(&authorization).expect("generated authorization"),
            );
            request.headers_mut().insert(
                proxy::GUEST_BRIDGE_HEADER,
                HeaderValue::from_str(&self.bridge).expect("bridge token"),
            );
            request
                .headers_mut()
                .insert(TARGET_HEADER, HeaderValue::from_str(host).expect("validated host"));
            request
                .headers_mut()
                .insert(SESSION_HEADER, HeaderValue::from_static("invalid"));
        }
        let Ok(response) = sender.send_request(request).await else {
            return false;
        };
        if bridge {
            response.status() == StatusCode::UNAUTHORIZED && !response.headers().contains_key(header::WWW_AUTHENTICATE)
        } else {
            response.status().is_redirection()
                && response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<axum::http::Uri>().ok())
                    .is_some_and(|url| {
                        url.scheme_str() == Some("https")
                            && url.host() == Some(host)
                            && url.port_u16().unwrap_or(443) == public_port
                    })
        }
    }
    async fn allowed(&self, host: &str, secret: &str) -> Option<shares::Allowed> {
        // The permit stays with the blocking closure even if the requester
        // times out: a busy SQLite mutex cannot grow an unbounded task queue.
        let permit = self.checks.clone().try_acquire_owned().ok()?;
        let (host, secret) = (host.to_string(), Zeroizing::new(secret.to_string()));
        let work = self.platform.store.call(move |c| {
            let _permit = permit;
            shares::authorize(c, &host, &secret, chrono::Utc::now().timestamp())
        });
        tokio::time::timeout(Duration::from_millis(750), work)
            .await
            .ok()?
            .ok()
            .flatten()
    }
    async fn live(&self, host: &str, secret: &str, deadline: tokio::time::Instant) -> bool {
        tokio::time::timeout_at(deadline, self.allowed(host, secret))
            .await
            .is_ok_and(|allowed| allowed.is_some())
    }
    async fn internal(&self, headers: &HeaderMap) -> Response {
        use subtle::ConstantTimeEq;
        let bridge = headers
            .get(proxy::GUEST_BRIDGE_HEADER)
            .map(|v| v.as_bytes())
            .unwrap_or_default();
        if !bool::from(bridge.ct_eq(self.bridge.as_bytes())) {
            return response(StatusCode::UNAUTHORIZED, "Preview access denied");
        }
        let target = headers.get(TARGET_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
        let session = headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
        if self.allowed(target, session).await.is_none() {
            return response(StatusCode::UNAUTHORIZED, "Preview access denied");
        }
        let mut result = response(StatusCode::NO_CONTENT, Body::empty());
        if let Some(value) = headers.get(APP_AUTH_HEADER) {
            result.headers_mut().insert(header::AUTHORIZATION, value.clone());
        }
        result
    }
    pub async fn request(self: Arc<Self>, request: Request) -> Response {
        if !self.rate.lock().unwrap_or_else(|e| e.into_inner()).take() {
            return response(StatusCode::TOO_MANY_REQUESTS, "Please retry shortly");
        }
        if request.uri().path() == INTERNAL {
            return self.internal(request.headers()).await;
        }
        if request.headers().contains_key(proxy::GUEST_BRIDGE_HEADER) {
            return response(StatusCode::UNAUTHORIZED, "Preview access denied");
        }
        let Some(host) = host(request.headers()) else {
            return response(StatusCode::BAD_REQUEST, "Invalid host");
        };
        if !self.handles(&host) {
            return response(StatusCode::NOT_FOUND, "No preview invitation");
        }
        if request.uri().path() == REDEEM {
            return self.redeem(request, &host).await;
        }
        if request.uri().path() == INVITE || cookie(request.headers()).is_none() {
            if request.method() != Method::GET && request.method() != Method::HEAD {
                return response(StatusCode::UNAUTHORIZED, "Open the invitation first");
            }
            let existing = if let Some(secret) = cookie(request.headers()) {
                self.allowed(&host, &secret).await.is_some()
            } else {
                false
            };
            let nonce = secrets::random_token(32);
            let mut result = response(
                if request.uri().path() == INVITE {
                    StatusCode::OK
                } else {
                    StatusCode::UNAUTHORIZED
                },
                if request.method() == Method::HEAD {
                    String::new()
                } else {
                    invitation_page(&nonce, existing)
                },
            );
            result.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            let csp=format!("default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; connect-src 'self'; form-action 'self'; base-uri 'none'; object-src 'none'");
            result.headers_mut().append(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_str(&csp).expect("generated CSP"),
            );
            return result;
        }
        if request.uri().path().starts_with("/__vfy/") {
            return response(StatusCode::NOT_FOUND, "Reserved preview path");
        }
        let secret = Zeroizing::new(cookie(request.headers()).unwrap_or_default());
        let Some(allowed) = self.allowed(&host, &secret).await else {
            return response(StatusCode::UNAUTHORIZED, "Preview session expired or revoked");
        };
        if allowed.asleep {
            let private_host = allowed
                .private_url
                .split("://")
                .nth(1)
                .unwrap_or("")
                .split('/')
                .next()
                .unwrap_or("");
            let (code, page) = self.sleep.on_request(private_host).await;
            let mut result = response(
                StatusCode::from_u16(code).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
                page,
            );
            result.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            result
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
            return result;
        }
        self.proxy(request, host, secret, allowed).await
    }
    async fn redeem(&self, request: Request, host: &str) -> Response {
        use http_body_util::BodyExt;
        if request.method() != Method::POST || !self.redemptions.lock().unwrap_or_else(|e| e.into_inner()).take() {
            return response(
                StatusCode::TOO_MANY_REQUESTS,
                "Explicit confirmation required; retry shortly",
            );
        }
        let cfg = match self.platform.store.with(shares::config) {
            Ok(cfg) => cfg,
            Err(_) => return response(StatusCode::SERVICE_UNAVAILABLE, "Preview access unavailable"),
        };
        if request.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok())
            != Some(origin(host, cfg.public_port).as_str())
            || request
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').next())
                != Some("application/json")
        {
            return response(StatusCode::FORBIDDEN, "Open the HTTPS invitation and confirm access");
        }
        let bytes = match http_body_util::Limited::new(request.into_body(), 2048).collect().await {
            Ok(body) => body.to_bytes(),
            Err(_) => return response(StatusCode::BAD_REQUEST, "Invalid confirmation"),
        };
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Redemption {
            token: String,
        }
        let Ok(body) = serde_json::from_slice::<Redemption>(&bytes) else {
            return response(StatusCode::BAD_REQUEST, "Invalid confirmation");
        };
        let (target, token) = (host.to_string(), Zeroizing::new(body.token));
        let session = self
            .platform
            .store
            .call(move |c| shares::redeem(c, &target, &token, chrono::Utc::now().timestamp()))
            .await;
        let Ok(Some(session)) = session else {
            return response(StatusCode::UNAUTHORIZED, "Invitation expired, used or unavailable");
        };
        let max_age = session
            .expires_at
            .saturating_sub(chrono::Utc::now().timestamp())
            .clamp(0, shares::SESSION_SECONDS);
        let value = format!(
            "{COOKIE}={}; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}",
            session.secret.as_str()
        );
        let mut result = response(StatusCode::NO_CONTENT, Body::empty());
        result.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_str(&value).expect("generated cookie"),
        );
        result
    }
}

fn strip_hop(headers: &mut HeaderMap, websocket: bool) {
    let named: Vec<_> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|v| axum::http::HeaderName::from_bytes(v.trim().as_bytes()).ok())
        .collect();
    for name in named {
        if !websocket || name != header::UPGRADE {
            headers.remove(name);
        }
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
    ] {
        headers.remove(name);
    }
    if websocket {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    } else {
        headers.remove(header::UPGRADE);
    }
}
fn strip_request(headers: &mut HeaderMap, websocket: bool) {
    strip_hop(headers, websocket);
    let names: Vec<_> = headers
        .keys()
        .filter(|k| {
            k.as_str().starts_with("x-forwarded-") || k.as_str().starts_with("x-vfy-") || k.as_str() == "forwarded"
        })
        .cloned()
        .collect();
    for key in names {
        headers.remove(key);
    }
    headers.remove(header::IF_NONE_MATCH);
    headers.remove(header::IF_MODIFIED_SINCE);
    let cookies = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter(|item| item.trim().split_once('=').is_none_or(|(name, _)| name != COOKIE))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("; ");
    headers.remove(header::COOKIE);
    if !cookies.is_empty() {
        if let Ok(value) = HeaderValue::from_str(&cookies) {
            headers.insert(header::COOKIE, value);
        }
    }
}
impl Gateway {
    async fn proxy(
        self: Arc<Self>,
        mut request: Request,
        host: String,
        secret: Zeroizing<String>,
        allowed: shares::Allowed,
    ) -> Response {
        use hyper_util::rt::TokioIo;
        use tokio::io::AsyncReadExt;
        let Ok(permit) = self.connections.clone().try_acquire_owned() else {
            return response(StatusCode::TOO_MANY_REQUESTS, "Too many active preview connections");
        };
        if !matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::POST | Method::PUT | Method::PATCH | Method::DELETE | Method::OPTIONS
        ) {
            return response(StatusCode::METHOD_NOT_ALLOWED, "Unsupported preview method");
        }
        let websocket = request
            .headers()
            .get(header::UPGRADE)
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
            && request.method() == Method::GET;
        if request.headers().contains_key(header::UPGRADE) && !websocket {
            return response(StatusCode::BAD_REQUEST, "Unsupported upgrade");
        }
        if request
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|n| n > MAX_REQUEST)
        {
            return response(StatusCode::PAYLOAD_TOO_LARGE, "Guest uploads are limited to 16 MiB");
        }
        let frontend = websocket.then(|| hyper::upgrade::on(&mut request));
        let path = request
            .uri()
            .path_and_query()
            .map(|v| v.as_str())
            .unwrap_or("/")
            .to_string();
        let public_port = self
            .platform
            .store
            .with(shares::config)
            .map(|c| c.public_port)
            .unwrap_or(443);
        let authority = if public_port == 443 {
            host.clone()
        } else {
            format!("{host}:{public_port}")
        };
        let app_authorization = request.headers().get(header::AUTHORIZATION).cloned();
        strip_request(request.headers_mut(), websocket);
        if let Some(value) = app_authorization {
            request.headers_mut().insert(APP_AUTH_HEADER, value);
        }
        use base64::Engine as _;
        let authorization = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("bridge:{}", self.bridge.as_str()))
        );
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&authorization).expect("generated authorization"),
        );
        request
            .headers_mut()
            .insert(header::HOST, HeaderValue::from_str(&authority).expect("validated host"));
        request.headers_mut().insert(
            proxy::GUEST_BRIDGE_HEADER,
            HeaderValue::from_str(&self.bridge).expect("bridge token"),
        );
        request.headers_mut().insert(
            SESSION_HEADER,
            HeaderValue::from_str(&secret).expect("validated session"),
        );
        request
            .headers_mut()
            .insert(TARGET_HEADER, HeaderValue::from_str(&host).expect("validated host"));
        *request.uri_mut() = match path.parse() {
            Ok(path) => path,
            Err(_) => return response(StatusCode::BAD_REQUEST, "Invalid preview path"),
        };
        let (parts, body) = request.into_parts();
        let request = Request::from_parts(
            parts,
            Body::new(http_body_util::Limited::new(body, MAX_REQUEST as usize)),
        );
        let connected = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::TcpStream::connect(("127.0.0.1", self.platform.engine.deployer.proxy.http_port)),
        )
        .await;
        let Ok(Ok(stream)) = connected else {
            return response(StatusCode::SERVICE_UNAVAILABLE, "Preview temporarily unavailable");
        };
        let Ok((mut sender, connection)) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await else {
            return response(StatusCode::SERVICE_UNAVAILABLE, "Preview temporarily unavailable");
        };
        let connection = tokio::spawn(async move {
            let _ = connection.with_upgrades().await;
        });
        let request_deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let deadline = tokio::time::Instant::now()
            + Duration::from_secs(
                allowed
                    .expires_at
                    .saturating_sub(chrono::Utc::now().timestamp())
                    .clamp(0, shares::SESSION_SECONDS) as u64,
            );
        let mut tick = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(1),
            Duration::from_secs(1),
        );
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let pending = sender.send_request(request);
        tokio::pin!(pending);
        let mut backend = loop {
            tokio::select! {
                result=&mut pending=>match result{Ok(response)=>break response,Err(_)=>{connection.abort();return response(StatusCode::BAD_GATEWAY,"Preview request failed");}},
                ()=tokio::time::sleep_until(request_deadline.min(deadline))=>{connection.abort();return response(StatusCode::GATEWAY_TIMEOUT,"Preview request exceeded its time limit");},
                _=tick.tick()=>if !self.live(&host,&secret,deadline).await{connection.abort();return response(StatusCode::UNAUTHORIZED,"Preview session expired or revoked");},
            }
        };
        if tokio::time::Instant::now() >= deadline || !self.live(&host, &secret, deadline).await {
            connection.abort();
            return response(StatusCode::UNAUTHORIZED, "Preview session expired or revoked");
        }
        if backend.status() == StatusCode::UNAUTHORIZED
            && backend
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .is_some_and(|v| v.to_str().unwrap_or("").contains("Vercelify internal guest bridge"))
        {
            connection.abort();
            return response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Preview route is preparing; retry shortly",
            );
        }
        if backend.status() == StatusCode::SWITCHING_PROTOCOLS {
            if !websocket {
                connection.abort();
                return response(StatusCode::BAD_GATEWAY, "Unexpected upgrade");
            }
            let upstream = hyper::upgrade::on(&mut backend);
            let (mut parts, _) = backend.into_parts();
            strip_hop(&mut parts.headers, true);
            let mut result = Response::from_parts(parts, Body::empty());
            secure(&mut result);
            let gateway = self.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let upgraded = tokio::time::timeout(Duration::from_secs(5), async {
                    let (front, back) = tokio::try_join!(frontend.expect("websocket upgrade"), upstream)?;
                    Ok::<_, hyper::Error>((TokioIo::new(front), TokioIo::new(back)))
                })
                .await;
                if let Ok(Ok((front, back))) = upgraded {
                    let (front_read, mut front_write) = tokio::io::split(front);
                    let (back_read, mut back_write) = tokio::io::split(back);
                    let mut front_read = front_read.take(MAX_TRANSFER);
                    let mut back_read = back_read.take(MAX_TRANSFER);
                    let send = tokio::io::copy(&mut front_read, &mut back_write);
                    let receive = tokio::io::copy(&mut back_read, &mut front_write);
                    tokio::pin!(send, receive);
                    loop {
                        tokio::select! {
                            _=&mut send=>break,_=&mut receive=>break,()=tokio::time::sleep_until(deadline)=>break,
                            _=tick.tick()=>if !gateway.live(&host,&secret,deadline).await{break;},
                        }
                    }
                }
                connection.abort();
            });
            return result;
        }
        if backend
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|n| n > MAX_TRANSFER)
        {
            connection.abort();
            return response(StatusCode::BAD_GATEWAY, "Guest responses are limited to 64 MiB");
        }
        let (mut parts, body) = backend.into_parts();
        strip_hop(&mut parts.headers, false);
        let control = Arc::new(StreamControl {
            revoked: std::sync::atomic::AtomicBool::new(false),
            waker: Mutex::new(None),
        });
        let supervisor = tokio::spawn({
            let gateway = self.clone();
            let control = control.clone();
            let abort = connection.abort_handle();
            async move {
                loop {
                    tokio::select! {
                        ()=tokio::time::sleep_until(deadline)=>break,
                        _=tick.tick()=>if !gateway.live(&host,&secret,deadline).await{break;},
                    }
                }
                control.revoked.store(true, std::sync::atomic::Ordering::Release);
                abort.abort();
                if let Some(waker) = control.waker.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    waker.wake();
                }
            }
        });
        let body = GuardedBody {
            inner: body,
            remaining: MAX_TRANSFER,
            connection,
            supervisor,
            control,
            _permit: permit,
            done: false,
        };
        let mut result = Response::from_parts(parts, Body::new(body));
        secure(&mut result);
        result
    }
}
struct StreamControl {
    revoked: std::sync::atomic::AtomicBool,
    waker: Mutex<Option<std::task::Waker>>,
}
struct GuardedBody {
    inner: hyper::body::Incoming,
    remaining: u64,
    connection: tokio::task::JoinHandle<()>,
    supervisor: tokio::task::JoinHandle<()>,
    control: Arc<StreamControl>,
    _permit: tokio::sync::OwnedSemaphorePermit,
    done: bool,
}
impl Drop for GuardedBody {
    fn drop(&mut self) {
        self.supervisor.abort();
        self.connection.abort();
    }
}
impl http_body::Body for GuardedBody {
    type Data = bytes::Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use std::{pin::Pin, task::Poll};
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        *this.control.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(cx.waker().clone());
        if this.control.revoked.load(std::sync::atomic::Ordering::Acquire) {
            this.done = true;
            return Poll::Ready(Some(Err(std::io::Error::other("preview session expired or revoked"))));
        }
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    if data.len() as u64 > this.remaining {
                        this.done = true;
                        return Poll::Ready(Some(Err(std::io::Error::other("guest transfer limit reached"))));
                    }
                    this.remaining -= data.len() as u64;
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(_))) => {
                this.done = true;
                Poll::Ready(Some(Err(std::io::Error::other("preview stream ended"))))
            }
            Poll::Ready(None) => {
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
    fn is_end_stream(&self) -> bool {
        self.done || self.inner.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// Guest requests never enter the bearer-authorized control router.
pub fn wake_router(state: crate::routes::AppState) -> axum::Router {
    axum::Router::new().fallback(move |request: Request| {
        let state = state.clone();
        async move {
            if let Some(gateway) = &state.guest {
                if request.uri().path() == INTERNAL
                    || host(request.headers()).is_some_and(|host| gateway.handles(&host))
                {
                    return gateway.clone().request(request).await;
                }
            }
            let name = request
                .headers()
                .get("x-forwarded-host")
                .or_else(|| request.headers().get(header::HOST))
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let (code, page) = state.sleep.on_request(name).await;
            let mut result = Response::new(Body::from(page));
            *result.status_mut() = StatusCode::from_u16(code).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
            result.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            result
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            result
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
            result
        }
    })
}
pub async fn run(gateway: Arc<Gateway>) {
    loop {
        if let Err(error) = gateway.refresh() {
            tracing::warn!("guest routing: {error}");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header, HeaderMap};
    #[test]
    fn cookies_and_hostnames_are_unambiguous() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, "SHR-ONE.guests.example.test:443".parse().unwrap());
        assert_eq!(host(&h).as_deref(), Some("shr-one.guests.example.test"));
        h.insert(header::COOKIE, "other=1; __Host-vfy_guest=one".parse().unwrap());
        assert_eq!(cookie(&h).as_deref(), Some("one"));
        h.insert(
            header::COOKIE,
            "__Host-vfy_guest=one; __Host-vfy_guest=two".parse().unwrap(),
        );
        assert!(cookie(&h).is_none());
    }
    #[test]
    fn invitation_page_removes_the_secret_before_explicit_confirmation() {
        let page = invitation_page("fixed-nonce", false);
        assert!(page.contains("history.replaceState"));
        assert!(page.contains("button.addEventListener"));
        assert!(page.find("history.replaceState").unwrap() < page.find("fetch(").unwrap());
        assert!(!page.contains("https://"));
    }
    #[test]
    fn backend_cannot_override_guest_security_headers_or_cookie() {
        let mut response = axum::response::Response::new(axum::body::Body::empty());
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "public, max-age=86400".parse().unwrap());
        response.headers_mut().append(
            header::SET_COOKIE,
            "__Host-vfy_guest=stolen; Secure; Path=/".parse().unwrap(),
        );
        response.headers_mut().append(
            header::SET_COOKIE,
            "app=yes; Domain=example.test; Path=/; HttpOnly".parse().unwrap(),
        );
        secure(&mut response);
        assert!(response.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("no-store"));
        let values: Vec<_> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(values, vec!["app=yes; Path=/; HttpOnly"]);
        assert!(response
            .headers()
            .get_all(header::CONTENT_SECURITY_POLICY)
            .iter()
            .any(|v| v.to_str().unwrap().contains("worker-src 'none'")));
    }
}

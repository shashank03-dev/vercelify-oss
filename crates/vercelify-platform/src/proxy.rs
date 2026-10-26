//! The proxy (design §9): Traefik, run by us as `vercelify-proxy`, routing
//! only from files we write. The Docker provider is off, so no container can
//! claim a hostname by labelling itself.
//!
//! Files in the dynamic directory:
//! - `vercelify-common.yaml`: shared middlewares (HTTPS redirect)
//! - `base-<key>.yaml`: an app's or preview's routers and service, priority 100
//! - `vercelify-routes-<key>.yaml` (routing.rs): `vercel.json` rules, 5000–50000
//! - `vercelify-wake.yaml` (sleep.rs): the catch-all to the wake listener, priority 1
//!
//! Every write is atomic (temporary file, then rename), so Traefik never
//! reads half a file.

use crate::docker::{ContainerSpec, Docker, Mount, MANAGED_LABEL, PROXY_NETWORK};
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub const CONTAINER: &str = "vercelify-proxy";
pub const TRAEFIK_IMAGE: &str = "traefik:v3.7";
pub const BASE_PRIORITY: u32 = 100;
pub const COMMON_FILE: &str = "vercelify-common.yaml";
pub const HTTPS_REDIRECT: &str = "vfy-https-redirect@file";
pub const RESOLVER: &str = "le";
pub const DNS_RESOLVER: &str = "le-dns";

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// `<state>/proxy`: traefik.yml, dynamic/, acme/
    pub dir: PathBuf,
    pub http_port: u16,
    pub https_port: u16,
    /// Prometheus metrics and ping, on 127.0.0.1 only.
    pub metrics_port: u16,
    pub acme_email: Option<String>,
    /// Cloudflare DNS challenge for wildcard certificates (token in env).
    pub dns_challenge: bool,
    pub image: String,
}

impl ProxyConfig {
    pub fn dynamic_dir(&self) -> PathBuf {
        self.dir.join("dynamic")
    }

    pub fn metrics_url(&self) -> String {
        format!("http://127.0.0.1:{}/metrics", self.metrics_port)
    }
}

/// Write `content` to `path` atomically; returns whether it changed.
pub fn write_atomic(path: &Path, content: &str) -> Result<bool> {
    if std::fs::read_to_string(path).is_ok_and(|c| c == content) {
        return Ok(false);
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// YAML string literal (double-quoted, escaped).
fn q(s: &str) -> String {
    serde_json::to_string(s).expect("serializable")
}

pub fn static_config(c: &ProxyConfig) -> String {
    let mut y = String::from("# Written by vercelify-agent. Changes are overwritten.\n");
    y.push_str("global:\n  checkNewVersion: false\n  sendAnonymousUsage: false\n");
    y.push_str("log:\n  level: WARN\n");
    y.push_str("entryPoints:\n");
    y.push_str("  web:\n    address: \":80\"\n");
    y.push_str("  websecure:\n    address: \":443\"\n    http3: {}\n");
    y.push_str("  metrics:\n    address: \":8082\"\n");
    y.push_str("providers:\n  file:\n    directory: /etc/traefik/dynamic\n    watch: true\n");
    y.push_str("ping:\n  entryPoint: metrics\n");
    y.push_str(
        "metrics:\n  prometheus:\n    entryPoint: metrics\n    addRoutersLabels: true\n    addServicesLabels: true\n",
    );
    y.push_str("certificatesResolvers:\n");
    let email = c
        .acme_email
        .as_deref()
        .map(|e| format!("      email: {}\n", q(e)))
        .unwrap_or_default();
    let _ = write!(
        y,
        "  {RESOLVER}:\n    acme:\n{email}      storage: /data/acme.json\n      httpChallenge:\n        entryPoint: web\n"
    );
    if c.dns_challenge {
        let _ = write!(
            y,
            "  {DNS_RESOLVER}:\n    acme:\n{email}      storage: /data/acme-dns.json\n      dnsChallenge:\n        provider: cloudflare\n"
        );
    }
    y
}

pub fn common_dynamic() -> String {
    "# Written by vercelify-agent.\nhttp:\n  middlewares:\n    vfy-https-redirect:\n      redirectScheme:\n        scheme: https\n        permanent: true\n".into()
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RouteHost {
    pub host: String,
    pub https: bool,
    /// Redirect this host (permanently) to another one, keeping the path.
    pub redirect_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Route {
    /// File and router key: `blog`, `blog-pr-7`.
    pub key: String,
    pub hosts: Vec<RouteHost>,
    /// `http://vfy-blog-7k2m:3000`; several = load-balanced replicas.
    pub upstreams: Vec<String>,
    /// htpasswd lines (`user:$apr1$…`) for HTTP Basic Auth.
    pub basic_auth: Vec<String>,
    /// Use the DNS-challenge resolver (wildcard certificates).
    pub dns_challenge: bool,
}

/// Router names for a key, so other modules (sleep, routing) can find them.
pub fn router_name(key: &str, index: usize, secure: bool) -> String {
    if secure {
        format!("vfy-{key}-{index}-secure")
    } else {
        format!("vfy-{key}-{index}")
    }
}

pub fn service_name(key: &str) -> String {
    format!("vfy-{key}")
}

pub fn base_file(key: &str) -> String {
    format!("base-{key}.yaml")
}

pub fn render(r: &Route) -> String {
    let svc = service_name(&r.key);
    let mut routers = String::new();
    let mut middlewares = String::new();
    let auth = (!r.basic_auth.is_empty()).then(|| format!("vfy-{}-auth", r.key));
    if let Some(a) = &auth {
        let users: Vec<String> = r.basic_auth.iter().map(|u| format!("          - {}\n", q(u))).collect();
        let _ = write!(
            middlewares,
            "    {a}:\n      basicAuth:\n        users:\n{}",
            users.concat()
        );
    }
    let resolver = if r.dns_challenge { DNS_RESOLVER } else { RESOLVER };
    for (i, h) in r.hosts.iter().enumerate() {
        let rule = format!("Host(`{}`)", h.host);
        let mut mws: Vec<String> = vec![];
        let target_service = svc.clone();
        if let Some(to) = &h.redirect_to {
            let name = format!("vfy-{}-{i}-redirect", r.key);
            let scheme = if h.https { "https" } else { "http" };
            let _ = write!(
                middlewares,
                "    {name}:\n      redirectRegex:\n        regex: {}\n        replacement: {}\n        permanent: true\n",
                q("^https?://[^/]+/?(.*)"),
                q(&format!("{scheme}://{to}/${{1}}"))
            );
            mws.push(name);
        } else if let Some(a) = &auth {
            mws.push(a.clone());
        }
        let mw_yaml = |extra: &[String]| -> String {
            let all: Vec<&String> = extra.iter().chain(mws.iter()).collect();
            if all.is_empty() {
                String::new()
            } else {
                let items: Vec<String> = all.iter().map(|m| format!("        - {}\n", q(m))).collect();
                format!("      middlewares:\n{}", items.concat())
            }
        };
        if h.https {
            let _ = write!(
                routers,
                "    {}:\n      rule: {}\n      entryPoints: [web]\n      priority: {BASE_PRIORITY}\n      service: {svc}\n{}",
                router_name(&r.key, i, false),
                q(&rule),
                mw_yaml(&[HTTPS_REDIRECT.to_string()])
            );
            let _ = write!(
                routers,
                "    {}:\n      rule: {}\n      entryPoints: [websecure]\n      priority: {BASE_PRIORITY}\n      service: {target_service}\n      tls:\n        certResolver: {resolver}\n{}",
                router_name(&r.key, i, true),
                q(&rule),
                mw_yaml(&[])
            );
        } else {
            let _ = write!(
                routers,
                "    {}:\n      rule: {}\n      entryPoints: [web]\n      priority: {BASE_PRIORITY}\n      service: {target_service}\n{}",
                router_name(&r.key, i, false),
                q(&rule),
                mw_yaml(&[])
            );
        }
    }
    let servers: Vec<String> = r
        .upstreams
        .iter()
        .map(|u| format!("          - url: {}\n", q(u)))
        .collect();
    let mut y = format!(
        "# Written by vercelify-agent for {}. Changes are overwritten.\nhttp:\n",
        r.key
    );
    if !routers.is_empty() {
        let _ = write!(y, "  routers:\n{routers}");
    }
    if !middlewares.is_empty() {
        let _ = write!(y, "  middlewares:\n{middlewares}");
    }
    let _ = write!(
        y,
        "  services:\n    {svc}:\n      loadBalancer:\n        passHostHeader: true\n        servers:\n{}",
        servers.concat()
    );
    y
}

/// Write the base file of a route; returns whether it changed.
pub fn write_route(c: &ProxyConfig, r: &Route) -> Result<bool> {
    if r.upstreams.is_empty() {
        return Err(Error::Internal(format!("route {} has no upstream", r.key)));
    }
    write_atomic(&c.dynamic_dir().join(base_file(&r.key)), &render(r))
}

pub fn remove_route(c: &ProxyConfig, key: &str) -> Result<()> {
    match std::fs::remove_file(c.dynamic_dir().join(base_file(key))) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Keys of the base files present (`base-<key>.yaml`).
pub fn route_keys(c: &ProxyConfig) -> Vec<String> {
    std::fs::read_dir(c.dynamic_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.strip_prefix("base-")?.strip_suffix(".yaml").map(String::from)
        })
        .collect()
}

fn config_hash(c: &ProxyConfig) -> String {
    // The directory too: a proxy that mounts another one never sees our routes.
    let h = Sha256::digest(format!(
        "{}|{}|{}|{}|{}|{}",
        static_config(c),
        c.image,
        c.http_port,
        c.https_port,
        c.metrics_port,
        c.dir.display()
    ));
    h.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Make sure the proxy runs with the current configuration: write the static
/// and common files, (re)create the container when its configuration
/// changed, start it when stopped. `env` carries e.g. `CF_DNS_API_TOKEN`.
pub async fn ensure(docker: &Docker, c: &ProxyConfig, env: Vec<(String, String)>) -> Result<bool> {
    std::fs::create_dir_all(c.dynamic_dir())?;
    std::fs::create_dir_all(c.dir.join("acme"))?;
    write_atomic(&c.dir.join("traefik.yml"), &static_config(c))?;
    write_atomic(&c.dynamic_dir().join(COMMON_FILE), &common_dynamic())?;
    docker.ensure_network(PROXY_NETWORK).await?;
    let hash = config_hash(c);
    let current = docker.inspect(CONTAINER).await?;
    if let Some(i) = &current {
        let o = docker
            .label(CONTAINER, "vercelify.config-hash")
            .await
            .unwrap_or_default();
        if o == hash {
            if !i.running {
                docker.start(CONTAINER).await?;
                return Ok(true);
            }
            return Ok(false);
        }
        tracing::info!("proxy configuration changed: recreating {CONTAINER}");
        docker.remove(CONTAINER).await?;
    }
    if !docker.image_exists(&c.image).await? {
        docker.pull(&c.image).await?;
    }
    let spec = ContainerSpec {
        name: CONTAINER.into(),
        image: c.image.clone(),
        labels: [
            (MANAGED_LABEL.to_string(), "proxy".to_string()),
            ("vercelify.kind".to_string(), "core".to_string()),
            ("vercelify.config-hash".to_string(), hash),
        ]
        .into(),
        env,
        networks: vec![PROXY_NETWORK.into()],
        mounts: vec![
            Mount {
                source: c.dir.join("traefik.yml").to_string_lossy().into(),
                target: "/etc/traefik/traefik.yml".into(),
                read_only: true,
            },
            Mount {
                source: c.dynamic_dir().to_string_lossy().into(),
                target: "/etc/traefik/dynamic".into(),
                read_only: true,
            },
            Mount {
                source: c.dir.join("acme").to_string_lossy().into(),
                target: "/data".into(),
                read_only: false,
            },
        ],
        publish: vec![
            format!("{}:80", c.http_port),
            format!("{}:443/tcp", c.https_port),
            format!("{}:443/udp", c.https_port),
            format!("127.0.0.1:{}:8082", c.metrics_port),
        ],
        extra_hosts: vec!["host.docker.internal:host-gateway".into()],
        restart: "unless-stopped".into(),
        ..ContainerSpec::default()
    };
    docker.create(&spec).await?;
    docker.start(CONTAINER).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> Route {
        Route {
            key: "blog".into(),
            hosts: vec![
                RouteHost {
                    host: "blog.example.com".into(),
                    https: true,
                    redirect_to: None,
                },
                RouteHost {
                    host: "www.example.com".into(),
                    https: true,
                    redirect_to: Some("blog.example.com".into()),
                },
                RouteHost {
                    host: "blog.10.77.0.1.sslip.io".into(),
                    https: false,
                    redirect_to: None,
                },
            ],
            upstreams: vec!["http://vfy-blog-abc:3000".into()],
            basic_auth: vec![],
            dns_challenge: false,
        }
    }

    #[test]
    fn routes_render() {
        let y = render(&route());
        assert!(y.contains("    vfy-blog-0:\n      rule: \"Host(`blog.example.com`)\"\n      entryPoints: [web]"));
        assert!(y.contains("        - \"vfy-https-redirect@file\""));
        assert!(y.contains("    vfy-blog-0-secure:\n"));
        assert!(y.contains("        certResolver: le"));
        assert!(y.contains("vfy-blog-1-redirect:\n      redirectRegex:"));
        assert!(y.contains("\"https://blog.example.com/${1}\""));
        assert!(y.contains("    vfy-blog-2:\n      rule: \"Host(`blog.10.77.0.1.sslip.io`)\""));
        assert!(!y.contains("vfy-blog-2-secure"), "plain HTTP host has no TLS router");
        assert!(y.contains("          - url: \"http://vfy-blog-abc:3000\""));
        assert!(y.contains("priority: 100"));
    }

    #[test]
    fn basic_auth_and_dns_resolver() {
        let mut r = route();
        r.basic_auth = vec!["me:$apr1$x$y".into()];
        r.dns_challenge = true;
        let y = render(&r);
        assert!(y.contains("vfy-blog-auth:\n      basicAuth:\n        users:\n          - \"me:$apr1$x$y\""));
        assert!(y.contains("certResolver: le-dns"));
        // Redirect hosts don't ask for a password; the app's hosts do.
        let secure0 = y
            .split("vfy-blog-0-secure:")
            .nth(1)
            .unwrap()
            .split("vfy-blog-1")
            .next()
            .unwrap();
        assert!(secure0.contains("vfy-blog-auth"));
    }

    #[test]
    fn static_config_has_what_sleep_and_tls_need() {
        let c = ProxyConfig {
            dir: "/x".into(),
            http_port: 80,
            https_port: 443,
            metrics_port: 8082,
            acme_email: Some("me@example.com".into()),
            dns_challenge: false,
            image: TRAEFIK_IMAGE.into(),
        };
        let y = static_config(&c);
        assert!(y.contains("addRoutersLabels: true"));
        assert!(y.contains("directory: /etc/traefik/dynamic"));
        assert!(!y.contains("docker:"), "no Docker provider");
        assert!(y.contains("email: \"me@example.com\""));
        assert!(!y.contains("le-dns"));
        assert_eq!(c.metrics_url(), "http://127.0.0.1:8082/metrics");
    }

    #[test]
    fn files_are_written_atomically_and_listed() {
        let dir = tempfile::tempdir().unwrap();
        let c = ProxyConfig {
            dir: dir.path().into(),
            http_port: 80,
            https_port: 443,
            metrics_port: 8082,
            acme_email: None,
            dns_challenge: false,
            image: TRAEFIK_IMAGE.into(),
        };
        assert!(write_route(&c, &route()).unwrap());
        assert!(!write_route(&c, &route()).unwrap(), "unchanged: not rewritten");
        assert_eq!(route_keys(&c), vec!["blog".to_string()]);
        let mut empty = route();
        empty.upstreams.clear();
        assert!(write_route(&c, &empty).is_err());
        remove_route(&c, "blog").unwrap();
        remove_route(&c, "blog").unwrap();
        assert!(route_keys(&c).is_empty());
    }
}

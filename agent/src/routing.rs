//! Proxy routes for an app's project rules (see `project.rs`).
//!
//! The platform engine gives every domain of an app a Traefik router in its
//! base route file (`vfy-<app>-<n>`, with its service and the basic-auth
//! middleware). For each of those we add routers with the same rule at a
//! higher priority through Traefik's file provider, reusing that service and
//! middlewares, so the base routes keep working and our rules run first.
//! When the app's base route is gone (asleep, redeploying) its service
//! disappears, Traefik skips our routers as broken and the request falls
//! through as before.
//!
//! Traefik picks one router per request, so rules are placed like this:
//! - conditional middlewares go on every router: redirects (a regex over the
//!   URL that only fires when it matches), `cleanUrls`/`trailingSlash`,
//!   headers and rate limits for every path, and a catch-all fallback
//!   rewrite (Traefik's `errors` middleware: served only when the app
//!   answers 404, which is how Vercel applies rewrites after its files);
//! - everything that applies to some paths only gets its own router, ordered
//!   firewall > conditional redirects > rewrites > rate limits > headers >
//!   the rest. A path-specific header or rate limit also rides along on every
//!   other router whose paths it provably covers (`/api/(.*)` covers
//!   `/api/users/:id`).

use crate::project::{net_regex, FirewallAction, Net, Pattern, Plan, RewriteTo};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// One of an app's base routers (`projects::platform_routers`).
#[derive(Debug, Clone, PartialEq)]
pub struct AppRouter {
    pub name: String,
    pub rule: String,
    pub entry_points: Vec<String>,
    /// With its provider (`http-0-abc@docker`).
    pub service: String,
    pub middlewares: Vec<String>,
    /// `{"certResolver": …}` or `{}` for TLS routers.
    pub tls: Option<Value>,
}

/// What the running proxy supports and how visitors reach it.
#[derive(Debug, Clone, PartialEq)]
pub struct Proxy {
    /// `errors` can rewrite the status (Traefik 3.5+).
    pub status_rewrites: bool,
    /// Public traffic arrives through the Cloudflare tunnel.
    pub cloudflare: bool,
    /// Where connections from this machine reach the proxy from: loopback and
    /// the Docker networks the proxy is on (the tunnel connects from there).
    pub local: Vec<Net>,
}

impl Default for Proxy {
    fn default() -> Self {
        Self {
            status_rewrites: false,
            cloudflare: false,
            local: local_nets(None),
        }
    }
}

/// Loopback plus the proxy's Docker subnets (none for a proxy on the host
/// network). None (they couldn't be read): Docker's default bridge pool.
pub fn local_nets(subnets: Option<&[Net]>) -> Vec<Net> {
    let mut out: Vec<Net> = ["127.0.0.0/8", "::1/128"]
        .iter()
        .filter_map(|n| crate::project::parse_net(n).ok())
        .collect();
    let Some(subnets) = subnets else {
        out.extend(crate::project::parse_net("172.16.0.0/12"));
        return out;
    };
    for n in subnets {
        if !out.contains(n) {
            out.push(*n);
        }
    }
    out
}

/// `{{.Subnet}}` values of `docker network inspect`, one or more per line.
pub fn parse_subnets(out: &str) -> Vec<Net> {
    out.split_whitespace()
        .filter_map(|s| crate::project::parse_net(s).ok())
        .collect()
}

/// `traefik:v3.7`, `traefik:3.6.1` → (3, 7). `latest` → a high version.
pub fn traefik_version(image: &str) -> Option<(u32, u32)> {
    let tag = image.rsplit_once(':')?.1;
    if tag == "latest" {
        return Some((u32::MAX, 0));
    }
    let mut it = tag.trim_start_matches('v').split(['.', '-']);
    let major = it.next()?.parse().ok()?;
    let minor = it.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// The connection comes from this machine (where the Cloudflare tunnel runs).
fn local_rule(local: &[Net]) -> String {
    format!(
        "({})",
        local
            .iter()
            .map(|n| format!("ClientIP(`{n}`)"))
            .collect::<Vec<_>>()
            .join(" || ")
    )
}
const CF_HEADER: &str = "Cf-Connecting-Ip";

/// The visitor is in `nets`: its address from the tunnel's header when the
/// request comes from the tunnel (local), else the connection's address.
/// A header sent from anywhere else is ignored, so it can't be spoofed.
fn visitor_in(nets: &[Net], local: &str, what: &str, warnings: &mut Vec<String>) -> String {
    let mut res = Vec::new();
    for n in nets {
        match net_regex(n) {
            Some(r) => res.push(r),
            None => warnings.push(format!(
                "{what}: {n} can only be matched for direct visitors, not through the Cloudflare tunnel (use single IPv6 addresses or /16-multiple ranges)"
            )),
        }
    }
    let direct = nets
        .iter()
        .map(|n| format!("ClientIP(`{n}`)"))
        .collect::<Vec<_>>()
        .join(" || ");
    let direct = format!("(!{local} && ({direct}))");
    if res.is_empty() {
        direct
    } else {
        format!(
            "(({local} && HeaderRegexp(`{CF_HEADER}`, `^(?:{})$`)) || {direct})",
            res.join("|")
        )
    }
}

fn and(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| format!("({p})"))
        .collect::<Vec<_>>()
        .join(" && ")
}

fn path_rule(p: &Pattern) -> String {
    format!("PathRegexp(`{}`)", p.path)
}

/// Redirect middlewares for one rule (two when its destination has a query:
/// one merging the visitor's query in, one for requests without).
fn redirect_middlewares(
    prefix: &str,
    dest: &str,
    has_query: bool,
    permanent: bool,
    p: &Pattern,
) -> Vec<(String, Value)> {
    let flags = if p.case_insensitive { "(?i)" } else { "" };
    let head = format!("{flags}^(?P<vfys>https?)://(?P<vfyh>[^/?#]+){}", p.url_part);
    let mw = |regex: String, replacement: String| json!({ "redirectRegex": { "regex": regex, "replacement": replacement, "permanent": permanent } });
    if has_query {
        vec![
            (
                format!("{prefix}q"),
                mw(format!("{head}\\?(?P<vfyq>.+)$"), format!("{dest}&${{vfyq}}")),
            ),
            (prefix.to_string(), mw(format!("{head}\\??$"), dest.to_string())),
        ]
    } else {
        vec![(
            prefix.to_string(),
            mw(format!("{head}(?P<vfyq>\\?.*)?$"), format!("{dest}${{vfyq}}")),
        )]
    }
}

/// `cleanUrls` and `trailingSlash` redirects, in the order they apply.
fn tidy_middlewares(prefix: &str, plan: &Plan) -> Vec<(String, Value)> {
    let host = "^(?P<vfys>https?)://(?P<vfyh>[^/?#]+)";
    let q = "(?P<vfyq>\\?.*)?$";
    let slash = if plan.trailing_slash == Some(true) { "/" } else { "" };
    let mw = |regex: String, replacement: String| json!({ "redirectRegex": { "regex": regex, "replacement": replacement, "permanent": true } });
    let mut out = Vec::new();
    if plan.clean_urls {
        out.push((
            format!("{prefix}-clean-root"),
            mw(format!("{host}/index\\.html{q}"), "/${vfyq}".into()),
        ));
        out.push((
            format!("{prefix}-clean-index"),
            mw(
                format!("{host}(?P<vfyp>/[^?#]*[^/?#])/index\\.html{q}"),
                format!("${{vfyp}}{slash}${{vfyq}}"),
            ),
        ));
        out.push((
            format!("{prefix}-clean"),
            mw(
                format!("{host}(?P<vfyp>/[^?#]*[^/?#])\\.html{q}"),
                format!("${{vfyp}}{slash}${{vfyq}}"),
            ),
        ));
    }
    match plan.trailing_slash {
        // A last segment without a dot (not a file) and no slash yet.
        Some(true) => out.push((
            format!("{prefix}-slash"),
            mw(
                format!("{host}(?P<vfyp>(?:/[^/?#]+)*/[^/?#.]+){q}"),
                "${vfyp}/${vfyq}".into(),
            ),
        )),
        Some(false) => out.push((
            format!("{prefix}-slash"),
            mw(format!("{host}(?P<vfyp>/[^?#]*[^/?#])/+{q}"), "${vfyp}${vfyq}".into()),
        )),
        None => {}
    }
    out
}

/// Priority bands: the base routers sit at 100.
const P_FIREWALL: i64 = 50_000;
const P_REDIRECT: i64 = 40_000;
const P_REWRITE: i64 = 30_000;
const P_RATE: i64 = 20_000;
const P_HEADER: i64 = 10_000;
const P_BASE: i64 = 5_000;

struct Shared<'a> {
    global_rates: Vec<String>,
    /// Path-only rate limits (can ride along on routers they cover).
    path_rates: Vec<(String, &'a Pattern)>,
    /// Headers without conditions.
    headers: Vec<(String, &'a Pattern)>,
    tidy: Vec<String>,
    redirects: Vec<String>,
    fallback: Option<String>,
}

impl Shared<'_> {
    /// Middlewares every router for paths of `pat` (None: any path) carries.
    fn chain(&self, pat: Option<&Pattern>, r: &AppRouter) -> Vec<String> {
        let mut v = self.global_rates.clone();
        if let Some(p) = pat {
            v.extend(
                self.path_rates
                    .iter()
                    .filter(|(_, rp)| rp.covers(p))
                    .map(|(m, _)| m.clone()),
            );
        }
        v.extend(
            self.headers
                .iter()
                .filter(|(_, hp)| hp.catch_all || pat.is_some_and(|p| hp.covers(p)))
                .map(|(m, _)| m.clone()),
        );
        v.extend(r.middlewares.iter().cloned());
        v.extend(self.tidy.iter().cloned());
        v.extend(self.redirects.iter().cloned());
        v
    }
}

fn dedup(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(v.len());
    for m in v {
        if !out.contains(&m) {
            out.push(m);
        }
    }
    out
}

/// The Traefik dynamic configuration for one app or preview (`key`), and
/// notes on what couldn't be applied exactly. None: nothing to route.
pub fn config(key: &str, routers: &[AppRouter], plan: &Plan, proxy: &Proxy) -> (Option<Value>, Vec<String>) {
    let mut notes = Vec::new();
    if routers.is_empty() || !plan.routes_anything() {
        return (None, notes);
    }
    let pre = format!("vfy-{key}");
    let mut mws = Map::new();
    let mut services = Map::new();
    let mut out_routers = Map::new();
    let fw = &plan.firewall;

    // Middlewares shared by all of the app's routers.
    let deny = format!("{pre}-deny");
    mws.insert(
        deny.clone(),
        json!({ "ipAllowList": { "sourceRange": ["255.255.255.255/32"] } }),
    );
    let mut sh = Shared {
        global_rates: vec![],
        path_rates: vec![],
        headers: vec![],
        tidy: vec![],
        redirects: vec![],
        fallback: None,
    };
    let mut rate_names = BTreeMap::new();
    for (i, r) in fw.rules.iter().enumerate() {
        if let FirewallAction::RateLimit { requests, window_secs } = r.action {
            let name = format!("{pre}-rl{i}");
            let mut rl = json!({
                "average": requests,
                "period": format!("{window_secs}s"),
                "burst": requests,
            });
            if proxy.cloudflare {
                rl["sourceCriterion"] = json!({ "requestHeaderName": CF_HEADER });
            }
            mws.insert(name.clone(), json!({ "rateLimit": rl }));
            if r.global() {
                sh.global_rates.push(name.clone());
            } else if r.user_agent.is_none() && r.methods.is_empty() {
                if let Some(p) = &r.path {
                    sh.path_rates.push((name.clone(), p));
                }
            }
            rate_names.insert(i, name);
        }
    }
    let mut header_names = Vec::new();
    for (i, h) in plan.headers.iter().enumerate() {
        let name = format!("{pre}-h{i}");
        let mut set = Map::new();
        for (k, v) in &h.headers {
            set.insert(k.clone(), Value::String(v.clone()));
        }
        mws.insert(name.clone(), json!({ "headers": { "customResponseHeaders": set } }));
        if h.conditions.is_empty() {
            sh.headers.push((name.clone(), &h.pattern));
        }
        header_names.push(name);
    }
    for (name, mw) in tidy_middlewares(&pre, plan) {
        sh.tidy.push(name.clone());
        mws.insert(name, mw);
    }
    let mut redirect_names = Vec::new();
    for (i, r) in plan.redirects.iter().enumerate() {
        let made = redirect_middlewares(
            &format!("{pre}-r{i}"),
            &r.destination,
            r.dest_has_query,
            r.permanent,
            &r.pattern,
        );
        let names: Vec<String> = made.iter().map(|(n, _)| n.clone()).collect();
        for (n, mw) in made {
            mws.insert(n, mw);
        }
        if r.conditions.is_empty() {
            sh.redirects.extend(names.iter().cloned());
        }
        redirect_names.push(names);
    }
    // (index, path) of the first rewrite to a fixed path for every request.
    let catch_all_fallback = plan.rewrites.iter().enumerate().find_map(|(i, rw)| match &rw.to {
        RewriteTo::Fallback(d) if rw.conditions.is_empty() && rw.pattern.catch_all => Some((i, d.as_str())),
        _ => None,
    });
    for (i, rw) in plan.rewrites.iter().enumerate() {
        match &rw.to {
            RewriteTo::Path(templ) => {
                mws.insert(
                    format!("{pre}-rw{i}"),
                    json!({ "replacePathRegex": { "regex": rw.pattern.path, "replacement": templ } }),
                );
            }
            RewriteTo::External { origin, path } => {
                mws.insert(
                    format!("{pre}-rw{i}"),
                    json!({ "replacePathRegex": { "regex": rw.pattern.path, "replacement": path } }),
                );
                services.insert(
                    format!("{pre}-ext{i}"),
                    json!({ "loadBalancer": { "servers": [{ "url": origin }], "passHostHeader": false } }),
                );
            }
            RewriteTo::Fallback(_) => {}
        }
    }
    if !proxy.status_rewrites && plan.rewrites.iter().any(|r| matches!(r.to, RewriteTo::Fallback(_))) {
        notes.push(
            "the proxy is older than Traefik 3.5: rewrites to a fixed path answer with status 404 (pages still show)"
                .into(),
        );
    }

    let local = local_rule(&proxy.local);
    let block_rule = (!fw.block.is_empty()).then(|| visitor_in(&fw.block, &local, "firewall.blockIps", &mut notes));
    let trusted_rule = (!fw.trusted.is_empty()).then(|| {
        format!(
            "!({local} && !HeaderRegexp(`{CF_HEADER}`, `.+`)) && !{}",
            visitor_in(&fw.trusted, &local, "firewall.trustedIps", &mut notes)
        )
    });

    for (ri, r) in routers.iter().enumerate() {
        // Errors middlewares need a service: one per base router.
        let fallback = |i: usize, dest: &str, mws: &mut Map<String, Value>| -> String {
            let name = format!("{pre}-fb{i}-{ri}");
            let mut e = json!({ "status": ["404"], "service": r.service, "query": dest });
            if proxy.status_rewrites {
                e["statusRewrites"] = json!({ "404": 200 });
            }
            mws.insert(name.clone(), json!({ "errors": e }));
            name
        };
        sh.fallback = catch_all_fallback.map(|(i, d)| fallback(i, d, &mut mws));
        let mut add =
            |tag: String, extra: Vec<String>, priority: i64, middlewares: Vec<String>, service: Option<String>| {
                let mut rule = vec![r.rule.clone()];
                rule.extend(extra);
                let mut router = json!({
                    "rule": and(&rule),
                    "entryPoints": r.entry_points,
                    "service": service.unwrap_or_else(|| r.service.clone()),
                    "priority": priority,
                });
                let middlewares = dedup(middlewares);
                if !middlewares.is_empty() {
                    router["middlewares"] = json!(middlewares);
                }
                if let Some(t) = &r.tls {
                    router["tls"] = t.clone();
                }
                out_routers.insert(format!("{pre}-{}-{tag}", r.name), router);
            };

        // Firewall.
        if let Some(b) = &block_rule {
            add(
                "block".into(),
                vec![b.clone()],
                P_FIREWALL + 2,
                vec![deny.clone()],
                None,
            );
        }
        if let Some(t) = &trusted_rule {
            add(
                "trusted".into(),
                vec![t.clone()],
                P_FIREWALL + 1,
                vec![deny.clone()],
                None,
            );
        }
        for (i, f) in fw.rules.iter().enumerate() {
            let mut cond = Vec::new();
            if let Some(p) = f.path.as_ref().filter(|p| !p.catch_all) {
                cond.push(path_rule(p));
            }
            if let Some(ua) = &f.user_agent {
                cond.push(format!("HeaderRegexp(`User-Agent`, `{ua}`)"));
            }
            if !f.methods.is_empty() {
                cond.push(
                    f.methods
                        .iter()
                        .map(|m| format!("Method(`{m}`)"))
                        .collect::<Vec<_>>()
                        .join(" || "),
                );
            }
            match f.action {
                FirewallAction::Deny => add(
                    format!("fw{i}"),
                    cond,
                    P_FIREWALL - 1 - i as i64,
                    vec![deny.clone()],
                    None,
                ),
                FirewallAction::RateLimit { .. } if !f.global() => {
                    let mut m = sh.chain(f.path.as_ref(), r);
                    m.insert(0, rate_names[&i].clone());
                    m.extend(sh.fallback.clone());
                    add(format!("rl{i}"), cond, P_RATE - i as i64, m, None);
                }
                FirewallAction::RateLimit { .. } => {}
            }
        }
        // Redirects with conditions (the rest ride on every router).
        for (i, rd) in plan.redirects.iter().enumerate() {
            if rd.conditions.is_empty() {
                continue;
            }
            let mut cond = vec![];
            if !rd.pattern.catch_all {
                cond.push(path_rule(&rd.pattern));
            }
            cond.extend(rd.conditions.iter().cloned());
            let mut m = sh.chain(Some(&rd.pattern), r);
            m.extend(redirect_names[i].iter().cloned());
            add(format!("r{i}"), cond, P_REDIRECT - i as i64, m, None);
        }
        // Rewrites.
        for (i, rw) in plan.rewrites.iter().enumerate() {
            if catch_all_fallback.is_some_and(|(f, _)| f == i) {
                continue;
            }
            let mut cond = vec![];
            if !rw.pattern.catch_all {
                cond.push(path_rule(&rw.pattern));
            }
            cond.extend(rw.conditions.iter().cloned());
            let mut m = sh.chain(Some(&rw.pattern), r);
            let service = match &rw.to {
                RewriteTo::Path(_) => {
                    m.push(format!("{pre}-rw{i}"));
                    None
                }
                RewriteTo::External { .. } => {
                    m.push(format!("{pre}-rw{i}"));
                    Some(format!("{pre}-ext{i}"))
                }
                RewriteTo::Fallback(d) => {
                    m.push(fallback(i, d, &mut mws));
                    None
                }
            };
            add(format!("rw{i}"), cond, P_REWRITE - i as i64, m, service);
        }
        // Headers for some paths.
        for (i, h) in plan.headers.iter().enumerate() {
            if h.pattern.catch_all && h.conditions.is_empty() {
                continue;
            }
            let mut cond = vec![];
            if !h.pattern.catch_all {
                cond.push(path_rule(&h.pattern));
            }
            cond.extend(h.conditions.iter().cloned());
            let mut m = sh.chain(Some(&h.pattern), r);
            m.push(header_names[i].clone());
            m.extend(sh.fallback.clone());
            add(format!("h{i}"), cond, P_HEADER - i as i64, m, None);
        }
        // Everything else.
        let mut m = sh.chain(None, r);
        m.extend(sh.fallback.clone());
        add("base".into(), vec![], P_BASE, m, None);
    }

    let mut http = Map::new();
    http.insert("routers".into(), Value::Object(out_routers));
    http.insert("middlewares".into(), Value::Object(mws));
    if !services.is_empty() {
        http.insert("services".into(), Value::Object(services));
    }
    notes.dedup();
    (Some(json!({ "http": http })), notes)
}

/// The file's text. JSON is YAML, and serde escapes regexes correctly.
pub fn render(config: &Value) -> String {
    format!(
        "# Written by vercelify-agent from the app's vercel.json. Do not edit.\n{}\n",
        serde_json::to_string_pretty(config).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project;

    fn router() -> AppRouter {
        AppRouter {
            name: "http-0-abc".into(),
            rule: "Host(`a.test`) && PathPrefix(`/`)".into(),
            entry_points: vec!["http".into()],
            service: "http-0-abc@docker".into(),
            middlewares: vec!["gzip@docker".into()],
            tls: None,
        }
    }

    fn cfg(json_text: &str, proxy: Proxy) -> (Value, Vec<String>) {
        let plan = project::parse(&[("vercel.json", json_text)]).unwrap();
        let (c, notes) = config("abc", &[router()], &plan, &proxy);
        (c.expect("a configuration"), notes)
    }

    fn mw_of<'a>(c: &'a Value, router: &str) -> Vec<&'a str> {
        c["http"]["routers"][router]["middlewares"]
            .as_array()
            .unwrap_or_else(|| panic!("no router {router} in {c:#}"))
            .iter()
            .filter_map(Value::as_str)
            .collect()
    }

    #[test]
    fn nothing_to_route() {
        let plan =
            project::parse(&[("vercel.json", r#"{"crons": [{"path": "/c", "schedule": "* * * * *"}]}"#)]).unwrap();
        assert_eq!(config("abc", &[router()], &plan, &Proxy::default()).0, None);
        let plan = project::parse(&[("vercel.json", r#"{"cleanUrls": true}"#)]).unwrap();
        assert_eq!(config("abc", &[], &plan, &Proxy::default()).0, None);
    }

    #[test]
    fn spa_with_headers_and_redirects() {
        let (c, notes) = cfg(
            r#"{
              "redirects": [{"source": "/old/:slug", "destination": "/new/:slug"}],
              "rewrites": [{"source": "/(.*)", "destination": "/index.html"}],
              "headers": [
                {"source": "/(.*)", "headers": [{"key": "X-Frame-Options", "value": "DENY"}]},
                {"source": "/assets/(.*)", "headers": [{"key": "Cache-Control", "value": "immutable"}]}
              ]
            }"#,
            Proxy {
                status_rewrites: true,
                ..Proxy::default()
            },
        );
        assert!(notes.is_empty(), "{notes:?}");
        let routers = c["http"]["routers"].as_object().unwrap();
        // The catch-all fallback is a middleware, not a router.
        assert_eq!(
            routers.keys().collect::<Vec<_>>(),
            vec!["vfy-abc-http-0-abc-base", "vfy-abc-http-0-abc-h1"]
        );
        assert_eq!(
            mw_of(&c, "vfy-abc-http-0-abc-base"),
            vec!["vfy-abc-h0", "gzip@docker", "vfy-abc-r0", "vfy-abc-fb0-0"]
        );
        assert_eq!(
            mw_of(&c, "vfy-abc-http-0-abc-h1"),
            vec!["vfy-abc-h0", "vfy-abc-h1", "gzip@docker", "vfy-abc-r0", "vfy-abc-fb0-0"]
        );
        let h1 = &routers["vfy-abc-http-0-abc-h1"];
        assert_eq!(
            h1["rule"],
            "(Host(`a.test`) && PathPrefix(`/`)) && (PathRegexp(`(?i)^/assets/(?P<p1>.*)/?$`))"
        );
        assert_eq!(h1["priority"], P_HEADER - 1);
        assert_eq!(h1["service"], "http-0-abc@docker");
        let m = &c["http"]["middlewares"];
        assert_eq!(
            m["vfy-abc-fb0-0"]["errors"],
            json!({"status": ["404"], "service": "http-0-abc@docker", "query": "/index.html", "statusRewrites": {"404": 200}})
        );
        assert_eq!(m["vfy-abc-r0"]["redirectRegex"]["replacement"], "/new/${slug}${vfyq}");
        assert_eq!(m["vfy-abc-r0"]["redirectRegex"]["permanent"], true);
        // Valid as YAML text: it's JSON.
        assert!(render(&c).contains("\"vfy-abc-deny\""));
    }

    #[test]
    fn rewrites_get_routers_in_order() {
        let (c, notes) = cfg(
            r#"{"rewrites": [
                {"source": "/api/(.*)", "destination": "https://api.example.com/v1/$1"},
                {"source": "/blog/:slug", "destination": "/posts/:slug"},
                {"source": "/about", "destination": "/about-us.html"},
                {"source": "/(.*)", "destination": "/index.html", "has": [{"type": "cookie", "key": "beta"}]}
            ]}"#,
            Proxy::default(),
        );
        assert!(notes.iter().any(|n| n.contains("older than Traefik 3.5")), "{notes:?}");
        let r = &c["http"]["routers"];
        assert_eq!(r["vfy-abc-http-0-abc-rw0"]["service"], "vfy-abc-ext0");
        assert_eq!(r["vfy-abc-http-0-abc-rw0"]["priority"], P_REWRITE);
        assert_eq!(r["vfy-abc-http-0-abc-rw1"]["priority"], P_REWRITE - 1);
        assert_eq!(
            mw_of(&c, "vfy-abc-http-0-abc-rw2"),
            vec!["gzip@docker", "vfy-abc-fb2-0"]
        );
        // Conditional catch-all: a router with only the condition.
        assert_eq!(
            r["vfy-abc-http-0-abc-rw3"]["rule"],
            "(Host(`a.test`) && PathPrefix(`/`)) && (HeaderRegexp(`Cookie`, `(?:^|;\\s*)beta=(?:[^;]*)(?:;|$)`))"
        );
        let m = &c["http"]["middlewares"];
        assert!(m["vfy-abc-fb2-0"]["errors"].get("statusRewrites").is_none());
        assert_eq!(
            m["vfy-abc-rw1"]["replacePathRegex"],
            json!({"regex": "(?i)^/blog/(?P<slug>[^/#?]+?)/?$", "replacement": "/posts/${slug}"})
        );
        assert_eq!(
            c["http"]["services"]["vfy-abc-ext0"],
            json!({"loadBalancer": {"servers": [{"url": "https://api.example.com"}], "passHostHeader": false}})
        );
        assert_eq!(m["vfy-abc-rw0"]["replacePathRegex"]["replacement"], "/v1/${p1}");
    }

    #[test]
    fn redirect_with_a_query_and_conditions() {
        let (c, _) = cfg(
            r#"{"redirects": [
                {"source": "/a", "destination": "/b?x=1", "permanent": false},
                {"source": "/uk/(.*)", "destination": "https://uk.example.com/$1", "has": [{"type": "header", "key": "x-country", "value": "GB"}]}
            ]}"#,
            Proxy::default(),
        );
        let m = &c["http"]["middlewares"];
        assert_eq!(m["vfy-abc-r0q"]["redirectRegex"]["replacement"], "/b?x=1&${vfyq}");
        assert_eq!(m["vfy-abc-r0"]["redirectRegex"]["replacement"], "/b?x=1");
        assert_eq!(m["vfy-abc-r0"]["redirectRegex"]["permanent"], false);
        assert_eq!(
            mw_of(&c, "vfy-abc-http-0-abc-base"),
            vec!["gzip@docker", "vfy-abc-r0q", "vfy-abc-r0"]
        );
        let r1 = &c["http"]["routers"]["vfy-abc-http-0-abc-r1"];
        assert!(r1["rule"]
            .as_str()
            .unwrap()
            .contains("HeaderRegexp(`x-country`, `^(?:GB)$`)"));
        assert_eq!(
            mw_of(&c, "vfy-abc-http-0-abc-r1"),
            vec!["gzip@docker", "vfy-abc-r0q", "vfy-abc-r0", "vfy-abc-r1"]
        );
        assert_eq!(
            m["vfy-abc-r1"]["redirectRegex"]["replacement"],
            "https://uk.example.com/${p1}${vfyq}"
        );
    }

    #[test]
    fn firewall_routes() {
        let plan = project::parse(&[(
            "vercelify.json",
            r#"{
              "headers": [{"source": "/api/(.*)", "headers": [{"key": "X-Api", "value": "1"}]}],
              "rewrites": [{"source": "/api/v1/:rest*", "destination": "/v1/:rest*"}],
              "firewall": {
                "blockIps": ["203.0.113.0/24", "2001:db8::/36"],
                "trustedIps": ["198.51.100.7"],
                "rules": [
                  {"path": "/wp-admin/(.*)", "action": "deny"},
                  {"userAgent": "(?i)badbot", "methods": ["POST"], "action": "deny"},
                  {"path": "/api/(.*)", "action": "rate_limit", "requests": 60, "window": 60},
                  {"action": "rate_limit", "requests": 600, "window": 60}
                ]
              }
            }"#,
        )])
        .unwrap();
        let (c, notes) = config(
            "abc",
            &[router()],
            &plan,
            &Proxy {
                status_rewrites: true,
                cloudflare: true,
                local: local_nets(Some(&parse_subnets("10.0.1.0/24 fd00:c0::/64\n\n"))),
            },
        );
        let c = c.unwrap();
        assert!(notes.iter().any(|n| n.contains("2001:db8::/36")), "{notes:?}");
        let r = &c["http"]["routers"];
        let block = r["vfy-abc-http-0-abc-block"]["rule"].as_str().unwrap();
        assert!(
            block.contains("HeaderRegexp(`Cf-Connecting-Ip`, `^(?:203\\.0\\.113\\.\\d{1,3})$`)"),
            "{block}"
        );
        assert!(block.contains("ClientIP(`2001:db8::/36`)"), "{block}");
        // "Local" = loopback and the proxy's own Docker networks.
        assert!(
            block.contains("ClientIP(`10.0.1.0/24`)") && block.contains("ClientIP(`fd00:c0::/64`)"),
            "{block}"
        );
        assert!(!block.contains("172.16.0.0/12"), "{block}");
        let trusted = r["vfy-abc-http-0-abc-trusted"]["rule"].as_str().unwrap();
        assert!(trusted.contains("!(("), "{trusted}");
        assert_eq!(mw_of(&c, "vfy-abc-http-0-abc-fw0"), vec!["vfy-abc-deny"]);
        assert!(r["vfy-abc-http-0-abc-fw1"]["rule"]
            .as_str()
            .unwrap()
            .contains("(HeaderRegexp(`User-Agent`, `(?i)badbot`)) && (Method(`POST`))"));
        let m = &c["http"]["middlewares"];
        assert_eq!(
            m["vfy-abc-rl2"]["rateLimit"],
            json!({"average": 60, "period": "60s", "burst": 60, "sourceCriterion": {"requestHeaderName": "Cf-Connecting-Ip"}})
        );
        // The /api rate limit and header ride along on the /api/v1 rewrite.
        assert_eq!(
            mw_of(&c, "vfy-abc-http-0-abc-rw0"),
            vec!["vfy-abc-rl3", "vfy-abc-rl2", "vfy-abc-h0", "gzip@docker", "vfy-abc-rw0"]
        );
        assert_eq!(
            mw_of(&c, "vfy-abc-http-0-abc-rl2"),
            vec!["vfy-abc-rl2", "vfy-abc-rl3", "vfy-abc-h0", "gzip@docker"]
        );
        assert_eq!(mw_of(&c, "vfy-abc-http-0-abc-base"), vec!["vfy-abc-rl3", "gzip@docker"]);
        let p = |n: &str| r[n]["priority"].as_i64().unwrap();
        assert!(p("vfy-abc-http-0-abc-block") > p("vfy-abc-http-0-abc-fw1"));
        assert!(p("vfy-abc-http-0-abc-fw1") > p("vfy-abc-http-0-abc-rw0"));
        assert!(p("vfy-abc-http-0-abc-rw0") > p("vfy-abc-http-0-abc-rl2"));
        assert!(p("vfy-abc-http-0-abc-rl2") > p("vfy-abc-http-0-abc-h0"));
        assert!(p("vfy-abc-http-0-abc-h0") > p("vfy-abc-http-0-abc-base"));
    }

    #[test]
    fn tidy_urls_regexes() {
        use regex::Regex;
        let plan = project::parse(&[("vercel.json", r#"{"cleanUrls": true, "trailingSlash": false}"#)]).unwrap();
        let mws = tidy_middlewares("p", &plan);
        let apply = |url: &str| -> Option<String> {
            for (_, mw) in &mws {
                let re = Regex::new(mw["redirectRegex"]["regex"].as_str().unwrap()).unwrap();
                if re.is_match(url) {
                    // Go's ${name} expansion matches Rust's here.
                    return Some(
                        re.replace(url, mw["redirectRegex"]["replacement"].as_str().unwrap())
                            .into_owned(),
                    );
                }
            }
            None
        };
        assert_eq!(apply("https://a.test/index.html").as_deref(), Some("/"));
        assert_eq!(apply("https://a.test/index.html?x=1").as_deref(), Some("/?x=1"));
        assert_eq!(apply("https://a.test/blog/index.html").as_deref(), Some("/blog"));
        assert_eq!(apply("https://a.test/about.html?q").as_deref(), Some("/about?q"));
        assert_eq!(apply("https://a.test/about/").as_deref(), Some("/about"));
        assert_eq!(apply("https://a.test/"), None);
        assert_eq!(apply("https://a.test/about"), None);
        let plan = project::parse(&[("vercel.json", r#"{"cleanUrls": true, "trailingSlash": true}"#)]).unwrap();
        let mws2 = tidy_middlewares("p", &plan);
        let apply2 = |url: &str| -> Option<String> {
            for (_, mw) in &mws2 {
                let re = Regex::new(mw["redirectRegex"]["regex"].as_str().unwrap()).unwrap();
                if re.is_match(url) {
                    return Some(
                        re.replace(url, mw["redirectRegex"]["replacement"].as_str().unwrap())
                            .into_owned(),
                    );
                }
            }
            None
        };
        assert_eq!(apply2("http://a.test/about").as_deref(), Some("/about/"));
        assert_eq!(apply2("http://a.test/a/b?x").as_deref(), Some("/a/b/?x"));
        assert_eq!(apply2("http://a.test/about.html").as_deref(), Some("/about/"));
        assert_eq!(apply2("http://a.test/app.js"), None);
        assert_eq!(apply2("http://a.test/about/"), None);
        assert_eq!(apply2("http://a.test/"), None);
    }

    #[test]
    fn redirect_regexes_keep_the_query() {
        use regex::Regex;
        let plan = project::parse(&[(
            "vercel.json",
            r#"{"redirects": [{"source": "/old/(.*)", "destination": "/new/$1?src=old"}]}"#,
        )])
        .unwrap();
        let r = &plan.redirects[0];
        let mws = redirect_middlewares("p", &r.destination, r.dest_has_query, true, &r.pattern);
        let apply = |url: &str| -> Option<String> {
            mws.iter().find_map(|(_, mw)| {
                let re = Regex::new(mw["redirectRegex"]["regex"].as_str().unwrap()).unwrap();
                re.is_match(url).then(|| {
                    re.replace(url, mw["redirectRegex"]["replacement"].as_str().unwrap())
                        .into_owned()
                })
            })
        };
        assert_eq!(
            apply("https://a.test/OLD/x/y?z=1").as_deref(),
            Some("/new/x/y?src=old&z=1")
        );
        assert_eq!(apply("https://a.test/old/x").as_deref(), Some("/new/x?src=old"));
        assert_eq!(apply("https://a.test/older/x"), None);
    }
}

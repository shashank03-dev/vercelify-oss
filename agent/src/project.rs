//! Project configuration: the `vercel.json` a repository already has (and an
//! optional `vercelify.json` next to it), turned into a plan the proxy and the
//! cron runner can carry out.
//!
//! Supported from `vercel.json`: `redirects`, `rewrites` (to a path of the
//! app or to another site), `headers`, `cleanUrls`, `trailingSlash` and
//! `crons`, with `has`/`missing` conditions. `vercelify.json` uses the same
//! keys (its top-level keys win) and adds `firewall`. Everything else is
//! reported, not silently dropped, so the user knows what still needs doing.
//!
//! Sources use path-to-regexp syntax (`/blog/:slug`, `/docs/:path*`,
//! `/(.*)`) and are compiled to RE2 regexes, the dialect Traefik (Go) runs.
//! Like on Vercel they match case-insensitively unless `caseSensitive` is set,
//! and a trailing slash is optional.

use serde_json::{Map, Value};
use std::net::IpAddr;

/// Files read from the app's root directory; later ones override earlier ones.
pub const FILES: [&str; 2] = ["vercel.json", "vercelify.json"];
/// Rules of all kinds together: each becomes routes in the proxy, so a typo
/// generating thousands of them must not slow a small machine down.
pub const MAX_RULES: usize = 200;
const MAX_CRONS: usize = 40;

/// Keys Vercel knows that a container platform handles another way.
const ELSEWHERE: &[(&str, &str)] = &[
    ("devCommand", "only used by `vercel dev`"),
    (
        "functions",
        "your app runs as one container, with no per-function memory or duration limits",
    ),
    ("regions", "apps run on your own server"),
    ("functionFailoverRegions", "apps run on your own server"),
    (
        "images",
        "image optimisation runs in your framework's server (e.g. next start)",
    ),
    ("github", "auto-deploy settings live in the app's Git panel"),
    ("git", "auto-deploy settings live in the app's Git panel"),
    ("public", "the source of a deployment is never published"),
    ("build", "set build-time variables in the app's Environment panel"),
    ("env", "set variables in the app's Environment panel"),
    (
        "ignoreCommand",
        "auto-deploy skips commits that don't touch the app's watch paths",
    ),
    ("bunVersion", "set the runtime in your Dockerfile or nixpacks.toml"),
    ("fluid", "your app runs as one container"),
];
const IGNORED_QUIETLY: &[&str] = &["$schema", "version", "name", "scope", "alias"];

/// Read by the build, not by the proxy (`vercelify_platform::detect`).
const BUILD_KEYS: &[&str] = &["buildCommand", "installCommand", "outputDirectory", "framework"];

/// A compiled `source` pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    /// The original text, for messages.
    pub source: String,
    /// Anchored regex for the request path.
    pub path: String,
    /// The same body unanchored, unable to run into the query string, for
    /// regexes over the whole URL (redirects).
    pub url_part: String,
    pub case_insensitive: bool,
    /// Named parameters (`:slug`, and `p1`, `p2`… for unnamed groups).
    pub names: Vec<String>,
    /// Names given to unnamed groups, in order (`$1` is the first).
    pub unnamed: Vec<String>,
    /// Matches every path.
    pub catch_all: bool,
    /// Literal text every matching path starts with (lowercase when case
    /// insensitive), and whether the pattern matches every path that starts
    /// with it (`/api/(.*)`, `/docs/:path*`).
    pub prefix: String,
    pub prefix_catch_all: bool,
}

impl Pattern {
    /// Every path `other` matches, this pattern matches too (a sound but
    /// incomplete test: only literal-prefix patterns can cover).
    pub fn covers(&self, other: &Pattern) -> bool {
        if self.catch_all {
            return true;
        }
        if !self.prefix_catch_all {
            return false;
        }
        let (a, b) = if self.case_insensitive {
            (self.prefix.to_lowercase(), other.prefix.to_lowercase())
        } else if other.case_insensitive {
            // Other matches mixed case this one doesn't.
            return false;
        } else {
            (self.prefix.clone(), other.prefix.clone())
        };
        b.starts_with(&a)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Redirect {
    pub pattern: Pattern,
    /// Traefik rule fragments (from `has`/`missing`), all must hold.
    pub conditions: Vec<String>,
    /// Go replacement template for the path and query.
    pub destination: String,
    pub dest_has_query: bool,
    pub permanent: bool,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RewriteTo {
    /// Rewrite the path before the app sees it (destinations with parameters).
    Path(String),
    /// Serve this path when the app has nothing at the requested one (404),
    /// like Vercel, which applies rewrites after its file system.
    Fallback(String),
    /// Proxy to another site.
    External { origin: String, path: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rewrite {
    pub pattern: Pattern,
    pub conditions: Vec<String>,
    pub to: RewriteTo,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeaderRule {
    pub pattern: Pattern,
    pub conditions: Vec<String>,
    pub headers: Vec<(String, String)>,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    pub path: String,
    pub schedule: String,
}

/// An IP address or network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Net {
    pub addr: IpAddr,
    pub prefix: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FirewallAction {
    Deny,
    RateLimit { requests: u32, window_secs: u32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FirewallRule {
    pub label: String,
    pub path: Option<Pattern>,
    /// Regex over the User-Agent header.
    pub user_agent: Option<String>,
    pub methods: Vec<String>,
    pub action: FirewallAction,
}

impl FirewallRule {
    /// No condition at all: applies to every request.
    pub fn global(&self) -> bool {
        self.path.as_ref().is_none_or(|p| p.catch_all) && self.user_agent.is_none() && self.methods.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Firewall {
    pub block: Vec<Net>,
    /// When set, only these may visit (Vercel's "Trusted IPs").
    pub trusted: Vec<Net>,
    pub rules: Vec<FirewallRule>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub redirects: Vec<Redirect>,
    pub rewrites: Vec<Rewrite>,
    pub headers: Vec<HeaderRule>,
    pub clean_urls: bool,
    pub trailing_slash: Option<bool>,
    pub crons: Vec<Cron>,
    pub firewall: Firewall,
    /// Rules skipped or approximated, and keys handled elsewhere.
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn rule_count(&self) -> usize {
        self.redirects.len()
            + self.rewrites.len()
            + self.headers.len()
            + self.firewall.rules.len()
            + usize::from(!self.firewall.block.is_empty())
            + usize::from(!self.firewall.trusted.is_empty())
            + usize::from(self.clean_urls)
            + usize::from(self.trailing_slash.is_some())
    }

    /// Anything for the proxy to do.
    pub fn routes_anything(&self) -> bool {
        self.rule_count() > 0
    }
}

// --- path-to-regexp ---------------------------------------------------------

/// Go accepts it: RE2 syntax, no look-around or back-references.
pub fn check_regex(re: &str) -> Result<(), String> {
    regex_syntax::Parser::new().parse(re).map(|_| ()).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("look-around") {
            format!("`{re}`: look-ahead/look-behind isn't supported by the proxy (Go regexes)")
        } else {
            format!(
                "`{re}` is not a valid regex: {}",
                msg.lines().last().unwrap_or("").trim()
            )
        }
    })
}

/// `.` and `[^…]` in a user regex, made unable to cross into `?query`.
fn confine(re: &str) -> String {
    let mut out = String::new();
    let mut chars = re.chars().peekable();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push(c);
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '[' if !in_class => {
                in_class = true;
                out.push('[');
                if chars.peek() == Some(&'^') {
                    chars.next();
                    out.push_str("^?#");
                }
            }
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            '.' if !in_class => out.push_str("[^?#]"),
            _ => out.push(c),
        }
    }
    out
}

/// Read a `(…)` group body starting after `(`; nested parentheses allowed.
fn group_body(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<String, String> {
    let mut depth = 1;
    let mut body = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                body.push(c);
                body.push(chars.next().ok_or("ends with a backslash")?);
            }
            '(' => {
                // path-to-regexp forbids capturing groups inside a pattern.
                if chars.peek() != Some(&'?') {
                    return Err("capturing groups inside a pattern aren't allowed; use (?:…)".into());
                }
                depth += 1;
                body.push(c);
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return if body.is_empty() {
                        Err("empty group ()".into())
                    } else {
                        Ok(body)
                    };
                }
                body.push(c);
            }
            _ => body.push(c),
        }
    }
    Err("unclosed (".into())
}

const SEGMENT: &str = "[^/#?]+?";

/// Compile a path-to-regexp `source`.
pub fn compile(source: &str, case_sensitive: bool) -> Result<Pattern, String> {
    if !source.starts_with('/') && !source.starts_with('(') {
        return Err(format!("`{source}` must start with /"));
    }
    if source.len() > 512 {
        return Err("source is longer than 512 characters".into());
    }
    if source.contains('`') {
        return Err(format!("`{source}`: backticks aren't allowed"));
    }
    // (regex body, confined body)
    let mut body: Vec<(String, String)> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut unnamed: Vec<String> = Vec::new();
    let mut prefix = String::new();
    let mut literal = true;
    // After the literal prefix: only one catch-all group, then nothing.
    let mut tail_catch_all = false;
    let mut after_tail = false;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        let (name, pattern) = match c {
            ':' => {
                let mut name = String::new();
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_alphanumeric() || n == '_' {
                        name.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
                    return Err(format!("`{source}`: a parameter needs a name (`:slug`)"));
                }
                let pattern = if chars.peek() == Some(&'(') {
                    chars.next();
                    Some(group_body(&mut chars).map_err(|e| format!("`{source}`: {e}"))?)
                } else {
                    None
                };
                (name, pattern)
            }
            '(' => {
                if chars.peek() == Some(&'?') {
                    return Err(format!("`{source}`: groups must capture (`(.*)`)"));
                }
                let pattern = group_body(&mut chars).map_err(|e| format!("`{source}`: {e}"))?;
                let name = format!("p{}", unnamed.len() + 1);
                unnamed.push(name.clone());
                (name, Some(pattern))
            }
            '*' => return Err(format!("`{source}`: use `(.*)` or `:path*` instead of a bare *")),
            '{' | '}' => return Err(format!("`{source}`: {{…}} groups aren't supported")),
            '\\' => {
                let n = chars
                    .next()
                    .ok_or_else(|| format!("`{source}` ends with a backslash"))?;
                if after_tail {
                    tail_catch_all = false;
                }
                if literal {
                    prefix.push(n);
                }
                let e = regex_syntax::escape(&n.to_string());
                body.push((e.clone(), e));
                continue;
            }
            _ => {
                if after_tail {
                    tail_catch_all = false;
                }
                if literal {
                    prefix.push(c);
                }
                let e = regex_syntax::escape(&c.to_string());
                body.push((e.clone(), e));
                continue;
            }
        };
        if names.contains(&name) {
            return Err(format!("`{source}`: parameter `{name}` is used twice"));
        }
        if name.starts_with("vfy") {
            return Err(format!("`{source}`: parameter names starting with vfy are reserved"));
        }
        if let Some(p) = &pattern {
            check_regex(p).map_err(|e| format!("`{source}`: {e}"))?;
        }
        let modifier = match chars.peek() {
            Some(&m @ ('?' | '*' | '+')) => {
                chars.next();
                Some(m)
            }
            _ => None,
        };
        let pat = pattern.clone().unwrap_or_else(|| SEGMENT.to_string());
        let cpat = pattern.as_deref().map_or_else(|| SEGMENT.to_string(), confine);
        // A modifier makes the preceding `/` part of the optional/repeated piece.
        let slash = modifier.is_some() && body.last().is_some_and(|(r, _)| r == "/");
        // (The `/` stays in `prefix`: `/docs/:path*` doesn't match `/docsx`.)
        if slash {
            body.pop();
        }
        let sep = if slash { "/" } else { "" };
        let (re, cre) = match modifier {
            None => (format!("(?P<{name}>{pat})"), format!("(?P<{name}>{cpat})")),
            Some('?') => (
                format!("(?:{sep}(?P<{name}>{pat}))?"),
                format!("(?:{sep}(?P<{name}>{cpat}))?"),
            ),
            Some(m) => {
                let rep = format!("(?:{pat})(?:/(?:{pat}))*");
                let crep = format!("(?:{cpat})(?:/(?:{cpat}))*");
                let opt = if m == '*' { "?" } else { "" };
                (
                    format!("(?:{sep}(?P<{name}>{rep})){opt}"),
                    format!("(?:{sep}(?P<{name}>{crep})){opt}"),
                )
            }
        };
        // Matches whatever follows: `(.*)`, `:path*`.
        let anything = pattern.as_deref() == Some(".*") || (pattern.is_none() && modifier == Some('*'));
        if literal {
            literal = false;
            tail_catch_all = anything;
            after_tail = true;
        } else {
            tail_catch_all = false;
        }
        names.push(name);
        body.push((re, cre));
    }
    let re: String = body.iter().map(|(r, _)| r.as_str()).collect();
    let cre: String = body.iter().map(|(_, c)| c.as_str()).collect();
    let flags = if case_sensitive { "" } else { "(?i)" };
    // Non-strict like path-to-regexp: an optional trailing slash.
    let path = if re.ends_with('/') {
        format!("{flags}^{re}$")
    } else {
        format!("{flags}^{re}/?$")
    };
    check_regex(&path)?;
    let url_part = if cre.ends_with('/') { cre } else { format!("{cre}/?") };
    let prefix_catch_all = !literal && tail_catch_all;
    let catch_all = prefix_catch_all && (prefix.is_empty() || prefix == "/");
    Ok(Pattern {
        source: source.to_string(),
        path,
        url_part,
        case_insensitive: !case_sensitive,
        names,
        unnamed,
        catch_all,
        prefix: if case_sensitive { prefix } else { prefix.to_lowercase() },
        prefix_catch_all,
    })
}

/// A destination as a Go replacement template: `:name` and `$1` become
/// `${name}`, other `$` are escaped.
pub fn destination(dest: &str, p: &Pattern) -> Result<String, String> {
    if dest.contains('`') || dest.chars().any(char::is_control) {
        return Err(format!("`{dest}`: invalid characters in destination"));
    }
    // In `https://host:8443/…` the colons before the path are not parameters.
    let path_start = dest
        .find("://")
        .map_or(0, |i| dest[i + 3..].find('/').map_or(dest.len(), |j| i + 3 + j));
    let bytes = dest.as_bytes();
    let word = |from: usize| {
        let len = dest[from..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(dest.len() - from);
        &dest[from..from + len]
    };
    let mut out = String::new();
    let mut i = 0;
    while i < dest.len() {
        let c = dest[i..].chars().next().unwrap_or(' ');
        if c == ':' && i >= path_start && bytes.get(i + 1).is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_') {
            let name = word(i + 1);
            if !p.names.iter().any(|n| n == name) {
                return Err(format!("`{dest}`: `:{name}` is not a parameter of `{}`", p.source));
            }
            out.push_str(&format!("${{{name}}}"));
            i += 1 + name.len();
            // `:path*` / `:path+` (a `?` after a name starts the query).
            if matches!(bytes.get(i), Some(b'*' | b'+')) {
                i += 1;
            }
            continue;
        }
        if c == '$' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
            let n = word(i + 1);
            let idx: usize = n.parse().unwrap_or(0);
            let name = idx
                .checked_sub(1)
                .and_then(|k| p.unnamed.get(k))
                .ok_or_else(|| format!("`{dest}`: `${n}` has no matching group in `{}`", p.source))?;
            out.push_str(&format!("${{{name}}}"));
            i += 1 + n.len();
            continue;
        }
        if c == '$' {
            out.push_str("$$");
        } else {
            out.push(c);
        }
        i += c.len_utf8();
    }
    Ok(out)
}

/// `(origin, path-and-query)` of an absolute http(s) URL.
fn split_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let scheme_len = url.len() - rest.len();
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c)) {
        return None;
    }
    let path = &rest[end..];
    let path = if path.starts_with('?') || path.is_empty() {
        format!("/{path}")
    } else {
        path.to_string()
    };
    Some((url[..scheme_len + end].to_string(), path))
}

// --- conditions -------------------------------------------------------------

fn tick_safe(s: &str) -> Result<&str, String> {
    if s.contains('`') || s.chars().any(char::is_control) {
        Err(format!("`{s}`: invalid characters"))
    } else {
        Ok(s)
    }
}

/// One `has`/`missing` entry as a Traefik rule fragment.
fn condition(c: &Value, negate: bool) -> Result<String, String> {
    let kind = c
        .get("type")
        .and_then(Value::as_str)
        .ok_or("a condition needs a type")?;
    let key = c.get("key").and_then(Value::as_str);
    let value = match c.get("value") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            check_regex(s)?;
            Some(tick_safe(s)?)
        }
        Some(_) => return Err("only text values are supported in conditions".into()),
    };
    let whole = |v: Option<&str>| v.map_or_else(|| "^.*$".to_string(), |v| format!("^(?:{v})$"));
    let rule = match kind {
        "header" => {
            let key = tick_safe(key.ok_or("a header condition needs a key")?)?;
            if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err(format!("`{key}` is not a header name"));
            }
            format!("HeaderRegexp(`{key}`, `{}`)", whole(value))
        }
        "cookie" => {
            let key = tick_safe(key.ok_or("a cookie condition needs a key")?)?;
            format!(
                "HeaderRegexp(`Cookie`, `(?:^|;\\s*){}=(?:{})(?:;|$)`)",
                regex_syntax::escape(key),
                value.unwrap_or("[^;]*")
            )
        }
        "query" => {
            let key = tick_safe(key.ok_or("a query condition needs a key")?)?;
            format!("QueryRegexp(`{key}`, `{}`)", whole(value))
        }
        "host" => format!("HostRegexp(`{}`)", whole(value)),
        other => return Err(format!("`{other}` conditions aren't supported")),
    };
    Ok(if negate { format!("!{rule}") } else { rule })
}

fn conditions(rule: &Value) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for (field, negate) in [("has", false), ("missing", true)] {
        match rule.get(field) {
            None | Some(Value::Null) => {}
            Some(Value::Array(list)) => {
                for c in list {
                    out.push(condition(c, negate)?);
                }
            }
            Some(_) => return Err(format!("`{field}` must be a list")),
        }
    }
    Ok(out)
}

// --- IPs --------------------------------------------------------------------

pub fn parse_net(s: &str) -> Result<Net, String> {
    let (ip, prefix) = match s.trim().split_once('/') {
        Some((ip, p)) => (ip, Some(p)),
        None => (s.trim(), None),
    };
    let addr: IpAddr = ip.parse().map_err(|_| format!("`{s}` is not an IP address or range"))?;
    let max = if addr.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        Some(p) => p
            .parse::<u8>()
            .ok()
            .filter(|p| *p <= max)
            .ok_or_else(|| format!("`{s}`: bad prefix length"))?,
        None => max,
    };
    // Clear host bits so the range reads the way it matches.
    let addr = match addr {
        IpAddr::V4(a) => {
            let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
            IpAddr::V4((u32::from(a) & mask).into())
        }
        IpAddr::V6(a) => {
            let mask = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
            IpAddr::V6((u128::from(a) & mask).into())
        }
    };
    Ok(Net { addr, prefix })
}

impl std::fmt::Display for Net {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

fn numbers(lo: u32, hi: u32) -> String {
    if lo == 0 && hi == 255 {
        return "\\d{1,3}".into();
    }
    if lo == hi {
        return lo.to_string();
    }
    format!("(?:{})", (lo..=hi).map(|n| n.to_string()).collect::<Vec<_>>().join("|"))
}

/// A regex matching the text form of every address in `net` (as Cloudflare
/// sends it in `Cf-Connecting-Ip`). None: can't be expressed (IPv6 ranges
/// that aren't on a group boundary or contain a zero group).
pub fn net_regex(net: &Net) -> Option<String> {
    match net.addr {
        IpAddr::V4(a) => {
            let o = a.octets();
            let full = usize::from(net.prefix / 8);
            let bits = net.prefix % 8;
            let mut parts: Vec<String> = o[..full].iter().map(u8::to_string).collect();
            if full < 4 {
                let lo = u32::from(o[full]);
                let span = if bits == 0 { 256 } else { 1u32 << (8 - bits) };
                parts.push(numbers(lo, lo + span - 1));
                parts.extend(std::iter::repeat_n("\\d{1,3}".to_string(), 3 - full));
            }
            Some(parts.join("\\."))
        }
        IpAddr::V6(a) => {
            if net.prefix == 128 {
                return Some(regex_syntax::escape(&a.to_string()));
            }
            if !net.prefix.is_multiple_of(16) {
                return None;
            }
            let groups = a.segments();
            let n = usize::from(net.prefix / 16);
            if n == 0 {
                return Some("[0-9a-f:.]+".into());
            }
            if groups[..n].contains(&0) {
                return None;
            }
            let head: Vec<String> = groups[..n].iter().map(|g| format!("{g:x}")).collect();
            Some(format!("{}:[0-9a-f:.]*", head.join(":")))
        }
    }
}

// --- parsing ----------------------------------------------------------------

fn list<'a>(root: &'a Map<String, Value>, key: &str, warnings: &mut Vec<String>) -> Vec<&'a Value> {
    match root.get(key) {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a.iter().collect(),
        Some(_) => {
            warnings.push(format!("`{key}` must be a list; ignored"));
            vec![]
        }
    }
}

fn source_of(rule: &Value) -> Result<Pattern, String> {
    let source = rule.get("source").and_then(Value::as_str).ok_or("needs a `source`")?;
    let case_sensitive = rule.get("caseSensitive").and_then(Value::as_bool).unwrap_or(false);
    compile(source, case_sensitive)
}

fn redirect(rule: &Value, label: &str) -> Result<Redirect, String> {
    let pattern = source_of(rule)?;
    let conditions = conditions(rule)?;
    let dest = rule
        .get("destination")
        .and_then(Value::as_str)
        .ok_or("needs a `destination`")?;
    if !dest.starts_with('/') && split_url(dest).is_none() {
        return Err(format!("`{dest}`: destination must be a path or an http(s) URL"));
    }
    let permanent = match (rule.get("statusCode").and_then(Value::as_u64), rule.get("permanent")) {
        (Some(301 | 308), _) => true,
        (Some(302 | 303 | 307), _) => false,
        (Some(c), _) => return Err(format!("status code {c} is not a redirect")),
        (None, Some(Value::Bool(b))) => *b,
        // Vercel's default is permanent (308).
        (None, _) => true,
    };
    let destination = destination(dest, &pattern)?;
    Ok(Redirect {
        dest_has_query: dest.contains('?'),
        pattern,
        conditions,
        destination,
        permanent,
        label: label.into(),
    })
}

fn rewrite(rule: &Value, label: &str, warnings: &mut Vec<String>) -> Result<Rewrite, String> {
    let pattern = source_of(rule)?;
    let conditions = conditions(rule)?;
    let dest = match rule.get("destination") {
        Some(Value::String(s)) => s.as_str(),
        Some(Value::Object(_)) => {
            return Err("rewrites to `services` aren't supported; give each service its own app".into())
        }
        _ => return Err("needs a `destination`".into()),
    };
    let to = if let Some((origin, path)) = split_url(dest) {
        let templ = destination(&path, &pattern)?;
        let (path, query) = templ.split_once('?').unwrap_or((templ.as_str(), ""));
        if !query.is_empty() {
            warnings.push(format!(
                "{label}: the query in `{dest}` is dropped (the proxy rewrites paths only)"
            ));
        }
        RewriteTo::External {
            origin,
            path: path.to_string(),
        }
    } else if dest.starts_with('/') {
        let templ = destination(dest, &pattern)?;
        if templ.contains("${") {
            let (path, query) = templ.split_once('?').unwrap_or((templ.as_str(), ""));
            if !query.is_empty() {
                warnings.push(format!(
                    "{label}: the query in `{dest}` is dropped (the proxy rewrites paths only)"
                ));
            }
            RewriteTo::Path(path.to_string())
        } else {
            RewriteTo::Fallback(dest.to_string())
        }
    } else {
        return Err(format!("`{dest}`: destination must be a path or an http(s) URL"));
    };
    Ok(Rewrite {
        pattern,
        conditions,
        to,
        label: label.into(),
    })
}

fn header_rule(rule: &Value, label: &str) -> Result<HeaderRule, String> {
    let pattern = source_of(rule)?;
    let conditions = conditions(rule)?;
    let mut headers = Vec::new();
    for h in rule
        .get("headers")
        .and_then(Value::as_array)
        .ok_or("needs a `headers` list")?
    {
        let key = h.get("key").and_then(Value::as_str).ok_or("each header needs a key")?;
        let value = h
            .get("value")
            .and_then(Value::as_str)
            .ok_or("each header needs a value")?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?={}".contains(&b))
        {
            return Err(format!("`{key}` is not a header name"));
        }
        if value.chars().any(|c| c.is_control() && c != '\t') {
            return Err(format!("header `{key}`: line breaks aren't allowed"));
        }
        headers.push((key.to_string(), value.to_string()));
    }
    if headers.is_empty() {
        return Err("no headers".into());
    }
    Ok(HeaderRule {
        pattern,
        conditions,
        headers,
        label: label.into(),
    })
}

fn nets(v: Option<&Value>, what: &str, warnings: &mut Vec<String>) -> Vec<Net> {
    let mut out = Vec::new();
    for s in v.and_then(Value::as_array).into_iter().flatten() {
        match s
            .as_str()
            .ok_or_else(|| format!("{what}: not text"))
            .and_then(parse_net)
        {
            Ok(n) => out.push(n),
            Err(e) => warnings.push(format!("{what}: {e}; skipped")),
        }
    }
    out
}

fn firewall(v: &Value, warnings: &mut Vec<String>) -> Firewall {
    let mut fw = Firewall {
        block: nets(v.get("blockIps"), "firewall.blockIps", warnings),
        trusted: nets(v.get("trustedIps"), "firewall.trustedIps", warnings),
        rules: vec![],
    };
    for (i, r) in v
        .get("rules")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let label = match r.get("name").and_then(Value::as_str) {
            Some(n) => format!("firewall rule \"{}\"", n.chars().take(60).collect::<String>()),
            None => format!("firewall.rules[{i}]"),
        };
        let rule = (|| -> Result<FirewallRule, String> {
            let path = match r.get("path").and_then(Value::as_str) {
                Some(p) => Some(compile(
                    p,
                    r.get("caseSensitive").and_then(Value::as_bool).unwrap_or(false),
                )?),
                None => None,
            };
            let user_agent = match r.get("userAgent").and_then(Value::as_str) {
                Some(ua) => {
                    check_regex(ua)?;
                    Some(tick_safe(ua)?.to_string())
                }
                None => None,
            };
            let mut methods = Vec::new();
            for m in r.get("methods").and_then(Value::as_array).into_iter().flatten() {
                let m = m.as_str().unwrap_or("").to_ascii_uppercase();
                if m.is_empty() || !m.chars().all(|c| c.is_ascii_uppercase()) {
                    return Err(format!("`{m}` is not an HTTP method"));
                }
                methods.push(m);
            }
            let action = match r.get("action").and_then(Value::as_str) {
                Some("deny") => FirewallAction::Deny,
                Some("rate_limit") => {
                    let requests = r.get("requests").and_then(Value::as_u64).unwrap_or(0);
                    let window = r.get("window").and_then(Value::as_u64).unwrap_or(60);
                    if !(1..=1_000_000).contains(&requests) {
                        return Err("`requests` must be 1 to 1000000".into());
                    }
                    if !(1..=3600).contains(&window) {
                        return Err("`window` must be 1 to 3600 seconds".into());
                    }
                    FirewallAction::RateLimit {
                        requests: requests as u32,
                        window_secs: window as u32,
                    }
                }
                other => return Err(format!("action must be deny or rate_limit (got {other:?})")),
            };
            let rule = FirewallRule {
                label: label.clone(),
                path,
                user_agent,
                methods,
                action,
            };
            if rule.global() && rule.action == FirewallAction::Deny {
                return Err("a deny rule without path, userAgent or methods would block every visitor".into());
            }
            Ok(rule)
        })();
        match rule {
            Ok(rule) => fw.rules.push(rule),
            Err(e) => warnings.push(format!("{label}: {e}; skipped")),
        }
    }
    fw
}

/// An origin on this machine or a private network (an IP literal, `localhost`,
/// a Docker container name, `.internal`/`.local`): proxying to it would put
/// something meant to stay private (Ollama, the agent, a database) on the
/// app's public domain.
pub fn private_origin(origin: &str) -> bool {
    let rest = origin.split("://").nth(1).unwrap_or(origin);
    let host = if let Some(v6) = rest.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        rest.split(':').next().unwrap_or("")
    }
    .to_ascii_lowercase();
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(a) => {
                let o = a.octets();
                a.is_loopback()
                    || a.is_private()
                    || a.is_link_local()
                    || a.is_unspecified()
                    || a.is_broadcast()
                    || (o[0] == 100 && (64..128).contains(&o[1]))
            }
            IpAddr::V6(a) => {
                let s = a.segments();
                a.is_loopback()
                    || a.is_unspecified()
                    || (s[0] & 0xfe00) == 0xfc00
                    || (s[0] & 0xffc0) == 0xfe80
                    || a.to_ipv4_mapped()
                        .is_some_and(|v4| private_origin(&format!("http://{v4}")))
            }
        };
    }
    !host.contains('.')
        || [".localhost", ".local", ".internal", ".lan", ".home.arpa"]
            .iter()
            .any(|s| host.ends_with(s))
}

/// Drop rewrites that proxy to a private origin (see `private_origin`).
pub fn drop_private_rewrites(plan: &mut Plan) {
    let mut notes = Vec::new();
    plan.rewrites.retain(|r| match &r.to {
        RewriteTo::External { origin, .. } if private_origin(origin) => {
            notes.push(format!(
                "{}: {origin} is on this machine or a private network; proxying to it would make it public — skipped",
                r.label
            ));
            false
        }
        _ => true,
    });
    plan.warnings.extend(notes);
}

/// Merge the files (later keys win) into one configuration object.
pub fn merge(files: &[(&str, &str)]) -> Result<Map<String, Value>, String> {
    let mut root = Map::new();
    for (name, text) in files {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("{name}: {e}"))?;
        let Value::Object(m) = v else {
            return Err(format!("{name}: must be a JSON object"));
        };
        root.extend(m);
    }
    Ok(root)
}

/// The plan for a merged configuration.
pub fn plan(root: &Map<String, Value>) -> Plan {
    let mut p = Plan::default();
    let mut w: Vec<String> = Vec::new();
    for (key, _) in root {
        if let Some((_, why)) = ELSEWHERE.iter().find(|(k, _)| k == key) {
            w.push(format!("`{key}` is not used here: {why}"));
        } else if key == "routes" {
            w.push("legacy `routes` aren't supported; use redirects, rewrites and headers".into());
        } else if key == "builds" {
            w.push("`builds` is not used here: the whole app is built as one container".into());
        } else if ![
            "redirects",
            "rewrites",
            "headers",
            "cleanUrls",
            "trailingSlash",
            "crons",
            "firewall",
        ]
        .contains(&key.as_str())
            && !IGNORED_QUIETLY.contains(&key.as_str())
            && !BUILD_KEYS.contains(&key.as_str())
        {
            w.push(format!("`{key}` is not supported and was ignored"));
        }
    }
    for (i, r) in list(root, "redirects", &mut w).into_iter().enumerate() {
        let label = format!("redirects[{i}]");
        match redirect(r, &label) {
            Ok(x) => p.redirects.push(x),
            Err(e) => w.push(format!("{label}: {e}; skipped")),
        }
    }
    for (i, r) in list(root, "rewrites", &mut w).into_iter().enumerate() {
        let label = format!("rewrites[{i}]");
        match rewrite(r, &label, &mut w) {
            Ok(x) => p.rewrites.push(x),
            Err(e) => w.push(format!("{label}: {e}; skipped")),
        }
    }
    for (i, r) in list(root, "headers", &mut w).into_iter().enumerate() {
        let label = format!("headers[{i}]");
        match header_rule(r, &label) {
            Ok(x) => p.headers.push(x),
            Err(e) => w.push(format!("{label}: {e}; skipped")),
        }
    }
    p.clean_urls = root.get("cleanUrls").and_then(Value::as_bool).unwrap_or(false);
    if p.clean_urls {
        w.push("cleanUrls: `.html` addresses redirect to clean ones; your server must answer the clean address (most static servers do)".into());
    }
    p.trailing_slash = root.get("trailingSlash").and_then(Value::as_bool);
    for (i, c) in list(root, "crons", &mut w).into_iter().enumerate() {
        let label = format!("crons[{i}]");
        let path = c.get("path").and_then(Value::as_str).unwrap_or("");
        let schedule = c.get("schedule").and_then(Value::as_str).unwrap_or("");
        if !path.starts_with('/') || path.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
            w.push(format!("{label}: path must start with / ; skipped"));
            continue;
        }
        if let Err(e) = crate::cron::parse(schedule) {
            w.push(format!("{label}: {e}; skipped"));
            continue;
        }
        if p.crons.len() >= MAX_CRONS {
            w.push(format!("{label}: more than {MAX_CRONS} cron jobs; skipped"));
            continue;
        }
        p.crons.push(Cron {
            path: path.into(),
            schedule: schedule.split_whitespace().collect::<Vec<_>>().join(" "),
        });
    }
    if let Some(fw) = root.get("firewall") {
        p.firewall = firewall(fw, &mut w);
        if !p.firewall.trusted.is_empty() {
            w.push("firewall.trustedIps: everyone else gets 403, including you unless your IP is listed".into());
        }
    }
    // Keep the proxy configuration small on weak machines.
    let mut budget = MAX_RULES;
    let mut cap = |n: usize, what: &str, w: &mut Vec<String>| -> usize {
        let keep = n.min(budget);
        budget -= keep;
        if keep < n {
            w.push(format!(
                "{} {what} over the limit of {MAX_RULES} rules were skipped",
                n - keep
            ));
        }
        keep
    };
    let n = cap(p.redirects.len(), "redirects", &mut w);
    p.redirects.truncate(n);
    let n = cap(p.rewrites.len(), "rewrites", &mut w);
    p.rewrites.truncate(n);
    let n = cap(p.headers.len(), "header rules", &mut w);
    p.headers.truncate(n);
    let n = cap(p.firewall.rules.len(), "firewall rules", &mut w);
    p.firewall.rules.truncate(n);
    p.warnings = w;
    p
}

/// Parse the files of a repository (name, text), later ones winning.
pub fn parse(files: &[(&str, &str)]) -> Result<Plan, String> {
    merge(files).map(|root| plan(&root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    /// Go and Rust share RE2 syntax for everything generated here.
    fn m(p: &Pattern, path: &str) -> Option<Vec<(String, String)>> {
        let re = Regex::new(&p.path).unwrap();
        let c = re.captures(path)?;
        Some(
            p.names
                .iter()
                .map(|n| (n.clone(), c.name(n).map_or("", |x| x.as_str()).to_string()))
                .collect(),
        )
    }

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn named_parameters() {
        let p = compile("/blog/:slug", false).unwrap();
        assert_eq!(m(&p, "/blog/hello"), Some(kv(&[("slug", "hello")])));
        assert_eq!(m(&p, "/blog/hello/"), Some(kv(&[("slug", "hello")])));
        assert_eq!(m(&p, "/BLOG/Hello"), Some(kv(&[("slug", "Hello")])));
        assert_eq!(m(&p, "/blog/a/b"), None);
        assert_eq!(m(&p, "/blog/"), None);
        assert!(!p.prefix_catch_all);
        assert_eq!(p.prefix, "/blog/");
        let cs = compile("/Blog/:slug", true).unwrap();
        assert!(m(&cs, "/blog/x").is_none());
        assert!(m(&cs, "/Blog/x").is_some());
    }

    #[test]
    fn modifiers() {
        let star = compile("/docs/:path*", false).unwrap();
        assert_eq!(m(&star, "/docs"), Some(kv(&[("path", "")])));
        assert_eq!(m(&star, "/docs/a/b/c"), Some(kv(&[("path", "a/b/c")])));
        assert_eq!(m(&star, "/docsx"), None);
        assert!(star.prefix_catch_all);
        assert_eq!(star.prefix, "/docs/");
        assert!(!star.covers(&compile("/docsx/:id", false).unwrap()));
        assert!(star.covers(&compile("/docs/a/:id", false).unwrap()));
        let plus = compile("/docs/:path+", false).unwrap();
        assert_eq!(m(&plus, "/docs"), None);
        assert_eq!(m(&plus, "/docs/a/b"), Some(kv(&[("path", "a/b")])));
        let opt = compile("/shop/:id?", false).unwrap();
        assert_eq!(m(&opt, "/shop"), Some(kv(&[("id", "")])));
        assert_eq!(m(&opt, "/shop/7"), Some(kv(&[("id", "7")])));
        assert_eq!(m(&opt, "/shop/7/8"), None);
        let custom = compile("/user/:id(\\d+)", false).unwrap();
        assert!(m(&custom, "/user/42").is_some());
        assert!(m(&custom, "/user/abc").is_none());
    }

    #[test]
    fn unnamed_groups_and_catch_all() {
        for src in ["/(.*)", "/:path*", "/:all(.*)", "(.*)"] {
            let p = compile(src, false).unwrap();
            assert!(p.catch_all, "{src}");
            assert!(m(&p, "/").is_some() && m(&p, "/a/b.js").is_some(), "{src}");
        }
        let p = compile("/assets/(.*)", false).unwrap();
        assert!(p.prefix_catch_all && !p.catch_all);
        assert_eq!(p.unnamed, vec!["p1"]);
        assert_eq!(m(&p, "/assets/x/y.js"), Some(kv(&[("p1", "x/y.js")])));
        // A suffix after the group: not a prefix catch-all.
        let p = compile("/(.*)\\.html", false).unwrap();
        assert!(!p.prefix_catch_all && !p.catch_all);
        assert!(m(&p, "/a/b.html").is_some());
        assert!(m(&p, "/a/bxhtml").is_none());
        assert!(!compile("/blog/:slug/(.*)", false).unwrap().prefix_catch_all);
    }

    #[test]
    fn rejects_what_the_proxy_cannot_run() {
        assert!(compile("/:path((?!uk/).*)", false).unwrap_err().contains("look-ahead"));
        assert!(compile("/a/*", false).is_err());
        assert!(compile("no-slash", false).is_err());
        assert!(compile("/{a}", false).is_err());
        assert!(compile("/:a/:a", false).is_err());
        assert!(compile("/x/(a|(b))", false).is_err());
        assert!(compile("/x`y", false).is_err());
        assert!(compile("/(unclosed", false).is_err());
    }

    #[test]
    fn covering() {
        let all = compile("/(.*)", false).unwrap();
        let api = compile("/api/(.*)", false).unwrap();
        let users = compile("/api/users/:id", false).unwrap();
        let other = compile("/blog/:id", false).unwrap();
        assert!(all.covers(&users));
        assert!(api.covers(&users));
        assert!(api.covers(&compile("/API/v2/(.*)", false).unwrap()));
        assert!(!api.covers(&other));
        assert!(!users.covers(&api));
        // A case-sensitive pattern can't cover a case-insensitive one.
        assert!(!compile("/api/(.*)", true).unwrap().covers(&users));
    }

    #[test]
    fn destinations() {
        let p = compile("/blog/:slug/(.*)", false).unwrap();
        assert_eq!(destination("/news/:slug/$1", &p).unwrap(), "/news/${slug}/${p1}");
        assert_eq!(destination("/x?cost=5$", &p).unwrap(), "/x?cost=5$$");
        assert_eq!(
            destination("https://example.com:8443/:slug", &p).unwrap(),
            "https://example.com:8443/${slug}"
        );
        assert!(destination("/x/:nope", &p).is_err());
        assert!(destination("/x/$2", &p).is_err());
        let star = compile("/docs/:path*", false).unwrap();
        assert_eq!(destination("/d/:path*", &star).unwrap(), "/d/${path}");
    }

    #[test]
    fn conditions_become_rule_fragments() {
        let r: Value = serde_json::json!({
            "has": [
                {"type": "header", "key": "x-country", "value": "GB|IE"},
                {"type": "cookie", "key": "beta"},
                {"type": "query", "key": "ref", "value": "news"},
                {"type": "host", "value": "www\\.example\\.com"}
            ],
            "missing": [{"type": "header", "key": "authorization"}]
        });
        assert_eq!(
            conditions(&r).unwrap(),
            vec![
                "HeaderRegexp(`x-country`, `^(?:GB|IE)$`)",
                "HeaderRegexp(`Cookie`, `(?:^|;\\s*)beta=(?:[^;]*)(?:;|$)`)",
                "QueryRegexp(`ref`, `^(?:news)$`)",
                "HostRegexp(`^(?:www\\.example\\.com)$`)",
                "!HeaderRegexp(`authorization`, `^.*$`)",
            ]
        );
        assert!(conditions(&serde_json::json!({"has": [{"type": "geo"}]})).is_err());
        assert!(conditions(&serde_json::json!({"has": [{"type": "header", "key": "a`b"}]})).is_err());
        assert!(conditions(&serde_json::json!({"has": [{"type": "header", "key": "a", "value": "(?=x)"}]})).is_err());
    }

    #[test]
    fn a_typical_vercel_json() {
        let plan = parse(&[(
            "vercel.json",
            r#"{
              "$schema": "https://openapi.vercel.sh/vercel.json",
              "buildCommand": "npm run build",
              "cleanUrls": true,
              "trailingSlash": false,
              "redirects": [
                { "source": "/old/:slug", "destination": "/new/:slug" },
                { "source": "/tmp", "destination": "/", "permanent": false },
                { "source": "/gh", "destination": "https://github.com/x", "statusCode": 302 },
                { "source": "/:path((?!uk/).*)", "destination": "/uk/:path*", "has": [{"type": "header", "key": "x-vercel-ip-country", "value": "GB"}] }
              ],
              "rewrites": [
                { "source": "/api/(.*)", "destination": "https://api.example.com/v1/$1" },
                { "source": "/blog/:slug", "destination": "/posts/:slug?from=blog" },
                { "source": "/(.*)", "destination": "/index.html" }
              ],
              "headers": [
                { "source": "/(.*)", "headers": [{ "key": "X-Frame-Options", "value": "DENY" }] },
                { "source": "/assets/(.*)", "headers": [{ "key": "Cache-Control", "value": "public, max-age=31536000, immutable" }] }
              ],
              "crons": [
                { "path": "/api/cron", "schedule": "0 5 * * *" },
                { "path": "/api/bad", "schedule": "every day" }
              ],
              "functions": { "api/*.ts": { "memory": 1024 } }
            }"#,
        )])
        .unwrap();
        assert_eq!(plan.redirects.len(), 3);
        assert!(plan.redirects[0].permanent && !plan.redirects[1].permanent && !plan.redirects[2].permanent);
        assert_eq!(plan.redirects[0].destination, "/new/${slug}");
        assert_eq!(
            plan.rewrites[0].to,
            RewriteTo::External {
                origin: "https://api.example.com".into(),
                path: "/v1/${p1}".into()
            }
        );
        assert_eq!(plan.rewrites[1].to, RewriteTo::Path("/posts/${slug}".into()));
        assert_eq!(plan.rewrites[2].to, RewriteTo::Fallback("/index.html".into()));
        assert!(plan.rewrites[2].pattern.catch_all);
        assert_eq!(plan.headers.len(), 2);
        assert!(plan.clean_urls);
        assert_eq!(plan.trailing_slash, Some(false));
        assert_eq!(
            plan.crons,
            vec![Cron {
                path: "/api/cron".into(),
                schedule: "0 5 * * *".into()
            }]
        );
        let w = plan.warnings.join("\n");
        assert!(w.contains("redirects[3]") && w.contains("look-ahead"), "{w}");
        assert!(w.contains("query in `/posts/:slug?from=blog` is dropped"), "{w}");
        assert!(w.contains("crons[1]"), "{w}");
        // the build reads it (detect.rs): no warning
        assert!(!w.contains("buildCommand"), "{w}");
        assert!(w.contains("`functions` is not used here"), "{w}");
        assert!(!w.contains("$schema"), "{w}");
    }

    #[test]
    fn vercelify_json_overrides_and_adds_a_firewall() {
        let plan = parse(&[
            (
                "vercel.json",
                r#"{"cleanUrls": true, "redirects": [{"source": "/a", "destination": "/b"}]}"#,
            ),
            (
                "vercelify.json",
                r#"{"cleanUrls": false, "firewall": {
                    "blockIps": ["203.0.113.9", "198.51.100.0/22", "nope"],
                    "trustedIps": ["2001:db8:1::/48"],
                    "rules": [
                      {"name": "wp", "path": "/wp-admin/(.*)", "action": "deny"},
                      {"userAgent": "(?i)badbot", "methods": ["get", "POST"], "action": "deny"},
                      {"path": "/api/(.*)", "action": "rate_limit", "requests": 60, "window": 60},
                      {"action": "rate_limit", "requests": 600},
                      {"action": "deny"},
                      {"path": "/x", "action": "rate_limit", "requests": 0}
                    ]}}"#,
            ),
        ])
        .unwrap();
        assert!(!plan.clean_urls);
        assert_eq!(plan.redirects.len(), 1);
        let fw = &plan.firewall;
        assert_eq!(fw.block.len(), 2);
        assert_eq!(fw.block[1].to_string(), "198.51.100.0/22");
        assert_eq!(fw.trusted[0].to_string(), "2001:db8:1::/48");
        assert_eq!(fw.rules.len(), 4);
        assert_eq!(fw.rules[1].methods, vec!["GET", "POST"]);
        assert!(fw.rules[3].global());
        assert!(!fw.rules[2].global());
        let w = plan.warnings.join("\n");
        assert!(w.contains("`nope` is not an IP"), "{w}");
        assert!(w.contains("would block every visitor"), "{w}");
        assert!(w.contains("`requests` must be"), "{w}");
        assert!(w.contains("trustedIps"), "{w}");
    }

    #[test]
    fn bad_files() {
        assert!(parse(&[("vercel.json", "[]")]).is_err());
        assert!(parse(&[("vercel.json", "{")]).is_err());
        let p = parse(&[("vercel.json", r#"{"redirects": {}, "routes": [], "foo": 1}"#)]).unwrap();
        let w = p.warnings.join("\n");
        assert!(w.contains("must be a list") && w.contains("legacy `routes`") && w.contains("`foo`"));
        assert!(!p.routes_anything());
    }

    #[test]
    fn rule_limit() {
        let many: Vec<Value> = (0..250)
            .map(|i| serde_json::json!({"source": format!("/r{i}"), "destination": "/"}))
            .collect();
        let text = serde_json::json!({"redirects": many, "headers": [{"source": "/(.*)", "headers": [{"key": "a", "value": "b"}]}]}).to_string();
        let p = parse(&[("vercel.json", &text)]).unwrap();
        assert_eq!(p.redirects.len(), MAX_RULES);
        assert!(p.headers.is_empty());
        assert!(p.warnings.iter().any(|w| w.contains("50 redirects over the limit")));
    }

    #[test]
    fn nets_and_their_regexes() {
        let re = |s: &str| net_regex(&parse_net(s).unwrap()).map(|r| Regex::new(&format!("^(?:{r})$")).unwrap());
        let r = re("198.51.100.0/22").unwrap();
        assert!(r.is_match("198.51.100.1") && r.is_match("198.51.103.255"));
        assert!(!r.is_match("198.51.104.1") && !r.is_match("198.51.99.1"));
        let r = re("10.0.0.7").unwrap();
        assert!(r.is_match("10.0.0.7") && !r.is_match("10.0.0.70"));
        let r = re("0.0.0.0/0").unwrap();
        assert!(r.is_match("8.8.8.8"));
        let r = re("2001:db8:1::/48").unwrap();
        assert!(r.is_match("2001:db8:1::5") && r.is_match("2001:db8:1:ff::1"));
        assert!(!r.is_match("2001:db8:2::1"));
        assert!(re("2001:db8::1").unwrap().is_match("2001:db8::1"));
        assert!(re("2001:db8::/36").is_none());
        assert!(re("2001:0:1::/48").is_none());
        assert_eq!(parse_net("192.168.1.77/24").unwrap().to_string(), "192.168.1.0/24");
        assert!(parse_net("1.2.3.4/33").is_err());
        assert!(parse_net("example.com").is_err());
    }

    #[test]
    fn private_origins_are_not_proxied() {
        for o in [
            "http://127.0.0.1:11434",
            "http://localhost:3000",
            "http://coolify:8080",
            "http://coolify-db",
            "https://10.0.0.5",
            "http://192.168.1.2",
            "http://172.20.0.3",
            "http://100.100.1.1",
            "http://169.254.169.254",
            "http://[::1]:7420",
            "http://[fd7a:115c:a1e0::1]",
            "http://[::ffff:127.0.0.1]",
            "http://host.docker.internal:7421",
            "http://printer.local",
            "http://0.0.0.0",
        ] {
            assert!(private_origin(o), "{o}");
        }
        for o in [
            "https://api.example.com",
            "http://8.8.8.8",
            "https://[2001:4860:4860::8888]",
            "https://a.b.c:8443",
        ] {
            assert!(!private_origin(o), "{o}");
        }
        let mut plan = parse(&[(
            "vercel.json",
            r#"{"rewrites": [{"source": "/ai/(.*)", "destination": "http://127.0.0.1:11434/$1"},
                             {"source": "/api/(.*)", "destination": "https://api.example.com/$1"}]}"#,
        )])
        .unwrap();
        drop_private_rewrites(&mut plan);
        assert_eq!(plan.rewrites.len(), 1);
        assert!(plan
            .warnings
            .iter()
            .any(|w| w.contains("rewrites[0]") && w.contains("private network")));
    }

    #[test]
    fn url_parts_stay_out_of_the_query() {
        let p = compile("/old/(.*)", false).unwrap();
        let re = Regex::new(&format!("^https?://[^/?#]+{}(?P<q>\\?.*)?$", p.url_part)).unwrap();
        let c = re.captures("https://a.test/old/x/y?z=1").unwrap();
        assert_eq!(&c["p1"], "x/y");
        assert_eq!(&c["q"], "?z=1");
        assert_eq!(confine("[^/]+\\.x.[a.]"), "[^?#/]+\\.x[^?#][a.]");
    }
}

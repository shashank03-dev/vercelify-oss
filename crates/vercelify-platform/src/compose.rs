//! Docker Compose apps (design §7.2, last paragraph).
//!
//! The repository's compose file runs as project `vfy-<app>` (previews:
//! `vfy-<app>-pr<n>`) with an override file of ours that adds our labels,
//! the restart policy and the variables to every service, and puts the routed
//! service on the proxy network. Variables reach Compose through an env file
//! that exists only while `docker compose up` runs. Compose recreates
//! containers in place, so zero downtime is best effort here.
//!
//! Previews never touch production data (as in `deploy.rs`): the override
//! gives every named volume a name of the preview's own, even one the file
//! names explicitly or marks external, and the merged configuration is
//! checked before anything starts. Storage that can't be made the preview's
//! own (a bind mount outside the checkout, a volume with driver options,
//! another container's volumes) stops the preview, and so does anything it
//! would read from the server: an env file, secret or config file outside
//! the checkout. `config` never reads env files (`--no-env-resolution`), so
//! nothing is read before these checks pass.
//!
//! Compose also reads files while resolving: the project's `.env` (for
//! `${…}`), `include`d files and their env files, `extends.file`. For a
//! preview [`implicit_reads`] finds those in the files themselves and they
//! must be in the checkout, before Compose is run at all.
//!
//! Builds of every compose app read only the checkout (as `detect.rs`): a
//! build context, Dockerfile or additional context outside it is refused.

use crate::docker::{Docker, MANAGED, MANAGED_LABEL, PROXY_NETWORK};
use crate::proc::{self, Cancel, Spec};
use crate::{validate, Error, Result};
use serde_json::Value;
use std::fmt::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

pub fn project_name(app: &str, pr: Option<u32>) -> String {
    match pr {
        Some(n) => format!("vfy-{app}-pr{n}"),
        None => format!("vfy-{app}"),
    }
}

/// Compose names containers `<project>-<service>-<index>`.
pub fn container_of(project: &str, service: &str) -> String {
    format!("{project}-{service}-1")
}

fn q(s: &str) -> String {
    serde_json::to_string(s).expect("serializable")
}

/// One line of a Compose env file. Single quotes are literal (no
/// interpolation); values that contain a quote or a newline use double
/// quotes with escapes.
pub fn env_line(k: &str, v: &str) -> String {
    if !v.contains(['\'', '\\', '\n', '\r']) {
        format!("{k}='{v}'\n")
    } else {
        let e = v
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        format!("{k}=\"{e}\"\n")
    }
}

pub struct Labels<'a> {
    pub app_id: &'a str,
    pub app_name: &'a str,
    pub deployment_id: &'a str,
    pub project_id: &'a str,
    pub pr: Option<u32>,
}

/// Labels on a preview's own volumes, to remove exactly those when it closes.
const PREVIEW_APP_LABEL: &str = "vercelify.preview-app";
const PREVIEW_PR_LABEL: &str = "vercelify.preview-pr";

/// `volumes`: the file's named volumes, renamed to the project's own
/// (previews only, see [`preview_volumes`]).
pub fn override_yaml(
    services: &[String],
    routed: &str,
    env_file: &Path,
    l: &Labels,
    project: &str,
    volumes: &[String],
) -> String {
    let mut y = String::from("# Written by vercelify-agent.\nservices:\n");
    for s in services {
        let _ = writeln!(y, "  {}:", q(s));
        y.push_str("    labels:\n");
        let kind = if l.pr.is_some() { "preview" } else { "app" };
        let mut labels = vec![
            (MANAGED_LABEL, MANAGED.to_string()),
            ("vercelify.app", l.app_id.to_string()),
            ("vercelify.app-name", l.app_name.to_string()),
            ("vercelify.deployment", l.deployment_id.to_string()),
            ("vercelify.project", l.project_id.to_string()),
            ("vercelify.kind", kind.to_string()),
            ("vercelify.service", s.clone()),
        ];
        if let Some(n) = l.pr {
            labels.push(("vercelify.pr", n.to_string()));
        }
        for (k, v) in labels {
            let _ = writeln!(y, "      {}: {}", q(k), q(&v));
        }
        let _ = writeln!(y, "    env_file:\n      - {}", q(&env_file.to_string_lossy()));
        y.push_str("    restart: unless-stopped\n    ports: !reset []\n");
        if let Some(pr) = l.pr {
            let net = crate::preview_network::name(l.app_id, pr);
            let _ = writeln!(y, "    networks: !override\n      {}: {{}}\n    cap_drop:\n      - NET_RAW\n    security_opt:\n      - no-new-privileges:true", q(&net));
        } else if s == routed {
            let _ = writeln!(
                y,
                "    networks:\n      default: {{}}\n      {}: {{}}",
                q(PROXY_NETWORK)
            );
        }
    }
    if let Some(pr) = l.pr {
        let net = crate::preview_network::name(l.app_id, pr);
        let _ = writeln!(
            y,
            "networks: !override\n  {}:\n    external: true\n    name: {}",
            q(&net),
            q(&net)
        );
    } else {
        let _ = writeln!(y, "networks:\n  {}:\n    external: true", q(PROXY_NETWORK));
    }
    if !volumes.is_empty() {
        y.push_str("volumes:\n");
        for v in volumes {
            let _ = writeln!(
                y,
                "  {}:\n    name: {}\n    external: false\n    labels:\n      {}: {}\n      {}: {}",
                q(v),
                q(&format!("{project}_{v}")),
                q(PREVIEW_APP_LABEL),
                q(l.app_id),
                q(PREVIEW_PR_LABEL),
                q(&l.pr.unwrap_or(0).to_string()),
            );
        }
    }
    y
}

fn storage_error(what: String) -> Error {
    Error::Invalid(format!(
        "{what}: a preview can't be given storage of its own for it, and it must not share production's \
         (turn previews off for this app, or use named volumes)"
    ))
}

/// The named volumes a preview's override renames. Volumes with another
/// driver or driver options (a `local` volume with `o: bind` is a folder on
/// the host) can't be copied safely: refused.
pub fn preview_volumes(config: &serde_json::Value) -> Result<Vec<String>> {
    let Some(vols) = config.get("volumes").and_then(|v| v.as_object()) else {
        return Ok(vec![]);
    };
    for (k, v) in vols {
        let driver = v.get("driver").and_then(|d| d.as_str()).unwrap_or("local");
        let opts = v
            .get("driver_opts")
            .and_then(|o| o.as_object())
            .is_some_and(|o| !o.is_empty());
        if driver != "local" || opts {
            return Err(storage_error(format!(
                "the volume {k} uses driver {driver} or driver options"
            )));
        }
    }
    Ok(vols.keys().cloned().collect())
}

/// `path`, or where it leads once symbolic links are followed (as far as
/// it exists; Docker creates the rest), is in `dir`.
fn resolves_inside(dir: &Path, path: &Path) -> bool {
    let Ok(dir) = dir.canonicalize() else {
        return false;
    };
    let mut existing = path;
    let mut rest = Vec::new();
    while existing.symlink_metadata().is_err() {
        match (existing.parent(), existing.file_name()) {
            (Some(p), Some(n)) => {
                rest.push(n);
                existing = p;
            }
            _ => return false,
        }
    }
    let Ok(mut real) = existing.canonicalize() else {
        return false;
    };
    for n in rest.into_iter().rev() {
        if n == ".." {
            return false;
        }
        real.push(n);
    }
    real.starts_with(dir)
}

/// Everything Compose reads to resolve `file` (in `dir`) besides `file`:
/// `.env` of each project directory, `include` paths, env files and project
/// directories, and `extends.file`, following included and extended files.
/// Refused: any of them outside `dir`, or a path with `$` in it (Compose
/// would interpolate it: there's no telling where it leads).
pub fn implicit_reads(dir: &Path, file: &str) -> Result<Vec<PathBuf>> {
    use yaml_rust2::{Yaml, YamlLoader};
    let outside = |what: &str, p: &Path| {
        let to = std::fs::canonicalize(p)
            .ok()
            .filter(|real| real != p)
            .map(|real| format!(" (it leads to {})", real.display()))
            .unwrap_or_default();
        Error::Invalid(format!(
            "{what} {}{to} is outside the repository: a preview must not read production's files from the server",
            p.display()
        ))
    };
    fn strings(y: &Yaml) -> Vec<&str> {
        match y {
            Yaml::String(s) => vec![s.as_str()],
            Yaml::Array(a) => a.iter().filter_map(Yaml::as_str).collect(),
            _ => vec![],
        }
    }
    let mut reads: Vec<PathBuf> = vec![];
    let mut queue = vec![dir.join(file)];
    let mut seen = std::collections::HashSet::new();
    let dotenv = |project: &Path, reads: &mut Vec<PathBuf>| {
        let env = project.join(".env");
        if env.symlink_metadata().is_ok() {
            reads.push(env);
        }
    };
    dotenv(dir.join(file).parent().unwrap_or(dir), &mut reads);
    while let Some(f) = queue.pop() {
        if !seen.insert(f.clone()) {
            continue;
        }
        if !resolves_inside(dir, &f) {
            return Err(outside("the compose file", &f));
        }
        let text = std::fs::read_to_string(&f).map_err(|e| Error::Invalid(format!("{}: {e}", f.display())))?;
        // The loader copies what an alias names: bound what that comes to
        // before building it (a few nested aliases are billions of nodes).
        if text.len() > MAX_COMPOSE_TEXT || expanded_nodes(&text) > MAX_COMPOSE_NODES {
            return Err(Error::Invalid(format!(
                "{} is too large once its aliases are expanded for a preview",
                f.display()
            )));
        }
        let docs = YamlLoader::load_from_str(&text).map_err(|e| Error::Invalid(format!("{}: {e}", f.display())))?;
        let base = f.parent().unwrap_or(dir).to_path_buf();
        let at = |p: &str| -> Result<PathBuf> {
            if p.contains('$') {
                return Err(Error::Invalid(format!(
                    "{}: `{p}` is interpolated: a preview's compose files must name their files plainly",
                    f.display()
                )));
            }
            Ok(base.join(p))
        };
        for doc in docs.iter().map(resolve_merges) {
            let includes = match &doc["include"] {
                Yaml::Array(a) => a.iter().collect::<Vec<_>>(),
                Yaml::BadValue | Yaml::Null => vec![],
                other => vec![other],
            };
            for inc in includes {
                // `- path` or `{path, env_file, project_directory}`.
                let (paths, env_files, project) = match inc {
                    Yaml::String(_) => (strings(inc), vec![], None),
                    _ => (
                        strings(&inc["path"]),
                        strings(&inc["env_file"]),
                        inc["project_directory"].as_str(),
                    ),
                };
                for p in paths {
                    let p = at(p)?;
                    if project.is_none() {
                        dotenv(p.parent().unwrap_or(dir), &mut reads);
                    }
                    queue.push(p);
                }
                for e in env_files {
                    reads.push(at(e)?);
                }
                if let Some(pd) = project {
                    // Its `.env` and relative paths are read from there.
                    let pd = at(pd)?;
                    if !resolves_inside(dir, &pd) {
                        return Err(outside("the include's project directory", &pd));
                    }
                    dotenv(&pd, &mut reads);
                }
            }
            // Every `extends.file` anywhere, not only under `services`: a
            // superset of what Compose reads, so no spelling hides one.
            let mut files = vec![];
            extends_files(&doc, &mut files);
            for file in files {
                queue.push(at(&file)?);
            }
        }
    }
    for r in &reads {
        if !resolves_inside(dir, r) {
            return Err(outside("the file", r));
        }
        // These files affect Compose itself before any container exists.
        // Inspect keys only; never include their values in diagnostics.
        let metadata = std::fs::metadata(r)?;
        if metadata.len() > MAX_COMPOSE_TEXT as u64 {
            return Err(Error::Invalid("Compose environment file exceeds 1 MiB".into()));
        }
        crate::env::parse_dotenv(&std::fs::read_to_string(r)?)?;
    }
    Ok(reads)
}

const MAX_COMPOSE_TEXT: usize = 1024 * 1024;
const MAX_COMPOSE_NODES: u64 = 100_000;

/// How many nodes `text` comes to with every alias expanded (all documents),
/// from the parser's events: nothing is built. A text that doesn't parse
/// counts 0 (the loader reports the error).
fn expanded_nodes(text: &str) -> u64 {
    use yaml_rust2::parser::{Event, EventReceiver, Parser};
    #[derive(Default)]
    struct Count {
        /// Open collections: (anchor id, nodes so far).
        open: Vec<(usize, u64)>,
        anchors: std::collections::HashMap<usize, u64>,
        total: u64,
    }
    impl Count {
        fn add(&mut self, n: u64) {
            match self.open.last_mut() {
                Some((_, c)) => *c = c.saturating_add(n),
                None => self.total = self.total.saturating_add(n),
            }
        }
    }
    impl EventReceiver for Count {
        fn on_event(&mut self, ev: Event) {
            match ev {
                Event::Scalar(_, _, anchor, _) => {
                    if anchor > 0 {
                        self.anchors.insert(anchor, 1);
                    }
                    self.add(1);
                }
                Event::SequenceStart(anchor, _) | Event::MappingStart(anchor, _) => self.open.push((anchor, 1)),
                Event::SequenceEnd | Event::MappingEnd => {
                    if let Some((anchor, n)) = self.open.pop() {
                        if anchor > 0 {
                            self.anchors.insert(anchor, n);
                        }
                        self.add(n);
                    }
                }
                Event::Alias(id) => {
                    let n = self.anchors.get(&id).copied().unwrap_or(1);
                    self.add(n);
                }
                _ => {}
            }
        }
    }
    let mut count = Count::default();
    if Parser::new_from_str(text).load(&mut count, true).is_err() {
        return 0;
    }
    // Collections still open (a parse cut short) count too.
    count.open.iter().fold(count.total, |t, (_, n)| t.saturating_add(*n))
}

/// `y` with YAML merge keys (`<<`) applied throughout, as Compose applies
/// them: a map's own keys win over merged ones, and an earlier map in a
/// `<<` list over a later one.
fn resolve_merges(y: &yaml_rust2::Yaml) -> yaml_rust2::Yaml {
    use yaml_rust2::Yaml;
    match y {
        Yaml::Hash(h) => {
            let merge = Yaml::String("<<".into());
            let mut out = yaml_rust2::yaml::Hash::new();
            for (k, v) in h.iter().filter(|(k, _)| **k != merge) {
                out.insert(k.clone(), resolve_merges(v));
            }
            let sources = match h.get(&merge) {
                Some(Yaml::Array(a)) => a.iter().collect(),
                Some(m) => vec![m],
                None => vec![],
            };
            for src in sources {
                if let Yaml::Hash(m) = resolve_merges(src) {
                    for (k, v) in m {
                        if !out.contains_key(&k) {
                            out.insert(k, v);
                        }
                    }
                }
            }
            Yaml::Hash(out)
        }
        Yaml::Array(a) => Yaml::Array(a.iter().map(resolve_merges).collect()),
        other => other.clone(),
    }
}

/// The `file` of every `extends` map in `y` (merges resolved).
fn extends_files(y: &yaml_rust2::Yaml, out: &mut Vec<String>) {
    use yaml_rust2::Yaml;
    match y {
        Yaml::Hash(h) => {
            for (k, v) in h {
                if k.as_str() == Some("extends") {
                    if let Some(f) = v["file"].as_str() {
                        out.push(f.to_string());
                    }
                }
                extends_files(v, out);
            }
        }
        Yaml::Array(a) => a.iter().for_each(|v| extends_files(v, out)),
        _ => {}
    }
}

/// The merged configuration a preview runs with (`docker compose config`
/// with our override): every volume is the project's own, nothing is mounted
/// from outside the checkout, no other container's volumes are used, and no
/// env file, secret, config or SSH key comes from outside it.
pub fn check_preview_storage(config: &serde_json::Value, dir: &Path, project: &str) -> Result<()> {
    let own = format!("{project}_");
    if let Some(vols) = config.get("volumes").and_then(|v| v.as_object()) {
        for (k, v) in vols {
            let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let external = v.get("external").and_then(|e| e.as_bool()).unwrap_or(false);
            let opts = v
                .get("driver_opts")
                .and_then(|o| o.as_object())
                .is_some_and(|o| !o.is_empty());
            if external || opts || !name.starts_with(&own) {
                return Err(storage_error(format!(
                    "the volume {k} would be {name}, not the preview's own"
                )));
            }
        }
    }
    let services = config.get("services").and_then(|s| s.as_object());
    for (svc, s) in services.into_iter().flatten() {
        check_preview_execution(svc, s)?;
        for m in s.get("volumes").and_then(|v| v.as_array()).into_iter().flatten() {
            let kind = m.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let source = m.get("source").and_then(|t| t.as_str()).unwrap_or("");
            match kind {
                "volume" | "tmpfs" | "image" => {}
                "bind" if resolves_inside(dir, Path::new(source)) => {}
                "bind" => {
                    return Err(storage_error(format!(
                        "{svc} mounts {source} from the server, outside the repository"
                    )))
                }
                other => return Err(storage_error(format!("{svc} has a mount of type {other}"))),
            }
        }
        for from in s.get("volumes_from").and_then(|v| v.as_array()).into_iter().flatten() {
            let from = from.as_str().unwrap_or("");
            if from.starts_with("container:") {
                return Err(storage_error(format!("{svc} uses the volumes of {from}")));
            }
        }
        // Env files: strings, or `{path, required}` since Compose 2.24.
        for f in s.get("env_file").and_then(|v| v.as_array()).into_iter().flatten() {
            let path = f
                .as_str()
                .or_else(|| f.get("path").and_then(|p| p.as_str()))
                .unwrap_or("");
            if !resolves_inside(dir, Path::new(path)) {
                return Err(server_file(format!("{svc} reads the env file {path}")));
            }
        }
        let ssh = s.get("build").and_then(|b| b.get("ssh")).and_then(|v| v.as_array());
        for k in ssh.into_iter().flatten() {
            let k = k.as_str().unwrap_or("");
            if let Some((_, path)) = k.split_once('=') {
                if !resolves_inside(dir, Path::new(path)) {
                    return Err(server_file(format!("{svc} builds with the SSH key {path}")));
                }
            }
        }
    }
    if config.get(ENV_FILES_READ).is_some() {
        return Err(Error::Invalid(
            "this Docker Compose reads env files before Vercelify can check them: update it \
             (2.24 or newer) for previews of compose apps"
                .into(),
        ));
    }
    // Secrets and configs from files (services and builds use them by name).
    for kind in ["secrets", "configs"] {
        for (k, v) in config.get(kind).and_then(|v| v.as_object()).into_iter().flatten() {
            if v.get("external").and_then(|e| e.as_bool()).unwrap_or(false) {
                return Err(server_file(format!("the {kind} entry {k} is external")));
            }
            if let Some(f) = v.get("file").and_then(|f| f.as_str()) {
                if !resolves_inside(dir, Path::new(f)) {
                    return Err(server_file(format!("the {kind} entry {k} is the file {f}")));
                }
            }
        }
    }
    Ok(())
}

fn check_preview_execution(svc: &str, s: &serde_json::Value) -> Result<()> {
    let nonempty = |key: &str| {
        s.get(key).is_some_and(|v| match v {
            serde_json::Value::Null => false,
            serde_json::Value::Array(a) => !a.is_empty(),
            serde_json::Value::Object(o) => !o.is_empty(),
            serde_json::Value::String(s) => !s.is_empty(),
            serde_json::Value::Bool(b) => *b,
            _ => true,
        })
    };
    for field in [
        "privileged",
        "network_mode",
        "pid",
        "ipc",
        "uts",
        "cgroup",
        "cap_add",
        "devices",
        "device_cgroup_rules",
        "ports",
        "external_links",
        "container_name",
        "use_api_socket",
        "provider",
        "gpus",
    ] {
        if nonempty(field) {
            return Err(Error::Invalid(format!(
                "{svc}: {field} is not allowed in isolated previews"
            )));
        }
    }
    for hook in ["post_start", "pre_stop"] {
        if s[hook]
            .as_array()
            .is_some_and(|a| a.iter().any(|h| h["privileged"] == true))
        {
            return Err(Error::Invalid(format!(
                "{svc}: privileged lifecycle hooks are not allowed in previews"
            )));
        }
    }
    if s.get("security_opt")
        .and_then(|v| v.as_array())
        .is_some_and(|a| a.iter().any(|v| v.as_str() != Some("no-new-privileges:true")))
    {
        return Err(Error::Invalid(format!(
            "{svc}: preview security options cannot weaken isolation"
        )));
    }
    if s.get("build").is_some_and(|b| {
        b["privileged"] == true
            || b["network"] == "host"
            || b.get("entitlements")
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty())
    }) {
        return Err(Error::Invalid(format!(
            "{svc}: privileged or host-network builds are not allowed in previews"
        )));
    }
    Ok(())
}

fn check_preview_networks(config: &serde_json::Value, network: &str) -> Result<()> {
    for (svc, s) in config["services"].as_object().into_iter().flatten() {
        let networks = s["networks"].as_object();
        if !networks.is_some_and(|n| n.len() == 1 && n.contains_key(network)) {
            return Err(Error::Invalid(format!(
                "{svc}: preview must join only its isolated network (Compose 2.24.4+ required)"
            )));
        }
    }
    Ok(())
}

fn server_file(what: String) -> Error {
    Error::Invalid(format!(
        "{what}, outside the repository: a preview must not read production's files from the server \
         (keep it in the repository, or set preview variables in the app)"
    ))
}

#[derive(Clone, Copy)]
pub struct Limits<'a> {
    /// Aggregate budget for all services, including sidecars.
    pub memory_mb: u32,
    pub cpus: f32,
    pub cgroup_parent: Option<&'a str>,
}
fn bounded_override(
    services: &[String],
    routed: &str,
    env_file: &Path,
    labels: &Labels,
    project: &str,
    volumes: &[String],
    limits: Limits<'_>,
) -> Result<String> {
    let count = services.len() as u32;
    if count == 0 || limits.memory_mb / count < 16 || !limits.cpus.is_finite() || limits.cpus <= 0.0 {
        return Err(Error::Invalid(
            "Compose budget must provide at least 16 MiB per service and a positive CPU limit".into(),
        ));
    }
    let memory = limits.memory_mb / count;
    let cpus = (f64::from(limits.cpus) * 1000.0 / f64::from(count)).floor() / 1000.0;
    if cpus < 0.001 {
        return Err(Error::Invalid(
            "Compose CPU budget is too small for its services".into(),
        ));
    }
    let mut yaml = override_yaml(services, routed, env_file, labels, project, volumes);
    for service in services {
        let header = format!("  {}:\n", q(service));
        let mut bounded = format!("{header}    mem_limit: {memory}m\n    memswap_limit: {memory}m\n    cpus: {cpus:.3}\n    scale: 1\n    deploy:\n      replicas: 1\n      resources:\n        limits:\n          memory: {memory}m\n          cpus: '{cpus:.3}'\n");
        if let Some(parent) = limits.cgroup_parent {
            let _ = writeln!(bounded, "    cgroup_parent: {}", q(parent));
        }
        yaml = yaml.replacen(&header, &bounded, 1);
    }
    Ok(yaml)
}

pub struct Up<'a> {
    pub limits: Limits<'a>,
    pub build: bool,
    pub docker: &'a Docker,
    /// The checkout (repository root).
    pub dir: &'a Path,
    pub file: &'a str,
    pub project: &'a str,
    /// The service the domains route to (default: the first one).
    pub service: Option<&'a str>,
    pub env: &'a [(String, String)],
    pub labels: Labels<'a>,
    pub timeout: Duration,
    pub scratch: &'a Path,
}

/// The resolved configuration of compose files (`docker compose config
/// --format json`), as the project `project` when given.
pub async fn config(
    docker: &Docker,
    dir: &Path,
    files: &[&str],
    project: Option<&str>,
    env: &[(String, String)],
) -> Result<serde_json::Value> {
    let file = files.first().copied().unwrap_or_default();
    let mut args = vec!["compose".to_string()];
    if let Some(p) = project {
        args.extend(["-p".to_string(), p.to_string()]);
    }
    for f in files {
        args.extend(["-f".to_string(), f.to_string()]);
    }
    args.extend(proc::args(&["config", "--format", "json", NO_ENV_RESOLUTION]));
    let run = |args: Vec<String>| async move {
        proc::run_clean(Spec {
            bin: &docker.bin,
            args: &args,
            env,
            cwd: Some(dir),
            timeout: Duration::from_secs(60),
        })
        .await
    };
    let o = run(args).await?;
    if !o.success() {
        return Err(Error::Invalid(format!(
            "{file} is not a valid compose file: {}",
            o.tail(4)
        )));
    }
    serde_json::from_str(&o.stdout).map_err(|e| Error::Invalid(format!("docker compose config: {e}")))
}

const NO_ENV_RESOLUTION: &str = "--no-env-resolution";
/// Set on a [`config`] made by a Compose that read the env files.
const ENV_FILES_READ: &str = "x-vercelify-env-files-read";

/// A local path Compose resolved (absolute), or a path inside `oci-layout://`;
/// None for what BuildKit fetches itself (a Git or HTTP URL, an image, a
/// service).
fn local_source(s: &str) -> Option<&str> {
    s.strip_prefix("oci-layout://")
        .or(Some(s))
        .filter(|p| p.starts_with('/'))
}

/// What every compose app's builds may read: the checkout only. A service's
/// `build.context`, its Dockerfile and `additional_contexts` must resolve
/// inside `dir` (an inline Dockerfile reads only its context).
pub fn check_build_inputs(config: &serde_json::Value, dir: &Path) -> Result<()> {
    let services = config.get("services").and_then(|s| s.as_object());
    for (svc, s) in services.into_iter().flatten() {
        let Some(b) = s.get("build").and_then(|b| b.as_object()) else {
            continue;
        };
        let outside = |what: &str, path: &str| {
            Error::Invalid(format!(
                "{svc}: the build {what} {path} is outside the repository: builds only read the repository's own files"
            ))
        };
        let context = b.get("context").and_then(|c| c.as_str()).unwrap_or("");
        let local = local_source(context);
        if let Some(c) = local {
            if !resolves_inside(dir, Path::new(c)) {
                return Err(outside("context", c));
            }
        }
        if let Some(df) = b.get("dockerfile").and_then(|d| d.as_str()) {
            let path = match local {
                _ if df.starts_with('/') => Some(Path::new(df).to_path_buf()),
                Some(c) => Some(Path::new(c).join(df)),
                None => None,
            };
            if let Some(p) = path.filter(|p| !resolves_inside(dir, p)) {
                return Err(outside("Dockerfile", &p.display().to_string()));
            }
        }
        for (name, v) in b
            .get("additional_contexts")
            .and_then(|a| a.as_object())
            .into_iter()
            .flatten()
        {
            if let Some(p) = v.as_str().and_then(local_source) {
                if !resolves_inside(dir, Path::new(p)) {
                    return Err(outside(&format!("context {name}"), p));
                }
            }
        }
    }
    Ok(())
}

/// Inline, image-only Compose does not need a serving-host builder. Unknown
/// includes/extends remain conservative until the full resolved plan is checked.
pub fn requires_build(raw: &str) -> Result<bool> {
    use yaml_rust2::{Yaml, YamlLoader};
    if raw.len() > MAX_COMPOSE_TEXT || expanded_nodes(raw) > MAX_COMPOSE_NODES {
        return Err(Error::Invalid("Compose file exceeds safe parsing limits".into()));
    }
    let docs = YamlLoader::load_from_str(raw).map_err(|e| Error::Invalid(e.to_string()))?;
    if docs.len() != 1 {
        return Err(Error::Invalid("Compose needs one document".into()));
    }
    let root = resolve_merges(&docs[0]);
    if !matches!(root["include"], Yaml::BadValue | Yaml::Null) {
        return Ok(true);
    }
    let Some(services) = root["services"].as_hash() else {
        return Ok(true);
    };
    Ok(services.values().any(|s| {
        !matches!(s["build"], Yaml::BadValue | Yaml::Null)
            || !matches!(s["extends"], Yaml::BadValue | Yaml::Null)
            || s["image"].as_str().is_none()
    }))
}

/// What `up` started.
#[derive(Debug, Clone, PartialEq)]
pub struct Started {
    pub container: String,
    /// The routed service's port, when the compose file names exactly one.
    pub port: Option<u16>,
}

/// The services of a compose file, the one to route to and its port.
#[derive(Debug, Clone, PartialEq)]
pub struct Picked {
    pub services: Vec<String>,
    pub routed: String,
    pub port: Option<u16>,
}

const WEB_NAMES: [&str; 4] = ["web", "app", "server", "frontend"];
/// Images and ports that are a database or broker, never the website.
const DATASTORE_IMAGES: [&str; 20] = [
    "postgres",
    "postgis",
    "pgvector",
    "timescale",
    "mysql",
    "mariadb",
    "mongo",
    "redis",
    "valkey",
    "keydb",
    "memcached",
    "rabbitmq",
    "clickhouse",
    "elasticsearch",
    "opensearch",
    "kafka",
    "zookeeper",
    "cassandra",
    "couchdb",
    "influxdb",
];
const DATASTORE_PORTS: [u16; 14] = [
    5432, 3306, 6379, 27017, 11211, 5672, 9042, 2181, 9092, 1433, 1521, 26257, 4222, 8086,
];

/// The container ports a service declares (`ports` targets and `expose`).
fn declared_ports(s: &Value) -> Vec<u16> {
    let mut out: Vec<u16> = vec![];
    for p in s.get("ports").and_then(Value::as_array).into_iter().flatten() {
        let t = p
            .get("target")
            .and_then(Value::as_u64)
            .and_then(|t| u16::try_from(t).ok());
        out.extend(t);
    }
    for e in s.get("expose").and_then(Value::as_array).into_iter().flatten() {
        let t = match e {
            Value::String(x) => x.split(['/', '-']).next().and_then(|n| n.trim().parse().ok()),
            Value::Number(n) => n.as_u64().and_then(|n| u16::try_from(n).ok()),
            _ => None,
        };
        out.extend(t);
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn is_datastore(s: &Value) -> bool {
    let image = s.get("image").and_then(Value::as_str).unwrap_or("");
    let base = image.split(['@', ':']).next().unwrap_or("");
    let base = base.rsplit('/').next().unwrap_or("");
    let ports = declared_ports(s);
    DATASTORE_IMAGES.iter().any(|d| base.starts_with(d))
        || (!ports.is_empty() && ports.iter().all(|p| DATASTORE_PORTS.contains(p)))
}

/// Which service gets the domains: the app's choice; else the one service
/// that declares a port and isn't a database; else the one called
/// web/app/server/frontend; else the only service. When several are
/// plausible the app has to choose (a database is never guessed).
pub fn picked(config: &Value, explicit: Option<&str>) -> Result<Picked> {
    let services = config
        .get("services")
        .and_then(|s| s.as_object())
        .ok_or_else(|| Error::Invalid("no services".into()))?;
    let mut names: Vec<String> = services.keys().cloned().collect();
    names.sort();
    if names.is_empty() {
        return Err(Error::Invalid("no services".into()));
    }
    let done = |routed: &str| Picked {
        services: names.clone(),
        routed: routed.to_string(),
        port: match declared_ports(&services[routed]).as_slice() {
            [one] => Some(*one),
            _ => None,
        },
    };
    if let Some(e) = explicit {
        return if names.iter().any(|n| n == e) {
            Ok(done(e))
        } else {
            Err(Error::Invalid(format!(
                "{e} is not one of its services ({})",
                names.join(", ")
            )))
        };
    }
    let web: Vec<&String> = names.iter().filter(|n| !is_datastore(&services[n.as_str()])).collect();
    let named = |among: &[&String]| -> Option<String> {
        let hits: Vec<&&String> = among.iter().filter(|n| WEB_NAMES.contains(&n.as_str())).collect();
        (hits.len() == 1).then(|| hits[0].to_string())
    };
    let with_port: Vec<&String> = web
        .iter()
        .copied()
        .filter(|n| !declared_ports(&services[n.as_str()]).is_empty())
        .collect();
    let routed = match with_port.len() {
        1 => Some(with_port[0].clone()),
        0 => named(&web).or_else(|| match (web.len(), names.len()) {
            (1, _) => Some(web[0].clone()),
            (_, 1) => Some(names[0].clone()),
            _ => None,
        }),
        _ => named(&with_port),
    };
    match routed {
        Some(r) => Ok(done(&r)),
        None => {
            let plausible: Vec<&str> = if with_port.is_empty() { &web } else { &with_port }
                .iter()
                .map(|s| s.as_str())
                .collect();
            let list = if plausible.is_empty() {
                names.join(", ")
            } else {
                plausible.join(", ")
            };
            Err(Error::Invalid(format!(
                "can't tell which service serves the website ({list}): choose the service in the app's settings"
            )))
        }
    }
}

pub fn pick(config: &Value, explicit: Option<&str>) -> Result<(Vec<String>, String)> {
    let p = picked(config, explicit)?;
    Ok((p.services, p.routed))
}

/// `a/./b/../c` → `a/c`, without touching the disk.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// No symbolic link in the checkout may lead out of it (or nowhere):
/// Compose runs as root and follows them for `.env`, env files, bind
/// mounts, configs, secrets and build contexts. `root` is canonical.
pub fn check_tree(root: &Path) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir)? {
            let e = e?;
            let p = e.path();
            let ft = e.file_type()?;
            if ft.is_symlink() {
                if !std::fs::canonicalize(&p).is_ok_and(|t| t.starts_with(root)) {
                    let rel = p.strip_prefix(root).unwrap_or(&p);
                    return Err(Error::Invalid(format!(
                        "{} is a symbolic link that leads outside the repository (or nowhere)",
                        rel.display()
                    )));
                }
            } else if ft.is_dir() && e.file_name() != ".git" {
                stack.push(p);
            }
        }
    }
    Ok(())
}

/// Docker's default capabilities: adding these back is harmless.
const SAFE_CAPS: [&str; 14] = [
    "CHOWN",
    "DAC_OVERRIDE",
    "FSETID",
    "FOWNER",
    "MKNOD",
    "NET_RAW",
    "SETGID",
    "SETUID",
    "SETFCAP",
    "SETPCAP",
    "NET_BIND_SERVICE",
    "SYS_CHROOT",
    "KILL",
    "AUDIT_WRITE",
];

/// The deployment boundary for compose apps (design §7.2): the resolved
/// model may not ask Docker for the host. No privileges, host namespaces,
/// devices, extra capabilities, published ports, Docker socket, host paths
/// or other apps' volumes and networks; bind mounts only read-only from the
/// checkout (persistent data belongs in named volumes). `inside` are the
/// directories files may come from (the checkout, our scratch directory).
pub fn check_model(config: &Value, inside: &[&Path]) -> Result<()> {
    let mut bad: Vec<String> = vec![];
    let ok_path = |p: &str| {
        let p = Path::new(p);
        let real = std::fs::canonicalize(p).unwrap_or_else(|_| lexical(p));
        p.is_absolute() && inside.iter().any(|d| real.starts_with(d))
    };
    let host_ns = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .is_some_and(|m| m == "host" || m.starts_with("container:"))
    };
    let empty = Default::default();
    let services = config.get("services").and_then(Value::as_object).unwrap_or(&empty);
    let top_volumes = config.get("volumes").and_then(Value::as_object);
    for (name, s) in services {
        let mut no = |what: &str| bad.push(format!("{name}: {what}"));
        if s.get("privileged").and_then(Value::as_bool) == Some(true) {
            no("privileged mode");
        }
        if s.get("use_api_socket").and_then(Value::as_bool) == Some(true) {
            no("the Docker API socket");
        }
        if s.get("provider").is_some() {
            no("a provider service (runs a program on the server)");
        }
        for c in s.get("cap_add").and_then(Value::as_array).into_iter().flatten() {
            let c = c.as_str().unwrap_or("").to_ascii_uppercase();
            if !SAFE_CAPS.contains(&c.trim_start_matches("CAP_")) {
                no(&format!("capability {c}"));
            }
        }
        for k in ["devices", "device_cgroup_rules"] {
            if s.get(k).and_then(Value::as_array).is_some_and(|a| !a.is_empty()) {
                no("host devices");
            }
        }
        for k in ["network_mode", "pid", "ipc", "uts", "userns_mode", "cgroup"] {
            if host_ns(s.get(k)) {
                no(&format!("{k}: {}", s[k].as_str().unwrap_or("")));
            }
        }
        if s.get("cgroup_parent")
            .and_then(Value::as_str)
            .is_some_and(|c| !c.is_empty())
        {
            no("cgroup_parent");
        }
        for o in s.get("security_opt").and_then(Value::as_array).into_iter().flatten() {
            let o = o.as_str().unwrap_or("");
            if !o.starts_with("no-new-privileges") {
                no(&format!("security_opt {o}"));
            }
        }
        if s.get("ports").and_then(Value::as_array).is_some_and(|a| !a.is_empty()) {
            no("published ports (use `expose`; the proxy routes the domains)");
        }
        for v in s.get("volumes_from").and_then(Value::as_array).into_iter().flatten() {
            if v.as_str().is_some_and(|v| v.starts_with("container:")) {
                no("volumes_from another container");
            }
        }
        for v in s.get("volumes").and_then(Value::as_array).into_iter().flatten() {
            let src = v.get("source").and_then(Value::as_str).unwrap_or("");
            let target = v.get("target").and_then(Value::as_str).unwrap_or("?");
            match v.get("type").and_then(Value::as_str).unwrap_or("") {
                "tmpfs" => {}
                "volume" => {
                    if !src.is_empty() && top_volumes.and_then(|t| t.get(src)).is_none() {
                        no(&format!("volume {src} is not declared"));
                    }
                }
                "bind" => {
                    let ro = v.get("read_only").and_then(Value::as_bool) == Some(true);
                    if !ok_path(src) {
                        no(&format!(
                            "bind mount of {src} (only files of the repository can be mounted)"
                        ));
                    } else if !ro {
                        no(&format!(
                            "writable bind mount at {target}: the checkout is temporary, keep data in a named volume (or add :ro)"
                        ));
                    }
                }
                other => no(&format!("{other} mount at {target}")),
            }
        }
        for f in s.get("env_file").and_then(Value::as_array).into_iter().flatten() {
            let p = f.get("path").or(Some(f)).and_then(Value::as_str).unwrap_or("");
            if !ok_path(p) {
                no(&format!("env_file {p} is outside the repository"));
            }
        }
        if let Some(b) = s.get("build").filter(|b| b.is_object()) {
            let local = |c: &str| !c.contains("://") && !c.starts_with("git@") && !c.contains(':');
            if let Some(c) = b.get("context").and_then(Value::as_str) {
                if local(c) && !ok_path(c) {
                    no(&format!("build context {c} is outside the repository"));
                }
                if let Some(d) = b.get("dockerfile").and_then(Value::as_str) {
                    let d = if Path::new(d).is_absolute() {
                        PathBuf::from(d)
                    } else {
                        Path::new(c).join(d)
                    };
                    if local(c) && !ok_path(&d.to_string_lossy()) {
                        no(&format!("dockerfile {} is outside the repository", d.display()));
                    }
                }
            }
            for c in b
                .get("additional_contexts")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|m| m.values())
            {
                let c = c.as_str().unwrap_or("");
                if c.starts_with('/') && !ok_path(c) {
                    no(&format!("build context {c} is outside the repository"));
                }
            }
            if b.get("ssh").is_some() {
                no("build ssh (the server's SSH agent)");
            }
            if b.get("network").and_then(Value::as_str) == Some("host") {
                no("build network: host");
            }
            if b.get("privileged").and_then(Value::as_bool) == Some(true)
                || b.get("entitlements")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty())
            {
                no("privileged builds");
            }
        }
    }
    for kind in ["configs", "secrets"] {
        for (name, c) in config.get(kind).and_then(Value::as_object).into_iter().flatten() {
            if let Some(f) = c.get("file").and_then(Value::as_str) {
                if !ok_path(f) {
                    bad.push(format!("{kind} {name}: {f} is outside the repository"));
                }
            }
            if c.get("external").and_then(Value::as_bool) == Some(true) {
                bad.push(format!("{kind} {name}: external"));
            }
        }
    }
    for (name, v) in top_volumes.into_iter().flatten() {
        if v.get("external").and_then(Value::as_bool) == Some(true) {
            bad.push(format!("volume {name}: external volumes belong to something else"));
        }
        if v.get("driver_opts")
            .is_some_and(|o| o.as_object().is_some_and(|o| !o.is_empty()))
        {
            bad.push(format!("volume {name}: driver_opts (host paths)"));
        }
        if v.get("driver").and_then(Value::as_str).is_some_and(|d| d != "local") {
            bad.push(format!("volume {name}: driver other than local"));
        }
    }
    for (name, n) in config.get("networks").and_then(Value::as_object).into_iter().flatten() {
        let real = n.get("name").and_then(Value::as_str).unwrap_or(name);
        if n.get("external").and_then(Value::as_bool) == Some(true) && real != PROXY_NETWORK {
            bad.push(format!("network {name}: external networks belong to something else"));
        }
        if n.get("driver")
            .and_then(Value::as_str)
            .is_some_and(|d| matches!(d, "host" | "macvlan" | "ipvlan"))
        {
            bad.push(format!("network {name}: driver {}", n["driver"].as_str().unwrap_or("")));
        }
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "this compose file asks for access to the server, which apps don't get: {}",
            bad.join("; ")
        )))
    }
}

/// A quick look at a compose file saved with the app (no Compose run) for
/// what [`check_model`] refuses anyway, so the mistake shows when the app is
/// saved rather than at its first deployment. Not the boundary itself.
pub fn precheck(content: &str) -> Result<()> {
    let mut bad: Vec<String> = vec![];
    for line in content.lines() {
        let l = line.split(" #").next().unwrap_or("").trim().trim_start_matches("- ");
        let Some((k, v)) = l.split_once(':') else { continue };
        let (k, v) = (k.trim().trim_matches(['"', '\'']), v.trim().trim_matches(['"', '\'']));
        let hit = match k {
            "privileged" | "use_api_socket" => matches!(v, "true" | "yes" | "on"),
            "network_mode" | "pid" | "ipc" | "uts" | "userns_mode" | "cgroup" => v == "host",
            "devices" | "device_cgroup_rules" | "cgroup_parent" => true,
            _ => l.contains("docker.sock"),
        };
        if hit {
            bad.push(l.to_string());
        }
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "this compose file asks for access to the server, which apps don't get: {}",
            bad.join("; ")
        )))
    }
}

/// A new private file (never through an existing name or link).
fn create_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    Ok(())
}

/// Build and start the project; returns the routed container's name.
pub async fn up(u: Up<'_>, cancel: Cancel, log: &mut (dyn FnMut(&str, &str) + Send)) -> Result<Started> {
    validate::repo_path("compose file", u.file)?;
    let root = std::fs::canonicalize(u.dir)?;
    check_tree(&root)?;
    {
        // Reject implicit server-file reads before even consulting Docker
        // for cleanup; no external tool may resolve this source first.
        implicit_reads(&root, u.file)?;
    }
    let network = u.labels.pr.map(|pr| crate::preview_network::name(u.labels.app_id, pr));
    let docker = u.docker.clone();
    let result = up_inner(u, cancel, log).await;
    if result.is_err() {
        if let Some(net) = network {
            if let Err(e) = crate::preview_network::remove_if_unused(&docker, &net).await {
                tracing::warn!("cleaning failed compose preview network: {e}");
            }
        }
    }
    result
}

async fn up_inner(u: Up<'_>, cancel: Cancel, log: &mut (dyn FnMut(&str, &str) + Send)) -> Result<Started> {
    let cfg = config(u.docker, u.dir, &[u.file], None, u.env).await?;
    check_build_inputs(&cfg, u.dir)?;
    if let Some(pr) = u.labels.pr {
        for (svc, s) in cfg["services"].as_object().into_iter().flatten() {
            check_preview_execution(svc, s)?;
        }
        crate::preview_network::ensure(u.docker, &crate::preview_network::name(u.labels.app_id, pr)).await?;
    }
    // The file's own order is lost in `config`.
    let selected = picked(&cfg, u.service).map_err(|e| Error::Invalid(format!("{}: {}", u.file, e.message())))?;
    let services = selected.services;
    let routed = selected.routed;
    let volumes = if u.labels.pr.is_some() {
        preview_volumes(&cfg)?
    } else {
        vec![]
    };
    log(
        "info",
        &format!("compose services: {} (routed: {routed})", services.join(", ")),
    );
    std::fs::create_dir_all(u.scratch)?;
    let work = tempfile::Builder::new().prefix("compose-").tempdir_in(u.scratch)?;
    let work_path = std::fs::canonicalize(work.path())?;
    let root = std::fs::canonicalize(u.dir)?;
    let env_path = work_path.join("app.env");
    let env: String = u.env.iter().map(|(k, v)| env_line(k, v)).collect();
    create_private(&env_path, env.as_bytes())?;
    let override_path = work_path.join("compose.override.yml");
    create_private(
        &override_path,
        bounded_override(&services, &routed, &env_path, &u.labels, u.project, &volumes, u.limits)?.as_bytes(),
    )?;
    let override_file = override_path.to_string_lossy();
    let merged = config(u.docker, u.dir, &[u.file, &override_file], Some(u.project), u.env).await?;
    // The bounded override owns the exact cgroup and isolated network.
    // Validate the remaining model without granting arbitrary host access.
    let mut policy_model = merged.clone();
    for service in policy_model["services"]
        .as_object_mut()
        .into_iter()
        .flat_map(|m| m.values_mut())
    {
        if u.limits
            .cgroup_parent
            .is_some_and(|parent| service["cgroup_parent"].as_str() == Some(parent))
        {
            service.as_object_mut().expect("service").remove("cgroup_parent");
        }
    }
    if let Some(pr) = u.labels.pr {
        let net = crate::preview_network::name(u.labels.app_id, pr);
        for (_, n) in policy_model["networks"].as_object_mut().into_iter().flatten() {
            if n["name"].as_str() == Some(net.as_str()) {
                n["external"] = Value::Bool(false);
            }
        }
    }
    check_model(&policy_model, &[&root, &work_path])?;
    if u.labels.pr.is_some() {
        if let Err(e) = (|| {
            let m = &merged;
            check_preview_storage(m, u.dir, u.project)?;
            check_preview_networks(
                m,
                &crate::preview_network::name(u.labels.app_id, u.labels.pr.expect("preview")),
            )
        })() {
            let _ = std::fs::remove_file(&env_path);
            return Err(e);
        }
        if !volumes.is_empty() {
            log("info", &format!("preview volumes: {} (its own)", volumes.join(", ")));
        }
    }
    if u.labels.pr.is_none() {
        u.docker.ensure_network(PROXY_NETWORK).await?;
    }
    let args = proc::args(&[
        "compose",
        "-p",
        u.project,
        "-f",
        u.file,
        "-f",
        &override_path.to_string_lossy(),
        "up",
        "--detach",
        if u.build { "--build" } else { "--no-build" },
        "--remove-orphans",
        "--wait",
        "--wait-timeout",
        &u.timeout.as_secs().to_string(),
    ]);
    let mut command_env = u.env.to_vec();
    if u.build {
        command_env.push(("BUILDX_BUILDER".into(), crate::build::BUILDER.into()));
    }
    let res = proc::stream_clean(
        Spec {
            bin: &u.docker.bin,
            args: &args,
            env: &command_env,
            cwd: Some(u.dir),
            timeout: u.timeout + Duration::from_secs(900),
        },
        cancel,
        log,
    )
    .await;
    let _ = std::fs::remove_file(&env_path);
    let out = res?;
    if !out.success() {
        return Err(Error::Invalid(format!("docker compose up failed:\n{}", out.tail(15))));
    }
    Ok(Started {
        container: container_of(u.project, &routed),
        port: selected.port,
    })
}

/// Remove the volumes a closed preview's override created (labelled with
/// its app and pull request; nothing production uses has those labels).
pub async fn remove_preview_volumes(docker: &Docker, app_id: &str, pr: u32) -> Result<()> {
    let args = proc::args(&[
        "volume",
        "ls",
        "--quiet",
        "--filter",
        &format!("label={PREVIEW_APP_LABEL}={app_id}"),
        "--filter",
        &format!("label={PREVIEW_PR_LABEL}={pr}"),
    ]);
    let o = docker.run(&args, Duration::from_secs(60)).await?;
    if !o.success() {
        return Err(Error::Internal(format!("docker volume ls: {}", o.tail(3))));
    }
    for v in o.stdout.lines().map(str::trim).filter(|v| !v.is_empty()) {
        docker.volume_remove(v).await?;
    }
    Ok(())
}

/// Stop and remove a compose project's containers (volumes are kept).
pub async fn down(docker: &Docker, project: &str) -> Result<()> {
    let args = proc::args(&["compose", "-p", project, "down", "--remove-orphans"]);
    let o = proc::run(Spec {
        bin: &docker.bin,
        args: &args,
        env: &[],
        cwd: None,
        timeout: Duration::from_secs(180),
    })
    .await?;
    if o.success() || o.tail(2).contains("no configuration file") {
        Ok(())
    } else {
        Err(Error::Internal(format!("docker compose down {project}: {}", o.tail(3))))
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn compose_does_not_inherit_the_agents_environment() {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            std::env::var("CARGO_MANIFEST_DIR").is_ok(),
            "run this regression with cargo test"
        );
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("docker");
        std::fs::write(&bin, "#!/bin/sh\nprintf '{\"services\":{\"web\":{\"environment\":{\"INHERITED\":\"%s\"}}}}' \"$CARGO_MANIFEST_DIR\"\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let docker = super::Docker {
            bin: bin.to_string_lossy().into_owned(),
        };
        let cfg = super::config(&docker, dir.path(), &["compose.yml"], None, &[])
            .await
            .unwrap();
        assert_eq!(cfg["services"]["web"]["environment"]["INHERITED"], "");
    }
    #[test]
    fn previews_reject_host_privileges_and_direct_ingress() {
        let dir = tempfile::tempdir().unwrap();
        for escape in [
            serde_json::json!({"privileged":true}),
            serde_json::json!({"network_mode":"host"}),
            serde_json::json!({"pid":"host"}),
            serde_json::json!({"cap_add":["NET_ADMIN"]}),
            serde_json::json!({"ports":[{"target":80,"published":"9000"}]}),
            serde_json::json!({"devices":["/dev/mem"]}),
            serde_json::json!({"external_links":["production-db"]}),
            serde_json::json!({"security_opt":["seccomp=unconfined"]}),
            serde_json::json!({"build":{"context":".","privileged":true}}),
        ] {
            let cfg = serde_json::json!({"services":{"web":escape}});
            assert!(
                super::check_preview_storage(&cfg, dir.path(), "preview").is_err(),
                "{cfg}"
            );
        }
    }
    use super::*;

    #[test]
    fn names_and_env() {
        assert_eq!(project_name("shop", None), "vfy-shop");
        assert_eq!(project_name("shop", Some(4)), "vfy-shop-pr4");
        assert_eq!(container_of("vfy-shop", "web"), "vfy-shop-web-1");
        assert_eq!(env_line("A", "plain $x"), "A='plain $x'\n");
        assert_eq!(env_line("B", "it's\nme \"q\""), "B=\"it's\\nme \\\"q\\\"\"\n");
    }

    #[test]
    fn compose_services_share_one_aggregate_budget() {
        let services = vec!["web".into(), "worker".into(), "db".into()];
        let labels = Labels {
            app_id: "app_1",
            app_name: "shop",
            deployment_id: "dpl_1",
            project_id: "prj_1",
            pr: None,
        };
        let yaml = bounded_override(
            &services,
            "web",
            Path::new("/app.env"),
            &labels,
            "vfy-shop",
            &[],
            Limits {
                memory_mb: 512,
                cpus: 1.0,
                cgroup_parent: Some("vercelify.slice"),
            },
        )
        .unwrap();
        let docs = yaml_rust2::YamlLoader::load_from_str(&yaml).unwrap();
        for name in services {
            let service = &docs[0]["services"][name.as_str()];
            assert_eq!(service["mem_limit"].as_str(), Some("170m"));
            assert_eq!(service["memswap_limit"].as_str(), Some("170m"));
            assert_eq!(service["deploy"]["replicas"].as_i64(), Some(1));
            assert_eq!(service["deploy"]["resources"]["limits"]["cpus"].as_str(), Some("0.333"));
            assert_eq!(service["cgroup_parent"].as_str(), Some("vercelify.slice"));
        }
    }

    #[test]
    fn override_file() {
        let y = override_yaml(
            &["web".into(), "worker".into()],
            "web",
            Path::new("/x/app.env"),
            &Labels {
                app_id: "app_1",
                app_name: "shop",
                deployment_id: "dpl_1",
                project_id: "prj_1",
                pr: None,
            },
            "vfy-shop",
            &[],
        );
        assert!(y.contains("  \"web\":\n    labels:\n      \"vercelify.managed\": \"platform\""));
        assert!(y.contains("\"vercelify.deployment\": \"dpl_1\""));
        assert!(y.contains("      - \"/x/app.env\""));
        let web = y.split("\"worker\":").next().unwrap();
        assert!(
            web.contains("\"vfy-proxy\": {}"),
            "the routed service joins the proxy network"
        );
        let worker = y
            .split("\"worker\":")
            .nth(1)
            .unwrap()
            .split("networks:\n  \"vfy-proxy\"")
            .next()
            .unwrap();
        assert!(!worker.contains("default: {}"), "other services keep their networks");
        assert!(y.ends_with("networks:\n  \"vfy-proxy\":\n    external: true\n"));
    }

    #[test]
    fn preview_storage() {
        let cfg = |j: &str| serde_json::from_str::<serde_json::Value>(j).unwrap();
        let checkout = tempfile::tempdir().unwrap();
        let dir = checkout.path();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.join("linked")).unwrap();
        let p = "vfy-shop-pr4";
        let file = cfg(
            r#"{"services":{"web":{}},"volumes":{"data":{"name":"production-data"},"ext":{"name":"ext","external":true}}}"#,
        );
        assert_eq!(preview_volumes(&file).unwrap(), ["data", "ext"]);
        let host_dir = cfg(r#"{"volumes":{"data":{"driver_opts":{"type":"none","o":"bind","device":"/srv"}}}}"#);
        assert!(preview_volumes(&host_dir).is_err());
        assert!(preview_volumes(&cfg(r#"{"volumes":{"d":{"driver":"nfs"}}}"#)).is_err());

        let y = override_yaml(
            &["web".into()],
            "web",
            Path::new("/x/app.env"),
            &Labels {
                app_id: "app_1",
                app_name: "shop",
                deployment_id: "dpl_1",
                project_id: "prj_1",
                pr: Some(4),
            },
            p,
            &["data".into()],
        );
        assert!(y.ends_with(
            "volumes:\n  \"data\":\n    name: \"vfy-shop-pr4_data\"\n    external: false\n    labels:\n      \
             \"vercelify.preview-app\": \"app_1\"\n      \"vercelify.preview-pr\": \"4\"\n"
        ));

        let merged = |vols: &str, mounts: &str| {
            cfg(&format!(
                r#"{{"services":{{"web":{{"volumes":[{mounts}]}}}},"volumes":{{{vols}}}}}"#
            ))
        };
        let bind = |src: &std::path::Path| format!(r#"{{"type":"bind","source":"{}","target":"/x"}}"#, src.display());
        let ok = merged(
            r#""data":{"name":"vfy-shop-pr4_data"}"#,
            &format!(
                r#"{{"type":"volume","source":"data","target":"/data"}},{{"type":"tmpfs","target":"/t"}},{},{}"#,
                bind(&dir.join("conf")),
                bind(&dir.join("made/by/docker"))
            ),
        );
        check_preview_storage(&ok, dir, p).unwrap();
        for bad in [
            merged(r#""data":{"name":"production-data"}"#, ""),
            merged(r#""data":{"name":"vfy-shop-pr4_data","external":true}"#, ""),
            merged(r#""data":{"name":"vfy-shop-pr40_data"}"#, ""),
            merged("", &bind(Path::new("/var/lib/vercelify"))),
            merged("", &bind(&dir.join("linked"))),
            merged("", &bind(&dir.join("linked/new"))),
            merged("", &bind(&dir.join("conf/../../x"))),
            merged("", r#"{"type":"npipe","source":"x","target":"/x"}"#),
            cfg(r#"{"services":{"web":{"volumes_from":["container:vfy-shop-web-1"]}}}"#),
        ] {
            let e = check_preview_storage(&bad, dir, p).unwrap_err();
            assert!(e.message().contains("must not share production"), "{bad}: {e}");
        }
        check_preview_storage(&cfg(r#"{"services":{"db":{"volumes_from":["web"]}}}"#), dir, p).unwrap();
    }

    #[test]
    fn builds_read_only_the_checkout() {
        let cfg = |j: String| serde_json::from_str::<serde_json::Value>(&j).unwrap();
        let checkout = tempfile::tempdir().unwrap();
        let dir = checkout.path();
        std::fs::create_dir_all(dir.join("web")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let out = outside.path().display();
        std::os::unix::fs::symlink(outside.path(), dir.join("linked")).unwrap();
        let d = dir.display();
        let build = |b: &str| {
            cfg(format!(
                r#"{{"services":{{"web":{{"build":{b}}},"db":{{"image":"postgres"}}}}}}"#
            ))
        };
        for ok in [
            format!(r#"{{"context":"{d}","dockerfile":"Dockerfile"}}"#),
            format!(r#"{{"context":"{d}/web","dockerfile":"../Dockerfile"}}"#),
            format!(r#"{{"context":"{d}","dockerfile_inline":"FROM alpine\nCOPY . /x"}}"#),
            r#"{"context":"https://github.com/me/app.git#main","dockerfile":"Dockerfile"}"#.to_string(),
            format!(
                r#"{{"context":"{d}","additional_contexts":{{"img":"docker-image://alpine","svc":"service:db","w":"{d}/web"}}}}"#
            ),
        ] {
            check_build_inputs(&build(&ok), dir).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            format!(r#"{{"context":"{out}","dockerfile_inline":"FROM alpine\nCOPY master.key /leak"}}"#),
            format!(r#"{{"context":"{d}/linked","dockerfile":"Dockerfile"}}"#),
            format!(r#"{{"context":"{d}","dockerfile":"{out}/Dockerfile"}}"#),
            format!(r#"{{"context":"{d}","dockerfile":"../outside/Dockerfile"}}"#),
            format!(r#"{{"context":"{d}","additional_contexts":{{"x":"{out}"}}}}"#),
            format!(r#"{{"context":"{d}","additional_contexts":{{"x":"oci-layout://{out}"}}}}"#),
        ] {
            let e = check_build_inputs(&build(&bad), dir).unwrap_err();
            assert!(e.message().contains("outside the repository"), "{bad}: {e}");
        }
    }

    #[test]
    fn previews_read_no_server_files() {
        let cfg = |j: String| serde_json::from_str::<serde_json::Value>(&j).unwrap();
        let checkout = tempfile::tempdir().unwrap();
        let dir = checkout.path();
        let d = dir.display();
        let p = "vfy-shop-pr4";
        let ok = cfg(format!(
            r#"{{"services":{{"web":{{"env_file":[{{"path":"{d}/.vercelify/app.env"}},"{d}/local.env"],
                "build":{{"context":"{d}","ssh":["default"]}}}}}},
              "secrets":{{"s":{{"file":"{d}/secret.txt"}},"e":{{"environment":"TOKEN"}}}},
              "configs":{{"c":{{"content":"x"}}}}}}"#
        ));
        check_preview_storage(&ok, dir, p).unwrap();
        for bad in [
            r#"{"services":{"web":{"env_file":[{"path":"/srv/prod/app.env","required":false}]}}}"#.to_string(),
            r#"{"services":{"web":{"env_file":["/srv/prod/app.env"]}}}"#.to_string(),
            r#"{"services":{"web":{"build":{"ssh":["k=/root/.ssh/id_ed25519"]}}}}"#.to_string(),
            r#"{"services":{},"secrets":{"s":{"file":"/var/lib/vercelify/master.key"}}}"#.to_string(),
            r#"{"services":{},"secrets":{"s":{"external":true}}}"#.to_string(),
            r#"{"services":{},"configs":{"c":{"file":"/etc/app.conf"}}}"#.to_string(),
        ] {
            let e = check_preview_storage(&cfg(bad.clone()), dir, p).unwrap_err();
            assert!(e.message().contains("must not read production's files"), "{bad}: {e}");
        }
        // A Compose that read the env files itself can't be checked.
        let e =
            check_preview_storage(&cfg(format!(r#"{{"services":{{}},"{ENV_FILES_READ}":true}}"#)), dir, p).unwrap_err();
        assert!(e.message().contains("update it"), "{e}");
    }

    /// What Compose would read besides the compose file, found in the files
    /// themselves: the review's `.env` symlink, `include` and `extends` out
    /// of the checkout, however they are spelled.
    #[test]
    fn preview_dotenv_cannot_redirect_host_tools() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("compose.yml"), "services:\n  web:\n    image: alpine\n").unwrap();
        for key in ["DOCKER_HOST", "BUILDX_BUILDER", "PATH", "LD_PRELOAD", "COMPOSE_FILE"] {
            std::fs::write(dir.path().join(".env"), format!("{key}=private-test-value\n")).unwrap();
            let e = implicit_reads(dir.path(), "compose.yml").unwrap_err();
            assert!(e.to_string().contains(key));
            assert!(!e.to_string().contains("private-test-value"));
        }
    }

    #[test]
    fn implicit_reads_stay_in_the_checkout() {
        use std::os::unix::fs::symlink;
        let server = tempfile::tempdir().unwrap();
        std::fs::write(server.path().join("prod.env"), "PRODUCTION_ONLY_KEY=leak\n").unwrap();
        std::fs::write(server.path().join("base.yml"), "services:\n  b:\n    image: alpine\n").unwrap();
        let s = server.path().display();
        let repo = |files: &[(&str, &str)]| {
            let d = tempfile::tempdir().unwrap();
            for (p, c) in files {
                let path = d.path().join(p);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, c).unwrap();
            }
            d
        };
        let web = "services:\n  web:\n    image: alpine\n    environment:\n      K: ${PRODUCTION_ONLY_KEY}\n";

        // Fine: a .env, included and extended files of the repository's own.
        let ok = repo(&[
            (".env", "A=1\n"),
            (
                "compose.yml",
                "include:\n  - lib/db.yml\n  - path: [lib/cache.yml]\n    env_file: lib/cache.env\n\
                 services:\n  web:\n    extends:\n      file: lib/base.yml\n      service: b\n  same:\n    extends: web\n",
            ),
            ("lib/db.yml", "services:\n  db:\n    image: postgres\n"),
            ("lib/cache.yml", "services:\n  cache:\n    image: redis\n"),
            ("lib/cache.env", "X=1\n"),
            ("lib/base.yml", "services:\n  b:\n    image: alpine\n"),
        ]);
        let reads = implicit_reads(ok.path(), "compose.yml").unwrap();
        assert!(
            reads.iter().any(|r| r.ends_with(".env")) && reads.iter().any(|r| r.ends_with("cache.env")),
            "{reads:?}"
        );

        // The reported case: `.env` links to production's env file.
        let linked = repo(&[("compose.yml", web)]);
        symlink(server.path().join("prod.env"), linked.path().join(".env")).unwrap();
        let e = implicit_reads(linked.path(), "compose.yml").unwrap_err();
        assert!(
            e.message().contains("prod.env") && e.message().contains("outside the repository"),
            "{e}"
        );
        // The same next to a compose file in a folder.
        let nested = repo(&[("apps/web/compose.yml", web)]);
        symlink(server.path().join("prod.env"), nested.path().join("apps/web/.env")).unwrap();
        assert!(implicit_reads(nested.path(), "apps/web/compose.yml").is_err());

        let to_server = "../".repeat(30) + &server.path().strip_prefix("/").unwrap().display().to_string();
        for (name, text) in [
            ("include", format!("include:\n  - {s}/base.yml\n{web}")),
            (
                "include, relative",
                format!("include:\n  - {to_server}/base.yml\n{web}"),
            ),
            (
                "include env_file",
                format!("include:\n  - path: lib.yml\n    env_file: {s}/prod.env\n{web}"),
            ),
            (
                "include project_directory",
                format!("include:\n  - path: lib.yml\n    project_directory: {s}\n{web}"),
            ),
            (
                "extends",
                format!("services:\n  web:\n    extends:\n      file: {s}/base.yml\n      service: b\n"),
            ),
            (
                "extends, flow",
                format!("services: {{web: {{extends: {{file: '{s}/base.yml', service: b}}}}}}\n"),
            ),
            (
                "extends, merge key",
                format!("x-e: &e\n  extends:\n    file: {s}/base.yml\n    service: b\nservices:\n  web:\n    <<: *e\n"),
            ),
            (
                "extends, escaped key",
                format!("services:\n  web:\n    \"ext\\x65nds\":\n      file: {s}/base.yml\n      service: b\n"),
            ),
            ("interpolated", "include:\n  - ${LIB}/x.yml\nservices: {}\n".to_string()),
            // The second review's case: a merge at the services level.
            (
                "services merged in",
                format!("x-s: &s\n  web:\n    extends: {{file: {s}/base.yml, service: b}}\nservices:\n  <<: *s\n"),
            ),
            (
                "services merged from a list",
                format!(
                    "x-a: &a\n  api: {{image: alpine}}\nx-b: &b\n  web:\n    extends: {{file: {s}/base.yml, service: b}}\n\
                     services:\n  <<: [*a, *b]\n"
                ),
            ),
            (
                "a merge in a merge",
                format!(
                    "x-e: &e {{extends: {{file: {s}/base.yml, service: b}}}}\nx-s: &s\n  web:\n    <<: *e\n\
                     services:\n  <<: *s\n"
                ),
            ),
            (
                "include merged at the top",
                format!("x-i: &i\n  include: [{s}/base.yml]\n<<: *i\nservices: {{}}\n"),
            ),
            (
                "an include entry merged",
                format!("x-i: &i {{path: {s}/base.yml}}\ninclude:\n  - <<: *i\nservices: {{}}\n"),
            ),
            (
                "extends in an extension field",
                format!("x-later:\n  extends: {{file: {s}/base.yml, service: b}}\nservices: {{}}\n"),
            ),
        ] {
            let d = repo(&[("compose.yml", &text), ("lib.yml", "services: {}\n")]);
            let e = implicit_reads(d.path(), "compose.yml").expect_err(name);
            assert!(
                e.message().contains("outside the repository") || e.message().contains("interpolated"),
                "{name}: {e}"
            );
        }
        // Deeper in: an included file includes the server's, or is a link to it.
        let deep = repo(&[
            ("compose.yml", "include:\n  - a.yml\n"),
            ("a.yml", &format!("include:\n  - {s}/base.yml\n")),
        ]);
        assert!(implicit_reads(deep.path(), "compose.yml").is_err());
        let link = repo(&[("compose.yml", "include:\n  - b.yml\n")]);
        symlink(server.path().join("base.yml"), link.path().join("b.yml")).unwrap();
        assert!(implicit_reads(link.path(), "compose.yml").is_err());
        // A merge whose own keys win: what it overrides isn't read.
        let own = repo(&[(
            "compose.yml",
            &format!("x-e: &e {{file: {s}/base.yml, service: b}}\nservices:\n  web:\n    extends:\n      <<: *e\n      file: lib.yml\n"),
        ), ("lib.yml", "services:\n  b:\n    image: alpine\n")]);
        assert!(implicit_reads(own.path(), "compose.yml").is_ok());
    }

    /// Aliases are copied by the loader: a few nested ones would be billions
    /// of nodes in the agent. Counted from the parser's events instead.
    #[test]
    fn alias_bombs_are_refused_before_loading() {
        let mut bomb = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
        for i in 1..9 {
            let prev = format!("*a{}", i - 1);
            bomb.push_str(&format!("a{i}: &a{i} [{}]\n", [prev.as_str(); 10].join(", ")));
        }
        bomb.push_str("services: {}\n");
        assert!(expanded_nodes(&bomb) > 1_000_000_000);
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("compose.yml"), &bomb).unwrap();
        let started = std::time::Instant::now();
        let e = implicit_reads(d.path(), "compose.yml").unwrap_err();
        assert!(e.message().contains("too large"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(2), "refused without expanding");
        // Anchors as compose files use them are fine.
        let usual = "x-env: &env {A: 1, B: 2}\nservices:\n  web: {image: alpine, environment: *env}\n  \
                     api: {image: alpine, environment: *env}\n";
        assert!(expanded_nodes(usual) < 50);
        assert_eq!(
            expanded_nodes("a: [1, 2]\nb: {c: 3}\n"),
            9,
            "maps and lists count with their items"
        );
    }

    /// A preview with a linked `.env` stops before Compose is run at all.
    #[tokio::test]
    async fn a_preview_checks_before_compose_reads_anything() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let d = tempfile::tempdir().unwrap();
        let server = tempfile::tempdir().unwrap();
        std::fs::write(server.path().join("prod.env"), "PRODUCTION_ONLY_KEY=leak\n").unwrap();
        let checkout = d.path().join("src");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(
            checkout.join("docker-compose.yml"),
            "services:\n  web:\n    image: alpine\n",
        )
        .unwrap();
        symlink(server.path().join("prod.env"), checkout.join(".env")).unwrap();
        let bin = d.path().join("docker");
        let calls = d.path().join("calls");
        std::fs::write(&bin, format!("#!/bin/sh\necho \"$*\" >> {}\nexit 1\n", calls.display())).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let docker = Docker {
            bin: bin.display().to_string(),
        };
        let e = up(
            Up {
                limits: Limits {
                    memory_mb: 256,
                    cpus: 1.0,
                    cgroup_parent: None,
                },
                build: false,
                docker: &docker,
                dir: &checkout,
                file: "docker-compose.yml",
                project: "vfy-shop-pr4",
                service: None,
                env: &[],
                labels: Labels {
                    app_id: "app_1",
                    app_name: "shop",
                    deployment_id: "dpl_1",
                    project_id: "prj_1",
                    pr: Some(4),
                },
                timeout: Duration::from_secs(10),
                scratch: d.path(),
            },
            Cancel::never(),
            &mut |_, _| {},
        )
        .await
        .unwrap_err();
        assert!(e.message().contains(".env"), "{e}");
        assert!(
            !calls.exists(),
            "docker was run: {}",
            std::fs::read_to_string(&calls).unwrap_or_default()
        );
    }

    /// The real Compose resolves our override the way the check expects:
    /// explicit, external and plain volumes all become the preview's own,
    /// and a bind mount from the server stops it (VERCELIFY_DOCKER_TESTS=1;
    /// `docker compose config` needs no daemon).
    #[tokio::test]
    #[ignore = "needs Docker Compose: VERCELIFY_DOCKER_TESTS=1 cargo test … -- --include-ignored"]
    async fn compose_resolves_preview_volumes() {
        if std::env::var("VERCELIFY_DOCKER_TESTS").as_deref() != Ok("1") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let file =
            "services:\n  web:\n    image: alpine:3.20\n    volumes:\n      - data:/data\n      - ext:/ext\n      \
                    - plain:/plain\n      - ./conf:/conf:ro\nvolumes:\n  data:\n    name: production-data\n  \
                    ext:\n    external: true\n  plain: {}\n";
        std::fs::write(dir.path().join("docker-compose.yml"), file).unwrap();
        let docker = Docker {
            bin: std::env::var("VERCELIFY_DOCKER_BIN").unwrap_or_else(|_| "docker".into()),
        };
        let p = "vfy-shop-pr4";
        let prod = config(&docker, dir.path(), &["docker-compose.yml"], Some("vfy-shop"), &[])
            .await
            .unwrap();
        assert_eq!(prod["volumes"]["data"]["name"], "production-data", "what the audit saw");
        let vols = preview_volumes(&prod).unwrap();
        let labels = Labels {
            app_id: "app_1",
            app_name: "shop",
            deployment_id: "dpl_1",
            project_id: "prj_1",
            pr: Some(4),
        };
        let env = dir.path().join("app.env");
        std::fs::write(&env, "").unwrap();
        let ov = dir.path().join("override.yml");
        std::fs::write(&ov, override_yaml(&["web".into()], "web", &env, &labels, p, &vols)).unwrap();
        let ov = ov.to_string_lossy().to_string();
        let merged = config(&docker, dir.path(), &["docker-compose.yml", &ov], Some(p), &[])
            .await
            .unwrap();
        for k in ["data", "ext", "plain"] {
            assert_eq!(merged["volumes"][k]["name"], format!("{p}_{k}"), "{merged:#}");
            assert!(merged["volumes"][k].get("external").is_none_or(|e| e == false));
        }
        check_preview_storage(&merged, dir.path(), p).unwrap();

        std::fs::write(
            dir.path().join("docker-compose.yml"),
            file.replace("./conf:/conf:ro", "/var/lib/vercelify:/state"),
        )
        .unwrap();
        let merged = config(&docker, dir.path(), &["docker-compose.yml", &ov], Some(p), &[])
            .await
            .unwrap();
        assert!(check_preview_storage(&merged, dir.path(), p).is_err());

        // Production's files on the server: an env file, a secret file, and
        // a build of a folder outside with an inline Dockerfile.
        let server = tempfile::tempdir().unwrap();
        std::fs::write(server.path().join("prod.env"), "SECRET=prod-secret\n").unwrap();
        std::fs::write(server.path().join("master.key"), "key").unwrap();
        let s = server.path().display();
        std::fs::write(
            dir.path().join("docker-compose.yml"),
            format!(
                "services:\n  web:\n    image: alpine:3.20\n    env_file: [{s}/prod.env]\n    secrets: [k]\n\
                 secrets:\n  k:\n    file: {s}/master.key\n"
            ),
        )
        .unwrap();
        let merged = config(&docker, dir.path(), &["docker-compose.yml", &ov], Some(p), &[])
            .await
            .unwrap();
        assert!(
            !merged.to_string().contains("prod-secret"),
            "config doesn't read env files: {merged}"
        );
        let e = check_preview_storage(&merged, dir.path(), p).unwrap_err();
        assert!(e.message().contains(".env"), "{e}");
        std::fs::write(
            dir.path().join("docker-compose.yml"),
            format!("services:\n  web:\n    image: alpine:3.20\n    secrets: [k]\nsecrets:\n  k:\n    file: {s}/master.key\n"),
        )
        .unwrap();
        let merged = config(&docker, dir.path(), &["docker-compose.yml"], Some(p), &[])
            .await
            .unwrap();
        let e = check_preview_storage(&merged, dir.path(), p).unwrap_err();
        assert!(e.message().contains("master.key"), "{e}");
        std::fs::write(
            dir.path().join("docker-compose.yml"),
            format!(
                "services:\n  web:\n    build:\n      context: {s}\n      dockerfile_inline: |\n        FROM alpine\n        \
                 COPY master.key /leak\n"
            ),
        )
        .unwrap();
        let built = config(&docker, dir.path(), &["docker-compose.yml"], Some("vfy-shop"), &[])
            .await
            .unwrap();
        let e = check_build_inputs(&built, dir.path()).unwrap_err();
        assert!(e.message().contains("the build context"), "{e}");

        // The review's case: `.env` links to production's env file, and the
        // file interpolates from it. Compose would read it (`config` shows
        // the value); `implicit_reads` refuses it first.
        std::fs::write(
            dir.path().join("docker-compose.yml"),
            "services:\n  web:\n    image: alpine:3.20\n    environment:\n      K: ${PRODUCTION_ONLY_KEY}\n",
        )
        .unwrap();
        std::fs::write(server.path().join("prod.env"), "PRODUCTION_ONLY_KEY=leak-dotenv\n").unwrap();
        std::os::unix::fs::symlink(server.path().join("prod.env"), dir.path().join(".env")).unwrap();
        let read = config(&docker, dir.path(), &["docker-compose.yml"], Some(p), &[])
            .await
            .unwrap();
        assert!(read.to_string().contains("leak-dotenv"), "what the review saw: {read}");
        let e = implicit_reads(dir.path(), "docker-compose.yml").unwrap_err();
        assert!(e.message().contains(".env"), "{e}");

        // The second review's case: services merged in, extending a file on
        // the server. Compose reads it; `implicit_reads` refuses it first.
        std::fs::remove_file(dir.path().join(".env")).unwrap();
        std::fs::write(
            server.path().join("prod.yml"),
            "services:\n  web:\n    image: alpine:3.20\n    environment:\n      SECRET: leak-extends\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("docker-compose.yml"),
            format!("x-s: &s\n  web:\n    extends: {{file: {s}/prod.yml, service: web}}\nservices:\n  <<: *s\n"),
        )
        .unwrap();
        let read = config(&docker, dir.path(), &["docker-compose.yml"], Some(p), &[])
            .await
            .unwrap();
        assert!(read.to_string().contains("leak-extends"), "what the review saw: {read}");
        let e = implicit_reads(dir.path(), "docker-compose.yml").unwrap_err();
        assert!(e.message().contains("prod.yml"), "{e}");
    }

    #[test]
    fn routed_service() {
        let cfg = |j: &str| serde_json::from_str::<serde_json::Value>(j).unwrap();
        let two = cfg(r#"{"services":{"worker":{},"web":{}}}"#);
        assert_eq!(pick(&two, None).unwrap().1, "web");
        let ports = cfg(r#"{"services":{"web":{},"api":{"ports":[{"target":80}]}}}"#);
        assert_eq!(pick(&ports, None).unwrap().1, "api", "the one with a port wins");
        let neither = cfg(r#"{"services":{"zeta":{},"alpha":{}}}"#);
        assert!(pick(&neither, None).is_err());
        assert_eq!(pick(&two, Some("worker")).unwrap().1, "worker");
        assert!(pick(&two, Some("db")).is_err());
        assert!(pick(&cfg(r#"{"services":{}}"#), None).is_err());
    }

    /// Real Compose (VERCELIFY_DOCKER_TESTS=1).
    #[tokio::test]
    #[ignore = "needs Docker: VERCELIFY_DOCKER_TESTS=1 cargo test … -- --include-ignored"]
    async fn compose_up_and_down() {
        if std::env::var("VERCELIFY_DOCKER_TESTS").as_deref() != Ok("1") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("docker-compose.yml"),
            "services:\n  web:\n    image: alpine:3.20\n    command: [\"sh\", \"-c\", \"echo $$GREETING > /tmp/g; sleep 300\"]\n  worker:\n    image: alpine:3.20\n    command: [\"sleep\", \"300\"]\n",
        )
        .unwrap();
        let docker = Docker::default();
        let mut log = |_: &str, _: &str| {};
        let env = vec![("GREETING".to_string(), "hi there".to_string())];
        let name = up(
            Up {
                limits: Limits {
                    memory_mb: 256,
                    cpus: 1.0,
                    cgroup_parent: None,
                },
                build: false,
                docker: &docker,
                dir: dir.path(),
                file: "docker-compose.yml",
                project: "vfy-composetest",
                service: None,
                env: &env,
                labels: Labels {
                    app_id: "app_t",
                    app_name: "composetest",
                    deployment_id: "dpl_t",
                    project_id: "prj_t",
                    pr: None,
                },
                timeout: Duration::from_secs(120),
                scratch: dir.path(),
            },
            Cancel::never(),
            &mut log,
        )
        .await
        .unwrap()
        .container;
        assert_eq!(name, "vfy-composetest-web-1");
        let i = docker.inspect(&name).await.unwrap().unwrap();
        assert!(i.running && i.ips.contains_key(PROXY_NETWORK));
        let o = docker
            .exec(&name, &proc::args(&["cat", "/tmp/g"]), Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(o.stdout.trim(), "hi there");
        assert!(
            !dir.path().join(".vercelify/app.env").exists(),
            "variables not left on disk"
        );
        let managed = docker.list_managed().await.unwrap();
        assert_eq!(
            managed
                .iter()
                .filter(|c| c.name.starts_with("vfy-composetest-"))
                .count(),
            2
        );
        down(&docker, "vfy-composetest").await.unwrap();
        assert!(docker.inspect(&name).await.unwrap().is_none());
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn names_and_env() {
        assert_eq!(project_name("shop", None), "vfy-shop");
        assert_eq!(project_name("shop", Some(4)), "vfy-shop-pr4");
        assert_eq!(container_of("vfy-shop", "web"), "vfy-shop-web-1");
        assert_eq!(env_line("A", "plain $x"), "A='plain $x'\n");
        assert_eq!(env_line("B", "it's\nme \"q\""), "B=\"it's\\nme \\\"q\\\"\"\n");
        assert_eq!(env_line("C", "it's ${X}"), "C=\"it's \\${X}\"\n");
    }

    #[test]
    fn override_file() {
        let y = override_yaml(
            &["web".into(), "worker".into()],
            "web",
            Path::new("/x/app.env"),
            &Labels {
                app_id: "app_1",
                app_name: "shop",
                deployment_id: "dpl_1",
                project_id: "prj_1",
                pr: None,
            },
            "vfy-shop",
            &[],
        );
        assert!(y.contains("  \"web\":\n    labels:\n      \"vercelify.managed\": \"platform\""));
        assert!(y.contains("\"vercelify.deployment\": \"dpl_1\""));
        assert!(y.contains("      - \"/x/app.env\""));
        assert_eq!(y.matches("    ports: !reset []\n").count(), 2, "no host ports");
        let web = y.split("\"worker\":").next().unwrap();
        assert!(
            web.contains("\"vfy-proxy\": {}"),
            "the routed service joins the proxy network"
        );
        let worker = y
            .split("\"worker\":")
            .nth(1)
            .unwrap()
            .split("networks:\n  \"vfy-proxy\"")
            .next()
            .unwrap();
        assert!(!worker.contains("default: {}"), "other services keep their networks");
        assert!(y.ends_with("networks:\n  \"vfy-proxy\":\n    external: true\n"));
    }

    #[test]
    fn routed_service() {
        let cfg = |j: &str| serde_json::from_str::<serde_json::Value>(j).unwrap();
        let two = cfg(r#"{"services":{"worker":{},"web":{}}}"#);
        assert_eq!(picked(&two, None).unwrap().routed, "web");
        let ports = cfg(r#"{"services":{"web":{},"api":{"ports":[{"target":80}]}}}"#);
        let p = picked(&ports, None).unwrap();
        assert_eq!(
            (p.routed.as_str(), p.port),
            ("api", Some(80)),
            "the one with a port wins"
        );
        let one = cfg(r#"{"services":{"site":{}}}"#);
        assert_eq!(picked(&one, None).unwrap().routed, "site");
        assert_eq!(picked(&two, Some("worker")).unwrap().routed, "worker");
        assert!(picked(&two, Some("db")).is_err());
        assert!(picked(&cfg(r#"{"services":{}}"#), None).is_err());
        // Several plausible services: the app has to say which.
        let neither = cfg(r#"{"services":{"zeta":{},"alpha":{}}}"#);
        let e = picked(&neither, None).unwrap_err();
        assert!(e.message().contains("alpha, zeta"), "{}", e.message());
    }

    /// Regression (audit A13): a database's port never makes it the website.
    #[test]
    fn routed_service_is_not_the_database() {
        let c = json!({"services":{
            "db":{"image":"postgres:17","expose":["5432"]},
            "web":{"build":{"context":"/r"},"expose":["3000"]}}});
        let p = picked(&c, None).unwrap();
        assert_eq!((p.routed.as_str(), p.port), ("web", Some(3000)));
        // A database known by its image, whatever port it says.
        let c = json!({"services":{
            "cache":{"image":"docker.io/library/redis:7","expose":["7000"]},
            "site":{"image":"nginx","ports":[{"target":8080,"published":"8080"}]}}});
        assert_eq!(picked(&c, None).unwrap().routed, "site");
        // Two web services with ports and no name to go by: ask.
        let c = json!({"services":{
            "api":{"expose":["4000"]},"ui":{"expose":["3000"]},"db":{"image":"mysql","expose":["3306"]}}});
        let e = picked(&c, None).unwrap_err();
        assert!(
            e.message().contains("api, ui") && !e.message().contains("db"),
            "{}",
            e.message()
        );
        // Only a database: still nothing to guess.
        let c = json!({"services":{"db":{"image":"postgres"},"cache":{"image":"redis"}}});
        assert!(picked(&c, None).is_err());
        // Several ports: none is guessed.
        let c = json!({"services":{"web":{"expose":["3000","9229"]}}});
        assert_eq!(picked(&c, None).unwrap().port, None);
    }

    /// Regression (audit A02, A04): what the resolved model may ask for.
    #[test]
    fn model_policy() {
        let root = Path::new("/srv/checkout");
        let check = |svc: serde_json::Value| check_model(&json!({"services":{"web":svc}}), &[root]);
        let denied = [
            json!({"privileged": true}),
            json!({"cap_add": ["SYS_ADMIN"]}),
            json!({"cap_add": ["ALL"]}),
            json!({"devices": [{"source":"/dev/sda","target":"/dev/sda"}]}),
            json!({"network_mode": "host"}),
            json!({"network_mode": "container:vercelify-proxy"}),
            json!({"pid": "host"}),
            json!({"ipc": "host"}),
            json!({"uts": "host"}),
            json!({"userns_mode": "host"}),
            json!({"cgroup": "host"}),
            json!({"cgroup_parent": "/system.slice"}),
            json!({"security_opt": ["seccomp=unconfined"]}),
            json!({"security_opt": ["apparmor=unconfined"]}),
            json!({"use_api_socket": true}),
            json!({"provider": {"type": "model"}}),
            json!({"ports": [{"target": 22, "published": "22"}]}),
            json!({"volumes": [{"type":"bind","source":"/var/run/docker.sock","target":"/var/run/docker.sock","read_only":true}]}),
            json!({"volumes": [{"type":"bind","source":"/","target":"/host"}]}),
            json!({"volumes": [{"type":"bind","source":"/srv/checkout/persist","target":"/data"}]}),
            json!({"volumes": [{"type":"bind","source":"/srv/checkout/../state","target":"/s","read_only":true}]}),
            json!({"volumes": [{"type":"volume","source":"other","target":"/d"}]}),
            json!({"volumes_from": ["container:vfy-db-1"]}),
            json!({"env_file": [{"path":"/var/lib/vercelify/state/agent.env","required":true}]}),
            json!({"build": {"context": "/etc"}}),
            json!({"build": {"context": "/srv/checkout", "dockerfile": "/etc/passwd"}}),
            json!({"build": {"context": "/srv/checkout", "ssh": ["default"]}}),
            json!({"build": {"context": "/srv/checkout", "network": "host"}}),
            json!({"build": {"context": "/srv/checkout", "additional_contexts": {"x": "/root"}}}),
        ];
        for svc in denied {
            assert!(check(svc.clone()).is_err(), "allowed: {svc}");
        }
        let allowed = [
            json!({"image": "nginx", "expose": ["80"]}),
            json!({"cap_add": ["NET_BIND_SERVICE", "cap_chown"], "security_opt": ["no-new-privileges:true"]}),
            json!({"network_mode": "service:api", "pid": "service:api"}),
            json!({"volumes": [{"type":"bind","source":"/srv/checkout/nginx.conf","target":"/etc/nginx.conf","read_only":true}]}),
            json!({"volumes": [{"type":"tmpfs","target":"/tmp"}]}),
            json!({"env_file": [{"path":"/srv/checkout/.env.production","required":true}]}),
            json!({"build": {"context": "/srv/checkout/web", "dockerfile": "Dockerfile"}}),
            json!({"build": {"context": "https://github.com/a/b.git#main"}}),
        ];
        for svc in allowed {
            check(svc.clone()).unwrap_or_else(|e| panic!("{svc}: {e}"));
        }
        let top = |extra: serde_json::Value| {
            let mut m = json!({"services":{"web":{"volumes":[{"type":"volume","source":"data","target":"/d"}]}},"volumes":{"data":{}}});
            for (k, v) in extra.as_object().unwrap() {
                m[k] = v.clone();
            }
            check_model(&m, &[root])
        };
        top(json!({})).unwrap();
        top(json!({"networks": {"vfy-proxy": {"name": "vfy-proxy", "external": true}}})).unwrap();
        for bad in [
            json!({"volumes": {"data": {"external": true, "name": "vfy-db-data"}}}),
            json!({"volumes": {"data": {"driver_opts": {"type": "none", "o": "bind", "device": "/etc"}}}}),
            json!({"networks": {"x": {"name": "vfy-db-net", "external": true}}}),
            json!({"networks": {"x": {"driver": "host"}}}),
            json!({"secrets": {"s": {"file": "/etc/shadow"}}}),
            json!({"configs": {"c": {"file": "/srv/checkout/../secret"}}}),
        ] {
            assert!(top(bad.clone()).is_err(), "allowed: {bad}");
        }
    }

    #[test]
    fn saved_files_are_prechecked() {
        for t in crate::templates::TEMPLATES {
            precheck(t.compose).unwrap_or_else(|e| panic!("{}: {e}", t.id));
        }
        for bad in [
            "services:\n  a:\n    image: x\n    privileged: true\n",
            "services:\n  a:\n    network_mode: \"host\"\n",
            "services:\n  a:\n    volumes:\n      - /var/run/docker.sock:/var/run/docker.sock\n",
            "services:\n  a:\n    devices:\n      - /dev/sda\n",
        ] {
            assert!(precheck(bad).is_err(), "{bad}");
        }
        precheck("services:\n  a:\n    image: x # privileged: true\n    environment:\n      PID: host\n").unwrap();
    }

    /// Regression (audit A01): links out of the checkout are refused.
    #[test]
    fn tree_links() {
        let t = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(t.path()).unwrap().join("repo");
        std::fs::create_dir_all(root.join("conf")).unwrap();
        std::fs::write(root.join("conf/a"), "a").unwrap();
        symlink("conf/a", root.join("inside")).unwrap();
        check_tree(&root).unwrap();
        symlink("/etc/hosts", root.join("conf/hosts")).unwrap();
        let e = check_tree(&root).unwrap_err();
        assert!(e.message().contains("conf/hosts"), "{}", e.message());
        std::fs::remove_file(root.join("conf/hosts")).unwrap();
        symlink("../../outside", root.join("conf/up")).unwrap();
        assert!(check_tree(&root).is_err());
        std::fs::remove_file(root.join("conf/up")).unwrap();
        symlink("missing", root.join("dangling")).unwrap();
        assert!(check_tree(&root).is_err());
    }

    /// A `docker` that answers `compose config` with `config` and records
    /// everything else.
    fn stub_docker(dir: &Path, config: &str) -> Docker {
        let bin = dir.join("docker");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"$*\" >> {}\ncase \"$*\" in *' config '*) cat <<'EOF'\n{config}\nEOF\n;; esac\nexit 0\n",
                dir.join("calls").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Docker {
            bin: bin.to_string_lossy().into(),
        }
    }

    fn labels() -> Labels<'static> {
        Labels {
            app_id: "app_t",
            app_name: "t",
            deployment_id: "dpl_t",
            project_id: "prj_t",
            pr: None,
        }
    }

    async fn run_up(docker: &Docker, dir: &Path, scratch: &Path, env: &[(String, String)]) -> Result<Started> {
        up(
            Up {
                docker,
                dir,
                file: "docker-compose.yml",
                project: "vfy-t",
                service: None,
                env,
                labels: labels(),
                timeout: Duration::from_secs(5),
                scratch,
                build: false,
                limits: Limits {
                    memory_mb: 256,
                    cpus: 1.0,
                    cgroup_parent: None,
                },
            },
            Cancel::never(),
            &mut |_, _| {},
        )
        .await
    }

    /// Regression (audit A01): the repository can't point our writes (the
    /// variables file, the override) at the server's files.
    #[tokio::test]
    async fn generated_files_stay_out_of_the_checkout() {
        let t = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(t.path()).unwrap();
        let repo = base.join("repo");
        let scratch = base.join("scratch");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("docker-compose.yml"),
            "services:\n  web:\n    image: alpine\n",
        )
        .unwrap();
        let victim = base.join("host-file");
        std::fs::write(&victim, "host data").unwrap();
        let docker = stub_docker(&base, r#"{"services":{"web":{"image":"alpine"}}}"#);
        let env = vec![("SECRET".to_string(), "s3cret".to_string())];

        // `.vercelify/app.env` (the old place) linked to a host file.
        std::fs::create_dir(repo.join(".vercelify")).unwrap();
        symlink(&victim, repo.join(".vercelify/app.env")).unwrap();
        let e = run_up(&docker, &repo, &scratch, &env).await.unwrap_err();
        assert!(e.message().contains(".vercelify/app.env"), "{}", e.message());
        // `.vercelify` itself a link out.
        std::fs::remove_dir_all(repo.join(".vercelify")).unwrap();
        symlink(&base, repo.join(".vercelify")).unwrap();
        assert!(run_up(&docker, &repo, &scratch, &env).await.is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "host data");
        assert!(!base.join("app.env").exists() && !base.join("compose.override.yml").exists());
        assert!(!std::fs::read_to_string(base.join("calls"))
            .unwrap_or_default()
            .contains(" up "));

        // A clean checkout: nothing is written into it, nothing is left.
        std::fs::remove_file(repo.join(".vercelify")).unwrap();
        let s = run_up(&docker, &repo, &scratch, &env).await.unwrap();
        assert_eq!(s.container, "vfy-t-web-1");
        let names: Vec<_> = std::fs::read_dir(&repo)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("docker-compose.yml")]);
        assert_eq!(
            std::fs::read_dir(&scratch).unwrap().count(),
            0,
            "variables not left on disk"
        );
        let calls = std::fs::read_to_string(base.join("calls")).unwrap();
        assert!(
            calls.contains("--no-env-resolution") && calls.contains(" up "),
            "{calls}"
        );
    }

    /// Regression (audit A02): a refused model never reaches `compose up`.
    #[tokio::test]
    async fn refused_model_is_not_started() {
        let t = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(t.path()).unwrap();
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("docker-compose.yml"), "services: {}\n").unwrap();
        let docker = stub_docker(
            &base,
            r#"{"services":{"web":{"image":"alpine","privileged":true,"volumes":[{"type":"bind","source":"/var/run/docker.sock","target":"/s"}]}}}"#,
        );
        let e = run_up(&docker, &repo, &base.join("scratch"), &[]).await.unwrap_err();
        assert!(
            e.message().contains("privileged") && e.message().contains("docker.sock"),
            "{}",
            e.message()
        );
        assert!(!std::fs::read_to_string(base.join("calls")).unwrap().contains(" up "));
    }

    /// The real Compose CLI (`docker compose` or `docker-compose`), when
    /// installed. Only `config` runs: no daemon, no containers.
    fn real_compose(dir: &Path) -> Option<Docker> {
        let ok = |cmd: &str, args: &[&str]| {
            std::process::Command::new(cmd)
                .args(args)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if ok("docker", &["compose", "version"]) {
            return Some(Docker::default());
        }
        if !ok("docker-compose", &["version"]) {
            eprintln!("skipped: no Docker Compose");
            return None;
        }
        let bin = dir.join("docker-compose-shim");
        std::fs::write(
            &bin,
            "#!/bin/sh\nif [ \"$1\" = compose ]; then shift; exec docker-compose \"$@\"; fi\nexec docker \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Some(Docker {
            bin: bin.to_string_lossy().into(),
        })
    }

    /// Regression (audit A10): what the user saved is what the container
    /// gets, with real Compose parsing the env file.
    #[tokio::test]
    async fn env_values_stay_literal_in_real_compose() {
        let t = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(t.path()).unwrap();
        let Some(docker) = real_compose(&dir) else { return };
        let values = [
            "it's ${AUDIT_EXPANSION}",
            "plain $AUDIT_EXPANSION ${AUDIT_EXPANSION:-x}",
            "two\nlines $AUDIT_EXPANSION",
            "quote \" and back\\slash and 'single'",
            "trailing backslash \\",
            "dollars $$ and \\$ and \\n literal",
            "crlf\r\nend",
        ];
        let env: String = values
            .iter()
            .enumerate()
            .map(|(i, v)| env_line(&format!("V{i}"), v))
            .collect();
        std::fs::write(dir.join("app.env"), env).unwrap();
        std::fs::write(
            dir.join("docker-compose.yml"),
            "services:\n  web:\n    image: alpine\n    env_file: [app.env]\n",
        )
        .unwrap();
        let host_env = [("AUDIT_EXPANSION".to_string(), "changed-by-host".to_string())];
        let args = proc::args(&["compose", "-f", "docker-compose.yml", "config", "--format", "json"]);
        let out = proc::run_clean(Spec {
            bin: &docker.bin,
            args: &args,
            env: &host_env,
            cwd: Some(&dir),
            timeout: Duration::from_secs(60),
        })
        .await
        .unwrap();
        assert!(out.success(), "{}", out.tail(4));
        let v: Value = serde_json::from_str(&out.stdout).unwrap();
        for (i, want) in values.iter().enumerate() {
            // `config` prints a model: a literal `$` comes out as `$$`.
            let got = v["services"]["web"]["environment"][format!("V{i}")]
                .as_str()
                .unwrap_or_else(|| panic!("V{i} missing: {v}"))
                .replace("$$", "$");
            assert_eq!(&got, want, "V{i}");
        }
    }

    /// Regression (audit A02, A04): the policy holds on the model real
    /// Compose resolves from the repository's file plus our override.
    #[tokio::test]
    async fn real_compose_model_is_checked() {
        let t = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(t.path()).unwrap();
        let Some(docker) = real_compose(&base) else { return };
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("conf")).unwrap();
        std::fs::write(repo.join("conf/site.conf"), "x").unwrap();
        let env_file = base.join("app.env");
        std::fs::write(&env_file, "").unwrap();
        let ov = override_yaml(&["web".into(), "db".into()], "web", &env_file, &labels(), "vfy-t", &[]);
        std::fs::write(base.join("override.yml"), ov).unwrap();
        let ov = base.join("override.yml").to_string_lossy().into_owned();
        let merged = |compose: &str| {
            std::fs::write(repo.join("docker-compose.yml"), compose).unwrap();
            let (docker, repo, ov) = (docker.clone(), repo.clone(), ov.clone());
            async move {
                config(&docker, &repo, &["docker-compose.yml", &ov], None, &[])
                    .await
                    .unwrap()
            }
        };
        let safe = merged(
            "services:\n  web:\n    image: nginx\n    ports: [\"8080:80\"]\n    volumes: [\"./conf/site.conf:/etc/nginx/conf.d/site.conf:ro\"]\n  db:\n    image: postgres:17\n    volumes: [\"data:/var/lib/postgresql/data\"]\nvolumes:\n  data: {}\n",
        )
        .await;
        assert!(
            safe["services"]["web"].get("ports").is_none(),
            "host ports reset: {safe}"
        );
        check_model(&safe, &[&repo, &base]).unwrap();
        let unsafe_ = merged(
            "services:\n  web:\n    image: alpine\n    privileged: true\n    volumes: [\"./persist:/data\", \"/var/run/docker.sock:/var/run/docker.sock\"]\n    env_file: [/etc/hosts]\n  db:\n    image: alpine\n    pid: host\n",
        )
        .await;
        let e = check_model(&unsafe_, &[&repo, &base]).unwrap_err();
        for want in [
            "privileged",
            "writable bind mount at /data",
            "docker.sock",
            "/etc/hosts",
            "pid: host",
        ] {
            assert!(e.message().contains(want), "{want}: {}", e.message());
        }
    }
}

#[cfg(test)]
mod build_detection_limits_tests {
    use super::*;
    #[test]
    fn build_detection_refuses_oversized_yaml_before_loading() {
        let text = format!("#{}\nservices: {{}}\n", "x".repeat(MAX_COMPOSE_TEXT));
        assert!(requires_build(&text).is_err());
    }
}

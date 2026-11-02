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
use crate::{Error, Result};
use std::fmt::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
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
    if !v.contains('\'') && !v.contains('\n') && !v.contains('\r') {
        format!("{k}='{v}'\n")
    } else {
        let e = v
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
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
        y.push_str("    restart: unless-stopped\n");
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

pub struct Up<'a> {
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
    let mut o = run(args.clone()).await?;
    let mut env_files_read = false;
    if !o.success() && o.tail(4).contains("unknown flag") {
        // Compose older than 2.24: env files are read (and inlined) then.
        args.retain(|a| a != NO_ENV_RESOLUTION);
        o = run(args).await?;
        env_files_read = true;
    }
    if !o.success() {
        return Err(Error::Invalid(format!(
            "{file} is not a valid compose file: {}",
            o.tail(4)
        )));
    }
    let mut v: serde_json::Value =
        serde_json::from_str(&o.stdout).map_err(|e| Error::Invalid(format!("docker compose config: {e}")))?;
    if env_files_read {
        v[ENV_FILES_READ] = serde_json::Value::Bool(true);
    }
    Ok(v)
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

/// Which service gets the domains: the app's choice; else one that publishes
/// or exposes a port; else one called web/app/server/frontend; else the first
/// by name.
pub fn pick(config: &serde_json::Value, explicit: Option<&str>) -> Result<(Vec<String>, String)> {
    let services = config
        .get("services")
        .and_then(|s| s.as_object())
        .ok_or_else(|| Error::Invalid("no services".into()))?;
    let mut names: Vec<String> = services.keys().cloned().collect();
    names.sort();
    if names.is_empty() {
        return Err(Error::Invalid("no services".into()));
    }
    if let Some(e) = explicit {
        return if names.iter().any(|n| n == e) {
            Ok((names.clone(), e.to_string()))
        } else {
            Err(Error::Invalid(format!(
                "{e} is not one of its services ({})",
                names.join(", ")
            )))
        };
    }
    let has_port = |n: &String| {
        let s = &services[n];
        ["ports", "expose"]
            .iter()
            .any(|k| s.get(*k).and_then(|p| p.as_array()).is_some_and(|a| !a.is_empty()))
    };
    let routed = names
        .iter()
        .find(|n| has_port(n))
        .or_else(|| {
            ["web", "app", "server", "frontend"]
                .iter()
                .find_map(|w| names.iter().find(|n| n.as_str() == *w))
        })
        .unwrap_or(&names[0])
        .clone();
    Ok((names, routed))
}

/// Build and start the project; returns the routed container's name.
pub async fn up(u: Up<'_>, cancel: Cancel, log: &mut (dyn FnMut(&str, &str) + Send)) -> Result<String> {
    if u.labels.pr.is_some() {
        // Reject implicit server-file reads before even consulting Docker
        // for cleanup; no external tool may resolve this source first.
        implicit_reads(u.dir, u.file)?;
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

async fn up_inner(u: Up<'_>, cancel: Cancel, log: &mut (dyn FnMut(&str, &str) + Send)) -> Result<String> {
    let cfg = config(u.docker, u.dir, &[u.file], None, u.env).await?;
    check_build_inputs(&cfg, u.dir)?;
    if let Some(pr) = u.labels.pr {
        for (svc, s) in cfg["services"].as_object().into_iter().flatten() {
            check_preview_execution(svc, s)?;
        }
        crate::preview_network::ensure(u.docker, &crate::preview_network::name(u.labels.app_id, pr)).await?;
    }
    // The file's own order is lost in `config`.
    let (services, routed) =
        pick(&cfg, u.service).map_err(|e| Error::Invalid(format!("{}: {}", u.file, e.message())))?;
    let volumes = if u.labels.pr.is_some() {
        preview_volumes(&cfg)?
    } else {
        vec![]
    };
    log(
        "info",
        &format!("compose services: {} (routed: {routed})", services.join(", ")),
    );
    let work = u.dir.join(".vercelify");
    std::fs::create_dir_all(&work)?;
    let env_path = work.join("app.env");
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&env_path)?;
        for (k, v) in u.env {
            f.write_all(env_line(k, v).as_bytes())?;
        }
    }
    let override_path = work.join("compose.override.yml");
    std::fs::write(
        &override_path,
        override_yaml(&services, &routed, &env_path, &u.labels, u.project, &volumes),
    )?;
    if u.labels.pr.is_some() {
        let override_file = override_path.to_string_lossy();
        let merged = config(u.docker, u.dir, &[u.file, &override_file], Some(u.project), u.env).await;
        if let Err(e) = merged.and_then(|m| {
            check_preview_storage(&m, u.dir, u.project)?;
            check_preview_networks(
                &m,
                &crate::preview_network::name(u.labels.app_id, u.labels.pr.expect("preview")),
            )
        }) {
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
        "--build",
        "--remove-orphans",
        "--wait",
        "--wait-timeout",
        &u.timeout.as_secs().to_string(),
    ]);
    let res = proc::stream_clean(
        Spec {
            bin: &u.docker.bin,
            args: &args,
            env: u.env,
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
    Ok(container_of(u.project, &routed))
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
            },
            Cancel::never(),
            &mut |_, _| {},
        )
        .await
        .unwrap_err();
        assert!(e.message().contains("prod.env"), "{e}");
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
        assert!(e.message().contains("prod.env"), "{e}");
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
        assert!(e.message().contains("prod.env"), "{e}");

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
        assert_eq!(
            pick(&neither, None).unwrap(),
            (vec!["alpha".to_string(), "zeta".to_string()], "alpha".to_string())
        );
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
            },
            Cancel::never(),
            &mut log,
        )
        .await
        .unwrap();
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

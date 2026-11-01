//! Docker Compose apps (design §7.2, last paragraph).
//!
//! The repository's compose file runs as project `vfy-<app>` (previews:
//! `vfy-<app>-pr<n>`) with an override file of ours that adds our labels,
//! the restart policy and the variables to every service, and puts the routed
//! service on the proxy network. Variables reach Compose through an env file
//! that exists only while `docker compose up` runs. Compose recreates
//! containers in place, so zero downtime is best effort here.

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
/// interpolation) but can't hold a quote, and a backslash before the closing
/// quote escapes it; other values use double quotes, where Compose
/// interpolates, so `$` is escaped too.
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

/// Ours, merged over the repository's file: labels, variables, restart
/// policy, the proxy network for the routed service, and no published host
/// ports (the proxy is the way in).
pub fn override_yaml(services: &[String], routed: &str, env_file: &Path, l: &Labels) -> String {
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
        y.push_str("    ports: !reset []\n");
        if s == routed {
            let _ = writeln!(
                y,
                "    networks:\n      default: {{}}\n      {}: {{}}",
                q(PROXY_NETWORK)
            );
        }
    }
    let _ = writeln!(y, "networks:\n  {}:\n    external: true", q(PROXY_NETWORK));
    y
}

pub struct Up<'a> {
    pub docker: &'a Docker,
    /// The checkout (repository root). It must outlive the project:
    /// read-only bind mounts and `configs` come from it.
    pub dir: &'a Path,
    pub file: &'a str,
    pub project: &'a str,
    /// The service the domains route to (default: see [`pick`]).
    pub service: Option<&'a str>,
    pub env: &'a [(String, String)],
    pub labels: Labels<'a>,
    pub timeout: Duration,
    /// A directory of ours, outside the checkout, for the generated files.
    pub scratch: &'a Path,
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

async fn config(
    docker: &Docker,
    dir: &Path,
    files: &[&str],
    env: &[(String, String)],
    extra: &[&str],
) -> Result<Value> {
    let mut args = proc::args(&["compose"]);
    for f in files {
        args.extend(proc::args(&["-f", f]));
    }
    args.extend(proc::args(&["config", "--format", "json"]));
    args.extend(proc::args(extra));
    let o = proc::run(Spec {
        bin: &docker.bin,
        args: &args,
        env,
        cwd: Some(dir),
        timeout: Duration::from_secs(60),
    })
    .await?;
    if !o.success() {
        return Err(Error::Invalid(format!(
            "{} is not a valid compose file: {}",
            files[0],
            o.tail(4)
        )));
    }
    serde_json::from_str(&o.stdout).map_err(|e| Error::Invalid(format!("docker compose config: {e}")))
}

/// The services of a compose file and the one to route to, from
/// `docker compose config --format json` (the file's own order is lost there).
pub async fn services(
    docker: &Docker,
    dir: &Path,
    file: &str,
    env: &[(String, String)],
    explicit: Option<&str>,
) -> Result<Picked> {
    let v = config(docker, dir, &[file], env, &[]).await?;
    pick(&v, explicit).map_err(|e| Error::Invalid(format!("{file}: {}", e.message())))
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
pub fn pick(config: &Value, explicit: Option<&str>) -> Result<Picked> {
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

/// Check, build and start the project; returns the routed container.
pub async fn up(u: Up<'_>, cancel: Cancel, log: &mut (dyn FnMut(&str, &str) + Send)) -> Result<Started> {
    validate::repo_path("compose file", u.file)?;
    let root = std::fs::canonicalize(u.dir)?;
    check_tree(&root)?;
    let picked = services(u.docker, &root, u.file, u.env, u.service).await?;
    let routed = picked.routed.clone();
    log(
        "info",
        &format!("compose services: {} (routed: {routed})", picked.services.join(", ")),
    );
    // Generated files live in a fresh private directory of ours, never in
    // the repository's tree; it goes away with this function.
    std::fs::create_dir_all(u.scratch)?;
    let work = tempfile::Builder::new().prefix("compose-").tempdir_in(u.scratch)?;
    let work_path = std::fs::canonicalize(work.path())?;
    let env_path = work_path.join("app.env");
    let env: String = u.env.iter().map(|(k, v)| env_line(k, v)).collect();
    create_private(&env_path, env.as_bytes())?;
    let override_path = work_path.join("compose.override.yml");
    create_private(
        &override_path,
        override_yaml(&picked.services, &routed, &env_path, &u.labels).as_bytes(),
    )?;
    let override_s = override_path.to_string_lossy().into_owned();
    // Env files stay listed (unresolved) so where they come from is checked.
    let merged = config(u.docker, &root, &[u.file, &override_s], u.env, &["--no-env-resolution"]).await?;
    check_model(&merged, &[&root, &work_path])?;
    u.docker.ensure_network(PROXY_NETWORK).await?;
    let args = proc::args(&[
        "compose",
        "-p",
        u.project,
        "-f",
        u.file,
        "-f",
        &override_s,
        "up",
        "--detach",
        "--build",
        "--remove-orphans",
        "--wait",
        "--wait-timeout",
        &u.timeout.as_secs().to_string(),
    ]);
    let res = proc::stream(
        Spec {
            bin: &u.docker.bin,
            args: &args,
            env: u.env,
            cwd: Some(&root),
            timeout: u.timeout + Duration::from_secs(900),
        },
        cancel,
        log,
    )
    .await;
    drop(work);
    let out = res?;
    if !out.success() {
        return Err(Error::Invalid(format!("docker compose up failed:\n{}", out.tail(15))));
    }
    Ok(Started {
        container: container_of(u.project, &routed),
        port: picked.port,
    })
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
        assert_eq!(pick(&two, None).unwrap().routed, "web");
        let ports = cfg(r#"{"services":{"web":{},"api":{"ports":[{"target":80}]}}}"#);
        let p = pick(&ports, None).unwrap();
        assert_eq!(
            (p.routed.as_str(), p.port),
            ("api", Some(80)),
            "the one with a port wins"
        );
        let one = cfg(r#"{"services":{"site":{}}}"#);
        assert_eq!(pick(&one, None).unwrap().routed, "site");
        assert_eq!(pick(&two, Some("worker")).unwrap().routed, "worker");
        assert!(pick(&two, Some("db")).is_err());
        assert!(pick(&cfg(r#"{"services":{}}"#), None).is_err());
        // Several plausible services: the app has to say which.
        let neither = cfg(r#"{"services":{"zeta":{},"alpha":{}}}"#);
        let e = pick(&neither, None).unwrap_err();
        assert!(e.message().contains("alpha, zeta"), "{}", e.message());
    }

    /// Regression (audit A13): a database's port never makes it the website.
    #[test]
    fn routed_service_is_not_the_database() {
        let c = json!({"services":{
            "db":{"image":"postgres:17","expose":["5432"]},
            "web":{"build":{"context":"/r"},"expose":["3000"]}}});
        let p = pick(&c, None).unwrap();
        assert_eq!((p.routed.as_str(), p.port), ("web", Some(3000)));
        // A database known by its image, whatever port it says.
        let c = json!({"services":{
            "cache":{"image":"docker.io/library/redis:7","expose":["7000"]},
            "site":{"image":"nginx","ports":[{"target":8080,"published":"8080"}]}}});
        assert_eq!(pick(&c, None).unwrap().routed, "site");
        // Two web services with ports and no name to go by: ask.
        let c = json!({"services":{
            "api":{"expose":["4000"]},"ui":{"expose":["3000"]},"db":{"image":"mysql","expose":["3306"]}}});
        let e = pick(&c, None).unwrap_err();
        assert!(
            e.message().contains("api, ui") && !e.message().contains("db"),
            "{}",
            e.message()
        );
        // Only a database: still nothing to guess.
        let c = json!({"services":{"db":{"image":"postgres"},"cache":{"image":"redis"}}});
        assert!(pick(&c, None).is_err());
        // Several ports: none is guessed.
        let c = json!({"services":{"web":{"expose":["3000","9229"]}}});
        assert_eq!(pick(&c, None).unwrap().port, None);
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
        let v = config(&docker, &dir, &["docker-compose.yml"], &host_env, &[])
            .await
            .unwrap();
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
        let ov = override_yaml(&["web".into(), "db".into()], "web", &env_file, &labels());
        std::fs::write(base.join("override.yml"), ov).unwrap();
        let ov = base.join("override.yml").to_string_lossy().into_owned();
        let merged = |compose: &str| {
            std::fs::write(repo.join("docker-compose.yml"), compose).unwrap();
            let (docker, repo, ov) = (docker.clone(), repo.clone(), ov.clone());
            async move {
                config(
                    &docker,
                    &repo,
                    &["docker-compose.yml", &ov],
                    &[],
                    &["--no-env-resolution"],
                )
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
        let scratch = tempfile::tempdir().unwrap();
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
                scratch: scratch.path(),
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
        assert_eq!(
            std::fs::read_dir(scratch.path()).unwrap().count(),
            0,
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

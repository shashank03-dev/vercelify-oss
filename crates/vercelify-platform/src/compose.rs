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
//! another container's volumes) stops the preview.

use crate::docker::{Docker, MANAGED, MANAGED_LABEL, PROXY_NETWORK};
use crate::proc::{self, Cancel, Spec};
use crate::{Error, Result};
use std::fmt::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
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
        if s == routed {
            let _ = writeln!(
                y,
                "    networks:\n      default: {{}}\n      {}: {{}}",
                q(PROXY_NETWORK)
            );
        }
    }
    let _ = writeln!(y, "networks:\n  {}:\n    external: true", q(PROXY_NETWORK));
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

/// The merged configuration a preview runs with (`docker compose config`
/// with our override): every volume is the project's own, nothing is mounted
/// from outside the checkout, and no other container's volumes are used.
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
    }
    Ok(())
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
    args.extend(proc::args(&["config", "--format", "json"]));
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
            "{file} is not a valid compose file: {}",
            o.tail(4)
        )));
    }
    serde_json::from_str(&o.stdout).map_err(|e| Error::Invalid(format!("docker compose config: {e}")))
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
    let cfg = config(u.docker, u.dir, &[u.file], None, u.env).await?;
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
        if let Err(e) = merged.and_then(|m| check_preview_storage(&m, u.dir, u.project)) {
            let _ = std::fs::remove_file(&env_path);
            return Err(e);
        }
        if !volumes.is_empty() {
            log("info", &format!("preview volumes: {} (its own)", volumes.join(", ")));
        }
    }
    u.docker.ensure_network(PROXY_NETWORK).await?;
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
    let res = proc::stream(
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

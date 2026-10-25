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

/// The services of a compose file and the one to route to, from
/// `docker compose config --format json` (the file's own order is lost there).
pub async fn services(
    docker: &Docker,
    dir: &Path,
    file: &str,
    env: &[(String, String)],
    explicit: Option<&str>,
) -> Result<(Vec<String>, String)> {
    let args = proc::args(&["compose", "-f", file, "config", "--format", "json"]);
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
    let v: serde_json::Value =
        serde_json::from_str(&o.stdout).map_err(|e| Error::Invalid(format!("docker compose config: {e}")))?;
    pick(&v, explicit).map_err(|e| Error::Invalid(format!("{file}: {}", e.message())))
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
    let (services, routed) = services(u.docker, u.dir, u.file, u.env, u.service).await?;
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
    std::fs::write(&override_path, override_yaml(&services, &routed, &env_path, &u.labels))?;
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

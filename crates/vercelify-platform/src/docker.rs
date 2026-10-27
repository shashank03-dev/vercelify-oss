//! The container runtime, through the docker CLI (design D2).
//!
//! Variables reach containers as `-e KEY` with the value in the CLI's own
//! environment: never in arguments, never in a file on disk, and multi-line
//! values work.

use crate::proc::{self, Spec};
use crate::{Error, Result};
use std::collections::BTreeMap;
use std::time::Duration;

pub const MANAGED_LABEL: &str = "vercelify.managed";
pub const MANAGED: &str = "platform";
/// Network every routed container joins; the proxy lives on it.
pub const PROXY_NETWORK: &str = "vfy-proxy";

#[derive(Debug, Clone)]
pub struct Docker {
    pub bin: String,
}

impl Default for Docker {
    fn default() -> Self {
        Self { bin: "docker".into() }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mount {
    /// Named volume or absolute host path.
    pub source: String,
    pub target: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub labels: BTreeMap<String, String>,
    /// Passed through the environment (`-e KEY`).
    pub env: Vec<(String, String)>,
    /// First network at create time; the rest are connected after.
    pub networks: Vec<String>,
    /// DNS aliases on the first network (the app's name for its project).
    pub aliases: Vec<String>,
    pub memory_mb: Option<u32>,
    pub cpus: Option<f32>,
    pub command: Option<Vec<String>>,
    pub mounts: Vec<Mount>,
    /// host_ip:host_port:container_port
    pub publish: Vec<String>,
    pub cgroup_parent: Option<String>,
    pub restart: String,
    /// `name:ip` entries for /etc/hosts (`host.docker.internal:host-gateway`).
    pub extra_hosts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContainerState {
    pub id: String,
    pub name: String,
    pub image: String,
    /// running / exited / created / paused / restarting / dead
    pub state: String,
    pub labels: BTreeMap<String, String>,
    pub created: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Inspect {
    pub running: bool,
    pub paused: bool,
    pub status: String,
    pub exit_code: i64,
    pub restart_count: u64,
    pub started_at: String,
    /// Address on each network.
    pub ips: BTreeMap<String, String>,
    pub oom_killed: bool,
}

fn quick() -> Duration {
    Duration::from_secs(30)
}

impl Docker {
    pub async fn run(&self, args: &[String], timeout: Duration) -> Result<proc::Output> {
        proc::run(Spec {
            bin: &self.bin,
            args,
            env: &[],
            cwd: None,
            timeout,
        })
        .await
    }

    async fn ok(&self, args: &[String], timeout: Duration, what: &str) -> Result<proc::Output> {
        let o = self.run(args, timeout).await?;
        if !o.success() {
            let tail = o.tail(3);
            if tail.contains("Cannot connect to the Docker daemon") || tail.contains("Is the docker daemon running") {
                return Err(Error::Unavailable("Docker is not running".into()));
            }
            return Err(Error::Internal(format!("{what}: {tail}")));
        }
        Ok(o)
    }

    pub async fn ensure_network(&self, name: &str) -> Result<()> {
        let o = self.run(&proc::args(&["network", "inspect", name]), quick()).await?;
        if o.success() {
            return Ok(());
        }
        let o = self
            .run(
                &proc::args(&[
                    "network",
                    "create",
                    "--label",
                    &format!("{MANAGED_LABEL}={MANAGED}"),
                    name,
                ]),
                quick(),
            )
            .await?;
        // Two deploys racing: someone else created it.
        if o.success() || o.tail(2).contains("already exists") {
            Ok(())
        } else {
            Err(Error::Internal(format!("docker network create {name}: {}", o.tail(2))))
        }
    }

    /// The user an image runs as (`.Config.User`, "" = root).
    pub async fn image_user(&self, image: &str) -> Result<String> {
        let o = self
            .ok(
                &proc::args(&["image", "inspect", "--format", "{{.Config.User}}", image]),
                quick(),
                "docker image inspect",
            )
            .await?;
        Ok(o.stdout.trim().to_string())
    }

    pub async fn image_exists(&self, image: &str) -> Result<bool> {
        Ok(self
            .run(
                &proc::args(&["image", "inspect", "--format", "{{.Id}}", image]),
                quick(),
            )
            .await?
            .success())
    }

    /// Pull, trying again twice on what may be transient (the network, the
    /// registry, containerd), never on an image that doesn't exist.
    pub async fn pull(&self, image: &str) -> Result<()> {
        let mut last = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2 * attempt)).await;
            }
            match self
                .ok(
                    &proc::args(&["pull", "--quiet", image]),
                    Duration::from_secs(900),
                    &format!("docker pull {image}"),
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) if !retryable_pull(e.message()) => return Err(e),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| Error::Internal(format!("docker pull {image}"))))
    }

    /// Create (not start) a container.
    pub async fn create(&self, spec: &ContainerSpec) -> Result<String> {
        let mut args = proc::args(&["create", "--name", &spec.name]);
        let restart = if spec.restart.is_empty() {
            "unless-stopped"
        } else {
            &spec.restart
        };
        args.extend(proc::args(&["--restart", restart, "--pids-limit", "4096"]));
        args.extend(proc::args(&[
            "--log-driver",
            "local",
            "--log-opt",
            "max-size=10m",
            "--log-opt",
            "max-file=3",
        ]));
        for (k, v) in &spec.labels {
            args.extend(["--label".into(), format!("{k}={v}")]);
        }
        for (k, _) in &spec.env {
            args.extend(["-e".into(), k.clone()]);
        }
        if let Some(n) = spec.networks.first() {
            args.extend(["--network".into(), n.clone()]);
            for a in &spec.aliases {
                args.extend(["--network-alias".into(), a.clone()]);
            }
        }
        if let Some(m) = spec.memory_mb {
            args.extend(["--memory".into(), format!("{m}m")]);
        }
        if let Some(c) = spec.cpus {
            args.extend(["--cpus".into(), format!("{c}")]);
        }
        if let Some(cg) = &spec.cgroup_parent {
            args.extend(["--cgroup-parent".into(), cg.clone()]);
        }
        for m in &spec.mounts {
            let kind = if m.source.starts_with('/') { "bind" } else { "volume" };
            let mut v = format!("type={kind},source={},target={}", m.source, m.target);
            if m.read_only {
                v.push_str(",readonly");
            }
            args.extend(["--mount".into(), v]);
        }
        for p in &spec.publish {
            args.extend(["--publish".into(), p.clone()]);
        }
        for h in &spec.extra_hosts {
            args.extend(["--add-host".into(), h.clone()]);
        }
        args.push(spec.image.clone());
        if let Some(cmd) = &spec.command {
            args.extend(cmd.iter().cloned());
        }
        let o = proc::run(Spec {
            bin: &self.bin,
            args: &args,
            env: &spec.env,
            cwd: None,
            timeout: Duration::from_secs(120),
        })
        .await?;
        if !o.success() {
            return Err(Error::Internal(format!("docker create {}: {}", spec.name, o.tail(3))));
        }
        let id = o.stdout.trim().to_string();
        for n in spec.networks.iter().skip(1) {
            self.ok(
                &proc::args(&["network", "connect", n, &spec.name]),
                quick(),
                &format!("docker network connect {n}"),
            )
            .await?;
        }
        Ok(id)
    }

    pub async fn start(&self, name: &str) -> Result<()> {
        self.ok(
            &proc::args(&["start", name]),
            Duration::from_secs(60),
            &format!("docker start {name}"),
        )
        .await
        .map(|_| ())
    }

    pub async fn restart(&self, name: &str, grace_secs: u32) -> Result<()> {
        self.ok(
            &proc::args(&["restart", "--time", &grace_secs.to_string(), name]),
            Duration::from_secs(grace_secs as u64 + 60),
            &format!("docker restart {name}"),
        )
        .await
        .map(|_| ())
    }

    pub async fn stop(&self, name: &str, grace_secs: u32) -> Result<()> {
        let o = self
            .run(
                &proc::args(&["stop", "--time", &grace_secs.to_string(), name]),
                Duration::from_secs(grace_secs as u64 + 30),
            )
            .await?;
        if o.success() || o.tail(2).contains("No such container") {
            Ok(())
        } else {
            Err(Error::Internal(format!("docker stop {name}: {}", o.tail(2))))
        }
    }

    pub async fn remove(&self, name: &str) -> Result<()> {
        let o = self
            .run(&proc::args(&["rm", "--force", "--volumes=false", name]), quick())
            .await?;
        if o.success() || o.tail(2).contains("No such container") {
            Ok(())
        } else {
            Err(Error::Internal(format!("docker rm {name}: {}", o.tail(2))))
        }
    }

    pub async fn inspect(&self, name: &str) -> Result<Option<Inspect>> {
        let o = self
            .run(
                &proc::args(&["container", "inspect", "--format", "{{json .}}", name]),
                quick(),
            )
            .await?;
        if !o.success() {
            return if o.tail(2).contains("No such") {
                Ok(None)
            } else {
                Err(Error::Internal(format!("docker inspect {name}: {}", o.tail(2))))
            };
        }
        let v: serde_json::Value = serde_json::from_str(o.stdout.trim())?;
        let st = &v["State"];
        let ips = v["NetworkSettings"]["Networks"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, n)| {
                        Some((
                            k.clone(),
                            n["IPAddress"].as_str().filter(|s| !s.is_empty())?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(Inspect {
            running: st["Running"].as_bool().unwrap_or(false),
            paused: st["Paused"].as_bool().unwrap_or(false),
            status: st["Status"].as_str().unwrap_or("").to_string(),
            exit_code: st["ExitCode"].as_i64().unwrap_or(0),
            restart_count: v["RestartCount"].as_u64().unwrap_or(0),
            started_at: st["StartedAt"].as_str().unwrap_or("").to_string(),
            ips,
            oom_killed: st["OOMKilled"].as_bool().unwrap_or(false),
        }))
    }

    /// Our containers (label `vercelify.managed=platform`), running or not.
    pub async fn list_managed(&self) -> Result<Vec<ContainerState>> {
        let o = self
            .ok(
                &proc::args(&[
                    "ps",
                    "--all",
                    "--no-trunc",
                    "--filter",
                    &format!("label={MANAGED_LABEL}={MANAGED}"),
                    "--format",
                    "{{json .}}",
                ]),
                quick(),
                "docker ps",
            )
            .await?;
        Ok(parse_ps(&o.stdout))
    }

    /// One label of a container ("" if absent).
    pub async fn label(&self, name: &str, key: &str) -> Result<String> {
        let o = self
            .ok(
                &proc::args(&["container", "inspect", "--format", "{{json .Config.Labels}}", name]),
                quick(),
                "docker inspect",
            )
            .await?;
        let v: serde_json::Value = serde_json::from_str(o.stdout.trim()).unwrap_or_default();
        Ok(v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string())
    }

    pub async fn logs(&self, name: &str, tail: u32) -> Result<String> {
        let o = self
            .run(
                &proc::args(&["logs", "--tail", &tail.to_string(), "--timestamps", name]),
                quick(),
            )
            .await?;
        Ok(by_time(&o.stdout, &o.stderr))
    }

    pub async fn exec(&self, name: &str, cmd: &[String], timeout: Duration) -> Result<proc::Output> {
        let mut args = proc::args(&["exec", name]);
        args.extend(cmd.iter().cloned());
        self.run(&args, timeout).await
    }

    /// Images of a repository (`vfy/blog`), newest first: (tag, id).
    pub async fn images(&self, repository: &str) -> Result<Vec<(String, String)>> {
        let o = self
            .ok(
                &proc::args(&[
                    "image",
                    "ls",
                    "--format",
                    "{{.Tag}}\t{{.ID}}\t{{.CreatedAt}}",
                    repository,
                ]),
                quick(),
                "docker image ls",
            )
            .await?;
        let mut rows: Vec<(String, String, String)> = o
            .stdout
            .lines()
            .filter_map(|l| {
                let mut p = l.split('\t');
                Some((p.next()?.to_string(), p.next()?.to_string(), p.next()?.to_string()))
            })
            .filter(|(t, _, _)| t != "<none>")
            .collect();
        rows.sort_by(|a, b| b.2.cmp(&a.2));
        Ok(rows.into_iter().map(|(t, id, _)| (t, id)).collect())
    }

    pub async fn remove_image(&self, image: &str) -> Result<()> {
        let _ = self.run(&proc::args(&["image", "rm", image]), quick()).await?;
        Ok(())
    }

    pub async fn volume_remove(&self, name: &str) -> Result<()> {
        let _ = self.run(&proc::args(&["volume", "rm", name]), quick()).await?;
        Ok(())
    }
}

/// Whether a failed pull may succeed on another try.
pub fn retryable_pull(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    ![
        "not found: manifest unknown",
        "manifest unknown",
        "unauthorized",
        "denied",
        "invalid reference",
        "docker is not running",
    ]
    .iter()
    .any(|p| m.contains(p))
}

pub fn parse_ps(stdout: &str) -> Vec<ContainerState> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| {
            let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
            ContainerState {
                id: s("ID"),
                name: s("Names").split(',').next().unwrap_or("").to_string(),
                image: s("Image"),
                state: s("State"),
                labels: s("Labels")
                    .split(',')
                    .filter_map(|kv| kv.split_once('='))
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                created: s("CreatedAt"),
            }
        })
        .collect()
}

/// `docker logs --timestamps` output, its two streams merged into one in
/// time order (a crash's error then follows the lines before it).
fn by_time(stdout: &str, stderr: &str) -> String {
    let mut lines: Vec<&str> = stdout.lines().chain(stderr.lines()).collect();
    lines.sort_by_key(|l| l.split_once(' ').map_or(*l, |(t, _)| t));
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_from_both_streams_are_in_time_order() {
        let out = "2026-10-27T11:07:38.4Z > node server.js\n2026-10-27T11:08:38.5Z > node server.js\n";
        let err = "2026-10-27T11:07:38.5Z SyntaxError\n2026-10-27T11:08:38.6Z SyntaxError\n";
        assert_eq!(
            by_time(out, err),
            "2026-10-27T11:07:38.4Z > node server.js\n2026-10-27T11:07:38.5Z SyntaxError\n\
             2026-10-27T11:08:38.5Z > node server.js\n2026-10-27T11:08:38.6Z SyntaxError\n"
        );
        assert_eq!(by_time("", ""), "");
    }

    #[test]
    fn pull_retries_only_what_may_pass() {
        assert!(retryable_pull(
            "unable to lease content: lease does not exist: not found"
        ));
        assert!(retryable_pull("net/http: TLS handshake timeout"));
        assert!(!retryable_pull("manifest unknown: manifest unknown"));
        assert!(!retryable_pull(
            "pull access denied for nope, repository does not exist"
        ));
        assert!(!retryable_pull("Docker is not running"));
    }

    #[test]
    fn ps_rows() {
        let out = r#"{"ID":"abc","Names":"vfy-blog-1","Image":"vfy/blog:1","State":"running","Labels":"vercelify.app=app_1,vercelify.managed=platform","CreatedAt":"2026-10-25 05:00:00 +0000 UTC"}
not json
{"ID":"def","Names":"vfy-db","Image":"postgres:17","State":"exited","Labels":"","CreatedAt":""}"#;
        let rows = parse_ps(out);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].labels["vercelify.app"], "app_1");
        assert_eq!(rows[1].state, "exited");
        assert!(rows[1].labels.is_empty());
    }

    /// Real Docker (opt in with VERCELIFY_DOCKER_TESTS=1).
    #[tokio::test]
    async fn container_lifecycle() {
        if std::env::var("VERCELIFY_DOCKER_TESTS").as_deref() != Ok("1") {
            return;
        }
        let d = Docker::default();
        d.ensure_network("vfy-test-net").await.unwrap();
        d.ensure_network("vfy-test-net").await.unwrap();
        let name = "vfy-test-lifecycle";
        let _ = d.remove(name).await;
        let spec = ContainerSpec {
            name: name.into(),
            image: "alpine:3.20".into(),
            labels: [(MANAGED_LABEL.to_string(), MANAGED.to_string())].into(),
            env: vec![("MULTI".into(), "line one\nline two".into())],
            networks: vec!["vfy-test-net".into()],
            command: Some(vec!["sh".into(), "-c".into(), "sleep 300".into()]),
            memory_mb: Some(64),
            ..ContainerSpec::default()
        };
        d.create(&spec).await.unwrap();
        d.start(name).await.unwrap();
        let i = d.inspect(name).await.unwrap().unwrap();
        assert!(i.running);
        assert!(i.ips.contains_key("vfy-test-net"));
        let out = d
            .exec(
                name,
                &proc::args(&["sh", "-c", "printf %s \"$MULTI\""]),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout, "line one\nline two", "multi-line values survive");
        assert!(d.list_managed().await.unwrap().iter().any(|c| c.name == name));
        d.stop(name, 1).await.unwrap();
        d.remove(name).await.unwrap();
        assert!(d.inspect(name).await.unwrap().is_none());
        d.remove(name).await.unwrap();
    }
}

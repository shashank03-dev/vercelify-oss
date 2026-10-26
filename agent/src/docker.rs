//! Docker container inspection and control via the `docker` CLI.
//!
//! Using the CLI (instead of a socket client) keeps the agent small and
//! automatically honours DOCKER_HOST / contexts configured on the host.

use crate::error::AppError;
use crate::exec::run;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use vercelify_core::ContainerInfo;

const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PsRow {
    #[serde(rename = "ID")]
    id: String,
    names: String,
    image: String,
    state: String,
    status: String,
    #[serde(default)]
    ports: String,
    #[serde(default)]
    labels: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct StatsRow {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "CPUPerc")]
    cpu_perc: String,
    mem_usage: String,
    mem_perc: String,
}

/// Container ids/names we are willing to pass to the docker CLI.
pub fn valid_ref(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 128
        && r.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && r.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn parse_labels(raw: &str) -> HashMap<String, String> {
    raw.split(',')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.to_string()))
        .collect()
}

fn parse_pct(s: &str) -> Option<f32> {
    s.trim().trim_end_matches('%').parse().ok()
}

/// "12.5MiB / 7.6GiB" -> bytes of the first value.
pub fn parse_mem_usage(s: &str) -> Option<u64> {
    let first = s.split('/').next()?.trim();
    let idx = first.find(|c: char| c.is_ascii_alphabetic())?;
    let (num, unit) = first.split_at(idx);
    let n: f64 = num.trim().parse().ok()?;
    let mult = match unit.trim() {
        "B" => 1.0,
        "kB" | "KB" => 1e3,
        "KiB" => 1024.0,
        "MB" => 1e6,
        "MiB" => 1024.0 * 1024.0,
        "GB" => 1e9,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TB" => 1e12,
        "TiB" => 1024f64.powi(4),
        _ => return None,
    };
    Some((n * mult) as u64)
}

pub fn parse_ps(stdout: &str) -> Vec<ContainerInfo> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<PsRow>(l).ok())
        .map(|r| {
            let labels = parse_labels(&r.labels);
            ContainerInfo {
                app: labels.get("vercelify.app-name").filter(|a| !a.is_empty()).cloned(),
                id: r.id,
                name: r.names,
                image: r.image,
                state: r.state,
                status: r.status,
                ports: r.ports,
                cpu_pct: None,
                mem_bytes: None,
                mem_pct: None,
            }
        })
        .collect()
}

pub fn merge_stats(containers: &mut [ContainerInfo], stats_stdout: &str) {
    let stats: HashMap<String, StatsRow> = stats_stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<StatsRow>(l).ok())
        .map(|s| (s.id.clone(), s))
        .collect();
    for c in containers {
        // `docker stats` prints 12-char ids, `ps` too unless --no-trunc.
        if let Some(s) = stats.get(&c.id).or_else(|| stats.get(c.id.get(..12).unwrap_or(&c.id))) {
            c.cpu_pct = parse_pct(&s.cpu_perc);
            c.mem_pct = parse_pct(&s.mem_perc);
            c.mem_bytes = parse_mem_usage(&s.mem_usage);
        }
    }
}

pub async fn list(docker: &str, with_stats: bool) -> Result<Vec<ContainerInfo>, AppError> {
    let out = run(docker, &["ps", "-a", "--format", "{{json .}}"], TIMEOUT).await?;
    if !out.success() {
        return Err(AppError::unavailable(format!(
            "docker ps failed: {}",
            out.stderr.trim()
        )));
    }
    let mut containers = parse_ps(&out.stdout);
    if with_stats && containers.iter().any(|c| c.state == "running") {
        let stats = run(docker, &["stats", "--no-stream", "--format", "{{json .}}"], TIMEOUT).await?;
        if stats.success() {
            merge_stats(&mut containers, &stats.stdout);
        }
    }
    containers.sort_by(|a, b| {
        (a.state != "running", a.app.is_none(), &a.name).cmp(&(b.state != "running", b.app.is_none(), &b.name))
    });
    Ok(containers)
}

pub async fn action(docker: &str, id: &str, action: &str) -> Result<(), AppError> {
    if !valid_ref(id) {
        return Err(AppError::bad_request("invalid container reference"));
    }
    let verb = match action {
        "start" | "stop" | "restart" | "pause" | "unpause" => action,
        _ => return Err(AppError::bad_request(format!("unsupported action `{action}`"))),
    };
    let out = run(docker, &[verb, id], Duration::from_secs(90)).await?;
    if out.success() {
        Ok(())
    } else if out.stderr.contains("No such container") {
        Err(AppError::not_found(out.stderr.trim()))
    } else {
        Err(AppError::conflict(out.stderr.trim()))
    }
}

pub async fn logs(docker: &str, id: &str, tail: u32) -> Result<Vec<String>, AppError> {
    if !valid_ref(id) {
        return Err(AppError::bad_request("invalid container reference"));
    }
    let tail = tail.clamp(1, 5000).to_string();
    let out = run(docker, &["logs", "--timestamps", "--tail", &tail, id], TIMEOUT).await?;
    if !out.success() {
        return Err(AppError::not_found(out.stderr.trim()));
    }
    // docker logs writes the container's stdout and stderr to ours; merge by timestamp.
    let mut lines: Vec<String> = out.stdout.lines().chain(out.stderr.lines()).map(String::from).collect();
    lines.sort();
    Ok(lines)
}

pub async fn volume_create(docker: &str, name: &str, service: &str) -> Result<bool, AppError> {
    let inspect = run(docker, &["volume", "inspect", name], TIMEOUT).await?;
    if inspect.success() {
        return Ok(false);
    }
    let label = format!("vercelify.service={service}");
    let out = run(docker, &["volume", "create", "--label", &label, name], TIMEOUT).await?;
    if !out.success() {
        return Err(AppError::internal(format!(
            "docker volume create failed: {}",
            out.stderr.trim()
        )));
    }
    Ok(true)
}

pub async fn volume_names(docker: &str) -> Result<Vec<String>, AppError> {
    let out = run(docker, &["volume", "ls", "--format", "{{.Name}}"], TIMEOUT).await?;
    if !out.success() {
        return Err(AppError::unavailable(format!(
            "docker volume ls failed: {}",
            out.stderr.trim()
        )));
    }
    Ok(out
        .stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS: &str = r#"{"Command":"\"docker-entrypoint.s…\"","CreatedAt":"2026-10-18 10:00:00 +0000 UTC","ID":"a1b2c3d4e5f6","Image":"postgres:16-alpine","Labels":"vercelify.managed=true,vercelify.app-name=shop,vercelify.kind=app","LocalVolumes":"1","Mounts":"postgres-data","Names":"postgresql-abc","Networks":"vfy-proxy","Ports":"5432/tcp","RunningFor":"2 hours ago","Size":"0B","State":"running","Status":"Up 2 hours (healthy)"}
{"Command":"\"/bin/sh\"","ID":"ffffeeeedddd","Image":"alpine","Labels":"","Names":"scratch","Ports":"","State":"exited","Status":"Exited (0) 3 days ago"}"#;

    const STATS: &str = r#"{"BlockIO":"0B / 0B","CPUPerc":"1.25%","Container":"a1b2c3d4e5f6","ID":"a1b2c3d4e5f6","MemPerc":"0.85%","MemUsage":"64.5MiB / 7.6GiB","Name":"postgresql-abc","NetIO":"1kB / 0B","PIDs":"7"}"#;

    #[test]
    fn parses_ps_and_stats() {
        let mut c = parse_ps(PS);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].app.as_deref(), Some("shop"));
        assert_eq!(c[1].app, None);
        merge_stats(&mut c, STATS);
        assert_eq!(c[0].cpu_pct, Some(1.25));
        assert_eq!(c[0].mem_bytes, Some(67_633_152));
        assert_eq!(c[1].cpu_pct, None);
    }

    #[test]
    fn mem_units() {
        assert_eq!(parse_mem_usage("1.5GiB / 8GiB"), Some(1_610_612_736));
        assert_eq!(parse_mem_usage("512kB / 1GB"), Some(512_000));
        assert_eq!(parse_mem_usage("0B / 0B"), Some(0));
        assert_eq!(parse_mem_usage("garbage"), None);
    }

    #[test]
    fn refs() {
        assert!(valid_ref("a1b2c3"));
        assert!(valid_ref("vercelify-proxy"));
        assert!(!valid_ref("-rm"));
        assert!(!valid_ref("a b"));
        assert!(!valid_ref("../x"));
    }
}

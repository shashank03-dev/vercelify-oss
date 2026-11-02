//! Fresh inventory for admission. Collection failures stay unknown.
use crate::config::Config;
use serde_json::Value;
use std::{collections::HashMap, path::Path, sync::OnceLock, time::Duration};
use vercelify_core::platform::{CapacityInventory, CapacitySample, NodeStatus};
use vercelify_platform::{capacity, proc};

const MIB: u64 = 1_048_576;
fn mb(bytes: u64) -> u64 {
    bytes.div_ceil(MIB)
}

fn inspect_inventory(raw: &Value, usage: Option<u64>) -> Option<CapacityInventory> {
    let id = raw["Id"].as_str()?.to_string();
    let name = raw["Name"].as_str()?.trim_start_matches('/');
    let labels = &raw["Config"]["Labels"];
    let label = |key: &str| labels[key].as_str().unwrap_or("");
    let (category, owner) = if name == vercelify_platform::build::BUILDER_CONTAINER {
        ("build", "builder")
    } else if !label("vercelify.database").is_empty() {
        ("database", label("vercelify.database"))
    } else if !label("vercelify.deployment").is_empty() {
        ("app", label("vercelify.deployment"))
    } else if !label("vercelify.managed").is_empty() {
        ("platform", id.as_str())
    } else {
        ("other", id.as_str())
    };
    let running = raw["State"]["Running"].as_bool()? || raw["State"]["Restarting"].as_bool().unwrap_or(false);
    let mut inventory = CapacityInventory {
        id: id.clone(),
        owner: owner.into(),
        category: category.into(),
        usage_mb: usage.map(mb),
        limit_mb: raw["HostConfig"]["Memory"].as_u64().filter(|m| *m > 0).map(mb),
        running,
        oom_killed: raw["State"]["OOMKilled"].as_bool(),
        finished_at: raw["State"]["FinishedAt"]
            .as_str()
            .filter(|s| !s.starts_with("0001-"))
            .map(String::from),
        ..CapacityInventory::default()
    };
    // A remote Docker PID must never be confused with an unrelated local PID.
    if let Some(pid) = raw["State"]["Pid"].as_u64().filter(|p| *p > 0) {
        if let Ok(group) = std::fs::read_to_string(format!("/proc/{pid}/cgroup")) {
            if let Some(path) = group
                .lines()
                .find_map(|l| l.strip_prefix("0::"))
                .filter(|p| p.contains(&id))
            {
                let path = Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/'));
                inventory.peak_mb = read_number(&path.join("memory.peak")).map(mb);
                if let Ok(events) = std::fs::read_to_string(path.join("memory.events")) {
                    inventory.oom_events = counter(&events, "oom_kill");
                    inventory.cap_events = counter(&events, "max");
                }
            }
        }
    }
    Some(inventory)
}
fn counter(raw: &str, name: &str) -> Option<u64> {
    raw.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next() == Some(name))
            .then(|| words.next()?.parse().ok())
            .flatten()
    })
}
fn read_number(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}
fn memory_max(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if let Some(v) = raw.strip_suffix('M') {
        v.parse().ok()
    } else if let Some(v) = raw.strip_suffix('G') {
        v.parse::<u64>().ok()?.checked_mul(1024)
    } else {
        raw.parse::<u64>().ok().map(mb)
    }
}
fn external_heavy() -> Option<bool> {
    Some(std::fs::read_dir("/proc").ok()?.flatten().any(|entry| {
        let name = entry.file_name();
        if !name.to_string_lossy().bytes().all(|c| c.is_ascii_digit()) {
            return false;
        }
        std::fs::read_to_string(entry.path().join("comm"))
            .is_ok_and(|c| matches!(c.trim(), "restic" | "pg_restore" | "mongorestore"))
    }))
}

pub async fn collect(cfg: &Config, status: &NodeStatus) -> Option<CapacitySample> {
    if !status.docker_ok || status.memory_total_mb == 0 {
        return None;
    }
    let measured_at = chrono::Utc::now().timestamp();
    let containers = crate::docker::list(&cfg.docker_bin, true).await.ok()?;
    let stats: HashMap<_, _> = containers.iter().map(|c| (c.id.as_str(), c.mem_bytes)).collect();
    let mut inventory = Vec::new();
    if !containers.is_empty() {
        let mut args = proc::args(&["inspect"]);
        args.extend(containers.iter().map(|c| c.id.clone()));
        let out = proc::run(proc::Spec {
            bin: &cfg.docker_bin,
            args: &args,
            env: &[],
            cwd: None,
            timeout: Duration::from_secs(20),
        })
        .await
        .ok()?;
        if !out.success() {
            return None;
        }
        let raw: Vec<Value> = serde_json::from_str(&out.stdout).ok()?;
        if raw.len() != containers.len() {
            return None;
        }
        for row in raw {
            let id = row["Id"].as_str()?;
            let usage = stats
                .get(id)
                .or_else(|| stats.get(id.get(..12).unwrap_or(id)))
                .copied()
                .flatten();
            inventory.push(inspect_inventory(&row, usage)?);
        }
    }
    let mut workload_limit = status
        .memory_total_mb
        .saturating_sub(capacity::os_reserve(status.memory_total_mb));
    let slice = cfg.systemd_dir.join("vercelify.slice");
    if slice.exists() {
        let raw = std::fs::read_to_string(slice).ok()?;
        let max = raw
            .lines()
            .find_map(|l| l.strip_prefix("MemoryMax="))
            .and_then(memory_max)?;
        workload_limit = workload_limit.min(max);
    }
    if let Some(max) = read_number(Path::new("/sys/fs/cgroup/vercelify.slice/memory.max")) {
        workload_limit = workload_limit.min(mb(max));
    }
    // Upload staging and Docker's layer store may use different filesystems.
    // Promise against the smaller free-space value; an unknown root stays unknown.
    let root = proc::run(proc::Spec {
        bin: &cfg.docker_bin,
        args: &proc::args(&["info", "--format", "{{.DockerRootDir}}"]),
        env: &[],
        cwd: None,
        timeout: Duration::from_secs(5),
    })
    .await
    .ok()
    .filter(|o| o.success());
    let mut disk = None;
    if let Some(root) = root.filter(|o| !o.stdout.trim().is_empty()) {
        let paths = [
            cfg.state_dir.to_string_lossy().to_string(),
            root.stdout.trim().to_string(),
        ];
        let mut free = Vec::new();
        for path in paths {
            let out = proc::run(proc::Spec {
                bin: "df",
                args: &proc::args(&["-Pk", &path]),
                env: &[],
                cwd: None,
                timeout: Duration::from_secs(5),
            })
            .await
            .ok()
            .filter(|o| o.success());
            let measured = out
                .and_then(|o| o.stdout.lines().last()?.split_whitespace().nth(3)?.parse::<u64>().ok())
                .map(|kb| kb / 1024);
            if let Some(measured) = measured {
                free.push(measured);
            }
        }
        if free.len() == 2 {
            disk = free.into_iter().min();
        }
    }
    static EPOCH: OnceLock<String> = OnceLock::new();
    Some(CapacitySample {
        epoch: EPOCH.get_or_init(|| vercelify_platform::ids::new_id("node")).clone(),
        measured_at,
        total_mb: status.memory_total_mb,
        available_mb: status.memory_available_mb,
        workload_limit_mb: workload_limit,
        disk_free_mb: disk,
        host_ready: true,
        external_heavy: external_heavy()?,
        containers: inventory,
        host_grant_mb: None,
        host_available_mb: None,
        provider_overhead_mb: None,
        host_oom_kills: std::fs::read_to_string("/proc/vmstat")
            .ok()
            .and_then(|s| counter(&s, "oom_kill")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn inventory_separates_working_memory_caps_and_explicit_oom_evidence() {
        let raw = json!({"Id":"abcdef","Name":"/vfy-app","State":{"Running":true,"OOMKilled":false},"HostConfig":{"Memory":268435456},"Config":{"Env":["PASSWORD=never-forward-me"],"Labels":{"vercelify.deployment":"dpl_one","vercelify.kind":"app"}}});
        let item = inspect_inventory(&raw, Some(64 * 1024 * 1024)).unwrap();
        assert_eq!((item.category.as_str(), item.owner.as_str()), ("app", "dpl_one"));
        assert_eq!(item.usage_mb, Some(64));
        assert_eq!(item.limit_mb, Some(256));
        assert_eq!(item.oom_killed, Some(false));
        assert!(!serde_json::to_string(&item).unwrap().contains("never-forward-me"));
    }
    #[test]
    fn missing_memory_data_never_becomes_an_observed_zero() {
        let raw = json!({"Id":"db","Name":"/vfy-db","State":{"Running":true},"HostConfig":{"Memory":0},"Config":{"Labels":{"vercelify.database":"db_one","vercelify.kind":"db"}}});
        let item = inspect_inventory(&raw, None).unwrap();
        assert_eq!((item.category.as_str(), item.owner.as_str()), ("database", "db_one"));
        assert_eq!(item.usage_mb, None);
        assert_eq!(item.limit_mb, None);
        assert_eq!(item.oom_killed, None);
        assert_eq!(item.peak_mb, None);
    }
    #[test]
    fn incomplete_container_identity_invalidates_the_inventory() {
        assert!(inspect_inventory(&json!({"State":{"Running":true}}), None).is_none());
    }
}

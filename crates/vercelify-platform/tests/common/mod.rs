//! Real Linux/Docker inventory for library contracts without an agent process.
use std::{path::Path, sync::Arc, time::Duration};
use vercelify_core::platform::{CapacityInventory, CapacitySample, NodeStatus};
use vercelify_platform::{capacity, docker::Docker, nodes, proc, Result, Store};

pub struct InventoryTask(tokio::task::JoinHandle<()>);
impl Drop for InventoryTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub async fn inventory(store: Arc<Store>, disk: &Path) -> InventoryTask {
    async fn record(store: &Store, disk: &Path) -> Result<()> {
        let memory = std::fs::read_to_string("/proc/meminfo")?;
        let mb = |name: &str| {
            memory
                .lines()
                .find(|line| line.starts_with(name))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse::<u64>().ok())
                .map(|kb| kb / 1024)
                .ok_or_else(|| vercelify_platform::Error::Unavailable(format!("contract needs measured {name}")))
        };
        let (total, available) = (mb("MemTotal:")?, mb("MemAvailable:")?);
        let docker = Docker::default();
        let ids = docker.run(&proc::args(&["ps", "-aq"]), Duration::from_secs(10)).await?;
        let mut containers = Vec::new();
        if !ids.stdout.trim().is_empty() {
            let mut args = proc::args(&["inspect"]);
            args.extend(ids.stdout.split_whitespace().map(String::from));
            let output = docker.run(&args, Duration::from_secs(10)).await?;
            let raw: Vec<serde_json::Value> = serde_json::from_str(&output.stdout)?;
            for item in raw {
                let id = item["Id"].as_str().unwrap().to_owned();
                let labels = &item["Config"]["Labels"];
                let (category, owner) = if let Some(db) = labels["vercelify.database"].as_str() {
                    ("database", db)
                } else if let Some(deployment) = labels["vercelify.deployment"].as_str() {
                    ("app", deployment)
                } else if item["Name"].as_str() == Some("/buildx_buildkit_vercelify0") {
                    ("build", "builder")
                } else {
                    ("platform", id.as_str())
                };
                let pid = item["State"]["Pid"].as_u64().unwrap_or(0);
                let usage = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
                    .ok()
                    .and_then(|raw| {
                        let relative = raw.lines().find_map(|line| line.strip_prefix("0::"))?;
                        if !relative.contains(&id) {
                            return None;
                        }
                        let directory = Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
                        let current = std::fs::read_to_string(directory.join("memory.current"))
                            .ok()?
                            .trim()
                            .parse::<u64>()
                            .ok()?;
                        let inactive = std::fs::read_to_string(directory.join("memory.stat"))
                            .ok()
                            .and_then(|raw| {
                                raw.lines().find_map(|line| {
                                    line.strip_prefix("inactive_file ").and_then(|n| n.parse::<u64>().ok())
                                })
                            })
                            .unwrap_or(0);
                        Some(current.saturating_sub(inactive).div_ceil(1_048_576))
                    });
                containers.push(CapacityInventory {
                    id: id.clone(),
                    owner: owner.to_owned(),
                    category: category.into(),
                    running: item["State"]["Running"].as_bool().unwrap_or(false),
                    usage_mb: usage,
                    limit_mb: item["HostConfig"]["Memory"]
                        .as_u64()
                        .filter(|m| *m > 0)
                        .map(|m| m.div_ceil(1_048_576)),
                    oom_killed: item["State"]["OOMKilled"].as_bool(),
                    ..Default::default()
                });
            }
        }
        let free = docker
            .run(
                &proc::args(&["info", "--format", "{{.DockerRootDir}}"]),
                Duration::from_secs(5),
            )
            .await?;
        let df = proc::run(proc::Spec {
            bin: "df",
            args: &proc::args(&["-Pm", &disk.to_string_lossy(), free.stdout.trim()]),
            env: &[],
            cwd: None,
            timeout: Duration::from_secs(5),
        })
        .await?;
        let disk_free = df
            .stdout
            .lines()
            .skip(1)
            .filter_map(|line| line.split_whitespace().nth(3)?.parse::<u64>().ok())
            .min();
        let sample = CapacitySample {
            epoch: "real-docker-contract".into(),
            measured_at: chrono::Utc::now().timestamp(),
            total_mb: total,
            available_mb: available,
            workload_limit_mb: total.saturating_sub(capacity::os_reserve(total)),
            disk_free_mb: disk_free,
            host_ready: true,
            containers,
            ..Default::default()
        };
        store.tx(|c| {
            nodes::set_main_status(
                c,
                "",
                &NodeStatus {
                    capacity: Some(sample),
                    memory_total_mb: total,
                    memory_available_mb: available,
                    docker_ok: true,
                    cpus: 2,
                    ..Default::default()
                },
            )
        })
    }
    record(&store, disk)
        .await
        .expect("contract requires real capacity measurements");
    let disk = disk.to_path_buf();
    InventoryTask(tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            if let Err(error) = record(&store, &disk).await {
                eprintln!("contract inventory unavailable: {error}");
            }
        }
    }))
}

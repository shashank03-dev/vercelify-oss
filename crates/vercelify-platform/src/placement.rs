//! Which servers an app's instances run on (design §8.3). A pure function
//! of the servers' last reports, so it is tested without any of them.
//!
//! - An app is placed once (`apps.node_id`, sticky) so volumes stay put;
//!   unpinned, the usable server with the most memory to spare wins, ties
//!   to the main server.
//! - Usable: heard from in the last 2 minutes, not drained, Docker up, not
//!   paused by the power guard, not on a battery below 50 %.
//! - Replicas spread over usable servers, the app's own first; apps with
//!   volumes keep every replica on their server (the data is there).
//! - Compose apps run on the main server (their checkout is there).
//! - [`Reservations`] shares the durable capacity ledger with other operations.
//!   Concurrent placements cannot spend the same measured headroom; completion
//!   waits for fresh inventory before releasing unseen allocations.

use crate::{capacity, Error, Result, Store};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use vercelify_core::platform::PlatformNode;

/// Placement promises use the engine's durable Store, shared with other jobs.
pub struct Reservations {
    store: Arc<Store>,
    refresh: Option<Arc<tokio::sync::Notify>>,
}
impl Default for Reservations {
    fn default() -> Self {
        Self::new(Arc::new(Store::open_in_memory().expect("standalone placement store")))
    }
}
impl Reservations {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store, refresh: None }
    }
    pub fn with_refresh(mut self, refresh: Arc<tokio::sync::Notify>) -> Self {
        self.refresh = Some(refresh);
        self
    }
    pub fn place(&self, deployment: &str, nodes: &[PlatformNode], need: &Need) -> Result<Vec<Slot>> {
        self.place_at(deployment, nodes, need, chrono::Utc::now().timestamp())
    }
    fn place_at(&self, deployment: &str, nodes: &[PlatformNode], need: &Need, now: i64) -> Result<Vec<Slot>> {
        self.store.tx(|c| {
            for node in nodes {
                match &node.status.capacity {
                    Some(sample) => capacity::record(c,&node.id,sample)?,
                    None => capacity::invalidate(c,&node.id)?,
                }
            }
            let plan:Option<(String,String)>=c.query_row("SELECT need,slots FROM resource_placements WHERE operation=?1",[deployment],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let slots=if let Some((old,slots))=plan {
                if serde_json::from_str::<Need>(&old)?!=*need {return Err(Error::Conflict("the deployment's resource plan changed; retry it".into()));}
                serde_json::from_str::<Vec<Slot>>(&slots)?
            } else {
                let mut adjusted=nodes.to_vec();
                let mut unavailable=None;
                for node in &mut adjusted {
                    match capacity::account(c,&node.id,None,now) {
                        Ok(a) => {node.status.memory_available_mb=a.available_mb.min(a.workload_remaining_mb).saturating_sub(need.overhead_mb);}
                        Err(e) => {node.status.memory_total_mb=0;unavailable=Some(e);}
                    }
                }
                match place(&adjusted,need) {
                    Ok(slots) => slots,
                    Err(e) => return Err(if adjusted.iter().all(|n|n.status.memory_total_mb==0) { unavailable.unwrap_or(e) } else { e }),
                }
            };
            let mut per_node:HashMap<String,u64>=HashMap::new();
            let mb=need.memory_mb.map(u64::from).unwrap_or(DEFAULT_MEMORY_MB);
            for slot in &slots { *per_node.entry(slot.node_id.clone()).or_default()+=mb; }
            let mut requests:Vec<_>=per_node.into_iter().map(|(node_id,memory_mb)|capacity::Request {
                node_id,owner:deployment.into(),kind:"deploy".into(),memory_mb,disk_mb:0,heavy:false
            }).collect();
            for slot in &slots {
                let owner=format!("transient:{deployment}");
                if requests.iter().any(|r|r.node_id==slot.node_id && r.owner==owner) {continue;}
                if need.overhead_mb>0 || need.disk_mb>0 {
                    requests.push(capacity::Request {node_id:slot.node_id.clone(),owner,kind:"deploy".into(),memory_mb:need.overhead_mb,disk_mb:need.disk_mb,heavy:true});
                }
            }
            requests.extend(need.extra.iter().cloned());
            capacity::reserve(c,deployment,&requests,now)?;
            c.execute("INSERT INTO resource_placements(operation,need,slots) VALUES(?1,?2,?3) ON CONFLICT(operation) DO NOTHING",params![deployment,serde_json::to_string(need)?,serde_json::to_string(&slots)?])?;
            Ok(slots)
        })
    }
    pub fn finish(&self, deployment: &str, started: bool) {
        self.finish_at(deployment, started, chrono::Utc::now().timestamp());
    }
    fn finish_at(&self, deployment: &str, started: bool, now: i64) {
        if let Err(e) = self.store.tx(|c| capacity::finish(c, deployment, started, now)) {
            tracing::warn!("capacity reservation retained after completion: {e}");
        }
        if let Some(refresh) = &self.refresh {
            refresh.notify_one();
        }
    }
    pub fn on(&self, node: &str) -> Result<u64> {
        self.store.with(|c| {
            Ok(c.query_row(
                "SELECT coalesce(sum(memory_mb),0) FROM resource_reservations WHERE node_id=?1",
                [node],
                |r| r.get(0),
            )?)
        })
    }
}

/// Memory an app is assumed to need when it sets no limit.
pub const DEFAULT_MEMORY_MB: u64 = 256;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Need {
    /// `apps.node_id`.
    pub pinned: Option<String>,
    pub replicas: u8,
    pub memory_mb: Option<u32>,
    pub overhead_mb: u64,
    pub disk_mb: u64,
    pub extra: Vec<capacity::Request>,
    /// Compose apps.
    pub main_only: bool,
    /// The app has volumes: one server holds them all.
    pub one_node: bool,
    /// `runtime.failover`: may leave a pinned server that is down.
    pub failover: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub node_id: String,
    pub replica: u8,
}

/// Why a server can't take instances now (None: it can).
pub fn unusable(n: &PlatformNode) -> Option<String> {
    if !n.online {
        return Some(format!("{} is offline (not heard from in 2 minutes)", n.name));
    }
    if !n.status.docker_ok {
        return Some(format!("Docker isn't running on {}", n.name));
    }
    if n.status.memory_total_mb == 0 {
        return Some(format!("{} has no measured memory capacity yet", n.name));
    }
    if n.status.thermal_paused {
        return Some(format!("{} is too hot (the power guard paused it)", n.name));
    }
    if n.status.on_battery && n.status.battery_percent.is_some_and(|p| p < 50) {
        return Some(format!("{} is on battery below 50 %", n.name));
    }
    None
}

fn score(n: &PlatformNode, need: &Need) -> i128 {
    let want = need.memory_mb.map(u64::from).unwrap_or(DEFAULT_MEMORY_MB);
    i128::from(n.status.memory_available_mb) - i128::from(want)
}

fn capacity(n: &PlatformNode, need: &Need, instances: u8) -> Result<()> {
    if let Some(why) = unusable(n) {
        return Err(Error::Unavailable(why));
    }
    let want = need.memory_mb.map(u64::from).unwrap_or(DEFAULT_MEMORY_MB) * u64::from(instances);
    if want > n.status.memory_available_mb {
        return Err(Error::Unavailable(format!(
            "{} needs {want} MiB for this operation, but only {} MiB is available after reservations; queued until capacity is available",
            n.name, n.status.memory_available_mb
        )));
    }
    Ok(())
}

/// Usable, undrained servers, best first (ties: the main server, then name).
fn ranked<'a>(nodes: &'a [PlatformNode], need: &Need) -> Vec<&'a PlatformNode> {
    let mut v: Vec<&PlatformNode> = nodes.iter().filter(|n| !n.drained && unusable(n).is_none()).collect();
    v.sort_by(|a, b| {
        score(b, need)
            .cmp(&score(a, need))
            .then((b.role == "main").cmp(&(a.role == "main")))
            .then(a.name.cmp(&b.name))
    });
    v
}

pub fn place(nodes: &[PlatformNode], need: &Need) -> Result<Vec<Slot>> {
    let main = nodes
        .iter()
        .find(|n| n.role == "main")
        .ok_or_else(|| Error::Unavailable("the main server isn't registered yet".into()))?;
    let replicas = need.replicas.clamp(1, 32);
    let one = |id: &str| -> Vec<Slot> {
        (0..replicas)
            .map(|r| Slot {
                node_id: id.to_string(),
                replica: r,
            })
            .collect()
    };
    if need.main_only {
        capacity(main, need, 1)?;
        return Ok(vec![Slot {
            node_id: main.id.clone(),
            replica: 0,
        }]);
    }
    let ranked = ranked(nodes, need);
    let primary: &PlatformNode = match &need.pinned {
        Some(id) => {
            let n = nodes
                .iter()
                .find(|n| &n.id == id)
                .ok_or_else(|| Error::Invalid(format!("the app runs on {id}, which isn't a server any more")))?;
            match unusable(n) {
                None => n,
                Some(why) if need.failover && !need.one_node => match ranked.first() {
                    Some(other) => {
                        tracing::warn!("{why}: placing on {} instead (failover)", other.name);
                        other
                    }
                    None => return Err(Error::Unavailable(format!("{why}, and no other server can take it"))),
                },
                Some(why) => return Err(Error::Unavailable(why)),
            }
        }
        None => *ranked
            .first()
            .ok_or_else(|| Error::Unavailable("no server can take the app right now".into()))?,
    };
    if need.one_node {
        capacity(primary, need, replicas)?;
        return Ok(one(&primary.id));
    }
    capacity(primary, need, 1)?;
    let mut order: Vec<&PlatformNode> = vec![primary];
    order.extend(ranked.iter().filter(|n| n.id != primary.id));
    let want = need.memory_mb.map(u64::from).unwrap_or(DEFAULT_MEMORY_MB);
    let mut remaining: HashMap<&str, u64> = order
        .iter()
        .map(|n| (n.id.as_str(), n.status.memory_available_mb))
        .collect();
    let mut slots = Vec::with_capacity(usize::from(replicas));
    for replica in 0..replicas {
        let node = (0..order.len())
            .map(|offset| order[(usize::from(replica) + offset) % order.len()])
            .find(|n| remaining[n.id.as_str()] >= want)
            .ok_or_else(|| {
                Error::Unavailable(
                    "not enough measured memory for all replicas; queued until capacity is available".into(),
                )
            })?;
        *remaining.get_mut(node.id.as_str()).unwrap() -= want;
        slots.push(Slot {
            node_id: node.id.clone(),
            replica,
        });
    }
    Ok(slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vercelify_core::platform::NodeStatus;

    fn node(id: &str, role: &str, mem: u64) -> PlatformNode {
        PlatformNode {
            id: id.into(),
            name: id.into(),
            role: role.into(),
            agent_url: String::new(),
            arch: "x86_64".into(),
            builds: role == "main",
            last_seen_at: None,
            created_at: String::new(),
            mesh_ip: String::new(),
            online: true,
            drained: false,
            status: NodeStatus {
                capacity: (mem > 0).then(|| capacity::Sample {
                    epoch: "test".into(),
                    measured_at: chrono::Utc::now().timestamp(),
                    total_mb: mem + 512,
                    available_mb: mem,
                    workload_limit_mb: mem + 512 - capacity::os_reserve(mem + 512),
                    host_ready: true,
                    ..Default::default()
                }),
                memory_total_mb: if mem == 0 { 0 } else { mem + 512 },
                memory_available_mb: mem,
                docker_ok: true,
                ..NodeStatus::default()
            },
            instances: 0,
        }
    }

    #[test]
    fn concurrent_deploys_do_not_spend_the_same_memory() {
        // main has 1000 MB free, pi 900: the first 512 MB app goes to main;
        // without the reservation the second would too (main still reports
        // 1000), with it main has 488 left and pi wins.
        let nodes = vec![node("main", "main", 1000), node("pi", "worker", 900)];
        let need = Need {
            replicas: 1,
            memory_mb: Some(512),
            ..Need::default()
        };
        let r = Reservations::default();
        let t0 = chrono::Utc::now().timestamp();
        assert_eq!(ids(&r.place_at("d1", &nodes, &need, t0).unwrap()), ["main"]);
        assert_eq!(ids(&r.place_at("d2", &nodes, &need, t0).unwrap()), ["pi"]);
        assert_eq!(r.on("main").unwrap(), 512);

        // Placing the same deployment again doesn't count itself.
        assert_eq!(ids(&r.place_at("d1", &nodes, &need, t0).unwrap()), ["main"]);

        // Failed work releases immediately; started work waits for inventory.
        r.finish_at("d2", false, t0);
        assert_eq!(r.on("pi").unwrap(), 0);
        r.finish_at("d1", true, t0);
        assert_eq!(ids(&r.place_at("d3", &nodes, &need, t0).unwrap()), ["pi"]);
        r.finish_at("d3", false, t0);
        let later = t0 + 1;
        let mut fresh = nodes.clone();
        for node in &mut fresh {
            node.status.capacity.as_mut().unwrap().measured_at = later;
        }
        assert_eq!(ids(&r.place_at("d4", &fresh, &need, later).unwrap()), ["main"]);
    }

    fn ids(s: &[Slot]) -> Vec<&str> {
        s.iter().map(|s| s.node_id.as_str()).collect()
    }

    #[test]
    fn most_free_memory_wins_ties_to_main() {
        let nodes = vec![node("main", "main", 2000), node("pi", "worker", 3000)];
        let need = Need {
            replicas: 1,
            ..Need::default()
        };
        assert_eq!(ids(&place(&nodes, &need).unwrap()), ["pi"]);
        let even = vec![node("pi", "worker", 2000), node("main", "main", 2000)];
        assert_eq!(ids(&place(&even, &need).unwrap()), ["main"]);
    }

    #[test]
    fn unusable_servers_are_skipped() {
        let mut hot = node("hot", "worker", 9000);
        hot.status.thermal_paused = true;
        let mut off = node("off", "worker", 9000);
        off.online = false;
        let mut low = node("low", "worker", 9000);
        low.status.on_battery = true;
        low.status.battery_percent = Some(30);
        let mut drained = node("drained", "worker", 9000);
        drained.drained = true;
        let mut nodocker = node("nodocker", "worker", 9000);
        nodocker.status.docker_ok = false;
        let nodes = vec![node("main", "main", 512), hot, off, low, drained, nodocker];
        let need = Need {
            replicas: 1,
            ..Need::default()
        };
        assert_eq!(ids(&place(&nodes, &need).unwrap()), ["main"]);
    }

    #[test]
    fn pinned_apps_stay_or_fail_over() {
        let mut pi = node("pi", "worker", 3000);
        pi.online = false;
        let nodes = vec![node("main", "main", 2000), pi];
        let mut need = Need {
            pinned: Some("pi".into()),
            replicas: 1,
            ..Need::default()
        };
        let err = place(&nodes, &need).unwrap_err();
        assert!(err.to_string().contains("offline"), "{err}");
        need.failover = true;
        assert_eq!(ids(&place(&nodes, &need).unwrap()), ["main"]);
        need.one_node = true;
        assert!(place(&nodes, &need).is_err(), "volumes don't fail over");
        // A drained server keeps what is pinned to it.
        let mut d = node("pi", "worker", 3000);
        d.drained = true;
        let nodes = vec![node("main", "main", 2000), d];
        need.failover = false;
        assert_eq!(ids(&place(&nodes, &need).unwrap()), ["pi"]);
        assert!(place(
            &nodes,
            &Need {
                pinned: Some("gone".into()),
                ..Need::default()
            }
        )
        .is_err());
    }

    #[test]
    fn replicas_spread_unless_volumes_or_compose() {
        let nodes = vec![
            node("main", "main", 2000),
            node("a", "worker", 4000),
            node("b", "worker", 3000),
        ];
        let need = Need {
            pinned: Some("b".into()),
            replicas: 4,
            ..Need::default()
        };
        assert_eq!(ids(&place(&nodes, &need).unwrap()), ["b", "a", "main", "b"]);
        let slots = place(
            &nodes,
            &Need {
                one_node: true,
                ..need.clone()
            },
        )
        .unwrap();
        assert_eq!(ids(&slots), ["b", "b", "b", "b"]);
        assert_eq!(slots[3].replica, 3);
        assert_eq!(
            ids(&place(
                &nodes,
                &Need {
                    main_only: true,
                    ..need
                }
            )
            .unwrap()),
            ["main"]
        );
    }

    #[test]
    fn admission_refuses_unknown_main_capacity() {
        let nodes = vec![node("main", "main", 0)];
        let error = place(
            &nodes,
            &Need {
                replicas: 1,
                ..Need::default()
            },
        );
        assert!(error.is_err(), "unmeasured main must not be treated as unlimited");
    }

    #[test]
    fn admission_checks_pinned_and_compose_memory_shortfalls() {
        let mut main = node("main", "main", 64);
        main.status.memory_total_mb = 1024;
        let nodes = vec![main];
        for need in [
            Need {
                pinned: Some("main".into()),
                memory_mb: Some(256),
                replicas: 1,
                ..Need::default()
            },
            Need {
                main_only: true,
                memory_mb: Some(256),
                replicas: 1,
                ..Need::default()
            },
        ] {
            assert!(
                place(&nodes, &need).is_err(),
                "explicit placement cannot bypass capacity"
            );
        }
    }

    #[test]
    fn admission_counts_all_replicas_on_a_volume_owner() {
        let mut main = node("main", "main", 400);
        main.status.memory_total_mb = 2048;
        let need = Need {
            one_node: true,
            replicas: 2,
            memory_mb: Some(256),
            ..Need::default()
        };
        assert!(
            place(&[main], &need).is_err(),
            "two instances need more than one instance's memory"
        );
    }
}

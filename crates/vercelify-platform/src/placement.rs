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
//! - Servers report every minute, so two deploys close together would both
//!   see the same free memory: [`Reservations`] holds what each placed
//!   deployment needs until the servers' reports can include it.

use crate::{Error, Result};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use vercelify_core::platform::PlatformNode;

/// How long a finished deployment's reservation outlives it: past the next
/// report from its servers (every minute), which then counts it.
pub const RESERVATION_GRACE: Duration = Duration::from_secs(90);

/// Memory promised to deployments being placed or just started, by server.
/// In memory on purpose: after a restart nothing is mid-placement and the
/// servers' reports are the truth again.
#[derive(Default)]
pub struct Reservations {
    held: Mutex<HashMap<String, Held>>,
}

struct Held {
    /// (server, MB) per instance.
    slots: Vec<(String, u64)>,
    /// None while the deployment runs.
    until: Option<Instant>,
}

impl Reservations {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Held>> {
        self.held.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Place `deployment` against the servers' reports minus what others
    /// hold, and hold its share, in one step: a second deploy placed at the
    /// same moment sees it.
    pub fn place(&self, deployment: &str, nodes: &[PlatformNode], need: &Need) -> Result<Vec<Slot>> {
        self.place_at(deployment, nodes, need, Instant::now())
    }

    fn place_at(&self, deployment: &str, nodes: &[PlatformNode], need: &Need, now: Instant) -> Result<Vec<Slot>> {
        let mut held = self.lock();
        held.retain(|_, h| h.until.is_none_or(|u| u > now));
        let mut by_node: HashMap<&str, u64> = HashMap::new();
        for (id, h) in held.iter() {
            if id == deployment {
                continue;
            }
            for (n, mb) in &h.slots {
                *by_node.entry(n.as_str()).or_default() += mb;
            }
        }
        let adjusted: Vec<PlatformNode> = nodes
            .iter()
            .map(|n| {
                let mut n = n.clone();
                let r = by_node.get(n.id.as_str()).copied().unwrap_or(0);
                n.status.memory_available_mb = n.status.memory_available_mb.saturating_sub(r);
                n
            })
            .collect();
        let slots = place(&adjusted, need)?;
        let mb = need.memory_mb.map(u64::from).unwrap_or(DEFAULT_MEMORY_MB);
        held.insert(
            deployment.to_string(),
            Held {
                slots: slots.iter().map(|s| (s.node_id.clone(), mb)).collect(),
                until: None,
            },
        );
        Ok(slots)
    }

    /// The deployment finished (any outcome): keep its share a little longer
    /// if it started containers, else drop it now.
    pub fn finish(&self, deployment: &str, started: bool) {
        self.finish_at(deployment, started, Instant::now())
    }

    fn finish_at(&self, deployment: &str, started: bool, now: Instant) {
        let mut held = self.lock();
        if started {
            if let Some(h) = held.get_mut(deployment) {
                h.until = Some(now + RESERVATION_GRACE);
            }
        } else {
            held.remove(deployment);
        }
    }

    /// MB held on a server now (for tests and diagnostics).
    pub fn on(&self, node_id: &str) -> u64 {
        let now = Instant::now();
        self.lock()
            .values()
            .filter(|h| h.until.is_none_or(|u| u > now))
            .flat_map(|h| h.slots.iter())
            .filter(|(n, _)| n == node_id)
            .map(|(_, mb)| mb)
            .sum()
    }
}

/// Memory an app is assumed to need when it sets no limit.
pub const DEFAULT_MEMORY_MB: u64 = 256;

#[derive(Debug, Clone, Default)]
pub struct Need {
    /// `apps.node_id`.
    pub pinned: Option<String>,
    pub replicas: u8,
    pub memory_mb: Option<u32>,
    /// Compose apps.
    pub main_only: bool,
    /// The app has volumes: one server holds them all.
    pub one_node: bool,
    /// `runtime.failover`: may leave a pinned server that is down.
    pub failover: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub node_id: String,
    pub replica: u8,
}

/// Why a server can't take instances now (None: it can).
pub fn unusable(n: &PlatformNode) -> Option<String> {
    if !n.online {
        return Some(format!("{} is offline (not heard from in 2 minutes)", n.name));
    }
    if n.role != "main" && !n.status.docker_ok {
        return Some(format!("Docker isn't running on {}", n.name));
    }
    if n.status.thermal_paused {
        return Some(format!("{} is too hot (the power guard paused it)", n.name));
    }
    if n.status.on_battery && n.status.battery_percent.is_some_and(|p| p < 50) {
        return Some(format!("{} is on battery below 50 %", n.name));
    }
    None
}

fn score(n: &PlatformNode, need: &Need) -> i64 {
    // The main server before its first report: it wins, as it did when it
    // was the only one.
    if n.role == "main" && n.status.memory_total_mb == 0 && n.status.memory_available_mb == 0 {
        return i64::MAX / 4;
    }
    let want = need.memory_mb.map(u64::from).unwrap_or(DEFAULT_MEMORY_MB) as i64;
    n.status.memory_available_mb as i64 - want
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
        return Ok(one(&primary.id));
    }
    let mut order: Vec<&PlatformNode> = vec![primary];
    order.extend(ranked.iter().filter(|n| n.id != primary.id));
    Ok((0..replicas)
        .map(|r| Slot {
            node_id: order[r as usize % order.len()].id.clone(),
            replica: r,
        })
        .collect())
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
        let t0 = Instant::now();
        assert_eq!(ids(&r.place_at("d1", &nodes, &need, t0).unwrap()), ["main"]);
        assert_eq!(ids(&r.place_at("d2", &nodes, &need, t0).unwrap()), ["pi"]);
        assert_eq!(r.on("main"), 512);

        // Placing the same deployment again doesn't count itself.
        assert_eq!(ids(&r.place_at("d1", &nodes, &need, t0).unwrap()), ["main"]);

        // A failed deploy frees its share at once; a started one after the
        // grace period (by then the servers' reports include it).
        r.finish_at("d2", false, t0);
        assert_eq!(r.on("pi"), 0);
        r.finish_at("d1", true, t0);
        assert_eq!(ids(&r.place_at("d3", &nodes, &need, t0).unwrap()), ["pi"]);
        r.finish_at("d3", false, t0);
        let later = t0 + RESERVATION_GRACE + Duration::from_secs(1);
        assert_eq!(ids(&r.place_at("d4", &nodes, &need, later).unwrap()), ["main"]);
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
        let nodes = vec![node("main", "main", 100), hot, off, low, drained, nodocker];
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
}

//! One durable capacity ledger shared by placement and heavyweight operations.
//! Availability is measured demand; the workload ceiling is configured demand.
//! They are checked separately, so the OS reserve is never subtracted twice.
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const POLICY_VERSION: u32 = 1;
pub const MAX_AGE_SECONDS: i64 = 90;
/// Refusals that only mean "no fresh measurement yet" (after a boot or an
/// agent restart): the reconciler retries these soon instead of a full tick.
pub const NO_SAMPLE: &str = "no capacity sample is available";
pub const STALE_SAMPLE: &str = "the capacity sample is stale; waiting for a fresh report";
pub const SAFETY_MB: u64 = 128;
pub const DISK_RECOVERY_MB: u64 = 512;

pub use vercelify_core::platform::{
    CapacityInventory as Inventory, CapacitySample as Sample, MemoryAccount as Account,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub node_id: String,
    /// Container ownership label (deployment, database or dedicated builder).
    pub owner: String,
    pub kind: String,
    pub memory_mb: u64,
    pub disk_mb: u64,
    pub heavy: bool,
}
#[derive(Debug, Clone)]
struct Held {
    operation: String,
    request: Request,
    settled: Option<i64>,
    recovering: bool,
}

/// Whether an error message is a missing or stale sample refusal.
pub fn awaiting_sample(message: &str) -> bool {
    message.contains(NO_SAMPLE) || message.contains(STALE_SAMPLE)
}
fn refusal(reason: impl Into<String>) -> Error {
    Error::Capacity(format!("capacity: {}; running services were left alone", reason.into()))
}
pub fn os_reserve(total_mb: u64) -> u64 {
    if total_mb < 2500 {
        384
    } else {
        1024
    }
}
pub fn default_app_mb(total_mb: u64) -> u32 {
    if (896..=1280).contains(&total_mb) {
        192
    } else {
        256
    }
}
pub fn transfer_mb(total_mb: u64) -> u64 {
    if (896..=1280).contains(&total_mb) {
        48
    } else {
        128
    }
}

/// Owns one admitted operation; cancellation and errors retain any started
/// allocation until a fresh inventory can account for it.
pub struct Permit {
    store: std::sync::Arc<crate::Store>,
    operation: String,
    started: bool,
    refresh: std::sync::Arc<tokio::sync::Notify>,
}
impl Permit {
    pub fn after_reserved(
        store: std::sync::Arc<crate::Store>,
        operation: String,
        refresh: std::sync::Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            store,
            operation,
            started: false,
            refresh,
        }
    }
    pub fn resize(&self, requests: &[Request]) -> Result<()> {
        self.store
            .tx(|c| resize(c, &self.operation, requests, chrono::Utc::now().timestamp()))
    }
    pub fn started(&mut self) {
        self.started = true;
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if let Err(e) = self
            .store
            .tx(|c| finish(c, &self.operation, self.started, chrono::Utc::now().timestamp()))
        {
            tracing::warn!("resource promise retained: {e}");
        }
        self.refresh.notify_one();
    }
}

pub fn build_memory_mb(raw: Option<&str>) -> Result<u64> {
    let Some(raw) = raw else {
        return Ok(2048);
    };
    let raw = raw.trim().to_ascii_lowercase();
    for (suffix, multiplier) in [
        ("gib", 1024u64),
        ("gb", 1024),
        ("g", 1024),
        ("mib", 1),
        ("mb", 1),
        ("m", 1),
    ] {
        if let Some(number) = raw.strip_suffix(suffix) {
            if let Some(memory) = number
                .parse::<u64>()
                .ok()
                .and_then(|m| m.checked_mul(multiplier))
                .filter(|m| *m > 0)
            {
                return Ok(memory);
            }
            break;
        }
    }
    if let Some(bytes) = raw.parse::<u64>().ok().filter(|m| *m > 0) {
        return Ok(bytes.div_ceil(1_048_576));
    }
    Err(Error::Invalid(
        "build_memory must declare a positive whole MiB or GiB limit (for example 2048m or 2g)".into(),
    ))
}

fn fresh(s: &Sample, now: i64) -> Result<()> {
    if s.epoch.is_empty()
        || !s.host_ready
        || s.total_mb < 896
        || s.available_mb > s.total_mb
        || s.workload_limit_mb == 0
    {
        return Err(refusal(
            "memory, workload grant or host-manager measurements are unavailable",
        ));
    }
    if !(-5..=MAX_AGE_SECONDS).contains(&(now - s.measured_at)) {
        return Err(refusal(STALE_SAMPLE));
    }
    Ok(())
}
pub fn record(c: &Connection, node: &str, sample: &Sample) -> Result<()> {
    let old: Option<String> = c
        .query_row("SELECT sample FROM capacity_samples WHERE node_id=?1", [node], |r| {
            r.get(0)
        })
        .optional()?;
    if let Some(raw) = old {
        let old: Sample = serde_json::from_str(&raw)?;
        if sample.measured_at < old.measured_at {
            return Err(refusal("an older capacity report arrived after a newer one"));
        }
    }
    c.execute("INSERT INTO capacity_samples(node_id,sample) VALUES(?1,?2) ON CONFLICT(node_id) DO UPDATE SET sample=excluded.sample",params![node,serde_json::to_string(sample)?])?;
    Ok(())
}
pub fn invalidate(c: &Connection, node: &str) -> Result<()> {
    c.execute("DELETE FROM capacity_samples WHERE node_id=?1", [node])?;
    Ok(())
}
pub fn sample(c: &Connection, node: &str) -> Result<Sample> {
    let raw: Option<String> = c
        .query_row("SELECT sample FROM capacity_samples WHERE node_id=?1", [node], |r| {
            r.get(0)
        })
        .optional()?;
    serde_json::from_str(&raw.ok_or_else(|| refusal(NO_SAMPLE))?).map_err(Error::from)
}
fn held(c: &Connection, node: &str) -> Result<Vec<Held>> {
    let mut st=c.prepare("SELECT operation,owner,kind,memory_mb,disk_mb,heavy,settled_at,recovering FROM resource_reservations WHERE node_id=?1")?;
    let rows = st
        .query_map([node], |r| {
            Ok(Held {
                operation: r.get(0)?,
                request: Request {
                    node_id: node.into(),
                    owner: r.get(1)?,
                    kind: r.get(2)?,
                    memory_mb: r.get(3)?,
                    disk_mb: r.get(4)?,
                    heavy: r.get(5)?,
                },
                settled: r.get(6)?,
                recovering: r.get(7)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(rows)
}
fn ceiling(i: &Inventory) -> u64 {
    if !i.running {
        return 0;
    }
    match i.category.as_str() {
        "app" | "database" => i
            .limit_mb
            .filter(|m| *m > 0)
            .unwrap_or_else(|| i.usage_mb.unwrap_or(0).max(256)),
        "build" => i.usage_mb.unwrap_or(256),
        _ => 0,
    }
}
/// Release completed promises only after a fresh sample can include their work.
/// Interrupted heavy work additionally waits for the actual processes to stop.
fn reconcile(c: &Connection, node: &str, s: &Sample, now: i64) -> Result<()> {
    fresh(s, now)?;
    for h in held(c, node)? {
        let busy = h.recovering
            && h.request.heavy
            && (s.external_heavy
                || (h.request.kind == "build" && s.containers.iter().any(|i| i.running && i.category == "build")));
        if h.settled.is_some_and(|t| t < s.measured_at) && !busy {
            c.execute(
                "DELETE FROM resource_reservations WHERE operation=?1 AND node_id=?2 AND owner=?3",
                params![h.operation, node, h.request.owner],
            )?;
        }
    }
    Ok(())
}
pub fn account(c: &Connection, node: &str, exclude: Option<&str>, now: i64) -> Result<Account> {
    let s = sample(c, node)?;
    reconcile(c, node, &s, now)?;
    let promises = held(c, node)?;
    let mut additional = 0u64;
    let mut disk = 0u64;
    let mut heavy = s.external_heavy;
    for h in promises.iter().filter(|h| Some(h.operation.as_str()) != exclude) {
        let visible: Vec<_> = s
            .containers
            .iter()
            .filter(|i| i.running && i.owner == h.request.owner)
            .collect();
        let caps = visible.iter().fold(0u64, |sum, i| sum.saturating_add(ceiling(i)));
        additional = additional.saturating_add(h.request.memory_mb.saturating_sub(caps));
        disk = disk.saturating_add(h.request.disk_mb);
        heavy |= h.request.heavy;
    }
    let committed = s.containers.iter().fold(0u64, |sum, i| sum.saturating_add(ceiling(i)));
    // MemAvailable already includes observed usage. Preserve the unused part
    // of existing workload commitments as future demand, even after an
    // operation's promise has been reconciled away.
    let growth = s.containers.iter().fold(0u64, |sum, i| {
        sum.saturating_add(ceiling(i).saturating_sub(i.usage_mb.unwrap_or(0)))
    });
    let budget = s
        .workload_limit_mb
        .min(s.total_mb.saturating_sub(os_reserve(s.total_mb)))
        .saturating_sub(SAFETY_MB);
    Ok(Account {
        available_mb: s
            .available_mb
            .min(s.host_available_mb.unwrap_or(s.available_mb))
            .saturating_sub(SAFETY_MB)
            .saturating_sub(growth)
            .saturating_sub(additional),
        workload_remaining_mb: budget.saturating_sub(committed).saturating_sub(additional),
        reserved_mb: additional,
        growth_mb: growth,
        committed_mb: committed,
        disk_remaining_mb: s
            .disk_free_mb
            .map(|d| d.saturating_sub(DISK_RECOVERY_MB).saturating_sub(disk)),
        heavy,
    })
}
/// Caller uses Store::tx: all nodes and resources are promised atomically.
pub fn reserve(c: &Connection, operation: &str, requests: &[Request], now: i64) -> Result<()> {
    if requests.is_empty() {
        return Err(refusal("no operation resources were declared"));
    }
    let mut owners = BTreeSet::new();
    if requests
        .iter()
        .any(|r| !owners.insert((r.node_id.as_str(), r.owner.as_str())))
    {
        return Err(refusal("duplicate workload ownership in a resource request"));
    }
    let existing_nodes: Vec<String> = {
        let mut st = c.prepare("SELECT DISTINCT node_id FROM resource_reservations WHERE operation=?1")?;
        let nodes = st
            .query_map([operation], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        nodes
    };
    if !existing_nodes.is_empty() {
        let mut existing = Vec::new();
        for node in &existing_nodes {
            fresh(&sample(c, node)?, now)?;
            existing.extend(
                held(c, node)?
                    .into_iter()
                    .filter(|h| h.operation == operation)
                    .map(|h| h.request),
            );
        }
        if existing.len() != requests.len() || existing.iter().any(|r| !requests.contains(r)) {
            return Err(refusal("the operation's resource request changed"));
        }
        return Ok(());
    }
    let mut grouped: BTreeMap<&str, Vec<&Request>> = BTreeMap::new();
    for req in requests {
        if req.owner.is_empty() || req.memory_mb > i64::MAX as u64 || req.disk_mb > i64::MAX as u64 {
            return Err(refusal("invalid resource request"));
        }
        grouped.entry(&req.node_id).or_default().push(req);
    }
    for (node, rs) in &grouped {
        let a = account(c, node, Some(operation), now)?;
        let frame = sample(c, node)?;
        if frame.total_mb.min(frame.host_grant_mb.unwrap_or(frame.total_mb)) <= 1280
            && rs
                .iter()
                .any(|r| matches!(r.kind.as_str(), "build" | "database" | "restore"))
        {
            return Err(refusal(
                "the experimental 1 GiB profile is runtime-only: use a selected build computer and no database",
            ));
        }
        let existing = held(c, node)?;
        for req in rs {
            if existing
                .iter()
                .any(|h| h.request.owner == req.owner && h.operation != operation)
            {
                return Err(refusal("this workload already has an operation in flight"));
            }
            if existing
                .iter()
                .any(|h| h.operation == operation && h.request.owner == req.owner && &h.request != *req)
            {
                return Err(refusal("the operation's resource request changed"));
            }
        }
        let memory = rs.iter().fold(0u64, |sum, r| sum.saturating_add(r.memory_mb));
        let frame = sample(c, node)?;
        let mut measured_demand = 0u64;
        let mut configured_demand = 0u64;
        for req in rs {
            let visible: Vec<_> = frame
                .containers
                .iter()
                .filter(|i| i.running && i.owner == req.owner)
                .collect();
            let caps = visible.iter().fold(0u64, |sum, i| sum.saturating_add(ceiling(i)));
            measured_demand = measured_demand.saturating_add(req.memory_mb.saturating_sub(caps));
            configured_demand = configured_demand.saturating_add(req.memory_mb.saturating_sub(caps));
        }
        let disk = rs.iter().fold(0u64, |sum, r| sum.saturating_add(r.disk_mb));
        if measured_demand > a.available_mb || configured_demand > a.workload_remaining_mb {
            return Err(refusal(format!("requested {memory} MiB on {node}; {} MiB already reserved, {} MiB safe measured headroom, {} MiB workload budget left; waiting for capacity",a.reserved_mb,a.available_mb,a.workload_remaining_mb)));
        }
        if disk > 0 && a.disk_remaining_mb.is_none_or(|d| disk > d) {
            return Err(refusal(format!(
                "requested {disk} MiB temporary disk on {node}; waiting for disk headroom"
            )));
        }
        if rs.iter().any(|r| r.heavy) && a.heavy {
            return Err(refusal("another heavy operation is using the node's CPU/I/O slot"));
        }
    }
    for req in requests {
        c.execute("INSERT INTO resource_reservations(operation,node_id,owner,kind,memory_mb,disk_mb,heavy) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(operation,node_id,owner) DO NOTHING",params![operation,req.node_id,req.owner,req.kind,req.memory_mb,req.disk_mb,req.heavy])?;
    }
    Ok(())
}
/// Advance a live operation to a measured stage without changing its ownership.
/// The savepoint preserves the original promise even if the caller handles a
/// refusal inside its transaction rather than rolling that transaction back.
pub fn resize(c: &Connection, operation: &str, requests: &[Request], now: i64) -> Result<()> {
    let mut st = c.prepare("SELECT DISTINCT node_id FROM resource_reservations WHERE operation=?1")?;
    let nodes = st
        .query_map([operation], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut old = Vec::new();
    for node in nodes {
        old.extend(held(c, &node)?.into_iter().filter(|h| h.operation == operation));
    }
    if old.is_empty()
        || old.len() != requests.len()
        || old.iter().any(|h| {
            h.settled.is_some()
                || h.recovering
                || !requests.iter().any(|r| {
                    r.node_id == h.request.node_id
                        && r.owner == h.request.owner
                        && r.kind == h.request.kind
                        && r.heavy == h.request.heavy
                })
        })
    {
        return Err(refusal(
            "only the resources of the same active operation may be resized",
        ));
    }
    let placed: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM resource_placements WHERE operation=?1)",
        [operation],
        |r| r.get(0),
    )?;
    if placed {
        return Err(refusal("a deployment placement cannot be resized"));
    }
    c.execute_batch("SAVEPOINT capacity_resize")?;
    let result = (|| {
        c.execute("DELETE FROM resource_reservations WHERE operation=?1", [operation])?;
        reserve(c, operation, requests, now)
    })();
    if result.is_err() {
        c.execute_batch("ROLLBACK TO capacity_resize")?;
    }
    c.execute_batch("RELEASE capacity_resize")?;
    result
}

pub fn finish(c: &Connection, operation: &str, started: bool, now: i64) -> Result<()> {
    c.execute("DELETE FROM resource_placements WHERE operation=?1", [operation])?;
    if started {
        c.execute(
            "UPDATE resource_reservations SET settled_at=?2 WHERE operation=?1",
            params![operation, now],
        )?;
    } else {
        c.execute("DELETE FROM resource_reservations WHERE operation=?1", [operation])?;
    }
    Ok(())
}
pub fn recover(c: &Connection, now: i64) -> Result<()> {
    c.execute("DELETE FROM capacity_samples", [])?;
    c.execute("DELETE FROM resource_placements", [])?;
    c.execute("UPDATE nodes SET status=json_remove(status,'$.capacity')", [])?;
    c.execute(
        "UPDATE resource_reservations SET settled_at=?1,recovering=1 WHERE settled_at IS NULL",
        [now],
    )?;
    Ok(())
}

pub fn profile(total_mb: u64) -> &'static str {
    match total_mb {
        0..=895 => "Below the admission minimum",
        896..=1280 => "1 GiB experimental · runtime only",
        1281..=2500 => "2 GiB · runtime first",
        2501..=4500 => "4 GiB lab profile",
        _ => "Custom capacity",
    }
}

pub fn reports(c: &Connection, now: i64) -> Result<Vec<vercelify_core::platform::MemoryReport>> {
    crate::nodes::list(c)?
        .into_iter()
        .map(|node| {
            let observed = sample(c, &node.id).ok();
            let effective = observed
                .as_ref()
                .map(|s| s.total_mb.min(s.host_grant_mb.unwrap_or(s.total_mb)));
            let result = account(c, &node.id, None, now);
            let reason = result.as_ref().err().map(ToString::to_string);
            Ok(vercelify_core::platform::MemoryReport {
                node_id: node.id,
                name: node.name,
                policy_version: POLICY_VERSION,
                profile: effective.map(profile).unwrap_or("Capacity unknown").into(),
                sample_age_secs: observed.as_ref().map(|s| now - s.measured_at),
                os_reserve_mb: effective.map(os_reserve),
                safety_mb: SAFETY_MB,
                sample: observed,
                account: result.ok(),
                reason,
            })
        })
        .collect()
}

#[cfg(test)]
mod awaiting_tests {
    use super::*;

    #[test]
    fn only_missing_or_stale_samples_count_as_awaiting() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE capacity_samples(node_id TEXT PRIMARY KEY, sample TEXT)")
            .unwrap();
        let missing = sample(&c, "n1").unwrap_err().to_string();
        assert!(awaiting_sample(&missing), "{missing}");
        assert!(awaiting_sample(&refusal(STALE_SAMPLE).to_string()));
        assert!(!awaiting_sample(&refusal("not enough memory for this app").to_string()));
    }
}

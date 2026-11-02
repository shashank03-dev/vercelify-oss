//! Admission owns durable promises; node samples already include visible memory.
use std::sync::{Arc, Barrier};
use vercelify_platform::{
    capacity::{self, Inventory, Request, Sample},
    Store,
};

fn sample(time: i64) -> Sample {
    Sample {
        epoch: "boot-a".into(),
        measured_at: time,
        total_mb: 2048,
        available_mb: 800,
        workload_limit_mb: 1664,
        disk_free_mb: Some(1400),
        host_ready: true,
        external_heavy: false,
        containers: vec![],
        host_grant_mb: None,
        host_available_mb: None,
        provider_overhead_mb: None,
        host_oom_kills: None,
    }
}
fn request(owner: &str, memory: u64) -> Request {
    Request {
        node_id: "main".into(),
        owner: owner.into(),
        kind: "deploy".into(),
        memory_mb: memory,
        disk_mb: 0,
        heavy: false,
    }
}

#[test]
fn simultaneous_requests_cannot_spend_the_same_headroom() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.tx(|c| capacity::record(c, "main", &sample(100))).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|i| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let owner = format!("dpl_{i}");
                barrier.wait();
                store
                    .tx(|c| capacity::reserve(c, &owner, &[request(&owner, 400)], 100))
                    .is_ok()
            })
        })
        .collect();
    barrier.wait();
    let accepted = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .filter(|accepted| *accepted)
        .count();
    assert_eq!(accepted, 1);
}

#[test]
fn reservations_survive_reopening_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("platform.db");
    {
        let store = Store::open(&file).unwrap();
        store
            .tx(|c| {
                capacity::record(c, "main", &sample(100))?;
                capacity::reserve(c, "first", &[request("first", 500)], 100)
            })
            .unwrap();
    }
    let store = Store::open(&file).unwrap();
    assert!(store
        .tx(|c| capacity::reserve(c, "second", &[request("second", 400)], 101))
        .is_err());
    store.tx(|c| capacity::finish(c, "first", false, 101)).unwrap();
    store
        .tx(|c| capacity::reserve(c, "second", &[request("second", 400)], 101))
        .unwrap();
}

#[test]
fn missing_and_stale_samples_never_imply_free_capacity() {
    let store = Store::open_in_memory().unwrap();
    assert!(store
        .tx(|c| capacity::reserve(c, "missing", &[request("missing", 1)], 100))
        .is_err());
    store.tx(|c| capacity::record(c, "main", &sample(100))).unwrap();
    assert!(store
        .tx(|c| capacity::reserve(c, "stale", &[request("stale", 1)], 191))
        .is_err());
}

#[test]
fn fresh_inventory_does_not_charge_visible_memory_twice() {
    let store = Store::open_in_memory().unwrap();
    store
        .tx(|c| {
            capacity::record(c, "main", &sample(100))?;
            capacity::reserve(c, "first", &[request("first", 500)], 100)
        })
        .unwrap();
    let mut next = sample(101);
    next.available_mb = 400;
    next.containers.push(Inventory {
        id: "container-one".into(),
        owner: "first".into(),
        category: "app".into(),
        usage_mb: Some(400),
        limit_mb: Some(500),
        running: true,
        ..Inventory::default()
    });
    store.tx(|c| capacity::record(c, "main", &next)).unwrap();
    // 400 already appears in MemAvailable. Only 100 of the original promise
    // remains additional demand, so another 150 plus 128 safety fits in 400.
    store
        .tx(|c| capacity::reserve(c, "second", &[request("second", 150)], 101))
        .unwrap();
}

#[test]
fn started_work_waits_for_a_new_sample_instead_of_a_grace_timer() {
    let store = Store::open_in_memory().unwrap();
    store
        .tx(|c| {
            capacity::record(c, "main", &sample(100))?;
            capacity::reserve(c, "first", &[request("first", 500)], 100)?;
            capacity::finish(c, "first", true, 101)
        })
        .unwrap();
    assert!(store
        .tx(|c| capacity::reserve(c, "second", &[request("second", 400)], 102))
        .is_err());
    store.tx(|c| capacity::record(c, "main", &sample(103))).unwrap();
    store
        .tx(|c| capacity::reserve(c, "second", &[request("second", 400)], 103))
        .unwrap();
}

#[test]
fn temporary_disk_space_is_reserved_atomically_too() {
    let store = Store::open_in_memory().unwrap();
    store.tx(|c| capacity::record(c, "main", &sample(100))).unwrap();
    let mut first = request("first", 1);
    first.disk_mb = 600;
    let mut second = request("second", 1);
    second.disk_mb = 600;
    store.tx(|c| capacity::reserve(c, "first", &[first], 100)).unwrap();
    assert!(store.tx(|c| capacity::reserve(c, "second", &[second], 100)).is_err());
}

#[test]
fn restart_retains_unknown_allocations_until_fresh_reconciliation() {
    let store = Store::open_in_memory().unwrap();
    store
        .tx(|c| {
            capacity::record(c, "main", &sample(100))?;
            capacity::reserve(c, "first", &[request("first", 500)], 100)?;
            capacity::recover(c, 101)
        })
        .unwrap();
    assert!(store
        .tx(|c| capacity::reserve(c, "second", &[request("second", 400)], 102))
        .is_err());
    let mut fresh = sample(103);
    fresh.epoch = "boot-b".into();
    store.tx(|c| capacity::record(c, "main", &fresh)).unwrap();
    store
        .tx(|c| capacity::reserve(c, "second", &[request("second", 400)], 103))
        .unwrap();
}

#[test]
fn reordered_reports_cannot_restore_old_free_memory() {
    let store = Store::open_in_memory().unwrap();
    store.tx(|c| capacity::record(c, "main", &sample(101))).unwrap();
    assert!(store.tx(|c| capacity::record(c, "main", &sample(100))).is_err());
}

#[test]
fn an_operation_cannot_reuse_its_identity_for_another_workload() {
    let store = Store::open_in_memory().unwrap();
    store
        .tx(|c| {
            capacity::record(c, "main", &sample(100))?;
            capacity::reserve(c, "operation", &[request("first", 400)], 100)
        })
        .unwrap();
    assert!(store
        .tx(|c| capacity::reserve(c, "operation", &[request("second", 400)], 100))
        .is_err());
}

#[test]
fn a_duplicate_owner_in_one_request_cannot_hide_a_promise() {
    let store = Store::open_in_memory().unwrap();
    store.tx(|c| capacity::record(c, "main", &sample(100))).unwrap();
    let req = request("same", 200);
    assert!(store
        .tx(|c| capacity::reserve(c, "operation", &[req.clone(), req], 100))
        .is_err());
}

#[test]
fn an_idempotent_retry_does_not_reserve_visible_memory_again() {
    let store = Store::open_in_memory().unwrap();
    let req = request("first", 500);
    store
        .tx(|c| {
            capacity::record(c, "main", &sample(100))?;
            capacity::reserve(c, "first", std::slice::from_ref(&req), 100)
        })
        .unwrap();
    let mut next = sample(101);
    next.available_mb = 300;
    next.containers.push(Inventory {
        id: "container-one".into(),
        owner: "first".into(),
        category: "app".into(),
        usage_mb: Some(500),
        limit_mb: Some(500),
        running: true,
        ..Inventory::default()
    });
    store.tx(|c| capacity::record(c, "main", &next)).unwrap();
    store.tx(|c| capacity::reserve(c, "first", &[req], 101)).unwrap();
}

#[test]
fn resizing_an_active_stage_is_atomic_and_preserves_ownership() {
    let store = Store::open_in_memory().unwrap();
    let mut req = request("upload", 200);
    req.disk_mb = 200;
    store
        .tx(|c| {
            capacity::record(c, "main", &sample(100))?;
            capacity::reserve(c, "upload-op", &[req.clone()], 100)
        })
        .unwrap();
    let mut larger = req.clone();
    larger.disk_mb = 900; // only 888 MiB remains after the recovery allowance
    assert!(store.tx(|c| capacity::resize(c, "upload-op", &[larger], 100)).is_err());
    assert_eq!(
        store
            .tx(|c| capacity::account(c, "main", None, 100))
            .unwrap()
            .disk_remaining_mb,
        Some(688)
    );
    let mut alien = req.clone();
    alien.owner = "another-owner".into();
    assert!(store.tx(|c| capacity::resize(c, "upload-op", &[alien], 100)).is_err());
    req.disk_mb = 700;
    store.tx(|c| capacity::resize(c, "upload-op", &[req], 100)).unwrap();
    assert_eq!(
        store
            .tx(|c| capacity::account(c, "main", None, 100))
            .unwrap()
            .disk_remaining_mb,
        Some(188)
    );
}

#[test]
fn a_new_build_only_promises_demand_above_its_already_measured_builder() {
    let store = Store::open_in_memory().unwrap();
    let mut frame = sample(100);
    frame.available_mb = 400;
    frame.containers.push(Inventory {
        id: "buildkit".into(),
        owner: "builder".into(),
        category: "build".into(),
        running: true,
        usage_mb: Some(300),
        limit_mb: Some(500),
        ..Inventory::default()
    });
    store.tx(|c| capacity::record(c, "main", &frame)).unwrap();
    store
        .tx(|c| capacity::reserve(c, "next-build", &[request("builder", 500)], 100))
        .unwrap();
}

#[test]
fn one_gib_profile_refuses_databases_and_local_builds_even_when_idle() {
    for kind in ["build", "database", "restore"] {
        let store = Store::open_in_memory().unwrap();
        let mut frame = sample(100);
        frame.total_mb = 956;
        frame.available_mb = 800;
        frame.workload_limit_mb = 572;
        store.tx(|c| capacity::record(c, "main", &frame)).unwrap();
        let mut request = request("small", 100);
        request.kind = kind.into();
        assert!(
            store.tx(|c| capacity::reserve(c, "small", &[request], 100)).is_err(),
            "{kind}"
        );
    }
}

#[test]
fn unrelated_host_pressure_cannot_spend_the_unused_part_of_existing_caps() {
    let store = Store::open_in_memory().unwrap();
    let mut frame = sample(100);
    frame.containers.push(Inventory {
        id: "existing-app".into(),
        owner: "old".into(),
        category: "app".into(),
        running: true,
        usage_mb: Some(10),
        limit_mb: Some(500),
        ..Inventory::default()
    });
    store.tx(|c| capacity::record(c, "main", &frame)).unwrap();
    // 800 available - 128 safety - 490 growth promised to the old app = 182.
    assert!(store
        .tx(|c| capacity::reserve(c, "new", &[request("new", 300)], 100))
        .is_err());
}

use std::collections::BTreeMap;
use vercelify_core::platform::*;
use vercelify_platform::{
    apps,
    deploy_guard::{self, Observation, Policy},
    deployments, Store,
};
fn fixture() -> (Store, App, Deployment, Deployment) {
    let s = Store::open_in_memory().unwrap();
    let app = s
        .tx(|c| {
            apps::create(
                c,
                &serde_json::from_value(
                    serde_json::json!({"name":"guard","kind":"image","source":{"image":"example:v1"}}),
                )
                .unwrap(),
            )
        })
        .unwrap();
    let mut deps = vec![];
    for n in 1..=2 {
        let d = s
            .tx(|c| {
                let d = deployments::create(
                    c,
                    &deployments::NewDeployment {
                        app_id: &app.id,
                        pr_number: None,
                        branch: "main",
                        commit_sha: "",
                        commit_message: "",
                        trigger: DeploymentTrigger::Manual,
                        rollback_of: None,
                        image: &format!("example@sha256:{n:064x}"),
                        node_id: None,
                    },
                )?;
                deployments::transition(c, &d.id, DeploymentStatus::Deploying, None)?;
                let d = deployments::transition(c, &d.id, DeploymentStatus::Healthy, None)?;
                deployments::promote(c, &app.id, &d.id)?;
                deploy_guard::published(
                    c,
                    &d,
                    deps.last().map(|d: &Deployment| d.id.as_str()),
                    "compatible",
                    &BTreeMap::from([("node/container".into(), 0)]),
                    Policy {
                        compatible: true,
                        grace_secs: 10,
                        failure_limit: 3,
                    },
                    100,
                )?;
                Ok(d)
            })
            .unwrap();
        deps.push(d);
    }
    (s, app, deps.remove(0), deps.remove(0))
}
#[test]
fn a_restart_keeps_deadline_and_early_restart_baseline() {
    let (s, _, _, d) = fixture();
    let disk = tempfile::tempdir().unwrap();
    let path = disk.path().join("platform.db");
    s.backup_to(&path).unwrap();
    drop(s);
    let s = Store::open(&path).unwrap();
    let g = s.with(|c| deploy_guard::get(c, &d.id)).unwrap().unwrap();
    assert_eq!((g.deadline, g.grace_until, g.baseline["node/container"]), (280, 110, 0));
    // Duplicate resume/publication cannot extend an existing deadline.
    s.tx(|c| {
        deploy_guard::published(
            c,
            &d,
            Some(&g.previous_id),
            "compatible",
            &g.baseline,
            Policy {
                compatible: true,
                grace_secs: 10,
                failure_limit: 3,
            },
            200,
        )
    })
    .unwrap();
    assert_eq!(s.with(|c| deploy_guard::get(c, &d.id)).unwrap().unwrap().deadline, 280);
}
#[test]
fn only_consecutive_failures_or_a_crash_loop_request_once() {
    let (s, _, _, d) = fixture();
    let bad = || Observation::Failed {
        reason: "HTTP 503".into(),
        crash_loop: false,
    };
    assert!(!s.tx(|c| deploy_guard::observe(c, &d.id, bad(), 105)).unwrap());
    for now in [110, 120] {
        assert!(!s.tx(|c| deploy_guard::observe(c, &d.id, bad(), now)).unwrap());
    }
    assert!(!s
        .tx(|c| deploy_guard::observe(c, &d.id, Observation::Healthy, 130))
        .unwrap());
    assert!(!s
        .tx(|c| deploy_guard::observe(c, &d.id, Observation::Unknown, 140))
        .unwrap());
    assert!(s
        .tx(|c| deploy_guard::observe(
            c,
            &d.id,
            Observation::Failed {
                reason: "two added restarts".into(),
                crash_loop: true
            },
            150
        ))
        .unwrap());
    s.tx(|c| {
        deploy_guard::requestable(c, &d.id, 150)?;
        deploy_guard::requested(c, &d.id, &d.id, 150)
    })
    .unwrap();
    assert!(s.tx(|c| deploy_guard::requestable(c, &d.id, 151)).is_err());
    assert!(!s.tx(|c| deploy_guard::observe(c, &d.id, bad(), 160)).unwrap());
    assert_eq!(
        s.with(|c| deploy_guard::get(c, &d.id)).unwrap().unwrap().state,
        "requested"
    );
}
#[test]
fn a_new_job_or_intentional_stop_fences_the_guard() {
    let (s, app, _, d) = fixture();
    s.tx(|c| {
        deploy_guard::observe(
            c,
            &d.id,
            Observation::Failed {
                reason: "crash".into(),
                crash_loop: true,
            },
            150,
        )
    })
    .unwrap();
    s.tx(|c| {
        apps::patch(
            c,
            &app.id,
            &AppPatch {
                desired: Some(Desired::Stopped),
                ..Default::default()
            },
        )
    })
    .unwrap();
    assert!(s.tx(|c| deploy_guard::requestable(c, &d.id, 151)).is_err());
    s.tx(|c| {
        apps::patch(
            c,
            &app.id,
            &AppPatch {
                desired: Some(Desired::Running),
                ..Default::default()
            },
        )
    })
    .unwrap();
    assert!(s.tx(|c| deploy_guard::requestable(c, &d.id, 280)).is_err());
}

#[test]
fn three_bad_probes_request_and_terminal_outcomes_cannot_be_rewritten() {
    let (s, _, _, d) = fixture();
    for (now, ready) in [(110, false), (120, false), (130, true)] {
        assert_eq!(
            s.tx(|c| deploy_guard::observe(
                c,
                &d.id,
                Observation::Failed {
                    reason: "HTTP failed".into(),
                    crash_loop: false
                },
                now
            ))
            .unwrap(),
            ready
        );
    }
    s.tx(|c| deploy_guard::finish(c, &d.id, "failed", "rollback failed", 140))
        .unwrap();
    s.tx(|c| deploy_guard::finish(c, &d.id, "verified", "late callback", 150))
        .unwrap();
    assert_eq!(
        s.with(|c| deploy_guard::get(c, &d.id)).unwrap().unwrap().state,
        "failed"
    );
}
#[test]
fn intentional_configuration_change_is_not_a_compatible_rollback() {
    use vercelify_platform::{nodes, secrets::MasterKey};
    let (s, app, _, d) = fixture();
    let key = MasterKey::from_bytes(&[9; 32]).unwrap();
    s.tx(|c| nodes::ensure_main(c, "main", "http://127.0.0.1:7420", "aarch64"))
        .unwrap();
    let original = s.with(|c| deploy_guard::fingerprint(c, &key, &app.id, 8080)).unwrap();
    s.tx(|c| {
        deployments::set_port(c, &d.id, 8080)?;
        c.execute(
            "UPDATE deployment_configs SET fingerprint=?2 WHERE deployment_id=?1",
            rusqlite::params![d.id, original],
        )?;
        Ok(())
    })
    .unwrap();
    let guard = s.with(|c| deploy_guard::get(c, &d.id)).unwrap().unwrap();
    assert!(s.with(|c| deploy_guard::compatible(c, &key, &guard)).unwrap());
    let mut runtime = app.runtime.clone();
    runtime.memory_mb = Some(512);
    s.tx(|c| {
        apps::patch(
            c,
            &app.id,
            &AppPatch {
                runtime: Some(runtime),
                ..Default::default()
            },
        )
    })
    .unwrap();
    assert!(!s.with(|c| deploy_guard::compatible(c, &key, &guard)).unwrap());
}

#[test]
fn explicit_runtime_port_is_compatible_without_a_detected_port() {
    use vercelify_platform::{nodes, secrets::MasterKey};
    let (s, app, _, d) = fixture();
    let key = MasterKey::from_bytes(&[9; 32]).unwrap();
    s.tx(|c| nodes::ensure_main(c, "main", "http://127.0.0.1:7420", "aarch64"))
        .unwrap();
    let mut runtime = app.runtime.clone();
    runtime.port = Some(8080);
    s.tx(|c| {
        apps::patch(
            c,
            &app.id,
            &AppPatch {
                runtime: Some(runtime),
                ..Default::default()
            },
        )
    })
    .unwrap();
    let fingerprint = s.with(|c| deploy_guard::fingerprint(c, &key, &app.id, 8080)).unwrap();
    s.tx(|c| {
        c.execute(
            "UPDATE deployment_configs SET fingerprint=?2 WHERE deployment_id=?1",
            rusqlite::params![d.id, fingerprint],
        )?;
        Ok(())
    })
    .unwrap();
    let guard = s.with(|c| deploy_guard::get(c, &d.id)).unwrap().unwrap();
    assert!(s.with(|c| deploy_guard::compatible(c, &key, &guard)).unwrap());
}

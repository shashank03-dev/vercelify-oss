//! The job repository must fence stale owners/generations independently of HTTP.
use vercelify_core::platform::*;
use vercelify_platform::{apps, deployments, offhost, Store};

fn deployment(s: &Store) -> Deployment {
    s.tx(|c| {
        let a = apps::create(
            c,
            &AppCreate {
                name: "offhost".into(),
                kind: AppKind::Git,
                source: AppSource {
                    repository: Some("https://example.com/app.git".into()),
                    ..Default::default()
                },
                environment_id: None,
                project_id: None,
                build: None,
                runtime: None,
                previews: None,
                node_id: None,
                domains: vec![],
            },
        )?;
        deployments::create(
            c,
            &deployments::NewDeployment {
                app_id: &a.id,
                pr_number: None,
                branch: "main",
                commit_sha: &"a".repeat(40),
                commit_message: "",
                trigger: DeploymentTrigger::Manual,
                rollback_of: None,
                image: "",
                node_id: None,
            },
        )
    })
    .unwrap()
}

#[test]
fn current_owner_and_generation_are_required_and_expiry_is_durable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.db");
    let s = Store::open(&path).unwrap();
    let d = deployment(&s);
    let plan = LocalBuildPlan {
        dockerfile: "FROM scratch\n".into(),
        ..Default::default()
    };
    let job = s
        .tx(|c| offhost::create(c, &d.id, "device-a", "revision", "linux/arm64", &plan, 100))
        .unwrap();
    drop(s);
    let s = Store::open(&path).unwrap();
    assert!(!s.with(|c| offhost::dispatchable(c, &d.id)).unwrap());
    assert!(s.with(|c| offhost::authorize(c, &d.id, "device-b", 1, 101)).is_err());
    assert!(s.with(|c| offhost::authorize(c, &d.id, "device-a", 0, 101)).is_err());
    assert!(s
        .with(|c| offhost::authorize(c, &d.id, "device-a", job.generation, 101))
        .is_ok());
    s.tx(|c| offhost::renew(c, &d.id, "device-a", job.generation, 350))
        .unwrap();
    assert!(s
        .with(|c| offhost::authorize(c, &d.id, "device-a", job.generation, 500))
        .is_ok());
    s.tx(|c| offhost::expire(c, 700)).unwrap();
    assert!(s
        .with(|c| offhost::authorize(c, &d.id, "device-a", job.generation, 701))
        .is_err());
    assert_eq!(
        s.with(|c| deployments::get(c, &d.id)).unwrap().status,
        DeploymentStatus::Failed
    );
}

#[test]
fn newer_deployment_or_cancellation_fences_a_finishing_upload() {
    let s = Store::open_in_memory().unwrap();
    let d = deployment(&s);
    let plan = LocalBuildPlan::default();
    s.tx(|c| offhost::create(c, &d.id, "device", "revision", "linux/amd64", &plan, 100))
        .unwrap();
    s.tx(|c| deployments::transition(c, &d.id, DeploymentStatus::Cancelled, None))
        .unwrap();
    assert!(s.with(|c| offhost::authorize(c, &d.id, "device", 1, 101)).is_err());
    assert!(!s.with(|c| offhost::dispatchable(c, &d.id)).unwrap());
}

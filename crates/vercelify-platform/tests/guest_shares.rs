use vercelify_core::platform::*;
use vercelify_platform::{apps, deployments, guest_shares as g, previews, secrets::MasterKey, Store};
fn fixture() -> (Store, MasterKey, App) {
    let s = Store::open_in_memory().unwrap();
    let key = MasterKey::from_bytes(&[81; 32]).unwrap();
    s.tx(|c| {
        g::ensure_key(c, &key, 1000)?;
        g::configure(c, Some("guests.example.test"))?;
        Ok(())
    })
    .unwrap();
    s.tx(|c| vercelify_platform::nodes::ensure_main(c, "main", "http://127.0.0.1:7420", "aarch64"))
        .unwrap();
    let app = s
        .tx(|c| {
            apps::create(
                c,
                &serde_json::from_value(
                    serde_json::json!({"name":"shared","kind":"image","source":{"image":"example:v1"},"previews":{"enabled":true,"access":"private","max":3}}),
                )
                .unwrap(),
            )
        })
        .unwrap();
    s.tx(|c| {
        previews::upsert(c, &app.id, 7, "abc1234", "test")?;
        let main = vercelify_platform::nodes::main(c)?;
        let d = deployments::create(
            c,
            &deployments::NewDeployment {
                app_id: &app.id,
                pr_number: Some(7),
                branch: "main",
                commit_sha: "abc1234",
                commit_message: "",
                trigger: DeploymentTrigger::Manual,
                rollback_of: None,
                image: "example:v1",
                node_id: Some(&main.id),
            },
        )?;
        deployments::transition(c, &d.id, DeploymentStatus::Deploying, None)?;
        deployments::transition(c, &d.id, DeploymentStatus::Healthy, None)?;
        previews::set_serving(c, &app.id, 7, &d.id, "http://pr-7-shared.main.vfy")
    })
    .unwrap();
    (s, key, app)
}
#[test]
fn opaque_invitation_and_cookie_are_scoped_hashed_and_bounded() {
    let (s, _, app) = fixture();
    let invite = s.tx(|c| g::create(c, &app.id, 7, "owner", 3600, true, 1000)).unwrap();
    let metadata = serde_json::to_string(&invite.grant).unwrap();
    assert!(!metadata.contains(invite.secret.as_str()));
    assert!(s
        .tx(|c| g::redeem(c, "wrong.guests.example.test", &invite.secret, 1001))
        .unwrap()
        .is_none());
    let session = s
        .tx(|c| g::redeem(c, &invite.grant.host, &invite.secret, 1001))
        .unwrap()
        .unwrap();
    assert_eq!(session.expires_at, 1901);
    let allowed = s
        .tx(|c| g::authorize(c, &invite.grant.host, &session.secret, 1002))
        .unwrap()
        .unwrap();
    assert_eq!(allowed.grant.id, invite.grant.id);
    assert!(s
        .tx(|c| g::redeem(c, &invite.grant.host, &invite.secret, 1002))
        .unwrap()
        .is_none());
    assert!(s
        .tx(|c| g::authorize(c, "wrong.guests.example.test", &session.secret, 1002))
        .unwrap()
        .is_none());
    let raw = s
        .with(|c| {
            Ok(c.query_row(
                "SELECT token_hash FROM guest_grants WHERE id=?1",
                [&invite.grant.id],
                |r| r.get::<_, String>(0),
            )?)
        })
        .unwrap();
    assert_ne!(raw, invite.secret.as_str());
    assert_eq!(raw.len(), 64);
    assert!(s
        .tx(|c| g::authorize(c, &invite.grant.host, &session.secret, 1901))
        .unwrap()
        .is_none());
}
#[test]
fn expiry_revocation_close_and_generation_changes_never_resurrect_access() {
    let (s, _, app) = fixture();
    let grant = s.tx(|c| g::create(c, &app.id, 7, "owner", 3600, false, 1000)).unwrap();
    let session = s
        .tx(|c| g::redeem(c, &grant.grant.host, &grant.secret, 1001))
        .unwrap()
        .unwrap();
    s.tx(|c| previews::close(c, &app.id, 7)).unwrap();
    s.tx(|c| previews::reopen(c, &app.id, 7)).unwrap();
    assert!(s
        .tx(|c| g::authorize(c, &grant.grant.host, &session.secret, 1002))
        .unwrap()
        .is_none());
    let grant = s.tx(|c| g::create(c, &app.id, 7, "owner", 3600, false, 1003)).unwrap();
    let session = s
        .tx(|c| g::redeem(c, &grant.grant.host, &grant.secret, 1004))
        .unwrap()
        .unwrap();
    s.tx(|c| g::revoke(c, &grant.grant.id, 1005)).unwrap();
    assert!(s
        .tx(|c| g::authorize(c, &grant.grant.host, &session.secret, 1006))
        .unwrap()
        .is_none());
    let grant = s.tx(|c| g::create(c, &app.id, 7, "owner", 3600, false, 1007)).unwrap();
    s.tx(|c| {
        let d = deployments::create(
            c,
            &deployments::NewDeployment {
                app_id: &app.id,
                pr_number: Some(7),
                branch: "main",
                commit_sha: "def5678",
                commit_message: "",
                trigger: DeploymentTrigger::Manual,
                rollback_of: None,
                image: "example:v2",
                node_id: None,
            },
        )?;
        deployments::transition(c, &d.id, DeploymentStatus::Deploying, None)?;
        deployments::transition(c, &d.id, DeploymentStatus::Healthy, None)?;
        previews::set_serving(c, &app.id, 7, &d.id, "http://pr-7-shared.main.vfy")
    })
    .unwrap();
    assert!(s
        .tx(|c| g::redeem(c, &grant.grant.host, &grant.secret, 1008))
        .unwrap()
        .is_none());
}
#[test]
fn restart_keeps_valid_sessions_but_key_rotation_and_clock_rollback_fail_closed() {
    let (s, key, app) = fixture();
    let grant = s.tx(|c| g::create(c, &app.id, 7, "owner", 3600, false, 1000)).unwrap();
    let session = s
        .tx(|c| g::redeem(c, &grant.grant.host, &grant.secret, 1001))
        .unwrap()
        .unwrap();
    s.tx(|c| g::ensure_key(c, &key, 1002)).unwrap();
    assert!(s
        .tx(|c| g::authorize(c, &grant.grant.host, &session.secret, 1003))
        .unwrap()
        .is_some());
    assert!(s
        .tx(|c| g::authorize(c, &grant.grant.host, &session.secret, 900))
        .unwrap()
        .is_none());
    let other = MasterKey::from_bytes(&[82; 32]).unwrap();
    s.tx(|c| g::ensure_key(c, &other, 1004)).unwrap();
    assert!(s
        .tx(|c| g::authorize(c, &grant.grant.host, &session.secret, 1005))
        .unwrap()
        .is_none());
    s.tx(|c| g::rotate(c, 900)).unwrap();
    assert!(s.tx(|c| g::create(c, &app.id, 7, "owner", 3600, true, 901)).is_ok());
}
#[test]
fn reserved_guest_domains_cannot_be_claimed_by_an_application() {
    let (s, _, app) = fixture();
    assert!(s
        .tx(|c| apps::add_domain(
            c,
            &app.id,
            &DomainRequest {
                host: "anything.guests.example.test".into(),
                https: Some(true),
                primary: Some(false),
                redirect_to: None
            }
        ))
        .is_err());
    assert!(s.tx(|c| g::configure(c, Some("example.test"))).is_err());
    assert!(s.tx(|c| g::create(c, &app.id, 7, "owner", 604801, true, 1000)).is_err());
}

#[test]
fn denied_authentication_commits_expiry_and_cannot_resurrect_after_clock_correction() {
    let (s, _, app) = fixture();
    let grant = s.tx(|c| g::create(c, &app.id, 7, "owner", 60, false, 1000)).unwrap();
    let session = s
        .tx(|c| g::redeem(c, &grant.grant.host, &grant.secret, 1001))
        .unwrap()
        .unwrap();
    assert!(s
        .tx(|c| g::authorize(c, &grant.grant.host, &session.secret, 1060))
        .unwrap()
        .is_none());
    assert!(s.with(|c| g::get(c, &grant.grant.id)).unwrap().revoked_at.is_some());
    assert!(s
        .tx(|c| g::authorize(c, &grant.grant.host, &session.secret, 1002))
        .unwrap()
        .is_none());
}
#[test]
fn a_single_use_invitation_is_consumed_by_only_one_concurrent_transaction() {
    let (s, _, app) = fixture();
    let grant = s.tx(|c| g::create(c, &app.id, 7, "owner", 3600, true, 1000)).unwrap();
    let s = std::sync::Arc::new(s);
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let s = s.clone();
            let host = grant.grant.host.clone();
            let secret = grant.secret.clone();
            std::thread::spawn(move || s.tx(|c| g::redeem(c, &host, &secret, 1001)).unwrap().is_some())
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|t| usize::from(t.join().unwrap()))
            .sum::<usize>(),
        1
    );
}
#[test]
fn reusable_invites_have_a_bounded_live_session_count() {
    let (s, _, app) = fixture();
    let grant = s.tx(|c| g::create(c, &app.id, 7, "owner", 3600, false, 1000)).unwrap();
    for _ in 0..8 {
        assert!(s
            .tx(|c| g::redeem(c, &grant.grant.host, &grant.secret, 1001))
            .unwrap()
            .is_some());
    }
    assert!(s
        .tx(|c| g::redeem(c, &grant.grant.host, &grant.secret, 1001))
        .unwrap()
        .is_none());
}

#[test]
fn changing_privacy_disabling_or_stopping_permanently_revokes_guest_access() {
    for change in [
        serde_json::json!({"previews":{"enabled":true,"access":"public","max":3}}),
        serde_json::json!({"previews":{"enabled":false,"access":"private","max":3}}),
        serde_json::json!({"desired":"stopped"}),
    ] {
        let (s, _, app) = fixture();
        let grant = s.tx(|c| g::create(c, &app.id, 7, "owner", 3600, false, 1000)).unwrap();
        let session = s
            .tx(|c| g::redeem(c, &grant.grant.host, &grant.secret, 1001))
            .unwrap()
            .unwrap();
        s.tx(|c| apps::patch(c, &app.id, &serde_json::from_value(change).unwrap()))
            .unwrap();
        s.tx(|c| {
            apps::patch(
                c,
                &app.id,
                &serde_json::from_value(
                    serde_json::json!({"desired":"running","previews":{"enabled":true,"access":"private","max":3}}),
                )
                .unwrap(),
            )
        })
        .unwrap();
        assert!(s
            .tx(|c| g::authorize(c, &grant.grant.host, &session.secret, 1002))
            .unwrap()
            .is_none());
        assert!(s
            .tx(|c| g::redeem(c, &grant.grant.host, &grant.secret, 1002))
            .unwrap()
            .is_none());
    }
}

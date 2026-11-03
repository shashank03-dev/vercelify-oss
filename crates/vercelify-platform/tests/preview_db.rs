use vercelify_core::platform::{App, AppCreate};
use vercelify_platform::{apps, preview_db, previews, secrets::MasterKey, Store};

fn key() -> MasterKey {
    MasterKey::from_bytes(&[17; 32]).unwrap()
}
fn app(store: &Store) -> App {
    let request: AppCreate = serde_json::from_value(serde_json::json!({
        "name":"preview-owner", "kind":"image", "source":{"image":"node:22-slim"},
        "previews":{"enabled":true,"database":{"migration":"node migrate.js","seed":"node seed.js"}}
    }))
    .unwrap();
    store.tx(|c| apps::create(c, &request)).unwrap()
}

#[test]
fn fresh_databases_are_opt_in_and_settings_cannot_control_host_tools() {
    let store = Store::open_in_memory().unwrap();
    let request: AppCreate =
        serde_json::from_value(serde_json::json!({"name":"legacy","kind":"image","source":{"image":"node:22-slim"}}))
            .unwrap();
    let legacy = store.tx(|c| apps::create(c, &request)).unwrap();
    assert!(legacy.previews.database.is_none());
    for (kind, variable, command) in [
        ("image", "DOCKER_HOST", "node migrate.js"),
        ("image", "9BAD", "node migrate.js"),
        ("image", "DATABASE_URL", "bad\0command"),
        ("compose", "DATABASE_URL", "node migrate.js"),
    ] {
        let request: AppCreate = serde_json::from_value(serde_json::json!({
            "name":"invalid", "kind":kind,
            "source":if kind=="compose" {serde_json::json!({"compose_content":"services:\n  web:\n    image: node:22-slim\n"})} else {serde_json::json!({"image":"node:22-slim"})},
            "previews":{"enabled":true,
                "database":{"variable":variable,"migration":command}}
        })).unwrap();
        assert!(store.tx(|c| apps::create(c, &request)).is_err());
    }
    assert_eq!(
        store
            .with(|c| Ok(c.query_row("SELECT count(*) FROM databases", [], |r| r.get::<_, i64>(0))?))
            .unwrap(),
        0
    );
}

#[test]
fn two_prs_share_only_the_preview_process_and_keep_sealed_distinct_credentials() {
    let store = Store::open_in_memory().unwrap();
    let app = app(&store);
    let key = key();
    for pr in [1, 2] {
        store
            .tx(|c| previews::upsert(c, &app.id, pr, "abc1234", "fixture"))
            .unwrap();
    }
    let a = store.tx(|c| preview_db::prepare(c, &key, &app, 1)).unwrap();
    let b = store.tx(|c| preview_db::prepare(c, &key, &app, 2)).unwrap();
    assert_eq!(a.service_id, b.service_id);
    assert_ne!(
        (a.id.as_str(), a.database.as_str(), a.username.as_str()),
        (b.id.as_str(), b.database.as_str(), b.username.as_str())
    );
    assert_ne!(&*a.password, &*b.password);
    assert!(!format!("{a:?}").contains(a.password.as_str()));
    let sealed: Vec<u8> = store
        .with(|c| {
            Ok(
                c.query_row("SELECT password FROM preview_databases WHERE id=?1", [&a.id], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap();
    assert!(!sealed.windows(a.password.len()).any(|w| w == a.password.as_bytes()));
    let service = store
        .with(|c| vercelify_platform::databases::get(c, &key, &a.service_id, false))
        .unwrap();
    assert_eq!(service.memory_mb, Some(256));
    assert!(service.public_port.is_none());
    assert!(store
        .tx(|c| vercelify_platform::databases::patch(
            c,
            &key,
            &service.id,
            &vercelify_core::platform::DatabasePatch {
                public_port: Some(15432),
                ..Default::default()
            }
        ))
        .is_err());
}

#[test]
fn restart_reuses_owned_identity_but_close_fences_it_and_reopen_rotates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("platform.db");
    let key = key();
    let store = Store::open(&path).unwrap();
    let app = app(&store);
    store
        .tx(|c| previews::upsert(c, &app.id, 7, "abc1234", "fixture"))
        .unwrap();
    let old = store.tx(|c| preview_db::prepare(c, &key, &app, 7)).unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    let resumed = store.tx(|c| preview_db::prepare(c, &key, &app, 7)).unwrap();
    assert_eq!(old.id, resumed.id);
    assert_eq!(&*old.password, &*resumed.password);
    store.tx(|c| previews::close(c, &app.id, 7)).unwrap();
    assert!(store.tx(|c| preview_db::set_state(c, &old.id, "ready", "")).is_err());
    assert!(store
        .tx(|c| previews::upsert(c, &app.id, 7, "def5678", "reopen"))
        .is_err());
    store.tx(|c| preview_db::set_state(c, &old.id, "deleted", "")).unwrap();
    store
        .tx(|c| previews::upsert(c, &app.id, 7, "def5678", "reopen"))
        .unwrap();
    let new = store.tx(|c| preview_db::prepare(c, &key, &app, 7)).unwrap();
    assert_ne!(new.id, old.id);
    assert_ne!(&*new.password, &*old.password);
    let retained: i64 = store
        .with(|c| {
            Ok(c.query_row(
                "SELECT length(password) FROM preview_databases WHERE id=?1",
                [&old.id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(retained, 0, "completed teardown retains no sealed credential");
}

#[test]
fn deleting_an_app_preserves_the_cleanup_journal_and_exact_resource_ownership() {
    let store = Store::open_in_memory().unwrap();
    let app = app(&store);
    let key = key();
    store
        .tx(|c| previews::upsert(c, &app.id, 3, "abc1234", "fixture"))
        .unwrap();
    let owned = store.tx(|c| preview_db::prepare(c, &key, &app, 3)).unwrap();
    store.tx(|c| apps::delete(c, &app.id)).unwrap();
    let pending = store.tx(|c| preview_db::pending_cleanup(c)).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, owned.id);
    assert_eq!(pending[0].service_id, owned.service_id);
}

#[test]
fn the_opt_in_database_binding_is_runtime_only_and_other_production_refs_stay_blocked() {
    use vercelify_core::platform::{DatabaseCreate, EnvVarsPut, VarScope};
    use vercelify_platform::{databases, env};
    let store = Store::open_in_memory().unwrap();
    let app = app(&store);
    let key = key();
    let create: DatabaseCreate =
        serde_json::from_value(serde_json::json!({"name":"production","kind":"postgresql"})).unwrap();
    let production = store.tx(|c| databases::create(c, &key, &create)).unwrap();
    let vars: EnvVarsPut=serde_json::from_value(serde_json::json!({"vars":[{"key":"DATABASE_URL","value":"${{db.production.url}}","share_with_previews":true,"build":true,"secret":true}]})).unwrap();
    store.tx(|c| env::put(c, &key, VarScope::App, &app.id, &vars)).unwrap();
    let preview = store
        .with(|c| env::resolve_for_deploy(c, &key, &app.id, true, None))
        .unwrap();
    assert!(
        !preview.vars.contains_key("DATABASE_URL"),
        "no DB connection enters builds; runtime binding is injected later"
    );
    let prod = store
        .with(|c| env::resolve_for_deploy(c, &key, &app.id, false, None))
        .unwrap();
    let password = store
        .with(|c| databases::credentials(c, &key, &production.id))
        .unwrap()
        .password;
    assert!(prod.vars["DATABASE_URL"].value.contains(&password));
    let vars: EnvVarsPut = serde_json::from_value(
        serde_json::json!({"vars":[{"key":"OTHER_URL","value":"${{db.production.url}}","share_with_previews":true}]}),
    )
    .unwrap();
    store.tx(|c| env::put(c, &key, VarScope::App, &app.id, &vars)).unwrap();
    assert!(store
        .with(|c| env::resolve_for_deploy(c, &key, &app.id, true, None))
        .is_err());
}

#[test]
fn changing_preview_database_policy_fences_only_preview_build_jobs() {
    let store = Store::open_in_memory().unwrap();
    let app = app(&store);
    let key = key();
    let before = store
        .with(|c| {
            Ok((
                vercelify_platform::offhost::revision(c, &key, &app.id, true)?,
                vercelify_platform::offhost::revision(c, &key, &app.id, false)?,
            ))
        })
        .unwrap();
    let mut settings = app.previews.clone();
    settings.database.as_mut().unwrap().seed = Some("node different-seed.js".into());
    store
        .tx(|c| {
            apps::patch(
                c,
                &app.id,
                &vercelify_core::platform::AppPatch {
                    previews: Some(settings),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let after = store
        .with(|c| {
            Ok((
                vercelify_platform::offhost::revision(c, &key, &app.id, true)?,
                vercelify_platform::offhost::revision(c, &key, &app.id, false)?,
            ))
        })
        .unwrap();
    assert_ne!(
        before.0, after.0,
        "pending PR images must not publish against a changed database policy"
    );
    assert_eq!(
        before.1, after.1,
        "production build jobs do not use preview database policy"
    );
}

#[test]
fn direct_deploys_cannot_exceed_the_database_preview_limit() {
    let store = Store::open_in_memory().unwrap();
    let app = app(&store);
    let key = key();
    for pr in 1..=4 {
        store
            .tx(|c| previews::upsert(c, &app.id, pr, "abc1234", "direct deploy"))
            .unwrap();
        let result = store.tx(|c| preview_db::prepare(c, &key, &app, pr));
        assert_eq!(result.is_ok(), pr <= 3);
    }
    // Reusing an existing lifecycle consumes no additional slot. A closing
    // lifecycle still owns resources until physical cleanup finishes.
    assert!(store.tx(|c| preview_db::prepare(c, &key, &app, 1)).is_ok());
    store.tx(|c| previews::close(c, &app.id, 1)).unwrap();
    assert!(store.tx(|c| preview_db::prepare(c, &key, &app, 4)).is_err());
    let old = store.with(|c| preview_db::live(c, &app.id, 1)).unwrap().unwrap();
    store.tx(|c| preview_db::set_state(c, &old.id, "deleted", "")).unwrap();
    assert!(store.tx(|c| preview_db::prepare(c, &key, &app, 4)).is_ok());
}

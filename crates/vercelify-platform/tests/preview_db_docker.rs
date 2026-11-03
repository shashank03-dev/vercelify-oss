//! Owns PostgreSQL and the proxy on a disposable daemon only.
mod common;
use std::{io::Write, sync::Arc, time::Duration};
use vercelify_core::platform::*;
use vercelify_platform::{
    apps,
    build::{Builder, Driver},
    deploy::Deployer,
    docker::Docker,
    engine::{Engine, Enqueue},
    gate::BuildGate,
    logs::LogHub,
    nodes, preview_db, previews,
    proc::Cancel,
    proxy::{self, ProxyConfig},
    secrets::MasterKey,
    Store,
};

fn query(container: &str, user: &str, password: &str, database: &str, sql: &str) -> std::process::Output {
    query_host(container, user, password, "127.0.0.1", database, sql)
}
fn query_host(
    container: &str,
    user: &str,
    password: &str,
    host: &str,
    database: &str,
    sql: &str,
) -> std::process::Output {
    let mut child = std::process::Command::new("docker")
        .args([
            "exec",
            "-i",
            "-e",
            "PGPASSWORD",
            "-e",
            "PGCONNECT_TIMEOUT",
            container,
            "psql",
            "-X",
            "-h",
            host,
            "-U",
            user,
            "-d",
            database,
            "-At",
            "-v",
            "ON_ERROR_STOP=1",
        ])
        .env("PGPASSWORD", password)
        .env("PGCONNECT_TIMEOUT", "2")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(sql.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "owns disposable Docker: VERCELIFY_PREVIEW_DB_TESTS=1"]
async fn isolated_postgres_roles_migrations_and_owned_recovery() {
    assert_eq!(std::env::var("VERCELIFY_PREVIEW_DB_TESTS").as_deref(), Ok("1"));
    assert!(
        !std::path::Path::new("/etc/vercelify").exists(),
        "never use a serving host"
    );
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("platform.db")).unwrap());
    store
        .tx(|c| nodes::ensure_main(c, "preview-test", "http://127.0.0.1:7420", std::env::consts::ARCH))
        .unwrap();
    let _inventory = common::inventory(store.clone(), dir.path()).await;
    let key = Arc::new(MasterKey::from_bytes(&[29; 32]).unwrap());
    let docker = Docker::default();
    let proxy = ProxyConfig {
        dir: dir.path().join("proxy"),
        http_port: 18080,
        https_port: 18443,
        metrics_port: 18082,
        acme_email: None,
        dns_challenge: false,
        image: proxy::TRAEFIK_IMAGE.into(),
    };
    let _ = docker.remove(proxy::CONTAINER).await;
    proxy::ensure(&docker, &proxy, vec![]).await.unwrap();
    let logs = LogHub::new(dir.path().join("logs"));
    let engine = Engine::new(
        store.clone(),
        key.clone(),
        Builder {
            docker: "docker".into(),
            git: "git".into(),
            work_dir: dir.path().join("work"),
            driver: Driver::Container,
            memory: Some("1024m".into()),
            timeout: Duration::from_secs(300),
        },
        Deployer {
            docker: docker.clone(),
            proxy,
            hdd_root: dir.path().join("hdd"),
            cgroup_parent: None,
            health_interval: Duration::from_millis(100),
        },
        BuildGate::new(0, dir.path().join("power")),
        logs.clone(),
    );
    let request:AppCreate=serde_json::from_value(serde_json::json!({"name":"pg-previews","kind":"image","source":{"image":"postgres:17-alpine"},
        "runtime":{"port":8080,"memory_mb":128,"cpus":1},"previews":{"enabled":true,"database":{
            "migration":"if [ \"$VERCELIFY_PR_NUMBER\" = 1 ]; then psql \"$DATABASE_URL\" -v ON_ERROR_STOP=1 -c 'CREATE TABLE entries(value text)'; else psql \"$DATABASE_URL\" -v ON_ERROR_STOP=1 -c 'CREATE TABLE entries(value integer)'; fi",
            "seed":"echo \"$DATABASE_URL\"; psql \"$DATABASE_URL\" -v ON_ERROR_STOP=1 -c \"INSERT INTO entries VALUES ('$VERCELIFY_PR_NUMBER')\""}}})).unwrap();
    let app = store.tx(|c| apps::create(c, &request)).unwrap();
    let mut owned = Vec::new();
    for pr in [1, 2] {
        let d = engine
            .enqueue(
                &app.id,
                Enqueue {
                    pr_number: Some(pr),
                    commit_sha: Some(format!("{pr:040}")),
                    ..Default::default()
                },
            )
            .unwrap();
        let mut vars = vec![];
        let mut log = logs.writer(&app.name, &d.id);
        if pr == 1 {
            let pending = store.tx(|c| preview_db::prepare(c, &key, &app, pr)).unwrap();
            let service = engine.ensure_database(&pending.service_id).await.unwrap();
            let admin = store
                .with(|c| vercelify_platform::databases::credentials(c, &key, &service.id))
                .unwrap();
            let owner = preview_db::owner_role(&pending);
            let partial = query(
                &service.container,
                &admin.user,
                &admin.password,
                &admin.database,
                &format!(
                    "BEGIN; CREATE ROLE {owner} NOLOGIN; COMMENT ON ROLE {owner} IS 'vercelify-preview:{}'; COMMIT;",
                    pending.id
                ),
            );
            assert!(partial.status.success());
            let reopened = Store::open(&dir.path().join("platform.db")).unwrap();
            assert_eq!(
                reopened.tx(|c| preview_db::prepare(c, &key, &app, pr)).unwrap().id,
                pending.id,
                "restart after role creation before database creation keeps the owned identity"
            );
        }
        engine
            .provision_preview_db(&app, &d, "postgres:17-alpine", &mut vars, Cancel::never(), &mut log)
            .await
            .unwrap();
        // Repeating a migrated job must not run CREATE TABLE or seed twice.
        engine
            .provision_preview_db(&app, &d, "postgres:17-alpine", &mut vars, Cancel::never(), &mut log)
            .await
            .unwrap();
        let row = store.with(|c| preview_db::live(c, &app.id, pr)).unwrap().unwrap();
        let binding = store.with(|c| preview_db::binding(c, &key, &row.id)).unwrap();
        assert_eq!(row.state, "ready");
        assert!(logs
            .read(&app.name, &d.id, None)
            .iter()
            .all(|l| !l.text.contains(binding.password.as_str())));
        owned.push(binding);
    }
    assert_eq!(owned[0].service_id, owned[1].service_id);
    let service = store
        .with(|c| vercelify_platform::databases::get(c, &key, &owned[0].service_id, false))
        .unwrap();
    docker.stop(&service.container, 10).await.unwrap();
    assert!(
        engine.reconcile_preview_databases().await.is_empty(),
        "owned databases recover after a real PostgreSQL restart"
    );
    for (index, row) in owned.iter().enumerate() {
        let out = query(
            &service.container,
            &row.username,
            &row.password,
            &row.database,
            "SELECT value FROM entries;",
        );
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), (index + 1).to_string());
        for database in [&owned[1 - index].database, &"postgres".to_string()] {
            assert!(
                !query(&service.container, &row.username, &row.password, database, "SELECT 1;")
                    .status
                    .success(),
                "cross-PR/maintenance connection denied"
            );
        }
        // PostgreSQL may report a refused GRANT as a warning with exit code 0.
        // The effective connection privilege is the actual isolation contract.
        let _ = query(
            &service.container,
            &row.username,
            &row.password,
            &row.database,
            &format!("GRANT CONNECT ON DATABASE {} TO PUBLIC;", row.database),
        );
        let other = &owned[1 - index];
        assert!(
            !query(
                &service.container,
                &other.username,
                &other.password,
                &row.database,
                "SELECT 1;"
            )
            .status
            .success(),
            "login role cannot weaken DB connection ACLs"
        );
        assert!(!query(
            &service.container,
            &row.username,
            &row.password,
            &row.database,
            "CREATE ROLE forbidden SUPERUSER;"
        )
        .status
        .success());
    }
    store.tx(|c| previews::close(c, &app.id, 1)).unwrap();
    assert!(engine.reconcile_preview_databases().await.is_empty());
    assert!(!query(
        &service.container,
        &owned[0].username,
        &owned[0].password,
        &owned[0].database,
        "SELECT 1;"
    )
    .status
    .success());
    assert!(query(
        &service.container,
        &owned[1].username,
        &owned[1].password,
        &owned[1].database,
        "SELECT value FROM entries;"
    )
    .status
    .success());
    store.tx(|c| previews::close(c, &app.id, 2)).unwrap();
    assert!(engine.reconcile_preview_databases().await.is_empty());
    assert!(docker.inspect(&service.container).await.unwrap().is_none());
    assert!(store.with(preview_db::services).unwrap().is_empty());
    assert!(
        engine.reconcile_preview_databases().await.is_empty(),
        "cleanup retries are idempotent"
    );
    assert_eq!(
        store
            .with(|c| Ok(
                c.query_row("SELECT sum(length(password)) FROM preview_databases", [], |r| r
                    .get::<_, i64>(0))?
            ))
            .unwrap(),
        0
    );
    full_pipeline(&engine, &app, dir.path()).await;
    let _ = docker.remove(proxy::CONTAINER).await;
}

async fn wait(engine: &Engine, id: &str, status: DeploymentStatus) -> Deployment {
    for _ in 0..240 {
        let d = engine
            .store
            .with(|c| vercelify_platform::deployments::get(c, id))
            .unwrap();
        if d.status == status {
            return d;
        }
        assert!(
            !matches!(d.status, DeploymentStatus::Failed | DeploymentStatus::Cancelled),
            "unexpected deployment result: {}",
            d.error
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("deployment did not reach expected state");
}
fn http(host: &str) -> bool {
    std::process::Command::new("curl")
        .args([
            "-fsS",
            "--max-time",
            "3",
            "-H",
            &format!("Host: {host}"),
            "http://127.0.0.1:18080/",
        ])
        .output()
        .is_ok_and(|out| out.status.success() && String::from_utf8_lossy(&out.stdout).contains("Hostname:"))
}

async fn full_pipeline(engine: &Arc<Engine>, original: &App, root: &std::path::Path) {
    use vercelify_platform::{databases, deployments, env, store};
    let fixture = root.join("fixture");
    std::fs::create_dir(&fixture).unwrap();
    std::fs::write(fixture.join("Dockerfile"),"FROM postgres:17-alpine\nCOPY --from=traefik/whoami:v1.11 /whoami /usr/local/bin/whoami\nEXPOSE 8080\nCMD [\"whoami\",\"--port\",\"8080\"]\n").unwrap();
    let image = "vfy/preview-db-contract:fixture";
    let built = engine
        .deployer
        .docker
        .run(
            &vercelify_platform::proc::args(&["build", "--pull=false", "-t", image, &fixture.to_string_lossy()]),
            Duration::from_secs(180),
        )
        .await
        .unwrap();
    assert!(built.success(), "fixture image: {}", built.tail(5));
    let prod_create: DatabaseCreate =
        serde_json::from_value(serde_json::json!({"name":"preview-prod-sentinel","kind":"postgresql","version":"17"}))
            .unwrap();
    let production = engine
        .store
        .tx(|c| databases::create(c, &engine.key, &prod_create))
        .unwrap();
    engine.ensure_database(&production.id).await.unwrap();
    let credential = engine
        .store
        .with(|c| databases::credentials(c, &engine.key, &production.id))
        .unwrap();
    assert!(query(
        &production.container,
        &credential.user,
        &credential.password,
        &credential.database,
        "CREATE TABLE sentinel(value text); INSERT INTO sentinel VALUES ('production-only');"
    )
    .status
    .success());
    let mut settings = original.previews.clone();
    settings.access = PreviewAccess::Public;
    let app=engine.store.tx(|c| {
        store::put_setting(c,apps::DOMAIN_SETTING,&apps::DomainSettings{wildcard:Some("previews.test.localhost".into()),..Default::default()})?;
        let app=apps::patch(c,&original.id,&AppPatch{source:Some(AppSource{image:Some(image.into()),..Default::default()}),previews:Some(settings.clone()),..Default::default()})?;
        apps::add_domain(c,&app.id,&serde_json::from_value::<DomainRequest>(serde_json::json!({"host":"production.previews.test.localhost","https":false,"primary":true}))?)?;
        env::put(c,&engine.key,VarScope::App,&app.id,&serde_json::from_value::<EnvVarsPut>(serde_json::json!({"vars":[{"key":"DATABASE_URL","value":"${{db.preview-prod-sentinel.url}}","secret":true}]}))?)?;
        apps::get(c,&app.id)
    }).unwrap();
    let dispatcher = tokio::spawn(engine.clone().run());
    let prod = engine.enqueue(&app.id, Enqueue::default()).unwrap();
    wait(engine, &prod.id, DeploymentStatus::Healthy).await;
    for _ in 0..30 {
        if http("production.previews.test.localhost") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(http("production.previews.test.localhost"));
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let failures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let watcher = {
        let (running, failures, requests) = (running.clone(), failures.clone(), requests.clone());
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(180);
            while running.load(std::sync::atomic::Ordering::Relaxed) && std::time::Instant::now() < deadline {
                requests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if !http("production.previews.test.localhost") {
                    failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };
    // Opting in must still clean up an older preview that never owned a DB.
    let mut without_db = settings.clone();
    without_db.database = None;
    engine
        .store
        .tx(|c| {
            apps::patch(
                c,
                &app.id,
                &AppPatch {
                    previews: Some(without_db),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let legacy = engine
        .enqueue(
            &app.id,
            Enqueue {
                pr_number: Some(4),
                commit_sha: Some("d".repeat(40)),
                ..Default::default()
            },
        )
        .unwrap();
    wait(engine, &legacy.id, DeploymentStatus::Healthy).await;
    let legacy_container = engine.store.with(|c| deployments::container(c, &legacy.id)).unwrap();
    engine
        .store
        .tx(|c| {
            apps::patch(
                c,
                &app.id,
                &AppPatch {
                    previews: Some(settings.clone()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    engine.remove_preview(&app.id, 4).await.unwrap();
    assert!(engine
        .deployer
        .docker
        .inspect(&legacy_container)
        .await
        .unwrap()
        .is_none());
    for pr in [1, 2] {
        let d = engine
            .enqueue(
                &app.id,
                Enqueue {
                    pr_number: Some(pr),
                    commit_sha: Some(format!("{pr:040}")),
                    ..Default::default()
                },
            )
            .unwrap();
        wait(engine, &d.id, DeploymentStatus::Healthy).await;
        let container = engine.store.with(|c| deployments::container(c, &d.id)).unwrap();
        let owned = engine
            .store
            .with(|c| preview_db::live(c, &app.id, pr))
            .unwrap()
            .unwrap();
        let binding = engine
            .store
            .with(|c| preview_db::binding(c, &engine.key, &owned.id))
            .unwrap();
        let service = engine
            .store
            .with(|c| databases::get(c, &engine.key, &owned.service_id, false))
            .unwrap();
        let out = query_host(
            &container,
            &owned.username,
            &binding.password,
            &service.container,
            &owned.database,
            "SELECT value FROM entries;",
        );
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), pr.to_string());
        let prod_ip = engine
            .deployer
            .docker
            .inspect(&production.container)
            .await
            .unwrap()
            .unwrap()
            .ips[&vercelify_platform::deploy::project_network(&app.project_id)]
            .clone();
        assert!(
            !query_host(
                &container,
                &credential.user,
                &credential.password,
                &prod_ip,
                &credential.database,
                "SELECT value FROM sentinel;"
            )
            .status
            .success(),
            "preview cannot reach production even with injected test credentials"
        );
    }
    let old = engine.store.with(|c| preview_db::live(c, &app.id, 1)).unwrap().unwrap();
    let old_binding = engine
        .store
        .with(|c| preview_db::binding(c, &engine.key, &old.id))
        .unwrap();
    engine.remove_preview(&app.id, 1).await.unwrap();
    let reopened = engine
        .enqueue(
            &app.id,
            Enqueue {
                pr_number: Some(1),
                commit_sha: Some("a".repeat(40)),
                ..Default::default()
            },
        )
        .unwrap();
    wait(engine, &reopened.id, DeploymentStatus::Healthy).await;
    let fresh = engine.store.with(|c| preview_db::live(c, &app.id, 1)).unwrap().unwrap();
    assert_ne!(old.id, fresh.id);
    let service = engine
        .store
        .with(|c| databases::get(c, &engine.key, &fresh.service_id, false))
        .unwrap();
    assert!(!query(
        &service.container,
        &old.username,
        &old_binding.password,
        &old.database,
        "SELECT 1;"
    )
    .status
    .success());
    let mut bad = settings.clone();
    bad.database.as_mut().unwrap().migration = Some("echo \"$DATABASE_URL\"; exit 9".into());
    engine
        .store
        .tx(|c| {
            apps::patch(
                c,
                &app.id,
                &AppPatch {
                    previews: Some(bad),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let failed = engine
        .enqueue(
            &app.id,
            Enqueue {
                pr_number: Some(3),
                commit_sha: Some("b".repeat(40)),
                ..Default::default()
            },
        )
        .unwrap();
    wait(engine, &failed.id, DeploymentStatus::Failed).await;
    assert!(!engine
        .deployer
        .docker
        .list_managed()
        .await
        .unwrap()
        .iter()
        .any(|c| c.labels.get("vercelify.app") == Some(&app.id)
            && c.labels.get("vercelify.pr").map(String::as_str) == Some("3")
            && c.labels.get("vercelify.kind").map(String::as_str) == Some("preview")));
    let failed_row = engine.store.with(|c| preview_db::live(c, &app.id, 3)).unwrap().unwrap();
    let failed_binding = engine
        .store
        .with(|c| preview_db::binding(c, &engine.key, &failed_row.id))
        .unwrap();
    assert!(engine
        .logs
        .read(&app.name, &failed.id, None)
        .iter()
        .all(|line| !line.text.contains(failed_binding.password.as_str())));
    engine
        .store
        .tx(|c| {
            apps::patch(
                c,
                &app.id,
                &AppPatch {
                    previews: Some(settings),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let retry = engine
        .enqueue(
            &app.id,
            Enqueue {
                pr_number: Some(3),
                commit_sha: Some("c".repeat(40)),
                ..Default::default()
            },
        )
        .unwrap();
    wait(engine, &retry.id, DeploymentStatus::Healthy).await;
    assert_eq!(
        engine
            .store
            .with(|c| preview_db::live(c, &app.id, 3))
            .unwrap()
            .unwrap()
            .id,
        failed_row.id,
        "retry retains one owned database"
    );
    for pr in [1, 2, 3] {
        engine.remove_preview(&app.id, pr).await.unwrap();
    }
    assert!(engine.store.with(preview_db::services).unwrap().is_empty());
    let sentinel = query(
        &production.container,
        &credential.user,
        &credential.password,
        &credential.database,
        "SELECT value FROM sentinel;",
    );
    assert!(sentinel.status.success());
    assert_eq!(String::from_utf8_lossy(&sentinel.stdout).trim(), "production-only");
    running.store(false, std::sync::atomic::Ordering::Relaxed);
    watcher.join().unwrap();
    assert_eq!(
        failures.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "production HTTP stayed serving"
    );
    println!("PASS: {} production probes; conflicting migrations, runtime binding, production denial, failure fencing, retry and reopen rotation",requests.load(std::sync::atomic::Ordering::Relaxed));
    dispatcher.abort();
    // A crash after deleting the app row must not discard the DB cleanup link.
    let dispatcher = tokio::spawn(engine.clone().run());
    let orphaned = engine
        .enqueue(
            &app.id,
            Enqueue {
                pr_number: Some(9),
                commit_sha: Some("e".repeat(40)),
                ..Default::default()
            },
        )
        .unwrap();
    wait(engine, &orphaned.id, DeploymentStatus::Healthy).await;
    let orphan = engine.store.with(|c| preview_db::live(c, &app.id, 9)).unwrap().unwrap();
    let orphan_service = engine
        .store
        .with(|c| databases::get(c, &engine.key, &orphan.service_id, false))
        .unwrap();
    let removed = engine.store.tx(|c| apps::delete(c, &app.id)).unwrap();
    for _ in 0..40 {
        assert!(engine.reconcile_preview_databases().await.is_empty());
        if engine.store.with(preview_db::services).unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(engine.store.with(preview_db::services).unwrap().is_empty());
    assert!(engine
        .deployer
        .docker
        .inspect(&orphan_service.container)
        .await
        .unwrap()
        .is_none());
    assert!(query(
        &production.container,
        &credential.user,
        &credential.password,
        &credential.database,
        "SELECT value FROM sentinel;"
    )
    .status
    .success());
    dispatcher.abort();
    engine.cleanup_app(&removed, true, &[]).await;
    let volume = engine
        .store
        .with(|c| vercelify_platform::volumes::checked_list(c, VolumeOwner::Database, &production.id))
        .unwrap()
        .remove(0);
    engine
        .remove_database(
            &production,
            true,
            None,
            &vercelify_platform::volumes::physical_name(&volume, &format!("db-{}", production.name), None),
        )
        .await
        .unwrap();
}

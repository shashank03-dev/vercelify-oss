//! Owns only the disposable daemon; production is represented by unchanged
//! fixtures continuously probed while an isolated dataset is restored.
mod common;
use std::{
    collections::BTreeMap,
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vercelify_core::platform::*;
use vercelify_platform::{
    apps,
    build::{Builder, Driver},
    databases,
    dbengine::Restic,
    deploy::Deployer,
    deployments,
    docker::Docker,
    engine::{Engine, Enqueue},
    gate::BuildGate,
    logs::LogHub,
    nodes,
    proxy::{self, ProxyConfig},
    restore_drill::{self, Assertion, Drill, Recovery},
    secrets::MasterKey,
    Store,
};
async fn get() -> bool {
    let work = async {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", 18080)).await.ok()?;
        s.write_all(b"GET / HTTP/1.1\r\nHost: drill-serving.test\r\nConnection: close\r\n\r\n")
            .await
            .ok()?;
        let mut out = String::new();
        s.read_to_string(&mut out).await.ok()?;
        Some(out.starts_with("HTTP/1.1 200") && out.contains("Hostname:"))
    };
    tokio::time::timeout(Duration::from_secs(3), work).await.ok().flatten() == Some(true)
}
fn sql(docker: &Docker, db: &Database, key: &MasterKey, store: &Store, sql: &str) -> String {
    let cred = store.with(|c| databases::credentials(c, key, &db.id)).unwrap();
    let mut p = std::process::Command::new(&docker.bin)
        .args([
            "exec",
            "-i",
            &db.container,
            "psql",
            "-X",
            "-q",
            "-U",
            &cred.user,
            "-d",
            &cred.database,
            "-At",
            "-v",
            "ON_ERROR_STOP=1",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    p.stdin.take().unwrap().write_all(sql.as_bytes()).unwrap();
    let out = p.wait_with_output().unwrap();
    assert!(out.status.success(), "SQL fixture failed");
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn engine(store: Arc<Store>, key: Arc<MasterKey>, dir: &std::path::Path, config: &ProxyConfig) -> Arc<Engine> {
    Engine::new(
        store,
        key,
        Builder {
            docker: "docker".into(),
            git: "git".into(),
            work_dir: dir.join("work"),
            driver: Driver::Container,
            memory: Some("1024m".into()),
            timeout: Duration::from_secs(300),
        },
        Deployer {
            docker: Docker::default(),
            proxy: config.clone(),
            hdd_root: dir.join("hdd"),
            cgroup_parent: None,
            health_interval: Duration::from_millis(100),
        },
        BuildGate::new(0, dir.join("power")),
        LogHub::new(dir.join("logs")),
    )
}
async fn finish(e: &Engine, id: &str, restic: &Restic) -> Drill {
    let end = Instant::now() + Duration::from_secs(180);
    loop {
        let job = e.execute_restore_drill(id, restic).await.unwrap();
        if matches!(job.state.as_str(), "succeeded" | "failed" | "cancelled") {
            return job;
        }
        assert!(Instant::now() < end, "drill stalled: {job:?}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
fn repository(repo: &std::path::Path, password: &str) -> Restic {
    Restic {
        bin: "restic".into(),
        env: BTreeMap::from([
            ("RESTIC_REPOSITORY".into(), repo.to_string_lossy().into()),
            ("RESTIC_PASSWORD".into(), password.into()),
        ]),
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "owns disposable Docker: VERCELIFY_RESTORE_DRILLS=1"]
async fn isolated_restore_drills_verify_selected_snapshots_and_fail_safely() {
    assert_eq!(std::env::var("VERCELIFY_RESTORE_DRILLS").as_deref(), Ok("1"));
    assert!(!std::path::Path::new("/etc/vercelify").exists());
    if let Ok(path) = std::env::var("VFY_DRILL_CHILD") {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let dir = std::path::Path::new(value["dir"].as_str().unwrap());
        let store = Arc::new(Store::open(&dir.join("platform.db")).unwrap());
        let key = Arc::new(MasterKey::from_bytes(&[73; 32]).unwrap());
        let config = ProxyConfig {
            dir: dir.join("proxy"),
            http_port: 18080,
            https_port: 18443,
            metrics_port: 18082,
            acme_email: None,
            dns_challenge: false,
            image: proxy::TRAEFIK_IMAGE.into(),
        };
        let e = engine(store, key, dir, &config);
        let repo = repository(
            std::path::Path::new(value["repo"].as_str().unwrap()),
            "drill-only-password",
        );
        e.execute_restore_drill(value["job"].as_str().unwrap(), &repo)
            .await
            .unwrap();
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("platform.db")).unwrap());
    let key = Arc::new(MasterKey::from_bytes(&[73; 32]).unwrap());
    store
        .tx(|c| nodes::ensure_main(c, "drill-test", "http://127.0.0.1:7420", std::env::consts::ARCH))
        .unwrap();
    let inventory = common::inventory(store.clone(), dir.path()).await;
    let docker = Docker::default();
    let config = ProxyConfig {
        dir: dir.path().join("proxy"),
        http_port: 18080,
        https_port: 18443,
        metrics_port: 18082,
        acme_email: None,
        dns_challenge: false,
        image: proxy::TRAEFIK_IMAGE.into(),
    };
    let _ = docker.remove(proxy::CONTAINER).await;
    proxy::ensure(&docker, &config, vec![]).await.unwrap();
    let e = engine(store.clone(), key.clone(), dir.path(), &config);
    let queue = tokio::spawn(e.clone().run());
    let app=store.tx(|c|apps::create(c,&serde_json::from_value(serde_json::json!({"name":"drill-serving","kind":"image","source":{"image":"traefik/whoami:v1.11"},"runtime":{"port":80,"memory_mb":64,"cpus":1}})).unwrap())).unwrap();
    store
        .tx(|c| {
            apps::add_domain(
                c,
                &app.id,
                &DomainRequest {
                    host: "drill-serving.test".into(),
                    https: Some(false),
                    primary: Some(true),
                    redirect_to: None,
                },
            )
        })
        .unwrap();
    let deployment = e.enqueue(&app.id, Enqueue::default()).unwrap();
    let end = Instant::now() + Duration::from_secs(60);
    while !get().await {
        let d = store.with(|c| deployments::get(c, &deployment.id)).unwrap();
        assert!(d.status != DeploymentStatus::Failed, "{}", d.error);
        assert!(Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let source = store
        .tx(|c| {
            databases::create(
                c,
                &key,
                &serde_json::from_value(serde_json::json!({"name":"drill-source","kind":"postgresql","version":"17"}))
                    .unwrap(),
            )
        })
        .unwrap();
    e.ensure_database(&source.id).await.unwrap();
    sql(&docker,&source,&key,&store,"CREATE TABLE entries(id integer PRIMARY KEY,payload text); INSERT INTO entries SELECT n,repeat('fixture-',20) FROM generate_series(1,20000)n; CREATE INDEX payload_idx ON entries(payload); CREATE TABLE migrations(version text); INSERT INTO migrations VALUES('20261103');");
    let repo = dir.path().join("repository");
    let good = repository(&repo, "drill-only-password");
    let init = std::process::Command::new("restic")
        .args(["init"])
        .envs(&good.env)
        .output()
        .unwrap();
    assert!(init.status.success(), "restic fixture init failed");
    let backup = e.backup_database(&source.id, &good).await.unwrap();
    let m = store.with(|c| restore_drill::manifest(c, &backup.id)).unwrap();
    assert_eq!(m.server_major, 17);
    assert!(m
        .objects
        .iter()
        .any(|(_, name, kind)| name == "payload_idx" && kind == "i"));
    // The selected snapshot must restore its own known state, not match a
    // later live count, and must never rewind the now-newer production state.
    sql(
        &docker,
        &source,
        &key,
        &store,
        "INSERT INTO entries VALUES(20001,'after-backup'); UPDATE migrations SET version='20261104';",
    );
    let stop = Arc::new(AtomicBool::new(false));
    let count = Arc::new(AtomicUsize::new(0));
    let failures = Arc::new(AtomicUsize::new(0));
    let probes = tokio::spawn({
        let (stop, count, failures) = (stop.clone(), count.clone(), failures.clone());
        async move {
            while !stop.load(Ordering::Relaxed) {
                count.fetch_add(1, Ordering::Relaxed);
                if !get().await {
                    failures.fetch_add(1, Ordering::Relaxed);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    });
    let assertions = vec![
        Assertion {
            name: "row-count".into(),
            query: "SELECT count(*) FROM entries".into(),
            expected: "20000".into(),
        },
        Assertion {
            name: "migration".into(),
            query: "SELECT version FROM migrations".into(),
            expected: "20261103".into(),
        },
        Assertion {
            name: "query-window".into(),
            query: "SELECT 1 FROM pg_sleep(4)".into(),
            expected: "1".into(),
        },
    ];
    let job = store
        .tx(|c| restore_drill::enqueue(c, &key, &source.id, &backup.id, &assertions))
        .unwrap();
    store
        .tx(|c| {
            c.execute("UPDATE restore_drills SET full_integrity=1 WHERE id=?1", [&job.id])?;
            Ok(())
        })
        .unwrap();
    let inspect_job = job.id.clone();
    let inspecting = e.clone();
    let inspected = tokio::spawn(async move {
        let end = Instant::now() + Duration::from_secs(60);
        let mut saw_database = false;
        let mut saw_helper = false;
        while Instant::now() < end {
            let all = inspecting.deployer.docker.list_managed().await.unwrap();
            for container in all
                .iter()
                .filter(|c| c.labels.get("vercelify.restore-drill") == Some(&inspect_job))
            {
                let raw = inspecting
                    .deployer
                    .docker
                    .run(&["inspect".into(), container.name.clone()], Duration::from_secs(5))
                    .await
                    .unwrap();
                if !raw.success() {
                    continue;
                }
                let raw: serde_json::Value = serde_json::from_str(&raw.stdout).unwrap();
                let c = &raw[0];
                assert_eq!(c["HostConfig"]["LogConfig"]["Type"], "none");
                assert_eq!(c["HostConfig"]["NanoCpus"], 1_000_000_000u64);
                let memory = c["HostConfig"]["Memory"].as_u64().unwrap();
                assert_eq!(c["HostConfig"]["MemorySwap"].as_u64(), Some(memory));
                if container.labels.get("vercelify.kind").map(String::as_str) == Some("db") {
                    assert_eq!(memory, 256 * 1024 * 1024);
                    let networks = c["NetworkSettings"]["Networks"].as_object().unwrap();
                    assert_eq!(networks.len(), 1);
                    assert!(networks.keys().all(|n| n.starts_with("vfy-preview-drill-")));
                    assert!(c["HostConfig"]["PortBindings"].as_object().is_none_or(|p| p.is_empty()));
                    let network = networks.keys().next().unwrap();
                    let inspected = inspecting
                        .deployer
                        .docker
                        .run(
                            &["network".into(), "inspect".into(), network.clone()],
                            Duration::from_secs(5),
                        )
                        .await
                        .unwrap();
                    let inspected: serde_json::Value = serde_json::from_str(&inspected.stdout).unwrap();
                    assert_eq!(inspected[0]["Internal"], true);
                    assert_eq!(
                        inspected[0]["Options"]["com.docker.network.bridge.gateway_mode_ipv4"],
                        "isolated"
                    );
                    assert!(inspected[0]["Containers"]
                        .as_object()
                        .unwrap()
                        .values()
                        .all(|c| c["Name"] != proxy::CONTAINER));
                    vercelify_platform::preview_network::reconnect(&inspecting.deployer.docker)
                        .await
                        .unwrap();
                    assert!(!inspecting
                        .deployer
                        .docker
                        .inspect(proxy::CONTAINER)
                        .await
                        .unwrap()
                        .unwrap()
                        .ips
                        .contains_key(network));
                    saw_database = true;
                } else {
                    assert_eq!(memory, 512 * 1024 * 1024);
                    assert!(c["Mounts"].as_array().unwrap().iter().all(|m| m["RW"] == false));
                    saw_helper = true;
                }
            }
            if saw_database && saw_helper {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("did not inspect capped quarantine and repository helper");
    });
    let result = finish(&e, &job.id, &good).await;
    assert_eq!(result.state, "succeeded", "{result:?}");
    assert!(result.verified_at.is_some() && result.restore_seconds.is_some() && result.rpo_seconds.is_some());
    assert_eq!(result.integrity, "full_data_checked");
    inspected.await.unwrap();
    println!(
        "PASS: selected PostgreSQL snapshot, catalog/index, known row count/migration, bounded helpers and cleanup"
    );
    let wrong = repository(&repo, "wrong-recovery-password");
    for (label, config) in [("wrong-key", wrong.clone()), ("missing-key", Restic::default())] {
        let job = store
            .tx(|c| restore_drill::enqueue(c, &key, &source.id, &backup.id, &assertions))
            .unwrap();
        let result = finish(&e, &job.id, &config).await;
        assert_eq!(result.state, "failed", "{label}: {result:?}");
        assert!(!result.cleanup_pending);
        println!("PASS: {label} fails without modifying production");
    }
    let mut bad = assertions.clone();
    bad[0].expected = "19999".into();
    let job = store
        .tx(|c| restore_drill::enqueue(c, &key, &source.id, &backup.id, &bad))
        .unwrap();
    let result = finish(&e, &job.id, &good).await;
    assert_eq!(result.state, "failed");
    assert!(result.error.contains("row-count"));
    assert!(result.verified_at.is_none());
    println!("PASS: failed known assertion cannot become verified");
    // A fresh control store/key, independent provided recovery artifact and no
    // original source row/master key. Host credentials are deliberately wrong.
    let recovered_dir = dir.path().join("fresh-recovery");
    std::fs::create_dir_all(&recovered_dir).unwrap();
    let fresh = Arc::new(Store::open(&recovered_dir.join("platform.db")).unwrap());
    let fresh_key = Arc::new(MasterKey::from_bytes(&[99; 32]).unwrap());
    fresh
        .tx(|c| nodes::ensure_main(c, "fresh-recovery", "http://127.0.0.1:7420", std::env::consts::ARCH))
        .unwrap();
    let _fresh_inventory = common::inventory(fresh.clone(), dir.path()).await;
    let recovered = engine(fresh.clone(), fresh_key.clone(), &recovered_dir, &config);
    // Copy the encrypted repository and a separately retained mode-0600
    // recovery artifact before reading either in the fresh control environment.
    let copied_repo = recovered_dir.join("encrypted-repository");
    copy_tree(&repo, &copied_repo);
    let kit = recovered_dir.join("recovery-artifact.json");
    let recovery = Recovery {
        repository: copied_repo.to_string_lossy().into(),
        password: "drill-only-password".into(),
        access: BTreeMap::new(),
    };
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&kit)
        .unwrap();
    file.write_all(
        &serde_json::to_vec(&serde_json::json!({"manifest":m,"snapshot":backup.snapshot,"recovery":recovery})).unwrap(),
    )
    .unwrap();
    drop(file);
    let artifact: serde_json::Value = serde_json::from_slice(&std::fs::read(&kit).unwrap()).unwrap();
    let imported_manifest: restore_drill::Manifest = serde_json::from_value(artifact["manifest"].clone()).unwrap();
    let imported_recovery: Recovery = serde_json::from_value(artifact["recovery"].clone()).unwrap();
    let job = fresh
        .tx(|c| {
            restore_drill::enqueue_imported(
                c,
                &fresh_key,
                &imported_manifest,
                artifact["snapshot"].as_str().unwrap(),
                &assertions,
                &imported_recovery,
            )
        })
        .unwrap();
    let result = finish(&recovered, &job.id, &wrong).await;
    assert_eq!(result.state, "succeeded", "{result:?}");
    assert_eq!(result.credential_source, "provided");
    assert!(fresh
        .with(|c| restore_drill::recovery(c, &fresh_key, &job.id))
        .unwrap()
        .is_none());
    println!("PASS: fresh control key/store uses independent provided recovery credentials, not host credentials");
    // A cancelled verification must never publish successful evidence.
    let job = store
        .tx(|c| restore_drill::enqueue(c, &key, &source.id, &backup.id, &assertions))
        .unwrap();
    let running = tokio::spawn({
        let e = e.clone();
        let good = good.clone();
        let id = job.id.clone();
        async move { e.execute_restore_drill(&id, &good).await.unwrap() }
    });
    wait_verifying(&store, &job.id).await;
    store.tx(|c| restore_drill::cancel(c, &job.id)).unwrap();
    let cancelled = running.await.unwrap();
    assert_eq!(cancelled.state, "cancelled");
    assert!(cancelled.verified_at.is_none() && !cancelled.cleanup_pending);
    println!("PASS: cancellation during verification cannot publish success");
    // Kill a real controller process while its quarantined database is live,
    // then reopen/reconcile the journal with the original production untouched.
    let job = store
        .tx(|c| restore_drill::enqueue(c, &key, &source.id, &backup.id, &assertions))
        .unwrap();
    let child_config = dir.path().join("child.json");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&child_config)
        .unwrap();
    file.write_all(&serde_json::to_vec(&serde_json::json!({"dir":dir.path(),"repo":repo,"job":job.id})).unwrap())
        .unwrap();
    drop(file);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--nocapture"])
        .env("VFY_DRILL_CHILD", &child_config)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_verifying(&store, &job.id).await;
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    let reopened = Arc::new(Store::open(&dir.path().join("platform.db")).unwrap());
    reopened
        .tx(|c| vercelify_platform::capacity::recover(c, chrono::Utc::now().timestamp()))
        .unwrap();
    let resumed = engine(reopened.clone(), key.clone(), dir.path(), &config);
    let result = finish(&resumed, &job.id, &good).await;
    assert_eq!(result.state, "failed");
    assert!(result.verified_at.is_none() && !result.cleanup_pending);
    assert!(reopened.with(|c| restore_drill::service(c, &job.id)).unwrap().is_none());
    println!("PASS: SIGKILL controller, reopen journal, clean unverified temporary dataset");
    // Missing filename and invalid custom archive fail despite a readable,
    // internally consistent encrypted repository.
    for (label, filename, data) in [
        ("missing-dump", "other.dump", b"not a PostgreSQL dump".as_slice()),
        ("invalid-dump", "drill-source.dump", b"not a PostgreSQL dump".as_slice()),
    ] {
        let mut process = std::process::Command::new("restic")
            .args(["backup", "--stdin", "--stdin-filename", filename, "--json"])
            .envs(&good.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        process.stdin.take().unwrap().write_all(data).unwrap();
        let output = process.wait_with_output().unwrap();
        assert!(output.status.success());
        let summary: serde_json::Value = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["message_type"] == "summary")
            .unwrap();
        let snapshot = summary["snapshot_id"].as_str().unwrap();
        let mut bad = m.clone();
        bad.bytes = data.len() as u64;
        let recovery = Recovery {
            repository: repo.to_string_lossy().into(),
            password: "drill-only-password".into(),
            access: BTreeMap::new(),
        };
        let job = store
            .tx(|c| restore_drill::enqueue_imported(c, &key, &bad, snapshot, &[], &recovery))
            .unwrap();
        let result = finish(&e, &job.id, &wrong).await;
        assert_eq!(result.state, "failed", "{label}: {result:?}");
        assert!(result.verified_at.is_none() && !result.cleanup_pending);
        println!("PASS: {label} stays unverified");
    }
    let mut incompatible = m.clone();
    incompatible
        .extensions
        .push(("unavailable_extension".into(), "1.0".into()));
    incompatible.extensions.sort();
    let recovery = Recovery {
        repository: repo.to_string_lossy().into(),
        password: "drill-only-password".into(),
        access: BTreeMap::new(),
    };
    let job = store
        .tx(|c| restore_drill::enqueue_imported(c, &key, &incompatible, &backup.snapshot, &[], &recovery))
        .unwrap();
    let result = finish(&e, &job.id, &good).await;
    assert_eq!(result.state, "failed");
    assert!(result.verified_at.is_none() && !result.cleanup_pending);
    println!("PASS: incompatible extension catalog cannot become verified");
    let corrupt_repo = dir.path().join("corrupt-repository");
    copy_tree(&repo, &corrupt_repo);
    let pack = first_file(&corrupt_repo.join("data")).unwrap();
    let mut bytes = std::fs::read(&pack).unwrap();
    let index = bytes.len() / 2;
    bytes[index] ^= 0x40;
    std::fs::write(&pack, bytes).unwrap();
    let recovery = Recovery {
        repository: corrupt_repo.to_string_lossy().into(),
        password: "drill-only-password".into(),
        access: BTreeMap::new(),
    };
    let job = store
        .tx(|c| {
            let job = restore_drill::enqueue_imported(c, &key, &m, &backup.snapshot, &[], &recovery)?;
            c.execute("UPDATE restore_drills SET full_integrity=1 WHERE id=?1", [&job.id])?;
            Ok(job)
        })
        .unwrap();
    let result = finish(&e, &job.id, &good).await;
    assert_eq!(result.state, "failed");
    assert_eq!(result.integrity, "check_failed");
    assert!(result.verified_at.is_none() && !result.cleanup_pending);
    println!("PASS: ciphertext pack corruption fails full repository data verification");
    // Simulated low battery defers before destination or repository work.
    drop(inventory);
    let original = store.with(nodes::main).unwrap().status;
    let mut low = original.clone();
    low.on_battery = true;
    low.battery_percent = Some(40);
    store.tx(|c| nodes::set_main_status(c, "", &low)).unwrap();
    let job = store
        .tx(|c| restore_drill::enqueue(c, &key, &source.id, &backup.id, &assertions))
        .unwrap();
    let result = e.execute_restore_drill(&job.id, &good).await.unwrap();
    assert_eq!(result.state, "deferred");
    assert!(store.with(|c| restore_drill::service(c, &job.id)).unwrap().is_none());
    store.tx(|c| restore_drill::cancel(c, &job.id)).unwrap();
    assert_eq!(
        e.execute_restore_drill(&job.id, &good).await.unwrap().state,
        "cancelled"
    );
    store.tx(|c| nodes::set_main_status(c, "", &original)).unwrap();
    let main = store.with(nodes::main).unwrap();
    let mut measured = store
        .with(|c| vercelify_platform::capacity::sample(c, &main.id))
        .unwrap();
    let mut pressure = measured.clone();
    pressure.available_mb = 0;
    pressure.measured_at = chrono::Utc::now().timestamp();
    store
        .tx(|c| vercelify_platform::capacity::record(c, &main.id, &pressure))
        .unwrap();
    let job = store
        .tx(|c| restore_drill::enqueue(c, &key, &source.id, &backup.id, &assertions))
        .unwrap();
    let result = e.execute_restore_drill(&job.id, &good).await.unwrap();
    assert_eq!(result.state, "deferred");
    let service = store.with(|c| restore_drill::service(c, &job.id)).unwrap().unwrap();
    let destination = store
        .with(|c| databases::get(c, &key, &service.database_id, false))
        .unwrap();
    assert!(docker.inspect(&destination.container).await.unwrap().is_none());
    store.tx(|c| restore_drill::cancel(c, &job.id)).unwrap();
    assert_eq!(
        e.execute_restore_drill(&job.id, &good).await.unwrap().state,
        "cancelled"
    );
    measured.measured_at = chrono::Utc::now().timestamp();
    store
        .tx(|c| vercelify_platform::capacity::record(c, &main.id, &measured))
        .unwrap();
    let _inventory = common::inventory(store.clone(), dir.path()).await;
    println!("PASS: simulated exhausted memory defers before starting quarantine and cancels cleanly");
    println!("PASS: simulated low-battery report defers heavy work and cancellation cleans ownership");
    assert_eq!(
        sql(&docker, &source, &key, &store, "SELECT count(*) FROM entries"),
        "20001"
    );
    assert_eq!(
        sql(&docker, &source, &key, &store, "SELECT version FROM migrations"),
        "20261104"
    );
    assert!(store.with(|c| restore_drill::list(c, None, true)).unwrap().is_empty());
    assert!(store
        .with(|c| databases::list(c, &key, None, false))
        .unwrap()
        .iter()
        .all(|d| d.id == source.id));
    stop.store(true, Ordering::Relaxed);
    probes.await.unwrap();
    assert_eq!(failures.load(Ordering::Relaxed), 0);
    println!(
        "PASS: {} unchanged production probes, zero failures",
        count.load(Ordering::Relaxed)
    );
    let owned = store
        .with(|c| vercelify_platform::volumes::checked_list(c, VolumeOwner::Database, &source.id))
        .unwrap();
    let storage = vercelify_platform::volumes::physical_name(&owned[0], &format!("db-{}", source.name), None);
    e.remove_database(&source, true, None, &storage).await.unwrap();
    store.tx(|c| databases::delete(c, &key, &source.id)).unwrap();
    store.tx(|c| apps::delete(c, &app.id)).unwrap();
    e.cleanup_app(&app, false, &[]).await;
    queue.abort();
    let _ = docker.remove(proxy::CONTAINER).await;
}

fn copy_tree(source: &std::path::Path, destination: &std::path::Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
async fn wait_verifying(store: &Store, id: &str) {
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        let job = store.with(|c| restore_drill::get(c, id)).unwrap();
        if job.state == "verifying" {
            return;
        }
        assert!(
            !matches!(job.state.as_str(), "failed" | "succeeded" | "cancelled"),
            "{job:?}"
        );
        assert!(Instant::now() < end, "{job:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn first_file(directory: &std::path::Path) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(directory).ok()? {
        let entry = entry.ok()?;
        if entry.file_type().ok()?.is_dir() {
            if let Some(file) = first_file(&entry.path()) {
                return Some(file);
            }
        } else {
            return Some(entry.path());
        }
    }
    None
}

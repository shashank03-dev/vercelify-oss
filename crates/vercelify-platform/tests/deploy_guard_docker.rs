//! Destructive only inside the owned disposable Docker daemon.
mod common;
use std::{
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
    deploy::Deployer,
    deploy_guard, deployments,
    docker::Docker,
    engine::{Engine, Enqueue},
    gate::BuildGate,
    logs::LogHub,
    nodes,
    proxy::{self, ProxyConfig},
    secrets::MasterKey,
    Store,
};
async fn get(host: &str) -> Option<(u16, String)> {
    let fut = async {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", 18080)).await.ok()?;
        s.write_all(format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes())
            .await
            .ok()?;
        let mut out = String::new();
        s.read_to_string(&mut out).await.ok()?;
        Some((
            out.split_whitespace().nth(1)?.parse().ok()?,
            out.split_once("\r\n\r\n")?.1.to_string(),
        ))
    };
    tokio::time::timeout(Duration::from_secs(5), fut).await.ok().flatten()
}
async fn healthy(e: &Engine, id: &str) -> Deployment {
    let end = Instant::now() + Duration::from_secs(90);
    loop {
        let d = e.store.with(|c| deployments::get(c, id)).unwrap();
        if d.status == DeploymentStatus::Healthy {
            return d;
        }
        assert!(
            !matches!(d.status, DeploymentStatus::Failed | DeploymentStatus::Cancelled),
            "{}",
            d.error
        );
        assert!(Instant::now() < end, "deployment stalled: {d:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn body(host: &str, want: &str) {
    let end = Instant::now() + Duration::from_secs(15);
    while !get(host).await.is_some_and(|(s, b)| s == 200 && b.contains(want)) {
        assert!(Instant::now() < end, "{host}: expected {want}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "owns disposable Docker: VERCELIFY_GUARD_TESTS=1"]
async fn running_http_failure_crash_loop_and_failed_recovery_are_durable() {
    assert_eq!(std::env::var("VERCELIFY_GUARD_TESTS").as_deref(), Ok("1"));
    assert!(!std::path::Path::new("/etc/vercelify").exists());
    let dir = tempfile::tempdir().unwrap();
    let docker = Docker::default();
    let store = Arc::new(Store::open(&dir.path().join("platform.db")).unwrap());
    store
        .tx(|c| nodes::ensure_main(c, "guard-test", "http://127.0.0.1:7420", std::env::consts::ARCH))
        .unwrap();
    let _inventory = common::inventory(store.clone(), dir.path()).await;
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
    let key = Arc::new(MasterKey::from_bytes(&[31; 32]).unwrap());
    let wrapper = dir.path().join("docker");
    std::fs::write(&wrapper,format!("#!/bin/sh\nif [ \"$1\" = create ] && [ -f '{}/fail-old-create' ]; then\n case \"$*\" in *'{}'*) echo 'qualification: injected old-image container create failure' >&2; exit 73 ;; esac\nfi\nexec docker \"$@\"\n",dir.path().display(),"OLD_IMAGE_PLACEHOLDER")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let e = Engine::new(
        store.clone(),
        key,
        Builder {
            docker: "docker".into(),
            git: "git".into(),
            work_dir: dir.path().join("work"),
            driver: Driver::Container,
            memory: Some("1024m".into()),
            timeout: Duration::from_secs(300),
        },
        Deployer {
            docker: Docker {
                bin: wrapper.display().to_string(),
            },
            proxy: proxy.clone(),
            hdd_root: dir.path().join("hdd"),
            health_interval: Duration::from_millis(100),
            cgroup_parent: None,
        },
        BuildGate::new(0, dir.path().join("power")),
        LogHub::new(dir.path().join("logs")),
    );
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut images = vec![];
    for version in ["old", "new"] {
        std::fs::write(src.join("server.js"),format!(r#"const fs=require('fs');const version='{version}';require('http').createServer((q,s)=>{{s.statusCode=fs.existsSync('/tmp/fail-http')?503:200;s.end(version)}}).listen(8080);setInterval(()=>{{if(fs.existsSync('/tmp/crash'))process.exit(7)}},100);"#)).unwrap();
        std::fs::write(
            src.join("Dockerfile"),
            "FROM node:22-slim\nCOPY server.js /server.js\nEXPOSE 8080\nCMD [\"node\",\"/server.js\"]\n",
        )
        .unwrap();
        let result = std::process::Command::new("docker")
            .args(["build", "-q", "--network=none"])
            .arg(&src)
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let id = String::from_utf8(result.stdout)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .to_owned();
        assert!(id.starts_with("sha256:"));
        images.push(id);
    }
    let wrapper_text = std::fs::read_to_string(&wrapper)
        .unwrap()
        .replace("OLD_IMAGE_PLACEHOLDER", &images[0]);
    std::fs::write(&wrapper, wrapper_text).unwrap();
    let dispatcher = tokio::spawn(e.clone().run());
    let create = |name: &str, image: &str, compatible: bool| -> App {
        store.tx(|c|apps::create(c,&serde_json::from_value(serde_json::json!({"name":name,"kind":"image","source":{"image":image},"runtime":{"port":8080,"memory_mb":128,"cpus":1,"health":{"path":"/health"},"health_timeout_secs":10,"drain_secs":0,"rollback_compatible":compatible,"rollback_grace_secs":0,"rollback_failures":3}})).unwrap())).unwrap()
    };
    let sentinel = create("guard-sentinel", &images[0], false);
    store
        .tx(|c| {
            apps::add_domain(
                c,
                &sentinel.id,
                &DomainRequest {
                    host: "guard-sentinel.test".into(),
                    https: Some(false),
                    primary: Some(true),
                    redirect_to: None,
                },
            )
        })
        .unwrap();
    let serving = e.enqueue(&sentinel.id, Enqueue::default()).unwrap();
    healthy(&e, &serving.id).await;
    body("guard-sentinel.test", "old").await;
    let stop = Arc::new(AtomicBool::new(false));
    let count = Arc::new(AtomicUsize::new(0));
    let failures = Arc::new(AtomicUsize::new(0));
    let probe = tokio::spawn({
        let stop = stop.clone();
        let count = count.clone();
        let failures = failures.clone();
        async move {
            while !stop.load(Ordering::Relaxed) {
                count.fetch_add(1, Ordering::Relaxed);
                if !get("guard-sentinel.test")
                    .await
                    .is_some_and(|(s, b)| s == 200 && b.contains("old"))
                {
                    failures.fetch_add(1, Ordering::Relaxed);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    });
    for case in ["http", "crash", "missing", "changed", "newer", "failed"] {
        let name = format!("guard-{case}");
        let host = format!("{name}.test");
        let app = create(&name, &images[0], true);
        store
            .tx(|c| {
                apps::add_domain(
                    c,
                    &app.id,
                    &DomainRequest {
                        host: host.clone(),
                        https: Some(false),
                        primary: Some(true),
                        redirect_to: None,
                    },
                )
            })
            .unwrap();
        let first = e.enqueue(&app.id, Enqueue::default()).unwrap();
        healthy(&e, &first.id).await;
        body(&host, "old").await;
        let mut source = app.source.clone();
        source.image = Some(images[1].clone());
        store
            .tx(|c| {
                apps::patch(
                    c,
                    &app.id,
                    &AppPatch {
                        source: Some(source),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let second = e.enqueue(&app.id, Enqueue::default()).unwrap();
        let second = healthy(&e, &second.id).await;
        body(&host, "new").await;
        let guard = store
            .with(|c| deploy_guard::get(c, &second.id))
            .unwrap()
            .expect("guard armed at publication");
        assert_eq!(guard.state, "watching");
        assert!(guard.baseline.values().all(|n| *n == 0));
        // A reopened SQLite file keeps the same observation deadline and baseline.
        let reopened = Store::open(&dir.path().join("platform.db")).unwrap();
        assert_eq!(
            reopened
                .with(|c| deploy_guard::get(c, &second.id))
                .unwrap()
                .unwrap()
                .deadline,
            guard.deadline
        );
        drop(reopened);
        if case == "changed" {
            let mut runtime = app.runtime.clone();
            runtime.memory_mb = Some(256);
            store
                .tx(|c| {
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
        } else if case == "newer" {
            let newer = e.enqueue(&app.id, Enqueue::default()).unwrap();
            e.poll_deploy_guards().await;
            healthy(&e, &newer.id).await;
        } else {
            if case == "missing" {
                store
                    .tx(|c| {
                        c.execute(
                            "UPDATE deployments SET image=?2 WHERE id=?1",
                            rusqlite::params![first.id, format!("sha256:{}", "0".repeat(64))],
                        )?;
                        Ok(())
                    })
                    .unwrap();
            }
            if case == "failed" {
                std::fs::write(dir.path().join("fail-old-create"), "qualification fault").unwrap();
            }
            let container = store.with(|c| deployments::container(c, &second.id)).unwrap();
            let flag = if case == "crash" {
                "/tmp/crash"
            } else {
                "/tmp/fail-http"
            };
            assert!(docker
                .exec(&container, &["touch".into(), flag.into()], Duration::from_secs(5))
                .await
                .unwrap()
                .success());
            if case == "crash" {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
        let end = Instant::now() + Duration::from_secs(70);
        let outcome = loop {
            let errors = e.poll_deploy_guards().await;
            assert!(errors.is_empty(), "{errors:?}");
            let guard = store.with(|c| deploy_guard::get(c, &second.id)).unwrap().unwrap();
            if !matches!(guard.state.as_str(), "watching" | "requested") {
                break guard;
            }
            assert!(Instant::now() < end, "{case}: guard did not finish: {guard:?}");
            tokio::time::sleep(Duration::from_millis(300)).await;
        };
        let expected = match case {
            "http" | "crash" => "verified",
            "changed" | "newer" => "cancelled",
            _ => "failed",
        };
        assert_eq!(outcome.state, expected, "{case}: {outcome:?}");
        let rollbacks = store
            .with(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM deployments WHERE app_id=?1 AND trigger='auto-rollback'",
                    [&app.id],
                    |r| r.get::<_, usize>(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            rollbacks,
            usize::from(matches!(case, "http" | "crash" | "failed")),
            "{case}"
        );
        if expected == "verified" {
            body(&host, "old").await;
            assert!(outcome.rollback_id.is_some());
        }
        println!("PASS: {case}: {} with {rollbacks} rollback job(s)", outcome.state);
        let _ = std::fs::remove_file(dir.path().join("fail-old-create"));
        store.tx(|c| apps::delete(c, &app.id)).unwrap();
        e.cleanup_app(&app, false, &[]).await;
    }
    stop.store(true, Ordering::Relaxed);
    probe.await.unwrap();
    assert_eq!(failures.load(Ordering::Relaxed), 0, "unchanged production probes");
    println!(
        "PASS: {} unchanged production probes, zero failures",
        count.load(Ordering::Relaxed)
    );
    store.tx(|c| apps::delete(c, &sentinel.id)).unwrap();
    e.cleanup_app(&sentinel, false, &[]).await;
    dispatcher.abort();
    for image in images {
        let _ = docker.remove_image(&image).await;
    }
    let _ = docker.remove(proxy::CONTAINER).await;
}

//! The engine against a real Docker daemon and a real Traefik: deploy,
//! zero-downtime redeploy, instant rollback, and recovery of a deleted
//! container. Opt in with `VERCELIFY_DOCKER_TESTS=1` (needs Docker and the
//! node, caddy and traefik images or network access to pull them).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vercelify_core::platform::*;
use vercelify_platform::apps::{DomainSettings, DOMAIN_SETTING};
use vercelify_platform::build::{Builder, DirBuild, Driver};
use vercelify_platform::deploy::Deployer;
use vercelify_platform::docker::Docker;
use vercelify_platform::engine::{Engine, Enqueue};
use vercelify_platform::gate::BuildGate;
use vercelify_platform::logs::LogHub;
use vercelify_platform::proc::Cancel;
use vercelify_platform::proxy::{self, ProxyConfig};
use vercelify_platform::reconcile::Reconciler;
use vercelify_platform::secrets::MasterKey;
use vercelify_platform::{apps, deployments, env, store, Store};

const HTTP_PORT: u16 = 18080;

async fn get(host: &str) -> Result<(u16, String), String> {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", HTTP_PORT))
        .await
        .map_err(|e| e.to_string())?;
    s.write_all(format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut buf = String::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_string(&mut buf))
        .await
        .map_err(|_| "timeout".to_string())?
        .map_err(|e| e.to_string())?;
    let code = buf
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or("no status")?;
    let body = buf
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    Ok((code, body))
}

async fn wait_for(engine: &Engine, id: &str, want: DeploymentStatus) -> Deployment {
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        let d = engine.store.with(|c| deployments::get(c, id)).unwrap();
        if d.status == want {
            return d;
        }
        assert!(
            !matches!(d.status, DeploymentStatus::Failed | DeploymentStatus::Cancelled) || want == d.status,
            "deployment {} ended {:?}: {}\n{:#?}",
            d.number,
            d.status,
            d.error,
            engine.logs.read("web", id, Some(40))
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {want:?}, at {:?}",
            d.status
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn body_of(host: &str, want: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok((200, b)) = get(host).await {
            if b.contains(want) {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "{host} never answered {want}: {:?}",
            get(host).await
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs Docker: VERCELIFY_DOCKER_TESTS=1 cargo test … -- --include-ignored"]
async fn deploy_redeploy_rollback_recover() {
    if std::env::var("VERCELIFY_DOCKER_TESTS").as_deref() != Ok("1") {
        return;
    }
    // It runs an engine on this machine's Docker: never on a real server.
    assert!(
        !std::path::Path::new("/etc/vercelify").exists()
            || std::env::var("VERCELIFY_TESTS_ON_SERVER").as_deref() == Ok("1"),
        "this machine is a Vercelify server: this test would change its containers (VERCELIFY_TESTS_ON_SERVER=1 runs it anyway)"
    );
    let dir = tempfile::tempdir().unwrap();
    let docker = Docker::default();
    let logs = LogHub::new(dir.path().join("logs"));
    let builder = Builder {
        docker: "docker".into(),
        git: "git".into(),
        work_dir: dir.path().join("work"),
        driver: Driver::Docker,
        memory: None,
        timeout: Duration::from_secs(900),
    };

    // An image that answers with $MESSAGE.
    let src = dir.path().join("app");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("package.json"),
        r#"{"name":"web","scripts":{"start":"node server.js"}}"#,
    )
    .unwrap();
    std::fs::write(
        src.join("server.js"),
        "require('http').createServer((q,s)=>s.end(process.env.MESSAGE||'none')).listen(process.env.PORT)",
    )
    .unwrap();
    let mut w = logs.writer("fixture", "dpl_fixture");
    let built = builder
        .build_dir(
            DirBuild {
                dir: &src,
                app_name: "fixture",
                deployment_id: "dpl_fixtureweb",
                build: &AppBuild::default(),
                build_env: &[],
            },
            Cancel::never(),
            &mut w,
        )
        .await
        .expect("fixture image");
    drop(w);

    let proxy_cfg = ProxyConfig {
        dir: dir.path().join("proxy"),
        http_port: HTTP_PORT,
        https_port: 18443,
        metrics_port: 18082,
        acme_email: None,
        dns_challenge: false,
        image: proxy::TRAEFIK_IMAGE.into(),
    };
    let _ = docker.remove(proxy::CONTAINER).await;
    proxy::ensure(&docker, &proxy_cfg, vec![]).await.expect("proxy");

    let store_ = Arc::new(Store::open(&dir.path().join("platform.db")).unwrap());
    store_
        .tx(|c| vercelify_platform::nodes::ensure_main(c, "test", "http://127.0.0.1:7420", std::env::consts::ARCH))
        .unwrap();
    let key = Arc::new(MasterKey::from_bytes(&[5u8; 32]).unwrap());
    store_
        .with(|c| {
            store::put_setting(
                c,
                DOMAIN_SETTING,
                &DomainSettings {
                    wildcard: Some("test.localhost".into()),
                    https: false,
                    edge_tls: false,
                },
            )
        })
        .unwrap();
    let engine = Engine::new(
        store_.clone(),
        key.clone(),
        builder,
        Deployer {
            docker: docker.clone(),
            proxy: proxy_cfg.clone(),
            hdd_root: dir.path().join("hdd"),
            cgroup_parent: None,
            health_interval: Duration::from_millis(300),
        },
        BuildGate::new(1, dir.path().join("power-guard.state")),
        logs.clone(),
    );
    tokio::spawn(engine.clone().run());

    let app = store_
        .tx(|c| {
            apps::create(
                c,
                &AppCreate {
                    name: "web".into(),
                    kind: AppKind::Image,
                    environment_id: None,
                    project_id: None,
                    source: AppSource {
                        image: Some(built.image.clone()),
                        ..AppSource::default()
                    },
                    build: None,
                    runtime: Some(AppRuntime {
                        port: Some(3000),
                        drain_secs: 2,
                        health_timeout_secs: 60,
                        ..AppRuntime::default()
                    }),
                    previews: None,
                    node_id: None,
                    domains: vec![],
                },
            )
        })
        .unwrap();
    assert_eq!(app.url.as_deref(), Some("http://web.test.localhost"));
    let set_message = |m: &str| {
        let m = m.to_string();
        store_
            .tx(|c| {
                env::put(
                    c,
                    &key,
                    VarScope::App,
                    &app.id,
                    &EnvVarsPut {
                        vars: vec![EnvVarInput {
                            key: "MESSAGE".into(),
                            value: m,
                            build: None,
                            preview_only: None,
                            secret: None,
                        }],
                        replace: false,
                    },
                )
            })
            .unwrap();
    };

    // 1. First deploy.
    set_message("version one");
    let d1 = engine.enqueue("web", Enqueue::default()).unwrap();
    let d1 = wait_for(&engine, &d1.id, DeploymentStatus::Healthy).await;
    assert!(d1.current);
    body_of("web.test.localhost", "version one").await;

    // 2. Redeploy while hammering the site: no request may fail.
    let stop = Arc::new(AtomicBool::new(false));
    let failures = Arc::new(AtomicUsize::new(0));
    let ok = Arc::new(AtomicUsize::new(0));
    let prober = {
        let (stop, failures, ok) = (stop.clone(), failures.clone(), ok.clone());
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                match get("web.test.localhost").await {
                    Ok((200, _)) => ok.fetch_add(1, Ordering::Relaxed),
                    other => {
                        eprintln!("probe failed: {other:?}");
                        failures.fetch_add(1, Ordering::Relaxed)
                    }
                };
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };
    set_message("version two");
    let d2 = engine.enqueue("web", Enqueue::default()).unwrap();
    wait_for(&engine, &d2.id, DeploymentStatus::Healthy).await;
    body_of("web.test.localhost", "version two").await;
    tokio::time::sleep(Duration::from_secs(4)).await; // past the drain
    stop.store(true, Ordering::Relaxed);
    prober.await.unwrap();
    assert!(ok.load(Ordering::Relaxed) > 20, "the prober ran");
    assert_eq!(failures.load(Ordering::Relaxed), 0, "zero-downtime switch");
    let old = store_.with(|c| deployments::get(c, &d1.id)).unwrap();
    assert_eq!(old.status, DeploymentStatus::Superseded);
    let old_container = store_.with(|c| deployments::container(c, &d1.id)).unwrap();
    assert!(
        docker.inspect(&old_container).await.unwrap().is_none(),
        "old container retired"
    );

    // 3. Instant rollback to #1 (same image, its variables are today's).
    let rb = engine
        .enqueue(
            "web",
            Enqueue {
                rollback_of: Some(d1.id.clone()),
                ..Enqueue::default()
            },
        )
        .unwrap();
    let rb = wait_for(&engine, &rb.id, DeploymentStatus::Healthy).await;
    assert_eq!(rb.trigger, DeploymentTrigger::Rollback);
    assert!(engine
        .logs
        .read("web", &rb.id, None)
        .iter()
        .any(|l| l.text.contains("no build")));
    let pinned = store_.with(|c| apps::get(c, &app.id)).unwrap().source.pinned_sha;
    assert_eq!(pinned, None, "image apps have no commit to pin");

    // 4. Someone deletes the container: the reconciler brings it back.
    let current = store_
        .with(|c| apps::get(c, &app.id))
        .unwrap()
        .current_deployment_id
        .unwrap();
    let container = store_.with(|c| deployments::container(c, &current)).unwrap();
    docker.remove(&container).await.unwrap();
    let rec = Reconciler::new(engine.clone());
    let report = rec.once().await.unwrap();
    assert_eq!(report.recovering.len(), 1, "{report:?}");
    let recovered = store_.with(|c| deployments::list(c, &app.id, 1)).unwrap().remove(0);
    assert_eq!(recovered.trigger, DeploymentTrigger::Recover);
    wait_for(&engine, &recovered.id, DeploymentStatus::Healthy).await;
    body_of("web.test.localhost", "version two").await;

    // 5. Stopping the app removes its route; a second pass changes nothing.
    store_
        .tx(|c| {
            apps::patch(
                c,
                &app.id,
                &AppPatch {
                    desired: Some(Desired::Stopped),
                    ..AppPatch::default()
                },
            )
        })
        .unwrap();
    let report = rec.once().await.unwrap();
    assert_eq!(report.stopped.len(), 1, "{report:?}");
    assert!(proxy::route_keys(&proxy_cfg).is_empty());
    // Traefik notices the removed file within a moment.
    let deadline = Instant::now() + Duration::from_secs(15);
    while get("web.test.localhost").await.map(|r| r.0) != Ok(404) {
        assert!(Instant::now() < deadline, "the stopped app is still routed");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let again = rec.once().await.unwrap();
    assert!(again.stopped.is_empty() && again.errors.is_empty(), "{again:?}");

    // 6. Basic Auth in front of the app.
    store_
        .tx(|c| {
            apps::patch(
                c,
                &app.id,
                &AppPatch {
                    desired: Some(Desired::Running),
                    ..AppPatch::default()
                },
            )?;
            apps::set_basic_auth(c, &app.id, Some(("me", "a long password")))
        })
        .unwrap();
    rec.once().await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while get("web.test.localhost").await.map(|r| r.0) != Ok(401) {
        assert!(
            Instant::now() < deadline,
            "no password asked: {:?}",
            get("web.test.localhost").await
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    store_.tx(|c| apps::set_basic_auth(c, &app.id, None)).unwrap();
    rec.once().await.unwrap();
    body_of("web.test.localhost", "version two").await;

    // 7. A release that crashes after going live is rolled back by itself.
    let crash_src = dir.path().join("crash");
    std::fs::create_dir_all(&crash_src).unwrap();
    std::fs::write(
        crash_src.join("package.json"),
        r#"{"name":"web","scripts":{"start":"node server.js"}}"#,
    )
    .unwrap();
    std::fs::write(
        crash_src.join("server.js"),
        "require('http').createServer((q,s)=>s.end('crashy')).listen(process.env.PORT); setTimeout(()=>process.exit(1), 1500)",
    )
    .unwrap();
    let mut w = logs.writer("fixture", "dpl_crash");
    let crash = engine
        .builder
        .build_dir(
            DirBuild {
                dir: &crash_src,
                app_name: "fixture",
                deployment_id: "dpl_fixturecrash",
                build: &AppBuild::default(),
                build_env: &[],
            },
            Cancel::never(),
            &mut w,
        )
        .await
        .unwrap();
    drop(w);
    let good = store_
        .with(|c| apps::get(c, &app.id))
        .unwrap()
        .current_deployment_id
        .unwrap();
    store_
        .tx(|c| {
            apps::patch(
                c,
                &app.id,
                &AppPatch {
                    source: Some(AppSource {
                        image: Some(crash.image.clone()),
                        ..AppSource::default()
                    }),
                    ..AppPatch::default()
                },
            )
        })
        .unwrap();
    let bad = engine.enqueue("web", Enqueue::default()).unwrap();
    wait_for(&engine, &bad.id, DeploymentStatus::Healthy).await;
    let deadline = Instant::now() + Duration::from_secs(120);
    let rolled = loop {
        let latest = store_.with(|c| deployments::list(c, &app.id, 1)).unwrap().remove(0);
        if latest.trigger == DeploymentTrigger::AutoRollback {
            break latest;
        }
        assert!(Instant::now() < deadline, "no automatic rollback");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert_eq!(rolled.rollback_of.as_deref(), Some(good.as_str()));
    wait_for(&engine, &rolled.id, DeploymentStatus::Healthy).await;
    body_of("web.test.localhost", "version two").await;

    // 7b. Runtime operations: a volume, restart, and moving its data to the
    // HDD and back (the container is recreated, the data survives).
    store_
        .tx(|c| {
            apps::patch(
                c,
                &app.id,
                &AppPatch {
                    runtime: Some(AppRuntime {
                        replicas: 2,
                        ..AppRuntime::default()
                    }),
                    source: Some(AppSource {
                        image: Some(built.image.clone()),
                        ..AppSource::default()
                    }),
                    ..AppPatch::default()
                },
            )?;
            vercelify_platform::volumes::create(
                c,
                VolumeOwner::App,
                &app.id,
                &AppVolumeRequest {
                    name: "data".into(),
                    mount_path: "/data".into(),
                    tier: None,
                },
            )
        })
        .unwrap();
    let with_vol = engine.enqueue("web", Enqueue::default()).unwrap();
    wait_for(&engine, &with_vol.id, DeploymentStatus::Healthy).await;
    let live = engine.live_container(&app.id).unwrap();
    let sh = |c: &str, cmd: &str| {
        let o = std::process::Command::new("docker")
            .args(["exec", c, "sh", "-c", cmd])
            .output()
            .unwrap();
        assert!(o.status.success(), "{cmd}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    sh(&live, "echo kept > /data/f");
    let owner = sh(&live, "stat -c %u /data/f");
    assert_ne!(owner, "0", "the app's own (non-root) user wrote to its volume");
    assert_eq!(engine.restart_app(&app.id).await.unwrap(), 2);
    body_of("web.test.localhost", "version two").await;
    let vol = store_
        .with(|c| vercelify_platform::volumes::list(c, VolumeOwner::App, &app.id))
        .unwrap()
        .remove(0);
    let docker_root = std::path::PathBuf::from(
        String::from_utf8(
            std::process::Command::new("docker")
                .args(["info", "-f", "{{.DockerRootDir}}"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim(),
    )
    .join("volumes");
    for tier in [vercelify_core::StorageTier::Hdd, vercelify_core::StorageTier::Ssd] {
        let before = docker
            .list_managed()
            .await
            .unwrap()
            .into_iter()
            .filter(|c| {
                c.labels.get(vercelify_platform::deploy::LABEL_APP) == Some(&app.id)
                    && !c.labels.contains_key(vercelify_platform::deploy::LABEL_PR)
                    && c.state == "running"
            })
            .map(|c| c.name)
            .collect::<Vec<_>>();
        assert!(before.len() >= 2, "both replicas are running before the move");
        let moved = engine.move_app_volume(&vol.id, tier, &docker_root).await.expect("move");
        for container in before {
            assert!(
                !docker.inspect(&container).await.unwrap().is_some_and(|c| c.running),
                "every writer must stop before copying: {container}"
            );
        }
        assert_eq!(moved.tier, tier);
        let rec_d = store_.with(|c| deployments::list(c, &app.id, 1)).unwrap().remove(0);
        assert_eq!(rec_d.trigger, DeploymentTrigger::Recover);
        wait_for(&engine, &rec_d.id, DeploymentStatus::Healthy).await;
        let now_live = engine.live_container(&app.id).unwrap();
        assert_eq!(sh(&now_live, "cat /data/f"), "kept");
        assert_eq!(sh(&now_live, "stat -c %u /data/f"), owner, "owner kept");
        if tier == vercelify_core::StorageTier::Hdd {
            assert!(dir
                .path()
                .join("hdd")
                .join(vercelify_platform::volumes::physical_name(&vol, &app.name, None))
                .join("f")
                .exists());
        }
        body_of("web.test.localhost", "version two").await;
    }

    // 8. A Docker Compose app from a git repository over HTTPS.
    let repo = dir.path().join("git/shop.git");
    let work = dir.path().join("shop-src");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        work.join("docker-compose.yml"),
        "services:\n  web:\n    image: node:22-slim\n    command: [\"node\", \"-e\", \"require('http').createServer((q,s)=>s.end('compose-'+process.env.GREETING)).listen(8080)\"]\n  worker:\n    image: alpine:3.20\n    command: [\"sleep\", \"600\"]\n",
    )
    .unwrap();
    let sh = |cmd: &str, cwd: &std::path::Path| {
        let o = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(o.status.success(), "{cmd}: {}", String::from_utf8_lossy(&o.stderr));
    };
    sh("git init -q -b main . && git -c user.email=t@t -c user.name=t add . && git -c user.email=t@t -c user.name=t commit -qm init", &work);
    std::fs::create_dir_all(repo.parent().unwrap()).unwrap();
    sh(
        &format!(
            "git clone -q --bare . {} && git -C {} update-server-info",
            repo.display(),
            repo.display()
        ),
        &work,
    );
    let certs = dir.path().join("certs");
    std::fs::create_dir_all(&certs).unwrap();
    sh("openssl req -x509 -newkey rsa:2048 -nodes -keyout key.pem -out cert.pem -days 1 -subj /CN=127.0.0.1 -addext subjectAltName=IP:127.0.0.1 2>/dev/null", &certs);
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let server = std::process::Command::new("python3")
        .arg("-c")
        .arg(format!(
            "import http.server,ssl,functools\nh=functools.partial(http.server.SimpleHTTPRequestHandler,directory='{}')\ns=http.server.HTTPServer(('127.0.0.1',{port}),h)\nc=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);c.load_cert_chain('{}','{}')\ns.socket=c.wrap_socket(s.socket,server_side=True)\ns.serve_forever()",
            dir.path().join("git").display(),
            certs.join("cert.pem").display(),
            certs.join("key.pem").display()
        ))
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }
    let _server = Kill(server);
    std::env::set_var("GIT_SSL_CAINFO", certs.join("cert.pem"));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let shop = store_
        .tx(|c| {
            apps::create(
                c,
                &AppCreate {
                    name: "shop".into(),
                    kind: AppKind::Compose,
                    environment_id: None,
                    project_id: None,
                    source: AppSource {
                        repository: Some(format!("https://127.0.0.1:{port}/shop.git")),
                        branch: Some("main".into()),
                        ..AppSource::default()
                    },
                    build: None,
                    runtime: Some(AppRuntime {
                        port: Some(8080),
                        health_timeout_secs: 60,
                        ..AppRuntime::default()
                    }),
                    previews: None,
                    node_id: None,
                    domains: vec![],
                },
            )
        })
        .unwrap();
    store_
        .tx(|c| {
            env::put(
                c,
                &key,
                VarScope::App,
                &shop.id,
                &EnvVarsPut {
                    vars: vec![EnvVarInput {
                        key: "GREETING".into(),
                        value: "ok".into(),
                        build: None,
                        preview_only: None,
                        secret: None,
                    }],
                    replace: false,
                },
            )
        })
        .unwrap();
    let cd = engine.enqueue("shop", Enqueue::default()).unwrap();
    let cd = wait_for(&engine, &cd.id, DeploymentStatus::Healthy).await;
    assert_eq!(cd.commit_message, "init");
    body_of("shop.test.localhost", "compose-ok").await;
    let report = rec.once().await.unwrap();
    assert!(
        report.removed.is_empty(),
        "the compose worker is not an orphan: {report:?}"
    );

    // Deleting the app cleans up after it.
    let shop_app = store_.tx(|c| apps::delete(c, &shop.id)).unwrap();
    engine.cleanup_app(&shop_app, true, &[]).await;
    assert!(!docker
        .list_managed()
        .await
        .unwrap()
        .iter()
        .any(|c| c.name.starts_with("vfy-shop-")));

    // 9. A compose app without a repository (a catalogue service), its
    // variables and the app's own URL interpolated into the file.
    let inline = store_
        .tx(|c| {
            apps::create(
                c,
                &AppCreate {
                    name: "kv".into(),
                    kind: AppKind::Compose,
                    environment_id: None,
                    project_id: None,
                    source: AppSource {
                        compose_content: Some(
                            "services:\n  web:\n    image: node:22-slim\n    environment:\n      SELF: ${VERCELIFY_URL}\n      TOKEN: ${TOKEN}\n    command: [\"node\", \"-e\", \"require('http').createServer((q,s)=>s.end('inline '+process.env.SELF+' '+process.env.TOKEN)).listen(8080)\"]\n".into(),
                        ),
                        ..AppSource::default()
                    },
                    build: None,
                    runtime: Some(AppRuntime {
                        port: Some(8080),
                        health_timeout_secs: 60,
                        ..AppRuntime::default()
                    }),
                    previews: None,
                    node_id: None,
                    domains: vec![],
                },
            )
        })
        .unwrap();
    store_
        .tx(|c| {
            env::put(
                c,
                &key,
                VarScope::App,
                &inline.id,
                &EnvVarsPut {
                    vars: vec![EnvVarInput {
                        key: "TOKEN".into(),
                        value: "tok".into(),
                        build: None,
                        preview_only: None,
                        secret: Some(true),
                    }],
                    replace: false,
                },
            )
        })
        .unwrap();
    let d = engine.enqueue("kv", Enqueue::default()).unwrap();
    wait_for(&engine, &d.id, DeploymentStatus::Healthy).await;
    body_of("kv.test.localhost", "inline http://kv.test.localhost tok").await;
    let kv = store_.tx(|c| apps::delete(c, &inline.id)).unwrap();
    engine.cleanup_app(&kv, true, &[]).await;

    // Clean up.
    for c in docker.list_managed().await.unwrap() {
        let _ = docker.remove(&c.name).await;
    }
    let _ = docker.remove(proxy::CONTAINER).await;
    let _ = docker.remove_image(&built.image).await;
    let _ = docker.remove_image(&crash.image).await;
}

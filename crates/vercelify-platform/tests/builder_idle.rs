//! Disposable-daemon contract: host/tests/contract/builder-idle.sh.
use std::{sync::Arc, time::Duration};
use vercelify_core::platform::AppBuild;
use vercelify_platform::{
    build::{Builder, DirBuild, Driver, BUILDER, BUILDER_CONTAINER},
    deploy::Deployer,
    docker::Docker,
    engine::Engine,
    gate::BuildGate,
    logs::LogHub,
    proc::{self, Cancel},
    proxy::ProxyConfig,
    secrets::MasterKey,
    Store,
};

#[tokio::test]
#[ignore = "owns the dedicated builder on a disposable Docker daemon"]
async fn idle_builder_retains_cache_and_restarts_for_repeated_builds() {
    assert_eq!(std::env::var("VERCELIFY_BUILDER_CONTRACT").as_deref(), Ok("1"));
    let dir = tempfile::tempdir().unwrap();
    let docker = Docker::default();
    // This contract deliberately exercises creation as well as restarts.
    docker
        .run(&proc::args(&["buildx", "rm", BUILDER]), Duration::from_secs(60))
        .await
        .unwrap();
    std::fs::write(dir.path().join("payload"), "idle-contract\n").unwrap();
    std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\nCOPY payload /payload\n").unwrap();
    let logs = LogHub::new(dir.path().join("logs"));
    let engine = Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(MasterKey::from_bytes(&[1; 32]).unwrap()),
        Builder {
            docker: "docker".into(),
            git: "git".into(),
            work_dir: dir.path().join("work"),
            driver: Driver::Container,
            memory: None,
            timeout: Duration::from_secs(120),
        },
        Deployer {
            docker: docker.clone(),
            proxy: ProxyConfig {
                dir: dir.path().join("proxy"),
                http_port: 18080,
                https_port: 18443,
                metrics_port: 18082,
                acme_email: None,
                dns_challenge: false,
                image: vercelify_platform::proxy::TRAEFIK_IMAGE.into(),
            },
            hdd_root: dir.path().join("hdd"),
            cgroup_parent: None,
            health_interval: Duration::from_secs(1),
        },
        BuildGate::new(2, dir.path().join("power")),
        logs,
    );
    let idle = Duration::from_millis(150);
    let mut cache = None;
    for n in 0..3 {
        let permit = engine.gate.acquire(&mut |_| {}).await;
        let id = format!("dpl_cycle{n}");
        let mut log = engine.logs.writer("idle-contract", &id);
        engine
            .builder
            .build_dir(
                DirBuild {
                    dir: dir.path(),
                    app_name: "idle-contract",
                    deployment_id: &id,
                    build: &AppBuild::default(),
                    build_env: &[],
                },
                Cancel::never(),
                &mut log,
            )
            .await
            .unwrap();
        let state = docker.inspect(BUILDER_CONTAINER).await.unwrap().unwrap();
        assert!(state.running);
        let mounts = docker
            .run(
                &proc::args(&["inspect", "--format", "{{json .Mounts}}", BUILDER_CONTAINER]),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert!(mounts.success());
        if let Some(prior) = &cache {
            assert_eq!(&mounts.stdout, prior, "cache retained");
        } else {
            cache = Some(mounts.stdout);
        }
        if n > 0 {
            assert!(
                engine
                    .logs
                    .read("idle-contract", &id, None)
                    .iter()
                    .any(|l| l.text.contains("CACHED")),
                "warm cache used"
            );
        }
        tokio::time::sleep(idle * 2).await;
        assert!(
            !engine.stop_idle_builder(idle).await.unwrap(),
            "active slot prevents stop"
        );
        drop(permit);
        assert!(!engine.stop_idle_builder(idle).await.unwrap(), "fresh completion");
        tokio::time::sleep(idle * 2).await;
        if n == 1 {
            docker
                .run(&proc::args(&["pause", BUILDER_CONTAINER]), Duration::from_secs(10))
                .await
                .unwrap();
            assert!(!engine.stop_idle_builder(idle).await.unwrap());
            docker
                .run(&proc::args(&["unpause", BUILDER_CONTAINER]), Duration::from_secs(10))
                .await
                .unwrap();
        }
        let start = std::time::Instant::now();
        assert!(engine.stop_idle_builder(idle).await.unwrap());
        assert!(start.elapsed() < Duration::from_secs(60));
        assert!(!docker.inspect(BUILDER_CONTAINER).await.unwrap().unwrap().running);
        assert!(!engine.stop_idle_builder(idle).await.unwrap(), "already stopped");
        println!("cycle {n}: built, retained cache, stopped safely");
    }
    // A deleted container leaves buildx metadata behind. Recreate it without
    // losing the prior cache, and enforce the new bounded builder settings.
    docker.remove(BUILDER_CONTAINER).await.unwrap();
    let mut recovered = engine.builder.clone();
    recovered.memory = Some("1024m".into());
    let mut log = engine.logs.writer("idle-contract", "dpl_recreated");
    recovered
        .build_dir(
            DirBuild {
                dir: dir.path(),
                app_name: "idle-contract",
                deployment_id: "dpl_recreated",
                build: &AppBuild::default(),
                build_env: &[],
            },
            Cancel::never(),
            &mut log,
        )
        .await
        .unwrap();
    drop(log);
    let state = docker
        .run(
            &proc::args(&["inspect", "--format", "{{json .HostConfig}}", BUILDER_CONTAINER]),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    let limits: serde_json::Value = serde_json::from_str(state.stdout.trim()).unwrap();
    assert_eq!(limits["Memory"], 1024_u64 * 1024 * 1024);
    assert_eq!(limits["MemorySwap"], 1024_u64 * 1024 * 1024);
    assert_eq!(limits["CpuQuota"], 100000);
    let mounts = docker
        .run(
            &proc::args(&["inspect", "--format", "{{json .Mounts}}", BUILDER_CONTAINER]),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(
        Some(mounts.stdout),
        cache,
        "deleted-container recovery preserves the cache volume"
    );
    assert!(
        engine
            .logs
            .read("idle-contract", "dpl_recreated", None)
            .iter()
            .any(|line| line.text.contains("CACHED")),
        "recreated builder reuses retained cache"
    );
    println!("deleted builder recreated with bounded resources and retained warm cache");
}

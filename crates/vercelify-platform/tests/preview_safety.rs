//! Real engine/SQLite/Docker/Traefik qualification. Run only on a disposable
//! Docker daemon: this suite owns its proxy. host/tests/contract/preview-safety.sh.
use std::{path::Path, sync::Arc, time::Duration};
use vercelify_core::platform::*;
use vercelify_platform::{
    apps,
    build::{Builder, Driver},
    deploy::Deployer,
    deployments,
    docker::{ContainerSpec, Docker, PROXY_NETWORK},
    engine::{Engine, Enqueue},
    env,
    gate::BuildGate,
    logs::LogHub,
    preview_network, proc,
    proxy::{self, ProxyConfig},
    secrets::MasterKey,
    Store,
};

async fn done(e: &Engine, id: &str) -> Deployment {
    for _ in 0..240 {
        let d = e.store.with(|c| deployments::get(c, id)).unwrap();
        if d.status == DeploymentStatus::Healthy {
            return d;
        }
        assert!(
            !matches!(d.status, DeploymentStatus::Failed | DeploymentStatus::Cancelled),
            "{}: {:?}",
            d.error,
            e.logs.read("contract", id, Some(20))
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("deployment timed out");
}

fn app(e: &Engine, name: &str, kind: AppKind, source: AppSource, port: Option<u16>) -> App {
    e.store
        .tx(|c| {
            apps::create(
                c,
                &AppCreate {
                    name: name.into(),
                    kind,
                    source,
                    environment_id: None,
                    project_id: None,
                    build: None,
                    runtime: Some(AppRuntime {
                        port,
                        health_timeout_secs: 30,
                        drain_secs: 0,
                        ..Default::default()
                    }),
                    previews: None,
                    node_id: None,
                    domains: vec![],
                },
            )
        })
        .unwrap()
}

fn container_name(e: &Engine, id: &str) -> String {
    e.store.with(|c| vercelify_platform::instances::of(c, id)).unwrap()[0]
        .container
        .clone()
}

async fn request(d: &Docker, host: &str, source: &str, headers: &[&str]) -> String {
    let mut args = vec![
        "wget".into(),
        "-S".into(),
        "-O".into(),
        "-".into(),
        "-T".into(),
        "5".into(),
        "--header".into(),
        format!("Host: {host}"),
    ];
    for h in headers {
        args.extend(["--header".into(), (*h).into()]);
    }
    args.push(format!("http://{source}/"));
    let o = d
        .exec("vfy-contract-client", &args, Duration::from_secs(8))
        .await
        .unwrap();
    format!("{}{}", o.stdout, o.stderr)
}

async fn denied_tcp(d: &Docker, container: &str, host: &str, port: u16) {
    let js = "const s=require('net').connect(+process.argv[2],process.argv[1]);s.on('connect',()=>process.exit(9));s.on('error',()=>process.exit(0));s.setTimeout(1500,()=>process.exit(0));";
    let o = d
        .exec(
            container,
            &proc::args(&["node", "-e", js, host, &port.to_string()]),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert!(
        o.success(),
        "preview reached forbidden endpoint {host}:{port}: {}",
        o.tail(2)
    );
}

async fn engine(d: &Docker, root: &Path) -> Arc<Engine> {
    let cfg = ProxyConfig {
        dir: root.join("proxy"),
        http_port: 18080,
        https_port: 18443,
        metrics_port: 18082,
        acme_email: None,
        dns_challenge: false,
        image: proxy::TRAEFIK_IMAGE.into(),
    };
    proxy::ensure(d, &cfg, vec![]).await.unwrap();
    let s = Arc::new(Store::open(&root.join("state.db")).unwrap());
    s.tx(|c| vercelify_platform::nodes::ensure_main(c, "contract", "http://127.0.0.1:7420", std::env::consts::ARCH))
        .unwrap();
    let gateway = d
        .run(
            &proc::args(&[
                "network",
                "inspect",
                "--format",
                "{{(index .IPAM.Config 0).Gateway}}",
                PROXY_NETWORK,
            ]),
            Duration::from_secs(5),
        )
        .await
        .unwrap()
        .stdout
        .trim()
        .to_owned();
    s.with(|c| {
        apps::set_mesh_ingress(
            c,
            Some(&apps::MeshIngress {
                node: "contract".into(),
                cidrs: vec![format!("{gateway}/32")],
            }),
        )
    })
    .unwrap();
    let key = Arc::new(MasterKey::from_bytes(&[7; 32]).unwrap());
    let e = Engine::new(
        s,
        key,
        Builder {
            docker: "docker".into(),
            git: "git".into(),
            work_dir: root.join("builds"),
            driver: Driver::Docker,
            memory: None,
            timeout: Duration::from_secs(120),
        },
        Deployer {
            docker: d.clone(),
            proxy: cfg,
            hdd_root: root.join("hdd"),
            cgroup_parent: None,
            health_interval: Duration::from_millis(100),
        },
        BuildGate::new(1, root.join("power")),
        LogHub::new(root.join("logs")),
    );
    tokio::spawn(e.clone().run());
    e
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a disposable Docker daemon; run host/tests/contract/preview-safety.sh"]
async fn previews_have_no_production_credentials_network_or_public_bypass() {
    assert_eq!(std::env::var("VERCELIFY_PREVIEW_CONTRACT").as_deref(), Ok("1"));
    assert!(
        !Path::new("/etc/vercelify").exists(),
        "use a disposable test daemon/host"
    );
    let root = tempfile::tempdir().unwrap();
    let d = Docker::default();
    // This daemon is the suite's own: a rerun starts from its fixed fixtures
    // removed (a failed run leaves them).
    for fixture in ["vfy-contract-db", "vfy-contract-client"] {
        let _ = d.remove(fixture).await;
    }
    for image in ["traefik/whoami:v1.11", "node:22-slim", "alpine:3.22"] {
        d.pull(image).await.unwrap();
    }
    let e = engine(&d, root.path()).await;
    let image = app(
        &e,
        "expose-contract",
        AppKind::Image,
        AppSource {
            image: Some("traefik/whoami:v1.11".into()),
            ..Default::default()
        },
        None,
    );
    let production = e.enqueue(&image.id, Enqueue::default()).unwrap();
    done(&e, &production.id).await;
    assert_eq!(
        e.store.with(|c| deployments::built_info(c, &production.id)).unwrap().0,
        Some(80)
    );

    let server = "require('http').createServer((q,s)=>s.end('preview-ok')).listen(+process.env.PORT)";
    let mut source = AppSource {
        image: Some("node:22-slim".into()),
        ..Default::default()
    };
    let a = app(&e, "contract", AppKind::Image, source.clone(), Some(8080));
    e.store
        .tx(|c| {
            apps::patch(
                c,
                &a.id,
                &AppPatch {
                    runtime: Some(AppRuntime {
                        port: Some(8080),
                        command: Some(vec!["node".into(), "-e".into(), server.into()]),
                        health_timeout_secs: 30,
                        drain_secs: 0,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    e.store
        .tx(|c| {
            env::put(
                c,
                &e.key,
                VarScope::App,
                &a.id,
                &EnvVarsPut {
                    vars: vec![
                        EnvVarInput {
                            key: "PRODUCTION_TOKEN".into(),
                            value: "production-must-never-arrive".into(),
                            build: Some(true),
                            preview_only: None,
                            secret: Some(true),
                            share_with_previews: None,
                        },
                        EnvVarInput {
                            key: "PREVIEW_TOKEN".into(),
                            value: "preview-only-token".into(),
                            build: None,
                            preview_only: Some(true),
                            secret: Some(true),
                            share_with_previews: None,
                        },
                    ],
                    replace: false,
                },
            )
        })
        .unwrap();
    let preview = e
        .enqueue(
            &a.id,
            Enqueue {
                pr_number: Some(7),
                commit_sha: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let live = done(&e, &preview.id).await;
    let container = &container_name(&e, &live.id);
    let inspect = d
        .run(
            &proc::args(&["inspect", "--format", "{{json .Config.Env}}", container]),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert!(!inspect.stdout.contains("PRODUCTION_TOKEN"));
    assert!(inspect.stdout.contains("PREVIEW_TOKEN=preview-only-token"));
    let net = preview_network::name(&a.id, 7);
    let ips = d.inspect(container).await.unwrap().unwrap().ips;
    assert_eq!(ips.keys().collect::<Vec<_>>(), vec![&net]);
    let preview_ip = ips[&net].clone();

    // A production-side listener and a host management listener both answer
    // before probing from PR code. Their denial must not be a missing server.
    let prodnet = vercelify_platform::deploy::project_network(&a.project_id);
    d.ensure_network(&prodnet).await.unwrap();
    d.create(&ContainerSpec {
        name: "vfy-contract-db".into(),
        image: "node:22-slim".into(),
        networks: vec![prodnet],
        command: Some(vec![
            "node".into(),
            "-e".into(),
            "require('net').createServer(s=>s.end('db')).listen(5432)".into(),
        ]),
        ..Default::default()
    })
    .await
    .unwrap();
    d.start("vfy-contract-db").await.unwrap();
    let dbip = d
        .inspect("vfy-contract-db")
        .await
        .unwrap()
        .unwrap()
        .ips
        .into_values()
        .next()
        .unwrap();
    for _ in 0..50 {
        if vercelify_platform::health::tcp_open(&dbip, 5432, Duration::from_millis(100)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(vercelify_platform::health::tcp_open(&dbip, 5432, Duration::from_secs(5)).await);
    let host = tokio::net::TcpListener::bind("0.0.0.0:17420").await.unwrap();
    let gateway = d
        .run(
            &proc::args(&[
                "network",
                "inspect",
                "--format",
                "{{(index .IPAM.Config 0).Gateway}}",
                PROXY_NETWORK,
            ]),
            Duration::from_secs(5),
        )
        .await
        .unwrap()
        .stdout
        .trim()
        .to_owned();
    assert!(vercelify_platform::health::tcp_open(&gateway, 17420, Duration::from_secs(5)).await);
    denied_tcp(&d, container, &dbip, 5432).await;
    denied_tcp(&d, container, &gateway, 17420).await;
    denied_tcp(&d, container, &gateway, 7420).await;
    denied_tcp(&d, container, &gateway, 2375).await;
    drop(host);

    d.create(&ContainerSpec {
        name: "vfy-contract-client".into(),
        image: "alpine:3.22".into(),
        networks: vec![PROXY_NETWORK.into()],
        command: Some(proc::args(&["sleep", "600"])),
        ..Default::default()
    })
    .await
    .unwrap();
    d.start("vfy-contract-client").await.unwrap();
    let proxyip = d.inspect(proxy::CONTAINER).await.unwrap().unwrap().ips[PROXY_NETWORK].clone();
    let hostname = "pr-7-contract.contract.vfy";
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(request(&d, hostname, &proxyip, &[]).await.contains("403 Forbidden"));
    assert!(request(
        &d,
        hostname,
        &proxyip,
        &[
            &format!("X-Forwarded-For: {gateway}"),
            &format!("X-Real-IP: {gateway}"),
            &format!("CF-Connecting-IP: {gateway}")
        ]
    )
    .await
    .contains("403 Forbidden"));
    let allowed = proc::run(proc::Spec {
        bin: "curl",
        args: &proc::args(&[
            "-fsS",
            "--max-time",
            "5",
            "-H",
            &format!("Host: {hostname}"),
            "http://127.0.0.1:18080/",
        ]),
        env: &[],
        cwd: None,
        timeout: Duration::from_secs(7),
    })
    .await
    .unwrap();
    assert!(
        allowed.success() && allowed.stdout.contains("preview-ok"),
        "{}",
        allowed.tail(3)
    );
    // An unrelated bridge cannot address the preview container directly.
    let direct = d
        .exec(
            "vfy-contract-client",
            &proc::args(&["wget", "-T", "2", "-O", "-", &format!("http://{preview_ip}:8080/")]),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert!(!direct.success());

    d.remove(proxy::CONTAINER).await.unwrap();
    proxy::ensure(&d, &e.deployer.proxy, vec![]).await.unwrap();
    assert!(d
        .inspect(proxy::CONTAINER)
        .await
        .unwrap()
        .unwrap()
        .ips
        .contains_key(&net));

    // Compose's requested external production network must be replaced,
    // including for unrouted sidecars, rather than merged into the override.
    source.image = None;
    source.compose_content=Some(format!("services:\n  web:\n    image: node:22-slim\n    command: [node, -e, \"{server}\"]\n    networks: [production]\nnetworks:\n  production:\n    external: true\n    name: vfy-proxy\n"));
    let compose = app(&e, "compose-contract", AppKind::Compose, source, Some(8080));
    let cp = e
        .enqueue(
            &compose.id,
            Enqueue {
                pr_number: Some(9),
                commit_sha: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let cp = done(&e, &cp.id).await;
    let cpnet = preview_network::name(&compose.id, 9);
    assert_eq!(
        d.inspect(&container_name(&e, &cp.id))
            .await
            .unwrap()
            .unwrap()
            .ips
            .keys()
            .collect::<Vec<_>>(),
        vec![&cpnet]
    );
    e.remove_preview(&compose.id, 9).await.unwrap();
    e.remove_preview(&a.id, 7).await.unwrap();
    e.remove_preview(&a.id, 7).await.unwrap();
    assert!(d.inspect(container).await.unwrap().is_none());
    let missing = d
        .run(&proc::args(&["network", "inspect", &net]), Duration::from_secs(5))
        .await
        .unwrap();
    assert!(!missing.success());
    assert!(
        d.inspect(&container_name(&e, &production.id))
            .await
            .unwrap()
            .unwrap()
            .running
    );
    println!("EXPOSE, preview credentials, production DB/management denial, direct IP/forwarded-header ingress denial, proxy recreation, Compose isolation and idempotent cleanup passed");
}

//! vercelify-agent — runs on the Linux host (and optional Raspberry Pi
//! workers) and exposes host state to the cockpit over Tailscale.

mod auth;
mod autodeploy;
mod cli;
mod config;
mod coolify;
mod cron;
mod display;
mod docker;
mod doctor;
mod error;
mod events;
mod exec;
mod github;
mod github_app;
mod http;
mod json;
mod llm;
mod mdns;
mod mesh;
mod mesh_relay;
mod pairing;
mod platform;
mod platform_api;
mod platform_git;
mod powerguard;
mod project;
mod projects;
mod routes;
mod routing;
mod runtime_api;
mod services;
mod sleep;
mod ssh;
mod standby;
mod tailscale;
mod terminal;
mod tokens;
mod update;
mod volumes;
mod watch;
mod webhooks;
mod worker;

use cli::Cmd;
use config::Config;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// RUST_LOG's `target=level` directives (`info,tower_http=warn`), or the default.
fn log_filter(default: &str) -> tracing_subscriber::filter::Targets {
    std::env::var("RUST_LOG")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| default.parse().expect("valid directives"))
}

/// A pairing for the Mac app on Vercelify's mesh (design §13.3): a device
/// join token, and the agent at the server's mesh address.
fn mesh_pairing(cfg: &Config, token: &str, ttl_secs: u64) -> Result<vercelify_core::pairing::Pairing, String> {
    let own = vercelify_mesh::state::State::load(&cfg.mesh_dir)
        .map_err(|e| e.to_string())?
        .ok_or("the mesh isn't set up on this server yet: sudo vercelify mesh init (or pair with --tailscale)")?;
    let key = vercelify_mesh::keys::NodeKey::load_or_create(&vercelify_mesh::state::key_path(&cfg.mesh_dir))
        .map_err(|e| e.to_string())?;
    let store = vercelify_platform::Store::open(&cfg.platform_db).map_err(|e| e.to_string())?;
    // The join token lives as long as the short code, and at least a day
    // for the full code (it works once either way).
    let ttl_minutes = ((ttl_secs / 60) as u32).max(24 * 60);
    let invite = mesh::make_invite(
        cfg,
        &store,
        &key.public(),
        &vercelify_core::platform::MeshInviteRequest {
            kind: Some("device".into()),
            role: Some("owner".into()),
            ttl_minutes: Some(ttl_minutes),
        },
    )
    .map_err(|e| e.message)?;
    Ok(vercelify_core::pairing::Pairing {
        agent_url: format!("http://{}:{}", own.ipv4, cfg.port),
        agent_token: token.to_string(),
        coolify_url: if cfg.platform_engine() {
            String::new()
        } else {
            format!("http://{}:8000", own.ipv4)
        },
        coolify_token: std::fs::read_to_string(&cfg.coolify_token_file)
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && !cfg.platform_engine()),
        hostname: own.name,
        role: Some(cfg.role()),
        mesh_join: Some(invite.token),
    })
}

fn random_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS RNG available");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::main]
async fn main() {
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_target(false)
            .finish()
            .with(log_filter("info,tower_http=warn"))
            .init();
    }

    let cli = match cli::parse(std::env::args().skip(1), std::env::var("VERCELIFY_CONFIG").ok()) {
        Ok(c) => c,
        Err(e) => fatal(&e),
    };
    match cli.cmd {
        Cmd::Version => {
            println!("vercelify-agent {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Cmd::Help => {
            println!("vercelify-agent {}\n{}", env!("CARGO_PKG_VERSION"), cli::USAGE);
            return;
        }
        _ => {}
    }
    let cfg = match Config::load(Some(&cli.config)) {
        Ok(c) => c,
        Err(e) => fatal(&e.to_string()),
    };

    match cli.cmd {
        Cmd::Version | Cmd::Help => {}
        Cmd::Serve => serve(cfg).await,
        Cmd::Token { write, force } => {
            let token = random_token();
            if write {
                if cfg.token_file.exists() && !force {
                    fatal(&format!("{} exists (use --force to replace)", cfg.token_file.display()));
                }
                if let Some(dir) = cfg.token_file.parent() {
                    std::fs::create_dir_all(dir).unwrap_or_else(|e| fatal(&e.to_string()));
                }
                write_private(&cfg.token_file, &token).unwrap_or_else(|e| fatal(&e.to_string()));
                eprintln!("wrote {}", cfg.token_file.display());
            } else {
                println!("{token}");
            }
        }
        Cmd::Pair {
            host,
            coolify_port,
            coolify_url,
            coolify_token_file,
            short,
            with_code,
            ttl,
            name,
            tailscale,
        } => {
            // Each pairing gets its own token, so a lost Mac can be revoked
            // alone (`vercelify tokens revoke`).
            let name = name.unwrap_or_else(|| format!("paired {}", chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")));
            let token = match tokens::create(&cfg, &name) {
                Ok((_, secret)) => secret,
                Err(e) => fatal(&format!(
                    "could not write {}: {e} (run as root)",
                    cfg.tokens_file.display()
                )),
            };
            let mesh_pairing = if cfg.mesh_coordinator && cfg.role() != "worker" && !tailscale {
                Some(mesh_pairing(&cfg, &token, ttl).unwrap_or_else(|e| fatal(&e)))
            } else {
                None
            };
            let ts = tailscale::status(cfg.tailscale_bin.as_deref()).await;
            let host = host
                .or_else(|| {
                    ts.self_node
                        .as_ref()
                        .map(|n| n.dns_name.clone())
                        .filter(|s| !s.is_empty())
                })
                .or_else(|| vercelify_core::tailscale::self_ipv4(&ts))
                .or_else(|| {
                    let lan = lan_ipv4()?;
                    eprintln!(
                        "vercelify-agent: Tailscale is not connected — this code uses the LAN address {lan}, \
                         which works only if agent.toml also listens there (e.g. listen = [\"tailscale\", \"127.0.0.1\", \"{lan}\"]). \
                         Recommended: `sudo tailscale up`, then `sudo vercelify pair` again."
                    );
                    Some(lan)
                })
                .unwrap_or_else(|| fatal("no network address found; pass --host"));
            let hostname = ts
                .self_node
                .as_ref()
                .map(|n| n.hostname.clone())
                .unwrap_or_else(|| host.clone());
            let role = cfg.role();
            let worker = role == "worker";
            // Servers on the platform engine have no Coolify to hand out.
            let no_coolify = worker || cfg.platform_engine();
            let token_file = coolify_token_file.unwrap_or_else(|| cfg.coolify_token_file.clone());
            let pairing = vercelify_core::pairing::Pairing {
                agent_url: format!("http://{host}:{}", cfg.port),
                agent_token: token,
                // Workers run no Coolify: the main host deploys to them.
                coolify_url: match (coolify_url, no_coolify) {
                    (Some(u), _) => u,
                    (None, true) => String::new(),
                    (None, false) => format!("http://{host}:{coolify_port}"),
                },
                coolify_token: std::fs::read_to_string(&token_file)
                    .ok()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty() && !no_coolify),
                hostname,
                role: Some(role),
                mesh_join: None,
            };
            let pairing = mesh_pairing.unwrap_or(pairing);
            let code = vercelify_core::pairing::encode(&pairing);
            if short {
                match pairing::create(&cfg, &code, Some(ttl)) {
                    Ok(f) if with_code => println!("{}\n{code}", f.code),
                    Ok(f) => println!("{}", f.code),
                    Err(e) => fatal(&format!(
                        "could not write {}: {e} (run as root)",
                        pairing::code_file(&cfg).display()
                    )),
                }
            } else {
                println!("{code}");
            }
        }
        Cmd::Promote { force } => match standby::promote(&cfg, force).await {
            Ok(m) => println!("{m}"),
            Err(e) => fatal(&e),
        },
        Cmd::TokensList => {
            let all = tokens::list(&cfg);
            if all.is_empty() {
                println!("no device tokens (pair a device with: sudo vercelify pair)");
            }
            for t in all {
                println!("{}", t.line());
            }
        }
        Cmd::TokensRevoke { id } => match tokens::revoke(&cfg, &id) {
            Ok(true) => eprintln!("revoked {id}: that device can no longer connect"),
            Ok(false) => fatal(&format!("no token {id} (see: vercelify tokens)")),
            Err(e) => fatal(&e.to_string()),
        },
    }
}

/// The local IPv4 used for outbound traffic (no packet is sent: connecting a
/// UDP socket only selects a route).
/// Every LAN IPv4 of this machine (join tokens list them all).
pub fn lan_ipv4s() -> Vec<std::net::Ipv4Addr> {
    vercelify_mesh::net::local_ipv4s(Some(std::net::Ipv4Addr::new(10, 77, 0, 0)))
}

fn lan_ipv4() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.0.2.1:9").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip.to_string()),
        _ => None,
    }
}

fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    f.write_all(b"\n")
}

fn fatal(msg: &str) -> ! {
    eprintln!("vercelify-agent: {msg}");
    std::process::exit(1)
}

async fn serve(cfg: Config) {
    let token = cfg.resolve_token().unwrap_or_else(|e| fatal(&e.to_string()));
    let port = cfg.port;
    let listen = cfg.listen.clone();
    let tailscale_bin = cfg.tailscale_bin.clone();
    let state = routes::AppState::new(cfg, token);
    let app = routes::router(state.clone());
    tokio::spawn(state.autodeploy.clone().run());
    tokio::spawn(state.projects.clone().run());
    tokio::spawn(state.projects.clone().run_crons());
    tokio::spawn(state.watcher.clone().run(state.clone()));
    tokio::spawn(update::resume_update_job(state.config.clone(), state.jobs.clone()));
    if let Some(p) = &state.platform {
        // Databases' public ports listen on the private network only.
        let private = match mesh::own_ipv4(&state.config) {
            Some(ip) => Some(ip.to_string()),
            None => tailscale::ipv4(state.config.tailscale_bin.as_deref()).await,
        };
        p.engine.set_private_ip(private);
        // Tests run with the background loops off (watch interval 0).
        let every = if state.config.watch_interval_secs == 0 { 0 } else { 30 };
        p.spawn(Duration::from_secs(every));
        if every > 0 {
            tokio::spawn(platform_api::run_tasks(p.clone(), state.config.clone()));
            let terminals = state.terminals.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    terminals.reap();
                }
            });
        }
    }
    if let Some(w) = &state.worker {
        if state.config.watch_interval_secs > 0 {
            w.spawn(state.config.clone(), state.collector.clone());
        }
    }
    if let Some(p) = &state.platform {
        if state.config.watch_interval_secs > 0 {
            tokio::spawn(platform_api::run_fleet(
                p.clone(),
                state.config.clone(),
                state.collector.clone(),
            ));
            tokio::spawn(standby::run(p.clone(), state.config.clone()));
        }
    }
    if let Some(co) = &state.mesh {
        if state.config.watch_interval_secs > 0 {
            tokio::spawn(standby::watch_replaced(state.config.clone(), co.clone()));
        }
    }
    if let Some(g) = &state.platform_git {
        tokio::spawn(g.clone().run());
    }

    let mut tasks = tokio::task::JoinSet::new();
    // Sleeping apps: Coolify's proxy forwards their requests here.
    let wake = state.config.wake_listen.trim().to_string();
    if !wake.is_empty() && state.config.role() != "worker" {
        match wake.parse::<SocketAddr>() {
            Ok(addr) => {
                let router = sleep::wake_router(state.sleep.clone());
                tasks.spawn(async move {
                    match tokio::net::TcpListener::bind(addr).await {
                        Ok(l) => {
                            tracing::info!("wake listener on http://{addr}");
                            if let Err(e) = axum::serve(l, router).with_graceful_shutdown(shutdown()).await {
                                tracing::error!("wake listener failed: {e}");
                            }
                        }
                        Err(e) => tracing::warn!("wake listener: bind {addr} failed: {e}; sleeping apps won't wake"),
                    }
                });
            }
            Err(_) => tracing::warn!("invalid wake_listen `{wake}`; sleeping apps won't wake"),
        }
    }
    // Mesh enrollment: sealed requests from machines joining.
    if let Some(co) = state.mesh.clone() {
        match state.config.mesh_control_listen.parse::<SocketAddr>() {
            Ok(addr) => {
                tasks.spawn(mdns::respond(state.config.clone(), co.clone()));
                // Git providers' webhooks and the GitHub App flow come here too.
                let router = mesh::control_router(co, state.claims.clone()).merge(webhooks::router(state.clone()));
                tasks.spawn(async move {
                    let listener = loop {
                        match tokio::net::TcpListener::bind(addr).await {
                            Ok(l) => break l,
                            Err(e) => {
                                tracing::warn!("mesh control: bind {addr} failed: {e}; retrying in 5s");
                                tokio::time::sleep(Duration::from_secs(5)).await;
                            }
                        }
                    };
                    tracing::info!("mesh enrollment on http://{addr}");
                    let svc = router.into_make_service_with_connect_info::<SocketAddr>();
                    if let Err(e) = axum::serve(listener, svc).with_graceful_shutdown(shutdown()).await {
                        tracing::error!("mesh control listener failed: {e}");
                    }
                });
            }
            Err(_) => tracing::warn!("invalid mesh_control_listen `{}`", state.config.mesh_control_listen),
        }
    }
    for spec in listen {
        let app = app.clone();
        let ts_bin = tailscale_bin.clone();
        let cfg = state.config.clone();
        tasks.spawn(async move {
            let ip = resolve_listen(&spec, ts_bin.as_deref(), &cfg).await;
            let addr = SocketAddr::new(ip, port);
            // tailscaled may hold the address briefly after (re)start; retry.
            let listener = loop {
                match tokio::net::TcpListener::bind(addr).await {
                    Ok(l) => break l,
                    Err(e) => {
                        tracing::warn!("bind {addr} failed: {e}; retrying in 5s");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            };
            tracing::info!("listening on http://{addr}");
            // Peer addresses: mesh nodes are accepted from their own mesh address.
            let svc = app.into_make_service_with_connect_info::<SocketAddr>();
            if let Err(e) = axum::serve(listener, svc).with_graceful_shutdown(shutdown()).await {
                tracing::error!("server on {addr} failed: {e}");
            }
        });
    }

    shutdown().await;
    tracing::info!("shutting down");
    state.receiver.stop().await;
    // Let in-flight requests finish briefly.
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
}

async fn resolve_listen(spec: &str, ts_bin: Option<&str>, cfg: &config::Config) -> IpAddr {
    if spec == "mesh" {
        // The mesh address, once this node has joined (the bind retries
        // until vercelify-mesh has put it on vfy0).
        let mut warned = false;
        loop {
            if let Some(ip) = mesh::own_ipv4(cfg) {
                return ip;
            }
            if !warned {
                tracing::warn!("waiting to join the mesh (sudo vercelify mesh join …)");
                warned = true;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
    if spec != "tailscale" {
        return spec
            .parse()
            .unwrap_or_else(|_| fatal(&format!("invalid listen address `{spec}`")));
    }
    let mut warned = false;
    loop {
        if let Some(ip) = tailscale::ipv4(ts_bin).await.and_then(|s| s.parse().ok()) {
            return ip;
        }
        if !warned {
            tracing::warn!("waiting for a Tailscale IPv4 address (is `tailscale up` done?)");
            warned = true;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}

//! vercelify-agent — runs on the Linux host (and optional Raspberry Pi
//! workers) and exposes host state to the cockpit over Tailscale.

mod auth;
mod autodeploy;
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
mod http;
mod llm;
mod pairing;
mod powerguard;
mod project;
mod projects;
mod routes;
mod routing;
mod services;
mod sleep;
mod ssh;
mod tailscale;
mod tokens;
mod update;
mod volumes;
mod watch;

use clap::{Parser, Subcommand};
use config::Config;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "vercelify-agent", version, about)]
struct Cli {
    /// Config file
    #[arg(long, short, env = "VERCELIFY_CONFIG", default_value = "/etc/vercelify/agent.toml")]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the HTTP API (default)
    Serve,
    /// Generate a random token; with --write, store it in the configured token file
    Token {
        #[arg(long)]
        write: bool,
        /// Overwrite an existing token file
        #[arg(long)]
        force: bool,
    },
    /// Print a pairing code for the cockpit
    Pair {
        /// Host name or IP the Mac should use (defaults to MagicDNS name)
        #[arg(long)]
        host: Option<String>,
        /// Coolify dashboard port
        #[arg(long, default_value_t = 8000)]
        coolify_port: u16,
        /// Coolify URL (overrides host + port, e.g. https://coolify.example.com)
        #[arg(long)]
        coolify_url: Option<String>,
        /// File containing a Coolify API token to embed (default: from agent.toml)
        #[arg(long)]
        coolify_token_file: Option<PathBuf>,
        /// Also create a one-time short code (printed instead of the full code)
        #[arg(long)]
        short: bool,
        /// With --short: print the full code on a second line too. Both use
        /// the same device token (two calls would mint two).
        #[arg(long, requires = "short")]
        with_code: bool,
        /// Lifetime of the short code in seconds
        #[arg(long, default_value_t = vercelify_core::pairing::SHORT_CODE_TTL_SECS)]
        ttl: u64,
        /// Name of the device token minted for this pairing
        #[arg(long)]
        name: Option<String>,
    },
    /// Device tokens: list, or revoke one so that device can no longer connect
    Tokens {
        #[command(subcommand)]
        cmd: Option<TokensCmd>,
    },
}

#[derive(Subcommand)]
enum TokensCmd {
    List,
    Revoke { id: String },
}

fn random_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS RNG available");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=warn".into()))
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let cfg = match Config::load(Some(&cli.config)) {
        Ok(c) => c,
        Err(e) => fatal(&e.to_string()),
    };

    match cli.cmd.unwrap_or(Cmd::Serve) {
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
            let token_file = coolify_token_file.unwrap_or_else(|| cfg.coolify_token_file.clone());
            let pairing = vercelify_core::pairing::Pairing {
                agent_url: format!("http://{host}:{}", cfg.port),
                agent_token: token,
                // Workers run no Coolify: the main host deploys to them.
                coolify_url: match (coolify_url, worker) {
                    (Some(u), _) => u,
                    (None, true) => String::new(),
                    (None, false) => format!("http://{host}:{coolify_port}"),
                },
                coolify_token: std::fs::read_to_string(&token_file)
                    .ok()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty() && !worker),
                hostname,
                role: Some(role),
            };
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
        Cmd::Tokens { cmd } => match cmd.unwrap_or(TokensCmd::List) {
            TokensCmd::List => {
                let all = tokens::list(&cfg);
                if all.is_empty() {
                    println!("no device tokens (pair a device with: sudo vercelify pair)");
                }
                for t in all {
                    println!("{}", t.line());
                }
            }
            TokensCmd::Revoke { id } => match tokens::revoke(&cfg, &id) {
                Ok(true) => eprintln!("revoked {id}: that device can no longer connect"),
                Ok(false) => fatal(&format!("no token {id} (see: vercelify tokens)")),
                Err(e) => fatal(&e.to_string()),
            },
        },
    }
}

/// The local IPv4 used for outbound traffic (no packet is sent: connecting a
/// UDP socket only selects a route).
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
    for spec in listen {
        let app = app.clone();
        let ts_bin = tailscale_bin.clone();
        tasks.spawn(async move {
            let ip = resolve_listen(&spec, ts_bin.as_deref()).await;
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
            if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown()).await {
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

async fn resolve_listen(spec: &str, ts_bin: Option<&str>) -> IpAddr {
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

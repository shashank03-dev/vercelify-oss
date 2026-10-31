//! `vercelify-mesh`: Vercelify's private network daemon (design §13).
//!
//!   vercelify-mesh join TOKEN [--name NAME]   join the mesh a token points at
//!   vercelify-mesh up                         run the node on the mesh (systemd)
//!   vercelify-mesh up --userspace --socks …   app mode, no root (the Mac app)
//!   vercelify-mesh status [--json]            this node and its peers
//!   vercelify-mesh rotate                     new key and certificate now
//!   vercelify-mesh pubkey                     this node's WireGuard public key
//!   vercelify-mesh discover                   coordinators on the LAN (mDNS)
//!   vercelify-mesh pair CODE [--join]         trade a short code for the pairing code

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use vercelify_mesh::daemon::{self, UpOptions};
use vercelify_mesh::keys::NodeKey;
use vercelify_mesh::pair_client;
use vercelify_mesh::proto::DEFAULT_WG_PORT;
use vercelify_mesh::state::key_path;

#[derive(Parser)]
#[command(name = "vercelify-mesh", version, about = "Vercelify's private network daemon")]
struct Cli {
    /// Keys and state.
    #[arg(
        long,
        global = true,
        default_value = "/var/lib/vercelify/mesh",
        env = "VERCELIFY_MESH_DIR"
    )]
    dir: PathBuf,
    /// Status, ACL and pid files.
    #[arg(long, global = true, default_value = "/run/vercelify", env = "VERCELIFY_RUN_DIR")]
    run_dir: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Join the mesh a `vfyj1.` token points at.
    Join {
        token: String,
        /// This node's name on the mesh (default: the hostname).
        #[arg(long)]
        name: Option<String>,
        /// UDP port peers reach this node on (announced with its addresses).
        #[arg(long, default_value_t = DEFAULT_WG_PORT)]
        port: u16,
    },
    /// Run the node on the mesh.
    Up {
        /// The tun interface (Linux; macOS picks a utunN).
        #[arg(long, default_value = "vfy0")]
        interface: String,
        /// App mode: no tun device and no root; reach the mesh through
        /// --socks and --forward (the Mac app).
        #[arg(long)]
        userspace: bool,
        /// With --userspace: a SOCKS5 proxy to the mesh (`127.0.0.1:7480`).
        #[arg(long, requires = "userspace")]
        socks: Option<std::net::SocketAddr>,
        /// With --userspace: private apps in the browser at
        /// `http://<app address>.localhost:PORT/` (`127.0.0.1:7481`).
        #[arg(long, requires = "userspace")]
        gateway: Option<std::net::SocketAddr>,
        /// With --userspace: `[udp:]127.0.0.1:PORT=HOST:PORT`, repeatable.
        #[arg(long = "forward", requires = "userspace")]
        forwards: Vec<vercelify_mesh::socks::Forward>,
        #[arg(long, default_value_t = DEFAULT_WG_PORT)]
        port: u16,
        /// Don't answer `*.vfy` names on the mesh address.
        #[arg(long)]
        no_dns: bool,
        /// Run after the ACL file changes.
        #[arg(long, env = "VERCELIFY_MESH_ACL_HOOK")]
        acl_hook: Option<String>,
        /// STUN servers for the public address, comma-separated ("" for none;
        /// peers on the mesh tell each other too).
        #[arg(
            long,
            env = "VERCELIFY_MESH_STUN",
            default_value = "stun.cloudflare.com:3478,stun.l.google.com:19302"
        )]
        stun: String,
        /// Where to look for the main server's signed address after losing it
        /// ("" for nowhere).
        #[arg(long, env = "VERCELIFY_MESH_BEACON", default_value = vercelify_mesh::beacon::DEFAULT_BEACON_URL)]
        beacon: String,
    },
    /// This node and its peers.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Rotate this node's key and certificate now.
    Rotate,
    /// Print this node's WireGuard public key (creating the key).
    Pubkey,
    /// Find coordinators on the LAN (mDNS).
    Discover {
        #[arg(long)]
        json: bool,
        /// Seconds to listen for answers.
        #[arg(long, default_value_t = 2)]
        wait: u64,
    },
    /// Trade a short pairing code (`sudo vercelify pair` on the server) for
    /// the full code; prints it.
    Pair {
        code: String,
        /// The server (`192.168.1.20`); default: found on the LAN.
        #[arg(long)]
        server: Option<String>,
        /// Also join the mesh with the code's join token.
        #[arg(long)]
        join: bool,
        /// With --join: this device's name on the mesh (default: the hostname).
        #[arg(long)]
        name: Option<String>,
        /// With --join: the UDP port to announce (0: `up` picks and reports one).
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

/// The `mesh_join` of a `vfy2.` pairing code.
fn mesh_join_of(code: &str) -> Option<String> {
    use base64::Engine;
    let body = code.trim().strip_prefix("vfy2.")?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(body).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    v["mesh_join"].as_str().map(String::from)
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "node".into())
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("vercelify-mesh: {msg}");
    std::process::exit(1)
}

fn ago(secs: Option<u64>) -> String {
    match secs {
        None => "never".into(),
        Some(s) if s < 120 => format!("{s}s ago"),
        Some(s) if s < 7200 => format!("{}m ago", s / 60),
        Some(s) => format!("{}h ago", s / 3600),
    }
}

fn status(run_dir: &std::path::Path, json: bool) {
    let path = run_dir.join("mesh-status.json");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|_| fail(format!("not running ({} missing)", path.display())));
    if json {
        println!("{raw}");
        return;
    }
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|e| fail(e));
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    println!("{} {} ({}) on {}", s("name"), s("ipv4"), s("ipv6"), s("interface"));
    if v["revoked"].as_bool() == Some(true) {
        println!("  REVOKED: the coordinator refused this node");
    }
    println!(
        "  map {} · coordinator {}",
        v["map_version"].as_u64().map(|n| n.to_string()).unwrap_or("-".into()),
        ago(v["coordinator_seen_secs"].as_u64())
    );
    for p in v["peers"].as_array().cloned().unwrap_or_default() {
        println!(
            "  {:<16} {:<15} {:<7} {:<22} handshake {}",
            p["name"].as_str().unwrap_or(""),
            p["addresses"][0].as_str().unwrap_or(""),
            p["path"].as_str().unwrap_or(""),
            p["endpoint"].as_str().unwrap_or("-"),
            ago(p["handshake_secs"].as_u64())
        );
    }
}

fn main() {
    let cli = Cli::parse();
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    // RUST_LOG takes `target=level` directives (`vercelify_mesh=debug`).
    let filter = std::env::var("RUST_LOG")
        .ok()
        .and_then(|v| v.parse::<tracing_subscriber::filter::Targets>().ok())
        .unwrap_or_else(|| "vercelify_mesh=info".parse().expect("valid directives"));
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_target(false)
        .finish()
        .with(filter)
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| fail(e));
    match cli.cmd {
        Cmd::Join { token, name, port } => {
            let name = name.unwrap_or_else(hostname);
            // `--port 0` (a device on a random port): `up` reports its addresses.
            let local: Vec<String> = vercelify_mesh::net::local_ipv4s(None)
                .into_iter()
                .filter(|_| port != 0)
                .map(|ip| format!("{ip}:{port}"))
                .collect();
            match rt.block_on(daemon::join(&cli.dir, &token, &name, local)) {
                Ok(s) => println!("joined as {} ({}, {})", s.name, s.ipv4, s.node_id),
                Err(e) => fail(e),
            }
        }
        Cmd::Up {
            interface,
            userspace,
            socks,
            gateway,
            forwards,
            port,
            no_dns,
            acl_hook,
            stun,
            beacon,
        } => {
            let opts = UpOptions {
                dir: cli.dir,
                run_dir: cli.run_dir,
                interface,
                port,
                dns: !no_dns,
                acl_hook,
                stun_servers: stun
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect(),
                userspace: userspace.then_some(daemon::Userspace {
                    socks,
                    gateway,
                    forwards,
                }),
                beacon: Some(beacon.trim().trim_end_matches('/').to_string()).filter(|b| !b.is_empty()),
            };
            if let Err(e) = rt.block_on(daemon::up(opts)) {
                fail(e)
            }
        }
        Cmd::Status { json } => status(&cli.run_dir, json),
        Cmd::Rotate => {
            let pid = std::fs::read_to_string(cli.run_dir.join("mesh.pid"))
                .ok()
                .and_then(|p| p.trim().parse::<i32>().ok())
                .unwrap_or_else(|| fail("the daemon isn't running"));
            if unsafe { libc::kill(pid, libc::SIGUSR1) } != 0 {
                fail(std::io::Error::last_os_error());
            }
            println!("asked the daemon to rotate its key (see: vercelify-mesh status)");
        }
        Cmd::Pubkey => match NodeKey::load_or_create(&key_path(&cli.dir)) {
            Ok(k) => println!("{}", k.public()),
            Err(e) => fail(e),
        },
        Cmd::Discover { json, wait } => {
            let found = rt
                .block_on(pair_client::discover(std::time::Duration::from_secs(wait.clamp(1, 30))))
                .unwrap_or_else(|e| fail(e));
            if json {
                let v: Vec<serde_json::Value> = found
                    .iter()
                    .map(|f| {
                        serde_json::json!({
                            "name": f.announcement.name, "control": f.control(),
                            "coordinator_wg": f.announcement.coordinator_wg,
                            "pairing_open": f.announcement.pairing_open,
                        })
                    })
                    .collect();
                println!("{}", serde_json::Value::Array(v));
            } else if found.is_empty() {
                fail("no Vercelify server answered on this network");
            } else {
                for f in &found {
                    println!(
                        "{:<20} {:<24} {}",
                        f.announcement.name,
                        f.control().first().cloned().unwrap_or_default(),
                        if f.announcement.pairing_open {
                            "pairing code waiting"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        Cmd::Pair {
            code,
            server,
            join,
            name,
            port,
        } => {
            let control = match server {
                Some(s) => vec![pair_client::control_of(&s)],
                None => {
                    let found = rt
                        .block_on(pair_client::discover(std::time::Duration::from_secs(2)))
                        .unwrap_or_else(|e| fail(e));
                    // The one waiting for a code, if several answer.
                    let pick = found
                        .iter()
                        .find(|f| f.announcement.pairing_open)
                        .or(found.first())
                        .unwrap_or_else(|| fail("no Vercelify server answered on this network; pass --server"));
                    pick.control()
                }
            };
            let full = rt
                .block_on(pair_client::claim(&control, &code))
                .unwrap_or_else(|e| fail(e));
            println!("{full}");
            if join {
                let token = mesh_join_of(&full).unwrap_or_else(|| fail("that server isn't on the mesh (a vfy1. code)"));
                let name = name.unwrap_or_else(hostname);
                let local: Vec<String> = vercelify_mesh::net::local_ipv4s(None)
                    .into_iter()
                    .filter(|_| port != 0)
                    .map(|ip| format!("{ip}:{port}"))
                    .collect();
                match rt.block_on(daemon::join(&cli.dir, &token, &name, local)) {
                    Ok(s) => eprintln!("joined as {} ({}, {})", s.name, s.ipv4, s.node_id),
                    Err(e) => fail(e),
                }
            }
        }
    }
}

//! `vercelify-mesh`: Vercelify's private network daemon (design §13).
//!
//!   vercelify-mesh join TOKEN [--name NAME]   join the mesh a token points at
//!   vercelify-mesh up                         run the node on the mesh (systemd)
//!   vercelify-mesh status [--json]            this node and its peers
//!   vercelify-mesh rotate                     new key and certificate now
//!   vercelify-mesh pubkey                     this node's WireGuard public key

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use vercelify_mesh::daemon::{self, UpOptions};
use vercelify_mesh::keys::NodeKey;
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
        #[arg(long, default_value = "vfy0")]
        interface: String,
        #[arg(long, default_value_t = DEFAULT_WG_PORT)]
        port: u16,
        /// Don't answer `*.vfy` names on the mesh address.
        #[arg(long)]
        no_dns: bool,
        /// Run after the ACL file changes.
        #[arg(long, env = "VERCELIFY_MESH_ACL_HOOK")]
        acl_hook: Option<String>,
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
            let local: Vec<String> = vercelify_mesh::tun::local_ipv4s(None)
                .into_iter()
                .map(|ip| format!("{ip}:{port}"))
                .collect();
            match rt.block_on(daemon::join(&cli.dir, &token, &name, local)) {
                Ok(s) => println!("joined as {} ({}, {})", s.name, s.ipv4, s.node_id),
                Err(e) => fail(e),
            }
        }
        Cmd::Up {
            interface,
            port,
            no_dns,
            acl_hook,
        } => {
            let opts = UpOptions {
                dir: cli.dir,
                run_dir: cli.run_dir,
                interface,
                port,
                dns: !no_dns,
                acl_hook,
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
    }
}

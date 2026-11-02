//! `vercelify-host`: the host manager daemon (`serve`) and a small CLI that
//! talks to it on this computer with the owner token.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vercelify_core::host::{human_bytes, recommend, EnvironmentGrant, GIB};
use vercelify_host::guest::{API_PORT, STATUS_PORT};
use vercelify_host::manager::Manager;
use vercelify_host::provider::Provider;
use vercelify_host::{api, lima, measure, qemu, state};

const USAGE: &str = "vercelify-host: the Vercelify environment on this computer

Usage:
  vercelify-host serve                      run the host manager (the service does this)
  vercelify-host status                     the environment and the last operation
  vercelify-host recommend                  what this computer can give it
  vercelify-host create --join vfyj1.… [--memory GB] [--cpus N] [--disk GB] --yes
  vercelify-host create --main [--memory GB] [--cpus N] [--disk GB] --yes
  vercelify-host start
  vercelify-host stop --yes                 stops every app on this computer
  vercelify-host resize [--memory GB] [--cpus N] [--disk GB] --yes
  vercelify-host remove --yes               deletes the environment and all its data
";

fn home() -> Result<PathBuf, String> {
    if let Some(h) = std::env::var_os("VERCELIFY_HOST_HOME") {
        return Ok(h.into());
    }
    if cfg!(target_os = "macos") {
        let h = std::env::var_os("HOME").ok_or("HOME isn't set")?;
        Ok(PathBuf::from(h).join("Library/Application Support/Vercelify/host"))
    } else if cfg!(windows) {
        let p = std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into());
        Ok(PathBuf::from(p).join("Vercelify").join("host"))
    } else {
        Err("on Linux, Vercelify runs directly (install.sh): no environment needed".into())
    }
}

/// Machine-wide, so a second OS account's host manager is refused.
fn lock_path(home: &std::path::Path) -> PathBuf {
    if std::env::var_os("VERCELIFY_HOST_HOME").is_none() && cfg!(target_os = "macos") {
        let shared = PathBuf::from("/Users/Shared/Vercelify");
        if std::fs::create_dir_all(&shared).is_ok() {
            return shared.join("host.lock");
        }
    }
    home.join("host.lock")
}

fn hostname() -> String {
    let raw = std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_default();
    sanitize_hostname(&raw)
}

fn sanitize_hostname(raw: &str) -> String {
    let s: String = raw
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').chars().take(55).collect::<String>();
    let s = s.trim_matches('-');
    if s.is_empty() {
        "vercelify-host".into()
    } else {
        format!("{s}-env")
    }
}

fn provider(home: &std::path::Path) -> Result<Arc<dyn Provider>, String> {
    if cfg!(target_os = "macos") {
        let limactl = lima::find_limactl().ok_or("Lima (limactl) isn't installed next to vercelify-host")?;
        Ok(Arc::new(lima::Lima {
            limactl,
            home: home.join("lima"),
            vm_type: lima::vm_type_for(&measure::os_version()),
        }))
    } else if cfg!(windows) {
        // VERCELIFY_QEMU_DIR, next to us, or where `winget install
        // SoftwareFreedomConservancy.QEMU` puts it.
        let mut dirs: Vec<PathBuf> = vec![];
        if let Some(d) = std::env::var_os("VERCELIFY_QEMU_DIR") {
            dirs.push(d.into());
        }
        if let Some(d) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.join("qemu")))
        {
            dirs.push(d);
        }
        if let Some(pf) = std::env::var_os("ProgramFiles") {
            dirs.push(PathBuf::from(pf).join("qemu"));
        }
        let bin_dir = dirs.into_iter().find(|d| d.join("qemu-system-x86_64.exe").is_file()).ok_or(
            "QEMU isn't installed: winget install --id SoftwareFreedomConservancy.QEMU (then start vercelify-host again)",
        )?;
        Ok(Arc::new(qemu::Qemu {
            bin_dir,
            dir: home.join("qemu"),
        }))
    } else {
        Err("on Linux, Vercelify runs directly (install.sh): no environment needed".into())
    }
}

/// The owner token: made once, readable only by this account. Desktop on
/// this computer reads it from here.
fn owner_token(home: &std::path::Path) -> Result<String, String> {
    let p = home.join("owner.token");
    if let Ok(t) = std::fs::read_to_string(&p) {
        let t = t.trim().to_string();
        if t.len() >= 32 {
            return Ok(t);
        }
    }
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| e.to_string())?;
    let t = format!("vfyh_{}", state::hex(&b));
    state::write_private(&p, t.as_bytes()).map_err(|e| e.to_string())?;
    Ok(t)
}

fn serve() -> Result<(), String> {
    let home = home()?;
    std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
    let _lock = state::OwnerLock::acquire(&lock_path(&home)).map_err(|e| e.to_string())?;
    let provider = provider(&home)?;
    let token = owner_token(&home)?;
    let data = home.clone();
    let p2 = provider.clone();
    let m = Manager::open(
        provider,
        home.join("state.json"),
        state::sha256_hex(&token),
        Box::new(move || measure::capabilities(&data, p2.virtualization(), Some(p2.name()))),
        Box::new(|| chrono::Utc::now().timestamp()),
        hostname(),
        std::env::var("VERCELIFY_HOST_INSTALL_REF").unwrap_or_else(|_| format!("v{}", env!("CARGO_PKG_VERSION"))),
    )
    .map_err(|e| e.to_string())?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let status = Arc::new(Mutex::new(measure::host_status(
            &home,
            None,
            m.environment().grant.as_ref(),
        )));
        {
            let (m, status, home) = (m.clone(), status.clone(), home.clone());
            tokio::spawn(async move {
                loop {
                    let (m2, h2) = (m.clone(), home.clone());
                    let s = tokio::task::spawn_blocking(move || {
                        // Every 20 s (a `limactl list` or one QMP query):
                        // bring the environment back to what the owner
                        // asked for, after a reboot or a crash.
                        m2.reconcile();
                        measure::host_status(&h2, None, m2.environment().grant.as_ref())
                    })
                    .await;
                    if let Ok(s) = s {
                        *status.lock().unwrap_or_else(|p| p.into_inner()) = s;
                    }
                    tokio::time::sleep(Duration::from_secs(20)).await;
                }
            });
        }
        let mgmt = tokio::net::TcpListener::bind(("127.0.0.1", API_PORT))
            .await
            .map_err(|e| format!("127.0.0.1:{API_PORT}: {e}"))?;
        let guest = tokio::net::TcpListener::bind(("127.0.0.1", STATUS_PORT))
            .await
            .map_err(|e| format!("127.0.0.1:{STATUS_PORT}: {e}"))?;
        tracing::info!("host manager on 127.0.0.1:{API_PORT} (guest status on :{STATUS_PORT})");
        let g = api::guest_router(api::Guest {
            manager: m.clone(),
            status,
        });
        tokio::spawn(async move {
            let _ = axum::serve(guest, g).await;
        });
        axum::serve(mgmt, api::router(api::Api::new(m)))
            .await
            .map_err(|e| e.to_string())
    })
}

// ----- the CLI: plain HTTP to 127.0.0.1 ---------------------------------------------------

fn request(method: &str, path: &str, body: Option<&serde_json::Value>) -> Result<serde_json::Value, String> {
    let token = owner_token(&home()?)?;
    let mut s = TcpStream::connect(("127.0.0.1", API_PORT))
        .map_err(|_| "the host manager isn't running (vercelify-host serve)".to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(120))).ok();
    let b = body.map(|b| b.to_string()).unwrap_or_default();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
        b.len()
    )
    .map_err(|e| e.to_string())?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).map_err(|e| e.to_string())?;
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let code: u16 = head.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = dechunk(head, body);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    if (200..300).contains(&code) {
        Ok(v)
    } else {
        Err(v["error"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| format!("HTTP {code}")))
    }
}

fn dechunk(head: &str, body: &str) -> String {
    if !head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        return body.to_string();
    }
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, after)) = rest.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 || after.len() < n {
            break;
        }
        out.push_str(&after[..n]);
        rest = after[n..].trim_start_matches("\r\n");
    }
    out
}

fn key() -> String {
    state::new_id("cli")
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn gb(args: &[String], name: &str) -> Result<Option<u64>, String> {
    flag(args, name)
        .map(|v| {
            v.parse::<f64>()
                .ok()
                .filter(|g| *g > 0.0 && *g < 4096.0)
                .map(|g| ((g * 1024.0).round() as u64) * (GIB / 1024))
                .ok_or_else(|| format!("{name} wants a size in GB"))
        })
        .transpose()
}

fn chosen(args: &[String], base: EnvironmentGrant) -> Result<EnvironmentGrant, String> {
    Ok(EnvironmentGrant {
        memory_bytes: gb(args, "--memory")?.unwrap_or(base.memory_bytes),
        cpus: flag(args, "--cpus")
            .map(|c| c.parse().map_err(|_| "--cpus wants a number".to_string()))
            .transpose()?
            .unwrap_or(base.cpus),
        disk_bytes: gb(args, "--disk")?.unwrap_or(base.disk_bytes),
        policy_version: base.policy_version,
    })
}

fn follow(op: &serde_json::Value) -> Result<(), String> {
    let id = op["id"].as_str().ok_or("no operation id")?.to_string();
    let mut last = String::new();
    loop {
        let o = request("GET", &format!("/v1/host/operations/{id}"), None)?;
        let stage = o["stage"].as_str().unwrap_or("").to_string();
        if stage != last && !stage.is_empty() {
            println!("  {}", stage.replace('_', " "));
            last = stage;
        }
        match o["status"].as_str() {
            Some("succeeded") => return Ok(()),
            Some("failed") => return Err(o["error"].as_str().unwrap_or("failed").to_string()),
            _ => std::thread::sleep(Duration::from_secs(2)),
        }
    }
}

fn print_grant(g: &serde_json::Value) {
    let b = |k: &str| human_bytes(g[k].as_u64().unwrap_or(0));
    println!(
        "  Memory {}  ·  Processing power {} processors  ·  Storage limit {}",
        b("memory_bytes"),
        g["cpus"],
        b("disk_bytes")
    );
}

fn cli(args: &[String]) -> Result<(), String> {
    let yes = args.iter().any(|a| a == "--yes");
    match args.first().map(String::as_str) {
        Some("serve") => serve(),
        Some("status") => {
            let e = request("GET", "/v1/host/environment", None)?;
            println!(
                "Vercelify environment: {}",
                e["state"].as_str().unwrap_or("?").replace('_', " ")
            );
            if e["grant"].is_object() {
                print_grant(&e["grant"]);
            }
            if let Some(r) = e["reason"].as_str() {
                println!("  {r}");
            }
            println!(
                "  revision {}  ·  {}",
                e["revision"],
                e["provider"].as_str().unwrap_or("")
            );
            Ok(())
        }
        Some("recommend") => {
            let r = request("POST", "/v1/host/environment/recommendation", None)?;
            for b in r["blockers"].as_array().into_iter().flatten() {
                println!("Can't host: {}", b.as_str().unwrap_or(""));
            }
            if r["grant"].is_object() {
                println!("Recommended for this computer:");
                print_grant(&r["grant"]);
                println!("Most it can give:");
                print_grant(&r["max"]);
            }
            for w in r["warnings"].as_array().into_iter().flatten() {
                println!("Note: {}", w.as_str().unwrap_or(""));
            }
            Ok(())
        }
        Some("create") => {
            let r = request("POST", "/v1/host/environment/recommendation", None)?;
            let rec: vercelify_core::host::EnvironmentRecommendation =
                serde_json::from_value(r.clone()).map_err(|e| e.to_string())?;
            let base = rec.grant.ok_or_else(|| rec.blockers.join("; "))?;
            let grant = chosen(args, base)?;
            let join = flag(args, "--join");
            if join.is_none() && !args.iter().any(|a| a == "--main") {
                return Err("say how it joins: --join vfyj1.… (from `sudo vercelify mesh invite` on your main server) or --main".into());
            }
            println!("Create Vercelify environment:");
            print_grant(&serde_json::to_value(grant).map_err(|e| e.to_string())?);
            if !yes {
                return Err("add --yes to create it".into());
            }
            let op = request(
                "POST",
                "/v1/host/environment",
                Some(&serde_json::json!({
                    "idempotency_key": key(),
                    "grant": grant,
                    "accepted_measured_at": rec.measured_at,
                    "join": join,
                })),
            )?;
            follow(&op)?;
            println!("Ready.");
            Ok(())
        }
        Some("start") => follow(&request(
            "POST",
            "/v1/host/environment/start",
            Some(&serde_json::json!({ "idempotency_key": key() })),
        )?),
        Some(cmd @ ("stop" | "remove")) => {
            if !yes {
                return Err(if cmd == "stop" {
                    "this stops every app on this computer: add --yes".into()
                } else {
                    "this deletes the environment with every app, database and file in it: add --yes".into()
                });
            }
            let e = request("GET", "/v1/host/environment", None)?;
            let body = serde_json::json!({ "idempotency_key": key(), "revision": e["revision"], "acknowledged": true });
            let op = if cmd == "stop" {
                request("POST", "/v1/host/environment/stop", Some(&body))?
            } else {
                request("DELETE", "/v1/host/environment", Some(&body))?
            };
            follow(&op)
        }
        Some("resize") => {
            let e = request("GET", "/v1/host/environment", None)?;
            let base: EnvironmentGrant =
                serde_json::from_value(e["grant"].clone()).map_err(|_| "no environment yet")?;
            let grant = chosen(args, base)?;
            let op = request(
                "PATCH",
                "/v1/host/environment",
                Some(&serde_json::json!({
                    "idempotency_key": key(),
                    "revision": e["revision"],
                    "grant": grant,
                    "restart_ok": yes,
                })),
            )?;
            follow(&op)
        }
        _ => {
            print!("{USAGE}");
            let _ = recommend;
            Ok(())
        }
    }
}

fn main() {
    tracing_subscriber::fmt().with_target(false).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = cli(&args) {
        eprintln!("vercelify-host: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostnames() {
        assert_eq!(
            sanitize_hostname("Shashank's MacBook Pro"),
            "shashank-s-macbook-pro-env"
        );
        assert_eq!(sanitize_hostname(""), "vercelify-host");
        assert_eq!(sanitize_hostname("---"), "vercelify-host");
        assert!(sanitize_hostname(&"x".repeat(100)).len() <= 63);
    }

    #[test]
    fn chunked_bodies() {
        assert_eq!(
            dechunk(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked",
                "5\r\nhello\r\n0\r\n\r\n"
            ),
            "hello"
        );
        assert_eq!(dechunk("HTTP/1.1 200 OK\r\nContent-Length: 2", "{}"), "{}");
    }

    #[test]
    fn sizes() {
        let a: Vec<String> = ["--memory", "4.5", "--cpus", "3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let g = chosen(
            &a,
            EnvironmentGrant {
                memory_bytes: GIB,
                cpus: 1,
                disk_bytes: 20 * GIB,
                policy_version: 1,
            },
        )
        .unwrap();
        assert_eq!(g.memory_bytes, 4 * GIB + GIB / 2);
        assert_eq!(g.cpus, 3);
        assert_eq!(g.disk_bytes, 20 * GIB);
        assert!(chosen(&["--memory".to_string(), "x".to_string()], g).is_err());
    }
}

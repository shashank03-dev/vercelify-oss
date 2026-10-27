//! Black-box test: runs the real agent binary against stub host tools and a
//! fixture sysfs tree, and exercises every endpoint over HTTP.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TOKEN: &str = "test-token-0123456789abcdef";

struct Agent {
    child: Child,
    port: u16,
    log: PathBuf,
    _dir: tempfile::TempDir,
    root: PathBuf,
    storage: PathBuf,
    extra_env: Vec<(String, String)>,
}

impl Agent {
    /// Run a CLI subcommand with the same configuration as the daemon.
    fn cli(&self, args: &[&str]) -> std::process::Output {
        let env: Vec<(&str, String)> = self.extra_env.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let r = &self.root;
        agent_cmd(
            r,
            &r.join("sys"),
            &self.storage,
            &r.join("power-guard.state"),
            &self.log,
            self.port,
            &env,
        )
        .args(args)
        .output()
        .unwrap()
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn write(root: &Path, rel: &str, val: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, val).unwrap();
}

fn start() -> Agent {
    start_with(&[])
}

fn start_with(extra_env: &[(&str, String)]) -> Agent {
    let dir = tempfile::tempdir().unwrap();
    let sys = dir.path().join("sys");
    write(&sys, "class/power_supply/AC/type", "Mains");
    write(&sys, "class/power_supply/AC/online", "0");
    write(&sys, "class/power_supply/BAT0/type", "Battery");
    write(&sys, "class/power_supply/BAT0/capacity", "42");
    write(&sys, "class/power_supply/BAT0/status", "Discharging");
    write(&sys, "class/thermal/thermal_zone0/type", "x86_pkg_temp");
    write(&sys, "class/thermal/thermal_zone0/temp", "55000");
    let storage = dir.path().join("storage");
    std::fs::create_dir_all(&storage).unwrap();
    let pg = dir.path().join("power-guard.state");
    std::fs::write(
        &pg,
        r#"{"state":"warning","reason":"on battery (42%)","updated_at":"2026-10-18T10:00:00Z"}"#,
    )
    .unwrap();
    let log = dir.path().join("calls.log");

    // free_port() releases the port before the agent binds it, and the tests
    // run in parallel with their own fake servers: make sure the port is
    // really this agent's, else start it again on another one.
    let (child, port) = (0..5)
        .find_map(|_| {
            let port = free_port();
            let mut child = agent_cmd(dir.path(), &sys, &storage, &pg, &log, port, extra_env)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if child.try_wait().unwrap().is_some() {
                    return None; // lost the port to another server
                }
                if listens_on(child.id(), port) {
                    return Some((child, port));
                }
                assert!(Instant::now() < deadline, "agent did not start");
                std::thread::sleep(Duration::from_millis(50));
            }
        })
        .expect("agent could not get a free port");
    let agent = Agent {
        child,
        port,
        log,
        root: dir.path().to_path_buf(),
        _dir: dir,
        storage,
        extra_env: extra_env.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
    };
    agent
}

/// Whether process `pid` itself has a socket listening on TCP `port`.
#[cfg(target_os = "linux")]
fn listens_on(pid: u32, port: u16) -> bool {
    let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return false;
    };
    let inodes: Vec<String> = fds
        .flatten()
        .filter_map(|f| std::fs::read_link(f.path()).ok())
        .filter_map(|l| {
            let l = l.to_string_lossy().to_string();
            Some(l.strip_prefix("socket:[")?.strip_suffix(']')?.to_string())
        })
        .collect();
    let want = format!(":{port:04X}");
    ["/proc/net/tcp", "/proc/net/tcp6"].iter().any(|t| {
        std::fs::read_to_string(t).unwrap_or_default().lines().skip(1).any(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            // local address, state 0A = LISTEN, inode
            f.len() > 9 && f[1].ends_with(&want) && f[3] == "0A" && inodes.iter().any(|i| i == f[9])
        })
    })
}

#[cfg(target_os = "macos")]
fn listens_on(pid: u32, port: u16) -> bool {
    Command::new("lsof")
        .args([
            "-nP",
            "-a",
            "-p",
            &pid.to_string(),
            &format!("-iTCP:{port}"),
            "-sTCP:LISTEN",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The agent binary with the test environment (also used for CLI subcommands).
fn agent_cmd(
    dir: &Path,
    sys: &Path,
    storage: &Path,
    pg: &Path,
    log: &Path,
    port: u16,
    extra_env: &[(&str, String)],
) -> Command {
    let stubs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/stubs");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vercelify-agent"));
    cmd.arg("--config")
        .arg(dir.join("absent.toml"))
        .env("VERCELIFY_LISTEN", "127.0.0.1")
        .env("VERCELIFY_PORT", port.to_string())
        .env("VERCELIFY_TOKEN", TOKEN)
        .env("VERCELIFY_SYS_CLASS", sys.join("class"))
        .env("VERCELIFY_SYS_BLOCK", sys.join("block"))
        .env("VERCELIFY_STORAGE_MOUNT", storage)
        .env("VERCELIFY_HDD_VOLUME_ROOT", storage.join("volumes"))
        .env("VERCELIFY_REQUIRE_STORAGE_MOUNT", "false")
        .env("VERCELIFY_POWER_GUARD_STATE", pg)
        .env("VERCELIFY_DOCKER_BIN", stubs.join("docker"))
        .env("VERCELIFY_TAILSCALE_BIN", stubs.join("tailscale"))
        .env("VERCELIFY_SYSTEMCTL_BIN", stubs.join("systemctl"))
        .env("VERCELIFY_MPV_BIN", stubs.join("mpv"))
        .env("VERCELIFY_CORS_ORIGINS", "http://localhost:1420")
        .env("VERCELIFY_GIT_BIN", stubs.join("git"))
        .env("VERCELIFY_SSH_KEYGEN_BIN", stubs.join("ssh-keygen"))
        .env("VERCELIFY_SYSTEMD_RUN_BIN", stubs.join("systemd-run"))
        .env("VERCELIFY_CLI_BIN", stubs.join("vercelify"))
        .env("VERCELIFY_FSTAB", dir.join("fstab"))
        .env("VERCELIFY_STATE_DIR", dir.join("state"))
        .env("VERCELIFY_RUN_DIR", dir.join("run"))
        .env("VERCELIFY_INSTALL_ENV", dir.join("install.env"))
        // A server not on the platform engine, unless a test says so.
        .env("VERCELIFY_ENGINE", "coolify")
        .env("VERCELIFY_COOLIFY_DATA_DIR", dir.join("coolify"))
        .env("VERCELIFY_TOKENS_FILE", dir.join("tokens.json"))
        .env("VERCELIFY_AUDIT_LOG", dir.join("state/audit.log"))
        .env("VERCELIFY_POWER_GUARD_CONF", dir.join("power-guard.conf"))
        .env("VERCELIFY_SYSTEMD_DIR", dir.join("systemd"))
        .env("VERCELIFY_BACKUP_ENV", dir.join("backup.env"))
        .env("VERCELIFY_LOGIND_DROPIN", dir.join("logind.conf"))
        .env("VERCELIFY_REBOOT_REQUIRED_FILE", dir.join("reboot-required"))
        .env("VERCELIFY_SMARTCTL_BIN", stubs.join("absent-smartctl"))
        .env("VERCELIFY_TIMEDATECTL_BIN", stubs.join("absent-timedatectl"))
        // No background loops in tests.
        .env("VERCELIFY_WATCH_INTERVAL", "0")
        .env("VERCELIFY_WAKE_LISTEN", "")
        .env(
            "VERCELIFY_POWER_GUARD_OVERRIDES",
            dir.join("state/power-guard.local.conf"),
        )
        .env("VERCELIFY_MAINTENANCE_CONF", dir.join("maintenance.conf"))
        .env("VERCELIFY_PROXY_DIR", dir.join("proxy"))
        .env("VERCELIFY_TRAEFIK_METRICS_URL", "http://127.0.0.1:9/metrics")
        .env("VERCELIFY_PLATFORM_DB", dir.join("state/platform.db"))
        .env("VERCELIFY_MASTER_KEY_FILE", dir.join("state/master.key"))
        .env("VERCELIFY_MESH_DIR", dir.join("state/mesh"))
        .env("STUB_LOG", log);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd
}

/// Minimal HTTP/1.1 client so the test has no extra dependencies.
fn req(a: &Agent, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value, String) {
    let mut s = TcpStream::connect(("127.0.0.1", a.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    if let Some(t) = token {
        head.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    s.write_all(format!("{head}\r\n{body}").as_bytes()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (headers, rest) = raw.split_once("\r\n\r\n").unwrap();
    let status: u16 = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = if headers.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        dechunk(rest)
    } else {
        rest.to_string()
    };
    (
        status,
        serde_json::from_str(&body).unwrap_or(Value::Null),
        headers.to_string(),
    )
}

fn dechunk(mut s: &str) -> String {
    let mut out = String::new();
    loop {
        let (len, rest) = s.split_once("\r\n").unwrap();
        let n = usize::from_str_radix(len.trim(), 16).unwrap();
        if n == 0 {
            return out;
        }
        out.push_str(&rest[..n]);
        s = &rest[n + 2..];
    }
}

fn get(a: &Agent, path: &str) -> (u16, Value) {
    let (s, v, _) = req(a, "GET", path, Some(TOKEN), None);
    (s, v)
}

fn post(a: &Agent, path: &str, body: Value) -> (u16, Value) {
    let (s, v, _) = req(a, "POST", path, Some(TOKEN), Some(body));
    (s, v)
}

#[test]
fn protected_routes_reject_missing_wrong_and_revoked_tokens() {
    let a = start_with(&[("VERCELIFY_ENGINE", "platform".into())]);
    let (status, created) = post(&a, "/v1/tokens", json!({"name": "auth-matrix", "role": "owner"}));
    assert_eq!(status, 200, "{created}");
    let secret = created["secret"].as_str().unwrap();
    let id = created["token"]["id"].as_str().unwrap();
    assert_eq!(req(&a, "GET", "/v1/system", Some(secret), None).0, 200);
    assert_eq!(req(&a, "DELETE", &format!("/v1/tokens/{id}"), Some(TOKEN), None).0, 200);
    let routes = [
        ("GET", "/v1/system"),
        ("GET", "/v1/containers"),
        ("GET", "/v1/containers/app/logs"),
        ("POST", "/v1/containers/app/restart"),
        ("POST", "/v1/volumes"),
        ("GET", "/v1/services"),
        ("POST", "/v1/display/receiver/start"),
        ("GET", "/v1/doctor"),
        ("GET", "/v1/tokens"),
        ("GET", "/v1/audit"),
        ("GET", "/v1/backup"),
        ("POST", "/v1/backup/run"),
        ("POST", "/v1/llm/chat"),
        ("GET", "/v1/events"),
        ("PUT", "/v1/projects/app"),
        ("GET", "/v1/platform/apps"),
        ("POST", "/v1/platform/apps"),
        ("GET", "/v1/platform/apps/app/env?reveal=true"),
        ("POST", "/v1/platform/apps/app/deploy"),
        ("POST", "/v1/platform/apps/app/terminal"),
        ("GET", "/v1/platform/deployments/id/logs"),
        ("POST", "/v1/platform/databases/db/restore"),
        ("POST", "/v1/platform/databases/db/terminal"),
        ("DELETE", "/v1/platform/terminals/id"),
        ("GET", "/v1/runtime/status"),
        ("GET", "/v1/mesh/nodes"),
        ("GET", "/v1/volumes"),
        ("GET", "/v1/display"),
        ("POST", "/v1/display/receiver/stop"),
        ("POST", "/v1/services/service/start"),
        ("GET", "/v1/llm"),
        ("POST", "/v1/llm/install"),
        ("POST", "/v1/llm/models/pull"),
        ("POST", "/v1/llm/models/delete"),
        ("POST", "/v1/llm/start"),
        ("POST", "/v1/tokens"),
        ("DELETE", "/v1/tokens/id"),
        ("GET", "/v1/alerts"),
        ("POST", "/v1/alerts/notify"),
        ("POST", "/v1/alerts/test"),
        ("POST", "/v1/alerts/check"),
        ("GET", "/v1/metrics"),
        ("GET", "/v1/uptime"),
        ("GET", "/v1/update"),
        ("POST", "/v1/update"),
        ("GET", "/v1/jobs"),
        ("GET", "/v1/jobs/id"),
        ("GET", "/v1/sleep"),
        ("POST", "/v1/sleep/proxy"),
        ("PUT", "/v1/sleep/app"),
        ("GET", "/v1/power-guard"),
        ("PUT", "/v1/power-guard"),
        ("DELETE", "/v1/power-guard"),
        ("GET", "/v1/domain"),
        ("GET", "/v1/projects"),
        ("POST", "/v1/projects/check"),
        ("DELETE", "/v1/projects/app"),
        ("POST", "/v1/projects/app/sync"),
        ("POST", "/v1/projects/app/crons/run"),
        ("DELETE", "/v1/mesh/nodes/id"),
        ("POST", "/v1/mesh/invites"),
        ("GET", "/v1/mesh/status"),
        ("GET", "/v1/platform/projects"),
        ("POST", "/v1/platform/projects"),
        ("GET", "/v1/platform/projects/id"),
        ("PATCH", "/v1/platform/projects/id"),
        ("DELETE", "/v1/platform/projects/id"),
        ("POST", "/v1/platform/projects/id/environments"),
        ("GET", "/v1/platform/projects/id/env"),
        ("PUT", "/v1/platform/projects/id/env"),
        ("DELETE", "/v1/platform/projects/id/env/KEY"),
        ("DELETE", "/v1/platform/environments/id"),
        ("GET", "/v1/platform/environments/id/env"),
        ("PUT", "/v1/platform/environments/id/env"),
        ("DELETE", "/v1/platform/environments/id/env/KEY"),
        ("GET", "/v1/platform/apps/app"),
        ("PATCH", "/v1/platform/apps/app"),
        ("DELETE", "/v1/platform/apps/app"),
        ("PUT", "/v1/platform/apps/app/env"),
        ("POST", "/v1/platform/apps/app/env/import"),
        ("DELETE", "/v1/platform/apps/app/env/KEY"),
        ("GET", "/v1/platform/apps/app/domains"),
        ("POST", "/v1/platform/apps/app/domains"),
        ("DELETE", "/v1/platform/apps/app/domains/example.com"),
        ("GET", "/v1/platform/apps/app/volumes"),
        ("POST", "/v1/platform/apps/app/volumes"),
        ("DELETE", "/v1/platform/apps/app/volumes/data"),
        ("GET", "/v1/platform/apps/app/deployments"),
        ("POST", "/v1/platform/apps/app/rollback"),
        ("GET", "/v1/platform/apps/app/previews"),
        ("DELETE", "/v1/platform/apps/app/previews/1"),
        ("GET", "/v1/platform/deployments/id"),
        ("POST", "/v1/platform/deployments/id/cancel"),
        ("GET", "/v1/platform/nodes"),
        ("PATCH", "/v1/platform/nodes/id"),
        ("DELETE", "/v1/platform/nodes/id"),
        ("GET", "/v1/platform/settings/standby"),
        ("PUT", "/v1/platform/settings/standby"),
        ("GET", "/v1/platform/apps/app/instances"),
        ("GET", "/v1/platform/apps/app/git"),
        ("PUT", "/v1/platform/apps/app/git"),
        ("POST", "/v1/platform/apps/app/git/check"),
        ("PUT", "/v1/platform/apps/app/basic-auth"),
        ("GET", "/v1/platform/apps/app/hooks"),
        ("POST", "/v1/platform/apps/app/hooks"),
        ("DELETE", "/v1/platform/apps/app/hooks/id"),
        ("GET", "/v1/platform/apps/app/tasks"),
        ("POST", "/v1/platform/apps/app/tasks"),
        ("DELETE", "/v1/platform/apps/app/tasks/id"),
        ("POST", "/v1/platform/apps/app/tasks/id/run"),
        ("GET", "/v1/platform/git-sources"),
        ("POST", "/v1/platform/git-sources"),
        ("DELETE", "/v1/platform/git-sources/id"),
        ("POST", "/v1/platform/git-sources/id/webhook"),
        ("POST", "/v1/platform/github-app"),
        ("POST", "/v1/platform/github-app/convert"),
        ("GET", "/v1/platform/settings/domain"),
        ("PUT", "/v1/platform/settings/domain"),
        ("GET", "/v1/platform/databases"),
        ("POST", "/v1/platform/databases"),
        ("GET", "/v1/platform/databases/db"),
        ("PATCH", "/v1/platform/databases/db"),
        ("DELETE", "/v1/platform/databases/db"),
        ("POST", "/v1/platform/databases/db/link"),
        ("POST", "/v1/platform/databases/db/move"),
        ("GET", "/v1/platform/databases/db/backups"),
        ("POST", "/v1/platform/databases/db/backups"),
        ("GET", "/v1/platform/apps/app/logs"),
        ("POST", "/v1/platform/apps/app/restart"),
        ("GET", "/v1/platform/terminals/id/output"),
        ("POST", "/v1/platform/terminals/id/input"),
        ("POST", "/v1/platform/terminals/id/resize"),
        ("GET", "/v1/platform/templates"),
        ("POST", "/v1/platform/templates/id"),
        ("GET", "/v1/platform/volumes"),
        ("POST", "/v1/platform/volumes/id/move"),
    ];
    for (method, path) in routes {
        for token in [None, Some("wrong-token-wrong-token"), Some(secret)] {
            let status = req(&a, method, path, token, None).0;
            assert_eq!(status, 401, "{method} {path} with {token:?}");
        }
    }
}

#[test]
fn oversized_json_is_rejected_without_creating_an_app() {
    let a = start_with(&[("VERCELIFY_ENGINE", "platform".into())]);
    let body = json!({"name": "large", "kind": "git", "source": {"repository": "x".repeat(2 * 1024 * 1024 + 1)}});
    let (status, _, _) = req(&a, "POST", "/v1/platform/apps", Some(TOKEN), Some(body));
    assert_eq!(status, 413);
    let (status, apps) = get(&a, "/v1/platform/apps");
    assert_eq!(status, 200);
    assert!(apps.as_array().unwrap().is_empty());
}

#[test]
fn agent_end_to_end() {
    let a = start();

    // --- auth
    let (s, v, _) = req(&a, "GET", "/v1/health", None, None);
    assert_eq!(s, 200);
    assert_eq!(v["ok"], true);
    let (s, _, _) = req(&a, "GET", "/v1/system", None, None);
    assert_eq!(s, 401);
    let (s, _, _) = req(&a, "GET", "/v1/system", Some("wrong-token-wrong-token"), None);
    assert_eq!(s, 401);

    // --- CORS preflight for the browser dev mode
    let (_, _, headers) = {
        let mut st = TcpStream::connect(("127.0.0.1", a.port)).unwrap();
        st.write_all(b"OPTIONS /v1/system HTTP/1.1\r\nHost: x\r\nOrigin: http://localhost:1420\r\nAccess-Control-Request-Method: GET\r\nAccess-Control-Request-Headers: authorization\r\nConnection: close\r\n\r\n").unwrap();
        let mut raw = String::new();
        st.read_to_string(&mut raw).unwrap();
        (0, Value::Null, raw)
    };
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("access-control-allow-origin: http://localhost:1420"),
        "{headers}"
    );

    // --- system snapshot reads the fixture sysfs + power-guard state
    let (s, v) = get(&a, "/v1/system");
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["power"]["on_battery"], true);
    assert_eq!(v["power"]["lowest_capacity_pct"], 42);
    assert_eq!(v["thermal"]["max_c"], 55.0);
    assert_eq!(v["power_guard"]["state"], "warning");
    assert!(v["cpu"]["cores"].as_u64().unwrap() > 0);
    assert!(v["disks"].as_array().unwrap().iter().any(|d| d["mount_point"] == "/"));

    // --- containers
    let (s, v) = get(&a, "/v1/containers?stats=true");
    assert_eq!(s, 200, "{v}");
    let list = v.as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0]["name"], "pg-main"); // running apps first, by name
    assert_eq!(list[0]["app"], "shop");
    assert_eq!(list[0]["cpu_pct"], 2.5);
    assert_eq!(list[1]["name"], "vercelify-proxy");
    assert_eq!(list[2]["state"], "exited");

    let (s, _) = post(&a, "/v1/containers/pg-main/restart", json!({}));
    assert_eq!(s, 200);
    let (s, _) = post(&a, "/v1/containers/missing/stop", json!({}));
    assert_eq!(s, 404);
    let (s, _) = post(&a, "/v1/containers/pg-main/rm", json!({}));
    assert_eq!(s, 400);
    let (s, _) = post(&a, "/v1/containers/-f/stop", json!({}));
    assert_eq!(s, 400);

    let (s, v) = get(&a, "/v1/containers/pg-main/logs?tail=10");
    assert_eq!(s, 200);
    assert_eq!(v["lines"][0], "2026-10-18T10:00:00Z stderr line");
    assert_eq!(v["lines"][1], "2026-10-18T10:00:01Z stdout line");

    // --- volumes
    let (s, v) = post(
        &a,
        "/v1/volumes",
        json!({"service": "postgres", "name": "data", "tier": "hdd"}),
    );
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["created"], true);
    assert!(a.storage.join("volumes/postgres/data").is_dir());
    let (_, v) = post(
        &a,
        "/v1/volumes",
        json!({"service": "postgres", "name": "data", "tier": "hdd"}),
    );
    assert_eq!(v["created"], false);
    let (s, v) = post(&a, "/v1/volumes", json!({"service": "redis", "tier": "ssd"}));
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["source"], "redis-data");
    assert_eq!(v["host_path"], "/var/lib/docker/volumes/redis-data/_data");
    let (_, v) = post(&a, "/v1/volumes", json!({"service": "existing", "tier": "ssd"}));
    assert_eq!(v["created"], false);
    let (s, v) = post(
        &a,
        "/v1/volumes",
        json!({"service": "pg", "name": "../../etc", "tier": "hdd"}),
    );
    assert_eq!(s, 400, "{v}");
    let (s, v) = get(&a, "/v1/volumes");
    assert_eq!(s, 200);
    let names: Vec<_> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["postgres", "existing-data", "redis-data"]);

    // --- services
    let (s, v) = get(&a, "/v1/services");
    assert_eq!(s, 200);
    let docker = v.as_array().unwrap().iter().find(|s| s["name"] == "docker").unwrap();
    assert_eq!(docker["active_state"], "active");
    assert_eq!(docker["enabled"], true);
    let (s, _) = post(&a, "/v1/services/docker/restart", json!({}));
    assert_eq!(s, 200);
    let (s, _) = post(&a, "/v1/services/sshd/restart", json!({}));
    assert_eq!(s, 404);
    let (s, _) = post(&a, "/v1/services/vercelify-agent/stop", json!({}));
    assert_eq!(s, 400);

    // --- display receiver lifecycle
    let (s, v) = get(&a, "/v1/display");
    assert_eq!(s, 200);
    assert_eq!(v["receiver"]["running"], false);
    assert_eq!(v["sunshine"]["active_state"], "not-found");
    let (s, v) = post(
        &a,
        "/v1/display/receiver/start",
        json!({"transport": "udp", "port": 5600}),
    );
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["receiver"]["running"], true, "{v}");
    assert_eq!(v["receiver_url"], "udp://127.0.0.1:5600?pkt_size=1316");
    let (_, v) = get(&a, "/v1/display");
    assert_eq!(v["receiver"]["last_output"][0], "[stub] waiting for stream");
    let (s, _) = post(
        &a,
        "/v1/display/receiver/start",
        json!({"transport": "srt", "port": 80}),
    );
    assert_eq!(s, 503);
    let (_, v) = post(&a, "/v1/display/receiver/stop", json!({}));
    assert_eq!(v["receiver"]["running"], false);

    // --- versioning, hardware, metrics, uptime, images, backups
    let (_, v, _) = req(&a, "GET", "/v1/health", None, None);
    assert!(v["api_version"].as_u64().unwrap() >= 2, "{v}");
    let caps: Vec<String> = serde_json::from_value(v["capabilities"].clone()).unwrap();
    for c in ["alerts", "metrics", "tokens", "backup", "previews"] {
        assert!(caps.iter().any(|x| x == c), "{c} in {caps:?}");
    }
    let (_, v) = get(&a, "/v1/system");
    assert_eq!(v["hardware"]["profile"], "laptop", "{v}");
    let (s, v) = get(&a, "/v1/metrics?minutes=30");
    assert_eq!(s, 200);
    assert!(v["points"].is_array());
    let (s, v) = get(&a, "/v1/uptime");
    assert_eq!((s, v.as_array().map(Vec::len)), (200, Some(0)));
    let (s, v) = get(&a, "/v1/backup");
    assert_eq!(s, 200);
    assert_eq!(v["configured"], false);
    let (s, v) = post(&a, "/v1/backup/run", json!({}));
    assert_eq!(s, 409, "{v}");

    // --- the stubs saw exactly the commands we expect
    let calls = std::fs::read_to_string(&a.log).unwrap();
    assert!(calls.contains("docker restart pg-main"), "{calls}");
    assert!(
        calls.contains("docker volume create --label vercelify.service=redis redis-data"),
        "{calls}"
    );
    assert!(calls.contains("systemctl restart -- docker"), "{calls}");
    assert!(calls.contains("--gpu-context=drm"), "{calls}");
    assert!(calls.contains("udp://0.0.0.0:5600"), "{calls}");
    assert!(!calls.contains("sshd"), "{calls}");
}

// --- fake HTTP services ---------------------------------------------------------

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    headers: String,
    body: String,
}

type Handler = fn(&Seen) -> (u16, &'static str, String);

/// A tiny HTTP server on a random port; records every request.
fn fake_server(handler: Handler) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<Seen>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut c) = conn else { continue };
            let log = log.clone();
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let head_end = loop {
                    let n = c.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let len: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + 4 + len {
                    let n = c.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let mut first = head.lines().next().unwrap_or("").split_whitespace();
                let seen = Seen {
                    method: first.next().unwrap_or("").into(),
                    path: first.next().unwrap_or("").into(),
                    headers: head.clone(),
                    body: String::from_utf8_lossy(&buf[head_end + 4..]).to_string(),
                };
                let (status, ctype, body) = handler(&seen);
                log.lock().unwrap().push(seen);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = c.write_all(resp.as_bytes());
            });
        }
    });
    (port, seen)
}

fn fake_ollama(r: &Seen) -> (u16, &'static str, String) {
    match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api/version") => (200, "application/json", r#"{"version":"0.12.3"}"#.into()),
        ("GET", "/api/tags") => (
            200,
            "application/json",
            r#"{"models":[{"name":"gemma3:1b","size":815319791,"details":{"family":"gemma3","parameter_size":"999.89M","quantization_level":"Q4_K_M"}}]}"#.into(),
        ),
        ("GET", "/api/ps") => (200, "application/json", r#"{"models":[]}"#.into()),
        ("POST", "/api/pull") => (
            200,
            "application/x-ndjson",
            "{\"status\":\"pulling manifest\"}\n{\"status\":\"pulling a1\",\"total\":100,\"completed\":100}\n{\"status\":\"success\"}\n".into(),
        ),
        ("DELETE", "/api/delete") if r.body.contains("gemma3:1b") => (200, "application/json", String::new()),
        ("DELETE", "/api/delete") => (404, "application/json", r#"{"error":"model not found"}"#.into()),
        ("POST", "/api/chat") if r.body.contains("\"stream\":true") => (
            200,
            "application/x-ndjson",
            "{\"message\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"done\":false}\n{\"message\":{\"role\":\"assistant\",\"content\":\"lo\"},\"done\":true,\"eval_count\":2}\n".into(),
        ),
        ("POST", "/api/chat") if r.body.contains("missing:1b") => {
            (404, "application/json", r#"{"error":"model \"missing:1b\" not found, try pulling it first"}"#.into())
        }
        ("POST", "/api/chat") => (
            200,
            "application/json",
            r#"{"message":{"role":"assistant","content":"Hello from the laptop"},"done":true}"#.into(),
        ),
        _ => (404, "text/plain", "404 page not found".into()),
    }
}

#[test]
fn pairing_llm_and_doctor() {
    let (ollama_port, ollama_seen) = fake_server(fake_ollama);
    let a = start_with(&[
        ("VERCELIFY_OLLAMA_URL", format!("http://127.0.0.1:{ollama_port}")),
        ("STUB_OLLAMA_STATE", "running".into()),
    ]);

    // --- health tells the cockpit what this machine is
    let (_, v, _) = req(&a, "GET", "/v1/health", None, None);
    assert_eq!(v["role"], "main");
    assert_eq!(v["pairing_open"], false);

    // --- short pairing code: CLI creates it, the cockpit claims it once
    let (s, v, _) = req(&a, "POST", "/v1/pair/claim", None, Some(json!({"code": "2222-2222"})));
    assert_eq!(s, 404, "{v}");
    let out = a.cli(&["pair", "--short"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let code = String::from_utf8(out.stdout).unwrap().trim().to_string();
    assert_eq!(code.len(), 9, "{code}");
    let (_, v, _) = req(&a, "GET", "/v1/health", None, None);
    assert_eq!(v["pairing_open"], true);
    let wrong = if code == "2222-2222" { "3333-3333" } else { "2222-2222" };
    let (s, v, _) = req(&a, "POST", "/v1/pair/claim", None, Some(json!({ "code": wrong })));
    assert_eq!(s, 403);
    assert!(v["error"].as_str().unwrap().contains("4 attempts left"), "{v}");
    let (s, v, _) = req(
        &a,
        "POST",
        "/v1/pair/claim",
        None,
        Some(json!({ "code": code.to_lowercase() })),
    );
    assert_eq!(s, 200, "{v}");
    let full = v["pairing"].as_str().unwrap();
    let p = vercelify_core::pairing::decode(full).unwrap();
    // Each pairing gets its own device token, not the host's.
    assert!(p.agent_token.starts_with("vfyd_"), "{}", p.agent_token);
    assert_ne!(p.agent_token, TOKEN);
    let device = p.agent_token.clone();
    let (s, _, _) = req(&a, "GET", "/v1/system", Some(&device), None);
    assert_eq!(s, 200, "the device token works");
    assert_eq!(p.role.as_deref(), Some("main"));
    assert!(!p.hostname.is_empty());
    let (s, _, _) = req(&a, "POST", "/v1/pair/claim", None, Some(json!({ "code": code })));
    assert_eq!(s, 404, "one-time");

    // the long code is unchanged for scripts
    let out = a.cli(&["pair"]);
    assert!(String::from_utf8(out.stdout).unwrap().starts_with("vfy1."));

    // `vercelify pair` shows both codes from one call: one device token, not two
    let (_, before, _) = req(&a, "GET", "/v1/tokens", Some(&device), None);
    let out = a.cli(&["pair", "--short", "--with-code"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert_eq!(lines[0].len(), 9, "{stdout}");
    let (_, after, _) = req(&a, "GET", "/v1/tokens", Some(&device), None);
    assert_eq!(after.as_array().unwrap().len(), before.as_array().unwrap().len() + 1);
    let (s, v, _) = req(&a, "POST", "/v1/pair/claim", None, Some(json!({ "code": lines[0] })));
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        v["pairing"].as_str().unwrap(),
        lines[1],
        "the short code stands for the printed full code"
    );

    // --- device tokens: listed, audited, revocable
    let (s, v, _) = req(&a, "GET", "/v1/tokens", Some(&device), None);
    assert_eq!(s, 200, "{v}");
    let mine = v
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["current"] == true)
        .cloned()
        .unwrap_or_else(|| panic!("{v}"));
    assert!(mine["name"].as_str().unwrap().starts_with("paired "));
    assert!(!v.to_string().contains(&device), "secrets are never listed");
    let (s, _, _) = req(
        &a,
        "POST",
        "/v1/containers/pg-main/restart",
        Some(&device),
        Some(json!({})),
    );
    assert_eq!(s, 200);
    let raw = std::fs::read_to_string(a.root.join("tokens.json")).unwrap();
    assert!(!raw.contains(&device), "only hashes on disk");
    let out = a.cli(&["tokens", "revoke", mine["id"].as_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let (s, _, _) = req(&a, "GET", "/v1/system", Some(&device), None);
    assert_eq!(s, 401, "revoked");
    let (s, v) = get(&a, "/v1/audit");
    assert_eq!(s, 200);
    let entries = v.as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e["actor"] == mine["id"] && e["path"] == "/v1/containers/pg-main/restart" && e["status"] == 200),
        "{v}"
    );
    assert!(!entries.iter().any(|e| e["method"] == "GET"), "reads are not audited");

    // --- alerts: a notification URL set from the cockpit, sent with curl
    let (ntfy_port, ntfy_seen) = fake_server(|_| (200, "application/json", "{}".into()));
    let (s, _) = post(&a, "/v1/alerts/test", json!({}));
    assert_eq!(s, 409, "no target yet");
    let (s, v) = post(&a, "/v1/alerts/notify", json!({"url": "ftp://nope"}));
    assert_eq!(s, 400, "{v}");
    let (s, v) = post(
        &a,
        "/v1/alerts/notify",
        json!({ "url": format!("http://127.0.0.1:{ntfy_port}/secret-topic") }),
    );
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["notify_target"], format!("http://127.0.0.1:{ntfy_port}/•••"));
    let (s, v) = post(&a, "/v1/alerts/test", json!({}));
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["notified"], true);
    {
        let seen = ntfy_seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].path, "/secret-topic");
        assert!(seen[0].headers.contains("Title: Vercelify"), "{}", seen[0].headers);
        assert_eq!(seen[0].body, "Alerts from this server reach you.");
    }
    let (_, v) = get(&a, "/v1/alerts");
    assert_eq!(v["alerts"][0]["title"], "Test notification");
    let calls = std::fs::read_to_string(&a.log).unwrap_or_default();
    assert!(!calls.contains("secret-topic"));

    // --- local LLMs
    let (s, v) = get(&a, "/v1/llm");
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["installed"], true);
    assert_eq!(v["ready"], true);
    assert_eq!(v["version"], "0.12.3");
    assert_eq!(v["models"][0]["name"], "gemma3:1b");
    assert_eq!(v["models"][0]["parameter_size"], "999.89M");
    assert_eq!(v["data_path"], "/var/lib/docker/volumes/ollama-models/_data");
    assert_eq!(
        v["endpoints"][0], "http://127.0.0.1:11434",
        "Tailscale's address, without the mesh"
    );

    let (s, v) = post(&a, "/v1/llm/models/pull", json!({"model": "qwen2.5:3b"}));
    assert_eq!(s, 200, "{v}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, v) = get(&a, "/v1/llm");
        let t = v["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["target"] == "qwen2.5:3b")
            .cloned()
            .unwrap();
        if t["done"] == true {
            assert_eq!(t["status"], "success", "{t}");
            assert!(t["error"].is_null());
            break;
        }
        assert!(Instant::now() < deadline, "pull never finished: {t}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let (s, _) = post(&a, "/v1/llm/models/pull", json!({"model": "../../etc"}));
    assert_eq!(s, 400);

    let (s, v) = post(
        &a,
        "/v1/llm/chat",
        json!({"model": "gemma3:1b", "messages": [{"role": "user", "content": "hi"}]}),
    );
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["message"]["content"], "Hello from the laptop");
    // streaming passes Ollama's NDJSON through
    let (s, _, raw) = {
        let mut st = TcpStream::connect(("127.0.0.1", a.port)).unwrap();
        let body =
            json!({"model": "gemma3:1b", "stream": true, "messages": [{"role": "user", "content": "hi"}]}).to_string();
        st.write_all(
            format!(
                "POST /v1/llm/chat HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
        let mut raw = String::new();
        st.read_to_string(&mut raw).unwrap();
        (200, Value::Null, raw)
    };
    assert_eq!(s, 200);
    assert!(
        raw.to_ascii_lowercase().contains("content-type: application/x-ndjson"),
        "{raw}"
    );
    assert!(
        raw.contains("\"content\":\"Hel\"") && raw.contains("\"done\":true"),
        "{raw}"
    );
    let (s, v) = post(
        &a,
        "/v1/llm/chat",
        json!({"model": "missing:1b", "messages": [{"role": "user", "content": "hi"}]}),
    );
    assert_eq!(s, 404);
    assert!(v["error"].as_str().unwrap().contains("download it first"), "{v}");
    let (s, _) = post(
        &a,
        "/v1/llm/chat",
        json!({"model": "gemma3:1b", "messages": [{"role": "root", "content": "hi"}]}),
    );
    assert_eq!(s, 400);

    let (s, _) = post(&a, "/v1/llm/models/delete", json!({"model": "gemma3:1b"}));
    assert_eq!(s, 200);
    let (s, _) = post(&a, "/v1/llm/models/delete", json!({"model": "nope:1b"}));
    assert_eq!(s, 404);
    let (s, _) = post(&a, "/v1/llm/restart", json!({}));
    assert_eq!(s, 200);
    let (s, _) = post(&a, "/v1/llm/explode", json!({}));
    assert_eq!(s, 400);
    let (s, v) = post(&a, "/v1/llm/install", json!({"tier": "ssd"}));
    assert_eq!(s, 200, "{v}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, v) = get(&a, "/v1/llm");
        let t = v["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["kind"] == "install")
            .cloned()
            .unwrap();
        if t["done"] == true {
            assert_eq!(t["status"], "success", "{t}");
            break;
        }
        assert!(Instant::now() < deadline, "install never finished: {t}");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(ollama_seen
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.path == "/api/pull" && r.body.contains("qwen2.5:3b")));

    // --- doctor: structured health checks
    let (s, v) = get(&a, "/v1/doctor");
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["role"], "main");
    let checks = v["checks"].as_array().unwrap();
    let by_id = |id: &str| checks.iter().find(|c| c["id"] == id).cloned().unwrap_or(Value::Null);
    assert_eq!(by_id("docker")["status"], "ok", "{v}");
    assert_eq!(by_id("docker")["detail"], "Docker 27.3.1");
    assert_eq!(by_id("mesh")["status"], "warn", "not on the mesh yet");
    assert_eq!(by_id("migrate")["status"], "warn", "{v}");
    assert!(by_id("migrate")["fix"].as_str().unwrap().contains("vercelify migrate"));
    assert_eq!(by_id("storage")["status"], "warn", "single disk");
    assert_eq!(by_id("firewall")["status"], "warn");
    assert!(by_id("firewall")["fix"]
        .as_str()
        .unwrap()
        .contains("vercelify-firewall"));
    assert_eq!(by_id("power")["status"], "warn", "the fixture laptop is on battery");
    assert_eq!(by_id("ollama")["status"], "ok");

    // --- update from the cockpit runs `vercelify update` as its own unit
    let (s, v) = get(&a, "/v1/update");
    assert_eq!(s, 200);
    assert_eq!(v["running"], false);
    let (s, v) = post(&a, "/v1/update", json!({}));
    assert_eq!(s, 200, "{v}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, v) = get(&a, "/v1/update");
        if v["exit_code"] == 0 {
            assert!(v["started_at"].is_string());
            let tail: Vec<String> = serde_json::from_value(v["log_tail"].clone()).unwrap();
            assert_eq!(
                tail,
                ["==> Downloading Vercelify", "  ✓ agent 0.2.0 running on :7420"],
                "colours stripped"
            );
            break;
        }
        assert!(Instant::now() < deadline, "update never finished: {v}");
        std::thread::sleep(Duration::from_millis(100));
    }

    let calls = std::fs::read_to_string(&a.log).unwrap();
    assert!(
        calls.contains("docker run -d --name vercelify-ollama --restart unless-stopped"),
        "{calls}"
    );
    assert!(
        calls.contains("-p 11434:11434 -v ollama-models:/root/.ollama"),
        "{calls}"
    );
    assert!(calls.contains("docker restart vercelify-ollama"), "{calls}");
    assert!(calls.contains("systemd-run --unit=vercelify-update"), "{calls}");
    assert!(calls.contains("vercelify update noninteractive=1"), "{calls}");
}

#[test]
fn worker_role_and_missing_ollama() {
    // Nothing listens on the Ollama port here, whatever runs on this machine.
    let dead = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let a = start_with(&[
        ("VERCELIFY_ROLE", "worker".into()),
        ("VERCELIFY_OLLAMA_URL", format!("http://127.0.0.1:{dead}")),
    ]);
    let (_, v, _) = req(&a, "GET", "/v1/health", None, None);
    assert_eq!(v["role"], "worker");
    let out = a.cli(&["pair", "--tailscale"]);
    let p = vercelify_core::pairing::decode(String::from_utf8(out.stdout).unwrap().trim()).unwrap();
    assert_eq!(p.role.as_deref(), Some("worker"));
    assert_eq!(
        p.agent_url.split(':').nth(1),
        Some("//127.0.0.1"),
        "Tailscale's address"
    );

    let (s, v) = get(&a, "/v1/llm");
    assert_eq!(s, 200);
    assert_eq!(v["installed"], false);
    assert_eq!(v["ready"], false);
    let (s, v) = post(&a, "/v1/llm/models/pull", json!({"model": "gemma3:1b"}));
    assert_eq!(s, 503, "{v}");
    let (s, _) = post(&a, "/v1/llm/start", json!({}));
    assert_eq!(s, 404);
    let (s, v) = post(
        &a,
        "/v1/llm/chat",
        json!({"model": "gemma3:1b", "messages": [{"role": "user", "content": "hi"}]}),
    );
    assert_eq!(s, 503, "{v}");
}

#[test]
fn background_pass_raises_and_sends_alerts() {
    let (ntfy_port, ntfy_seen) = fake_server(|_| (200, "application/json", "{}".into()));
    let a = start_with(&[("VERCELIFY_NOTIFY_URL", format!("http://127.0.0.1:{ntfy_port}/topic"))]);
    let (s, v) = post(&a, "/v1/alerts/check", json!({}));
    assert_eq!(s, 200, "{v}");
    let alerts = v["alerts"].as_array().unwrap();
    // The fixture is a laptop on battery with a single disk: both are warnings.
    let titles: Vec<&str> = alerts.iter().map(|a| a["title"].as_str().unwrap()).collect();
    assert!(titles.iter().any(|t| t.starts_with("Power: warning")), "{titles:?}");
    assert!(
        titles.iter().any(|t| t.starts_with("Storage disk: warning")),
        "{titles:?}"
    );
    assert!(alerts.iter().all(|a| a["notified"] == true), "{v}");
    let sent = ntfy_seen.lock().unwrap().len();
    assert_eq!(sent, alerts.len());
    // A second pass with nothing changed sends nothing new.
    let (_, v) = post(&a, "/v1/alerts/check", json!({}));
    assert_eq!(v["alerts"].as_array().unwrap().len(), alerts.len());
    assert_eq!(ntfy_seen.lock().unwrap().len(), sent);
    // and each pass recorded a metrics point
    let (_, m) = get(&a, "/v1/metrics");
    assert_eq!(m["points"].as_array().unwrap().len(), 2, "one per pass: {m}");
}

/// An open `GET /v1/events` connection; `wait_for` reads until a line
/// containing `needle` arrives.
struct EventStream {
    s: TcpStream,
    buf: String,
}

impl EventStream {
    fn open(a: &Agent) -> Self {
        let mut s = TcpStream::connect(("127.0.0.1", a.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        s.write_all(
            format!("GET /v1/events HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\n\r\n").as_bytes(),
        )
        .unwrap();
        let mut e = Self { s, buf: String::new() };
        let head = e.wait_for("\r\n\r\n");
        assert!(head.to_ascii_lowercase().contains("application/x-ndjson"), "{head}");
        e.wait_for("\"kind\":\"hello\"");
        e
    }

    fn wait_for(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if needle == "\r\n\r\n" {
                if let Some(i) = self.buf.find(needle) {
                    let head = self.buf[..i].to_string();
                    self.buf.drain(..i + 4);
                    return head;
                }
            } else if let Some(i) = self.buf.find(needle) {
                let start = self.buf[..i].rfind('{').unwrap_or(0);
                let start = self.buf[..start].rfind("\n").map_or(0, |n| n + 1);
                let end = self.buf[i..].find('\n').map_or(self.buf.len(), |n| i + n);
                let line = self.buf[start..end].to_string();
                self.buf.drain(..end);
                return line;
            }
            assert!(Instant::now() < deadline, "no event with {needle}; got: {}", self.buf);
            let mut chunk = [0u8; 4096];
            match self.s.read(&mut chunk) {
                Ok(0) => panic!("event stream closed; got: {}", self.buf),
                Ok(n) => self.buf.push_str(&String::from_utf8_lossy(&chunk[..n])),
                Err(_) => {}
            }
        }
    }
}

#[test]
fn events_jobs_power_guard_and_domain() {
    let a = start();
    let (_, h) = get(&a, "/v1/health");
    for cap in ["events", "jobs", "sleep", "power-guard", "domain"] {
        assert!(h["capabilities"].as_array().unwrap().iter().any(|c| c == cap), "{cap}");
    }
    // The stream needs the token like every other endpoint.
    let (s, _, _) = req(&a, "GET", "/v1/events", None, None);
    assert_eq!(s, 401);
    let mut ev = EventStream::open(&a);

    // Power guard: thresholds from the app, written for the guard, applied.
    write(&a.root, "systemd/vercelify-power-guard.service", "[Service]\n");
    let (s, st) = get(&a, "/v1/power-guard");
    assert_eq!(s, 200, "{st}");
    assert_eq!(st["customized"], false);
    assert_eq!(st["state"]["state"], "warning");
    let mut new = st["settings"].clone();
    new["thermal_pause_c"] = json!(85);
    let (s, st, _) = req(&a, "PUT", "/v1/power-guard", Some(TOKEN), Some(new.clone()));
    assert_eq!(s, 200, "{st}");
    assert_eq!(
        (st["customized"].clone(), st["settings"]["thermal_pause_c"].clone()),
        (json!(true), json!(85))
    );
    let local = std::fs::read_to_string(a.root.join("state/power-guard.local.conf")).unwrap();
    assert!(local.contains("THERMAL_PAUSE_C=85\n"), "{local}");
    assert!(std::fs::read_to_string(&a.log)
        .unwrap()
        .contains("systemctl restart vercelify-power-guard"));
    ev.wait_for("\"topic\":\"power-guard\"");
    // The doctor reads the same thresholds.
    let (_, d) = get(&a, "/v1/doctor");
    assert!(d.to_string().contains("85"), "{d}");
    new["thermal_resume_c"] = json!(90);
    let (s, v, _) = req(&a, "PUT", "/v1/power-guard", Some(TOKEN), Some(new));
    assert_eq!(s, 400, "{v}");
    let (s, st, _) = req(&a, "DELETE", "/v1/power-guard", Some(TOKEN), None);
    assert_eq!((s, st["customized"].clone()), (200, json!(false)));

    // A backup started from the app is a job, followed to its result.
    write(&a.root, "backup.env", "BACKUP_REPOS=/mnt/storage/backups/restic\n");
    write(&a.root, "systemd/vercelify-backup.service", "[Service]\n");
    let (s, v) = post(&a, "/v1/backup/run", json!({}));
    assert_eq!(s, 200, "{v}");
    // Only a result written after the start counts (the backup writes it).
    let fresh = chrono_now();
    write(
        &a.root,
        "state/backup.json",
        &format!("{{\"ok\":true,\"finished_at\":\"{fresh}\"}}"),
    );
    let line = ev.wait_for("\"kind\":\"backup\"");
    assert!(line.contains("\"status\":\"running\""), "{line}");
    let line = ev.wait_for("\"status\":\"ok\"");
    assert!(line.contains("backed up"), "{line}");
    let (_, jobs) = get(&a, "/v1/jobs");
    let job = &jobs[0];
    assert_eq!(
        (job["kind"].clone(), job["status"].clone()),
        (json!("backup"), json!("ok"))
    );
    let (s, one) = get(&a, &format!("/v1/jobs/{}", job["id"].as_str().unwrap()));
    assert_eq!((s, one["id"].clone()), (200, job["id"].clone()));
    assert_eq!(get(&a, "/v1/jobs/nope").0, 404);

    // Alerts arrive live.
    let (_, _) = post(&a, "/v1/alerts/check", json!({}));
    ev.wait_for("\"kind\":\"alert\"");

    // Domain: how apps are reached.
    write(
        &a.root,
        "install.env",
        "role=main\ndomain=example.com\ndomain_mode=cloudflare\n",
    );
    let (s, d) = get(&a, "/v1/domain");
    assert_eq!(s, 200, "{d}");
    assert_eq!(
        (d["mode"].clone(), d["domain"].clone(), d["tunnel_active"].clone()),
        (json!("cloudflare"), json!("example.com"), json!(false))
    );
    write(&a.root, "install.env", "role=main\n");
    assert_eq!(get(&a, "/v1/domain").1["mode"], "private");

    // Sleep needs the wake listener, which this agent runs without.
    let (s, v, _) = req(
        &a,
        "PUT",
        "/v1/sleep/app1",
        Some(TOKEN),
        Some(json!({ "enabled": true })),
    );
    assert_eq!(s, 409, "{v}");
    let (s, st) = get(&a, "/v1/sleep");
    assert_eq!((s, st["settings"].as_array().unwrap().len()), (200, 0));
}

fn chrono_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // RFC 3339 in UTC without a date library.
    let days = secs / 86_400;
    let (h, m, s) = ((secs % 86_400) / 3600, (secs % 3600) / 60, secs % 60);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if mo <= 2 { 1 } else { 0 };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn send(a: &Agent, method: &str, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
    let (s, v, _) = req(a, method, path, Some(token), body);
    (s, v)
}

#[test]
fn platform_api() {
    // A server not migrated yet: the platform API says how to get it.
    let coolify = start();
    let (s, h, _) = req(&coolify, "GET", "/v1/health", None, None);
    assert_eq!(s, 200);
    assert!(!h["capabilities"].as_array().unwrap().iter().any(|c| c == "platform"));
    let (s, v) = get(&coolify, "/v1/platform/apps");
    assert_eq!(s, 503);
    assert!(v["error"].as_str().unwrap().contains("vercelify migrate"), "{v}");
    drop(coolify);

    let a = start_with(&[("VERCELIFY_ENGINE", "platform".into())]);
    let (_, h, _) = req(&a, "GET", "/v1/health", None, None);
    let caps = h["capabilities"].as_array().unwrap();
    assert!(
        caps.iter().any(|c| c == "platform") && caps.iter().any(|c| c == "roles"),
        "{h}"
    );
    assert!(a.root.join("state/platform.db").exists());
    assert!(a.root.join("state/master.key").exists());

    // The main server registered itself.
    let (s, nodes) = get(&a, "/v1/platform/nodes");
    assert_eq!(s, 200);
    assert_eq!(nodes[0]["role"], "main");

    // Projects and apps.
    let (s, p) = post(&a, "/v1/platform/projects", json!({"name": "Blog"}));
    assert_eq!(s, 200, "{p}");
    assert_eq!(p["environments"][0]["name"], "production");
    let pid = p["id"].as_str().unwrap().to_string();
    let (s, app) = post(
        &a,
        "/v1/platform/apps",
        json!({"name": "blog", "kind": "git", "project_id": pid,
               "source": {"repository": "https://github.com/me/blog", "branch": "main"},
               "domains": ["blog.example.com"]}),
    );
    assert_eq!(s, 200, "{app}");
    assert_eq!(app["url"], "https://blog.example.com");
    assert_eq!(app["runtime"]["replicas"], 1);
    let (s, v) = post(
        &a,
        "/v1/platform/apps",
        json!({"name": "blog", "kind": "git",
        "source": {"repository": "https://github.com/me/blog"}}),
    );
    assert_eq!(s, 409, "{v}");
    let (s, v) = post(
        &a,
        "/v1/platform/apps",
        json!({"name": "x", "kind": "git",
        "source": {"repository": "file:///etc"}}),
    );
    assert_eq!(s, 400, "{v}");
    let (s, v) = send(
        &a,
        "PATCH",
        "/v1/platform/apps/blog",
        TOKEN,
        Some(json!({"runtime": {"port": 8080}})),
    );
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["runtime"]["port"], 8080);
    let (s, list) = get(&a, &format!("/v1/platform/apps?project={pid}"));
    assert_eq!((s, list.as_array().unwrap().len()), (200, 1));

    // Variables: sealed at rest, masked unless revealed.
    let (s, v) = send(
        &a,
        "PUT",
        "/v1/platform/apps/blog/env",
        TOKEN,
        Some(json!({"vars": [{"key": "DATABASE_URL", "value": "postgres://secret"}]})),
    );
    assert_eq!(s, 200, "{v}");
    let (s, v) = post(
        &a,
        "/v1/platform/apps/blog/env/import",
        json!({"content": "NODE_ENV=production\nA=1\n"}),
    );
    assert_eq!((s, &v["saved"]), (200, &json!(2)), "{v}");
    let (_, vars) = get(&a, "/v1/platform/apps/blog/env");
    assert_eq!(vars.as_array().unwrap().len(), 3);
    assert!(vars.as_array().unwrap().iter().all(|v| v["value"].is_null()));
    let (_, vars) = get(&a, "/v1/platform/apps/blog/env?reveal=true");
    let db = vars
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["key"] == "DATABASE_URL")
        .unwrap();
    assert_eq!(db["value"], "postgres://secret");
    let raw = std::fs::read(a.root.join("state/platform.db")).unwrap();
    assert!(
        !String::from_utf8_lossy(&raw).contains("postgres://secret"),
        "sealed on disk"
    );

    // Domains and volumes.
    let (s, d) = post(
        &a,
        "/v1/platform/apps/blog/domains",
        json!({"host": "www.example.com", "redirect_to": "blog.example.com"}),
    );
    assert_eq!(s, 200, "{d}");
    let (s, _) = send(
        &a,
        "DELETE",
        "/v1/platform/apps/blog/domains/www.example.com",
        TOKEN,
        None,
    );
    assert_eq!(s, 200);
    let (s, v) = post(
        &a,
        "/v1/platform/apps/blog/volumes",
        json!({"name": "data", "mount_path": "/data"}),
    );
    assert_eq!(s, 200, "{v}");
    let (s, v) = get(&a, "/v1/platform/apps/blog/deployments");
    assert_eq!((s, v.as_array().unwrap().len()), (200, 0));

    // Deploying queues a deployment (the queue itself waits for the proxy,
    // which the stub Docker never starts); it can be cancelled.
    let (s, d) = post(&a, "/v1/platform/apps/blog/deploy", json!({}));
    assert_eq!(s, 200, "{d}");
    assert_eq!(
        (d["status"].as_str(), d["number"].as_u64(), d["trigger"].as_str()),
        (Some("queued"), Some(1), Some("manual"))
    );
    let did = d["id"].as_str().unwrap().to_string();
    let (s, v) = post(&a, "/v1/platform/apps/blog/deploy", json!({"commit_sha": "not-a-sha"}));
    assert_eq!(s, 400, "{v}");
    let (s, v) = post(
        &a,
        "/v1/platform/apps/blog/rollback",
        json!({"deployment_id": "dpl_missing"}),
    );
    assert_eq!(s, 404, "{v}");
    let (s, v) = post(&a, &format!("/v1/platform/deployments/{did}/cancel"), json!({}));
    assert_eq!((s, v["status"].as_str()), (200, Some("cancelled")), "{v}");
    let (s, v) = post(&a, &format!("/v1/platform/deployments/{did}/cancel"), json!({}));
    assert_eq!(s, 409, "already cancelled: {v}");
    let (s, _, _) = req(
        &a,
        "GET",
        &format!("/v1/platform/deployments/{did}/logs"),
        Some(TOKEN),
        None,
    );
    assert_eq!(s, 200);
    let (s, v) = get(&a, "/v1/platform/apps/blog/previews");
    assert_eq!((s, v.as_array().unwrap().len()), (200, 0));

    // Roles: a deployer ships but doesn't delete; a viewer only reads.
    let (s, t) = post(&a, "/v1/tokens", json!({"name": "ci", "role": "deployer"}));
    assert_eq!(s, 200, "{t}");
    assert_eq!(t["token"]["role"], "deployer");
    let deployer = t["secret"].as_str().unwrap().to_string();
    let (_, t) = post(&a, "/v1/tokens", json!({"name": "dash", "role": "viewer"}));
    let viewer = t["secret"].as_str().unwrap().to_string();
    let (s, _) = send(
        &a,
        "PATCH",
        "/v1/platform/apps/blog",
        &deployer,
        Some(json!({"desired": "stopped"})),
    );
    assert_eq!(s, 200);
    let (s, v) = send(&a, "DELETE", "/v1/platform/apps/blog", &deployer, None);
    assert_eq!(s, 403, "{v}");
    assert!(v["error"].as_str().unwrap().contains("deployer"));
    let (s, _) = send(&a, "POST", "/v1/tokens", &deployer, Some(json!({"name": "escalate"})));
    assert_eq!(s, 403, "deployers can't mint tokens");
    let (s, _) = send(&a, "GET", "/v1/platform/apps/blog", &viewer, None);
    assert_eq!(s, 200);
    let (s, _) = send(&a, "GET", "/v1/platform/apps/blog/env?reveal=true", &viewer, None);
    assert_eq!(s, 403, "viewers can't reveal secrets");
    let (s, _) = send(
        &a,
        "PUT",
        "/v1/platform/apps/blog/env",
        &viewer,
        Some(json!({"vars": []})),
    );
    assert_eq!(s, 403);
    let audit = std::fs::read_to_string(a.root.join("state/audit.log")).unwrap();
    assert!(audit.contains("\"status\":403"), "refusals are audited");

    // Server domain: owners only.
    let (s, v) = send(
        &a,
        "PUT",
        "/v1/platform/settings/domain",
        TOKEN,
        Some(json!({"wildcard": "*.example.org", "https": true, "edge_tls": false})),
    );
    assert_eq!((s, v["wildcard"].as_str()), (200, Some("example.org")), "{v}");
    let (s, _) = send(
        &a,
        "PUT",
        "/v1/platform/settings/domain",
        &deployer,
        Some(json!({"wildcard": null, "https": false, "edge_tls": false})),
    );
    assert_eq!(s, 403);
    let (s, v) = get(&a, "/v1/platform/settings/domain");
    assert_eq!((s, v["https"].as_bool()), (200, Some(true)));

    // Git credentials (owners), auto-deploy state.
    let (s, g) = post(
        &a,
        "/v1/platform/git-sources",
        json!({"name": "gh", "provider": "github", "auth": "token", "token": "ghp_test"}),
    );
    assert_eq!(s, 200, "{g}");
    assert!(!g.to_string().contains("ghp_test"), "the token is never returned");
    let (s, _) = send(
        &a,
        "POST",
        "/v1/platform/git-sources",
        &deployer,
        Some(json!({"name": "x", "provider": "github", "auth": "none"})),
    );
    assert_eq!(s, 403);
    let (s, w) = get(&a, "/v1/platform/apps/blog/git");
    assert_eq!((s, w["enabled"].as_bool()), (200, Some(true)), "{w}");
    let (s, w) = send(
        &a,
        "PUT",
        "/v1/platform/apps/blog/git",
        &deployer,
        Some(json!({"enabled": false})),
    );
    assert_eq!((s, w["enabled"].as_bool()), (200, Some(false)), "{w}");

    // Basic Auth.
    let (s, v) = send(
        &a,
        "PUT",
        "/v1/platform/apps/blog/basic-auth",
        &deployer,
        Some(json!({"user": "me", "password": "short"})),
    );
    assert_eq!(s, 400, "{v}");
    let (s, v) = send(
        &a,
        "PUT",
        "/v1/platform/apps/blog/basic-auth",
        &deployer,
        Some(json!({"user": "me", "password": "long enough"})),
    );
    assert_eq!((s, v["protected"].as_bool()), (200, Some(true)), "{v}");

    // Scheduled tasks.
    let (s, v) = post(
        &a,
        "/v1/platform/apps/blog/tasks",
        json!({"name": "cleanup", "schedule": "not cron", "command": "true"}),
    );
    assert_eq!(s, 400, "{v}");
    let (s, t) = post(
        &a,
        "/v1/platform/apps/blog/tasks",
        json!({"name": "cleanup", "schedule": "*/15 * * * *", "command": "rm -rf /tmp/cache"}),
    );
    assert_eq!(s, 200, "{t}");
    // Runtime: an undeployed app has no logs, terminal or restart yet.
    for (m, path) in [
        ("GET", "/v1/platform/apps/blog/logs"),
        ("POST", "/v1/platform/apps/blog/restart"),
        ("POST", "/v1/platform/apps/blog/terminal"),
    ] {
        let (s, v) = send(&a, m, path, TOKEN, (m == "POST").then(|| json!({})));
        assert_eq!(s, 409, "{path}: {v}");
        assert!(v["error"].as_str().unwrap().contains("isn't deployed"), "{v}");
    }
    let (s, v) = send(&a, "GET", "/v1/platform/terminals/term_nope/output", TOKEN, None);
    assert_eq!(s, 404, "{v}");
    let (s, v) = send(
        &a,
        "POST",
        "/v1/platform/terminals/term_nope/input",
        &deployer,
        Some(json!({"data": "aGk="})),
    );
    assert_eq!(s, 404, "a deployer may type into its own terminals: {v}");

    // The catalogue: list, and an app from a template with its secrets.
    let (s, list) = get(&a, "/v1/platform/templates");
    assert_eq!(s, 200);
    assert!(list.as_array().unwrap().iter().any(|t| t["id"] == "umami"), "{list}");
    let (s, app) = send(
        &a,
        "POST",
        "/v1/platform/templates/uptime-kuma",
        &deployer,
        Some(json!({"name": "status"})),
    );
    assert_eq!(s, 200, "{app}");
    assert_eq!(app["kind"], "compose");
    assert_eq!(app["runtime"]["service"], "kuma");
    let (s, v) = post(&a, "/v1/platform/templates/nope", json!({}));
    assert_eq!(s, 404, "{v}");
    let (s, vars) = get(&a, "/v1/platform/apps/umami/env");
    assert_eq!(s, 404, "{vars}");

    // Every volume of the server, with its owner's name.
    let (s, vols) = get(&a, "/v1/platform/volumes");
    assert_eq!(s, 200, "{vols}");
    assert!(
        vols.as_array().unwrap().iter().all(|v| v["owner_name"].is_string()),
        "{vols}"
    );
    let (s, v) = send(
        &a,
        "POST",
        "/v1/platform/volumes/vol_nope/move",
        &deployer,
        Some(json!({"tier": "hdd"})),
    );
    assert_eq!(s, 403, "storage moves are an owner's: {v}");

    let (s, v) = post(&a, "/v1/platform/apps/blog/tasks/cleanup/run", json!({}));
    assert_eq!(
        (s, v["status"].as_str()),
        (200, Some("skipped: the app isn't deployed")),
        "{v}"
    );
    let (s, v) = get(&a, "/v1/platform/apps/blog/tasks");
    assert_eq!(v[0]["last_status"], "skipped: the app isn't deployed", "{s}");

    // Deploy hooks: a public URL whose secret is the credential.
    let (s, h) = post(&a, "/v1/platform/apps/blog/hooks", json!({}));
    assert_eq!(s, 200, "{h}");
    let secret = h["secret"].as_str().unwrap().to_string();
    let (s, v, _) = req(&a, "POST", &format!("/v1/hooks/deploy/{secret}"), None, None);
    assert_eq!((s, v["status"].as_str()), (200, Some("queued")), "{v}");
    let (s, _, _) = req(&a, "POST", "/v1/hooks/deploy/vfyh_wrong", None, None);
    assert_eq!(s, 404);
    let codes: Vec<u16> = (0..6)
        .map(|_| req(&a, "POST", &format!("/v1/hooks/deploy/{secret}"), None, None).0)
        .collect();
    assert_eq!(codes.last(), Some(&429), "rate limited: {codes:?}");
    let (_, hooks) = get(&a, "/v1/platform/apps/blog/hooks");
    assert!(hooks[0]["secret"].is_null() && hooks[0]["last_used_at"].is_string());
    let (s, _) = send(&a, "DELETE", "/v1/platform/apps/blog/previews/9", TOKEN, None);
    assert_eq!(s, 200, "removing a preview that isn't there is fine");

    // Databases: created (started in the background), passwords masked
    // unless revealed, linked to an app, protected while linked.
    let (s, d) = post(
        &a,
        "/v1/platform/databases",
        json!({"name": "blog-db", "kind": "postgresql", "link_app": "blog"}),
    );
    assert_eq!(s, 200, "{d}");
    assert_eq!(d["image"], "postgres:17-alpine");
    assert!(d["internal_url"].as_str().unwrap().contains("••••••"));
    let (s, v) = post(&a, "/v1/platform/databases", json!({"name": "blog", "kind": "redis"}));
    assert_eq!(s, 409, "names are shared with apps: {v}");
    let (s, v) = send(&a, "GET", "/v1/platform/databases/blog-db?reveal=true", &viewer, None);
    assert_eq!(s, 403, "{v}");
    let (s, v) = send(&a, "GET", "/v1/platform/databases/blog-db?reveal=true", &deployer, None);
    assert!(
        s == 200 && !v["internal_url"].as_str().unwrap().contains("••••••"),
        "{v}"
    );
    let (_, vars) = get(&a, "/v1/platform/apps/blog/env?reveal=true");
    let url = vars
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["key"] == "DATABASE_URL")
        .unwrap();
    assert_eq!(url["value"], "${{db.blog-db.url}}");
    let (s, v) = send(&a, "DELETE", "/v1/platform/databases/blog-db", TOKEN, None);
    assert_eq!(s, 409, "in use by blog: {v}");
    let (s, v) = send(
        &a,
        "POST",
        "/v1/platform/databases/blog-db/restore",
        &deployer,
        Some(json!({"snapshot": "abc"})),
    );
    assert_eq!(s, 403, "{v}");
    let (s, v) = post(&a, "/v1/platform/databases/blog-db/backups", json!({}));
    assert_eq!(s, 409, "backups not set up: {v}");
    let (s, v) = send(
        &a,
        "PATCH",
        "/v1/platform/databases/blog-db",
        TOKEN,
        Some(json!({"backup_schedule": "nope"})),
    );
    assert_eq!(s, 400, "{v}");
    let (s, v) = send(&a, "DELETE", "/v1/platform/apps/blog/env/DATABASE_URL", TOKEN, None);
    assert_eq!(s, 200, "{v}");
    let (s, v) = send(&a, "DELETE", "/v1/platform/databases/blog-db", TOKEN, None);
    assert_eq!((s, v["data_kept"].as_bool()), (200, Some(true)), "{v}");

    // Delete.
    let (s, _) = send(&a, "DELETE", "/v1/platform/apps/blog", TOKEN, None);
    assert_eq!(s, 200);
    let (s, _) = get(&a, "/v1/platform/apps/blog");
    assert_eq!(s, 404);
    let (s, _) = send(&a, "DELETE", &format!("/v1/platform/projects/{pid}"), TOKEN, None);
    assert_eq!(s, 200);

    // The doctor reports the platform database.
    let (_, doc) = get(&a, "/v1/doctor");
    let check = doc["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "platform")
        .unwrap();
    assert_eq!(check["status"], "ok", "{check}");
}

/// The mesh coordinator: invitations, a sealed join (the daemon's own code),
/// single-use tokens, node tokens only from the node's own mesh address,
/// maps pushed on revocation, and the coordinator's own node.
#[test]
fn mesh_coordinator() {
    let control = free_port();
    let a = start_with(&[
        ("VERCELIFY_MESH_COORDINATOR", "1".into()),
        ("VERCELIFY_MESH_CONTROL_LISTEN", format!("127.0.0.1:{control}")),
    ]);
    let (_, h, _) = req(&a, "GET", "/v1/health", None, None);
    assert!(h["capabilities"].as_array().unwrap().iter().any(|c| c == "mesh"), "{h}");
    // The coordinator is node 1, and its own daemon's state is written.
    let mesh_dir = a.root.join("state/mesh");
    let own: Value = serde_json::from_str(&std::fs::read_to_string(mesh_dir.join("state.json")).unwrap()).unwrap();
    assert_eq!(own["ipv4"], "10.77.0.1");
    assert_eq!(own["api_url"], format!("http://127.0.0.1:{}", a.port));

    // Owners invite; deployers can't.
    let (s, t) = post(&a, "/v1/tokens", json!({"name": "ci", "role": "deployer"}));
    assert_eq!(s, 200, "{t}");
    let deployer = t["secret"].as_str().unwrap().to_string();
    let (s, _) = send(&a, "POST", "/v1/mesh/invites", &deployer, Some(json!({})));
    assert_eq!(s, 403);
    let (s, inv) = post(&a, "/v1/mesh/invites", json!({"kind": "server"}));
    assert_eq!(s, 200, "{inv}");
    let token = inv["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("vfyj1."));
    let jt = vercelify_mesh::proto::JoinToken::decode(&token).unwrap();
    assert_eq!(jt.control, vec![format!("127.0.0.1:{control}")]);

    // A machine joins with it.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let node_dir = tempfile::tempdir().unwrap();
    let st = rt
        .block_on(vercelify_mesh::daemon::join(
            node_dir.path(),
            &token,
            "pi5",
            vec!["192.168.1.31:51820".into()],
        ))
        .expect("joined");
    assert_eq!((st.name.as_str(), st.ipv4.as_str()), ("pi5", "10.77.0.2"));
    assert_eq!(st.api_url, format!("http://10.77.0.1:{}", a.port));
    let vk = vercelify_mesh::keys::VerifyingKey::from_base64(&st.coordinator_key).unwrap();
    let map = vercelify_mesh::proto::PeerMap::verify(&vk, st.map.as_ref().unwrap()).unwrap();
    assert_eq!(map.peers.len(), 1);
    assert_eq!(map.peers[0].ipv4, "10.77.0.1", "the coordinator is its first peer");
    assert_eq!(map.dns.records.get("pi5").map(String::as_str), Some("10.77.0.2"));

    // The relay: pi5 and the coordinator's own node reach each other through
    // it, each authenticated by its WireGuard key; a stranger is refused.
    let relay_url = format!("ws://127.0.0.1:{control}/v1/mesh/relay");
    assert!(map.relays.contains(&relay_url), "{:?}", map.relays);
    {
        use std::sync::atomic::Ordering;
        use std::sync::Arc;
        use vercelify_mesh::keys::{NodeKey, PublicKey};
        use vercelify_mesh::relay_client::{spawn, RelayConfig};
        use vercelify_mesh::state::key_path;
        let coordinator = PublicKey::from_base64(own["coordinator"]["public_key"].as_str().unwrap()).unwrap();
        let pi_key = NodeKey::load_or_create(&key_path(node_dir.path())).unwrap();
        let main_key = NodeKey::load_or_create(&key_path(&mesh_dir)).unwrap();
        let (pi_pub, main_pub) = (pi_key.public(), main_key.public());
        rt.block_on(async {
            let (urls, urls_rx) = tokio::sync::watch::channel(vec![relay_url.clone()]);
            let cfg = |key: NodeKey| RelayConfig {
                urls: urls_rx.clone(),
                key: tokio::sync::watch::channel(Arc::new(key)).1,
                coordinator,
            };
            let mut pi = spawn(cfg(pi_key));
            let mut main = spawn(cfg(main_key));
            let stranger = spawn(cfg(NodeKey::generate()));
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while !(pi.up.load(Ordering::Relaxed) && main.up.load(Ordering::Relaxed)) {
                assert!(tokio::time::Instant::now() < deadline, "the relay didn't connect");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            pi.tx.send((main_pub, b"through the relay".to_vec())).await.unwrap();
            let (src, data) = tokio::time::timeout(Duration::from_secs(5), main.rx.recv())
                .await
                .expect("relayed in time")
                .unwrap();
            assert_eq!((src, data.as_slice()), (pi_pub, &b"through the relay"[..]));
            main.tx.send((pi_pub, b"and back".to_vec())).await.unwrap();
            let (src, _) = tokio::time::timeout(Duration::from_secs(5), pi.rx.recv())
                .await
                .expect("relayed back")
                .unwrap();
            assert_eq!(src, main_pub);
            assert!(!stranger.up.load(Ordering::Relaxed), "an unknown key was let in");
            drop(urls);
        });
        let (_, nodes) = get(&a, "/v1/mesh/nodes");
        let pi = nodes.as_array().unwrap().iter().find(|n| n["name"] == "pi5").unwrap();
        assert!(pi["relayed_bytes"].as_u64().unwrap() >= 17, "{pi}");
    }

    // Single use.
    let other = tempfile::tempdir().unwrap();
    let err = rt
        .block_on(vercelify_mesh::daemon::join(other.path(), &token, "pi6", vec![]))
        .unwrap_err();
    assert!(err.to_string().contains("used already"), "{err}");

    let (s, nodes) = get(&a, "/v1/mesh/nodes");
    assert_eq!(s, 200);
    let names: Vec<&str> = nodes
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2, "{nodes}");
    assert!(names.contains(&"pi5"));
    let pi = nodes.as_array().unwrap().iter().find(|n| n["name"] == "pi5").unwrap();
    assert_eq!(pi["endpoints"], json!(["192.168.1.31:51820"]));
    assert!(pi["cert_expires_at"].is_string());

    // A node token works only from the node's own mesh address.
    let (s, v, _) = req(
        &a,
        "POST",
        "/v1/mesh/endpoints",
        Some(&st.node_token),
        Some(json!({"endpoints": []})),
    );
    assert_eq!(s, 403, "{v}");
    assert!(v["error"].as_str().unwrap().contains("own mesh address"), "{v}");
    let (s, _, _) = req(&a, "GET", "/v1/mesh/map", Some("vfyn_nope"), None);
    assert_eq!(s, 401);

    // The coordinator's own daemon (from this machine) gets its map stream,
    // and a new map when a node is revoked.
    let own_token = own["node_token"].as_str().unwrap().to_string();
    let mut sock = TcpStream::connect(("127.0.0.1", a.port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    sock.write_all(
        format!("GET /v1/mesh/map HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {own_token}\r\n\r\n").as_bytes(),
    )
    .unwrap();
    let mut seen = String::new();
    let mut buf = [0u8; 65536];
    fn read_until(sock: &mut TcpStream, buf: &mut [u8], seen: &mut String, maps: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while seen.matches("\"type\":\"map\"").count() < maps {
            assert!(std::time::Instant::now() < deadline, "stream so far: {seen}");
            let n = sock.read(buf).unwrap();
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }
    read_until(&mut sock, &mut buf, &mut seen, 1);
    assert!(seen.starts_with("HTTP/1.1 200"), "{seen}");
    let (s, v) = send(&a, "DELETE", "/v1/mesh/nodes/pi5", TOKEN, None);
    assert_eq!(s, 200, "{v}");
    read_until(&mut sock, &mut buf, &mut seen, 2);
    let last = seen
        .lines()
        .rfind(|l| l.contains("\"type\":\"map\""))
        .unwrap()
        .to_string();
    let ev: Value = serde_json::from_str(last.trim()).unwrap();
    let m = vercelify_mesh::proto::PeerMap::verify(&vk, ev["signed"].as_str().unwrap()).unwrap();
    assert!(m.peers.is_empty(), "the revoked node left every map: {m:?}");
    // …and its token no longer works anywhere.
    let (s, _, _) = req(&a, "GET", "/v1/mesh/map", Some(&st.node_token), None);
    assert_eq!(s, 401);
    let (s, status) = get(&a, "/v1/mesh/status");
    assert_eq!(s, 200);
    assert_eq!(status["coordinator"]["nodes"], 1);
    assert_eq!(status["coordinator"]["revoked"], 1);
}

// --- GitHub App and webhooks (design §11) ----------------------------------------

static APP_PEM: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static APP_PUB: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Checks the JWT's RS256 signature with the app's public key, like GitHub.
fn jwt_valid(headers: &str) -> bool {
    use base64::Engine as _;
    let Some(t) = headers.lines().find_map(|l| {
        l.strip_prefix("Authorization: Bearer ")
            .or_else(|| l.strip_prefix("authorization: Bearer "))
    }) else {
        return false;
    };
    let parts: Vec<&str> = t.trim().split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let claims: Value = serde_json::from_slice(&b64.decode(parts[1]).unwrap_or_default()).unwrap_or_default();
    if claims["iss"] != "7" {
        return false;
    }
    let dir = tempfile::tempdir().unwrap();
    let sig = dir.path().join("sig");
    std::fs::write(&sig, b64.decode(parts[2]).unwrap_or_default()).unwrap();
    let mut child = Command::new("openssl")
        .args(["dgst", "-sha256", "-verify"])
        .arg(APP_PUB.get().unwrap())
        .arg("-signature")
        .arg(&sig)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{}.{}", parts[0], parts[1]).as_bytes())
        .unwrap();
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).contains("Verified OK")
}

fn fake_github_app(r: &Seen) -> (u16, &'static str, String) {
    let j = "application/json";
    match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/app-manifests/c0de/conversions") => (
            201,
            j,
            json!({"id": 7, "slug": "vercelify-test", "pem": APP_PEM.get().unwrap(), "webhook_secret": "whs3cret",
                   "client_id": "Iv1.c", "client_secret": "cs", "html_url": "https://github.com/apps/vercelify-test",
                   "owner": {"login": "me"}})
            .to_string(),
        ),
        ("POST", p) if p.starts_with("/app-manifests/") => (404, j, r#"{"message":"Not Found"}"#.into()),
        ("GET", "/repos/me/app/installation") if jwt_valid(&r.headers) => (200, j, r#"{"id": 99}"#.into()),
        ("POST", "/app/installations/99/access_tokens") if jwt_valid(&r.headers) => (
            201,
            j,
            r#"{"token": "ghs_installation", "expires_at": "2099-01-01T00:00:00Z"}"#.into(),
        ),
        ("GET", p) if p.starts_with("/repos/me/app/pulls") => (200, j, "[]".into()),
        _ => (401, j, r#"{"message":"Bad credentials"}"#.into()),
    }
}

fn hmac(key: &[u8], msg: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut k = [0u8; 64];
    k[..key.len()].copy_from_slice(key);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new().chain_update(&ipad).chain_update(msg).finalize();
    let out = Sha256::new().chain_update(&opad).chain_update(inner).finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

/// A request with headers of our own to `port` (the control listener).
fn raw(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    s.write_all(format!("{head}\r\n{body}").as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (h, b) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    (
        h.split_whitespace().nth(1).unwrap_or("0").parse().unwrap_or(0),
        b.to_string(),
    )
}

#[test]
fn github_app_and_webhooks() {
    if Command::new("openssl").arg("version").output().is_err() {
        return;
    }
    let keys = tempfile::tempdir().unwrap();
    let pem = keys.path().join("app.pem");
    let ok = Command::new("openssl")
        .args(["genrsa", "-traditional", "-out"])
        .arg(&pem)
        .arg("2048")
        .output()
        .is_ok_and(|o| o.status.success())
        || Command::new("openssl")
            .args(["genrsa", "-out"])
            .arg(&pem)
            .arg("2048")
            .output()
            .is_ok_and(|o| o.status.success());
    assert!(ok, "openssl genrsa");
    let public = keys.path().join("app.pub");
    assert!(Command::new("openssl")
        .args(["rsa", "-pubout", "-in"])
        .arg(&pem)
        .arg("-out")
        .arg(&public)
        .output()
        .unwrap()
        .status
        .success());
    APP_PEM.set(std::fs::read_to_string(&pem).unwrap()).unwrap();
    APP_PUB.set(public).unwrap();
    let (gh, seen) = fake_server(fake_github_app);
    let control = free_port();
    let a = start_with(&[
        ("VERCELIFY_ENGINE", "platform".into()),
        ("VERCELIFY_ROLE", "main".into()),
        ("VERCELIFY_MESH_COORDINATOR", "1".into()),
        ("VERCELIFY_MESH_CONTROL_LISTEN", format!("127.0.0.1:{control}")),
        ("VERCELIFY_MESH_PUBLIC_URL", format!("http://127.0.0.1:{control}")),
        ("VERCELIFY_GITHUB_API", format!("http://127.0.0.1:{gh}")),
    ]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", control)).is_err() {
        assert!(Instant::now() < deadline, "no control listener");
        std::thread::sleep(Duration::from_millis(50));
    }

    // The manifest flow: a start page that posts to GitHub, GitHub's
    // callback with a code, the app stored as a git source.
    let (s, st) = post(&a, "/v1/platform/github-app", json!({}));
    assert_eq!(s, 200, "{st}");
    let url = st["start_url"].as_str().unwrap();
    let path = url.split_once(&format!(":{control}")).unwrap().1;
    let (s, page) = raw(control, "GET", path, &[], "");
    assert_eq!(s, 200);
    assert!(page.contains("https://github.com/settings/apps/new?state="), "{page}");
    assert!(page.contains("&quot;default_events&quot;"), "{page}");
    let state = st["state"].as_str().unwrap();
    let (s, page) = raw(
        control,
        "GET",
        &format!("/v1/github-app/callback?code=bad&state={state}"),
        &[],
        "",
    );
    assert_eq!(s, 200);
    assert!(page.contains("not created"), "{page}");
    let (s, page) = raw(
        control,
        "GET",
        &format!("/v1/github-app/callback?code=c0de&state={state}"),
        &[],
        "",
    );
    assert_eq!(s, 200);
    assert!(
        page.contains("GitHub App created") && page.contains("vercelify-test/installations/new"),
        "{page}"
    );
    let (s, again) = raw(
        control,
        "GET",
        &format!("/v1/github-app/callback?code=c0de&state={state}"),
        &[],
        "",
    );
    assert!(s == 200 && again.contains("expired or was used"), "one use: {again}");
    let (_, sources) = get(&a, "/v1/platform/git-sources");
    let src = &sources[0];
    assert_eq!(
        (src["auth"].as_str(), src["webhook"].as_bool()),
        (Some("github_app"), Some(true))
    );
    let src_id = src["id"].as_str().unwrap().to_string();

    // An app on it, followed.
    let (s, app) = post(
        &a,
        "/v1/platform/apps",
        json!({"name": "site", "kind": "git",
               "source": {"repository": "https://github.com/me/app", "branch": "main", "git_source_id": src_id}}),
    );
    assert_eq!(s, 200, "{app}");
    let (s, v) = send(
        &a,
        "PUT",
        "/v1/platform/apps/site/git",
        TOKEN,
        Some(json!({"enabled": true})),
    );
    assert_eq!(s, 200, "{v}");

    // A push, signed with the app's webhook secret: the app is checked, with
    // an installation token minted by a JWT GitHub accepts.
    let body = r#"{"ref":"refs/heads/main","repository":{"clone_url":"https://github.com/me/app.git"}}"#;
    let sig = format!("sha256={}", hmac(b"whs3cret", body.as_bytes()));
    let (s, out) = raw(
        control,
        "POST",
        "/v1/webhooks/github",
        &[
            ("X-GitHub-Event", "push"),
            ("X-Hub-Signature-256", &sig),
            ("Content-Type", "application/json"),
        ],
        body,
    );
    assert_eq!(s, 202, "{out}");
    assert!(out.contains("checking 1 app"), "{out}");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let paths: Vec<String> = seen.lock().unwrap().iter().map(|r| r.path.clone()).collect();
        if paths.iter().any(|p| p == "/app/installations/99/access_tokens") {
            break;
        }
        assert!(Instant::now() < deadline, "no installation token was minted: {paths:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while !std::fs::read_to_string(&a.log)
        .unwrap_or_default()
        .contains("ls-remote")
    {
        assert!(Instant::now() < deadline, "the poller didn't look");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Bad signatures, pings, and other events.
    let (s, _) = raw(
        control,
        "POST",
        "/v1/webhooks/github",
        &[("X-GitHub-Event", "push"), ("X-Hub-Signature-256", "sha256=00")],
        body,
    );
    assert_eq!(s, 401);
    let ping = r#"{"zen":"hi"}"#;
    let sig = format!("sha256={}", hmac(b"whs3cret", ping.as_bytes()));
    let (s, out) = raw(
        control,
        "POST",
        "/v1/webhooks/github",
        &[("X-GitHub-Event", "ping"), ("X-Hub-Signature-256", &sig)],
        ping,
    );
    assert_eq!((s, out.contains("pong")), (200, true), "{out}");

    // A token source gets a webhook of its own.
    let (s, g) = post(
        &a,
        "/v1/platform/git-sources",
        json!({"name": "gitea", "provider": "gitea", "auth": "token", "token": "tok"}),
    );
    assert_eq!(s, 200, "{g}");
    let gid = g["id"].as_str().unwrap();
    let (s, w) = post(&a, &format!("/v1/platform/git-sources/{gid}/webhook"), json!({}));
    assert_eq!(s, 200, "{w}");
    assert_eq!(w["url"], format!("http://127.0.0.1:{control}/v1/webhooks/gitea/{gid}"));
    assert_eq!(w["secret"].as_str().unwrap().len(), 40);
    let body = r#"{"repository":{"clone_url":"https://git.example.com/me/other.git"}}"#;
    let sig = hmac(w["secret"].as_str().unwrap().as_bytes(), body.as_bytes());
    let (s, out) = raw(
        control,
        "POST",
        &format!("/v1/webhooks/gitea/{gid}"),
        &[("X-Gitea-Event", "push"), ("X-Gitea-Signature", &sig)],
        body,
    );
    assert_eq!(s, 202, "{out}");
    assert!(out.contains("checking 0 app"), "{out}");
    // Without a token: refused.
    let (s, _) = send(&a, "POST", "/v1/platform/github-app", "nope", Some(json!({})));
    assert_eq!(s, 401);
}

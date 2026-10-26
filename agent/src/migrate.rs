//! `vercelify-agent migrate`: moving a server from Coolify to the platform
//! engine (design §18). `host/lib/migrate.sh` (`sudo vercelify migrate`)
//! runs these steps around restarting the agent:
//!
//! - `plan`: what would happen; changes nothing.
//! - `apply`: the records (one transaction), Coolify's images tagged as
//!   ours, Coolify stopped (containers kept, restart policy remembered),
//!   then volume data copied into ours (Coolify's stays as it was).
//! - `check`: after the agent restarted on the platform engine, waits for
//!   the first deployments and asks every domain through our proxy.
//! - `rollback`: our containers, copies and records removed, Coolify
//!   started again as it was.
//! - `finish`: Coolify's containers and images removed (`--purge`: its
//!   volumes and /data/coolify too).
//!
//! The steps keep their progress in `<state_dir>/migrate/state.json`, so a
//! step interrupted halfway can be run again or rolled back.

use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use vercelify_platform::import::{self, Adopt, Export, Imported, Plan, VolumeCopy};
use vercelify_platform::secrets::MasterKey;
use vercelify_platform::Store;

const EXPORT_PHP: &str = include_str!("migrate_export.php");
/// Coolify's own containers (its apps and databases carry `coolify.managed`).
const STACK: &[&str] = &[
    "coolify",
    "coolify-realtime",
    "coolify-sentinel",
    "coolify-proxy",
    "coolify-redis",
    "coolify-db",
];

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Plan { json: bool },
    Apply { force: bool },
    Check { wait_secs: u64, json: bool },
    Rollback,
    Finish { purge: bool },
    Status { json: bool },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Records written; Docker steps under way.
    Importing,
    /// Coolify stopped, data copied: the platform engine takes over.
    Imported,
    /// `check` saw every app answer.
    Switched,
    RolledBack,
    Finished,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Stopped {
    pub container: String,
    pub restart: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub phase: Phase,
    pub started_at: String,
    pub coolify_version: String,
    pub imported: Imported,
    /// Coolify's containers we stopped, and their restart policy.
    #[serde(default)]
    pub stopped: Vec<Stopped>,
    /// Images tagged for the first deployments.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Our volumes filled with Coolify's data.
    #[serde(default)]
    pub copied: Vec<String>,
    #[serde(default)]
    pub acme: bool,
    #[serde(default)]
    pub notes: Vec<String>,
}

fn state_file(cfg: &Config) -> PathBuf {
    cfg.state_dir.join("migrate").join("state.json")
}

fn load(cfg: &Config) -> Option<State> {
    serde_json::from_str(&std::fs::read_to_string(state_file(cfg)).ok()?).ok()
}

fn save(cfg: &Config, s: &State) -> Result<(), String> {
    let p = state_file(cfg);
    std::fs::create_dir_all(p.parent().unwrap_or(Path::new("/"))).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

/// The configuration the platform engine will run with (its proxy, not
/// Coolify's).
fn platform_cfg(cfg: &Config) -> Config {
    let mut c = cfg.clone();
    c.engine = "platform".into();
    c.apply_engine_defaults();
    c
}

fn docker(cfg: &Config, args: &[&str]) -> Result<String, String> {
    let o = Command::new(&cfg.docker_bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", cfg.docker_bin))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(format!(
            "docker {}: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&o.stderr).trim()
        ))
    }
}

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Everything Coolify knows, from its own container (secrets decrypted).
fn export(cfg: &Config) -> Result<Export, String> {
    let mut child = Command::new(&cfg.docker_bin)
        .args(["exec", "-i", &cfg.coolify_container, "php"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", cfg.docker_bin))?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(EXPORT_PHP.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut out = String::new();
    child
        .stdout
        .take()
        .ok_or("no stdout")?
        .read_to_string(&mut out)
        .map_err(|e| e.to_string())?;
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(if err.contains("No such container") || err.contains("is not running") {
            format!(
                "Coolify isn't running ({}): start it first (it must run to be read), or there is nothing to migrate",
                cfg.coolify_container
            )
        } else {
            format!("reading Coolify failed: {}", err.trim())
        });
    }
    serde_json::from_str(&out).map_err(|e| {
        format!(
            "Coolify's export isn't readable ({e}): {}",
            out.chars().take(300).collect::<String>()
        )
    })
}

fn open(cfg: &Config) -> Result<(Store, MasterKey), String> {
    let key = MasterKey::load_or_create(&cfg.master_key_file).map_err(|e| format!("master key: {e}"))?;
    let store = Store::open(&cfg.platform_db).map_err(|e| e.to_string())?;
    Ok((store, key))
}

fn print_plan(p: &Plan) {
    println!("Coolify {} → Vercelify\n", p.coolify_version);
    for (name, new) in &p.projects {
        println!("  project {name}{}", if *new { "" } else { " (exists here, used)" });
    }
    for i in &p.items {
        let mark = match i.action {
            import::Action::Create => "+",
            import::Action::Skip => "-",
        };
        let renamed = if i.name != i.coolify_name {
            format!(" (was {})", i.coolify_name)
        } else {
            String::new()
        };
        println!(
            "  {mark} {} {}{renamed} — {} [{} / {}]",
            i.kind, i.name, i.summary, i.project, i.environment
        );
        for n in &i.notes {
            println!("      • {n}");
        }
    }
    for w in &p.warnings {
        println!("\n  ! {w}");
    }
    println!(
        "\n  {} to take over, {} left with Coolify",
        p.creates(),
        p.items.len() - p.creates()
    );
}

pub fn run(cfg: &Config, step: Step) -> Result<(), String> {
    match step {
        Step::Plan { json } => {
            let e = export(cfg)?;
            let (store, _) = open(cfg)?;
            let p = store.with(|c| import::plan(c, &e)).map_err(|e| e.to_string())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&p).map_err(|e| e.to_string())?);
            } else {
                print_plan(&p);
            }
            Ok(())
        }
        Step::Apply { force } => apply(cfg, force),
        Step::Check { wait_secs, json } => check(cfg, Duration::from_secs(wait_secs), json),
        Step::Rollback => rollback(cfg),
        Step::Finish { purge } => finish(cfg, purge),
        Step::Status { json } => {
            let s = load(cfg).ok_or("no migration on this server")?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s).map_err(|e| e.to_string())?);
            } else {
                println!(
                    "{:?} (started {}, Coolify {}): {} apps, {} databases",
                    s.phase,
                    s.started_at,
                    s.coolify_version,
                    s.imported.apps.len(),
                    s.imported.databases.len()
                );
            }
            Ok(())
        }
    }
}

fn apply(cfg: &Config, force: bool) -> Result<(), String> {
    match load(cfg).map(|s| s.phase) {
        Some(Phase::Importing) => {
            return Err("an earlier migration stopped halfway: sudo vercelify migrate --rollback, then again".into())
        }
        Some(Phase::Imported | Phase::Switched) => return Err("this server is migrated already".into()),
        Some(Phase::Finished) => return Err("this server was migrated and Coolify removed".into()),
        Some(Phase::RolledBack) | None => {}
    }
    let e = export(cfg)?;
    let (store, key) = open(cfg)?;
    let plan = store.with(|c| import::plan(c, &e)).map_err(|e| e.to_string())?;
    let left: Vec<&str> = plan
        .items
        .iter()
        .filter(|i| i.action == import::Action::Skip && !i.notes.iter().any(|n| n.contains("runs on ")))
        .map(|i| i.coolify_name.as_str())
        .collect();
    if !left.is_empty() && !force {
        return Err(format!(
            "{} can't be taken over and would stop with Coolify: see `sudo vercelify migrate`; --force to go ahead anyway",
            left.join(", ")
        ));
    }
    if plan.creates() == 0 {
        println!("Coolify runs nothing to take over here.");
    }
    let valid = |s: &str| crate::cron::parse(s).map(|_| ());
    let imported = store
        .tx(|c| import::apply(c, &key, &e, &plan, &valid))
        .map_err(|e| e.to_string())?;
    let mut st = State {
        phase: Phase::Importing,
        started_at: vercelify_platform::now(),
        coolify_version: plan.coolify_version.clone(),
        imported,
        stopped: vec![],
        tags: vec![],
        copied: vec![],
        acme: false,
        notes: vec![],
    };
    save(cfg, &st)?;
    println!(
        "  ✓ {} apps and {} databases written",
        st.imported.apps.len(),
        st.imported.databases.len()
    );

    // Coolify's images, under our names: the first deployments start them
    // without building.
    for a in &st.imported.apps.clone() {
        if let Adopt::Built { tag } = &a.adopt {
            match coolify_image(cfg, &a.coolify) {
                Some(img) => {
                    docker(cfg, &["tag", &img, tag])?;
                    st.tags.push(tag.clone());
                }
                None => st.notes.push(format!(
                    "{}: Coolify kept no image of it; it is built from its repository",
                    a.name
                )),
            }
        }
    }
    save(cfg, &st)?;

    // Coolify stops (Coolify itself first, so it restarts nothing).
    let mut names: Vec<String> = STACK
        .iter()
        .filter(|n| **n == "coolify")
        .map(|n| n.to_string())
        .collect();
    names.extend(lines(&docker(
        cfg,
        &[
            "ps",
            "-a",
            "--filter",
            "label=coolify.managed=true",
            "--format",
            "{{.Names}}",
        ],
    )?));
    names.extend(STACK.iter().filter(|n| **n != "coolify").map(|n| n.to_string()));
    names.dedup();
    for n in names {
        let Ok(policy) = docker(cfg, &["inspect", "-f", "{{.HostConfig.RestartPolicy.Name}}", &n]) else {
            continue;
        };
        if st.stopped.iter().any(|s| s.container == n) {
            continue;
        }
        st.stopped.push(Stopped {
            container: n.clone(),
            restart: policy,
        });
        save(cfg, &st)?;
        docker(cfg, &["update", "--restart=no", &n])?;
        let _ = docker(cfg, &["stop", "-t", "20", &n]);
    }
    println!("  ✓ Coolify stopped ({} containers, kept)", st.stopped.len());

    // Data, copied while nothing writes to it.
    let mut copies: Vec<VolumeCopy> = vec![];
    for d in &st.imported.databases {
        copies.extend(d.volume.clone());
    }
    for a in &st.imported.apps {
        copies.extend(a.volumes.clone());
        if let Adopt::Compose { coolify_project } = &a.adopt {
            copies.extend(compose_volumes(cfg, coolify_project, &a.name)?);
        }
    }
    for v in copies {
        copy_volume(cfg, &v, &mut st)?;
        save(cfg, &st)?;
    }
    println!("  ✓ {} volumes copied", st.copied.len());

    // Certificates: no new ones to ask Let's Encrypt for.
    st.acme = take_certificates(cfg).unwrap_or_else(|e| {
        st.notes
            .push(format!("certificates weren't taken over ({e}): they are issued again"));
        false
    });
    st.phase = Phase::Imported;
    save(cfg, &st)?;
    for n in &st.notes {
        println!("  • {n}");
    }
    Ok(())
}

/// The image Coolify runs (or last built) for an app.
fn coolify_image(cfg: &Config, uuid: &str) -> Option<String> {
    let running = docker(
        cfg,
        &[
            "ps",
            "-a",
            "--filter",
            &format!("label=coolify.applicationUuid={uuid}"),
            "--filter",
            "label=coolify.pullRequestId=0",
            "--format",
            "{{.Image}}",
        ],
    )
    .ok()?;
    if let Some(i) = lines(&running).into_iter().next() {
        return Some(i);
    }
    let built = docker(cfg, &["images", uuid, "--format", "{{.Repository}}:{{.Tag}}"]).ok()?;
    lines(&built).into_iter().find(|i| !i.ends_with(":<none>"))
}

/// A Coolify compose project's volumes, as our compose project's.
fn compose_volumes(cfg: &Config, coolify_project: &str, app: &str) -> Result<Vec<VolumeCopy>, String> {
    let ours = vercelify_platform::compose::project_name(app, None);
    let mut out = vec![];
    let listed = docker(
        cfg,
        &[
            "volume",
            "ls",
            "--filter",
            &format!("label=com.docker.compose.project={coolify_project}"),
            "--format",
            "{{.Name}}\t{{.Label \"com.docker.compose.volume\"}}",
        ],
    )?;
    for l in lines(&listed) {
        let (name, key) = l.split_once('\t').unwrap_or((&l, ""));
        // Coolify's rendered compose keys its volumes `<uuid>_<name>` too.
        let key = if key.is_empty() { name } else { key };
        let key = key.strip_prefix(&format!("{coolify_project}_")).unwrap_or(key);
        out.push(VolumeCopy {
            from: name.to_string(),
            to: format!("{ours}_{key}"),
        });
    }
    // Coolify names service volumes `<uuid>_<name>` without compose labels.
    for name in lines(&docker(cfg, &["volume", "ls", "-q"])?) {
        if let Some(key) = name.strip_prefix(&format!("{coolify_project}_")) {
            if !out.iter().any(|v| v.from == name) {
                out.push(VolumeCopy {
                    from: name.clone(),
                    to: format!("{ours}_{key}"),
                });
            }
        }
    }
    Ok(out)
}

fn mountpoint(cfg: &Config, volume: &str) -> Option<PathBuf> {
    docker(cfg, &["volume", "inspect", "-f", "{{.Mountpoint}}", volume])
        .ok()
        .map(PathBuf::from)
}

fn copy_volume(cfg: &Config, v: &VolumeCopy, st: &mut State) -> Result<(), String> {
    if st.copied.contains(&v.to) {
        return Ok(());
    }
    let Some(from) = mountpoint(cfg, &v.from) else {
        st.notes.push(format!(
            "Coolify's volume {} doesn't exist: {} starts empty",
            v.from, v.to
        ));
        return Ok(());
    };
    if let Some(to) = mountpoint(cfg, &v.to) {
        if std::fs::read_dir(&to).map(|mut d| d.next().is_some()).unwrap_or(false) {
            return Err(format!(
                "the volume {} exists here already and isn't empty: not overwritten",
                v.to
            ));
        }
    }
    let labels: Vec<String> = match v.to.split_once('_') {
        Some((project, key)) if project.starts_with("vfy-") && !v.to.starts_with("vfy-db-") => vec![
            format!("com.docker.compose.project={project}"),
            format!("com.docker.compose.volume={key}"),
        ],
        _ => vec![],
    };
    let mut args = vec!["volume", "create"];
    for l in &labels {
        args.push("--label");
        args.push(l);
    }
    args.push("--label");
    args.push("vercelify.from=coolify");
    args.push(&v.to);
    docker(cfg, &args)?;
    st.copied.push(v.to.clone());
    let to = mountpoint(cfg, &v.to).ok_or_else(|| format!("volume {} has no mountpoint", v.to))?;
    let src = format!("{}/.", from.display());
    let o = Command::new("cp")
        .args(["-a", &src])
        .arg(&to)
        .output()
        .map_err(|e| format!("cp: {e}"))?;
    if !o.status.success() {
        return Err(format!(
            "copying {} to {}: {}",
            v.from,
            v.to,
            String::from_utf8_lossy(&o.stderr).trim()
        ));
    }
    Ok(())
}

/// Coolify's Let's Encrypt certificates, for our proxy (its resolver
/// renamed to ours). Never over certificates of our own.
fn take_certificates(cfg: &Config) -> Result<bool, String> {
    let src = cfg.proxy_dir.join("acme.json");
    // Traefik's placeholder before its first certificate is empty.
    let Some(raw) = std::fs::read_to_string(&src).ok().filter(|r| !r.trim().is_empty()) else {
        return Ok(false);
    };
    let dst = platform_cfg(cfg).proxy_dir.join("acme").join("acme.json");
    if dst.exists() {
        return Ok(false);
    }
    let mut v: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let mut out = serde_json::Map::new();
    // Coolify's resolver is `letsencrypt`; ours the platform's own.
    if let Some(le) = v.remove("letsencrypt").or_else(|| v.into_iter().next().map(|(_, x)| x)) {
        out.insert(vercelify_platform::proxy::RESOLVER.to_string(), le);
    }
    std::fs::create_dir_all(dst.parent().unwrap_or(Path::new("/"))).map_err(|e| e.to_string())?;
    let tmp = dst.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new();
    f.write(true).create(true).truncate(true);
    std::os::unix::fs::OpenOptionsExt::mode(&mut f, 0o600);
    f.open(&tmp)
        .and_then(|mut h| h.write_all(serde_json::to_string(&out).unwrap_or_default().as_bytes()))
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
    Ok(true)
}

#[derive(Serialize)]
struct AppCheck {
    name: String,
    status: String,
    error: Option<String>,
    hosts: Vec<(String, Option<u16>)>,
}

/// One request through our proxy: its status code.
fn ask(port: u16, host: &str) -> Option<u16> {
    let mut s = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(3)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    write!(
        s,
        "GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: vercelify-migrate\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut buf = [0u8; 32];
    let n = s.read(&mut buf).ok()?;
    let head = String::from_utf8_lossy(&buf[..n]);
    head.split_whitespace().nth(1)?.parse().ok()
}

fn check(cfg: &Config, wait: Duration, json: bool) -> Result<(), String> {
    let mut st = load(cfg).ok_or("no migration on this server")?;
    if !matches!(st.phase, Phase::Imported | Phase::Switched) {
        return Err(format!("the migration is {:?}: nothing to check", st.phase));
    }
    let (store, _) = open(cfg)?;
    let pc = platform_cfg(cfg);
    let start = Instant::now();
    let end = start + wait;
    let results = loop {
        let mut done = true;
        let mut out = vec![];
        for a in &st.imported.apps {
            let (status, error) = match &a.deployment {
                None => ("stopped".to_string(), None),
                Some(d) => match store.with(|c| vercelify_platform::deployments::get(c, d)) {
                    Ok(d) => (d.status.as_str().to_string(), Some(d.error).filter(|e| !e.is_empty())),
                    Err(e) => ("unknown".to_string(), Some(e.to_string())),
                },
            };
            if matches!(status.as_str(), "queued" | "building" | "deploying") {
                done = false;
            }
            let hosts = a
                .hosts
                .iter()
                .map(|h| {
                    (
                        h.clone(),
                        (status == "healthy").then(|| ask(pc.proxy_http_port, h)).flatten(),
                    )
                })
                .collect::<Vec<_>>();
            // Our proxy answers 404 until it has read the new routes; an
            // app's own 404 at `/` is accepted once that had time to happen.
            let settled = start.elapsed() > Duration::from_secs(30);
            if status == "healthy"
                && hosts
                    .iter()
                    .any(|(_, c)| c.is_none_or(|c| c >= 500 || c == 404 && !settled))
            {
                done = false;
            }
            out.push(AppCheck {
                name: a.name.clone(),
                status,
                error,
                hosts,
            });
        }
        if done || Instant::now() >= end {
            break out;
        }
        std::thread::sleep(Duration::from_secs(2));
    };
    let ok = results.iter().all(|r| {
        r.status == "stopped" || r.status == "healthy" && r.hosts.iter().all(|(_, c)| c.is_some_and(|c| c < 500))
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&results).map_err(|e| e.to_string())?);
    } else {
        for r in &results {
            let mark = if r.status == "healthy" {
                "✓"
            } else if r.status == "stopped" {
                "•"
            } else {
                "✗"
            };
            println!(
                "  {mark} {} — {}{}",
                r.name,
                r.status,
                r.error.as_deref().map(|e| format!(": {e}")).unwrap_or_default()
            );
            for (h, c) in &r.hosts {
                println!(
                    "      {h}: {}",
                    c.map(|c| c.to_string()).unwrap_or_else(|| "no answer".into())
                );
            }
        }
    }
    if ok {
        st.phase = Phase::Switched;
        save(cfg, &st)?;
        Ok(())
    } else {
        Err("not every app answers yet: fix them from the app (or sudo vercelify migrate --rollback)".into())
    }
}

fn rollback(cfg: &Config) -> Result<(), String> {
    let mut st = load(cfg).ok_or("no migration on this server")?;
    match st.phase {
        Phase::Finished => return Err("Coolify was removed (migrate --finish): there is nothing to go back to".into()),
        Phase::RolledBack => return Err("rolled back already".into()),
        _ => {}
    }
    let (store, key) = open(cfg)?;
    // Our containers of the migrated apps and databases, and our proxy:
    // the ports go back to Coolify's.
    let mut ours = vec![];
    for a in &st.imported.apps {
        ours.extend(lines(&docker(
            cfg,
            &["ps", "-aq", "--filter", &format!("label=vercelify.app={}", a.id)],
        )?));
    }
    for d in &st.imported.databases {
        ours.extend(lines(&docker(
            cfg,
            &["ps", "-aq", "--filter", &format!("label=vercelify.database={}", d.id)],
        )?));
    }
    ours.push(vercelify_platform::proxy::CONTAINER.to_string());
    for c in &ours {
        let _ = docker(cfg, &["rm", "-f", c]);
    }
    for a in &st.imported.apps {
        if matches!(a.adopt, Adopt::Compose { .. }) {
            let project = vercelify_platform::compose::project_name(&a.name, None);
            for c in lines(&docker(
                cfg,
                &[
                    "ps",
                    "-aq",
                    "--filter",
                    &format!("label=com.docker.compose.project={project}"),
                ],
            )?) {
                let _ = docker(cfg, &["rm", "-f", &c]);
            }
        }
    }
    store
        .tx(|c| import::remove(c, &key, &st.imported))
        .map_err(|e| e.to_string())?;
    for v in &st.copied {
        let _ = docker(cfg, &["volume", "rm", v]);
    }
    for t in &st.tags {
        let _ = docker(cfg, &["rmi", t]);
    }
    if st.acme {
        let _ = std::fs::remove_file(platform_cfg(cfg).proxy_dir.join("acme").join("acme.json"));
    }
    // Coolify as it was: its database first, Coolify last.
    let mut order = st.stopped.clone();
    order.sort_by_key(|s| match s.container.as_str() {
        "coolify-db" | "coolify-redis" => 0,
        "coolify" => 2,
        _ => 1,
    });
    let mut failed = vec![];
    for s in &order {
        let policy = if s.restart.is_empty() { "no" } else { s.restart.as_str() };
        let _ = docker(cfg, &["update", &format!("--restart={policy}"), &s.container]);
        if let Err(e) = docker(cfg, &["start", &s.container]) {
            failed.push(format!("{}: {e}", s.container));
        }
    }
    st.phase = Phase::RolledBack;
    st.imported = Imported::default();
    st.copied.clear();
    st.tags.clear();
    st.stopped.clear();
    save(cfg, &st)?;
    if failed.is_empty() {
        println!("  ✓ Coolify runs again, as before the migration");
        Ok(())
    } else {
        Err(format!(
            "Coolify is back, but these didn't start: {}",
            failed.join("; ")
        ))
    }
}

fn finish(cfg: &Config, purge: bool) -> Result<(), String> {
    let mut st = load(cfg).ok_or("no migration on this server")?;
    if st.phase != Phase::Switched {
        return Err(format!(
            "the migration is {:?}: run `sudo vercelify migrate --check` until every app answers first",
            st.phase
        ));
    }
    let mut volumes = vec![];
    for s in &st.stopped {
        if purge {
            if let Ok(v) = docker(
                cfg,
                &["inspect", "-f", "{{range .Mounts}}{{.Name}} {{end}}", &s.container],
            ) {
                volumes.extend(v.split_whitespace().map(str::to_string));
            }
        }
        let _ = docker(cfg, &["rm", "-f", &s.container]);
    }
    let images = docker(cfg, &["images", "--format", "{{.Repository}}:{{.Tag}}"]).unwrap_or_default();
    for img in lines(&images) {
        let coolify = img.contains("coollabsio/")
            || st
                .imported
                .apps
                .iter()
                .any(|a| img.starts_with(&format!("{}:", a.coolify)));
        if coolify {
            let _ = docker(cfg, &["rmi", &img]);
        }
    }
    let _ = docker(cfg, &["network", "rm", "coolify"]);
    if purge {
        volumes.sort();
        volumes.dedup();
        for v in volumes.iter().filter(|v| !st.copied.contains(v)) {
            let _ = docker(cfg, &["volume", "rm", v]);
        }
        let _ = std::fs::remove_dir_all(&cfg.coolify_data_dir);
    }
    st.phase = Phase::Finished;
    st.stopped.clear();
    save(cfg, &st)?;
    println!(
        "  ✓ Coolify removed{}",
        if purge {
            ", with its data"
        } else {
            " (its volumes and /data/coolify are kept: --purge removes them)"
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            state_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        assert!(load(&cfg).is_none());
        let s = State {
            phase: Phase::Imported,
            started_at: "now".into(),
            coolify_version: "4.4.6".into(),
            imported: Imported::default(),
            stopped: vec![Stopped {
                container: "coolify".into(),
                restart: "always".into(),
            }],
            tags: vec![],
            copied: vec![],
            acme: false,
            notes: vec![],
        };
        save(&cfg, &s).unwrap();
        assert_eq!(load(&cfg), Some(s));
    }

    #[test]
    fn certificates_keep_their_resolver_and_never_replace_ours() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config {
            state_dir: dir.path().join("state"),
            proxy_dir: dir.path().join("coolify-proxy"),
            ..Config::default()
        };
        cfg.engine = "coolify".into();
        assert!(!take_certificates(&cfg).unwrap(), "no Coolify certificates");
        std::fs::create_dir_all(&cfg.proxy_dir).unwrap();
        std::fs::write(
            cfg.proxy_dir.join("acme.json"),
            r#"{"letsencrypt": {"Account": {"Email": "a@b.c"}, "Certificates": [{"domain": {"main": "x.test"}}]}}"#,
        )
        .unwrap();
        let ours = platform_cfg(&cfg).proxy_dir.join("acme").join("acme.json");
        assert!(take_certificates(&cfg).unwrap());
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&ours).unwrap()).unwrap();
        assert_eq!(
            v[vercelify_platform::proxy::RESOLVER]["Certificates"][0]["domain"]["main"],
            "x.test"
        );
        assert!(!take_certificates(&cfg).unwrap(), "ours exist now: kept");
    }
}

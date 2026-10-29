//! Running databases (design §10): start, keep running, stop, move between
//! storage tiers, back up and restore.
//!
//! A database container is recreated from its template whenever its spec
//! changes (version, public port): the data lives in its volume, so that is
//! as safe as a restart. Backups stream a dump straight from the container
//! into restic (`restic backup --stdin`), so no plain copy ever touches disk.

use crate::databases::{self, BackupStyle, Credentials};
use crate::docker::Docker;
use crate::engine::{Engine, EngineEvent};
use crate::health::{self, Probe};
use crate::ids::new_id;
use crate::proc::{self, Spec};
use crate::volumes;
use crate::{now, Error, Result};
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tokio::process::Command;
use vercelify_core::platform::{Database, DatabaseBackup, DatabaseKind, Desired, VolumeOwner};
use vercelify_core::StorageTier;

/// restic settings (`/etc/vercelify/backup.env`): RESTIC_PASSWORD,
/// BACKUP_REPOS and any S3/B2 credentials.
#[derive(Debug, Clone, Default)]
pub struct Restic {
    pub bin: String,
    pub env: BTreeMap<String, String>,
}

impl Restic {
    pub fn configured(&self) -> bool {
        self.env.contains_key("RESTIC_PASSWORD") && self.repo().is_some()
    }

    /// The first repository (the local one the installer made).
    pub fn repo(&self) -> Option<String> {
        self.env
            .get("BACKUP_REPOS")
            .and_then(|r| r.split_whitespace().next().map(String::from))
            .or_else(|| self.env.get("RESTIC_REPOSITORY").cloned())
    }

    fn command_env(&self) -> Vec<(String, String)> {
        let mut e: Vec<(String, String)> = self
            .env
            .iter()
            .filter(|(k, _)| k.as_str() != "BACKUP_REPOS")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Some(r) = self.repo() {
            e.push(("RESTIC_REPOSITORY".into(), r));
        }
        e
    }
}

/// A hash of what a database container is made of: recreate when it changes.
fn spec_hash(d: &Database, mesh_ip: Option<&str>, hdd: Option<&str>) -> String {
    let h = Sha256::digest(format!(
        "{}|{:?}|{}|{:?}|{:?}",
        d.image, d.public_port, d.public_bind, mesh_ip, hdd
    ));
    h.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// (spec, hash) of a database's container.
fn container_spec(engine: &Engine, d: &Database, cred: &Credentials) -> Result<(crate::docker::ContainerSpec, String)> {
    let vols = engine
        .store
        .with(|c| volumes::checked_list(c, VolumeOwner::Database, &d.id))?;
    let vol = vols
        .first()
        .ok_or_else(|| Error::Internal(format!("{} has no volume", d.name)))?;
    let storage_name = volumes::physical_name(vol, &format!("db-{}", d.name), None);
    let hdd = vols.first().and_then(|v| match v.tier {
        StorageTier::Hdd => Some(
            v.host_path
                .clone()
                .unwrap_or_else(|| engine.deployer.hdd_root.join(&storage_name).to_string_lossy().into()),
        ),
        StorageTier::Ssd => None,
    });
    if let Some(p) = &hdd {
        std::fs::create_dir_all(p)
            .map_err(|e| Error::Unavailable(format!("can't create {p} on the storage disk: {e}")))?;
    }
    let ip = engine.private_ip();
    let hash = spec_hash(d, ip.as_deref(), Some(hdd.as_deref().unwrap_or(&storage_name)));
    let mut spec = databases::spec(d, cred, ip.as_deref(), hdd);
    if let Some(mount) = spec.mounts.first_mut() {
        if vol.tier == StorageTier::Ssd {
            mount.source = storage_name;
        }
    }
    spec.labels.insert("vercelify.spec".into(), hash.clone());
    Ok((spec, hash))
}

async fn wait_ready(docker: &Docker, d: &Database, timeout: Duration) -> Result<()> {
    let probe = match databases::health_command(d.kind) {
        Some(c) => Probe::Command(c),
        None => Probe::Tcp,
    };
    health::wait_healthy(
        health::Wait {
            docker,
            container: &d.container,
            network: &crate::deploy::project_network(&d.project_id),
            port: databases::template(d.kind).port,
            probe,
            timeout,
            interval: Duration::from_millis(500),
        },
        &mut |_| {},
    )
    .await
}

impl Engine {
    fn database(&self, id: &str) -> Result<(Database, Credentials)> {
        self.store.with(|c| {
            let d = databases::get(c, &self.key, id, false)?;
            let cred = databases::credentials(c, &self.key, &d.id)?;
            Ok((d, cred))
        })
    }

    /// Make the database's container match its record: create (or recreate
    /// after a change), start, wait until it accepts connections.
    pub async fn ensure_database(&self, id: &str) -> Result<Database> {
        let (d, _) = self.database(id)?;
        let _operation = self.database_operation(&d.id).await;
        self.ensure_database_inner(&d.id).await
    }

    async fn ensure_database_inner(&self, id: &str) -> Result<Database> {
        let (d, cred) = self.database(id)?;
        let docker = &self.deployer.docker;
        if d.desired == Desired::Stopped {
            docker.stop(&d.container, 30).await?;
            return Ok(d);
        }
        let (spec, hash) = container_spec(self, &d, &cred)?;
        docker.ensure_network(&spec.networks[0]).await?;
        let current = docker.inspect(&d.container).await?;
        let same = current.is_some() && docker.label(&d.container, "vercelify.spec").await.unwrap_or_default() == hash;
        if !same {
            if current.is_some() {
                // Same volume, new container: like a restart.
                docker.stop(&d.container, 60).await?;
                docker.remove(&d.container).await?;
            }
            if !docker.image_exists(&d.image).await? {
                docker.pull(&d.image).await?;
            }
            docker.create(&spec).await?;
        }
        let running = docker.inspect(&d.container).await?.is_some_and(|i| i.running);
        if !running {
            docker.start(&d.container).await?;
        }
        wait_ready(docker, &d, Duration::from_secs(180)).await?;
        self.emit(EngineEvent::Changed);
        Ok(d)
    }

    /// One reconcile pass over databases: start what should run, recreate
    /// what changed or is missing, stop what is stopped.
    pub async fn reconcile_databases(&self) -> Vec<String> {
        let list = match self.store.with(|c| databases::list(c, &self.key, None, false)) {
            Ok(l) => l,
            Err(e) => return vec![format!("databases: {e}")],
        };
        let mut errors = vec![];
        for d in list {
            let state = self.deployer.docker.inspect(&d.container).await.ok().flatten();
            let ok = match (&state, d.desired) {
                // The power guard froze it (never databases, but be safe).
                (Some(i), _) if i.paused => true,
                (Some(i), Desired::Running) => {
                    i.running
                        && self
                            .deployer
                            .docker
                            .label(&d.container, "vercelify.spec")
                            .await
                            .unwrap_or_default()
                            == self
                                .database(&d.id)
                                .and_then(|(d, c)| container_spec(self, &d, &c))
                                .map(|(_, h)| h)
                                .unwrap_or_default()
                }
                (Some(i), _) => !i.running,
                (None, Desired::Stopped) => true,
                (None, _) => false,
            };
            if !ok {
                // A first pull can take many minutes on home internet: it runs on
                // its own, so this pass still brings every other app and database back.
                if d.desired == Desired::Running && !self.deployer.docker.image_exists(&d.image).await.unwrap_or(true) {
                    pull_in_background(&self.deployer.docker, &d.image);
                    continue;
                }
                if let Err(e) = self.ensure_database(&d.id).await {
                    errors.push(format!("{}: {e}", d.name));
                }
            }
        }
        errors
    }

    /// Remove a database's container (and with `purge`, its data).
    pub async fn remove_database(
        &self,
        d: &Database,
        purge: bool,
        hdd_path: Option<String>,
        storage_name: &str,
    ) -> Result<()> {
        let _operation = self.database_operation(&d.id).await;
        let docker = &self.deployer.docker;
        docker.stop(&d.container, 30).await?;
        docker.remove(&d.container).await?;
        if purge {
            docker.volume_remove(storage_name).await?;
            let p = hdd_path.unwrap_or_else(|| self.deployer.hdd_root.join(storage_name).to_string_lossy().into());
            let _ = std::fs::remove_dir_all(p);
        }
        self.emit(EngineEvent::Changed);
        Ok(())
    }

    /// Move a database's data to the other storage tier: stop it, copy with
    /// owners and permissions kept, switch the volume record, start it again.
    pub async fn move_database(&self, id: &str, tier: StorageTier, docker_root: &Path) -> Result<Database> {
        let (d, _) = self.database(id)?;
        let _operation = self.database_operation(&d.id).await;
        let (d, _) = self.database(&d.id)?;
        let vol = self
            .store
            .with(|c| volumes::checked_list(c, VolumeOwner::Database, &d.id))?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Internal(format!("{} has no volume", d.name)))?;
        if vol.tier == tier {
            return Ok(d);
        }
        let storage_name = volumes::physical_name(&vol, &format!("db-{}", d.name), None);
        let named = docker_root.join(&storage_name).join("_data");
        let hdd = self.deployer.hdd_root.join(&storage_name);
        let (from, to) = match tier {
            StorageTier::Hdd => (named.clone(), hdd.clone()),
            StorageTier::Ssd => (
                vol.host_path.clone().map(PathBuf::from).unwrap_or(hdd.clone()),
                named.clone(),
            ),
        };
        if tier == StorageTier::Ssd {
            // The named volume must exist to receive the data.
            let _ = proc::run(Spec {
                bin: &self.deployer.docker.bin,
                args: &proc::args(&["volume", "create", &storage_name]),
                env: &[],
                cwd: None,
                timeout: Duration::from_secs(30),
            })
            .await?;
        }
        self.deployer.docker.stop(&d.container, 60).await?;
        let copied = copy_tree(&from, &to).await;
        if let Err(e) = copied {
            // Back as it was.
            let _ = self.ensure_database_inner(&d.id).await;
            return Err(e);
        }
        let host_path = (tier == StorageTier::Hdd).then(|| to.to_string_lossy().into_owned());
        self.store
            .with(|c| volumes::set_tier(c, &vol.id, tier, host_path.as_deref()))?;
        self.ensure_database_inner(&d.id).await
    }

    /// Back up one database into the restic repository.
    pub async fn backup_database(&self, id: &str, restic: &Restic) -> Result<DatabaseBackup> {
        if !restic.configured() {
            return Err(Error::Unavailable(
                "backups aren't set up on this server (sudo vercelify backup setup)".into(),
            ));
        }
        let (d, _) = self.database(id)?;
        let _operation = self.database_operation(&d.id).await;
        let (d, _) = self.database(&d.id)?;
        let bid = new_id("bak");
        let t = now();
        self.store.with(|c| {
            c.execute(
                "INSERT INTO database_backups (id, database_id, status, created_at) VALUES (?1, ?2, 'running', ?3)",
                params![bid, d.id, t],
            )?;
            Ok(())
        })?;
        let res = self.backup_stream(&d, restic).await;
        let (status, bytes, snapshot, error) = match &res {
            Ok((bytes, snap)) => ("succeeded", *bytes as i64, snap.clone(), String::new()),
            Err(e) => ("failed", 0, String::new(), e.to_string()),
        };
        self.store.with(|c| {
            c.execute(
                "UPDATE database_backups SET status = ?2, bytes = ?3, snapshot = ?4, error = ?5, finished_at = ?6
                 WHERE id = ?1",
                params![bid, status, bytes, snapshot, error, now()],
            )?;
            Ok(())
        })?;
        res?;
        self.store.with(|c| backup(c, &bid))
    }

    async fn backup_stream(&self, d: &Database, restic: &Restic) -> Result<(u64, String)> {
        let docker = &self.deployer.docker.bin;
        let style = databases::template(d.kind).backup;
        let (producer, file): (Vec<String>, String) = match style {
            BackupStyle::Dump => {
                let mut a = proc::args(&["exec", &d.container]);
                a.extend(databases::dump_command(d.kind).unwrap_or_default());
                (a, format!("{}.dump", d.name))
            }
            BackupStyle::Snapshot | BackupStyle::Files => {
                if style == BackupStyle::Snapshot {
                    // A consistent snapshot on disk first. Dragonfly's image
                    // has no CLI: ask from a throwaway Valkey CLI container on
                    // the project network (password via the environment).
                    let (save, env): (Vec<String>, Vec<(String, String)>) = if d.kind == DatabaseKind::Dragonfly {
                        let (_, cred) = self.database(&d.id)?;
                        (
                            proc::args(&[
                                "run",
                                "--rm",
                                "--network",
                                &crate::deploy::project_network(&d.project_id),
                                "-e",
                                "REDISCLI_AUTH",
                                &databases::image(
                                    DatabaseKind::Redis,
                                    databases::template(DatabaseKind::Redis).default_version,
                                ),
                                "valkey-cli",
                                "-h",
                                &d.name,
                                "SAVE",
                            ]),
                            vec![("REDISCLI_AUTH".to_string(), cred.password)],
                        )
                    } else {
                        (
                            proc::args(&["exec", &d.container, "sh", "-c", save_script(d.kind)]),
                            vec![],
                        )
                    };
                    let o = proc::run(Spec {
                        bin: docker,
                        args: &save,
                        env: &env,
                        cwd: None,
                        timeout: Duration::from_secs(300),
                    })
                    .await?;
                    if !o.success() {
                        return Err(Error::Internal(format!("SAVE failed: {}", o.tail(2))));
                    }
                }
                let data = databases::template(d.kind).data_path;
                (
                    proc::args(&["exec", &d.container, "tar", "-C", data, "-cf", "-", "."]),
                    format!("{}.tar", d.name),
                )
            }
        };
        let restic_args = proc::args(&[
            "backup",
            "--stdin",
            "--stdin-filename",
            &file,
            "--tag",
            &format!("db:{}", d.name),
            "--tag",
            "vercelify-db",
            "--json",
        ]);
        let out = pipe(
            docker,
            &producer,
            &restic.bin,
            &restic_args,
            &restic.command_env(),
            Duration::from_secs(3 * 3600),
        )
        .await?;
        // restic --json: the last line is the summary.
        let summary = out
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["message_type"] == "summary")
            .ok_or_else(|| Error::Internal("restic printed no summary".into()))?;
        Ok((
            summary["total_bytes_processed"].as_u64().unwrap_or(0),
            summary["snapshot_id"].as_str().unwrap_or("").to_string(),
        ))
    }

    /// Restore a backup (by snapshot) over the database's current data.
    pub async fn restore_database(&self, id: &str, snapshot: &str, restic: &Restic) -> Result<()> {
        if !restic.configured() {
            return Err(Error::Unavailable("backups aren't set up on this server".into()));
        }
        if snapshot.is_empty() || snapshot.len() > 64 || !snapshot.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Invalid(format!("`{snapshot}` is not a snapshot id")));
        }
        let (d, _) = self.database(id)?;
        let _operation = self.database_operation(&d.id).await;
        let (d, _) = self.database(&d.id)?;
        let docker = &self.deployer.docker;
        let style = databases::template(d.kind).backup;
        let file = match style {
            BackupStyle::Dump => format!("{}.dump", d.name),
            _ => format!("{}.tar", d.name),
        };
        let dump = proc::args(&["dump", snapshot, &format!("/{file}")]);
        // A missing/incomplete snapshot must never reach a consumer that
        // deletes live files. Stage on disk (bounded memory), check the entire
        // producer result, and validate file archives before stopping anything.
        std::fs::create_dir_all(&self.builder.work_dir)?;
        let staged = tempfile::Builder::new()
            .prefix("restore-")
            .tempdir_in(&self.builder.work_dir)?;
        let archive = staged.path().join("backup");
        pipe(
            &restic.bin,
            &dump,
            "sh",
            &proc::args(&[
                "-c",
                "umask 077; cat > \"$1\"",
                "stage-restore",
                &archive.to_string_lossy(),
            ]),
            &restic.command_env(),
            Duration::from_secs(3 * 3600),
        )
        .await?;
        if style != BackupStyle::Dump {
            let checked = proc::run(Spec {
                bin: "sh",
                args: &proc::args(&[
                    "-c",
                    "tar -tf \"$1\" >/dev/null",
                    "check-restore",
                    &archive.to_string_lossy(),
                ]),
                env: &[],
                cwd: None,
                timeout: Duration::from_secs(180),
            })
            .await?;
            if !checked.success() {
                return Err(Error::Invalid(
                    "the backup archive is damaged; the database was not changed".into(),
                ));
            }
        }
        let staged_args = proc::args(&[&archive.to_string_lossy()]);
        match style {
            BackupStyle::Dump => {
                self.ensure_database_inner(&d.id).await?;
                let mut consumer = proc::args(&["exec", "-i", &d.container]);
                consumer.extend(databases::restore_command(d.kind).unwrap_or_default());
                pipe(
                    "cat",
                    &staged_args,
                    &docker.bin,
                    &consumer,
                    &[],
                    Duration::from_secs(3 * 3600),
                )
                .await?;
            }
            BackupStyle::Snapshot | BackupStyle::Files => {
                // Files are replaced while the database is stopped.
                docker.stop(&d.container, 60).await?;
                let data = databases::template(d.kind).data_path;
                let consumer = proc::args(&[
                    "run",
                    "--rm",
                    "-i",
                    "--volumes-from",
                    &d.container,
                    "--entrypoint",
                    "sh",
                    &d.image,
                    "-c",
                    &format!("cd {data} && find . -mindepth 1 -delete && tar -xf -"),
                ]);
                let res = pipe(
                    "cat",
                    &staged_args,
                    &docker.bin,
                    &consumer,
                    &[],
                    Duration::from_secs(3 * 3600),
                )
                .await;
                self.ensure_database_inner(&d.id).await?;
                res?;
            }
        }
        self.emit(EngineEvent::Changed);
        Ok(())
    }
}

/// Ask a Redis-like store to write its data to disk now.
fn save_script(kind: DatabaseKind) -> &'static str {
    use DatabaseKind::*;
    match kind {
        Keydb => "REDISCLI_AUTH=\"$VFY_PASSWORD\" keydb-cli SAVE | grep -q OK",
        _ => "REDISCLI_AUTH=\"$VFY_PASSWORD\" \"$(command -v valkey-cli || echo redis-cli)\" SAVE | grep -q OK",
    }
}

pub fn backup(c: &rusqlite::Connection, id: &str) -> Result<DatabaseBackup> {
    Ok(c.query_row("SELECT * FROM database_backups WHERE id = ?1", [id], |r| {
        Ok(DatabaseBackup {
            id: r.get("id")?,
            database_id: r.get("database_id")?,
            status: r.get("status")?,
            bytes: r.get::<_, i64>("bytes")? as u64,
            snapshot: r.get("snapshot")?,
            error: r.get("error")?,
            created_at: r.get("created_at")?,
            finished_at: r.get("finished_at")?,
        })
    })?)
}

pub fn backups(c: &rusqlite::Connection, database_id: &str) -> Result<Vec<DatabaseBackup>> {
    let mut st =
        c.prepare("SELECT id FROM database_backups WHERE database_id = ?1 ORDER BY created_at DESC LIMIT 50")?;
    let ids: Vec<String> = st
        .query_map([database_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    ids.iter().map(|id| backup(c, id)).collect()
}

/// `a | b`: stdout of the first into stdin of the second; both must
/// succeed. Returns the second's stdout. `env` goes to both (restic needs
/// it, docker ignores it).
async fn pipe(
    a_bin: &str,
    a_args: &[String],
    b_bin: &str,
    b_args: &[String],
    env: &[(String, String)],
    timeout: Duration,
) -> Result<String> {
    let spawn = |bin: &str, args: &[String], stdin: Stdio| {
        let mut c = Command::new(bin);
        c.args(args)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in env {
            c.env(k, v);
        }
        c.spawn().map_err(|e| Error::Unavailable(format!("{bin}: {e}")))
    };
    let mut a = spawn(a_bin, a_args, Stdio::null())?;
    let mut a_out = a.stdout.take().expect("piped");
    let mut b = spawn(b_bin, b_args, Stdio::piped())?;
    let mut b_in = b.stdin.take().expect("piped");
    let copy = async move {
        let r = tokio::io::copy(&mut a_out, &mut b_in).await;
        drop(b_in);
        r
    };
    let work = async {
        let (copied, a_res, b_res) = tokio::join!(copy, a.wait_with_output(), b.wait_with_output());
        (copied, a_res, b_res)
    };
    let (copied, a_res, b_res) = tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| Error::Unavailable("the backup took too long".into()))?;
    let a_out = a_res.map_err(|e| Error::Internal(format!("{a_bin}: {e}")))?;
    let b_out = b_res.map_err(|e| Error::Internal(format!("{b_bin}: {e}")))?;
    let tail = |o: &std::process::Output| {
        String::from_utf8_lossy(&o.stderr)
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string()
    };
    if !a_out.status.success() {
        return Err(Error::Internal(format!("{a_bin} failed: {}", tail(&a_out))));
    }
    if !b_out.status.success() {
        return Err(Error::Internal(format!("{b_bin} failed: {}", tail(&b_out))));
    }
    copied.map_err(|e| Error::Internal(format!("copying between {a_bin} and {b_bin}: {e}")))?;
    Ok(String::from_utf8_lossy(&b_out.stdout).into_owned())
}

/// Images the reconciler is pulling for databases (one pull each).
static PULLING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Pull `image` unless a pull of it is already running; the next reconcile
/// pass after it lands starts the database. Whether a pull was started.
fn pull_in_background(docker: &Docker, image: &str) -> bool {
    if !PULLING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(image.to_string())
    {
        return false;
    }
    let (docker, image) = (docker.clone(), image.to_string());
    tokio::spawn(async move {
        if let Err(e) = docker.pull(&image).await {
            tracing::warn!("pulling {image}: {e}");
        }
        PULLING.lock().unwrap_or_else(|e| e.into_inner()).remove(&image);
    });
    true
}

/// Copy a directory tree keeping owners, permissions and links (`cp -a`),
/// replacing what is at `to`.
pub(crate) async fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    // Not `is_dir`: a source we may not read isn't a missing one.
    match std::fs::metadata(from) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(Error::Invalid(format!("{} is not a directory", from.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::NotFound(format!("{} doesn't exist", from.display())))
        }
        Err(e) => return Err(Error::Internal(format!("can't read {}: {e}", from.display()))),
    }
    std::fs::create_dir_all(to)?;
    let source = std::fs::canonicalize(from)?;
    let destination = std::fs::canonicalize(to)?;
    if source.starts_with(&destination) || destination.starts_with(&source) {
        return Err(Error::Invalid(
            "source and destination volume directories overlap; refusing to delete or copy data".into(),
        ));
    }
    let script = "set -e; find \"$2\" -mindepth 1 -delete; cp -a \"$1\"/. \"$2\"/";
    let out = proc::run(Spec {
        bin: "sh",
        args: &proc::args(&["-c", script, "copy", &from.to_string_lossy(), &to.to_string_lossy()]),
        env: &[],
        cwd: None,
        timeout: Duration::from_secs(6 * 3600),
    })
    .await?;
    if !out.success() {
        return Err(Error::Internal(format!("copying the data failed: {}", out.tail(3))));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn overlapping_copy_paths_never_delete_volume_data() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("volume");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("database"), b"valuable data").unwrap();
        assert!(super::copy_tree(&source, &source).await.is_err());
        assert_eq!(std::fs::read(source.join("database")).unwrap(), b"valuable data");
        assert!(super::copy_tree(&source, dir.path()).await.is_err());
        assert_eq!(std::fs::read(source.join("database")).unwrap(), b"valuable data");
        assert!(super::copy_tree(dir.path(), &source).await.is_err());
        #[cfg(unix)]
        {
            let alias = dir.path().join("alias");
            std::os::unix::fs::symlink(&source, &alias).unwrap();
            assert!(super::copy_tree(&source, &alias).await.is_err());
            assert_eq!(std::fs::read(source.join("database")).unwrap(), b"valuable data");
        }
    }

    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn copy_tree_says_why_a_source_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let locked = temp.path().join("volumes");
        let from = locked.join("vfy-db-x-data").join("_data");
        let to = temp.path().join("hdd");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = std::fs::metadata(&from).is_err();
        let result = copy_tree(&from, &to).await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !denied {
            eprintln!("skipped: permissions don't apply to this user (root)");
            return;
        }
        let e = result.unwrap_err();
        assert!(!matches!(e, Error::NotFound(_)), "{e:?}");
        assert!(e.to_string().contains("can't read"), "{e}");
        assert!(!to.exists());
        let missing = copy_tree(&temp.path().join("nope"), &to).await.unwrap_err();
        assert!(matches!(missing, Error::NotFound(_)), "{missing:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_database_image_is_pulled_once_in_the_background() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let calls = temp.path().join("calls");
        let bin = temp.path().join("docker");
        std::fs::write(&bin, format!("#!/bin/sh\necho \"$*\" >>{}\nsleep 1\n", calls.display())).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let docker = Docker {
            bin: bin.to_string_lossy().into_owned(),
        };
        let started = std::time::Instant::now();
        assert!(pull_in_background(&docker, "postgres:test-only"));
        // Already pulling: no second pull, and neither call waits for it.
        assert!(!pull_in_background(&docker, "postgres:test-only"));
        assert!(started.elapsed() < Duration::from_millis(500));
        for _ in 0..50 {
            if !PULLING.lock().unwrap().contains("postgres:test-only") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let log = std::fs::read_to_string(&calls).unwrap();
        assert_eq!(log.matches("pull --quiet postgres:test-only").count(), 1, "{log}");
        assert!(
            pull_in_background(&docker, "postgres:test-only"),
            "pulls again once done"
        );
    }

    #[test]
    fn restic_settings() {
        let r = Restic {
            bin: "restic".into(),
            env: [
                ("RESTIC_PASSWORD".to_string(), "pw".to_string()),
                (
                    "BACKUP_REPOS".to_string(),
                    "/mnt/storage/backups/restic s3:x".to_string(),
                ),
            ]
            .into(),
        };
        assert!(r.configured());
        assert_eq!(r.repo().as_deref(), Some("/mnt/storage/backups/restic"));
        let env = r.command_env();
        assert!(env
            .iter()
            .any(|(k, v)| k == "RESTIC_REPOSITORY" && v == "/mnt/storage/backups/restic"));
        assert!(!env.iter().any(|(k, _)| k == "BACKUP_REPOS"));
        assert!(!Restic::default().configured());
    }

    #[tokio::test]
    async fn pipes_and_copies() {
        let out = pipe(
            "sh",
            &proc::args(&["-c", "printf 'a\\nb\\n'"]),
            "sh",
            &proc::args(&["-c", "wc -l"]),
            &[],
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(out.trim(), "2");
        let failed = pipe(
            "sh",
            &proc::args(&["-c", "exit 3"]),
            "cat",
            &[],
            &[],
            Duration::from_secs(5),
        )
        .await;
        assert!(failed.is_err(), "a failing producer fails the pipe");

        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::write(a.join("sub/f"), "x").unwrap();
        std::os::unix::fs::symlink("f", a.join("sub/l")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("stale"), "old").unwrap();
        copy_tree(&a, &b).await.unwrap();
        assert_eq!(std::fs::read_to_string(b.join("sub/f")).unwrap(), "x");
        assert!(std::fs::symlink_metadata(b.join("sub/l"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!b.join("stale").exists(), "replaced, not merged");
        assert!(copy_tree(&d.path().join("missing"), &b).await.is_err());
    }
}

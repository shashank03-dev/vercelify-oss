//! An ephemeral database restores selected production data in quarantine. Source
//! service credentials/app environment never enter it. Repository access uses a
//! separate bounded official helper, whose stdout must never be logged.
use crate::{engine::Engine, proc, restore_drill::Manifest, Error, Result};
use std::time::Duration;
use vercelify_core::platform::Database;

pub const RESTIC_IMAGE: &str = "restic/restic@sha256:4cf4a61ef9786f4de53e9de8c8f5c040f33830eb0a10bf3d614410ee2fcb6120";
const CATALOG: &str = r#"SET search_path=pg_catalog; SELECT json_build_object('database_bytes',pg_database_size(current_database()),'server_major',current_setting('server_version_num')::int/10000,'extensions',(SELECT coalesce(json_agg(json_build_array(extname,extversion) ORDER BY extname),'[]'::json) FROM pg_extension),'objects',(SELECT coalesce(json_agg(json_build_array(nspname,relname,relkind) ORDER BY nspname,relname,relkind),'[]'::json) FROM (SELECT n.nspname,c.relname,c.relkind::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname NOT LIKE 'pg_%' AND n.nspname<>'information_schema' AND c.relkind IN ('r','p','i','I','v','m','S','f') ORDER BY n.nspname,c.relname,c.relkind LIMIT 5001) objects))"#;
impl Engine {
    pub(crate) async fn restore_drill_metadata(&self, db: &Database) -> Result<Manifest> {
        let output = self
            .deployer
            .docker
            .exec(
                &db.container,
                &proc::args(&[
                    "sh",
                    "-ec",
                    "exec psql -X -U \"$POSTGRES_USER\" -d \"$POSTGRES_DB\" -q -At -v ON_ERROR_STOP=1 -c \"$1\"",
                    "drill-metadata",
                    CATALOG,
                ]),
                Duration::from_secs(30),
            )
            .await?;
        if !output.success() || output.stdout.len() > 1024 * 1024 {
            return Err(Error::Invalid(
                "PostgreSQL drill metadata unavailable or too large".into(),
            ));
        }
        let value: serde_json::Value = serde_json::from_str(output.stdout.trim())?;
        let mut manifest = Manifest {
            version: 1,
            database_id: db.id.clone(),
            database_name: db.name.clone(),
            server_major: value["server_major"].as_u64().unwrap_or(0) as u16,
            filename: format!("{}.dump", db.name),
            format: "postgresql-custom".into(),
            backup_started_at: crate::now(),
            bytes: 1,
            database_bytes: value["database_bytes"].as_u64().unwrap_or(0),
            extensions: serde_json::from_value(value["extensions"].clone())?,
            objects: serde_json::from_value(value["objects"].clone())?,
        };
        manifest.extensions.sort();
        manifest.objects.sort();
        Ok(manifest)
    }
}

use crate::{
    dbengine::Restic,
    deploy,
    docker::{ContainerSpec, Mount, MANAGED, MANAGED_LABEL},
    restore_drill::{self, Drill},
    volumes,
};
use std::{collections::BTreeMap, process::Stdio, sync::Arc, time::Instant};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use vercelify_core::platform::{DatabaseKind, VolumeOwner};

fn helper_name(id: &str, phase: &str) -> String {
    format!("vfy-drill-{id}-{phase}")
}
impl Engine {
    async fn drill_repository_helper(
        &self,
        job: &Drill,
        restic: &Restic,
        phase: &str,
        args: Vec<String>,
    ) -> Result<String> {
        if !restic.configured() {
            return Err(Error::Invalid(
                "a repository and recovery password are required for this drill".into(),
            ));
        }
        let repo = restic
            .repo()
            .ok_or_else(|| Error::Invalid("recovery repository missing".into()))?;
        let mut mounts = vec![];
        let repo = if repo.starts_with('/') {
            let path = std::fs::canonicalize(&repo)
                .map_err(|_| Error::Invalid("local backup repository is missing".into()))?;
            if !path.is_dir() {
                return Err(Error::Invalid("backup repository must be a directory".into()));
            }
            mounts.push(Mount {
                source: path.to_string_lossy().into(),
                target: "/repository".into(),
                read_only: true,
            });
            "/repository".into()
        } else if ["s3:", "rest:", "b2:", "azure:"].iter().any(|p| repo.starts_with(p)) {
            repo
        } else {
            return Err(Error::Invalid(
                "bounded drills require an absolute local repository or S3/REST/B2/Azure backend".into(),
            ));
        };
        let mut env: Vec<_> = restic
            .env
            .iter()
            .filter(|(k, _)| k.as_str() != "BACKUP_REPOS" && k.as_str() != "RESTIC_REPOSITORY")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        env.push(("RESTIC_REPOSITORY".into(), repo));
        let name = helper_name(&job.id, phase);
        let docker = &self.deployer.docker;
        if docker.inspect(&name).await?.is_some() {
            if docker.label(&name, "vercelify.restore-drill").await? != job.id {
                return Err(Error::Conflict(
                    "restore helper name is owned by another resource".into(),
                ));
            }
            docker.remove(&name).await?;
        }
        if !docker.image_exists(RESTIC_IMAGE).await? {
            docker.pull(RESTIC_IMAGE).await?;
        }
        let mut command = proc::args(&["--no-lock", "--no-cache"]);
        command.extend(args);
        docker
            .create(&ContainerSpec {
                name: name.clone(),
                image: RESTIC_IMAGE.into(),
                labels: BTreeMap::from([
                    (MANAGED_LABEL.into(), MANAGED.into()),
                    ("vercelify.kind".into(), "restore-drill-repository".into()),
                    ("vercelify.restore-drill".into(), job.id.clone()),
                    (deploy::LABEL_DEPLOYMENT.into(), job.id.clone()),
                ]),
                env,
                networks: vec!["bridge".into()],
                mounts,
                command: Some(command),
                discard_logs: true,
                isolated_preview: true,
                memory_mb: Some(512),
                cpus: Some(1.0),
                restart: "no".into(),
                cgroup_parent: self.deployer.cgroup_parent.clone(),
                ..Default::default()
            })
            .await?;
        Ok(name)
    }
    fn drill_can_continue(&self, id: &str) -> Result<()> {
        self.store.with(|c| {
            if restore_drill::cancelled(c, id)? {
                return Err(Error::Conflict("restore drill cancelled".into()));
            }
            let main = crate::nodes::main(c)?;
            if main.status.thermal_paused
                || (main.status.on_battery && main.status.battery_percent.is_none_or(|p| p < 50))
            {
                return Err(Error::Capacity("restore drill paused for power or heat".into()));
            }
            if let Ok(sample) = crate::capacity::sample(c, &main.id) {
                if sample.disk_free_mb.is_some_and(|free| free < 512) {
                    return Err(Error::Capacity(
                        "restore drill stopped before exhausting disk headroom".into(),
                    ));
                }
            }
            Ok(())
        })
    }
    async fn drill_check_repository(&self, job: &Drill, restic: &Restic) -> Result<()> {
        let full: bool = self.store.with(|c| {
            Ok(c.query_row(
                "SELECT full_integrity FROM restore_drills WHERE id=?1",
                [&job.id],
                |r| r.get(0),
            )?)
        })?;
        let mut args = proc::args(&["check"]);
        if full {
            args.push("--read-data".into());
        }
        let name = self.drill_repository_helper(job, restic, "check", args).await?;
        let mut child = Command::new(&self.deployer.docker.bin)
            .args(["start", "--attach", &name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let work = async {
            loop {
                tokio::select! {
                    status=child.wait()=>return Ok::<bool,Error>(status?.success()),
                    ()=tokio::time::sleep(Duration::from_secs(1))=>self.drill_can_continue(&job.id)?,
                }
            }
        };
        let outcome = tokio::time::timeout(Duration::from_secs(3 * 3600), work)
            .await
            .map_err(|_| Error::Unavailable("repository check exceeded its time budget".into()))?;
        let status = outcome?;
        let inspected = self.deployer.docker.inspect(&name).await?;
        if !status || inspected.is_none_or(|i| i.exit_code != 0) {
            self.store.tx(|c| {
                c.execute(
                    "UPDATE restore_drills SET integrity='check_failed' WHERE id=?1",
                    [&job.id],
                )?;
                Ok(())
            })?;
            return Err(Error::Invalid(
                "repository/key check failed or was incomplete; no restored dataset was verified".into(),
            ));
        }
        self.store.tx(|c| {
            c.execute(
                "UPDATE restore_drills SET integrity=?2 WHERE id=?1",
                rusqlite::params![job.id, if full { "full_data_checked" } else { "metadata_checked" }],
            )?;
            Ok(())
        })?;
        self.deployer.docker.remove(&name).await?;
        Ok(())
    }
    async fn drill_restore_stream(
        &self,
        job: &Drill,
        restic: &Restic,
        destination: &Database,
        m: &Manifest,
    ) -> Result<()> {
        let name = self
            .drill_repository_helper(
                job,
                restic,
                "dump",
                proc::args(&["dump", &job.snapshot, &format!("/{}", m.filename)]),
            )
            .await?;
        let mut producer = Command::new(&self.deployer.docker.bin)
            .args(["start", "--attach", &name])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut consumer=Command::new(&self.deployer.docker.bin).args(["exec","-i",&destination.container,"sh","-ec","exec pg_restore -U \"$POSTGRES_USER\" -d \"$POSTGRES_DB\" --single-transaction --exit-on-error --no-owner --no-acl"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).spawn()?;
        let mut output = producer
            .stdout
            .take()
            .ok_or_else(|| Error::Internal("dump pipe missing".into()))?
            .take(m.bytes + 1);
        let mut input = consumer
            .stdin
            .take()
            .ok_or_else(|| Error::Internal("restore pipe missing".into()))?;
        let work = async {
            let bytes = tokio::io::copy(&mut output, &mut input).await?;
            drop(input);
            if bytes != m.bytes {
                return Err(Error::Invalid(
                    "selected dump size differs from the backup manifest".into(),
                ));
            }
            let (a, b) = tokio::join!(producer.wait(), consumer.wait());
            if !a?.success() || !b?.success() {
                return Err(Error::Invalid(
                    "selected snapshot restore failed: key, dump or engine compatibility requires attention".into(),
                ));
            }
            Ok(())
        };
        tokio::pin!(work);
        let mut deadline = Box::pin(tokio::time::sleep(Duration::from_secs(3 * 3600)));
        loop {
            tokio::select! {
                result=&mut work=>{result?;break},
                ()=&mut deadline=>return Err(Error::Unavailable("restore stream exceeded its time budget".into())),
                ()=tokio::time::sleep(Duration::from_secs(1))=>self.drill_can_continue(&job.id)?,
            }
        }
        let inspected = self.deployer.docker.inspect(&name).await?;
        if inspected.is_none_or(|i| i.exit_code != 0) {
            return Err(Error::Invalid("repository did not complete the selected dump".into()));
        }
        self.deployer.docker.remove(&name).await?;
        Ok(())
    }
    async fn drill_sql(&self, destination: &Database, role: &str, sql: &str, limit: u64) -> Result<String> {
        let cred = self
            .store
            .with(|c| crate::databases::credentials(c, &self.key, &destination.id))?;
        let mut child = Command::new(&self.deployer.docker.bin)
            .args([
                "exec",
                "-i",
                &destination.container,
                "psql",
                "-X",
                "-q",
                "-U",
                role,
                "-d",
                &cred.database,
                "-At",
                "-v",
                "ON_ERROR_STOP=1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| Error::Internal("query input missing".into()))?;
        input.write_all(sql.as_bytes()).await?;
        drop(input);
        let work = async {
            let mut output = vec![];
            child
                .stdout
                .take()
                .ok_or_else(|| Error::Internal("query output missing".into()))?
                .take(limit + 1)
                .read_to_end(&mut output)
                .await?;
            if output.len() as u64 > limit {
                let _ = child.kill().await;
                return Err(Error::Invalid("verification query output exceeded its bound".into()));
            }
            if !child.wait().await?.success() {
                return Err(Error::Invalid(
                    "verification SQL failed; query output is not included in diagnostics".into(),
                ));
            }
            String::from_utf8(output)
                .map(|s| s.trim_end_matches('\n').to_string())
                .map_err(|_| Error::Invalid("verification returned invalid text".into()))
        };
        tokio::time::timeout(Duration::from_secs(30), work)
            .await
            .map_err(|_| Error::Unavailable("verification query exceeded its time budget".into()))?
    }
    async fn verify_drill(&self, job: &Drill, destination: &Database, m: &Manifest) -> Result<()> {
        let restored = self.restore_drill_metadata(destination).await?;
        if restored.server_major != m.server_major
            || restored.extensions != m.extensions
            || restored.objects != m.objects
        {
            return Err(Error::Invalid("restored catalog/version/extensions differ from the backup manifest; concurrent schema changes or incompatibility require a fresh backup".into()));
        }
        let cred = self
            .store
            .with(|c| crate::databases::credentials(c, &self.key, &destination.id))?;
        self.drill_sql(destination,&cred.user,"CREATE ROLE vfy_drill_verify LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; GRANT pg_read_all_data TO vfy_drill_verify; ALTER ROLE vfy_drill_verify SET default_transaction_read_only=on; ALTER ROLE vfy_drill_verify SET statement_timeout=5000;",1024).await?;
        if self
            .drill_sql(
                destination,
                "vfy_drill_verify",
                "BEGIN READ ONLY; SELECT 1; ROLLBACK;",
                1024,
            )
            .await?
            != "1"
        {
            return Err(Error::Invalid("restored query smoke check failed".into()));
        }
        let assertions = self.store.with(|c| restore_drill::assertions(c, &self.key, &job.id))?;
        for assertion in assertions {
            self.drill_can_continue(&job.id)?;
            let sql = format!(
                "BEGIN READ ONLY; SET statement_timeout=5000;\n{}\n;\nROLLBACK;",
                assertion.query
            );
            let observed = self
                .drill_sql(destination, "vfy_drill_verify", &sql, 65536)
                .await
                .map_err(|_| Error::Invalid(format!("assertion {} could not complete", assertion.name)))?;
            if observed != assertion.expected {
                return Err(Error::Invalid(format!(
                    "assertion {} did not match its known expectation",
                    assertion.name
                )));
            }
        }
        Ok(())
    }
    async fn cleanup_restore_drill(&self, job: &Drill) -> Result<()> {
        let docker = &self.deployer.docker;
        for phase in ["check", "dump"] {
            let name = helper_name(&job.id, phase);
            if docker.inspect(&name).await?.is_some() {
                if docker.label(&name, "vercelify.restore-drill").await? != job.id {
                    return Err(Error::Conflict(
                        "refusing cleanup of an unowned repository helper".into(),
                    ));
                }
                docker.remove(&name).await?;
            }
        }
        if let Some(service) = self.store.with(|c| restore_drill::service(c, &job.id))? {
            let db = self
                .store
                .with(|c| crate::databases::get(c, &self.key, &service.database_id, false))?;
            if docker.inspect(&db.container).await?.is_some()
                && docker.label(&db.container, "vercelify.database").await? != db.id
            {
                return Err(Error::Conflict(
                    "refusing cleanup of an unowned restore database".into(),
                ));
            }
            let owned = self
                .store
                .with(|c| volumes::checked_list(c, VolumeOwner::Database, &db.id))?;
            self.remove_database(&db, false, None, "").await?;
            for volume in owned {
                let name = volumes::physical_name(&volume, &format!("db-{}", db.name), None);
                let result = docker
                    .run(&proc::args(&["volume", "rm", &name]), Duration::from_secs(30))
                    .await?;
                if !result.success() && !result.stderr.to_lowercase().contains("no such volume") {
                    return Err(Error::Unavailable("restore volume cleanup did not complete".into()));
                }
            }
            crate::restore_network::remove(docker, &service.network).await?;
        }
        self.store.tx(|c| restore_drill::finish_cleanup(c, &job.id))?;
        Ok(())
    }
    async fn finish_restore_drill_cleanup(&self, job: &Drill) -> Result<Drill> {
        self.cleanup_restore_drill(job).await?;
        self.store.tx(|c| {
            let current = restore_drill::get(c, &job.id)?;
            let state = if restore_drill::cancelled(c, &job.id)? {
                "cancelled"
            } else if current.verified_at.is_some() && current.error.is_empty() {
                "succeeded"
            } else if current.error.starts_with("deferred:") {
                "deferred"
            } else {
                "failed"
            };
            restore_drill::set_state(c, &job.id, state, &current.error)?;
            restore_drill::get(c, &job.id)
        })
    }
    /// The same serialized runner handles API requests and periodic reconciliation.
    /// A process restart abandons an interrupted disposable restore and retries
    /// its exact cleanup; it never relabels partial data as verified.
    pub async fn execute_restore_drill(&self, id: &str, host_restic: &Restic) -> Result<Drill> {
        let _serial = self.database_operation("__restore_drills__").await;
        let mut job = self.store.with(|c| restore_drill::get(c, id))?;
        if matches!(job.state.as_str(), "succeeded" | "failed" | "cancelled") {
            return Ok(job);
        }
        if matches!(job.state.as_str(), "running" | "verifying") {
            let reason = if job.verified_at.is_some() {
                ""
            } else {
                "interrupted restore; temporary data was not verified"
            };
            self.store.tx(|c| restore_drill::begin_cleanup(c, id, reason))?;
            job = self.store.with(|c| restore_drill::get(c, id))?;
        }
        if job.state == "cleanup" {
            return self.finish_restore_drill_cleanup(&job).await;
        }
        if self.store.with(|c| restore_drill::cancelled(c, id))? {
            if job.cleanup_pending {
                self.store.tx(|c| restore_drill::begin_cleanup(c, id, "cancelled"))?;
                return self.finish_restore_drill_cleanup(&job).await;
            }
            self.store
                .tx(|c| restore_drill::set_state(c, id, "cancelled", "cancelled"))?;
            return self.store.with(|c| restore_drill::get(c, id));
        }
        if let Err(error) = self.drill_can_continue(id) {
            self.store
                .tx(|c| restore_drill::set_state(c, id, "deferred", error.message()))?;
            return self.store.with(|c| restore_drill::get(c, id));
        }
        let recovery = self.store.with(|c| restore_drill::recovery(c, &self.key, id))?;
        let restic = if job.credential_source == "provided" {
            recovery
                .ok_or_else(|| Error::Invalid("provided recovery key is missing".into()))?
                .restic()
        } else {
            host_restic.clone()
        };
        if !restic.configured() {
            if job.cleanup_pending {
                self.store.tx(|c| {
                    restore_drill::begin_cleanup(c, id, "recovery repository/key unavailable; no restore performed")
                })?;
                return self.finish_restore_drill_cleanup(&job).await;
            }
            self.store.tx(|c| {
                restore_drill::set_state(
                    c,
                    id,
                    "failed",
                    "recovery repository/key unavailable; no restore performed",
                )
            })?;
            return self.store.with(|c| restore_drill::get(c, id));
        }
        let attempts: u32 = self
            .store
            .with(|c| Ok(c.query_row("SELECT attempts FROM restore_drills WHERE id=?1", [id], |r| r.get(0))?))?;
        if attempts >= 3 {
            self.store.tx(|c| {
                restore_drill::begin_cleanup(
                    c,
                    id,
                    "restore attempt limit reached; rerun after resolving repeated deferrals",
                )
            })?;
            return self.finish_restore_drill_cleanup(&job).await;
        }
        let manifest = self.store.with(|c| restore_drill::job_manifest(c, id))?;
        let service = self.store.tx(|c| restore_drill::prepare(c, &self.key, id))?;
        let main = self.store.with(crate::nodes::main)?;
        let disk = manifest
            .bytes
            .saturating_mul(3)
            .max(manifest.database_bytes.saturating_mul(2))
            .div_ceil(1_048_576)
            + 1024;
        let admitted = self
            .admit(
                id,
                &[
                    crate::capacity::Request {
                        node_id: main.id.clone(),
                        owner: service.database_id.clone(),
                        kind: "database".into(),
                        memory_mb: 384,
                        disk_mb: disk,
                        heavy: true,
                    },
                    crate::capacity::Request {
                        node_id: main.id,
                        owner: id.into(),
                        kind: "deploy".into(),
                        memory_mb: 512,
                        disk_mb: 0,
                        heavy: false,
                    },
                ],
            )
            .await;
        let mut permit = match admitted {
            Ok(p) => p,
            Err(Error::Capacity(reason) | Error::Unavailable(reason)) => {
                self.store
                    .tx(|c| restore_drill::set_state(c, id, "deferred", &reason))?;
                return self.store.with(|c| restore_drill::get(c, id));
            }
            Err(e) => return Err(e),
        };
        permit.started();
        self.store.tx(|c| restore_drill::set_state(c, id, "running", ""))?;
        job = self.store.with(|c| restore_drill::get(c, id))?;
        let start = Instant::now();
        let result = async {
            self.drill_check_repository(&job, &restic).await?;
            self.drill_can_continue(id)?;
            let _database = self.database_operation(&service.database_id).await;
            let destination = self.ensure_database_with_resources(&service.database_id, true).await?;
            if destination.kind != DatabaseKind::Postgresql {
                return Err(Error::Invalid("restore destination changed engine".into()));
            }
            self.drill_restore_stream(&job, &restic, &destination, &manifest)
                .await?;
            self.store.tx(|c| restore_drill::set_state(c, id, "verifying", ""))?;
            self.verify_drill(&job, &destination, &manifest).await?;
            self.store
                .tx(|c| restore_drill::recorded_verification(c, id, start.elapsed().as_secs()))?;
            Ok(())
        }
        .await;
        let reason = match result {
            Ok(()) => String::new(),
            Err(Error::Capacity(reason)) => format!("deferred: {reason}"),
            Err(Error::Conflict(reason)) if reason == "restore drill cancelled" => "cancelled".into(),
            Err(error) => error.message().to_string(),
        };
        self.store.tx(|c| restore_drill::begin_cleanup(c, id, &reason))?;
        self.finish_restore_drill_cleanup(&job).await
    }
    pub async fn reconcile_restore_drills(self: &Arc<Self>, restic: &Restic) -> Vec<String> {
        let jobs = match self.store.with(|c| restore_drill::list(c, None, true)) {
            Ok(j) => j,
            Err(e) => return vec![e.to_string()],
        };
        let mut errors = vec![];
        for job in jobs {
            if let Err(error) = self.execute_restore_drill(&job.id, restic).await {
                errors.push(format!("restore drill {}: {error}", job.id));
            }
        }
        errors
    }
}

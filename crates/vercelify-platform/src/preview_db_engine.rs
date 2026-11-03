//! PostgreSQL preview execution. The service is separate from production; each
//! PR login owns its objects but cannot change database CONNECT privileges.
use crate::{
    capacity, databases, deploy, deployments,
    docker::{ContainerSpec, MANAGED, MANAGED_LABEL},
    engine::Engine,
    ids::new_id,
    logs::{LogWriter, Redactor},
    nodes,
    preview_db::{self, Resource},
    preview_network, previews,
    proc::{self, Cancel, Spec},
    volumes, Error, Result,
};
use rusqlite::params;
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};
use vercelify_core::platform::{App, Database, Deployment, VolumeOwner};

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
fn marker(id: &str) -> String {
    format!("vercelify-preview:{id}")
}

impl Engine {
    async fn remove_preview_db_runtime(&self, resource: &Resource) -> Result<()> {
        let docker = &self.deployer.docker;
        for container in docker.list_managed().await? {
            if container.labels.get(deploy::LABEL_APP) == Some(&resource.app_id)
                && container.labels.get(deploy::LABEL_PR) == Some(&resource.pr_number.to_string())
            {
                docker.remove(&container.name).await?;
            }
        }
        let service = self
            .store
            .with(|c| databases::get(c, &self.key, &resource.service_id, false))?;
        let network = preview_network::name(&resource.app_id, resource.pr_number);
        if docker
            .inspect(&service.container)
            .await?
            .is_some_and(|s| s.ips.contains_key(&network))
        {
            let output = docker
                .run(
                    &proc::args(&["network", "disconnect", "--force", &network, &service.container]),
                    Duration::from_secs(30),
                )
                .await?;
            if !output.success() {
                return Err(Error::Unavailable(format!(
                    "disconnect closed preview database: {}",
                    output.tail(2)
                )));
            }
        }
        self.teardown_preview_owned(&resource.app_id, &resource.app_name, false, resource.pr_number)
            .await
    }

    async fn drop_preview_db_role(&self, service: &Database, resource: &Resource) -> Result<()> {
        self.remove_preview_db_runtime(resource).await?;
        // Cleanup starts no new workload when the existing bounded PG service
        // is running. If it is absent/stopped, its restart uses normal admission.
        if !self
            .deployer
            .docker
            .inspect(&service.container)
            .await?
            .is_some_and(|s| s.running)
        {
            self.ensure_database(&service.id).await?;
        }
        let _database = self.database_operation(&service.id).await;
        let admin = self.store.with(|c| databases::credentials(c, &self.key, &service.id))?;
        let redactor = Redactor::default();
        let owner = preview_db::owner_role(resource);
        for role in [&resource.username, &owner] {
            let actual = self.preview_role_marker(service, role, &redactor).await?;
            if !actual.is_empty() && actual != marker(&resource.id) {
                return Err(Error::Conflict(
                    "refusing to remove an unowned preview PostgreSQL role".into(),
                ));
            }
        }
        let existing=self.preview_sql(service,&admin.database,&format!("SELECT pg_get_userbyid(datdba)||'|'||coalesce(shobj_description(oid,'pg_database'),'') FROM pg_database WHERE datname={};",literal(&resource.database)),&redactor).await?;
        if !existing.is_empty() {
            let (actual_owner, comment) = existing.split_once('|').unwrap_or(("", ""));
            if actual_owner != owner || (!comment.is_empty() && comment != marker(&resource.id)) {
                return Err(Error::Conflict(
                    "refusing to remove an unowned preview PostgreSQL database".into(),
                ));
            }
        }
        if !self
            .preview_role_marker(service, &resource.username, &redactor)
            .await?
            .is_empty()
        {
            self.preview_sql(service,&admin.database,&format!("ALTER ROLE {} NOLOGIN; SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename={} AND pid<>pg_backend_pid();",ident(&resource.username),literal(&resource.username)),&redactor).await?;
        }
        self.preview_sql(
            service,
            &admin.database,
            &format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE); DROP ROLE IF EXISTS {}; DROP ROLE IF EXISTS {};",
                ident(&resource.database),
                ident(&resource.username),
                ident(&owner)
            ),
            &redactor,
        )
        .await?;
        self.store
            .tx(|c| preview_db::set_state(c, &resource.id, "deleted", ""))?;
        Ok(())
    }

    async fn retire_preview_db_service(
        &self,
        service: &Database,
        owned: &preview_db::Service,
        resources: &[Resource],
    ) -> Result<()> {
        let docker = &self.deployer.docker;
        if docker.inspect(&service.container).await?.is_some()
            && docker.label(&service.container, "vercelify.database").await? != service.id
        {
            return Err(Error::Conflict(
                "refusing to remove an unowned preview PostgreSQL container".into(),
            ));
        }
        let volume = self
            .store
            .with(|c| volumes::checked_list(c, VolumeOwner::Database, &service.id))?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Internal("preview PostgreSQL volume ownership missing".into()))?;
        // Marked closing atomically before external deletion; new jobs cannot
        // adopt the cluster while its journaled teardown is incomplete.
        self.remove_database(
            service,
            false,
            volume.host_path.clone(),
            &volumes::physical_name(&volume, &format!("db-{}", service.name), None),
        )
        .await?;
        let storage = volumes::physical_name(&volume, &format!("db-{}", service.name), None);
        let removed = docker
            .run(&proc::args(&["volume", "rm", &storage]), Duration::from_secs(30))
            .await?;
        if !removed.success()
            && !removed.tail(2).contains("no such volume")
            && !removed.tail(2).contains("No such volume")
        {
            return Err(Error::Unavailable(
                "preview PostgreSQL data volume is still in use; cleanup will retry".into(),
            ));
        }
        for resource in resources {
            self.remove_preview_db_runtime(resource).await?;
        }
        preview_network::remove(docker, &owned.network).await?;
        self.store.tx(|c| preview_db::finish_service(c, &service.id))?;
        Ok(())
    }

    /// Retry journaled close/app-delete cleanup and recover active services.
    /// App guards exclude migration/publication while external resources change.
    pub async fn reconcile_preview_databases(&self) -> Vec<String> {
        let state = self.store.tx(|c| {
            preview_db::pending_cleanup(c)?;
            preview_db::services(c)
        });
        let services = match state {
            Ok(s) => s,
            Err(e) => return vec![format!("preview databases: {e}")],
        };
        let mut errors = vec![];
        for owned in services {
            let Ok(_app) = self.app_operation(&owned.app_id) else {
                continue;
            };
            let result = async {
                let resources = self.store.with(|c| preview_db::resources(c, &owned.database_id))?;
                self.store.tx(|c| {
                    for resource in resources.iter().filter(|r| r.state == "closing") {
                        previews::close(c, &resource.app_id, resource.pr_number)?;
                        for d in deployments::active(c)? {
                            if d.app_id == resource.app_id
                                && d.pr_number == Some(resource.pr_number)
                                && d.status == vercelify_core::platform::DeploymentStatus::Queued
                            {
                                deployments::transition(
                                    c,
                                    &d.id,
                                    vercelify_core::platform::DeploymentStatus::Cancelled,
                                    Some("the pull request closed"),
                                )?;
                            }
                        }
                    }
                    Ok(())
                })?;
                let service = self
                    .store
                    .with(|c| databases::get(c, &self.key, &owned.database_id, false))?;
                if self.store.tx(|c| preview_db::close_service_if_unused(c, &service.id))? {
                    return self.retire_preview_db_service(&service, &owned, &resources).await;
                }
                for resource in resources.iter().filter(|r| r.state == "closing") {
                    self.drop_preview_db_role(&service, resource).await?;
                }
                // Agent/process death can leave an attached helper behind. Its
                // exact journal and finished deployment, never a name alone,
                // authorize removal.
                for container in self.deployer.docker.list_managed().await? {
                    if container.labels.get(deploy::LABEL_APP) != Some(&owned.app_id)
                        || container.labels.get(deploy::LABEL_KIND).map(String::as_str) != Some("preview-db-task")
                    {
                        continue;
                    }
                    let Some(id) = container.labels.get("vercelify.preview-db-resource") else {
                        continue;
                    };
                    let row = self.store.with(|c| preview_db::get(c, id))?;
                    if row.service_id != service.id {
                        continue;
                    }
                    let active = container
                        .labels
                        .get(deploy::LABEL_DEPLOYMENT)
                        .and_then(|id| self.store.with(|c| deployments::get(c, id)).ok())
                        .is_some_and(|d| d.status.is_active());
                    if !active {
                        self.deployer.docker.remove(&container.name).await?;
                    }
                }
                if !self
                    .deployer
                    .docker
                    .inspect(&service.container)
                    .await?
                    .is_some_and(|s| s.running)
                {
                    self.ensure_database(&service.id).await?;
                    for resource in resources.iter().filter(|r| r.state == "ready") {
                        self.store.with(|c| preview_db::require_active(c, &resource.id))?;
                        let network = preview_network::name(&resource.app_id, resource.pr_number);
                        preview_network::ensure(&self.deployer.docker, &network).await?;
                        let output = self
                            .deployer
                            .docker
                            .run(
                                &proc::args(&["network", "connect", &network, &service.container]),
                                Duration::from_secs(30),
                            )
                            .await?;
                        if !output.success() && !output.tail(2).contains("already exists") {
                            return Err(Error::Unavailable("could not reconnect preview PostgreSQL".into()));
                        }
                    }
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                errors.push(format!("preview PostgreSQL {}: {error}", owned.app_id));
            }
        }
        errors
    }

    async fn preview_sql(&self, service: &Database, database: &str, sql: &str, redactor: &Redactor) -> Result<String> {
        let credential = self.store.with(|c| databases::credentials(c, &self.key, &service.id))?;
        let args = proc::args(&[
            "exec",
            "-i",
            &service.container,
            "psql",
            "-X",
            "-q",
            "-U",
            &credential.user,
            "-d",
            database,
            "-At",
            "-v",
            "ON_ERROR_STOP=1",
        ]);
        let mut child = Command::new(&self.deployer.docker.bin)
            .args(&args)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| Error::Internal("preview SQL stdin unavailable".into()))?;
        input.write_all(b"SET statement_timeout='15s';\n").await?;
        input.write_all(sql.as_bytes()).await?;
        drop(input);
        let output = tokio::time::timeout(Duration::from_secs(90), child.wait_with_output())
            .await
            .map_err(|_| Error::Unavailable("preview PostgreSQL operation timed out".into()))??;
        if !output.status.success() {
            let safe = Redactor::new([credential.password.as_str()])
                .apply(&redactor.apply(&String::from_utf8_lossy(&output.stderr)));
            return Err(Error::Unavailable(format!(
                "preview PostgreSQL: {}",
                safe.lines().take(8).collect::<Vec<_>>().join("\n")
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    async fn preview_role_marker(&self, service: &Database, role: &str, redactor: &Redactor) -> Result<String> {
        let admin = self.store.with(|c| databases::credentials(c, &self.key, &service.id))?;
        self.preview_sql(
            service,
            &admin.database,
            &format!(
                "SELECT coalesce(shobj_description(oid,'pg_authid'),'unowned') FROM pg_roles WHERE rolname={};",
                literal(role)
            ),
            redactor,
        )
        .await
    }

    fn preview_job_active(&self, app: &App, d: &Deployment, resource: &Resource, cancel: &Cancel) -> Result<()> {
        if cancel.is_cancelled() {
            return Err(Error::Conflict("cancelled".into()));
        }
        self.store.with(|c| {
            preview_db::require_active(c, &resource.id)?;
            if preview_db::settings(c, &app.id)? != app.previews.database {
                return Err(Error::Conflict("preview database settings changed; redeploy".into()));
            }
            let current = deployments::get(c, &d.id)?;
            let preview =
                previews::get(c, &app.id, resource.pr_number)?.ok_or_else(|| Error::Conflict("cancelled".into()))?;
            if !current.status.is_active()
                || (!preview.head_sha.is_empty() && !d.commit_sha.is_empty() && preview.head_sha != d.commit_sha)
            {
                return Err(Error::Conflict("cancelled".into()));
            }
            Ok(())
        })
    }

    /// Used by the deploy pipeline; standalone library callers get the same M4
    /// promise when no covering deployment reservation is present.
    pub async fn provision_preview_db(
        &self,
        app: &App,
        d: &Deployment,
        image: &str,
        env: &mut Vec<(String, String)>,
        cancel: Cancel,
        log: &mut LogWriter,
    ) -> Result<Option<Resource>> {
        let (Some(pr), Some(settings)) = (d.pr_number, app.previews.database.as_ref()) else {
            return Ok(None);
        };
        if d.app_id != app.id {
            return Err(Error::Invalid("preview database deployment owner mismatch".into()));
        }
        let main = self.store.with(nodes::main)?;
        if d.node_id
            .as_ref()
            .or(app.node_id.as_ref())
            .is_some_and(|id| id != &main.id)
        {
            return Err(Error::Invalid(
                "built-in PostgreSQL previews currently run on the main node".into(),
            ));
        }
        let binding = self.store.tx(|c| preview_db::prepare(c, &self.key, app, pr))?;
        let covered:bool=self.store.with(|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM resource_reservations WHERE operation=?1 AND owner=?2 AND kind='database' AND settled_at IS NULL)",params![d.id,binding.service_id],|r|r.get(0))?))?;
        let _app = if covered {
            None
        } else {
            Some(self.app_operation(&app.id)?)
        };
        let service = self
            .store
            .with(|c| databases::get(c, &self.key, &binding.service_id, false))?;
        let mut promise = if covered {
            None
        } else {
            Some(
                self.admit(
                    &new_id("task"),
                    &[
                        capacity::Request {
                            node_id: main.id.clone(),
                            owner: service.id.clone(),
                            kind: "database".into(),
                            memory_mb: u64::from(service.memory_mb.unwrap_or(256)) + 128,
                            disk_mb: 1024,
                            heavy: true,
                        },
                        capacity::Request {
                            node_id: main.id.clone(),
                            owner: d.id.clone(),
                            kind: "deploy".into(),
                            memory_mb: u64::from(app.runtime.memory_mb.unwrap_or(256))
                                + capacity::transfer_mb(main.status.memory_total_mb),
                            disk_mb: 1024,
                            heavy: true,
                        },
                    ],
                )
                .await?,
            )
        };
        if let Some(p) = &mut promise {
            p.started();
        }
        let _database = self.database_operation(&service.id).await;
        self.preview_job_active(app, d, &binding, &cancel)?;
        let service = self.ensure_database_with_resources(&service.id, true).await?;
        let url = format!(
            "postgresql://{}:{}@{}:5432/{}",
            binding.username,
            binding.password.as_str(),
            service.container,
            binding.database
        );
        log.hide([binding.password.as_str(), url.as_str()]);
        env.retain(|(key, _)| key != &settings.variable && key != "VERCELIFY_PR_NUMBER");
        env.push((settings.variable.clone(), url.clone()));
        env.push(("VERCELIFY_PR_NUMBER".into(), pr.to_string()));
        let redactor = Redactor::new([binding.password.as_str(), url.as_str()]);
        let admin = self.store.with(|c| databases::credentials(c, &self.key, &service.id))?;
        let owner = preview_db::owner_role(&binding);
        for (role, login) in [(&owner, false), (&binding.username, true)] {
            let existing = self.preview_role_marker(&service, role, &redactor).await?;
            if existing.is_empty() {
                let password = if login {
                    format!(" PASSWORD {}", literal(&binding.password))
                } else {
                    String::new()
                };
                self.preview_sql(&service,&admin.database,&format!("BEGIN; CREATE ROLE {} {} NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS CONNECTION LIMIT {}{}; COMMENT ON ROLE {} IS {}; COMMIT;",ident(role),if login{"LOGIN"}else{"NOLOGIN"},if login{preview_db::CONNECTION_LIMIT}else{0},password,ident(role),literal(&marker(&binding.id))),&redactor).await?;
            } else if existing != marker(&binding.id) {
                return Err(Error::Conflict(
                    "refusing to adopt an unowned preview PostgreSQL role".into(),
                ));
            }
        }
        self.preview_sql(&service,&admin.database,&format!("ALTER ROLE {} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS CONNECTION LIMIT {} PASSWORD {};",ident(&binding.username),preview_db::CONNECTION_LIMIT,literal(&binding.password)),&redactor).await?;
        for database in ["postgres", "template1", admin.database.as_str()] {
            self.preview_sql(
                &service,
                &admin.database,
                &format!("REVOKE ALL ON DATABASE {} FROM PUBLIC;", ident(database)),
                &redactor,
            )
            .await?;
        }
        let existing=self.preview_sql(&service,&admin.database,&format!("SELECT pg_get_userbyid(datdba)||'|'||coalesce(shobj_description(oid,'pg_database'),'') FROM pg_database WHERE datname={};",literal(&binding.database)),&redactor).await?;
        if existing.is_empty() {
            // No non-superuser can connect during the CREATE/ACL crash window.
            self.preview_sql(
                &service,
                &admin.database,
                &format!(
                    "CREATE DATABASE {} OWNER {} TEMPLATE template0 CONNECTION LIMIT 0;",
                    ident(&binding.database),
                    ident(&owner)
                ),
                &redactor,
            )
            .await?;
        } else {
            let (actual_owner, comment) = existing.split_once('|').unwrap_or(("", ""));
            if actual_owner != owner || (!comment.is_empty() && comment != marker(&binding.id)) {
                return Err(Error::Conflict(
                    "refusing to adopt an unowned preview PostgreSQL database".into(),
                ));
            }
        }
        self.preview_sql(&service,&admin.database,&format!("COMMENT ON DATABASE {} IS {}; REVOKE ALL ON DATABASE {} FROM PUBLIC; GRANT CONNECT,CREATE,TEMPORARY ON DATABASE {} TO {};",ident(&binding.database),literal(&marker(&binding.id)),ident(&binding.database),ident(&binding.database),ident(&binding.username)),&redactor).await?;
        self.preview_sql(
            &service,
            &binding.database,
            &format!(
                "REVOKE ALL ON SCHEMA public FROM PUBLIC; GRANT USAGE,CREATE ON SCHEMA public TO {};",
                ident(&binding.username)
            ),
            &redactor,
        )
        .await?;
        self.preview_sql(
            &service,
            &admin.database,
            &format!(
                "ALTER DATABASE {} CONNECTION LIMIT {};",
                ident(&binding.database),
                preview_db::CONNECTION_LIMIT
            ),
            &redactor,
        )
        .await?;
        self.preview_job_active(app, d, &binding, &cancel)?;
        let network = preview_network::name(&app.id, pr);
        preview_network::ensure(&self.deployer.docker, &network).await?;
        let state = self
            .deployer
            .docker
            .inspect(&service.container)
            .await?
            .ok_or_else(|| Error::Unavailable("preview PostgreSQL service vanished".into()))?;
        let base = self
            .store
            .with(|c| preview_db::service_network(c, &service.id))?
            .ok_or_else(|| Error::Internal("preview PostgreSQL network ownership missing".into()))?;
        let allowed = self.store.with(|c| preview_db::resources(c, &service.id))?;
        if state.ips.keys().any(|net| {
            net != &base
                && !allowed
                    .iter()
                    .any(|r| *net == preview_network::name(&r.app_id, r.pr_number))
        }) {
            return Err(Error::Conflict(
                "preview PostgreSQL is attached to an unexpected network".into(),
            ));
        }
        if !state.ips.contains_key(&network) {
            let output = self
                .deployer
                .docker
                .run(
                    &proc::args(&["network", "connect", &network, &service.container]),
                    Duration::from_secs(30),
                )
                .await?;
            if !output.success() {
                return Err(Error::Unavailable(format!(
                    "connect preview PostgreSQL: {}",
                    output.tail(2)
                )));
            }
        }
        if binding.state == "ready" && binding.deployment_id == d.id {
            return Ok(Some(binding.resource));
        }
        self.store
            .tx(|c| preview_db::set_state(c, &binding.id, "provisioned", ""))?;
        if !self.deployer.docker.image_exists(image).await? {
            self.deployer.docker.pull(image).await?;
        }
        for (phase, command) in [("migration", &settings.migration), ("seed", &settings.seed)] {
            let Some(command) = command else { continue };
            self.preview_job_active(app, d, &binding, &cancel)?;
            log.info(&format!("running isolated preview database {phase}"));
            let name = format!("vfy-preview-db-task-{}-{phase}", d.id);
            let mut labels = BTreeMap::from([
                (MANAGED_LABEL.into(), MANAGED.into()),
                (deploy::LABEL_APP.into(), app.id.clone()),
                (deploy::LABEL_APP_NAME.into(), app.name.clone()),
                (deploy::LABEL_DEPLOYMENT.into(), d.id.clone()),
                (deploy::LABEL_PR.into(), pr.to_string()),
                (deploy::LABEL_KIND.into(), "preview-db-task".into()),
            ]);
            labels.insert("vercelify.preview-db-resource".into(), binding.id.clone());
            self.deployer.docker.remove(&name).await?;
            self.deployer
                .docker
                .create(&ContainerSpec {
                    name: name.clone(),
                    image: image.into(),
                    labels,
                    env: env.clone(),
                    networks: vec![network.clone()],
                    isolated_preview: true,
                    memory_mb: Some(app.runtime.memory_mb.unwrap_or(256)),
                    cpus: Some(app.runtime.cpus.unwrap_or(1.0)),
                    command: Some(vec!["-ec".into(), command.clone()]),
                    entrypoint: Some("sh".into()),
                    restart: "no".into(),
                    cgroup_parent: self.deployer.cgroup_parent.clone(),
                    ..Default::default()
                })
                .await?;
            let output = proc::stream_clean(
                Spec {
                    bin: &self.deployer.docker.bin,
                    args: &proc::args(&["start", "--attach", &name]),
                    env: &[],
                    cwd: None,
                    timeout: Duration::from_secs(300),
                },
                cancel.clone(),
                &mut |stream, line| log.line(stream, line),
            )
            .await;
            let state = self.deployer.docker.inspect(&name).await;
            let removed = self.deployer.docker.remove(&name).await;
            let output = output?;
            let state = state?.ok_or_else(|| Error::Unavailable("preview database task vanished".into()))?;
            removed?;
            if !output.success() || state.exit_code != 0 {
                let reason = log.redact_text(&format!("preview database {phase} failed: {}", output.tail(8)));
                let _ = self
                    .store
                    .tx(|c| preview_db::set_state(c, &binding.id, "provisioned", &reason));
                return Err(Error::Invalid(reason));
            }
        }
        self.preview_job_active(app, d, &binding, &cancel)?;
        self.store.tx(|c| {
            preview_db::set_state(c, &binding.id, "ready", "")?;
            c.execute(
                "UPDATE preview_databases SET deployment_id=?2 WHERE id=?1",
                params![binding.id, d.id],
            )?;
            Ok(())
        })?;
        Ok(Some(self.store.with(|c| preview_db::get(c, &binding.id))?))
    }
}

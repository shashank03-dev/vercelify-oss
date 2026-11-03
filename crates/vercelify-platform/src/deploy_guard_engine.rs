//! Resumable guard execution. A partition is unknown; image recovery is never
//! database recovery, and enqueue is never reported as verified serving.
use crate::{
    apps, deploy,
    deploy_guard::{self, Guard, Observation},
    deployments,
    engine::{Engine, EngineEvent, Enqueue},
    health,
    runtime::HealthWait,
    Error, Result,
};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vercelify_core::platform::{App, Deployment, DeploymentStatus, DeploymentTrigger, Desired};

#[derive(Debug, serde::Serialize)]
pub struct ServingCheck {
    pub app_id: String,
    pub deployment_id: Option<String>,
    pub desired: Desired,
    pub runtime: Option<bool>,
    pub ingress: Option<bool>,
}

impl Engine {
    /// Read-only update/doctor probe. It never starts stopped/sleeping apps.
    pub async fn serving_check(&self, app: &App) -> Result<ServingCheck> {
        let mut check = ServingCheck {
            app_id: app.id.clone(),
            deployment_id: app.current_deployment_id.clone(),
            desired: app.desired,
            runtime: None,
            ingress: None,
        };
        if app.desired != Desired::Running {
            return Ok(check);
        }
        let Some(id) = &app.current_deployment_id else {
            return Ok(check);
        };
        let Ok(_operation) = self.app_operation(&app.id) else {
            return Ok(check);
        };
        let d = self.store.with(|c| deployments::get(c, id))?;
        check.runtime = match self.observe_guard_runtime(app, &d, None).await? {
            Observation::Healthy => Some(true),
            Observation::Failed { .. } => Some(false),
            Observation::Unknown => None,
        };
        if check.runtime == Some(true) {
            check.ingress = Some(self.guard_ingress(app, &d).await);
        }
        let after = self.store.with(|c| apps::get(c, &app.id))?;
        if after.current_deployment_id != app.current_deployment_id
            || after.desired != app.desired
            || after.runtime != app.runtime
        {
            check.runtime = None;
            check.ingress = None;
        }
        Ok(check)
    }
    pub(crate) async fn run_deploy_guards(self: Arc<Self>) {
        loop {
            for error in self.poll_deploy_guards().await {
                tracing::warn!("deploy guard: {error}")
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }
    /// Also exposed to deterministic contract runners; all state remains in SQLite.
    pub async fn poll_deploy_guards(&self) -> Vec<String> {
        let guards = match self.store.with(|c| deploy_guard::list(c, None, true)) {
            Ok(g) => g,
            Err(e) => return vec![e.to_string()],
        };
        let mut errors = vec![];
        for guard in guards {
            if let Err(error) = self.poll_deploy_guard(&guard).await {
                errors.push(format!("{}: {error}", guard.deployment_id));
            }
        }
        errors
    }
    fn guard_finish(&self, g: &Guard, state: &str, reason: &str) -> Result<()> {
        self.store
            .tx(|c| deploy_guard::finish(c, &g.deployment_id, state, reason, chrono::Utc::now().timestamp()))?;
        self.emit(EngineEvent::Changed);
        Ok(())
    }
    async fn poll_deploy_guard(&self, g: &Guard) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let app = self.store.with(|c| apps::get(c, &g.app_id))?;
        let current = app.current_deployment_id.as_deref();
        let expected = current == Some(g.deployment_id.as_str())
            || (g.state == "requested" && current == g.rollback_id.as_deref());
        let newer = self.store.with(|c| {
            Ok(deployments::active(c)?.iter().any(|d| {
                d.app_id == g.app_id && d.pr_number.is_none() && Some(d.id.as_str()) != g.rollback_id.as_deref()
            }))
        })?;
        if app.desired != Desired::Running || !expected || newer {
            return self.guard_finish(
                g,
                "cancelled",
                "new work or intentional workload state superseded this guard",
            );
        }
        if g.state == "watching" && now >= g.deadline {
            return self.guard_finish(g, "expired", "observation window ended");
        }
        if g.state == "requested" && now >= g.requested_at.unwrap_or(now) + deploy_guard::RECOVERY_SECS {
            return self.guard_finish(
                g,
                "failed",
                "rollback did not verify serving before the recovery deadline",
            );
        }
        if !self.store.with(|c| deploy_guard::compatible(c, &self.key, g))? {
            return self.guard_finish(
                g,
                "cancelled",
                "runtime, variables, storage or placement changed; manual recovery required",
            );
        }
        // The deployment queue owns this same app operation while it mutates
        // runtime state. Never overlap publication/cleanup with observation.
        let Ok(_operation) = self.app_operation(&app.id) else {
            return Ok(());
        };
        if g.state == "requested" {
            let Some(id) = &g.rollback_id else {
                return self.guard_finish(g, "failed", "rollback job identity is missing");
            };
            let rollback = self.store.with(|c| deployments::get(c, id))?;
            match rollback.status {
                DeploymentStatus::Failed => {
                    return self.guard_finish(g, "failed", "rollback deployment failed; manual recovery required")
                }
                DeploymentStatus::Cancelled | DeploymentStatus::Superseded => {
                    return self.guard_finish(g, "cancelled", "rollback job was superseded or cancelled")
                }
                DeploymentStatus::Healthy
                    if current == Some(id.as_str())
                        && matches!(
                            self.observe_guard_runtime(&app, &rollback, None).await?,
                            Observation::Healthy
                        )
                        && self.guard_ingress(&app, &rollback).await =>
                {
                    self.guard_finish(
                        g,
                        "verified",
                        "previous compatible image passed runtime health and local ingress",
                    )?;
                    let failed = self.store.with(|c| deployments::get(c, &g.deployment_id))?;
                    self.emit(EngineEvent::Failed {
                        app: app.name.clone(),
                        deployment: failed,
                        error: format!(
                            "previous compatible version verified serving (deployment #{})",
                            rollback.number
                        ),
                    });
                }
                _ => {}
            }
            return Ok(());
        }
        let d = self.store.with(|c| deployments::get(c, &g.deployment_id))?;
        let observation = self.observe_guard_runtime(&app, &d, Some(g)).await?;
        if !self.store.tx(|c| deploy_guard::observe(c, &d.id, observation, now))? {
            return Ok(());
        }
        let previous = self.store.with(|c| deployments::get(c, &g.previous_id))?;
        if !deploy_guard::immutable_image(&app.name, &previous)
            || !self.deployer.docker.image_exists(&previous.image).await?
        {
            return self.guard_finish(
                g,
                "failed",
                "previous immutable image is not retained; manual recovery required",
            );
        }
        // Guard state and rollback identity change in the enqueue transaction.
        // A crash before commit requests nothing; after commit resumes this job.
        match self.enqueue(
            &app.id,
            Enqueue {
                trigger: Some(DeploymentTrigger::AutoRollback),
                rollback_of: Some(g.previous_id.clone()),
                guard_request: Some(g.deployment_id.clone()),
                ..Default::default()
            },
        ) {
            Ok(rollback) => {
                let mut log = self.logs.writer(&app.name, &d.id);
                log.error(&format!(
                    "post-deploy failure: rollback requested (deployment #{}); serving verification pending",
                    rollback.number
                ));
                self.emit(EngineEvent::Failed {
                    app: app.name,
                    deployment: d,
                    error: format!(
                        "rollback requested (deployment #{}); serving verification pending",
                        rollback.number
                    ),
                });
                Ok(())
            }
            Err(Error::Conflict(_)) => {
                self.guard_finish(g, "cancelled", "new work or changed configuration fenced rollback")
            }
            Err(error) => Err(error),
        }
    }
    async fn observe_guard_runtime(&self, app: &App, d: &Deployment, g: Option<&Guard>) -> Result<Observation> {
        let placed = self.placed(&d.id)?;
        if placed.is_empty() {
            return Ok(Observation::Unknown);
        }
        let (port, _) = self.store.with(|c| deployments::built_info(c, &d.id))?;
        let port = app.runtime.port.or(port).unwrap_or(3000);
        let mut failed = None;
        for p in placed {
            if p.node.role != "main" && !p.node.online {
                return Ok(Observation::Unknown);
            }
            let Ok(rt) = self.node_runtime(&p.node) else {
                return Ok(Observation::Unknown);
            };
            let i = match rt.inspect(&p.instance.container).await {
                Ok(Some(i)) if !i.paused => i,
                _ => return Ok(Observation::Unknown),
            };
            if let Some(g) = g {
                let Some(base) = g.baseline.get(&format!("{}/{}", p.node.id, p.instance.container)) else {
                    return Ok(Observation::Unknown);
                };
                if i.restart_count.saturating_sub(*base) >= 2 {
                    failed = Some(Observation::Failed {
                        reason: if i.oom_killed {
                            "crash loop with explicit OOM evidence".into()
                        } else {
                            "two or more restarts after publication".into()
                        },
                        crash_loop: true,
                    });
                    continue;
                }
            }
            if !i.running {
                failed = Some(Observation::Failed {
                    reason: if i.oom_killed {
                        "container stopped with explicit OOM evidence".into()
                    } else {
                        format!("container exited with code {}", i.exit_code)
                    },
                    crash_loop: false,
                });
                continue;
            }
            match rt
                .wait_healthy(&HealthWait {
                    container: p.instance.container,
                    port,
                    probe: deploy::probe_for(app.runtime.health.as_ref()),
                    timeout_secs: 1,
                })
                .await
            {
                Ok(()) => {}
                Err(Error::Unavailable(_)) => return Ok(Observation::Unknown),
                Err(_) => {
                    // Health command output may contain secrets: evidence is a
                    // classification, never unredacted process stderr.
                    if failed.is_none() {
                        failed = Some(Observation::Failed {
                            reason: "declared health probe failed while the container was running".into(),
                            crash_loop: false,
                        });
                    }
                }
            }
        }
        Ok(failed.unwrap_or(Observation::Healthy))
    }
    async fn guard_ingress(&self, app: &App, d: &Deployment) -> bool {
        let hosts = match self.store.with(apps::domain_settings) {
            Ok(settings) => crate::engine::hosts_for(&settings, app, None).0,
            Err(_) => return false,
        };
        let Some(host) = hosts.first() else { return false };
        let path = match deploy::probe_for(app.runtime.health.as_ref()) {
            health::Probe::Http { path } => path,
            _ => "/".into(),
        };
        let future = async {
            let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", self.deployer.proxy.http_port))
                .await
                .ok()?;
            stream
                .write_all(
                    format!(
                        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                        host.host
                    )
                    .as_bytes(),
                )
                .await
                .ok()?;
            let mut buffer = [0u8; 64];
            let n = stream.read(&mut buffer).await.ok()?;
            let code = String::from_utf8_lossy(&buffer[..n])
                .split_whitespace()
                .nth(1)?
                .parse::<u16>()
                .ok()?;
            Some((200..400).contains(&code))
        };
        let _ = d; // The caller has already fenced the current rollback identity.
        tokio::time::timeout(Duration::from_secs(5), future)
            .await
            .ok()
            .flatten()
            == Some(true)
    }
}

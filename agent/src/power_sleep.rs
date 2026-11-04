//! Battery stages reuse explicit sleep opt-ins and the admitted engine wake path.
use super::{Asleep, Sleep, Target};
use vercelify_core::{platform::Desired, PowerGuardState};
use vercelify_platform::{apps, previews};

fn state(cfg: &crate::config::Config) -> Option<PowerGuardState> {
    let raw = std::fs::read_to_string(&cfg.power_guard_state).ok()?;
    let state: PowerGuardState = serde_json::from_str(&raw).ok()?;
    let time = chrono::DateTime::parse_from_rfc3339(&state.updated_at)
        .ok()?
        .timestamp();
    let age = chrono::Utc::now().timestamp() - time;
    (0..=90).contains(&age).then_some(state)
}
fn selected(stage: &str, pr: u64) -> bool {
    stage == "battery_app_sleep" || stage == "battery_preview_sleep" && pr > 0
}
fn current(engine: &vercelify_platform::engine::Engine, t: &Target, asleep: bool) -> Option<String> {
    engine
        .store
        .with(|c| {
            let app = apps::get(c, &t.app_uuid)?;
            if app.desired == Desired::Stopped {
                return Ok(None);
            }
            if t.pr == 0 {
                if (app.desired == Desired::Sleeping) != asleep {
                    return Ok(None);
                }
                Ok(app.current_deployment_id)
            } else {
                Ok(previews::get(c, &t.app_uuid, t.pr as u32)?
                    .filter(|p| p.status == "active" && p.asleep == asleep)
                    .and_then(|p| p.deployment_id))
            }
        })
        .ok()
        .flatten()
}
impl Sleep {
    pub(super) async fn power_blocks_wake(&self, key: &str) -> bool {
        let power_owned = self
            .store
            .lock()
            .await
            .asleep
            .get(key)
            .is_some_and(|a| a.reason == "battery");
        power_owned && !state(&self.cfg).is_some_and(|s| s.state == "ok")
    }
    pub(super) async fn power_tick(&self, targets: &[Target]) -> bool {
        let Some(engine) = self.engine.get() else { return false };
        let power = state(&self.cfg);
        let stage = power.as_ref().map_or("unknown", |s| s.state.as_str());
        let entries: Vec<_> = self
            .store
            .lock()
            .await
            .asleep
            .iter()
            .filter(|(_, a)| a.reason == "battery")
            .map(|(k, a)| (k.clone(), a.deployment_id.clone()))
            .collect();
        if stage == "ok" {
            for (key, generation) in entries {
                let target = targets.iter().find(|t| t.key == key);
                let valid = target
                    .and_then(|t| current(engine, t, true))
                    .is_some_and(|id| Some(id) == generation);
                if valid {
                    self.wake_key(&key, "stable AC restored; resuming power-slept workload")
                        .await;
                } else {
                    let mut store = self.store.lock().await;
                    store.asleep.remove(&key);
                    self.save(&store);
                }
            }
            return false;
        }
        if !matches!(stage, "battery_preview_sleep" | "battery_app_sleep") {
            return !entries.is_empty() || stage.starts_with("battery_");
        }
        let Some(rows) = self.docker_rows().await else {
            return true;
        };
        // Battery policy has already held/paused build work at earlier tiers.
        // A paused build remains Building; it must not prevent conservation.
        for t in targets.iter().filter(|t| selected(stage, t.pr)) {
            let previous = self.store.lock().await.asleep.get(&t.key).cloned();
            if let Some(previous) = previous {
                if previous.reason == "battery" {
                    let still_owned =
                        current(engine, t, true).or_else(|| current(engine, t, false)) == previous.deployment_id;
                    if still_owned {
                        // Finish a journaled stop after process exit or a transient
                        // Docker failure; this never creates a fresh deployment.
                        if let Err(error) = engine.sleep(&t.app_uuid, (t.pr > 0).then_some(t.pr as u32)).await {
                            tracing::warn!("retrying battery sleep {}: {error}", t.key);
                        }
                    }
                }
                continue;
            }
            let Some(deployment_id) = current(engine, t, false) else {
                continue;
            };
            let containers: Vec<_> = super::containers_for(&rows, &t.app_uuid, t.pr)
                .into_iter()
                .filter(|(_, running)| *running)
                .map(|(id, _)| id)
                .collect();
            if containers.is_empty() {
                continue;
            }
            // Journal intent before the engine persists Sleeping, so a process
            // exit between durable state and Docker stop remains recoverable.
            {
                let mut store = self.store.lock().await;
                store.asleep.insert(
                    t.key.clone(),
                    Asleep {
                        containers,
                        since: super::now(),
                        reason: "battery".into(),
                        deployment_id: Some(deployment_id),
                    },
                );
                let persisted = serde_json::to_vec(&*store)
                    .ok()
                    .and_then(|data| crate::watch::write_private(&Self::path(&self.cfg), &data).ok());
                if persisted.is_none() {
                    store.asleep.remove(&t.key);
                    tracing::warn!(
                        "battery sleep {}: journal could not be persisted; leaving workload awake",
                        t.key
                    );
                    continue;
                }
            }
            if let Err(error) = engine.sleep(&t.app_uuid, (t.pr > 0).then_some(t.pr as u32)).await {
                tracing::warn!("battery sleep {}: {error}", t.key);
            }
            self.events.changed("sleep");
        }
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn battery_selection_requires_the_preview_tier_before_production_tier() {
        assert!(!selected("battery_work_paused", 7));
        assert!(selected("battery_preview_sleep", 7));
        assert!(!selected("battery_preview_sleep", 0));
        assert!(selected("battery_app_sleep", 0));
        assert!(!selected("unknown", 0));
    }
    #[tokio::test]
    async fn power_sleep_reason_survives_restart_and_requires_a_fresh_recovery_state() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let cfg = Arc::new(crate::config::Config {
            state_dir: dir.path().join("state"),
            power_guard_state: dir.path().join("power"),
            ..Default::default()
        });
        let events = Arc::new(crate::events::Events::default());
        let jobs = Arc::new(crate::events::Jobs::load(&cfg, events.clone()));
        let sleep = Sleep::load(cfg.clone(), events.clone(), jobs.clone());
        {
            let mut store = sleep.store.lock().await;
            store.asleep.insert(
                "power".into(),
                Asleep {
                    containers: vec!["owned".into()],
                    since: super::super::now(),
                    reason: "battery".into(),
                    deployment_id: Some("dpl_old".into()),
                },
            );
            store.asleep.insert("idle".into(), Asleep::default());
            sleep.save(&store);
        }
        let resumed = Sleep::load(cfg.clone(), events, jobs);
        assert!(
            resumed.power_blocks_wake("power").await,
            "missing guard is not recovery"
        );
        assert!(
            !resumed.power_blocks_wake("idle").await,
            "ordinary idle sleep keeps its behavior"
        );
        for (status, age, blocked) in [
            ("battery_recovery", 0, true),
            ("ok", 91, true),
            ("ok", -60, true),
            ("ok", 0, false),
        ] {
            let updated = (chrono::Utc::now() - chrono::Duration::seconds(age)).to_rfc3339();
            std::fs::write(
                &cfg.power_guard_state,
                serde_json::json!({"state":status,"reason":"test","updated_at":updated}).to_string(),
            )
            .unwrap();
            assert_eq!(resumed.power_blocks_wake("power").await, blocked);
        }
    }
    #[tokio::test]
    async fn persisted_preview_generation_and_manual_stop_fence_power_recovery() {
        use vercelify_core::platform::*;
        use vercelify_platform::deployments;
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::Config {
            state_dir: dir.path().join("state"),
            platform_db: dir.path().join("platform.db"),
            power_guard_state: dir.path().join("power"),
            master_key_file: dir.path().join("master.key"),
            ..Default::default()
        };
        let platform = crate::platform::Platform::open(&cfg).unwrap();
        let app=platform.store.tx(|c|apps::create(c,&serde_json::from_value(serde_json::json!({"name":"battery-generation","kind":"image","source":{"image":"fixture:v1"},"previews":{"enabled":true,"access":"private","max":3}})).unwrap())).unwrap();
        let serving = |sha: &str| {
            platform
                .store
                .tx(|c| {
                    previews::upsert(c, &app.id, 7, sha, "power fixture")?;
                    let d = deployments::create(
                        c,
                        &deployments::NewDeployment {
                            app_id: &app.id,
                            pr_number: Some(7),
                            branch: "main",
                            commit_sha: sha,
                            commit_message: "",
                            trigger: DeploymentTrigger::Manual,
                            rollback_of: None,
                            image: "fixture:v1",
                            node_id: None,
                        },
                    )?;
                    deployments::transition(c, &d.id, DeploymentStatus::Deploying, None)?;
                    deployments::transition(c, &d.id, DeploymentStatus::Healthy, None)?;
                    previews::set_serving(c, &app.id, 7, &d.id, "http://private.vfy")?;
                    previews::set_asleep(c, &app.id, 7, true)?;
                    Ok(d.id)
                })
                .unwrap()
        };
        let events = std::sync::Arc::new(crate::events::Events::default());
        let jobs = std::sync::Arc::new(crate::events::Jobs::load(&cfg, events.clone()));
        let sleeper = Sleep::load(std::sync::Arc::new(cfg.clone()), events, jobs);
        sleeper.set_engine(platform.engine.clone());
        {
            let mut store = sleeper.store.lock().await;
            store.asleep.insert(
                "battery-generation-pr-7".into(),
                Asleep {
                    reason: "battery".into(),
                    ..Default::default()
                },
            );
        }
        // Even once AC reports okay, changing settings must not discard the
        // pending recovery intent before the automatic wake completes.
        std::fs::write(
            &cfg.power_guard_state,
            serde_json::json!({"state":"ok","reason":"stable AC","updated_at":super::super::now()}).to_string(),
        )
        .unwrap();
        let request = vercelify_core::SleepRequest {
            enabled: false,
            idle_minutes: None,
            previews: None,
        };
        assert!(sleeper.set(&app.id, request).await.is_err());
        let old = serving("aaaaaaa");
        let t = Target {
            key: "battery-generation-pr-7".into(),
            app_uuid: app.id.clone(),
            pr: 7,
            hosts: vec![],
            idle: std::time::Duration::ZERO,
        };
        assert_eq!(current(&platform.engine, &t, true).as_deref(), Some(old.as_str()));
        let new = serving("bbbbbbb");
        assert_ne!(current(&platform.engine, &t, true).as_deref(), Some(old.as_str()));
        assert_eq!(current(&platform.engine, &t, true).as_deref(), Some(new.as_str()));
        platform
            .store
            .tx(|c| {
                apps::patch(
                    c,
                    &app.id,
                    &AppPatch {
                        desired: Some(Desired::Stopped),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        assert!(current(&platform.engine, &t, true).is_none());
    }
}

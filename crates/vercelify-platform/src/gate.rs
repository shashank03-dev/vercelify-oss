//! When a build may start (design §6.1): at most `concurrency` at once, and
//! none while the power guard says the machine is too hot or shutting down.
//! A build already running when it gets hot is paused by the power guard
//! (it freezes the BuildKit container); this gate keeps new ones from
//! starting until it cools down.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub struct BuildGate {
    slots: Arc<Semaphore>,
    concurrency: usize,
    builds_enabled: bool,
    power_state: PathBuf,
    poll: Duration,
    last_finished: Mutex<Instant>,
}

/// Records completion on every exit path, including cancellation and failure.
pub struct BuildPermit<'a> {
    gate: &'a BuildGate,
    _slot: OwnedSemaphorePermit,
}

impl Drop for BuildPermit<'_> {
    fn drop(&mut self) {
        *self.gate.last_finished.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
    }
}

/// Power guard states that hold builds back.
pub fn holds_builds(state: &str) -> bool {
    matches!(
        state,
        "thermal_paused"
            | "shutting_down"
            | "battery_build_hold"
            | "battery_work_paused"
            | "battery_preview_sleep"
            | "battery_app_sleep"
            | "battery_recovery"
    )
}

impl BuildGate {
    pub fn new(concurrency: usize, power_state: PathBuf) -> Self {
        let builds_enabled = concurrency > 0;
        let concurrency = concurrency.max(1);
        Self {
            slots: Arc::new(Semaphore::new(concurrency)),
            concurrency,
            builds_enabled,
            power_state,
            poll: Duration::from_secs(5),
            last_finished: Mutex::new(Instant::now()),
        }
    }

    pub fn with_poll(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }

    pub fn builds_enabled(&self) -> bool {
        self.builds_enabled
    }

    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// The power guard's state, if it is running.
    pub fn power_state(&self) -> Option<String> {
        let raw = std::fs::read_to_string(&self.power_state).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        v.get("state")?.as_str().map(String::from)
    }

    /// Why builds are held right now, if they are.
    pub fn held(&self) -> Option<String> {
        self.power_state().filter(|s| holds_builds(s))
    }

    /// Hold every slot while maintenance inspects/stops the idle builder.
    /// Nonblocking: queued and active builds take priority. Zero disables it.
    pub fn try_idle(&self, timeout: Duration) -> Option<OwnedSemaphorePermit> {
        if timeout.is_zero() || self.held().is_some() {
            return None;
        }
        let slots = u32::try_from(self.concurrency).ok()?;
        let permit = self.slots.clone().try_acquire_many_owned(slots).ok()?;
        if self.held().is_some() || self.last_finished.lock().unwrap_or_else(|p| p.into_inner()).elapsed() < timeout {
            return None;
        }
        Some(permit)
    }

    /// Wait for a slot and for the machine to be fit to build. `on_wait` is
    /// called once if the wait is for the power guard (for the build log).
    pub async fn acquire(&self, on_wait: &mut (dyn FnMut(&str) + Send)) -> BuildPermit<'_> {
        let mut told = false;
        loop {
            if let Some(state) = self.held() {
                if !told {
                    on_wait(&format!("waiting: the power guard says {state}"));
                    told = true;
                }
                tokio::time::sleep(self.poll).await;
                continue;
            }
            let permit = self.slots.clone().acquire_owned().await.expect("never closed");
            // It may have got hot while waiting for the slot.
            if self.held().is_none() {
                return BuildPermit {
                    gate: self,
                    _slot: permit,
                };
            }
            drop(permit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Removing the all-slot lock or measuring from acquire instead of drop
    // lets maintenance stop a running or just-completed build.
    #[tokio::test]
    async fn idle_maintenance_excludes_builds_and_counts_from_completion() {
        let dir = tempfile::tempdir().unwrap();
        let gate = BuildGate::new(2, dir.path().join("power"));
        let timeout = Duration::from_millis(40);
        assert!(gate.try_idle(timeout).is_none());
        let build = gate.acquire(&mut |_| {}).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(gate.try_idle(timeout).is_none(), "even one of two slots is active");
        drop(build);
        assert!(gate.try_idle(timeout).is_none(), "timeout begins on completion");
        tokio::time::sleep(Duration::from_millis(60)).await;
        let maintenance = gate.try_idle(timeout).expect("idle");
        assert!(
            tokio::time::timeout(Duration::from_millis(20), gate.acquire(&mut |_| {}))
                .await
                .is_err()
        );
        drop(maintenance);
        assert!(
            gate.try_idle(timeout).is_some(),
            "maintenance does not count as a build"
        );
        assert!(gate.try_idle(Duration::ZERO).is_none(), "zero disables stopping");
        std::fs::write(dir.path().join("power"), r#"{"state":"thermal_paused"}"#).unwrap();
        assert!(gate.try_idle(timeout).is_none(), "power guard owns a paused builder");
    }

    #[tokio::test]
    async fn concurrency_and_power_state() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("power-guard.state");
        let gate = Arc::new(BuildGate::new(1, state.clone()).with_poll(Duration::from_millis(20)));
        let p1 = gate.acquire(&mut |_| {}).await;
        let g2 = gate.clone();
        let second = tokio::spawn(async move {
            let _p = g2.acquire(&mut |_| {}).await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!second.is_finished(), "one build at a time");
        drop(p1);
        second.await.unwrap();

        std::fs::write(&state, r#"{"state":"thermal_paused"}"#).unwrap();
        assert_eq!(gate.held().as_deref(), Some("thermal_paused"));
        let g3 = gate.clone();
        let waiting = tokio::spawn(async move {
            let mut msgs = vec![];
            let _p = g3.acquire(&mut |m| msgs.push(m.to_string())).await;
            msgs
        });
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(!waiting.is_finished(), "held while hot");
        std::fs::write(&state, r#"{"state":"ok"}"#).unwrap();
        let msgs = waiting.await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("thermal_paused"));
        assert!(!holds_builds("warning"), "legacy warning does not hold builds");
        for state in [
            "battery_build_hold",
            "battery_work_paused",
            "battery_preview_sleep",
            "battery_app_sleep",
            "battery_recovery",
        ] {
            assert!(holds_builds(state));
        }
    }
}

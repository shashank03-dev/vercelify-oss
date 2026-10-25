//! When a build may start (design §6.1): at most `concurrency` at once, and
//! none while the power guard says the machine is too hot or shutting down.
//! A build already running when it gets hot is paused by the power guard
//! (it freezes the BuildKit container); this gate keeps new ones from
//! starting until it cools down.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub struct BuildGate {
    slots: Arc<Semaphore>,
    concurrency: usize,
    power_state: PathBuf,
    poll: Duration,
}

/// Power guard states that hold builds back.
pub fn holds_builds(state: &str) -> bool {
    matches!(state, "thermal_paused" | "shutting_down")
}

impl BuildGate {
    pub fn new(concurrency: usize, power_state: PathBuf) -> Self {
        let concurrency = concurrency.max(1);
        Self {
            slots: Arc::new(Semaphore::new(concurrency)),
            concurrency,
            power_state,
            poll: Duration::from_secs(5),
        }
    }

    pub fn with_poll(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
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

    /// Wait for a slot and for the machine to be fit to build. `on_wait` is
    /// called once if the wait is for the power guard (for the build log).
    pub async fn acquire(&self, on_wait: &mut (dyn FnMut(&str) + Send)) -> OwnedSemaphorePermit {
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
                return permit;
            }
            drop(permit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrency_and_power_state() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("power-guard.state");
        let gate = Arc::new(BuildGate::new(1, state.clone()).with_poll(Duration::from_millis(20)));
        let p1 = gate.acquire(&mut |_| {}).await;
        let g2 = gate.clone();
        let second = tokio::spawn(async move { g2.acquire(&mut |_| {}).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!second.is_finished(), "one build at a time");
        drop(p1);
        let p2 = second.await.unwrap();
        drop(p2);

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
        assert!(!holds_builds("warning"), "on battery builds still run");
    }
}

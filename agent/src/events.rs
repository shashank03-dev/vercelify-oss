//! Live events and the job model.
//!
//! Everything that takes longer than a request (a backup, a volume move, a
//! model download, an automatic deploy, an app going to sleep) is a [`Job`]
//! with one shape. Jobs and alerts are pushed to `GET /v1/events`, a
//! newline-delimited JSON stream the cockpit keeps open, so it can show
//! progress and notifications as they happen instead of polling for them.

use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use vercelify_core::{AgentEvent, Alert, Job, JobStatus};

/// Finished jobs kept for the activity list.
const MAX_JOBS: usize = 60;

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub struct Events {
    tx: broadcast::Sender<AgentEvent>,
    seq: AtomicU64,
}

impl Default for Events {
    fn default() -> Self {
        Self {
            tx: broadcast::channel(256).0,
            seq: AtomicU64::new(0),
        }
    }
}

impl Events {
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.tx.subscribe()
    }

    pub fn event(&self, kind: &str) -> AgentEvent {
        AgentEvent {
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            at: now(),
            kind: kind.into(),
            alert: None,
            job: None,
            topic: None,
        }
    }

    fn send(&self, e: AgentEvent) {
        // No subscribers is fine: nobody has the cockpit open.
        let _ = self.tx.send(e);
    }

    pub fn alert(&self, a: &Alert) {
        let mut e = self.event("alert");
        e.alert = Some(a.clone());
        self.send(e);
    }

    pub fn job(&self, j: &Job) {
        let mut e = self.event("job");
        e.job = Some(j.clone());
        self.send(e);
    }

    /// Something the cockpit shows changed; it should fetch `topic` again.
    pub fn changed(&self, topic: &str) {
        let mut e = self.event("changed");
        e.topic = Some(topic.into());
        self.send(e);
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    next: u64,
    jobs: VecDeque<Job>,
}

/// Running and recent jobs. Persisted, so the activity list survives an
/// agent restart (jobs that were running then are closed on load).
pub struct Jobs {
    path: PathBuf,
    store: Mutex<Store>,
    events: Arc<Events>,
}

/// A running job; finish it with [`JobHandle::done`] or [`JobHandle::fail`].
/// Dropped unfinished (a panic, an early return), it is marked failed.
pub struct JobHandle {
    id: String,
    jobs: Arc<Jobs>,
    finished: bool,
}

impl Jobs {
    pub fn load(cfg: &Config, events: Arc<Events>) -> Self {
        let path = cfg.state_dir.join("jobs.json");
        let mut store: Store = std::fs::read_to_string(&path)
            .ok()
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default();
        for j in store.jobs.iter_mut().filter(|j| j.status == JobStatus::Running) {
            // Updates restart the agent on purpose; their result is read back.
            if j.kind != "update" {
                j.status = JobStatus::Failed;
                j.finished_at = Some(now());
                j.detail = Some("interrupted: the agent restarted".into());
            }
        }
        Self {
            path,
            store: Mutex::new(store),
            events,
        }
    }

    fn save(&self, store: &Store) {
        if let Ok(json) = serde_json::to_vec(store) {
            if let Err(e) = crate::watch::write_private(&self.path, &json) {
                tracing::warn!("jobs: could not save: {e}");
            }
        }
    }

    /// Newest first.
    pub fn list(&self) -> Vec<Job> {
        self.store
            .lock()
            .map(|s| s.jobs.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<Job> {
        self.store.lock().ok()?.jobs.iter().find(|j| j.id == id).cloned()
    }

    /// A running job of this kind for this target, if any.
    pub fn running(&self, kind: &str, target: Option<&str>) -> Option<Job> {
        self.store
            .lock()
            .ok()?
            .jobs
            .iter()
            .rev()
            .find(|j| j.status == JobStatus::Running && j.kind == kind && j.target.as_deref() == target)
            .cloned()
    }

    pub fn start(self: &Arc<Self>, kind: &str, title: impl Into<String>, target: Option<&str>) -> JobHandle {
        let job = {
            let mut s = self.store.lock().unwrap_or_else(|e| e.into_inner());
            s.next += 1;
            let job = Job {
                id: format!("j{}-{}", chrono::Utc::now().timestamp(), s.next),
                kind: kind.into(),
                title: title.into(),
                status: JobStatus::Running,
                started_at: now(),
                finished_at: None,
                detail: None,
                progress: None,
                target: target.map(String::from),
            };
            s.jobs.push_back(job.clone());
            while s.jobs.len() > MAX_JOBS {
                // Drop the oldest finished job; never a running one.
                match s.jobs.iter().position(|j| j.status != JobStatus::Running) {
                    Some(i) => {
                        s.jobs.remove(i);
                    }
                    None => break,
                }
            }
            self.save(&s);
            job
        };
        self.events.job(&job);
        JobHandle {
            id: job.id,
            jobs: self.clone(),
            finished: false,
        }
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Job), persist: bool) {
        let job = {
            let Ok(mut s) = self.store.lock() else { return };
            let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) else {
                return;
            };
            f(j);
            let j = j.clone();
            if persist {
                self.save(&s);
            }
            j
        };
        self.events.job(&job);
    }

    /// Close a job found running after a restart (an update).
    pub fn finish_by_id(&self, id: &str, result: Result<String, String>) {
        self.update(id, |j| finish(j, result), true);
    }
}

fn finish(j: &mut Job, result: Result<String, String>) {
    j.finished_at = Some(now());
    match result {
        Ok(d) => {
            j.status = JobStatus::Ok;
            j.progress = j.progress.map(|_| 100.0);
            j.detail = (!d.is_empty()).then_some(d).or(j.detail.take());
        }
        Err(e) => {
            j.status = JobStatus::Failed;
            j.detail = Some(e);
        }
    }
}

impl JobHandle {
    #[cfg(test)]
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn snapshot(&self) -> Option<Job> {
        self.jobs.get(&self.id)
    }

    /// A progress line (and percentage when known). Sent live, saved on finish.
    pub fn progress(&self, detail: impl Into<String>, pct: Option<f32>) {
        let detail = detail.into();
        self.jobs.update(
            &self.id,
            |j| {
                j.detail = Some(detail);
                if pct.is_some() {
                    j.progress = pct;
                }
            },
            false,
        );
    }

    pub fn done(mut self, detail: impl Into<String>) {
        self.finished = true;
        self.jobs.update(&self.id, |j| finish(j, Ok(detail.into())), true);
    }

    pub fn fail(mut self, error: impl Into<String>) {
        self.finished = true;
        self.jobs.update(&self.id, |j| finish(j, Err(error.into())), true);
    }

    /// Keep the job running after this handle is gone (state that outlives
    /// a request, e.g. a deploy followed across checks); finish it later
    /// with [`Jobs::finish_by_id`].
    pub fn detach(mut self) -> String {
        self.finished = true;
        std::mem::take(&mut self.id)
    }

    pub fn finish(self, result: Result<String, String>) {
        match result {
            Ok(d) => self.done(d),
            Err(e) => self.fail(e),
        }
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        if !self.finished {
            self.jobs
                .update(&self.id, |j| finish(j, Err("stopped unexpectedly".into())), true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jobs(dir: &std::path::Path) -> (Arc<Jobs>, Arc<Events>) {
        let cfg = Config {
            state_dir: dir.into(),
            ..Config::default()
        };
        let events = Arc::new(Events::default());
        (Arc::new(Jobs::load(&cfg, events.clone())), events)
    }

    #[tokio::test]
    async fn lifecycle_events_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let (jobs, events) = jobs(dir.path());
        let mut rx = events.subscribe();
        let h = jobs.start("backup", "Backup", None);
        assert_eq!(rx.recv().await.unwrap().job.unwrap().status, JobStatus::Running);
        h.progress("dumping databases", Some(40.0));
        let e = rx.recv().await.unwrap();
        assert_eq!(e.job.as_ref().unwrap().progress, Some(40.0));
        assert!(e.seq > 1);
        let id = h.id().to_string();
        h.done("3 snapshots");
        let j = rx.recv().await.unwrap().job.unwrap();
        assert_eq!(
            (j.status, j.progress, j.detail.as_deref()),
            (JobStatus::Ok, Some(100.0), Some("3 snapshots"))
        );

        // Dropped without finishing = failed.
        drop(jobs.start("volume-move", "Move", Some("7")));
        assert_eq!(jobs.list()[0].status, JobStatus::Failed);

        // Reloaded: history kept, newest first.
        let (again, _) = super::tests::jobs(dir.path());
        let list = again.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].id, id);
    }

    #[test]
    fn interrupted_jobs_are_closed_on_load_except_updates() {
        let dir = tempfile::tempdir().unwrap();
        {
            let (jobs, _) = jobs(dir.path());
            std::mem::forget(jobs.start("llm-pull", "Pull", Some("llama3.2:3b")));
            std::mem::forget(jobs.start("update", "Update", None));
        }
        let (jobs, _) = jobs(dir.path());
        let list = jobs.list();
        assert_eq!(list[0].kind, "update");
        assert_eq!(list[0].status, JobStatus::Running);
        assert_eq!(list[1].status, JobStatus::Failed);
        assert!(jobs.running("update", None).is_some());
        assert!(jobs.running("llm-pull", Some("llama3.2:3b")).is_none());
    }

    #[test]
    fn history_is_bounded_but_running_jobs_stay() {
        let dir = tempfile::tempdir().unwrap();
        let (jobs, _) = jobs(dir.path());
        let first = jobs.start("backup", "long", None);
        for i in 0..(MAX_JOBS + 10) {
            jobs.start("cleanup", format!("c{i}"), None).done("");
        }
        let list = jobs.list();
        assert_eq!(list.len(), MAX_JOBS);
        assert!(list.iter().any(|j| j.id == first.id()));
        first.done("");
    }
}

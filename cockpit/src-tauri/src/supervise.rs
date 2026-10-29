//! Keeps the screen sender streaming until Stop. ffmpeg exits when its SRT
//! connection fails: a handshake half-completed on a lossy link (the
//! receiver drops the silent caller and listens again), or a network outage
//! longer than SRT's idle timeout. Start it again with the same arguments,
//! backing off, and give up after a run of quick failures (no Screen
//! Recording permission, the server gone) so its last output stays visible.

use crate::procs::Processes;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Pace {
    /// How often to look at the process.
    pub poll: Duration,
    /// A run at least this long resets the failure count.
    pub healthy: Duration,
    /// Wait before each restart after consecutive quick failures; past the
    /// end of the list, give up.
    pub delays: &'static [Duration],
}

pub const PACE: Pace = Pace {
    poll: Duration::from_secs(1),
    healthy: Duration::from_secs(10),
    delays: &[
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(4),
        Duration::from_secs(8),
        Duration::from_secs(10),
        Duration::from_secs(10),
        Duration::from_secs(10),
        Duration::from_secs(10),
    ],
};

/// Restart `slot` while `run` is still the current run. Every Start and Stop
/// changes `current` while holding its lock, and a restart holds it too, so
/// a restart never outlives a Stop. Returns when the run ends or it gives up.
pub fn supervise(
    procs: &Processes,
    current: &Mutex<u64>,
    run: u64,
    slot: &'static str,
    bin: &str,
    args: &[String],
    pace: &Pace,
) {
    let is_current = || *current.lock().unwrap_or_else(|e| e.into_inner()) == run;
    let mut started = Instant::now();
    let mut failures = 0;
    loop {
        std::thread::sleep(pace.poll);
        if !is_current() {
            return;
        }
        if procs.status(slot).running {
            continue;
        }
        failures = if started.elapsed() >= pace.healthy {
            1
        } else {
            failures + 1
        };
        let Some(&delay) = pace.delays.get(failures - 1) else {
            tracing::warn!("{slot} keeps exiting; giving up after {failures} quick failures");
            return;
        };
        std::thread::sleep(delay);
        let guard = current.lock().unwrap_or_else(|e| e.into_inner());
        if *guard != run {
            return;
        }
        tracing::info!("{slot} exited; starting it again (attempt {failures})");
        if let Err(e) = procs.start(slot, bin, args, true) {
            tracing::warn!("restarting {slot}: {e}");
        }
        drop(guard);
        started = Instant::now();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::Arc;

    const FAST: Pace = Pace {
        poll: Duration::from_millis(20),
        healthy: Duration::from_millis(400),
        delays: &[
            Duration::from_millis(10),
            Duration::from_millis(10),
            Duration::from_millis(10),
        ],
    };

    fn sh(script: String) -> Vec<String> {
        vec!["-c".into(), script]
    }

    fn starts(log: &std::path::Path) -> usize {
        std::fs::read_to_string(log).map(|s| s.lines().count()).unwrap_or(0)
    }

    fn wait_until(what: &str, f: impl Fn() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn restarts_a_sender_that_dies_and_gives_up_on_one_that_cannot_start() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("starts");
        // Dies at once, every time: a few attempts, then it stays down.
        let args = sh(format!(
            "echo x >> '{}'; echo 'Connection failed' >&2; exit 1",
            log.display()
        ));
        let procs = Processes::default();
        let current = Mutex::new(1);
        procs.start("s", "/bin/sh", &args, false).unwrap();
        supervise(&procs, &current, 1, "s", "/bin/sh", &args, &FAST);
        assert_eq!(starts(&log), 1 + FAST.delays.len(), "the first start and one per delay");
        let st = procs.status("s");
        assert!(!st.running && st.exit_code == Some(1), "{st:?}");
    }

    #[test]
    fn a_healthy_run_resets_the_failures() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("starts");
        // Streams long enough to count as healthy, then drops: forever.
        let args = sh(format!("echo x >> '{}'; sleep 0.5; exit 1", log.display()));
        let procs = Arc::new(Processes::default());
        let current = Arc::new(Mutex::new(1));
        procs.start("s", "/bin/sh", &args, false).unwrap();
        let worker = {
            let (procs, current, args) = (procs.clone(), current.clone(), args.clone());
            std::thread::spawn(move || supervise(&procs, &current, 1, "s", "/bin/sh", &args, &FAST))
        };
        wait_until("more restarts than the delays allow in a row", || {
            starts(&log) > FAST.delays.len() + 2
        });
        *current.lock().unwrap() += 1;
        worker.join().unwrap();
        procs.stop_all();
    }

    #[test]
    fn stop_ends_supervision_and_nothing_restarts_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("starts");
        let args = sh(format!("echo x >> '{}'; exec sleep 0.1", log.display()));
        let procs = Arc::new(Processes::default());
        let current = Arc::new(Mutex::new(7));
        procs.start("s", "/bin/sh", &args, false).unwrap();
        let worker = {
            let (procs, current, args) = (procs.clone(), current.clone(), args.clone());
            std::thread::spawn(move || supervise(&procs, &current, 7, "s", "/bin/sh", &args, &FAST))
        };
        wait_until("a restart", || starts(&log) >= 2);
        // What sender_stop does: end the run and stop, under the lock.
        {
            let mut run = current.lock().unwrap();
            *run += 1;
            procs.stop("s");
        }
        worker.join().unwrap();
        let after = starts(&log);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(starts(&log), after, "restarted after Stop");
        assert!(!procs.status("s").running);
    }
}

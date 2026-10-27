//! Child processes the cockpit launches on this machine (ffmpeg sender,
//! Moonlight). Each slot holds at most one process; all are killed when
//! the app exits.

use chrono::Utc;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use vercelify_core::ProcessStatus;

const OUTPUT_LINES: usize = 40;

struct Slot {
    child: Option<Child>,
    command: Option<String>,
    started_at: Option<String>,
    output: Arc<Mutex<VecDeque<String>>>,
    exit_code: Option<i32>,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            child: None,
            command: None,
            started_at: None,
            output: Arc::new(Mutex::new(VecDeque::new())),
            exit_code: None,
        }
    }
}

#[derive(Default)]
pub struct Processes {
    slots: Mutex<HashMap<&'static str, Slot>>,
}

impl Processes {
    /// Start `bin` in slot `name`. With `tie_to_app`, a watchdog kills the
    /// process if the cockpit dies without running its exit hook (crash,
    /// force quit) — a headless ffmpeg must never outlive the app.
    pub fn start(
        &self,
        name: &'static str,
        bin: &str,
        args: &[String],
        tie_to_app: bool,
    ) -> Result<ProcessStatus, String> {
        self.stop(name);
        let mut child = Command::new(bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start {bin}: {e}"))?;

        if tie_to_app {
            spawn_watchdog(child.id());
        }

        let output = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stderr) = child.stderr.take() {
            let sink = output.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut g = sink.lock().unwrap_or_else(|e| e.into_inner());
                    if g.len() >= OUTPUT_LINES {
                        g.pop_front();
                    }
                    g.push_back(line);
                }
            });
        }
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.insert(
            name,
            Slot {
                child: Some(child),
                command: Some(format!("{bin} {}", args.join(" "))),
                started_at: Some(Utc::now().to_rfc3339()),
                output,
                exit_code: None,
            },
        );
        drop(slots);
        Ok(self.status(name))
    }

    pub fn stop(&self, name: &str) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = slots.get_mut(name) {
            if let Some(mut c) = slot.child.take() {
                let _ = c.kill();
                slot.exit_code = c.wait().ok().and_then(|s| s.code());
            }
        }
    }

    pub fn status(&self, name: &str) -> ProcessStatus {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let Some(slot) = slots.get_mut(name) else {
            return ProcessStatus::default();
        };
        if let Some(c) = slot.child.as_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                slot.exit_code = status.code().or(Some(-1));
                slot.child = None;
            }
        }
        let last_output = slot
            .output
            .lock()
            .map(|g| g.iter().cloned().collect())
            .unwrap_or_default();
        ProcessStatus {
            running: slot.child.is_some(),
            pid: slot.child.as_ref().map(|c| c.id()),
            command: slot.command.clone(),
            started_at: slot.started_at.clone(),
            last_output,
            exit_code: if slot.child.is_some() { None } else { slot.exit_code },
        }
    }

    pub fn stop_all(&self) {
        let names: Vec<&'static str> = self
            .slots
            .lock()
            .map(|s| s.keys().copied().collect())
            .unwrap_or_default();
        for n in names {
            self.stop(n);
        }
    }
}

/// `sh` loop that exits as soon as either the app or the child is gone, and
/// kills the child only if the app went first. Exiting with the child avoids
/// ever signalling a recycled PID.
#[cfg(unix)]
fn spawn_watchdog(child: u32) {
    let script = format!(
        "while kill -0 {app} 2>/dev/null && kill -0 {child} 2>/dev/null; do sleep 1; done; \
         kill -0 {app} 2>/dev/null || kill -TERM {child} 2>/dev/null",
        app = std::process::id(),
    );
    let _ = Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(not(unix))]
fn spawn_watchdog(_child: u32) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn alive(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn start_status_stop() {
        let p = Processes::default();
        let st = p
            .start("t", "/bin/sh", &["-c".into(), "echo hello >&2; sleep 30".into()], false)
            .unwrap();
        assert!(st.running);
        let pid = st.pid.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while p.status("t").last_output.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(p.status("t").last_output, ["hello"]);
        p.stop("t");
        assert!(!p.status("t").running);
        assert!(!alive(pid));
    }

    #[test]
    fn exit_code_is_reported() {
        let p = Processes::default();
        p.start("t", "/bin/sh", &["-c".into(), "exit 3".into()], false).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while p.status("t").running && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let st = p.status("t");
        assert!(!st.running);
        assert_eq!(st.exit_code, Some(3));
    }

    /// The watchdog must kill a tied child when its parent dies abruptly.
    /// Simulate the "app" with an intermediate shell that we SIGKILL.
    #[test]
    fn watchdog_kills_orphan_when_parent_dies() {
        let dir = std::env::temp_dir().join(format!("vfy-wd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("child.pid");
        // fake app: start a child + the same watchdog loop, then wait
        let script = format!(
            "sleep 60 & c=$!; echo $c > {pf}; \\
             (while kill -0 $$ 2>/dev/null && kill -0 $c 2>/dev/null; do sleep 1; done; kill -0 $$ 2>/dev/null || kill -TERM $c) & \\
             wait $c",
            pf = pidfile.display()
        );
        let mut app = Command::new("/bin/sh").args(["-c", &script]).spawn().unwrap();
        // The shell creates the file before it writes the pid: wait for both.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut child: Option<u32> = None;
        while child.is_none() && Instant::now() < deadline {
            child = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse().ok());
            std::thread::sleep(Duration::from_millis(50));
        }
        let child = child.expect("the fake app never wrote its child's pid");
        assert!(alive(child));
        app.kill().unwrap(); // SIGKILL: no exit hooks run
        let _ = app.wait();
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(child) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(!alive(child), "orphaned child survived its parent");
        let _ = std::fs::remove_dir_all(dir);
    }
}

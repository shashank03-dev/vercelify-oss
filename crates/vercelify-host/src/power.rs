//! Keeping the computer awake while it hosts. A sleeping computer serves
//! nothing; this keeps it from idle sleep (on mains power on a Mac). It
//! can't stop a closed laptop on battery from sleeping, and the owner is
//! told so instead.

/// Holds the "don't idle-sleep" request while alive.
pub struct KeepAwake {
    #[cfg(target_os = "macos")]
    child: Option<std::process::Child>,
    #[cfg(windows)]
    stop: Option<std::sync::mpsc::Sender<()>>,
}

impl KeepAwake {
    pub fn new() -> KeepAwake {
        #[cfg(target_os = "macos")]
        {
            // -i: no idle sleep; -s: no system sleep on AC power. Tied to
            // our pid (-w) so it ends if we die without dropping it.
            let child = std::process::Command::new("caffeinate")
                .args(["-i", "-s", "-w", &std::process::id().to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| tracing::warn!("keep awake: caffeinate: {e}"))
                .ok();
            KeepAwake { child }
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED};
            // The request belongs to the thread that made it: a thread of
            // its own holds it until told to stop.
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            std::thread::spawn(move || {
                unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
                let _ = rx.recv();
                unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
            });
            KeepAwake { stop: Some(tx) }
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            KeepAwake {}
        }
    }
}

impl Default for KeepAwake {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        #[cfg(windows)]
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}

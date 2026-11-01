//! The boundary to whatever runs the Linux guest: Lima on macOS, QEMU with
//! the Windows Hypervisor Platform on Windows. The manager only talks to
//! this trait, so its lifecycle is tested against [`Fake`].
//!
//! Every provider owns exactly one guest, named [`GUEST_NAME`], in a
//! directory of Vercelify's own: it never lists, edits or stops the
//! owner's other virtual machines.

use crate::Result;
use vercelify_core::host::{EnvironmentGrant, Virtualization};

pub const GUEST_NAME: &str = "vercelify";

/// The pinned guest image (Debian 12 genericcloud, 2026-10-26). Changing it
/// is a release decision: qualify the new one first.
pub const IMAGE_RELEASE: &str = "20261026-2643";
pub const IMAGE_AMD64_SHA512: &str = "0464b5d4ae3aa4f658468d88da83af7857b71548b30d46dcb6282770955a747fb95a983476db90d25afd0653af607224c7ebd05122ab71faa106fd712aaad652";
pub const IMAGE_ARM64_SHA512: &str = "f42866fe22b4baf9f0ed4f81d620064f2a4b86b18b92bfe3aaaa40babf6819d39cbf0caaaad13f4ae3e7c9f82cbd78b0c8cabadd8bc737ce13578e61fa86970c";

pub fn image_url(arch: &str) -> String {
    let a = if arch == "aarch64" { "arm64" } else { "amd64" };
    format!(
        "https://cloud.debian.org/images/cloud/bookworm/{IMAGE_RELEASE}/debian-12-genericcloud-{a}-{IMAGE_RELEASE}.qcow2"
    )
}

pub fn image_sha512(arch: &str) -> &'static str {
    if arch == "aarch64" {
        IMAGE_ARM64_SHA512
    } else {
        IMAGE_AMD64_SHA512
    }
}

/// What the provider sees of the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observed {
    Missing,
    Stopped,
    Running,
}

/// Everything a provider needs to make the guest.
#[derive(Debug, Clone, PartialEq)]
pub struct GuestSpec {
    pub grant: EnvironmentGrant,
    pub arch: String,
    /// A main server: its mesh control (TCP) and WireGuard (UDP) ports are
    /// opened on this computer's network, as on a Linux main server.
    pub expose_mesh: bool,
}

/// The mesh ports a main server answers on (agent `mesh_control_listen`,
/// `mesh_port`).
pub const MESH_CONTROL_PORT: u16 = 7443;
pub const MESH_WG_PORT: u16 = 51820;

pub trait Provider: Send + Sync {
    /// `lima-vz`, `lima-qemu`, `qemu-whpx`, `fake`.
    fn name(&self) -> String;
    /// Whether this computer can run it now.
    fn virtualization(&self) -> Virtualization;
    /// The image the guest is made from, for the record.
    fn image(&self, arch: &str) -> String {
        format!("debian-12@{IMAGE_RELEASE} sha512:{}", &image_sha512(arch)[..16])
    }
    fn observe(&self) -> Result<Observed>;
    /// Make the guest (not started). Safe to call again after a failure:
    /// a half-made guest of ours is replaced.
    fn create(&self, spec: &GuestSpec) -> Result<()>;
    fn start(&self) -> Result<()>;
    fn stop(&self) -> Result<()>;
    /// Apply a new grant to the stopped guest.
    fn resize(&self, spec: &GuestSpec) -> Result<()>;
    /// Delete the guest and its disk.
    fn remove(&self) -> Result<()>;
    /// Run a script as root inside the running guest; its output.
    fn exec(&self, script: &str) -> Result<String>;
    /// The guest's address for the host manager's status endpoint.
    fn host_address(&self) -> &'static str;
    /// cloud-init seed files the guest fetches at first boot, if this
    /// provider seeds over the network.
    fn seed(&self, _file: &str) -> Option<String> {
        None
    }
}

/// An in-memory provider for tests: records calls, fails on demand.
#[derive(Default)]
pub struct Fake {
    pub inner: std::sync::Mutex<FakeState>,
}

#[derive(Default, Debug)]
pub struct FakeState {
    pub observed: Option<Observed>,
    pub calls: Vec<String>,
    /// The next call with this name fails.
    pub fail: Option<String>,
    pub grant: Option<EnvironmentGrant>,
    pub exec_reply: String,
}

impl Fake {
    fn call(&self, name: &str) -> Result<std::sync::MutexGuard<'_, FakeState>> {
        let mut s = self.inner.lock().unwrap();
        s.calls.push(name.to_string());
        if s.fail.as_deref() == Some(name) {
            s.fail = None;
            return Err(crate::Error::Unavailable(format!("{name} failed (fake)")));
        }
        Ok(s)
    }
}

impl Provider for Fake {
    fn name(&self) -> String {
        "fake".into()
    }
    fn virtualization(&self) -> Virtualization {
        Virtualization::Ready
    }
    fn observe(&self) -> Result<Observed> {
        Ok(self.inner.lock().unwrap().observed.unwrap_or(Observed::Missing))
    }
    fn create(&self, spec: &GuestSpec) -> Result<()> {
        let mut s = self.call("create")?;
        s.observed = Some(Observed::Stopped);
        s.grant = Some(spec.grant);
        Ok(())
    }
    fn start(&self) -> Result<()> {
        let mut s = self.call("start")?;
        if s.observed.unwrap_or(Observed::Missing) == Observed::Missing {
            return Err(crate::Error::Conflict("no guest".into()));
        }
        s.observed = Some(Observed::Running);
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        let mut s = self.call("stop")?;
        if s.observed == Some(Observed::Running) {
            s.observed = Some(Observed::Stopped);
        }
        Ok(())
    }
    fn resize(&self, spec: &GuestSpec) -> Result<()> {
        let mut s = self.call("resize")?;
        s.grant = Some(spec.grant);
        Ok(())
    }
    fn remove(&self) -> Result<()> {
        let mut s = self.call("remove")?;
        s.observed = Some(Observed::Missing);
        s.grant = None;
        Ok(())
    }
    fn exec(&self, _script: &str) -> Result<String> {
        let s = self.call("exec")?;
        if s.observed != Some(Observed::Running) {
            return Err(crate::Error::Unavailable("the guest isn't running".into()));
        }
        Ok(s.exec_reply.clone())
    }
    fn host_address(&self) -> &'static str {
        "10.0.2.2"
    }
}

pub enum Waited {
    TimedOut,
    Io(std::io::Error),
}

/// Wait for a child with a deadline; past it the child is killed. Its
/// output is read on threads so a chatty child can't block on a full pipe.
pub fn wait_with_timeout(
    mut child: std::process::Child,
    limit: std::time::Duration,
) -> std::result::Result<std::process::Output, Waited> {
    use std::io::Read;
    fn read(p: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut b = vec![];
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut b);
            }
            b
        })
    }
    let out = read(child.stdout.take());
    let err = read(child.stderr.take());
    let until = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait().map_err(Waited::Io)? {
            Some(s) => break s,
            None if std::time::Instant::now() >= until => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Waited::TimedOut);
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    };
    Ok(std::process::Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    #[test]
    fn a_stuck_command_is_stopped() {
        let c = Command::new("sleep").arg("30").stdout(Stdio::piped()).spawn().unwrap();
        let t = std::time::Instant::now();
        assert!(matches!(
            wait_with_timeout(c, Duration::from_millis(300)),
            Err(Waited::TimedOut)
        ));
        assert!(t.elapsed() < Duration::from_secs(5));
        let c = Command::new("echo").arg("hi").stdout(Stdio::piped()).spawn().unwrap();
        let o = wait_with_timeout(c, Duration::from_secs(5)).ok().unwrap();
        assert_eq!(String::from_utf8_lossy(&o.stdout), "hi\n");
    }
}

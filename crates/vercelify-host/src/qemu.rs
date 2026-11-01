//! The Windows provider: QEMU accelerated by the Windows Hypervisor
//! Platform (WHPX). Vercelify owns the whole configuration (no global
//! `.wslconfig`, no other VM touched), so its limits are its own.
//!
//! - The disk is a qcow2 made from the pinned Debian image (SHA-512 checked
//!   after download), grown to the grant; it lives in Vercelify's data dir.
//! - cloud-init reads its seed from the host manager over the guest's
//!   gateway (`ds=nocloud;s=http://10.0.2.2:7426/seed/`): no ISO tooling.
//! - Commands run in the guest over SSH to a port forwarded on the host's
//!   loopback only, with a key made for this guest.
//! - Lifecycle goes through QMP on the loopback; the VM keeps running when
//!   the host manager restarts (an update), and is found again by QMP.
//!
//! WHPX and QEMU on Windows remain to be qualified on real hardware before
//! Windows hosting is advertised (design §6); this is the implementation
//! that qualification runs against.

use crate::provider::{
    image_sha512, image_url, GuestSpec, Observed, Provider, GUEST_NAME, MESH_CONTROL_PORT, MESH_WG_PORT,
};
use crate::{Error, Result};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use vercelify_core::host::{EnvironmentGrant, Virtualization, GIB, MIB};

/// Loopback ports on the host: the guest's SSH and QEMU's QMP.
pub const SSH_PORT: u16 = 7427;
pub const QMP_PORT: u16 = 7428;

pub struct Qemu {
    /// Where `qemu-system-x86_64` and `qemu-img` are (bundled, pinned).
    pub bin_dir: PathBuf,
    /// `<data>\qemu`: disk, key, seed, grant.
    pub dir: PathBuf,
}

/// QEMU's command line for the guest (pure).
pub fn argv(dir: &std::path::Path, grant: &EnvironmentGrant, expose_mesh: bool) -> Vec<String> {
    let disk = dir.join("disk.qcow2");
    // SSH for the host manager on the loopback only; a main server's mesh
    // ports on this computer's network, as on a Linux main server.
    let mut net = format!("user,id=n0,hostfwd=tcp:127.0.0.1:{SSH_PORT}-:22");
    if expose_mesh {
        net.push_str(&format!(
            ",hostfwd=tcp:0.0.0.0:{MESH_CONTROL_PORT}-:{MESH_CONTROL_PORT},hostfwd=udp:0.0.0.0:{MESH_WG_PORT}-:{MESH_WG_PORT}"
        ));
    }
    vec![
        "-name".into(),
        GUEST_NAME.into(),
        "-machine".into(),
        "q35".into(),
        // Hardware acceleration only: no silent software-emulation fallback.
        "-accel".into(),
        "whpx".into(),
        "-smp".into(),
        grant.cpus.to_string(),
        "-m".into(),
        format!("{}M", grant.memory_bytes / MIB),
        "-display".into(),
        "none".into(),
        "-serial".into(),
        "none".into(),
        "-monitor".into(),
        "none".into(),
        "-drive".into(),
        format!(
            "file={},if=virtio,format=qcow2,cache=writeback,discard=unmap",
            disk.display()
        ),
        "-netdev".into(),
        net,
        "-device".into(),
        "virtio-net-pci,netdev=n0".into(),
        "-device".into(),
        "virtio-rng-pci".into(),
        "-smbios".into(),
        format!(
            "type=1,serial=ds=nocloud;s=http://10.0.2.2:{}/seed/",
            crate::guest::STATUS_PORT
        ),
        "-rtc".into(),
        "base=utc,driftfix=slew".into(),
        "-qmp".into(),
        format!("tcp:127.0.0.1:{QMP_PORT},server=on,wait=off"),
    ]
}

/// cloud-init's user-data: a `vercelify` admin with this guest's key, no
/// password login.
pub fn user_data(public_key: &str) -> Result<String> {
    let k = public_key.trim();
    if !k.starts_with("ssh-ed25519 ") || k.contains('\n') || k.len() > 200 {
        return Err(Error::Internal("unexpected SSH public key".into()));
    }
    Ok(format!(
        "#cloud-config\nhostname: {GUEST_NAME}\nssh_pwauth: false\nusers:\n  - name: vercelify\n    sudo: ALL=(ALL) NOPASSWD:ALL\n    shell: /bin/bash\n    lock_passwd: true\n    ssh_authorized_keys:\n      - {k}\n"
    ))
}

pub fn meta_data() -> String {
    format!("instance-id: {GUEST_NAME}\nlocal-hostname: {GUEST_NAME}\n")
}

// ----- QMP ---------------------------------------------------------------------------

/// One QMP command; its `return` value. `None`: QEMU isn't listening (the
/// VM is off).
pub fn qmp(addr: SocketAddr, command: &str) -> Result<Option<serde_json::Value>> {
    let Ok(s) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else {
        return Ok(None);
    };
    let io = |e: std::io::Error| Error::Unavailable(format!("QMP: {e}"));
    s.set_read_timeout(Some(Duration::from_secs(10))).map_err(io)?;
    let mut w = s.try_clone().map_err(io)?;
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).map_err(io)?; // greeting
    let mut send = |w: &mut TcpStream, cmd: &str| -> Result<serde_json::Value> {
        writeln!(w, "{}", serde_json::json!({ "execute": cmd })).map_err(io)?;
        loop {
            line.clear();
            if r.read_line(&mut line).map_err(io)? == 0 {
                return Err(Error::Unavailable("QMP closed".into()));
            }
            let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
            if v.get("event").is_some() {
                continue;
            }
            if let Some(e) = v.get("error") {
                return Err(Error::Unavailable(format!("QMP {cmd}: {e}")));
            }
            return Ok(v["return"].clone());
        }
    };
    send(&mut w, "qmp_capabilities")?;
    send(&mut w, command).map(Some)
}

fn qmp_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], QMP_PORT))
}

impl Qemu {
    fn exe(&self, name: &str) -> PathBuf {
        let n = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        };
        self.bin_dir.join(n)
    }

    fn disk(&self) -> PathBuf {
        self.dir.join("disk.qcow2")
    }

    fn key(&self) -> PathBuf {
        self.dir.join("guest_ed25519")
    }

    /// The grant and role the VM starts with.
    fn spec_file(&self) -> PathBuf {
        self.dir.join("spec.json")
    }

    fn save_spec(&self, spec: &GuestSpec) -> Result<()> {
        let v = serde_json::json!({ "grant": spec.grant, "expose_mesh": spec.expose_mesh });
        crate::state::write_private(&self.spec_file(), v.to_string().as_bytes())
    }

    /// The seed files cloud-init fetches (served by the status listener).
    pub fn seed_file(&self, file: &str) -> Option<String> {
        match file {
            "meta-data" => Some(meta_data()),
            "user-data" => {
                let pk = std::fs::read_to_string(self.key().with_extension("pub")).ok()?;
                user_data(&pk).ok()
            }
            "vendor-data" => Some(String::new()),
            _ => None,
        }
    }

    fn run(&self, exe: PathBuf, args: &[String]) -> Result<String> {
        let child = Command::new(&exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Unavailable(format!("running {}: {e}", exe.display())))?;
        // The longest is the image download on a slow link.
        let out = wait(child, Duration::from_secs(45 * 60))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(Error::Unavailable(format!(
                "{}: {}",
                exe.file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    /// The pinned image, downloaded once and checked.
    fn base_image(&self, arch: &str) -> Result<PathBuf> {
        let path = self.dir.join(format!("base-{}.qcow2", crate::provider::IMAGE_RELEASE));
        if path.exists() && sha512_file(&path)? == image_sha512(arch) {
            return Ok(path);
        }
        let part = path.with_extension("part");
        let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
        self.run(
            PathBuf::from(curl),
            &[
                "-fL".into(),
                "--retry".into(),
                "5".into(),
                "-C".into(),
                "-".into(),
                "-o".into(),
                part.display().to_string(),
                image_url(arch),
            ],
        )?;
        let got = sha512_file(&part)?;
        if got != image_sha512(arch) {
            let _ = std::fs::remove_file(&part);
            return Err(Error::Unavailable(
                "the downloaded system image doesn't match its checksum: try again (a proxy or a broken download?)"
                    .into(),
            ));
        }
        std::fs::rename(&part, &path).map_err(|e| Error::Internal(e.to_string()))?;
        Ok(path)
    }
}

fn wait(child: std::process::Child, limit: Duration) -> Result<std::process::Output> {
    crate::provider::wait_with_timeout(child, limit).map_err(|e| match e {
        crate::provider::Waited::TimedOut => Error::Unavailable(format!(
            "a step took longer than {} minutes (a stalled download or a stuck VM?): retrying is safe",
            limit.as_secs() / 60
        )),
        crate::provider::Waited::Io(e) => Error::Unavailable(e.to_string()),
    })
}

pub fn sha512_file(p: &std::path::Path) -> Result<String> {
    use sha2::Digest;
    let mut f = std::fs::File::open(p).map_err(|e| Error::Internal(format!("{}: {e}", p.display())))?;
    let mut h = sha2::Sha512::new();
    std::io::copy(&mut f, &mut h).map_err(|e| Error::Internal(e.to_string()))?;
    Ok(crate::state::hex(&h.finalize()))
}

impl Provider for Qemu {
    fn name(&self) -> String {
        "qemu-whpx".into()
    }

    fn virtualization(&self) -> Virtualization {
        windows_virtualization()
    }

    fn observe(&self) -> Result<Observed> {
        if !self.disk().exists() {
            return Ok(Observed::Missing);
        }
        Ok(match qmp(qmp_addr(), "query-status")? {
            Some(v) if v["status"] == "running" => Observed::Running,
            Some(_) | None => Observed::Stopped,
        })
    }

    fn create(&self, spec: &GuestSpec) -> Result<()> {
        if spec.arch != "x86_64" {
            return Err(Error::Invalid("Windows on ARM can't host yet".into()));
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| Error::Internal(e.to_string()))?;
        let base = self.base_image(&spec.arch)?;
        let _ = std::fs::remove_file(self.disk());
        let disk = self.disk().display().to_string();
        self.run(
            self.exe("qemu-img"),
            &[
                "convert".into(),
                "-O".into(),
                "qcow2".into(),
                base.display().to_string(),
                disk.clone(),
            ],
        )?;
        self.run(
            self.exe("qemu-img"),
            &["resize".into(), disk, format!("{}G", spec.grant.disk_bytes / GIB)],
        )?;
        let _ = std::fs::remove_file(self.key());
        let _ = std::fs::remove_file(self.key().with_extension("pub"));
        self.run(
            PathBuf::from("ssh-keygen"),
            &[
                "-q".into(),
                "-t".into(),
                "ed25519".into(),
                "-N".into(),
                String::new(),
                "-C".into(),
                "vercelify-host".into(),
                "-f".into(),
                self.key().display().to_string(),
            ],
        )?;
        self.save_spec(spec)
    }

    fn start(&self) -> Result<()> {
        if self.observe()? == Observed::Running {
            return Ok(());
        }
        let raw =
            std::fs::read(self.spec_file()).map_err(|_| Error::Conflict("the environment isn't made yet".into()))?;
        let v: serde_json::Value = serde_json::from_slice(&raw).map_err(|e| Error::Internal(e.to_string()))?;
        let grant: EnvironmentGrant =
            serde_json::from_value(v["grant"].clone()).map_err(|e| Error::Internal(e.to_string()))?;
        let expose_mesh = v["expose_mesh"].as_bool().unwrap_or(false);
        let mut c = Command::new(self.exe("qemu-system-x86_64"));
        c.args(argv(&self.dir, &grant, expose_mesh))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // No console window; not killed with the host manager.
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
        }
        c.spawn()
            .map_err(|e| Error::Unavailable(format!("starting QEMU: {e}")))?;
        for _ in 0..30 {
            std::thread::sleep(Duration::from_secs(1));
            if self.observe()? == Observed::Running {
                return Ok(());
            }
        }
        Err(Error::Unavailable(
            "QEMU didn't start: is the Windows Hypervisor Platform turned on, and ports 7427-7428 free?".into(),
        ))
    }

    fn stop(&self) -> Result<()> {
        if qmp(qmp_addr(), "system_powerdown")?.is_none() {
            return Ok(());
        }
        for _ in 0..90 {
            std::thread::sleep(Duration::from_secs(1));
            if qmp(qmp_addr(), "query-status")?.is_none() {
                return Ok(());
            }
        }
        // It didn't shut down in 90 s: stop it (its disk is journaled).
        let _ = qmp(qmp_addr(), "quit");
        Ok(())
    }

    fn resize(&self, spec: &GuestSpec) -> Result<()> {
        if self.observe()? == Observed::Running {
            return Err(Error::Conflict(
                "stop the environment before changing its resources".into(),
            ));
        }
        self.run(
            self.exe("qemu-img"),
            &[
                "resize".into(),
                self.disk().display().to_string(),
                format!("{}G", spec.grant.disk_bytes / GIB),
            ],
        )?;
        self.save_spec(spec)
    }

    fn remove(&self) -> Result<()> {
        self.stop()?;
        for f in [
            "disk.qcow2",
            "spec.json",
            "guest_ed25519",
            "guest_ed25519.pub",
            "known_hosts",
        ] {
            let _ = std::fs::remove_file(self.dir.join(f));
        }
        Ok(())
    }

    fn exec(&self, script: &str) -> Result<String> {
        let mut child = Command::new("ssh")
            .args([
                "-i".into(),
                self.key().display().to_string(),
                "-p".into(),
                SSH_PORT.to_string(),
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "StrictHostKeyChecking=accept-new".into(),
                "-o".into(),
                format!("UserKnownHostsFile={}", self.dir.join("known_hosts").display()),
                "-o".into(),
                "ConnectTimeout=10".into(),
                "vercelify@127.0.0.1".to_string(),
                "sudo bash -s".into(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Unavailable(format!("running ssh: {e}")))?;
        child
            .stdin
            .take()
            .expect("piped")
            .write_all(script.as_bytes())
            .map_err(|e| Error::Unavailable(e.to_string()))?;
        let out = wait(child, Duration::from_secs(40 * 60))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(Error::Unavailable(format!(
                "in the environment: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    fn host_address(&self) -> &'static str {
        "10.0.2.2"
    }

    fn seed(&self, file: &str) -> Option<String> {
        self.seed_file(file)
    }
}

#[cfg(windows)]
fn windows_virtualization() -> Virtualization {
    use windows_sys::Win32::System::Threading::IsProcessorFeaturePresent;
    // PF_VIRT_FIRMWARE_ENABLED
    const PF_VIRT_FIRMWARE_ENABLED: u32 = 21;
    // The hypervisor already running (WSL2, Hyper-V, WHP) reports firmware
    // virtualization as "not present" to the host: so check WHP first.
    let sys = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into())
        .join("System32");
    if sys.join("WinHvPlatform.dll").exists() {
        return Virtualization::Ready;
    }
    if unsafe { IsProcessorFeaturePresent(PF_VIRT_FIRMWARE_ENABLED) } == 0 {
        return Virtualization::NeedsFirmware;
    }
    Virtualization::NeedsOsFeature {
        feature: "Windows Hypervisor Platform".into(),
        restart: true,
    }
}

#[cfg(not(windows))]
fn windows_virtualization() -> Virtualization {
    Virtualization::Unsupported {
        reason: "QEMU with WHPX runs on Windows only".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn command_line_is_accelerated_and_loopback_only() {
        let g = EnvironmentGrant {
            memory_bytes: 3 * GIB,
            cpus: 2,
            disk_bytes: 30 * GIB,
            policy_version: 1,
        };
        let a = argv(std::path::Path::new("D:/data/qemu"), &g, false).join(" ");
        assert!(!a.contains("7443"), "a worker opens nothing");
        let m = argv(std::path::Path::new("D:/data/qemu"), &g, true).join(" ");
        assert!(m.contains("hostfwd=tcp:0.0.0.0:7443-:7443") && m.contains("hostfwd=udp:0.0.0.0:51820-:51820"));
        assert!(a.contains("-accel whpx"));
        assert!(!a.contains("tcg"), "no software emulation");
        assert!(a.contains("-m 3072M") && a.contains("-smp 2"));
        assert!(a.contains("hostfwd=tcp:127.0.0.1:7427-:22"), "SSH on loopback only");
        assert!(a.contains("tcp:127.0.0.1:7428,server=on"));
        assert!(a.contains("ds=nocloud;s=http://10.0.2.2:7426/seed/"));
        assert!(!a.contains("hostfwd=tcp::"), "nothing on every interface");
    }

    #[test]
    fn cloud_init_has_only_our_key() {
        let u = user_data("ssh-ed25519 AAAAC3Nza vercelify-host\n").unwrap();
        assert!(u.contains("ssh_pwauth: false") && u.contains("lock_passwd: true"));
        assert!(u.contains("- ssh-ed25519 AAAAC3Nza vercelify-host"));
        assert!(user_data("ssh-rsa AAAA").is_err());
        assert!(user_data("ssh-ed25519 A\nruncmd: [x]").is_err());
    }

    #[test]
    fn qmp_talks_to_a_server_and_sees_none_when_off() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut w = s.try_clone().unwrap();
            let mut r = BufReader::new(s);
            writeln!(w, "{{\"QMP\":{{}}}}").unwrap();
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            writeln!(w, "{{\"return\":{{}}}}").unwrap();
            line.clear();
            r.read_line(&mut line).unwrap();
            assert!(line.contains("query-status"));
            writeln!(w, "{{\"event\":\"RTC_CHANGE\"}}").unwrap();
            writeln!(w, "{{\"return\":{{\"status\":\"running\"}}}}").unwrap();
        });
        let v = qmp(addr, "query-status").unwrap().unwrap();
        assert_eq!(v["status"], "running");
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert!(qmp(closed, "query-status").unwrap().is_none());
    }
}

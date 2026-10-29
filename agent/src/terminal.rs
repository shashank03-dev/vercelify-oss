//! Web terminal sessions (design §12): `docker exec -it` into an app's or a
//! database's container on a pseudo-terminal the agent owns.
//!
//! Over the agent's ordinary authenticated transport rather than a
//! WebSocket: the cockpit opens a session (`POST`), reads its output as an
//! NDJSON stream, and sends keystrokes and window sizes as small `POST`s.
//! That works the same from the Tauri app, the browser and `vfy`, and adds
//! no dependency to the agent.
//!
//! A session belongs to the token that opened it. It ends when its shell
//! exits, when it is closed, or after [`IDLE`] without a reader or input.

use bytes::Bytes;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// At most this many sessions at once (each is a `docker exec` process).
pub const MAX_SESSIONS: usize = 8;
/// A session nobody reads from or types into for this long is closed.
pub const IDLE: Duration = Duration::from_secs(15 * 60);
/// Output kept for a reader that (re)attaches: the last screenfuls.
const BACKLOG: usize = 64 * 1024;

/// What a reader receives.
#[derive(Debug, Clone, PartialEq)]
pub enum Chunk {
    Data(Bytes),
    /// The shell exited with this code (-1: killed or unknown).
    Exit(i32),
}

pub struct Session {
    pub id: String,
    /// Token that opened it; only it may use the session.
    pub owner: String,
    pub target: String,
    master: OwnedFd,
    child: Mutex<Child>,
    output: broadcast::Sender<Chunk>,
    backlog: Mutex<VecDeque<u8>>,
    exit: Mutex<Option<i32>>,
    last_active: Mutex<Instant>,
    readers: AtomicUsize,
}

impl Session {
    fn touch(&self) {
        *self.last_active.lock().unwrap() = Instant::now();
    }

    /// The output so far (bounded) and a receiver for what follows. The two
    /// are taken under one lock so no byte falls between them.
    pub fn attach(self: &Arc<Self>) -> (Vec<u8>, broadcast::Receiver<Chunk>, Option<i32>, Reader) {
        let backlog = self.backlog.lock().unwrap();
        let rx = self.output.subscribe();
        let exit = *self.exit.lock().unwrap();
        self.readers.fetch_add(1, Ordering::SeqCst);
        self.touch();
        (backlog.iter().copied().collect(), rx, exit, Reader(self.clone()))
    }

    pub fn write(&self, data: &[u8]) -> std::io::Result<()> {
        self.touch();
        // A borrowed File on the master: writes go to the shell's stdin.
        let mut f = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(self.master.as_raw_fd()) });
        f.write_all(data)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        self.touch();
        set_size(self.master.as_raw_fd(), cols, rows)
    }

    pub fn exited(&self) -> Option<i32> {
        *self.exit.lock().unwrap()
    }

    fn idle(&self, now: Instant) -> bool {
        self.readers.load(Ordering::SeqCst) == 0 && now.duration_since(*self.last_active.lock().unwrap()) > IDLE
    }

    fn kill(&self) {
        let mut c = self.child.lock().unwrap();
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Counts an attached reader while alive (an idle session has none).
pub struct Reader(Arc<Session>);

impl Drop for Reader {
    fn drop(&mut self) {
        self.0.readers.fetch_sub(1, Ordering::SeqCst);
        self.0.touch();
    }
}

#[derive(Default)]
pub struct Terminals {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

impl Terminals {
    /// Start `argv` on a new pseudo-terminal of `cols`×`rows`.
    pub fn open(
        &self,
        owner: &str,
        target: &str,
        argv: &[String],
        cols: u16,
        rows: u16,
    ) -> Result<Arc<Session>, String> {
        self.reap();
        if self.sessions.lock().unwrap().len() >= MAX_SESSIONS {
            return Err(format!(
                "{MAX_SESSIONS} terminals are open on this server; close one first"
            ));
        }
        let (program, args) = argv.split_first().ok_or("no command")?;
        let (master, slave) = open_pty(cols.max(10), rows.max(4)).map_err(|e| format!("no pseudo-terminal: {e}"))?;
        let stdio = |fd: &OwnedFd| fd.try_clone().map(Stdio::from).map_err(|e| e.to_string());
        let mut cmd = Command::new(program);
        cmd.args(args)
            .env("TERM", "xterm-256color")
            .stdin(stdio(&slave)?)
            .stdout(stdio(&slave)?)
            .stderr(stdio(&slave)?);
        // The child leads a new session with the terminal as its controlling one,
        // so Ctrl-C reaches the program in it and not the agent.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|e| format!("could not start {program}: {e}"))?;
        drop(slave);
        let reader = master.try_clone().map_err(|e| e.to_string())?;
        let (output, _) = broadcast::channel(256);
        let session = Arc::new(Session {
            id: format!("term_{}", random_hex(16)),
            owner: owner.to_string(),
            target: target.to_string(),
            master,
            child: Mutex::new(child),
            output,
            backlog: Mutex::new(VecDeque::with_capacity(BACKLOG)),
            exit: Mutex::new(None),
            last_active: Mutex::new(Instant::now()),
            readers: AtomicUsize::new(0),
        });
        let s = session.clone();
        std::thread::Builder::new()
            .name("terminal".into())
            .spawn(move || pump(s, reader))
            .map_err(|e| e.to_string())?;
        self.sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), session.clone());
        Ok(session)
    }

    /// The session `id`, if `owner` opened it.
    pub fn get(&self, id: &str, owner: &str) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .filter(|s| s.owner == owner)
            .cloned()
    }

    pub fn close(&self, id: &str, owner: &str) -> bool {
        let s = {
            let mut map = self.sessions.lock().unwrap();
            match map.get(id) {
                Some(s) if s.owner == owner => map.remove(id),
                _ => None,
            }
        };
        match s {
            Some(s) => {
                s.kill();
                true
            }
            None => false,
        }
    }

    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    /// Drop ended sessions (after their readers saw the end) and idle ones.
    pub fn reap(&self) {
        let now = Instant::now();
        let gone: Vec<Arc<Session>> = {
            let mut map = self.sessions.lock().unwrap();
            let ids: Vec<String> = map
                .values()
                .filter(|s| s.idle(now) || (s.exited().is_some() && s.readers.load(Ordering::SeqCst) == 0))
                .map(|s| s.id.clone())
                .collect();
            ids.iter().filter_map(|id| map.remove(id)).collect()
        };
        for s in gone {
            s.kill();
        }
    }
}

/// Copy the terminal's output to the backlog and the readers until the
/// shell is gone (a pty master reads EIO once every slave is closed).
fn pump(s: Arc<Session>, master: OwnedFd) {
    let mut f = std::fs::File::from(master);
    let mut buf = [0u8; 8192];
    loop {
        match f.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut b = s.backlog.lock().unwrap();
                b.extend(&buf[..n]);
                let over = b.len().saturating_sub(BACKLOG);
                b.drain(..over);
                // Under the backlog lock: `attach` sees each byte exactly once.
                let _ = s.output.send(Chunk::Data(Bytes::copy_from_slice(&buf[..n])));
            }
        }
    }
    let code = s
        .child
        .lock()
        .unwrap()
        .wait()
        .ok()
        .and_then(|st| st.code())
        .unwrap_or(-1);
    let _b = s.backlog.lock().unwrap();
    *s.exit.lock().unwrap() = Some(code);
    let _ = s.output.send(Chunk::Exit(code));
}

fn open_pty(cols: u16, rows: u16) -> std::io::Result<(OwnedFd, OwnedFd)> {
    let (mut m, mut sl) = (0, 0);
    let mut ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::openpty(&mut m, &mut sl, std::ptr::null_mut(), std::ptr::null_mut(), &raw mut ws) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (m, sl) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(sl)) };
    // Neither end may leak into other processes the agent starts.
    for fd in [&m, &sl] {
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    Ok((m, sl))
}

fn set_size(fd: i32, cols: u16, rows: u16) -> std::io::Result<()> {
    let ws = libc::winsize {
        ws_row: rows.max(4),
        ws_col: cols.max(10),
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &ws) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::fill(&mut b).expect("system randomness");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `docker exec -it` into `container`, running `command` or the best shell
/// the image has.
pub fn exec_argv(docker: &str, container: &str, command: Option<&[String]>) -> Vec<String> {
    let mut argv = vec![
        docker.to_string(),
        "exec".into(),
        "-it".into(),
        "-e".into(),
        "TERM=xterm-256color".into(),
        container.to_string(),
    ];
    match command {
        Some(c) if !c.is_empty() => argv.extend(c.iter().cloned()),
        _ => argv.extend(
            [
                "sh",
                "-c",
                "if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi",
            ]
            .map(String::from),
        ),
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read_until(rx: &mut broadcast::Receiver<Chunk>, seen: &mut Vec<u8>, needle: &str) -> Option<i32> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if String::from_utf8_lossy(seen).contains(needle) {
                return None;
            }
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(Chunk::Data(b))) => seen.extend_from_slice(&b),
                Ok(Ok(Chunk::Exit(c))) => return Some(c),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                other => panic!(
                    "waiting for {needle:?}: {other:?}, saw {:?}",
                    String::from_utf8_lossy(seen)
                ),
            }
        }
    }

    #[tokio::test]
    async fn a_shell_on_a_pty() {
        let t = Terminals::default();
        let s = t.open("dev_a", "test", &["sh".into()], 80, 24).expect("a pty session");
        let (backlog, mut rx, exit, _reader) = s.attach();
        assert!(exit.is_none());
        let mut seen = backlog;
        // A real terminal: stdin is a tty, and resizes reach `stty`.
        s.write(b"test -t 0 && echo IS-A-TTY\n").unwrap();
        read_until(&mut rx, &mut seen, "IS-A-TTY\r\n").await;
        s.resize(120, 40).unwrap();
        s.write(b"stty size\n").unwrap();
        read_until(&mut rx, &mut seen, "40 120").await;

        // Only the token that opened it may use it.
        assert!(t.get(&s.id, "dev_b").is_none());
        assert!(t.get(&s.id, "dev_a").is_some());
        assert!(!t.close(&s.id, "dev_b"));

        s.write(b"exit 3\n").unwrap();
        let code = read_until(&mut rx, &mut seen, "never printed").await;
        assert_eq!(code, Some(3));
        assert_eq!(s.exited(), Some(3));
    }

    #[tokio::test]
    async fn backlog_for_late_readers_and_limits() {
        let t = Terminals::default();
        let s = t
            .open(
                "o",
                "x",
                &["sh".into(), "-c".into(), "echo early; sleep 30".into()],
                80,
                24,
            )
            .unwrap();
        // Output produced before anyone attached is replayed.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (b, _, _, _r) = s.attach();
            if String::from_utf8_lossy(&b).contains("early") {
                break;
            }
            assert!(Instant::now() < deadline, "no backlog");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(t.close(&s.id, "o"));
        assert_eq!(t.count(), 0);
        assert!(t.open("o", "x", &["/nonexistent/binary".into()], 80, 24).is_err());
    }

    /// `docker exec -it` into a real container (`VERCELIFY_DOCKER_TESTS=1`).
    #[tokio::test]
    #[ignore = "needs Docker: VERCELIFY_DOCKER_TESTS=1 cargo test … -- --include-ignored"]
    async fn into_a_container() {
        if std::env::var("VERCELIFY_DOCKER_TESTS").as_deref() != Ok("1") {
            return;
        }
        let name = "vfy-terminal-test";
        let _ = Command::new("docker").args(["rm", "-f", name]).output();
        let up = Command::new("docker")
            .args(["run", "-d", "--name", name, "alpine:3.20", "sleep", "120"])
            .output()
            .unwrap();
        assert!(up.status.success(), "{}", String::from_utf8_lossy(&up.stderr));
        let t = Terminals::default();
        let s = t.open("o", name, &exec_argv("docker", name, None), 90, 20).unwrap();
        let (b, mut rx, _, _r) = s.attach();
        let mut seen = b;
        s.write(b"cat /etc/alpine-release; tty; stty size\n").unwrap();
        read_until(&mut rx, &mut seen, "/dev/pts/").await;
        read_until(&mut rx, &mut seen, "20 90").await;
        read_until(&mut rx, &mut seen, "3.20").await;
        s.write(b"exit 0\n").unwrap();
        assert_eq!(read_until(&mut rx, &mut seen, "never printed").await, Some(0));
        let _ = Command::new("docker").args(["rm", "-f", name]).output();
    }

    #[test]
    fn docker_exec_command() {
        let a = exec_argv("docker", "vfy-blog-1", None);
        assert_eq!(
            &a[..6],
            ["docker", "exec", "-it", "-e", "TERM=xterm-256color", "vfy-blog-1"]
        );
        assert!(a.last().unwrap().contains("exec bash -l"));
        let a = exec_argv("docker", "c", Some(&["psql".into(), "-U".into(), "app".into()]));
        assert_eq!(&a[5..], ["c", "psql", "-U", "app"]);
    }

    #[test]
    fn pty_starts_with_requested_size() {
        let (_master, slave) = open_pty(90, 20).unwrap();
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCGWINSZ as _, &mut size) },
            0
        );
        assert_eq!((size.ws_col, size.ws_row), (90, 20));
    }
}

//! The environment's lifecycle (design §7, §9): one environment, changed
//! only by owner-approved operations that are journaled before they act,
//! replayable by idempotency key, revision-checked, and resumed or reported
//! honestly after a crash. Desired state is what the owner last asked for
//! (ready = running, stopped = stopped); the manager brings the guest back
//! to it after a reboot or a crash.

use crate::guest::{health_ok, Provision, Role, HEALTH_SCRIPT};
use crate::power::KeepAwake;
use crate::provider::{GuestSpec, Observed, Provider};
use crate::state::{self, new_id, State};
use crate::{Error, Result};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use vercelify_core::host::{
    grant_change, recommend, validate_grant, ChangeEnvironment, ConfirmedChange, CreateEnvironment, EnvironmentPairing,
    EnvironmentRecommendation, EnvironmentState, GrantChange, HostCapabilities, HostOperation, ManagedEnvironment,
    OperationKind, OperationStatus, SetupStage, StartEnvironment,
};

/// How long the guest may take to answer its health check after start
/// (first boot installs Docker and the agent: minutes on a slow link).
pub const VERIFY_ATTEMPTS: u32 = 60;
pub const VERIFY_EVERY: Duration = Duration::from_secs(10);

pub type Measure = Box<dyn Fn() -> HostCapabilities + Send + Sync>;
pub type Clock = Box<dyn Fn() -> i64 + Send + Sync>;

pub struct Manager {
    pub provider: Arc<dyn Provider>,
    state_path: PathBuf,
    state: Mutex<State>,
    measure: Measure,
    clock: Clock,
    hostname: String,
    install_ref: String,
    awake: Mutex<Option<KeepAwake>>,
    keep_awake: bool,
    check_ports: bool,
    verify_every: Duration,
    /// Operations running in this process. One in the journal as running
    /// but not here was left by a process that died.
    active: Mutex<std::collections::HashSet<String>>,
}

/// What the environment does after a step, kept in the journal.
enum Step {
    Make { spec: GuestSpec, role: Role },
    Start,
    Stop,
    Resize { spec: GuestSpec, restart: bool },
    Remove,
}

impl Manager {
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        provider: Arc<dyn Provider>,
        state_path: PathBuf,
        owner_token_sha256: String,
        measure: Measure,
        clock: Clock,
        hostname: String,
        install_ref: String,
    ) -> Result<Arc<Manager>> {
        let st = match state::load(&state_path)? {
            Some(s) => s,
            None => {
                let s = State::new(&provider.name(), owner_token_sha256);
                state::save(&state_path, &s)?;
                s
            }
        };
        Ok(Arc::new(Manager {
            provider,
            state_path,
            state: Mutex::new(st),
            measure,
            clock,
            hostname,
            install_ref,
            awake: Mutex::new(None),
            keep_awake: true,
            check_ports: true,
            verify_every: VERIFY_EVERY,
            active: Mutex::default(),
        }))
    }

    /// For tests: don't wait between health checks, don't hold the
    /// computer awake.
    pub fn for_tests(mut self: Arc<Self>) -> Arc<Self> {
        if let Some(m) = Arc::get_mut(&mut self) {
            m.verify_every = Duration::ZERO;
            m.keep_awake = false;
            m.check_ports = false;
        }
        self
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn save(&self, s: &State) -> Result<()> {
        state::save(&self.state_path, s)
    }

    pub fn owner_token_sha256(&self) -> String {
        self.lock().owner_token_sha256.clone()
    }

    pub fn environment(&self) -> ManagedEnvironment {
        self.lock().environment.clone()
    }

    pub fn operation(&self, id: &str) -> Option<HostOperation> {
        self.lock().operation(id).cloned()
    }

    pub fn capabilities(&self) -> HostCapabilities {
        (self.measure)()
    }

    pub fn recommendation(&self) -> EnvironmentRecommendation {
        recommend(&self.capabilities())
    }

    // ----- requests ------------------------------------------------------------------

    /// Begin (or replay) an operation: checks under the state lock, journal
    /// written before anything acts. Returns the operation and whether it is
    /// new (the caller then runs it).
    fn begin(
        &self,
        key: &str,
        kind: OperationKind,
        check: impl FnOnce(&ManagedEnvironment) -> Result<EnvironmentState>,
        update: impl FnOnce(&mut ManagedEnvironment),
    ) -> Result<(HostOperation, bool)> {
        if key.is_empty() || key.len() > 128 {
            return Err(Error::Invalid(
                "an idempotency key (1-128 characters) is required".into(),
            ));
        }
        let mut st = self.lock();
        if let Some(op) = st.by_key(key) {
            if op.kind != kind {
                return Err(Error::Conflict(
                    "this idempotency key was used for another operation".into(),
                ));
            }
            return Ok((op.clone(), false));
        }
        if let Some(running) = st.environment.operation.as_deref().and_then(|id| st.operation(id)) {
            if running.status == OperationStatus::Running {
                return Err(Error::Conflict(format!(
                    "another change ({:?}) is in progress: wait for it to finish",
                    running.kind
                )));
            }
        }
        let next = check(&st.environment)?;
        if !st.environment.state.can_become(next) && st.environment.state != next {
            return Err(Error::Conflict(format!(
                "the environment is {:?}: it can't be {:?} now",
                st.environment.state, kind
            )));
        }
        let op = HostOperation {
            id: new_id("op"),
            kind,
            key: key.to_string(),
            status: OperationStatus::Running,
            stage: None,
            error: None,
            created_at: (self.clock)(),
            finished_at: None,
        };
        let mut next_state = st.clone();
        next_state.push_operation(op.clone());
        let env = &mut next_state.environment;
        env.state = next;
        env.revision += 1;
        env.operation = Some(op.id.clone());
        env.reason = None;
        update(env);
        self.save(&next_state)?;
        *st = next_state;
        Ok((op, true))
    }

    /// Create the environment (or retry a failed creation).
    pub fn create(self: &Arc<Self>, req: &CreateEnvironment) -> Result<HostOperation> {
        let caps = self.capabilities();
        let role = match &req.join {
            Some(j) => Role::Worker { join: j.clone() },
            None => Role::Main,
        };
        // Refuse a bad invite now, not after the VM is made.
        Provision {
            hostname: self.hostname.clone(),
            role: role.clone(),
            host_address: self.provider.host_address().into(),
            install_ref: self.install_ref.clone(),
        }
        .script()?;
        let grant = req.grant;
        let main = role == Role::Main;
        // A main server opens two ports here: refuse now if another program
        // has them, rather than make a server nobody can reach.
        if main && self.check_ports && self.provider.observe()? != Observed::Running {
            crate::measure::mesh_ports_free().map_err(Error::Conflict)?;
        }
        let (op, new) = self.begin(
            &req.idempotency_key,
            OperationKind::Create,
            |env| {
                if !matches!(
                    env.state,
                    EnvironmentState::NotCreated | EnvironmentState::Blocked | EnvironmentState::Failed
                ) {
                    return Err(Error::Conflict(
                        "this computer has its Vercelify environment already: new apps use it".into(),
                    ));
                }
                // Measurements changed since the owner agreed: they review
                // a fresh proposal rather than get a grant the computer can't honour.
                validate_grant(&caps, &grant)
                    .map_err(|e| Error::Conflict(format!("this computer changed since you chose: {}", e.join("; "))))?;
                Ok(EnvironmentState::Preparing)
            },
            |env| {
                env.grant = Some(grant);
                env.provider = self.provider.name();
                env.image = self.provider.image(&caps.arch);
                env.last_stage = Some(SetupStage::CheckingComputer);
                env.role = Some(if main { "main" } else { "worker" }.into());
            },
        )?;
        if new {
            self.spawn(
                op.id.clone(),
                Step::Make {
                    spec: GuestSpec {
                        grant,
                        arch: caps.arch.clone(),
                        expose_mesh: main,
                    },
                    role,
                },
            );
        }
        Ok(op)
    }

    pub fn start(self: &Arc<Self>, req: &StartEnvironment) -> Result<HostOperation> {
        let (op, new) = self.begin(
            &req.idempotency_key,
            OperationKind::Start,
            |env| match env.state {
                EnvironmentState::Stopped | EnvironmentState::Ready => Ok(EnvironmentState::Starting),
                EnvironmentState::Failed if env.grant.is_some() => Ok(EnvironmentState::Starting),
                s => Err(Error::Conflict(format!("the environment is {s:?}: it can't start"))),
            },
            |_| {},
        )?;
        if new {
            self.spawn(op.id.clone(), Step::Start);
        }
        Ok(op)
    }

    pub fn stop(self: &Arc<Self>, req: &ConfirmedChange) -> Result<HostOperation> {
        if !req.acknowledged {
            return Err(Error::Invalid(
                "stopping the environment stops every app on this computer: confirm it".into(),
            ));
        }
        let (op, new) = self.begin(
            &req.idempotency_key,
            OperationKind::Stop,
            |env| {
                revision(env, req.revision)?;
                match env.state {
                    EnvironmentState::Ready | EnvironmentState::Failed => Ok(EnvironmentState::Stopping),
                    s => Err(Error::Conflict(format!("the environment is {s:?}: nothing to stop"))),
                }
            },
            |_| {},
        )?;
        if new {
            self.spawn(op.id.clone(), Step::Stop);
        }
        Ok(op)
    }

    /// Change memory, processors or the storage limit.
    pub fn change(self: &Arc<Self>, req: &ChangeEnvironment) -> Result<HostOperation> {
        let caps = self.capabilities();
        let grant = req.grant;
        let mut restart = false;
        let expose_mesh = self.environment().role.as_deref() == Some("main");
        let (op, new) = self.begin(
            &req.idempotency_key,
            OperationKind::Resize,
            |env| {
                revision(env, req.revision)?;
                let from = env
                    .grant
                    .ok_or_else(|| Error::Conflict("there is no environment to change".into()))?;
                if !matches!(env.state, EnvironmentState::Ready | EnvironmentState::Stopped) {
                    return Err(Error::Conflict(format!(
                        "the environment is {:?}: try again later",
                        env.state
                    )));
                }
                let change = grant_change(&from, &grant).map_err(Error::Invalid)?;
                if change == GrantChange::None {
                    return Err(Error::Invalid("nothing changed".into()));
                }
                validate_grant(&caps, &grant).map_err(|e| Error::Invalid(e.join("; ")))?;
                restart = env.state == EnvironmentState::Ready;
                if restart && !req.restart_ok {
                    return Err(Error::Conflict(
                        "this restarts the environment: its apps stop for a minute or two. Confirm to go ahead".into(),
                    ));
                }
                Ok(EnvironmentState::Updating)
            },
            |env| env.grant = Some(grant),
        )?;
        if new {
            self.spawn(
                op.id.clone(),
                Step::Resize {
                    spec: GuestSpec {
                        grant,
                        arch: caps.arch.clone(),
                        expose_mesh,
                    },
                    restart,
                },
            );
        }
        Ok(op)
    }

    /// Delete the environment and everything in it. The caller (Desktop)
    /// has shown which apps and databases live here and got the owner's
    /// decision about their data; `acknowledged` records it.
    pub fn remove(self: &Arc<Self>, req: &ConfirmedChange) -> Result<HostOperation> {
        if !req.acknowledged {
            return Err(Error::Invalid(
                "deleting the environment deletes every app, database and file in it: confirm it".into(),
            ));
        }
        let (op, new) = self.begin(
            &req.idempotency_key,
            OperationKind::Remove,
            |env| {
                revision(env, req.revision)?;
                Ok(EnvironmentState::Removing)
            },
            |_| {},
        )?;
        if new {
            self.spawn(op.id.clone(), Step::Remove);
        }
        Ok(op)
    }

    // ----- running operations --------------------------------------------------------------

    fn spawn(self: &Arc<Self>, op: String, step: Step) {
        self.active.lock().unwrap_or_else(|p| p.into_inner()).insert(op.clone());
        let me = self.clone();
        std::thread::spawn(move || {
            me.run(&op, step);
            me.active.lock().unwrap_or_else(|p| p.into_inner()).remove(&op);
        });
    }

    fn running_here(&self) -> bool {
        !self.active.lock().unwrap_or_else(|p| p.into_inner()).is_empty()
    }

    /// Run an operation to its end (on its own thread; tests call it
    /// through the public requests and [`Manager::wait`]).
    fn run(&self, op: &str, step: Step) {
        let res = match step {
            Step::Make { spec, role } => self.make(op, &spec, role),
            Step::Start => self.boot(op),
            Step::Stop => self.provider.stop().map(|_| EnvironmentState::Stopped),
            Step::Resize { spec, restart } => self.resize(op, &spec, restart),
            Step::Remove => self.provider.remove().map(|_| EnvironmentState::NotCreated),
        };
        self.finish(op, res);
    }

    fn set(&self, op: &str, state: Option<EnvironmentState>, stage: Option<SetupStage>) {
        let mut st = self.lock();
        let mut next = st.clone();
        if let Some(s) = state {
            next.environment.state = s;
        }
        if let Some(s) = stage {
            next.environment.last_stage = Some(s);
            if let Some(o) = next.operation_mut(op) {
                o.stage = Some(s);
            }
        }
        if self.save(&next).is_ok() {
            *st = next;
        }
    }

    fn make(&self, op: &str, spec: &GuestSpec, role: Role) -> Result<EnvironmentState> {
        self.set(op, None, Some(SetupStage::CheckingComputer));
        match self.provider.virtualization() {
            vercelify_core::host::Virtualization::Ready => {}
            v => {
                return Err(Error::Blocked(
                    vercelify_core::host::recommend(&HostCapabilities {
                        virtualization: v,
                        ..self.capabilities()
                    })
                    .blockers
                    .join("; "),
                ))
            }
        }
        self.set(op, None, Some(SetupStage::PreparingEnvironment));
        self.provider.create(spec)?;
        self.set(op, Some(EnvironmentState::Starting), Some(SetupStage::StartingHosting));
        self.provider.start()?;
        let script = Provision {
            hostname: self.hostname.clone(),
            role,
            host_address: self.provider.host_address().into(),
            install_ref: self.install_ref.clone(),
        }
        .script()?;
        self.provider.exec(&script)?;
        self.verify(op)
    }

    fn boot(&self, op: &str) -> Result<EnvironmentState> {
        self.set(op, Some(EnvironmentState::Starting), Some(SetupStage::StartingHosting));
        self.provider.start()?;
        self.verify(op)
    }

    fn resize(&self, op: &str, spec: &GuestSpec, restart: bool) -> Result<EnvironmentState> {
        self.provider.stop()?;
        self.provider.resize(spec)?;
        if restart {
            self.boot(op)
        } else {
            Ok(EnvironmentState::Stopped)
        }
    }

    fn verify(&self, op: &str) -> Result<EnvironmentState> {
        self.set(
            op,
            Some(EnvironmentState::Verifying),
            Some(SetupStage::CheckingConnection),
        );
        let mut last = String::new();
        for i in 0..VERIFY_ATTEMPTS {
            if i > 0 {
                std::thread::sleep(self.verify_every);
            }
            match self.provider.exec(HEALTH_SCRIPT) {
                Ok(out) => match health_ok(&out, (self.clock)()) {
                    Ok(()) => return Ok(EnvironmentState::Ready),
                    Err(e) => last = e,
                },
                Err(e) => last = e.message().to_string(),
            }
        }
        Err(Error::Unavailable(format!(
            "the environment started but isn't answering: {last}"
        )))
    }

    fn finish(&self, op: &str, res: Result<EnvironmentState>) {
        let now = (self.clock)();
        let mut st = self.lock();
        let mut next = st.clone();
        let removed = matches!(res, Ok(EnvironmentState::NotCreated));
        match &res {
            Ok(s) => {
                next.environment.state = *s;
                next.environment.reason = None;
                next.environment.retry_safe = true;
            }
            Err(Error::Blocked(why)) => {
                next.environment.state = EnvironmentState::Blocked;
                next.environment.reason = Some(why.clone());
                next.environment.retry_safe = true;
            }
            Err(e) => {
                next.environment.state = EnvironmentState::Failed;
                next.environment.reason = Some(e.message().to_string());
                // Every step re-runs safely: create replaces a half-made
                // guest of ours, provisioning is idempotent.
                next.environment.retry_safe = true;
            }
        }
        if removed {
            // A fresh identity: an old image or invitation can't bring the
            // removed environment back.
            let provider = next.environment.provider.clone();
            let revision = next.environment.revision;
            next.environment = State::new(&provider, String::new()).environment;
            // Never back to an old revision: a stale request can't match.
            next.environment.revision = revision + 1;
        }
        next.environment.operation = Some(op.to_string());
        if let Some(o) = next.operation_mut(op) {
            o.finished_at = Some(now);
            match &res {
                Ok(_) => o.status = OperationStatus::Succeeded,
                Err(e) => {
                    o.status = OperationStatus::Failed;
                    o.error = Some(e.message().to_string());
                }
            }
        }
        if let Err(e) = self.save(&next) {
            tracing::error!("saving the environment's state: {}", e.message());
        }
        *st = next;
        let ready = st.environment.state == EnvironmentState::Ready;
        drop(st);
        *self.awake.lock().unwrap_or_else(|p| p.into_inner()) = (ready && self.keep_awake).then(KeepAwake::new);
    }

    /// Wait for an operation to finish (tests and the CLI).
    pub fn wait(&self, op: &str, timeout: Duration) -> Option<HostOperation> {
        let until = std::time::Instant::now() + timeout;
        loop {
            let o = self.operation(op)?;
            if o.status != OperationStatus::Running || std::time::Instant::now() > until {
                return Some(o);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A one-use pairing code for a main-server environment, with this
    /// computer's network address in it (its mesh ports are forwarded here).
    pub fn pairing(&self) -> Result<EnvironmentPairing> {
        let env = self.environment();
        if env.state != EnvironmentState::Ready {
            return Err(Error::Conflict("start the environment first".into()));
        }
        if env.role.as_deref() != Some("main") {
            return Err(Error::Invalid(
                "this environment joined another main server: pair with that one".into(),
            ));
        }
        let address = crate::measure::lan_ipv4()
            .ok_or_else(|| Error::Unavailable("this computer isn't on a network right now".into()))?;
        let out = self
            .provider
            .exec(&format!("vercelify pair --host {address} 2>/dev/null"))?;
        let code = out
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .filter(|l| l.starts_with("vfy") && l.len() < 8192 && !l.contains(' '))
            .ok_or_else(|| Error::Unavailable("the environment didn't give a pairing code".into()))?
            .to_string();
        Ok(EnvironmentPairing { code, address })
    }

    // ----- after a restart, and every minute --------------------------------------------------

    /// Make the recorded state true again: an operation the last process
    /// didn't finish is reported failed (retry is safe); a ready
    /// environment whose guest is down (the computer restarted, the VM
    /// crashed) is started; one the owner stopped is kept stopped.
    pub fn reconcile(self: &Arc<Self>) -> Option<HostOperation> {
        // An operation of ours is changing things: it owns the state now.
        if self.running_here() {
            return None;
        }
        let interrupted = {
            let st = self.lock();
            let env = &st.environment;
            env.state.busy()
                && env
                    .operation
                    .as_deref()
                    .and_then(|id| st.operation(id))
                    .is_none_or(|o| o.status == OperationStatus::Running)
        };
        if interrupted {
            let op = self.lock().environment.operation.clone().unwrap_or_default();
            self.finish(
                &op,
                Err(Error::Unavailable(
                    "interrupted: the host manager stopped during this step; retrying is safe".into(),
                )),
            );
        }
        let env = self.environment();
        let observed = match self.provider.observe() {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("looking at the environment: {}", e.message());
                return None;
            }
        };
        match (env.state, observed) {
            (EnvironmentState::Ready, Observed::Running) => {
                let mut a = self.awake.lock().unwrap_or_else(|p| p.into_inner());
                if a.is_none() && self.keep_awake {
                    *a = Some(KeepAwake::new());
                }
                None
            }
            (EnvironmentState::Ready, Observed::Stopped) => {
                tracing::info!("the environment is down: starting it again");
                self.restart_after_outage()
            }
            (EnvironmentState::Ready | EnvironmentState::Stopped, Observed::Missing) => {
                self.mark_failed("the environment's virtual machine is missing (deleted outside Vercelify?): delete and create it again");
                None
            }
            (EnvironmentState::Stopped, Observed::Running) => {
                let _ = self.provider.stop();
                None
            }
            _ => None,
        }
    }

    fn mark_failed(&self, why: &str) {
        let mut st = self.lock();
        let mut next = st.clone();
        next.environment.state = EnvironmentState::Failed;
        next.environment.reason = Some(why.into());
        next.environment.retry_safe = false;
        if self.save(&next).is_ok() {
            *st = next;
        }
    }

    fn restart_after_outage(self: &Arc<Self>) -> Option<HostOperation> {
        // Not through begin(): Ready -> Starting is the owner's standing
        // wish, journaled like any other operation.
        let key = format!("recover-{}", new_id("k"));
        let (op, new) = self
            .begin(&key, OperationKind::Start, |_| Ok(EnvironmentState::Starting), |_| {})
            .ok()?;
        if new {
            self.spawn(op.id.clone(), Step::Start);
        }
        Some(op)
    }
}

fn revision(env: &ManagedEnvironment, expected: u64) -> Result<()> {
    if env.revision != expected {
        return Err(Error::Conflict(format!(
            "the environment changed since you looked (revision {} now, not {expected}): look again",
            env.revision
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Fake;
    use vercelify_core::host::{EnvironmentGrant, HostOs, Virtualization, GIB};

    fn caps() -> HostCapabilities {
        HostCapabilities {
            os: HostOs::Macos,
            os_version: "15.0".into(),
            arch: "aarch64".into(),
            logical_cpus: Some(8),
            memory_total_bytes: Some(16 * GIB),
            memory_available_bytes: Some(10 * GIB),
            disk_free_bytes: Some(200 * GIB),
            disk_rotational: Some(false),
            on_battery: Some(false),
            virtualization: Virtualization::Ready,
            provider: Some("fake".into()),
            measured_at: 1_000,
        }
    }

    struct T {
        _dir: tempfile::TempDir,
        path: PathBuf,
        fake: Arc<Fake>,
        m: Arc<Manager>,
    }

    fn manager() -> T {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let fake = Arc::new(Fake::default());
        let m = open(&path, fake.clone());
        T {
            _dir: dir,
            path,
            fake,
            m,
        }
    }

    fn open(path: &std::path::Path, fake: Arc<Fake>) -> Arc<Manager> {
        fake.inner.lock().unwrap().exec_reply = "1000\n".into();
        Manager::open(
            fake,
            path.to_path_buf(),
            "h".into(),
            Box::new(caps),
            Box::new(|| 1_000),
            "studio".into(),
            "v1.1.0".into(),
        )
        .unwrap()
        .for_tests()
    }

    fn grant() -> EnvironmentGrant {
        recommend(&caps()).grant.unwrap()
    }

    fn create(m: &Arc<Manager>, key: &str) -> Result<HostOperation> {
        m.create(&CreateEnvironment {
            idempotency_key: key.into(),
            grant: grant(),
            accepted_measured_at: 1_000,
            join: Some("vfyj1.abc".into()),
        })
    }

    fn done(m: &Manager, op: &HostOperation) -> HostOperation {
        m.wait(&op.id, Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn create_makes_one_environment_and_replays() {
        let t = manager();
        let op = create(&t.m, "k1").unwrap();
        let again = create(&t.m, "k1").unwrap();
        assert_eq!(op.id, again.id, "a replayed request returns the same operation");
        assert_eq!(done(&t.m, &op).status, OperationStatus::Succeeded);
        let env = t.m.environment();
        assert_eq!(env.state, EnvironmentState::Ready);
        assert_eq!(env.grant, Some(grant()));
        assert_eq!(env.last_stage, Some(SetupStage::CheckingConnection));
        let calls = t.fake.inner.lock().unwrap().calls.clone();
        assert_eq!(calls.iter().filter(|c| *c == "create").count(), 1, "{calls:?}");

        // A second project never makes a second environment.
        let second = create(&t.m, "k2");
        assert!(matches!(second, Err(Error::Conflict(_))), "{second:?}");
    }

    #[test]
    fn pairing_is_for_a_ready_main_server_only() {
        let t = manager();
        assert!(matches!(t.m.pairing(), Err(Error::Conflict(_))), "not ready");
        done(&t.m, &create(&t.m, "c").unwrap());
        assert_eq!(t.m.environment().role.as_deref(), Some("worker"));
        assert!(
            matches!(t.m.pairing(), Err(Error::Invalid(_))),
            "a worker pairs through its main"
        );
    }

    #[test]
    fn two_desktops_racing_get_one_creation() {
        let t = manager();
        let a = create(&t.m, "desktop-a");
        let b = create(&t.m, "desktop-b");
        assert!(a.is_ok());
        assert!(matches!(b, Err(Error::Conflict(_))), "{b:?}");
        done(&t.m, &a.unwrap());
    }

    #[test]
    fn a_failed_step_is_reported_and_retry_is_safe() {
        let t = manager();
        t.fake.inner.lock().unwrap().fail = Some("start".into());
        let op = done(&t.m, &create(&t.m, "k1").unwrap());
        assert_eq!(op.status, OperationStatus::Failed);
        let env = t.m.environment();
        assert_eq!(env.state, EnvironmentState::Failed);
        assert_eq!(env.last_stage, Some(SetupStage::StartingHosting));
        assert!(env.retry_safe && env.reason.unwrap().contains("start failed"));
        // Retry with a new key: replaces the half-made guest.
        let op = done(&t.m, &create(&t.m, "k2").unwrap());
        assert_eq!(op.status, OperationStatus::Succeeded);
        assert_eq!(t.m.environment().state, EnvironmentState::Ready);
    }

    #[test]
    fn changed_measurements_ask_for_a_fresh_review() {
        let t = manager();
        let mut g = grant();
        g.memory_bytes = 15 * GIB;
        let r = t.m.create(&CreateEnvironment {
            idempotency_key: "k".into(),
            grant: g,
            accepted_measured_at: 900,
            join: Some("vfyj1.abc".into()),
        });
        assert!(
            matches!(r, Err(Error::Conflict(ref m)) if m.contains("changed since you chose")),
            "{r:?}"
        );
        assert_eq!(
            t.m.environment().state,
            EnvironmentState::NotCreated,
            "nothing provisioned"
        );
        assert!(t.fake.inner.lock().unwrap().calls.is_empty());
    }

    #[test]
    fn a_bad_invite_is_refused_before_anything_is_made() {
        let t = manager();
        let r = t.m.create(&CreateEnvironment {
            idempotency_key: "k".into(),
            grant: grant(),
            accepted_measured_at: 1_000,
            join: Some("vfyj1.x;reboot".into()),
        });
        assert!(matches!(r, Err(Error::Invalid(_))));
        assert_eq!(t.m.environment().revision, 0);
    }

    #[test]
    fn stop_start_and_resize_need_the_owners_word_and_the_right_revision() {
        let t = manager();
        done(&t.m, &create(&t.m, "c").unwrap());
        let rev = t.m.environment().revision;

        let stale = t.m.stop(&ConfirmedChange {
            idempotency_key: "s".into(),
            revision: rev - 1,
            acknowledged: true,
        });
        assert!(matches!(stale, Err(Error::Conflict(_))));
        assert!(t
            .m
            .stop(&ConfirmedChange {
                idempotency_key: "s".into(),
                revision: rev,
                acknowledged: false,
            })
            .is_err());

        // Resizing a running environment restarts it: confirm first.
        let mut bigger = grant();
        bigger.cpus = 4;
        let need = t.m.change(&ChangeEnvironment {
            idempotency_key: "r".into(),
            revision: rev,
            grant: bigger,
            restart_ok: false,
        });
        assert!(
            matches!(need, Err(Error::Conflict(ref m)) if m.contains("restarts")),
            "{need:?}"
        );
        let op =
            t.m.change(&ChangeEnvironment {
                idempotency_key: "r2".into(),
                revision: rev,
                grant: bigger,
                restart_ok: true,
            })
            .unwrap();
        assert_eq!(done(&t.m, &op).status, OperationStatus::Succeeded);
        assert_eq!(t.m.environment().state, EnvironmentState::Ready);
        assert_eq!(t.fake.inner.lock().unwrap().grant.unwrap().cpus, 4);

        let rev = t.m.environment().revision;
        let mut smaller = bigger;
        smaller.disk_bytes -= GIB;
        assert!(
            t.m.change(&ChangeEnvironment {
                idempotency_key: "r3".into(),
                revision: rev,
                grant: smaller,
                restart_ok: true,
            })
            .is_err(),
            "no disk shrinking"
        );

        let op =
            t.m.stop(&ConfirmedChange {
                idempotency_key: "s2".into(),
                revision: rev,
                acknowledged: true,
            })
            .unwrap();
        done(&t.m, &op);
        assert_eq!(t.m.environment().state, EnvironmentState::Stopped);
        let op =
            t.m.start(&StartEnvironment {
                idempotency_key: "st".into(),
            })
            .unwrap();
        assert_eq!(done(&t.m, &op).status, OperationStatus::Succeeded);
        assert_eq!(t.m.environment().state, EnvironmentState::Ready);
    }

    #[test]
    fn removal_gives_a_fresh_identity() {
        let t = manager();
        done(&t.m, &create(&t.m, "c").unwrap());
        let before = t.m.environment();
        let op =
            t.m.remove(&ConfirmedChange {
                idempotency_key: "rm".into(),
                revision: before.revision,
                acknowledged: true,
            })
            .unwrap();
        assert_eq!(done(&t.m, &op).status, OperationStatus::Succeeded);
        let after = t.m.environment();
        assert_eq!(after.state, EnvironmentState::NotCreated);
        assert_ne!(after.id, before.id);
        assert!(after.revision > before.revision, "revisions only go up");
        assert!(after.grant.is_none());
        assert_eq!(t.fake.inner.lock().unwrap().observed, Some(Observed::Missing));
    }

    #[test]
    fn after_a_crash_mid_step_it_says_so_and_after_a_reboot_it_starts_again() {
        let t = manager();
        done(&t.m, &create(&t.m, "c").unwrap());
        // The computer restarted: the guest is off, the state says ready.
        t.fake.inner.lock().unwrap().observed = Some(Observed::Stopped);
        let m2 = open(&t.path, t.fake.clone());
        let op = m2.reconcile().expect("a recovery start");
        assert_eq!(done(&m2, &op).status, OperationStatus::Succeeded);
        assert_eq!(m2.environment().state, EnvironmentState::Ready);

        // The process died while preparing: the journal says running.
        let mut st = state::load(&t.path).unwrap().unwrap();
        st.environment.state = EnvironmentState::Preparing;
        let id = new_id("op");
        st.environment.operation = Some(id.clone());
        st.push_operation(HostOperation {
            id: id.clone(),
            kind: OperationKind::Create,
            key: "x".into(),
            status: OperationStatus::Running,
            stage: Some(SetupStage::PreparingEnvironment),
            error: None,
            created_at: 0,
            finished_at: None,
        });
        state::save(&t.path, &st).unwrap();
        let m3 = open(&t.path, t.fake.clone());
        m3.reconcile();
        let env = m3.environment();
        assert_eq!(env.state, EnvironmentState::Failed);
        assert!(env.retry_safe && env.reason.unwrap().contains("interrupted"));
        assert_eq!(m3.operation(&id).unwrap().status, OperationStatus::Failed);
    }

    #[test]
    fn the_minute_check_leaves_an_operation_in_progress_alone() {
        let t = manager();
        // Creation is slow (the health check keeps failing for now).
        t.fake.inner.lock().unwrap().exec_reply = "0\n".into();
        let op = create(&t.m, "c").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(t.m.reconcile().is_none());
        let o = t.m.operation(&op.id).unwrap();
        assert_ne!(o.error.as_deref().unwrap_or(""), "interrupted", "{o:?}");
        assert!(!t.m.environment().reason.unwrap_or_default().contains("interrupted"));
        done(&t.m, &op);
    }

    #[test]
    fn a_stopped_environment_stays_stopped() {
        let t = manager();
        done(&t.m, &create(&t.m, "c").unwrap());
        let rev = t.m.environment().revision;
        done(
            &t.m,
            &t.m.stop(&ConfirmedChange {
                idempotency_key: "s".into(),
                revision: rev,
                acknowledged: true,
            })
            .unwrap(),
        );
        t.fake.inner.lock().unwrap().observed = Some(Observed::Running);
        t.m.reconcile();
        assert_eq!(t.fake.inner.lock().unwrap().observed, Some(Observed::Stopped));
    }

    #[test]
    fn health_failures_end_in_failed_not_ready() {
        let t = manager();
        t.fake.inner.lock().unwrap().exec_reply = "0\n".into(); // clock far off
        let op = done(&t.m, &create(&t.m, "c").unwrap());
        assert_eq!(op.status, OperationStatus::Failed);
        assert!(op.error.unwrap().contains("clock"));
        assert_eq!(t.m.environment().state, EnvironmentState::Failed);
    }
}

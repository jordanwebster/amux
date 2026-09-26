//! `amux supervise`: the daemon's parent.
//!
//! It restarts the daemon whenever it exits, so agents survive any daemon
//! death, and under `updates: auto` it installs releases. Its rules:
//!
//! - One pipe joins it to the child: the child writes `prepared` after
//!   migrating and looking without writing, the supervisor answers `go`,
//!   and end of file means the supervisor is gone and the daemon exits.
//! - On each tick it installs the channel's build only if the version is
//!   newer than the running one, is not the rejected build, and the host is
//!   inside the rollout percentage.
//! - The swap: stage beside the binary, verify hash and signature,
//!   hard-link the current binary to prev, rename the staged binary over
//!   the path, stop the child, start the new one.
//! - Activation: on `prepared`, delete prev, fsync the directory, write
//!   `go`. After `go` a crash is restarted with backoff, never rolled back.
//! - K starts that never activate while prev exists: write the rejected
//!   build's version to one file, fsync, then rename prev back and start
//!   it. Record first, rename second: a crash between the two leaves a
//!   rejected version equal to the installed binary with prev present,
//!   which a starting supervisor finishes; the other order could leave
//!   neither record and reinstall the same build within the hour.
//! - After `go`, and last, it execs the installed binary in supervise mode,
//!   keeping pid, child, pipe and lock, so the supervisor is never the
//!   stale part for long.
//! - prev is the whole record of an update in progress: a starting
//!   supervisor that finds it counts failed starts against it, and if it is
//!   the same file as the binary the swap never happened, so it deletes it.
//! - Stopping the child is a signal, a stop deadline, then a kill.
//!
//! Counters and timers live in memory. The supervisor holds its own lock;
//! the installation lock stays the daemon's.
//!
//! On Windows a running executable cannot be renamed over, so the
//! supervisor stops the child before the swap and moves the binary aside
//! to prev by rename, and its own update is a spawn-then-exit.

mod child;
mod files;

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_dir::Clock;
use child::Child;
pub use child::Inherited;
pub use files::{FileId, Files, SUPERVISOR_LOCK};
use semver::Version;
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;
use tokio::task::JoinHandle;

use crate::install::{private_dir, sync_dir};
use crate::profiles::{self, Registry};
use crate::release::{self, Choice, Manifest};

/// The supervisor's timings and thresholds; the defaults are the
/// parameters table's starting points.
#[derive(Clone, Debug)]
pub struct Params {
    pub check_interval: Duration,
    /// Starts that never activate, while prev exists, before rolling back.
    pub rollback_after: u32,
    /// From spawn to `prepared`.
    pub start_deadline: Duration,
    /// From the stop signal to the kill.
    pub stop_deadline: Duration,
    pub backoff_first: Duration,
    pub backoff_max: Duration,
    /// A child up this long starts the backoff over.
    pub backoff_reset: Duration,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(60 * 60),
            // 3 until failed starts in the field say otherwise.
            rollback_after: 3,
            start_deadline: Duration::from_secs(60),
            stop_deadline: Duration::from_secs(30),
            backoff_first: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            backoff_reset: Duration::from_secs(60),
        }
    }
}

/// Where releases come from, under `updates: auto`.
#[derive(Clone, Debug)]
pub struct UpdateSource {
    /// The channel's manifest.
    pub manifest_url: String,
    /// The key release signatures must verify against.
    pub key: [u8; 32],
}

pub struct SuperviseOptions {
    /// The canonical install path: what the child runs, what the swap
    /// replaces, and what the supervisor execs after an update.
    pub binary: PathBuf,
    /// Arguments before the subcommand, passed to the child and to the
    /// next supervisor, such as `--config <path>`.
    pub args: Vec<OsString>,
    /// The installation's data directory: the supervisor's lock, and the
    /// host id that places it in a rollout.
    pub data_dir: PathBuf,
    /// This binary's version.
    pub running: Version,
    pub target: String,
    /// None under `updates: manual`: restarts only.
    pub updates: Option<UpdateSource>,
    pub clock: Arc<dyn Clock>,
    pub params: Params,
    /// What the supervisor that exec'd this one handed over.
    pub inherited: Option<Inherited>,
}

#[derive(Debug, thiserror::Error)]
pub enum SuperviseError {
    #[error("another amux supervisor (pid {pid}) holds {}", .path.display())]
    Busy { path: PathBuf, pid: String },
    #[error("the supervisor's lock {}: {error}", .path.display())]
    Lock { path: PathBuf, error: io::Error },
    #[error("taking over from the previous supervisor: {0}")]
    Inherit(io::Error),
    #[error("{what}: {error}")]
    Files {
        what: &'static str,
        error: io::Error,
    },
}

fn files_error(what: &'static str) -> impl FnOnce(io::Error) -> SuperviseError {
    move |error| SuperviseError::Files { what, error }
}

/// Supervises the daemon until the supervisor is asked to stop, then stops
/// the daemon and returns. After an update it execs the new binary instead
/// and does not return.
pub async fn supervise(options: SuperviseOptions) -> Result<(), SuperviseError> {
    let SuperviseOptions {
        binary,
        args,
        data_dir,
        running,
        target,
        updates,
        clock,
        params,
        inherited,
    } = options;
    let lock = match inherited.and_then(|inherited| inherited.lock) {
        Some(fd) => SupervisorLock::inherit(fd, &data_dir)?,
        None => SupervisorLock::acquire(&data_dir, inherited.is_some())?,
    };
    let files = Files::beside(&binary);
    // What this process runs: the binary at the path when it started.
    let mine = FileId::of(&binary).map_err(files_error("reading the installed binary"))?;
    let mut supervisor = Supervisor {
        backoff: Backoff::new(&params),
        files,
        binary,
        args,
        data_dir,
        target,
        updates,
        clock,
        params,
        installed: Some(running.clone()),
        running,
        mine,
        failed: 0,
        lock,
    };
    supervisor.files.clear_leftovers();
    let child = match inherited {
        Some(inherited) => Some(
            Child::inherit(&inherited, supervisor.clock.now_ms())
                .map_err(SuperviseError::Inherit)?,
        ),
        None => {
            supervisor.recover()?;
            None
        }
    };
    supervisor.run(child).await
}

struct Supervisor {
    files: Files,
    binary: PathBuf,
    args: Vec<OsString>,
    data_dir: PathBuf,
    running: Version,
    target: String,
    updates: Option<UpdateSource>,
    clock: Arc<dyn Clock>,
    params: Params,
    /// The version of the binary at the path, when known: the one to
    /// record as rejected if it never activates.
    installed: Option<Version>,
    mine: FileId,
    /// Starts that never activated since prev appeared.
    failed: u32,
    backoff: Backoff,
    lock: SupervisorLock,
}

enum Event {
    Terminate,
    Exited(child::ExitCode),
    Prepared,
    StartDeadline,
    CheckDue,
    Checked(Option<Version>),
}

impl Supervisor {
    /// A starting supervisor reads the record an update left: prev.
    fn recover(&mut self) -> Result<(), SuperviseError> {
        if !self.files.prev_exists() {
            return Ok(());
        }
        if self.files.prev_is(&self.binary) {
            tracing::info!("the swap never happened: removing prev");
            self.files
                .remove_prev()
                .map_err(files_error("removing prev"))?;
            return Ok(());
        }
        if self.files.rejected().as_ref() == Some(&self.running) {
            tracing::info!(rejected = %self.running, "finishing a rollback");
            self.put_prev_back()?;
            return Ok(());
        }
        tracing::info!(
            installed = %self.running,
            "an update is in progress: counting failed starts against it"
        );
        Ok(())
    }

    async fn run(mut self, mut child: Option<Child>) -> Result<(), SuperviseError> {
        let mut terminate = std::pin::pin!(terminated());
        let mut next_check = self.after(self.params.check_interval);
        let mut restart_at: Option<i64> = None;
        let mut checking: Option<JoinHandle<Option<Version>>> = None;
        loop {
            let Some(running) = child.as_mut() else {
                if let Some(at) = restart_at.take() {
                    tokio::select! {
                        () = self.clock.sleep_until(at) => {}
                        () = &mut terminate => return Ok(()),
                    }
                }
                match Child::spawn(&self.binary, &self.args, self.clock.now_ms()) {
                    Ok(spawned) => {
                        tracing::info!(pid = spawned.pid, "started the daemon");
                        child = Some(spawned);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "could not start the daemon");
                        self.failed_start()?;
                        let delay = self.backoff.next(Duration::ZERO);
                        restart_at = Some(self.after(delay));
                    }
                }
                continue;
            };
            let may_check = self.updates.is_some()
                && running.activated()
                && checking.is_none()
                && !self.files.prev_exists();
            let start_deadline = running.started_ms + millis(self.params.start_deadline);
            let activated = running.activated();
            let exited = running.exited();
            let event = tokio::select! {
                () = &mut terminate => Event::Terminate,
                code = exited => Event::Exited(code),
                () = running.prepared(), if !activated => Event::Prepared,
                () = self.clock.sleep_until(start_deadline), if !activated => Event::StartDeadline,
                () = self.clock.sleep_until(next_check), if may_check => Event::CheckDue,
                checked = async { checking.as_mut().expect("checking").await }, if checking.is_some() => {
                    Event::Checked(checked.unwrap_or(None))
                }
            };
            if matches!(event, Event::Checked(_)) {
                checking = None;
            }
            match event {
                Event::Terminate => {
                    tracing::info!("asked to stop: stopping the daemon");
                    if let Some(mut running) = child.take() {
                        self.stop(&mut running).await;
                    }
                    return Ok(());
                }
                Event::Exited(code) => {
                    let running = child.take().expect("a child");
                    match running.activated_ms {
                        Some(activated) => {
                            let up = Duration::from_millis(
                                (self.clock.now_ms() - activated).max(0) as u64,
                            );
                            let delay = self.backoff.next(up);
                            tracing::warn!(code, ?delay, "the daemon exited: restarting it");
                            restart_at = Some(self.after(delay));
                        }
                        None => {
                            tracing::warn!(code, "the daemon exited before it was prepared");
                            restart_at = self.failed_start()?;
                        }
                    }
                }
                Event::Prepared => {
                    let running = child.as_mut().expect("a child");
                    self.activate(running)?;
                    if self.stale() {
                        let running = child.as_ref().expect("a child");
                        self.become_installed(running);
                    }
                }
                Event::StartDeadline => {
                    let mut running = child.take().expect("a child");
                    tracing::warn!(
                        deadline = ?self.params.start_deadline,
                        "the daemon did not prepare in time: stopping it"
                    );
                    self.stop(&mut running).await;
                    restart_at = self.failed_start()?;
                }
                Event::CheckDue => {
                    next_check = self.after(self.params.check_interval);
                    checking = Some(self.check());
                }
                Event::Checked(None) => {}
                Event::Checked(Some(version)) => {
                    let mut running = child.take().expect("a child");
                    self.swap(&mut running, version).await?;
                    restart_at = None;
                }
            }
        }
    }

    fn after(&self, delay: Duration) -> i64 {
        self.clock.now_ms() + millis(delay)
    }

    /// Whether the path holds a different binary than this process runs.
    fn stale(&self) -> bool {
        FileId::of(&self.binary).is_ok_and(|id| id != self.mine)
    }

    /// A start that never activated. While prev exists it counts toward
    /// the rollback; the K-th rolls back and starts prev at once. Returns
    /// when to start again.
    fn failed_start(&mut self) -> Result<Option<i64>, SuperviseError> {
        if self.files.prev_exists() {
            self.failed += 1;
            tracing::warn!(
                failed = self.failed,
                of = self.params.rollback_after,
                "a start that never activated"
            );
            if self.failed >= self.params.rollback_after {
                self.roll_back()?;
                return Ok(None);
            }
        }
        let delay = self.backoff.next(Duration::ZERO);
        Ok(Some(self.after(delay)))
    }

    fn roll_back(&mut self) -> Result<(), SuperviseError> {
        let rejected = self
            .installed
            .clone()
            .ok_or_else(|| SuperviseError::Files {
                what: "rolling back",
                error: io::Error::other("the installed version is not known"),
            })?;
        tracing::warn!(%rejected, "rolling back to prev");
        self.files
            .write_rejected(&rejected)
            .map_err(files_error("recording the rejected build"))?;
        self.put_prev_back()
    }

    fn put_prev_back(&mut self) -> Result<(), SuperviseError> {
        self.files
            .restore_prev(&self.binary)
            .map_err(files_error("putting prev back"))?;
        self.failed = 0;
        self.backoff = Backoff::new(&self.params);
        self.installed = (!self.stale()).then(|| self.running.clone());
        Ok(())
    }

    fn activate(&mut self, child: &mut Child) -> Result<(), SuperviseError> {
        if self.files.prev_exists() {
            self.files
                .retire_prev(&self.mine)
                .map_err(files_error("removing prev"))?;
        }
        self.failed = 0;
        if let Err(error) = child.go() {
            // It is going away; its exit comes next.
            tracing::warn!(%error, "could not tell the daemon go");
            return Ok(());
        }
        child.activated_ms = Some(self.clock.now_ms());
        tracing::info!(pid = child.pid, "the daemon is prepared: go");
        Ok(())
    }

    /// Replaces this process with the installed binary in supervise mode,
    /// keeping the child, the pipe and the lock. A failure leaves this
    /// supervisor running, which is still correct.
    fn become_installed(&self, child: &Child) {
        let inherited = match child.hand_over(&self.lock.file) {
            Ok(inherited) => inherited,
            Err(error) => {
                tracing::warn!(%error, "could not hand over to the installed binary");
                child.keep(&self.lock.file);
                return;
            }
        };
        tracing::info!(binary = %self.binary.display(), "handing over to the installed binary");
        #[cfg(unix)]
        {
            let error = child::exec_supervisor(&self.binary, &self.args, &inherited);
            tracing::warn!(%error, "could not exec the installed binary");
        }
        #[cfg(windows)]
        match child::spawn_supervisor(&self.binary, &self.args, &inherited) {
            Ok(()) => std::process::exit(0),
            Err(error) => tracing::warn!(%error, "could not start the installed binary"),
        }
        child.keep(&self.lock.file);
    }

    /// Stops the child: a signal, the stop deadline, then a kill.
    async fn stop(&self, child: &mut Child) {
        child.ask_to_stop();
        let deadline = self.after(self.params.stop_deadline);
        tokio::select! {
            _ = child.exited() => return,
            () = self.clock.sleep_until(deadline) => {}
        }
        tracing::warn!(
            pid = child.pid,
            "the daemon did not stop in time: killing it"
        );
        child.kill();
        child.exited().await;
    }

    /// Fetches the manifest and, when it names a build to install, stages
    /// and verifies it. Resolves to its version once it is staged.
    fn check(&self) -> JoinHandle<Option<Version>> {
        let source = self.updates.clone().expect("checked under updates: auto");
        let target = self.target.clone();
        let running = self.running.clone();
        let rejected = self.files.rejected();
        let host = rollout_host(&self.data_dir);
        let staged = self.files.staged.clone();
        tokio::spawn(async move {
            match fetch(&source, &target, &running, rejected.as_ref(), host, &staged).await {
                Ok(staged) => staged,
                Err(error) => {
                    tracing::warn!(%error, "the update check failed");
                    let _ = std::fs::remove_file(&staged);
                    None
                }
            }
        })
    }

    async fn swap(&mut self, child: &mut Child, version: Version) -> Result<(), SuperviseError> {
        tracing::info!(%version, "installing");
        #[cfg(unix)]
        {
            self.files
                .install_staged(&self.binary)
                .map_err(files_error("installing the staged binary"))?;
            self.stop(child).await;
        }
        // A running executable cannot be renamed over: stop first, and the
        // current binary moves aside to prev instead of gaining a link.
        #[cfg(windows)]
        {
            self.stop(child).await;
            self.files
                .install_staged(&self.binary)
                .map_err(files_error("installing the staged binary"))?;
        }
        self.installed = Some(version);
        self.failed = 0;
        self.backoff = Backoff::new(&self.params);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
enum CheckError {
    #[error("fetching {url}: {error}")]
    Fetch { url: String, error: reqwest::Error },
    #[error("staging: {0}")]
    Stage(io::Error),
    #[error(transparent)]
    Verify(#[from] release::VerifyError),
}

async fn fetch(
    source: &UpdateSource,
    target: &str,
    running: &Version,
    rejected: Option<&Version>,
    host: Option<uuid::Uuid>,
    staged: &Path,
) -> Result<Option<Version>, CheckError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(15 * 60))
        .build()
        .map_err(|error| CheckError::Fetch {
            url: source.manifest_url.clone(),
            error,
        })?;
    let fetch_error = |url: &str| {
        let url = url.to_owned();
        move |error| CheckError::Fetch { url, error }
    };
    let manifest: Manifest = client
        .get(&source.manifest_url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(fetch_error(&source.manifest_url))?
        .json()
        .await
        .map_err(fetch_error(&source.manifest_url))?;
    let (release, version) =
        match release::choose(&manifest, target, running, rejected, host.as_ref()) {
            Choice::Install { release, version } => (release, version),
            Choice::Skip(skip) => {
                tracing::debug!(?skip, "nothing to install");
                return Ok(None);
            }
        };
    let mut response = client
        .get(&release.url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(fetch_error(&release.url))?;
    let mut file = tokio::fs::File::create(staged)
        .await
        .map_err(CheckError::Stage)?;
    let mut hash = Sha256::new();
    while let Some(chunk) = response.chunk().await.map_err(fetch_error(&release.url))? {
        hash.update(&chunk);
        file.write_all(&chunk).await.map_err(CheckError::Stage)?;
    }
    file.sync_all().await.map_err(CheckError::Stage)?;
    drop(file);
    release::verify(
        &release,
        target,
        &release::sha256_hex(&hash.finalize()),
        &source.key,
    )?;
    files::make_executable(staged).map_err(CheckError::Stage)?;
    Ok(Some(version))
}

/// The host id that places this installation in a rollout: its first
/// profile's.
fn rollout_host(data_dir: &Path) -> Option<uuid::Uuid> {
    let registry = Registry::read(data_dir).ok()?;
    let first = registry.profiles.first()?;
    profiles::host_id(&profiles::profile_dir(data_dir, first.id)).ok()
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Restart delays: doubling from the first to the cap, starting over once
/// a child has stayed up long enough.
struct Backoff {
    first: Duration,
    max: Duration,
    reset: Duration,
    next: Duration,
}

impl Backoff {
    fn new(params: &Params) -> Self {
        Self {
            first: params.backoff_first,
            max: params.backoff_max,
            reset: params.backoff_reset,
            next: params.backoff_first,
        }
    }

    fn next(&mut self, up: Duration) -> Duration {
        if up >= self.reset {
            self.next = self.first;
        }
        let delay = self.next;
        self.next = (self.next * 2).min(self.max);
        delay
    }
}

/// The supervisor's own lock, `<data_dir>/supervisor.lock`, holding its
/// pid for `amux server stop`. It crosses the exec after an update with the
/// rest of the supervisor's state.
struct SupervisorLock {
    file: File,
}

impl SupervisorLock {
    /// Takes the lock. A supervisor started by the previous one on Windows
    /// waits for it to exit and release it.
    fn acquire(data_dir: &Path, handed_over: bool) -> Result<Self, SuperviseError> {
        use std::io::{Read as _, Seek as _, Write as _};
        let path = data_dir.join(SUPERVISOR_LOCK);
        let lock_error = |error| SuperviseError::Lock {
            path: path.clone(),
            error,
        };
        private_dir(data_dir).map_err(lock_error)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(lock_error)?;
        let mut tries = if handed_over { 100 } else { 1 };
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if tries > 1 => {
                    tries -= 1;
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    let mut pid = String::new();
                    let _ = file.read_to_string(&mut pid);
                    return Err(SuperviseError::Busy {
                        path,
                        pid: pid.trim().to_owned(),
                    });
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(lock_error(error)),
            }
        }
        file.set_len(0).map_err(lock_error)?;
        file.rewind().map_err(lock_error)?;
        write!(file, "{}", std::process::id()).map_err(lock_error)?;
        file.sync_all().map_err(lock_error)?;
        Ok(Self { file })
    }

    fn inherit(fd: u64, data_dir: &Path) -> Result<Self, SuperviseError> {
        let file = files::inherited_file(fd).map_err(|error| SuperviseError::Lock {
            path: data_dir.join(SUPERVISOR_LOCK),
            error,
        })?;
        Ok(Self { file })
    }
}

#[cfg(unix)]
async fn terminated() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

#[cfg(not(unix))]
async fn terminated() {
    let _ = tokio::signal::ctrl_c().await;
}

/// Makes a rename or removal of `path` durable.
fn sync_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => sync_dir(dir),
        _ => sync_dir(Path::new(".")),
    }
}

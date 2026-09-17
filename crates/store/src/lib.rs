//! SQLite-backed client store.
//!
//! Each open store owns one worker thread and one SQLite connection. The
//! public async facade sends operations to that thread, which also owns the
//! sidecar lease for the connection's lifetime.

#![forbid(unsafe_code)]

mod chat;
mod db;
mod dump;
mod families;
mod fleet;
mod maintain;
mod quarantine;
mod view;

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use db::{LibraryReport, OpenReport, StoreGenerations, linked_library_report, qualify_library};
pub use families::{
    CHAT, CHAT_SHAPE, CLAUDE_PTY, CLAUDE_SDK, CODEX, FLEET, FLEET_SHAPE, Family, META, Migration,
    REGISTRY, Regime, Registry, VIEW,
};
pub use fleet::FleetChange;
pub use fold::{
    AttemptId, CommitOutcome, Fleet, FleetAgent, FleetDelta, FleetHost, FleetSnapshot, Generations,
    Loaded, Membership, OpId, ProviderFold, StoreError, claude_pty, claude_sdk, codex,
};
use fold::{
    Entry, ExpectedHead, Head, Mutation, Page, PageToken, SegmentTransition, WindowBudget,
    WindowInterest,
};
pub use maintain::{Budget, MaintenanceReport};
use model::AgentId;
pub use quarantine::{QuarantineDurableState, QuarantineRecord, QuarantineReport};

const LEASE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum LockMode {
    Shared,
    Exclusive,
}

pub struct Store {
    sender: Option<Sender<Command>>,
    worker: Option<JoinHandle<()>>,
    path: PathBuf,
    generations: StoreGenerations,
    library: LibraryReport,
    open_report: OpenReport,
}

impl Store {
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let path = path.to_owned();
        let worker_path = path.clone();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("amux-store".to_owned())
            .spawn(move || worker(worker_path, receiver, ready_sender))
            .map_err(|_| StoreError::Io)?;

        match ready_receiver.recv().map_err(|_| StoreError::Io)? {
            Ok(ready) => Ok(Self {
                sender: Some(sender),
                worker: Some(worker),
                path,
                generations: ready.generations,
                library: ready.library,
                open_report: ready.open_report,
            }),
            Err(error) => {
                let _ = worker.join();
                Err(error)
            }
        }
    }

    pub fn generations(&self) -> StoreGenerations {
        self.generations
    }

    pub fn library_report(&self) -> &LibraryReport {
        &self.library
    }

    pub fn open_report(&self) -> OpenReport {
        self.open_report
    }

    pub async fn maintain(
        &self,
        budget: Budget,
        deadline: Duration,
    ) -> Result<MaintenanceReport, StoreError> {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .ok_or(StoreError::Corrupt)?
            .send(Command::Maintain {
                budget,
                deadline,
                reply: reply_sender,
            })
            .map_err(|_| StoreError::Corrupt)?;
        reply_receiver.recv().map_err(|_| StoreError::Corrupt)?
    }

    pub async fn apply_fleet(
        &self,
        generations: fold::Generations,
        delta: FleetDelta,
    ) -> Result<FleetChange, StoreError> {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .ok_or(StoreError::Corrupt)?
            .send(Command::ApplyFleet {
                generations,
                delta: Box::new(delta),
                reply: reply_sender,
            })
            .map_err(|_| StoreError::Corrupt)?;
        reply_receiver.recv().map_err(|_| StoreError::Corrupt)?
    }

    pub async fn fleet(&self, generations: fold::Generations) -> Result<Fleet, StoreError> {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .ok_or(StoreError::Corrupt)?
            .send(Command::Fleet {
                generations,
                reply: reply_sender,
            })
            .map_err(|_| StoreError::Corrupt)?;
        reply_receiver.recv().map_err(|_| StoreError::Corrupt)?
    }

    pub async fn load<F>(
        &self,
        agent: AgentId,
        window: WindowBudget,
    ) -> Result<fold::Loaded<F>, StoreError>
    where
        F: ProviderFold + Send + 'static,
        F::Entry: Send,
    {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.send_run(move |connection| {
            let result = chat::load::<F>(connection, agent, window);
            let corrupt = matches!(result, Err(StoreError::Corrupt));
            let _ = reply_sender.send(result);
            corrupt
        })?;
        reply_receiver.recv().map_err(|_| StoreError::Corrupt)?
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the public store API mirrors the complete optimistic commit contract"
    )]
    pub async fn commit<F>(
        &self,
        agent: AgentId,
        generations: Generations,
        expected: ExpectedHead,
        head: Head<F>,
        transition: Option<SegmentTransition>,
        mutations: Vec<Mutation<F::Entry>>,
        interest: WindowInterest,
    ) -> CommitOutcome<F>
    where
        F: ProviderFold + Send + 'static,
        F::Entry: Send,
        <F::Entry as Entry>::Partial: Send,
    {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        if let Err(error) = self.send_run(move |connection| {
            let result = chat::commit(
                connection,
                agent,
                generations,
                expected,
                head,
                transition,
                mutations,
                interest,
            );
            let corrupt = matches!(result, CommitOutcome::Refused(StoreError::Corrupt));
            let _ = reply_sender.send(result);
            corrupt
        }) {
            return CommitOutcome::Refused(error);
        }
        reply_receiver
            .recv()
            .unwrap_or(CommitOutcome::Refused(StoreError::Corrupt))
    }

    pub async fn page<F>(
        &self,
        agent: AgentId,
        token: PageToken,
        n: usize,
    ) -> Result<Page<F::Entry>, StoreError>
    where
        F: ProviderFold + Send + 'static,
        F::Entry: Send,
    {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.send_run(move |connection| {
            let result = chat::page::<F>(connection, agent, token, n);
            let corrupt = matches!(result, Err(StoreError::Corrupt));
            let _ = reply_sender.send(result);
            corrupt
        })?;
        reply_receiver.recv().map_err(|_| StoreError::Corrupt)?
    }

    pub async fn invalidate<F>(
        &self,
        agent: AgentId,
        generations: Generations,
        expected: ExpectedHead,
        reason: fold::BaselineReason,
    ) -> CommitOutcome<F>
    where
        F: ProviderFold + Send + 'static,
        F::Entry: Send,
    {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        if let Err(error) = self.send_run(move |connection| {
            let result = chat::invalidate::<F>(connection, agent, generations, expected, reason);
            let corrupt = matches!(result, CommitOutcome::Refused(StoreError::Corrupt));
            let _ = reply_sender.send(result);
            corrupt
        }) {
            return CommitOutcome::Refused(error);
        }
        reply_receiver
            .recv()
            .unwrap_or(CommitOutcome::Refused(StoreError::Corrupt))
    }

    pub async fn view_get(&self, kind: &str, key: &str) -> Result<Option<String>, StoreError> {
        let kind = kind.to_owned();
        let key = key.to_owned();
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.send_run(move |connection| {
            let result = view::get(connection, &kind, &key);
            let corrupt = matches!(result, Err(StoreError::Corrupt));
            let _ = reply_sender.send(result);
            corrupt
        })?;
        reply_receiver.recv().map_err(|_| StoreError::Corrupt)?
    }

    pub async fn view_set(&self, kind: &str, key: &str, value: &str) -> Result<(), StoreError> {
        let kind = kind.to_owned();
        let key = key.to_owned();
        let value = value.to_owned();
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.send_run(move |connection| {
            let result = view::set(connection, &kind, &key, &value);
            let corrupt = matches!(result, Err(StoreError::Corrupt));
            let _ = reply_sender.send(result);
            corrupt
        })?;
        reply_receiver.recv().map_err(|_| StoreError::Corrupt)?
    }

    pub async fn data_version(&self) -> Result<u64, StoreError> {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.send_run(move |connection| {
            let result = connection
                .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
                .map_err(db::map_sqlite_error)
                .and_then(|value| u64::try_from(value).map_err(|_| StoreError::Corrupt));
            let corrupt = matches!(result, Err(StoreError::Corrupt));
            let _ = reply_sender.send(result);
            corrupt
        })?;
        reply_receiver.recv().map_err(|_| StoreError::Corrupt)?
    }

    pub async fn record_chat_opened(&self, agent: AgentId) -> Result<(), StoreError> {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.send_run(move |connection| {
            let result = connection
                .execute(
                    "UPDATE agent SET last_opened_at=?2 WHERE id=?1",
                    rusqlite::params![agent.to_string(), chrono::Utc::now().timestamp_millis()],
                )
                .map(|_| ())
                .map_err(db::map_sqlite_error);
            let corrupt = matches!(result, Err(StoreError::Corrupt));
            let _ = reply_sender.send(result);
            corrupt
        })?;
        reply_receiver.recv().map_err(|_| StoreError::Corrupt)?
    }

    pub async fn dump(&self, agent: AgentId) -> Result<String, StoreError> {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.send_run(move |connection| {
            let result = dump::render(connection, agent);
            let corrupt = matches!(result, Err(StoreError::Corrupt));
            let _ = reply_sender.send(result);
            corrupt
        })?;
        reply_receiver.recv().map_err(|_| StoreError::Corrupt)?
    }

    pub async fn quarantine_report(&self) -> Result<QuarantineReport, StoreError> {
        let path = self.path.clone();
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.send_run(move |connection| {
            let result = quarantine::inspect(connection, &path);
            let corrupt = matches!(result, Err(StoreError::Corrupt));
            let _ = reply_sender.send(result);
            corrupt
        })?;
        reply_receiver.recv().map_err(|_| StoreError::Corrupt)?
    }

    pub async fn resolve_quarantine(
        path: &Path,
        report: &QuarantineReport,
    ) -> Result<(), StoreError> {
        if report.is_empty() {
            return Err(StoreError::Invalid);
        }
        let parent = path.parent().ok_or(StoreError::Io)?;
        std::fs::create_dir_all(parent).map_err(map_io)?;
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(parent.join("store.lock"))
            .map_err(map_io)?;
        acquire_lock(&lock_file, LockMode::Exclusive)?;

        if quarantine::has_pending(path)? {
            return Err(StoreError::Invalid);
        }
        let mut opened = db::open_database(path, &[])?;
        quarantine::resolve(&mut opened.connection, &report.ids())
    }

    fn send_run(
        &self,
        run: impl FnOnce(&mut rusqlite::Connection) -> bool + Send + 'static,
    ) -> Result<(), StoreError> {
        self.sender
            .as_ref()
            .ok_or(StoreError::Corrupt)?
            .send(Command::Run(Box::new(run)))
            .map_err(|_| StoreError::Corrupt)
    }

    pub async fn close(mut self) {
        self.close_inner();
    }

    fn close_inner(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Command::Close);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        self.close_inner();
    }
}

enum Command {
    Run(Box<dyn FnOnce(&mut rusqlite::Connection) -> bool + Send>),
    ApplyFleet {
        generations: fold::Generations,
        delta: Box<FleetDelta>,
        reply: mpsc::SyncSender<Result<FleetChange, StoreError>>,
    },
    Fleet {
        generations: fold::Generations,
        reply: mpsc::SyncSender<Result<Fleet, StoreError>>,
    },
    Maintain {
        budget: Budget,
        deadline: Duration,
        reply: mpsc::SyncSender<Result<MaintenanceReport, StoreError>>,
    },
    Close,
}

struct Ready {
    generations: StoreGenerations,
    library: LibraryReport,
    open_report: OpenReport,
}

fn worker(
    path: PathBuf,
    receiver: Receiver<Command>,
    ready: mpsc::SyncSender<Result<Ready, StoreError>>,
) {
    let opened = open_on_worker(&path);
    let (mut connection, lock_file, metadata) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            if error == StoreError::Corrupt {
                let _ = quarantine::request(&path, QuarantineDurableState::Unknown);
            }
            let _ = ready.send(Err(error));
            return;
        }
    };
    if ready.send(Ok(metadata)).is_err() {
        return;
    }

    let mut corrupt = false;
    let mut pending = None;
    loop {
        let command = match pending.take() {
            Some(command) => command,
            None => match receiver.recv() {
                Ok(command) => command,
                Err(_) => break,
            },
        };
        match command {
            Command::Run(run) => {
                corrupt = run(&mut connection);
                if corrupt {
                    break;
                }
            }
            Command::ApplyFleet {
                generations,
                delta,
                reply,
            } => {
                let result = fleet::apply(&mut connection, generations, *delta, chrono::Utc::now());
                corrupt = result == Err(StoreError::Corrupt);
                let _ = reply.send(result);
                if corrupt {
                    break;
                }
            }
            Command::Fleet { generations, reply } => {
                let result = fleet::load(&mut connection, generations);
                corrupt = result == Err(StoreError::Corrupt);
                let _ = reply.send(result);
                if corrupt {
                    break;
                }
            }
            Command::Maintain {
                budget,
                deadline,
                reply,
            } => {
                let mut queued = None;
                let result = maintain::run(&connection, budget, deadline, || {
                    if queued.is_none() {
                        queued = receiver.try_recv().ok();
                    }
                    queued.is_some()
                });
                corrupt = result == Err(StoreError::Corrupt);
                let _ = reply.send(result);
                if corrupt {
                    break;
                }
                pending = queued;
            }
            Command::Close => break,
        }
    }

    drop(connection);
    drop(lock_file);
    if corrupt {
        let _ = quarantine::request(&path, quarantine::known_durable_state());
    }
}

fn open_on_worker(path: &Path) -> Result<(rusqlite::Connection, File, Ready), StoreError> {
    let parent = path.parent().ok_or(StoreError::Io)?;
    std::fs::create_dir_all(parent).map_err(map_io)?;
    let lock_path = parent.join("store.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(map_io)?;

    let pending_before_lock = quarantine::has_pending(path)?;
    acquire_lock(
        &lock_file,
        if pending_before_lock {
            LockMode::Exclusive
        } else {
            LockMode::Shared
        },
    )?;

    let mut exclusive = pending_before_lock;
    if !exclusive && quarantine::has_pending(path)? {
        lock_file.unlock().map_err(map_io)?;
        acquire_lock(&lock_file, LockMode::Exclusive)?;
        exclusive = true;
    }

    let pending = if exclusive {
        quarantine::prepare_pending(path)?
    } else {
        Vec::new()
    };
    let opened = db::open_database(path, &pending)?;
    if exclusive {
        quarantine::finish_pending(&pending)?;
        lock_file.unlock().map_err(map_io)?;
        acquire_lock(&lock_file, LockMode::Shared)?;
    }
    let ready = Ready {
        generations: opened.generations,
        library: opened.library,
        open_report: opened.open_report,
    };
    Ok((opened.connection, lock_file, ready))
}

fn acquire_lock(file: &File, mode: LockMode) -> Result<(), StoreError> {
    let started = Instant::now();
    loop {
        let result = match mode {
            LockMode::Shared => file.try_lock_shared(),
            LockMode::Exclusive => file.try_lock(),
        };
        match result {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {
                if started.elapsed() >= LEASE_TIMEOUT {
                    return Err(StoreError::Busy);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(map_io(error)),
        }
    }
}

fn map_io(error: std::io::Error) -> StoreError {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => StoreError::Permission,
        _ => StoreError::Io,
    }
}

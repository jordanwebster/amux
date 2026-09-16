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

pub use db::{LibraryReport, StoreGenerations, qualify_library};
pub use families::{
    CHAT, CHAT_SHAPE, CLAUDE_PTY, CLAUDE_SDK, CODEX, FLEET, FLEET_SHAPE, Family, META, Migration,
    REGISTRY, Regime, Registry, VIEW,
};
pub use fleet::FleetChange;
use fold::{
    CommitOutcome, Entry, ExpectedHead, Generations, Head, Mutation, Page, PageToken, ProviderFold,
    SegmentTransition, WindowBudget, WindowInterest,
};
pub use fold::{Fleet, FleetAgent, FleetDelta, FleetHost, FleetSnapshot, Membership, StoreError};
pub use maintain::{Budget, MaintenanceReport};
use model::AgentId;
use rustix::fs::{FlockOperation, flock};

const LEASE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Store {
    sender: Option<Sender<Command>>,
    worker: Option<JoinHandle<()>>,
    generations: StoreGenerations,
    library: LibraryReport,
}

impl Store {
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let path = path.to_owned();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("amux-store".to_owned())
            .spawn(move || worker(path, receiver, ready_sender))
            .map_err(|_| StoreError::Io)?;

        match ready_receiver.recv().map_err(|_| StoreError::Io)? {
            Ok(ready) => Ok(Self {
                sender: Some(sender),
                worker: Some(worker),
                generations: ready.generations,
                library: ready.library,
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
            let _ = reply_sender.send(result);
            false
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
            let _ = reply_sender.send(result);
            false
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
                let _ = quarantine::request(&path);
            }
            let _ = ready.send(Err(error));
            return;
        }
    };
    if ready.send(Ok(metadata)).is_err() {
        return;
    }

    let mut corrupt = false;
    while let Ok(command) = receiver.recv() {
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
                let result = maintain::run(&connection, budget, deadline);
                corrupt = result == Err(StoreError::Corrupt);
                let _ = reply.send(result);
                if corrupt {
                    break;
                }
            }
            Command::Close => break,
        }
    }

    drop(connection);
    drop(lock_file);
    if corrupt {
        let _ = quarantine::request(&path);
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
            FlockOperation::NonBlockingLockExclusive
        } else {
            FlockOperation::NonBlockingLockShared
        },
    )?;

    let mut exclusive = pending_before_lock;
    if !exclusive && quarantine::has_pending(path)? {
        flock(&lock_file, FlockOperation::Unlock).map_err(|_| StoreError::Io)?;
        acquire_lock(&lock_file, FlockOperation::NonBlockingLockExclusive)?;
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
        flock(&lock_file, FlockOperation::LockShared).map_err(|_| StoreError::Io)?;
    }
    let ready = Ready {
        generations: opened.generations,
        library: opened.library,
    };
    Ok((opened.connection, lock_file, ready))
}

fn acquire_lock(file: &File, operation: FlockOperation) -> Result<(), StoreError> {
    let started = Instant::now();
    loop {
        match flock(file, operation) {
            Ok(()) => return Ok(()),
            Err(error) if error == rustix::io::Errno::AGAIN => {
                if started.elapsed() >= LEASE_TIMEOUT {
                    return Err(StoreError::Busy);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error == rustix::io::Errno::ACCESS => return Err(StoreError::Permission),
            Err(_) => return Err(StoreError::Io),
        }
    }
}

fn map_io(error: std::io::Error) -> StoreError {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => StoreError::Permission,
        _ => StoreError::Io,
    }
}

//! SQLite-backed client store.
//!
//! Each open store owns one worker thread and one SQLite connection. The
//! public async facade sends operations to that thread, which also owns the
//! sidecar lease for the connection's lifetime.

#![forbid(unsafe_code)]

mod db;
mod families;
mod maintain;
mod quarantine;

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
pub use fold::StoreError;
pub use maintain::{Budget, MaintenanceReport};
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
    let (connection, lock_file, metadata) = match opened {
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

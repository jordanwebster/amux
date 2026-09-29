//! The one startup path and the clean shutdown.
//!
//! There is no "started by an update" or "recovering from a crash" mode.
//! Every start takes the installation lock; rewrites the generation file,
//! bumping the counter after an unclean reboot; opens and migrates each
//! profile's store; looks at every agent directory without writing; tells
//! the supervisor it is prepared and waits for go, or goes at once with no
//! supervisor; and only then finishes the sweep, which is the first write
//! a rollback would have to undo, and binds the sockets clients dial. Crash
//! recovery, updates and a reboot after power loss all run this same code.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agent_dir::Clock;
use agent_dir::local_socket::{self, LocalListener};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::activation::{ActivationError, ActivationPipe};
use crate::edge::EdgeOptions;
use crate::front_door::{self, ProfileEvent};
use crate::generation::{self, Generation};
use crate::grpc::{self, ClientApi};
use crate::install::{InstallationLock, LockError, REPORTS, STORE, private_dir};
use crate::outbox::PushSender;
use crate::profiles::{self, PROFILE_SOCKET, ProfileEntry, ProfileId, Registry};
use crate::runtime::{Launch, Looked, Profile, ProfileRuntime, RegistryError, SweepReport};

pub struct StartOptions {
    pub data_dir: PathBuf,
    /// This boot of the machine; read from the system when None.
    pub boot_id: Option<String>,
    pub launch: Launch,
    pub clock: Arc<dyn Clock>,
    /// Where needs-you pushes go.
    pub push: Arc<dyn PushSender>,
    /// The daemon's own log file, which dumps include.
    pub daemon_log: Option<PathBuf>,
    /// Where the front door listens. None serves nothing on sockets: the
    /// runtimes are reached in process.
    pub front_door: Option<PathBuf>,
    /// What each profile's network edge serves.
    pub edge: EdgeOptions,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error("reading the boot id: {0}")]
    BootId(io::Error),
    #[error("the generation file: {0}")]
    Generation(io::Error),
    #[error("the profile registry: {0}")]
    Registry(io::Error),
    #[error("profile {profile}: {error}")]
    Profile {
        profile: ProfileId,
        error: io::Error,
    },
    #[error("profile {profile}'s store: {error}")]
    Store {
        profile: ProfileId,
        error: store::OpenError,
    },
    #[error("profile {profile}'s agents: {error}")]
    Sweep {
        profile: ProfileId,
        error: RegistryError,
    },
    #[error(transparent)]
    Activation(#[from] ActivationError),
    #[error("another daemon answers on {}", .0.display())]
    FrontDoorTaken(PathBuf),
    #[error("binding {}: {error}", .path.display())]
    Bind { path: PathBuf, error: io::Error },
    #[error("profile {profile}'s network edge: {error}")]
    Edge {
        profile: ProfileId,
        error: crate::edge::EdgeError,
    },
}

/// What the daemon shares with its front door: how to open a profile, the
/// profiles it hosts, and the shutdown request.
pub(crate) struct Installation {
    pub(crate) data_dir: PathBuf,
    pub(crate) front_door: Option<PathBuf>,
    generation: u64,
    launch: Launch,
    clock: Arc<dyn Clock>,
    push: Arc<dyn PushSender>,
    daemon_log: Option<PathBuf>,
    edge: EdgeOptions,
    pub(crate) hosted: Mutex<BTreeMap<ProfileId, Hosted>>,
    /// One registry change at a time: create, rename, delete.
    pub(crate) registry_changes: tokio::sync::Mutex<()>,
    /// Profile changes in order, numbered from one.
    pub(crate) events: broadcast::Sender<(u64, ProfileEvent)>,
    pub(crate) sequence: Mutex<u64>,
    shutdown: watch::Sender<bool>,
}

pub(crate) struct Hosted {
    pub(crate) runtime: Arc<ProfileRuntime>,
    pub(crate) entry: ProfileEntry,
    /// Where the profile stands in the registry: the front door lists
    /// profiles in the order they were created, oldest first.
    pub(crate) position: usize,
    socket: Option<JoinHandle<()>>,
    /// Republishes the profile when its cloud link's state moves.
    observer: Option<JoinHandle<()>>,
}

impl Drop for Hosted {
    fn drop(&mut self) {
        if let Some(socket) = self.socket.take() {
            socket.abort();
        }
        if let Some(observer) = self.observer.take() {
            observer.abort();
        }
    }
}

impl Installation {
    /// Opens a profile's store and its runtime. Writes nothing but the
    /// store's migrations.
    fn open(&self, profile: ProfileId) -> Result<Arc<ProfileRuntime>, StartError> {
        let dir = profiles::profile_dir(&self.data_dir, profile);
        let host =
            profiles::host_id(&dir).map_err(|error| StartError::Profile { profile, error })?;
        let store = store::Sqlite::open(&dir.join(STORE), host.as_bytes().to_vec())
            .map_err(|error| StartError::Store { profile, error })?;
        Ok(ProfileRuntime::open(Profile {
            profile,
            host,
            generation: self.generation,
            dir,
            store,
            launch: self.launch.clone(),
            clock: self.clock.clone(),
            push: self.push.clone(),
            reports: self.data_dir.join(REPORTS),
            daemon_log: self.daemon_log.clone(),
        }))
    }

    /// Binds a profile's client socket when the daemon serves sockets.
    fn bind(&self, runtime: &Arc<ProfileRuntime>) -> Result<Option<JoinHandle<()>>, StartError> {
        if self.front_door.is_none() {
            return Ok(None);
        }
        let path = runtime.dir().join(PROFILE_SOCKET);
        let listener = LocalListener::bind(&path).map_err(|error| StartError::Bind {
            path: path.clone(),
            error,
        })?;
        Ok(Some(grpc::serve_client(
            listener,
            ClientApi::new(runtime, None),
        )))
    }

    /// Puts a profile whose sweep is finished into service: its outboxes,
    /// its network edge, its socket, its place in the front door's list.
    async fn host(
        self: &Arc<Self>,
        runtime: Arc<ProfileRuntime>,
        entry: ProfileEntry,
    ) -> Result<(), StartError> {
        runtime.start_background();
        let edge = runtime
            .start_edge(&self.edge)
            .await
            .map_err(|error| StartError::Edge {
                profile: entry.id,
                error,
            })?;
        let observer = {
            let installation = Arc::downgrade(self);
            let profile = entry.id;
            let mut observed = edge.subscribe_observed();
            tokio::spawn(async move {
                while observed.changed().await.is_ok() {
                    let Some(installation) = installation.upgrade() else {
                        return;
                    };
                    installation.republish(profile);
                }
            })
        };
        let socket = self.bind(&runtime)?;
        let info = {
            let mut all = self.hosted.lock().unwrap();
            let position = all
                .values()
                .map(|hosted| hosted.position + 1)
                .max()
                .unwrap_or(0);
            let hosted = Hosted {
                runtime,
                entry,
                position,
                socket,
                observer: Some(observer),
            };
            let info = front_door::info(&hosted);
            all.insert(hosted.entry.id, hosted);
            info
        };
        self.publish(ProfileEvent::Upserted(Box::new(info)));
        Ok(())
    }

    /// Creates, opens, sweeps and hosts a new profile.
    pub(crate) async fn create(self: &Arc<Self>, label: &str) -> Result<ProfileId, StartError> {
        let entry =
            profiles::create_labelled(&self.data_dir, label).map_err(StartError::Registry)?;
        let profile = entry.id;
        let runtime = self.open(profile)?;
        let looked = runtime
            .look()
            .await
            .map_err(|error| StartError::Sweep { profile, error })?;
        runtime
            .finish_sweep(looked)
            .await
            .map_err(|error| StartError::Sweep { profile, error })?;
        self.host(runtime, entry).await?;
        Ok(profile)
    }

    pub(crate) fn publish(&self, event: ProfileEvent) {
        let mut sequence = self.sequence.lock().unwrap();
        *sequence += 1;
        let _ = self.events.send((*sequence, event));
    }

    /// Tells the front door's watchers a profile's description moved.
    pub(crate) fn republish(&self, profile: ProfileId) {
        let info = self
            .hosted
            .lock()
            .unwrap()
            .get(&profile)
            .map(front_door::info);
        if let Some(info) = info {
            self.publish(ProfileEvent::Upserted(Box::new(info)));
        }
    }

    pub(crate) fn request_shutdown(&self) {
        self.shutdown.send_replace(true);
    }
}

pub struct Daemon {
    installation: Arc<Installation>,
    generation: Generation,
    sweep: BTreeMap<ProfileId, SweepReport>,
    supervisor: Option<ActivationPipe>,
    front_door: Option<JoinHandle<()>>,
    // Dropped last: the lock outlives everything that writes under it.
    lock: Option<InstallationLock>,
}

impl Drop for Daemon {
    /// A daemon dropped without a shutdown is a crash: nothing is flushed
    /// and the installation stays dirty, but no task of this run goes on
    /// writing once its lock is released.
    fn drop(&mut self) {
        if let Some(front_door) = self.front_door.take() {
            front_door.abort();
        }
        let hosted = std::mem::take(&mut *self.installation.hosted.lock().unwrap());
        for hosted in hosted.values() {
            hosted.runtime.abort_all();
        }
    }
}

/// Starts the daemon for one installation. With a pipe, writes `prepared`
/// once the stores are migrated and the agent directories looked at, and
/// waits for `go` before anything else is written.
pub async fn start(
    options: StartOptions,
    pipe: Option<ActivationPipe>,
) -> Result<Daemon, StartError> {
    let StartOptions {
        data_dir,
        boot_id,
        launch,
        clock,
        push,
        daemon_log,
        front_door,
        edge,
    } = options;
    let lock = InstallationLock::acquire(&data_dir)?;
    let boot_id = match boot_id {
        Some(boot_id) => boot_id,
        None => generation::boot_id().map_err(StartError::BootId)?,
    };
    // Before anything is served: a peer must never see this run's
    // revisions under the previous generation.
    let generation = Generation::start(&data_dir, &boot_id).map_err(StartError::Generation)?;

    let installation = Arc::new(Installation {
        data_dir,
        front_door,
        generation: generation.counter,
        launch,
        clock,
        push,
        daemon_log,
        edge,
        hosted: Mutex::new(BTreeMap::new()),
        registry_changes: tokio::sync::Mutex::new(()),
        events: broadcast::channel(256).0,
        sequence: Mutex::new(0),
        shutdown: watch::Sender::new(false),
    });

    let registry = Registry::read(&installation.data_dir).map_err(StartError::Registry)?;
    let mut opened: Vec<(ProfileEntry, Arc<ProfileRuntime>, Looked)> = Vec::new();
    for entry in registry.profiles {
        let profile = entry.id;
        let runtime = installation.open(profile)?;
        let look = runtime
            .look()
            .await
            .map_err(|error| StartError::Sweep { profile, error })?;
        opened.push((entry, runtime, look));
    }

    let mut supervisor = pipe;
    if let Some(pipe) = supervisor.as_mut() {
        pipe.activate().await?;
    }

    let mut sweep = BTreeMap::new();
    let mut swept = Vec::new();
    for (entry, runtime, look) in opened {
        let profile = entry.id;
        let report = runtime
            .finish_sweep(look)
            .await
            .map_err(|error| StartError::Sweep { profile, error })?;
        sweep.insert(profile, report);
        swept.push((entry, runtime));
    }
    // A front door that answers belongs to another installation's daemon;
    // taking its socket would strand that daemon's clients.
    if let Some(path) = &installation.front_door
        && local_socket::connect(path).await.is_ok()
    {
        return Err(StartError::FrontDoorTaken(path.clone()));
    }
    // Every own journal has been read to its end: the outboxes may run.
    for (entry, runtime) in swept {
        installation.host(runtime, entry).await?;
    }
    // An installation always has a profile to serve.
    if installation.hosted.lock().unwrap().is_empty() {
        let profile = installation.create("default").await?;
        sweep.insert(profile, SweepReport::default());
    }
    let front_door = match &installation.front_door {
        Some(path) => Some(bind_front_door(&installation, path)?),
        None => None,
    };

    Ok(Daemon {
        installation,
        generation,
        sweep,
        supervisor,
        front_door,
        lock: Some(lock),
    })
}

fn bind_front_door(
    installation: &Arc<Installation>,
    path: &Path,
) -> Result<JoinHandle<()>, StartError> {
    let bind = |error| StartError::Bind {
        path: path.to_owned(),
        error,
    };
    if let Some(parent) = path.parent() {
        private_dir(parent).map_err(bind)?;
    }
    let listener = LocalListener::bind(path).map_err(bind)?;
    Ok(front_door::serve(listener, Arc::downgrade(installation)))
}

impl Daemon {
    pub fn data_dir(&self) -> &Path {
        &self.installation.data_dir
    }

    /// This run's generation file, as written at start.
    pub fn generation(&self) -> &Generation {
        &self.generation
    }

    pub fn profiles(&self) -> Vec<Arc<ProfileRuntime>> {
        self.installation
            .hosted
            .lock()
            .unwrap()
            .values()
            .map(|hosted| hosted.runtime.clone())
            .collect()
    }

    pub fn profile(&self, profile: ProfileId) -> Option<Arc<ProfileRuntime>> {
        self.installation
            .hosted
            .lock()
            .unwrap()
            .get(&profile)
            .map(|hosted| hosted.runtime.clone())
    }

    /// The front door's services in process: profiles, pairing, peers and
    /// accounts, for an embedder that serves no socket.
    pub fn front_door(&self) -> crate::FrontDoor {
        crate::FrontDoor::new(Arc::downgrade(&self.installation))
    }

    /// What the startup sweep found, per profile.
    pub fn sweep(&self, profile: ProfileId) -> Option<&SweepReport> {
        self.sweep.get(&profile)
    }

    /// The supervisor pipe, once activation is done: its end of file is
    /// the signal to shut down.
    pub fn take_supervisor(&mut self) -> Option<ActivationPipe> {
        self.supervisor.take()
    }

    /// Resolves once a client has asked the daemon to shut down.
    pub async fn shutdown_requested(&self) {
        let mut shutdown = self.installation.shutdown.subscribe();
        let _ = shutdown.wait_for(|requested| *requested).await;
    }

    /// Shuts down cleanly: stops serving, stops every writer, flushes each
    /// store to the drive, and sets the clean flag as the very last write.
    /// Agents keep running; they wait out their grace for the next daemon.
    /// A failure before the flag leaves the installation dirty, the safe
    /// error.
    pub async fn shutdown(mut self) -> io::Result<()> {
        if let Some(front_door) = self.front_door.take() {
            front_door.abort();
        }
        let hosted = std::mem::take(&mut *self.installation.hosted.lock().unwrap());
        for hosted in hosted.values() {
            hosted
                .runtime
                .stop_edge(wire::LinkCloseReason::UserShutdown)
                .await;
        }
        for hosted in hosted.values() {
            hosted.runtime.stop_background().await;
            hosted.runtime.stop_watching().await;
        }
        for hosted in hosted.values() {
            hosted
                .runtime
                .store()
                .await
                .flush_to_drive()
                .map_err(io::Error::other)?;
        }
        self.generation.mark_clean(&self.installation.data_dir)?;
        drop(hosted);
        // Said while the lock is still held: whoever waits for the lock to
        // free reads a log that already ends here.
        tracing::info!("stopped cleanly");
        drop(self.lock.take());
        Ok(())
    }
}

//! The one startup path and the clean shutdown.
//!
//! There is no "started by an update" or "recovering from a crash" mode.
//! Every start takes the installation lock; rewrites the generation file,
//! bumping the counter after an unclean reboot; opens and migrates each
//! profile's store; looks at every agent directory without writing; tells
//! the supervisor it is prepared and waits for go, or goes at once with no
//! supervisor; and only then finishes the sweep, which is the first write
//! a rollback would have to undo. Crash recovery, updates and a reboot
//! after power loss all run this same code.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_dir::Clock;

use crate::activation::{ActivationError, ActivationPipe};
use crate::generation::{self, Generation};
use crate::install::{InstallationLock, LockError, STORE};
use crate::profiles::{self, ProfileId, Registry};
use crate::runtime::{Launch, ProfileRuntime, RegistryError, SweepReport};

pub struct StartOptions {
    pub data_dir: PathBuf,
    /// This boot of the machine; read from the system when None.
    pub boot_id: Option<String>,
    pub launch: Launch,
    pub clock: Arc<dyn Clock>,
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
}

pub struct Daemon {
    data_dir: PathBuf,
    generation: Generation,
    profiles: BTreeMap<ProfileId, Arc<ProfileRuntime>>,
    sweep: BTreeMap<ProfileId, SweepReport>,
    supervisor: Option<ActivationPipe>,
    // Dropped last: the lock outlives everything that writes under it.
    _lock: InstallationLock,
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
    } = options;
    let lock = InstallationLock::acquire(&data_dir)?;
    let boot_id = match boot_id {
        Some(boot_id) => boot_id,
        None => generation::boot_id().map_err(StartError::BootId)?,
    };
    // Before anything is served: a peer must never see this run's
    // revisions under the previous generation.
    let generation = Generation::start(&data_dir, &boot_id).map_err(StartError::Generation)?;

    let registry = Registry::read(&data_dir).map_err(StartError::Registry)?;
    let mut runtimes = BTreeMap::new();
    for profile in registry.profiles {
        let dir = profiles::profile_dir(&data_dir, profile);
        let host =
            profiles::host_id(&dir).map_err(|error| StartError::Profile { profile, error })?;
        let store = store::Sqlite::open(&dir.join(STORE), host.as_bytes().to_vec())
            .map_err(|error| StartError::Store { profile, error })?;
        let runtime = ProfileRuntime::open(
            profile,
            host,
            generation.counter,
            dir,
            store,
            launch.clone(),
            clock.clone(),
        );
        runtimes.insert(profile, runtime);
    }

    let mut looked = Vec::new();
    for (&profile, runtime) in &runtimes {
        let look = runtime
            .look()
            .await
            .map_err(|error| StartError::Sweep { profile, error })?;
        looked.push((profile, look));
    }

    let mut supervisor = pipe;
    if let Some(pipe) = supervisor.as_mut() {
        pipe.activate().await?;
    }

    let mut sweep = BTreeMap::new();
    for (profile, look) in looked {
        let report = runtimes[&profile]
            .finish_sweep(look)
            .await
            .map_err(|error| StartError::Sweep { profile, error })?;
        sweep.insert(profile, report);
    }

    Ok(Daemon {
        data_dir,
        generation,
        profiles: runtimes,
        sweep,
        supervisor,
        _lock: lock,
    })
}

impl Daemon {
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// This run's generation file, as written at start.
    pub fn generation(&self) -> &Generation {
        &self.generation
    }

    pub fn profiles(&self) -> impl Iterator<Item = &Arc<ProfileRuntime>> {
        self.profiles.values()
    }

    pub fn profile(&self, profile: ProfileId) -> Option<&Arc<ProfileRuntime>> {
        self.profiles.get(&profile)
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

    /// Shuts down cleanly: stops every writer, flushes each store to the
    /// drive, and sets the clean flag as the very last write. Agents keep
    /// running; they wait out their grace for the next daemon. A failure
    /// before the flag leaves the installation dirty, the safe error.
    pub async fn shutdown(self) -> io::Result<()> {
        let Daemon {
            data_dir,
            generation,
            profiles,
            _lock: lock,
            ..
        } = self;
        for runtime in profiles.values() {
            runtime.stop_watching().await;
        }
        for runtime in profiles.values() {
            runtime
                .store()
                .await
                .flush_to_drive()
                .map_err(io::Error::other)?;
        }
        generation.mark_clean(&data_dir)?;
        drop(lock);
        Ok(())
    }
}

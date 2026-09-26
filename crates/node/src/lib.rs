//! The amux daemon.
//!
//! One daemon process is an installation: it holds the installation lock,
//! the profile registry and the generation file, and hosts one
//! [`ProfileRuntime`] per profile. A profile runtime owns its store and its
//! agents: every agent runs as its own process (`amux agent <dir>`) in a
//! directory under the profile, writes what happened to its journal there,
//! and the runtime ingests that journal into the store.
//!
//! [`start`] is the one startup path; [`Daemon::shutdown`] the clean
//! shutdown that marks the installation clean.

mod activation;
mod blobs;
mod daemon;
mod dump;
mod fanout;
mod generation;
mod install;
mod outbox;
mod profiles;
mod relay;
mod retention;
mod runtime;
mod serve;
mod spec;

pub use activation::{ActivationError, ActivationPipe, GO, PREPARED};
pub use blobs::{BlobError, PATCH_MIME};
pub use daemon::{Daemon, StartError, StartOptions, start};
pub use dump::{DAEMON_LOG, DUMP_LOG_BYTES, DUMP_ROWS, DUMP_SEGMENTS, DumpError, MANIFEST, pack};
pub use generation::{Generation, boot_id};
pub use install::{
    AGENTS, GENERATION, HOST_ID, INSTALLATION_LOCK, InstallationLock, LockError, PROFILES,
    REGISTRY, REPLICAS, REPORTS, STORE,
};
pub use outbox::{DrainReport, HttpSender, NoopSender, Push, PushError, PushFuture, PushSender};
pub use profiles::{ProfileId, Registry, create_profile, host_id, profile_dir};
pub use relay::{EXITED, EXITING, RelayError};
pub use retention::{REMOVED_BY_RETENTION, Retention};
pub use runtime::{
    AGENT_LOG, AgentId, CAUSE_ABORTED, CAUSE_EXITED, CAUSE_EXITED_AWAY, CAUSE_KILLED,
    CAUSE_NO_DIRECTORY, CAUSE_STOPPED, CAUSE_UNSTARTED, JoinHook, KEPT_SEGMENTS, Launch, Profile,
    ProfileRuntime, RegistryError, SweepReport, TAIL_ROWS, locked, to_wire,
};
pub use serve::{InventorySubscription, MAX_PAGE, ServeError, Subscription};

/// The version this daemon writes into every spec.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

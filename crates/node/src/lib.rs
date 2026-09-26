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
mod audit;
mod auth;
mod blobs;
mod connection;
mod daemon;
pub mod discovery;
mod dispatcher;
mod dump;
mod edge;
mod fanout;
mod forward;
mod front_door;
mod generation;
mod grpc;
mod hosts;
mod identity;
mod install;
mod link;
mod net_error;
mod outbox;
mod pairing;
mod profiles;
mod relay;
mod resource_limits;
mod retention;
mod routing;
mod runtime;
mod serve;
mod services;
mod sources;
mod spec;
mod transport;
mod trust;

pub use activation::{ActivationError, ActivationPipe, GO, PREPARED};
pub use blobs::{BlobError, PATCH_MIME};
pub use daemon::{Daemon, StartError, StartOptions, start};
pub use dump::{DAEMON_LOG, DUMP_LOG_BYTES, DUMP_ROWS, DUMP_SEGMENTS, DumpError, MANIFEST, pack};
pub use edge::{
    ACCOUNT_FILE, CloudLinkServer, CloudOptions, DiscoveryFactory, Edge, EdgeError, EdgeOptions,
    FREE_TIER_REFRESH_INTERVAL, JwtCloudLinkAuthenticator, LINK_SOCKET, LanOptions, LoopbackLink,
    Observed, RelayCarrier, RelayIdentity, RelayQuic, UDP_BLOCKED_MEMORY, mdns_discovery,
};
pub use forward::{ForwardError, HostNameError, Owner};
pub use generation::{Generation, boot_id};
pub use grpc::{ClientApi, status};
pub use install::{
    AGENTS, GENERATION, HOST_ID, INSTALLATION_LOCK, InstallationLock, LockError, PROFILES,
    REGISTRY, REPLICAS, REPORTS, STORE,
};
pub use outbox::{DrainReport, HttpSender, NoopSender, Push, PushError, PushFuture, PushSender};
pub use profiles::{
    PROFILE_SOCKET, ProfileEntry, ProfileId, Registry, create_profile, host_id, profile_dir,
};
pub use relay::{EXITED, EXITING, RelayError};
pub use retention::{REMOVED_BY_RETENTION, Retention};
pub use runtime::{
    AGENT_LOG, AgentId, CAUSE_ABORTED, CAUSE_EXITED, CAUSE_EXITED_AWAY, CAUSE_KILLED,
    CAUSE_NO_DIRECTORY, CAUSE_STOPPED, CAUSE_UNSTARTED, INGEST_BATCH, JoinHook, KEPT_SEGMENTS,
    Launch, Profile, ProfileRuntime, RegistryError, SweepReport, TAIL_ROWS, locked, to_wire,
};
pub use serve::{InventorySubscription, MAX_PAGE, ServeError, Subscription};
pub use sources::{
    InventoryHook, NO_LONGER_LISTED, NOT_TRUSTED, SourceHook, SourcePolicy, SourceVerdict,
};

/// Internals the testnet harness assembles topologies from. Hidden and
/// unstable: reachable only for test infrastructure, never for products.
#[doc(hidden)]
pub mod harness {
    pub use crate::auth::claims::Tier;
    pub use crate::discovery::{Advertisement, Discovery, DiscoveryEvent, ScriptedDiscovery};
    pub use crate::identity::device_key_path;
    pub use crate::pairing::qr::{
        QrPairingError, QrPairingPayload, encode_qr_pairing_invitation, parse_qr_pairing_payload,
    };
    pub use crate::routing::{AuthenticatedLinkUser, HostVia, LinkTokenAuthenticator};
    pub use crate::transport::{
        relay_quic_client_config_with_roots, relay_quic_server_config_from_der,
    };

    /// A discovery factory whose instances share one scripted bus.
    pub fn scripted_discovery(bus: &ScriptedDiscovery) -> crate::edge::DiscoveryFactory {
        let bus = bus.clone();
        std::sync::Arc::new(move || {
            Ok(std::sync::Arc::new(bus.clone()) as std::sync::Arc<dyn Discovery>)
        })
    }
}

/// A host's id: one per profile, the identity its peers pin.
pub type HostId = uuid::Uuid;

pub use agent_dir::Clock;
pub use auth::claims::Tier;
pub use auth::oauth::{OAuthError, run_device_flow};
pub use transport::{TransportError, create_tls_acceptor, relay_quic_server_config};
pub use wire::PROTOCOL_VERSION;

/// The version this daemon writes into every spec.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#![allow(clippy::result_large_err)]
#[cfg(test)]
extern crate self as node;
pub mod agent_tools;
mod agents;
mod audit;
mod auth;
mod config;
mod connection;
mod debug;
mod dispatcher;
use model::envelope;
mod identity;
pub mod installation;
mod pairing;
mod paths;
mod profile;
mod resource_limits;
mod routing;
mod server;
mod services;
mod sleep_inhibitor;
mod subscription;
/// The fake identity service and account fixtures shared by node's own tests
/// and the `testnet` harness. Compiled unconditionally so the harness can use
/// it without a feature and without a second copy of node's types.
#[doc(hidden)]
pub mod test_fixtures;
mod transport;
mod trust;
mod tunnel;
pub mod update;
#[doc(hidden)]
pub mod user_state;

/// Internals the `testnet` harness needs to assemble whole daemons in-process.
/// Hidden and unstable: reachable only for test infrastructure, never for
/// products, and widened only when a harness verb needs another item.
#[doc(hidden)]
pub mod harness {
    pub use crate::agents::{AgentEvent, Protocol, SendInputRequest, agent_from_wire};
    pub use crate::auth::AuthError;
    pub use crate::config::Config;
    pub use crate::connection::ConnectionManager;
    pub use crate::dispatcher::TrackedTcpConnections;
    pub use crate::identity::{DeviceIdentity, device_key_path, load_or_create_device_identity_in};
    pub use crate::routing::{
        AuthenticatedLinkUser, Capabilities, Host, HostEntry, HostTrustStatus, LinkConnectorAuth,
        LinkConnectorToken, LinkConnectorTokenRefresher, LinkTokenAuthenticator, Route,
        RoutingCore, RoutingEvent, host_to_wire,
    };
    pub use crate::server::ShutdownReason;
    pub use crate::services::{
        AgentServiceCtx, ClientService, CloudLinkService, DeviceRuntimeSecurity,
        PeerTrustCommitContext, PeerTrustUpdate, StartedUserServices, commit_peer_trust,
        start_user_services,
    };
    pub use crate::transport::{
        InProcessConnection, TcpServerTransport, trusted_device_channel_tracked,
    };
    pub use crate::trust::{Reachability, SharedTrustStore, TrustEntry, TrustStore};
    pub use crate::tunnel::TunnelPool;

    pub mod runtime {
        pub use crate::profile::runtime::{
            CloudFixtureAuth, Listeners, ProfileRuntime, ProfileRuntimeOptions, RuntimeFixtures,
            start,
        };
    }
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use agents::{
    Agent, AgentEvent, AgentKind, AgentParent, AgentType, ArtifactRef, BaseIdentity,
    CreateAgentRequest, DiffBase, DiffFile, DiffResponse, Protocol, SessionCloseReason,
    SubscribeSessionEvent, TerminalSize, WorkingOn,
};
pub use auth::oauth::{OAuthError, refresh_access_token, run_device_flow};
pub use auth::{AccessToken, AuthError, CredentialProvider};
pub use client::{
    AgentEventStream, Client, ClientError, ConnectError, DeleteAgentSummary, DeviceIdentity,
    HostEventStream, PairingError, PairingSecret, PairingStart, PeerEntry, PeerReachability,
    PendingPeer, SessionStream,
};
pub use config::{
    ColorSetting, Config, ConfigError, InstallationConfig, Keybinds, LeaderKey, OpenMode,
    ProfileConfig, ResolvedConfig, ThemeSetting, UiSettings, load_profile_config,
};
pub use debug::DebugFormat;
pub use identity::{device_files_ready_in, ensure_device_files_in, stored_host_id_in};
pub use installation::{
    BindError, BindRequest, BindTarget, CloudServiceId, CredentialSource, Installation,
    InstallationError, InstallationOptions, InstallationRoot, InstallationSettings, Listeners,
    OperationId, ProfileAdmin, ProfileEvent, ProfileId, ProfileStatus, ProfileWatch,
    RelocationPolicy, ResumeReport, SuspendReason, SuspendReport,
};
pub use model::{
    AgentId, AgentIdentifier, ArtifactId, ArtifactKind, DisconnectReason, HostId,
    ListRepositoriesRequest, ListRepositoriesResponse, PeerIdentifier, ProjectEntry, ProtocolError,
    RelayConnection, SendInputRequest, SendMessageRequest, SetAgentStatusRequest,
    SubscribeSessionRequest,
};
pub use pairing::PairingAdmin;
pub use pairing::pin::{PinPairingError, pair_via_pin_direct_tcp};
pub use pairing::qr::{
    QrPairingError, QrPairingPayload, encode_qr_pairing_invitation, encode_qr_pairing_payload,
    parse_qr_pairing_payload,
};
pub use pairing::ssh::{
    SshPairingError, SshPairingPeer, SshPairingProfile, SshTarget, pair_via_ssh_initiator,
    pair_via_ssh_responder, pair_via_ssh_target,
};
#[cfg(unix)]
pub use pairing::ssh::{pair_via_ssh_responder_stdio, relay_stdio_to_unix_socket};
pub use paths::{default_data_dir, default_log_path, keymap_dir};
pub use routing::{Capabilities, Host, HostEntry, HostEvent, HostTrustStatus, SupportedAgentType};
pub use server::{
    DaemonBuilder, EmbeddedRuntime, Server, ServerBuilder, ServerError, ShutdownReason,
};
pub use subscription::SubscriptionReporter;
pub use transport::{EmbeddedRelay, RelayEndpoint, RelayRetry, TransportError};
pub use update::{UpdateInfo, UpdateReporter, UpdateStatus};
pub use wire::PROTOCOL_VERSION;

#![allow(clippy::result_large_err)]
#[cfg(test)]
extern crate self as node;
pub mod agent_tools;
mod agents;
mod audit;
mod auth;
mod clock;
mod config;
mod connection;
mod debug;
pub mod discovery;
mod dispatcher;
use model::envelope;
mod identity;
pub mod installation;
mod link;
mod pairing;
mod paths;
mod profile;
mod resource_limits;
mod routing;
mod server;
mod services;
mod sleep_inhibitor;
/// The fake identity service and account fixtures shared by node's own tests
/// and the `testnet` harness. Compiled unconditionally so the harness can use
/// it without a feature and without a second copy of node's types.
#[doc(hidden)]
pub mod test_fixtures;
mod transport;
mod trust;
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
    pub use crate::auth::claims::ConnectionClaims;
    pub use crate::auth::jwt::{JwtError, JwtValidator};
    pub use crate::config::{Config, LanConfig};
    pub use crate::connection::ConnectionManager;
    pub use crate::identity::{
        DeviceIdentity, QUIC_ALPN, device_key_path, ed25519_public_key_from_certificate,
        load_or_create_device_identity_in,
    };
    pub use crate::link::run::{LinkAuthSession, ReauthError};
    pub use crate::routing::{
        AuthenticatedLinkUser, Capabilities, ConnectRole, Host, HostEntry, HostTrustStatus,
        HostVia, LinkCarrier, LinkConnectorAuth, LinkConnectorToken, LinkConnectorTokenRefresher,
        LinkRole, LinkTokenAuthenticator, Route, RoutingCore, RoutingEvent, host_to_wire,
        spawn_connector_with_establishment,
    };
    pub use crate::server::{ShutdownReason, TLS_HANDSHAKE_TIMEOUT};
    pub use crate::services::{
        AgentServiceCtx, ClientService, CloudLinkServer, DeviceRuntimeSecurity,
        FREE_TIER_REFRESH_INTERVAL, PeerTrustCommitContext, PeerTrustUpdate, StartedUserServices,
        UDP_BLOCKED_MEMORY, commit_peer_trust, start_user_services,
    };
    pub use crate::transport::{
        InProcessConnection, pairing_quic_client_config, relay_quic_client_config_with_roots,
        relay_quic_server_config_from_der,
    };
    pub use crate::trust::{Reachability, SharedTrustStore, TrustEntry, TrustStore};

    /// The native-stream link runtime. Separate from the flat surface because
    /// the `link::LinkCarrier` trait shares its name with routing's enum.
    pub mod link {
        pub use crate::link::carrier::{AsyncStream, write_raw_control_frame};
        pub use crate::link::channels::BulkResponseHold;
        pub use crate::link::{
            CarrierKind, ChannelClass, ChannelDebug, ChannelError, ChannelPool, ControlSink,
            ControlSource, LinkCarrier, LinkCtx, MuxCarrier, MuxRole, OpenError, QuicCarrier,
            read_message, run_link, write_message,
        };
    }

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
pub use auth::claims::Tier;
pub use auth::oauth::{OAuthError, refresh_access_token, run_device_flow};
pub use auth::{AccessToken, AuthError, CredentialProvider};
pub use client::{
    AgentEventStream, Client, ClientError, ConnectError, DeleteAgentSummary, DeviceIdentity,
    HostEventStream, PairingCandidate, PairingError, PairingSecret, PairingStart, PeerEntry,
    PeerReachability, PeerVia, PendingPeer, SessionStream,
};
pub use clock::{Clock, WallClock};
pub use config::{
    ColorSetting, Config, ConfigError, InstallationConfig, Keybinds, LanConfig, LeaderKey,
    OpenMode, ProfileConfig, ResolvedConfig, ThemeSetting, UiSettings, load_profile_config,
};
pub use debug::DebugFormat;
pub use identity::{device_files_ready_in, ensure_device_files_in, stored_host_id_in};
#[cfg(unix)]
pub use installation::adjacent_link_socket_path;
pub use installation::{
    BindError, BindRequest, BindTarget, CloudServiceId, CredentialSource, Installation,
    InstallationError, InstallationOptions, InstallationRoot, InstallationSettings, Listeners,
    Observed, OperationId, ProfileAdmin, ProfileEvent, ProfileId, ProfileStatus, ProfileWatch,
    RelocationPolicy, ResumeReport, SuspendReason, SuspendReport,
};
pub use model::{
    AgentId, AgentIdentifier, ArtifactId, ArtifactKind, DisconnectReason, HostId,
    ListRepositoriesRequest, ListRepositoriesResponse, PeerIdentifier, ProjectEntry, ProtocolError,
    RelayConnection, SendInputRequest, SendMessageRequest, SetAgentStatusRequest,
    SubscribeSessionRequest,
};
pub use pairing::pin::{PinPairingError, pair_via_pin_direct_quic};
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
pub use pairing::{ONRAMP_PAIR_MODE_TTL, PairingAdmin};
pub use paths::{default_data_dir, default_log_path, keymap_dir};
pub use routing::{
    Capabilities, Host, HostEntry, HostEvent, HostTrustStatus, HostVia, SupportedAgentType,
};
pub use server::{
    DaemonBuilder, EmbeddedRuntime, Server, ServerBuilder, ServerError, ShutdownReason,
};
pub use transport::{EmbeddedRelay, RelayEndpoint, RelayRetry, TransportError};
pub use update::{UpdateInfo, UpdateReporter, UpdateStatus};
pub use wire::PROTOCOL_VERSION;

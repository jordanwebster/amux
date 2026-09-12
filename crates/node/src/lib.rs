#![allow(clippy::result_large_err)]
#[cfg(test)]
extern crate self as node;
pub mod agent_tools;
#[doc(hidden)]
pub mod agents;
mod audit;
#[doc(hidden)]
pub mod auth;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod connection;
mod debug;
#[doc(hidden)]
pub mod dispatcher;
#[doc(hidden)]
pub use model::envelope;
#[doc(hidden)]
pub mod identity;
pub mod installation;
#[doc(hidden)]
pub mod pairing;
mod paths;
#[doc(hidden)]
pub mod profile;
mod resource_limits;
#[doc(hidden)]
pub mod routing;
#[doc(hidden)]
pub mod server;
#[doc(hidden)]
pub mod services;
mod sleep_inhibitor;
mod subscription;
#[cfg(test)]
#[doc(hidden)]
#[path = "../../testnet/src/identity.rs"]
pub mod test_fixtures;
#[doc(hidden)]
pub mod transport;
#[doc(hidden)]
pub mod trust;
#[doc(hidden)]
pub mod tunnel;
pub mod update;
#[doc(hidden)]
pub mod user_state;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use agents::{
    Agent, AgentEvent, AgentKind, AgentParent, AgentType, ArtifactRef, BaseIdentity,
    CreateAgentRequest, DiffBase, DiffFile, DiffResponse, Protocol, SessionCloseReason,
    SubscribeSessionEvent, TerminalSize, WorkingOn,
};
pub use auth::oauth::{OAuthError, refresh_access_token, run_device_flow};
pub use auth::{AccessToken, AuthError, CredentialProvider};
pub use client::{
    AgentEventStream, Client, ClientError, ConnectError, DeleteAgentSummary, HostEventStream,
    PairingSecret, PairingStart, PeerEntry, PeerReachability, SessionStream,
};
pub use config::{
    ColorSetting, Config, ConfigError, InstallationConfig, Keybinds, LeaderKey, OpenMode,
    ProfileConfig, ResolvedConfig, ThemeSetting, UiSettings, load_profile_config,
};
pub use debug::DebugFormat;
pub use installation::{
    BindError, BindRequest, BindTarget, CredentialSource, Installation, InstallationError,
    InstallationOptions, InstallationRoot, InstallationSettings, Listeners, OperationId,
    ProfileAdmin, ProfileEvent, ProfileId, ProfileStatus, ProfileWatch, ResumeReport,
    SuspendReason, SuspendReport,
};
pub use model::{
    AgentId, AgentIdentifier, ArtifactId, ArtifactKind, HostId, PeerIdentifier, ProtocolError,
    SendInputRequest, SendMessageRequest, SetAgentStatusRequest, SubscribeSessionRequest,
};
pub use pairing::PairingAdmin;
pub use pairing::pin::{PinPairingError, pair_via_pin_direct_tcp};
pub use pairing::qr::{
    QrPairingError, QrPairingPayload, encode_qr_pairing_payload, parse_qr_pairing_payload,
    parse_qr_pairing_payload_for_cloud, validate_qr_payload_cloud_url,
};
pub use pairing::ssh::{
    SshPairingError, SshPairingPeer, SshPairingProfile, SshTarget, pair_via_ssh_initiator,
    pair_via_ssh_responder, pair_via_ssh_target,
};
#[cfg(unix)]
pub use pairing::ssh::{pair_via_ssh_responder_stdio, relay_stdio_to_unix_socket};
pub use paths::{default_data_dir, default_log_path, keymap_dir};
pub use routing::{Capabilities, Host, HostEntry, HostEvent, HostTrustStatus, SupportedAgentType};
pub use server::{DaemonBuilder, Server, ServerBuilder, ServerError, ShutdownReason};
pub use subscription::SubscriptionReporter;
pub use transport::TransportError;
pub use update::{UpdateInfo, UpdateReporter, UpdateStatus};
pub use wire::PROTOCOL_VERSION;

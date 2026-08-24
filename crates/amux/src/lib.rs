#![allow(clippy::result_large_err)]
#![cfg_attr(
    not(feature = "local-agents"),
    allow(
        dead_code,
        unused_imports,
        unused_mut,
        unused_variables,
        unreachable_code
    )
)]

#[cfg(all(target_os = "ios", feature = "local-agents"))]
compile_error!(
    "iOS builds must disable the `local-agents` feature; depend on amux with `default-features = false`."
);

pub mod agent_tools;
mod agents;
mod audit;
mod auth;
mod client;
mod config;
mod connection;
mod debug;
mod dispatcher;
#[path = "agents/envelope.rs"]
pub mod envelope;
mod identity;
mod pairing;
mod paths;
mod protocol;
mod resource_limits;
mod routing;
mod server;
mod services;
pub mod setup;
mod sleep_inhibitor;
mod state;
mod subscription;
mod suspend;
#[cfg(feature = "testnet")]
#[doc(hidden)]
pub mod testnet;
mod transport;
mod trust;
mod tunnel;
pub mod update;
mod user_state;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use agents::{
    Agent, AgentEvent, AgentParent, AgentType, CreateAgentRequest, SessionCloseReason,
    SubscribeSessionEvent, TerminalSize, WorkingOn,
};
pub use auth::oauth::{OAuthError, refresh_access_token, run_device_flow};
pub use auth::{AccessToken, AuthError, CredentialProvider};
pub use client::{
    AgentEventStream, Client, ClientError, ConnectError, DeleteAgentSummary, HostEventStream,
    PairingSecret, PairingStart, PeerEntry, PeerReachability, ResumeSummary, SessionStream,
    SuspendSummary,
};
pub use config::{Config, ConfigError, Keybinds, LeaderKey, OpenMode, UiSettings};
pub use debug::DebugFormat;
pub use pairing::pin::{PinPairingError, pair_via_pin_direct_tcp};
pub use pairing::qr::{
    QrPairingError, QrPairingPayload, encode_qr_pairing_payload, parse_qr_pairing_payload,
    parse_qr_pairing_payload_for_cloud, validate_qr_payload_cloud_url,
};
pub use pairing::ssh::{
    SshPairingError, SshPairingPeer, pair_via_ssh_initiator, pair_via_ssh_responder,
    pair_via_ssh_target,
};
#[cfg(unix)]
pub use pairing::ssh::{pair_via_ssh_responder_stdio, relay_stdio_to_unix_socket};
pub use paths::{default_data_dir, default_log_path};
pub use protocol::{PROTOCOL_VERSION, ProtocolError};
pub use routing::{Capabilities, Host, HostEntry, HostEvent, HostTrustStatus, SupportedAgentType};
pub use server::{
    DaemonBuilder, EmbeddedBuilder, Server, ServerBuilder, ServerError, ShutdownReason,
};
pub use subscription::SubscriptionReporter;
pub use transport::TransportError;
pub use update::{UpdateInfo, UpdateReporter, UpdateStatus};

pub mod claude_io {
    pub use crate::agents::claude::io::{
        ClaudePtyTranscriptV1Action, ClaudePtyTranscriptV1Args, ClaudePtyTranscriptV1Input,
        ClaudePtyTranscriptV1Output, ClaudePtyTranscriptV1ReplayQuery, PTY_TRANSCRIPT_V1,
        decode_pty_transcript_v1_cursor, decode_pty_transcript_v1_output,
        encode_pty_transcript_v1_args, encode_pty_transcript_v1_input,
    };
}

pub mod codex_io {
    pub use crate::agents::codex::CODEX_RAW_THREAD_NOT_READY;
    pub use crate::agents::codex::io::{
        CODEX_SDK_V1, CodexSdkV1Args, CodexSdkV1Input, CodexSdkV1Output, CodexSdkV1ReplayQuery,
        decode_codex_sdk_v1_output, encode_codex_sdk_v1_args, encode_codex_sdk_v1_input,
    };
}

pub mod terminal_io {
    pub use crate::agents::terminal_io::{
        TERMINAL_V1, TerminalV1Args, TerminalV1ReplayQuery, encode_terminal_v1_args,
    };
}

pub type AgentId = uuid::Uuid;
pub type HostId = uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentIdentifier {
    Id(AgentId),
    Name(String),
}

impl From<AgentId> for AgentIdentifier {
    fn from(id: AgentId) -> Self {
        Self::Id(id)
    }
}

impl From<String> for AgentIdentifier {
    fn from(name: String) -> Self {
        Self::Name(name)
    }
}

impl From<&str> for AgentIdentifier {
    fn from(value: &str) -> Self {
        uuid::Uuid::parse_str(value)
            .map(Self::Id)
            .unwrap_or_else(|_| Self::Name(value.to_string()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PeerIdentifier {
    Id(HostId),
    Name(String),
}

impl From<HostId> for PeerIdentifier {
    fn from(id: HostId) -> Self {
        Self::Id(id)
    }
}

impl From<String> for PeerIdentifier {
    fn from(name: String) -> Self {
        Self::Name(name)
    }
}

impl From<&str> for PeerIdentifier {
    fn from(value: &str) -> Self {
        uuid::Uuid::parse_str(value)
            .map(Self::Id)
            .unwrap_or_else(|_| Self::Name(value.to_string()))
    }
}

#[derive(Clone, Debug)]
pub struct SendInputRequest {
    pub agent: AgentIdentifier,
    /// Caller-supplied correlation id, returned verbatim in the structured
    /// stream's `amux.input_result` row.
    pub input_id: Vec<u8>,
    pub io_protocol: String,
    pub payload: bytes::Bytes,
}

#[derive(Clone, Debug)]
pub struct SendMessageRequest {
    pub to: AgentIdentifier,
    pub text: String,
    pub context: Option<AgentId>,
    pub from_agent_id: Option<AgentId>,
}

#[derive(Clone, Debug)]
pub struct SetAgentStatusRequest {
    pub agent: AgentIdentifier,
    pub working_on: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SubscribeSessionRequest {
    pub agent: AgentIdentifier,
    pub io_protocol: String,
    pub args: Option<bytes::Bytes>,
}

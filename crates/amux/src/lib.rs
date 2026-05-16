#![allow(clippy::result_large_err)]

mod agents;
mod auth;
mod client;
mod config;
mod debug;
mod paths;
mod protocol;
mod routing;
mod server;
mod services;
pub mod setup;
mod sleep_inhibitor;
mod state;
mod suspend;
mod transport;
mod tunnel;
pub mod update;
mod user_state;

pub use agents::{
    Agent, AgentEvent, AgentType, CreateAgentRequest, SessionCloseReason, SubscribeSessionEvent,
    TerminalSize,
};
pub use auth::oauth::{OAuthError, refresh_access_token, run_device_flow};
pub use auth::{AccessToken, AuthError, CredentialProvider};
pub use client::{
    AgentEventStream, Client, ClientError, ConnectError, HostEventStream, ResumeSummary,
    SessionStream, SuspendSummary,
};
pub use config::{Config, ConfigError, Keybinds, LeaderKey};
pub use debug::DebugFormat;
pub use paths::{default_data_dir, default_log_path};
pub use protocol::ProtocolError;
pub use routing::{Capabilities, Host, HostEvent, SupportedAgentType};
pub use server::{
    DaemonBuilder, EmbeddedBuilder, Server, ServerBuilder, ServerError, ShutdownReason,
};
pub use transport::TransportError;
pub use update::{UpdateInfo, UpdateReporter, UpdateStatus};

pub mod claude_io {
    pub use crate::agents::claude::io::{
        ClaudePtyTranscriptV1Action, ClaudePtyTranscriptV1Args, ClaudePtyTranscriptV1Input,
        ClaudePtyTranscriptV1Output, ClaudePtyTranscriptV1ReplayQuery, ClaudeRawV1Args,
        ClaudeRawV1Control, ClaudeRawV1ReplayQuery, PTY_TRANSCRIPT_V1, RAW_V1,
        decode_pty_transcript_v1_cursor, decode_pty_transcript_v1_output,
        encode_pty_transcript_v1_args, encode_pty_transcript_v1_input, encode_raw_v1_args,
        encode_raw_v1_control,
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

#[derive(Clone, Debug)]
pub struct SendInputRequest {
    pub agent: AgentIdentifier,
    pub io_protocol: String,
    pub payload: bytes::Bytes,
}

#[derive(Clone, Debug)]
pub struct SubscribeSessionRequest {
    pub agent: AgentIdentifier,
    pub io_protocol: String,
    pub args: Option<bytes::Bytes>,
}

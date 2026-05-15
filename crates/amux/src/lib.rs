pub mod agent;
mod auth;
mod buffer;
mod client;
mod config;
mod debug;
mod paths;
pub mod protocol;
mod rpc;
mod server;
mod services;
pub mod setup;
mod sleep_inhibitor;
mod state;
mod suspend;
mod transport;
pub mod update;

pub use auth::{AccessToken, AuthError, CredentialProvider};
pub use client::{
    AgentEventStream, Client, ClientError, ConnectError, ResumeSummary, RoutingEventStream,
    SessionStream, SuspendSummary,
};
pub use config::{Config, ConfigError, Keybinds, LeaderKey};
pub use paths::{default_data_dir, default_log_path};
pub use protocol::{
    Agent, AgentEntry, AgentType, CreateAgentRequest, DebugFormat, HookProvider, Host, Route,
    ShutdownReason, TerminalSize,
};
pub use server::{DaemonBuilder, EmbeddedBuilder, Server, ServerBuilder, ServerError};
pub use transport::TransportError;
pub use update::{UpdateInfo, UpdateReporter, UpdateStatus};

pub type AgentId = uuid::Uuid;
pub type HostId = uuid::Uuid;
pub type ConnectToServerTarget = String;

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
    pub id: AgentId,
    pub route: Route,
    pub io_protocol: String,
    pub payload: bytes::Bytes,
}

#[derive(Clone, Debug)]
pub struct SubscribeSessionRequest {
    pub id: AgentId,
    pub route: Route,
    pub io_protocol: String,
    pub args: Option<bytes::Bytes>,
}

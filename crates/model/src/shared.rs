use bytes::Bytes;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Agent, AgentKind, Protocol};

pub type AgentId = Uuid;
pub type HostId = Uuid;

/// Account-scoped data carried by a one-shot QR pairing code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QrPairingPayload {
    pub host_id: HostId,
    pub cloud_url: String,
    pub secret: Vec<u8>,
}

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
        Uuid::parse_str(value)
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
        Uuid::parse_str(value)
            .map(Self::Id)
            .unwrap_or_else(|_| Self::Name(value.to_string()))
    }
}

#[derive(Clone, Debug)]
pub struct SendInputRequest {
    pub agent: AgentIdentifier,
    pub input_id: Vec<u8>,
    pub io_protocol: String,
    pub payload: Bytes,
    pub pin: Vec<String>,
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
    pub args: Option<Bytes>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    SnapshotComplete,
    AgentUp { agent: Agent },
    AgentUpdated { agent: Agent },
    AgentDown { agent_id: Uuid },
}

impl AgentEvent {
    pub fn type_label(&self) -> &'static str {
        match self {
            Self::SnapshotComplete => "Agent::SnapshotComplete",
            Self::AgentUp { .. } => "Agent::AgentUp",
            Self::AgentUpdated { .. } => "Agent::AgentUpdated",
            Self::AgentDown { .. } => "Agent::AgentDown",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_agent_types: Vec<SupportedAgentType>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SupportedAgentType {
    pub agent_type: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub id: Uuid,
    pub name: String,
    pub version: String,
    pub capabilities: Capabilities,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostTrustStatus {
    Trusted,
    UntrustedButOnline,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    pub id: Uuid,
    pub name: String,
    pub online: bool,
    pub version: Option<String>,
    pub capabilities: Option<Capabilities>,
    pub trust_status: HostTrustStatus,
    pub last_dial_error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostEvent {
    SnapshotComplete,
    HostUpdated { host: HostEntry },
    HostRemoved { id: Uuid },
}

impl HostEvent {
    pub fn type_label(&self) -> &'static str {
        match self {
            Self::SnapshotComplete => "Host::SnapshotComplete",
            Self::HostUpdated { .. } => "Host::HostUpdated",
            Self::HostRemoved { .. } => "Host::HostRemoved",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactRef {
    pub id: crate::ArtifactId,
    pub kind: crate::ArtifactKind,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffBase {
    WorkingTree,
    Branch { base: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BaseIdentity {
    pub base: DiffBase,
    pub head: String,
    pub merge_base: Option<String>,
    pub blobs: Vec<(String, String)>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiffFile {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiffResponse {
    pub artifact: ArtifactRef,
    pub patch: String,
    pub identity: BaseIdentity,
    pub files: Vec<DiffFile>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ProtocolError {
    #[error("No agent found")]
    NoAgentFound,
    #[error("{message}")]
    Unimplemented { message: String },
    #[error("{message}")]
    Cancelled { message: String },
    #[error("{message}")]
    InvalidArgument { message: String },
    #[error("{kind} does not expose `{protocol}`")]
    NotExposed { kind: AgentKind, protocol: Protocol },
    #[error("{message}")]
    AlreadyExists { message: String },
    #[error("{message}")]
    PermissionDenied { message: String },
    #[error("{message}")]
    FailedPrecondition { message: String },
    #[error("{message}")]
    Unreachable { message: String },
    #[error("ambiguous agent name `{name}`")]
    AmbiguousAgentName { name: String, agent_ids: Vec<Uuid> },
    #[error("{message}")]
    ServerError { message: String },
    #[error("Invalid or missing credentials")]
    InvalidCredentials,
    #[error("Cloud subscription required")]
    PaymentRequired,
    #[error("{message}")]
    ResourceExhausted { message: String },
    #[error(
        "amux update required (supported protocol versions {supported_versions:?}, peer supports {peer_supported_versions:?})"
    )]
    ProtocolMismatch {
        supported_versions: Vec<u32>,
        peer_supported_versions: Vec<u32>,
    },
    #[error("amux update required (minimum v{minimum_version}, you have v{client_version})")]
    UpdateRequired {
        minimum_version: String,
        client_version: String,
    },
    #[error("sequence number mismatch (client {client_seq}, server {current_seq})")]
    SequenceNumberMismatch { client_seq: u64, current_seq: u64 },
    #[error("attachment `{id}` is missing")]
    AttachmentMissing { id: String },
    #[error("attachment is {size} bytes; maximum is {max} bytes")]
    AttachmentTooLarge { size: u64, max: u64 },
    #[error("artifact `{id}` is corrupt")]
    ArtifactCorrupt { id: String },
    #[error("{message}")]
    DiffUnavailable { message: String },
}

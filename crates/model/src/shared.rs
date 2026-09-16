use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Agent, AgentKind, Protocol};

pub type AgentId = Uuid;
pub type HostId = Uuid;

/// Output format requested for a node debug snapshot.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DebugFormat {
    #[default]
    Yaml,
    Json,
}

/// Reason attached to a node shutdown notification.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    UpdateRequired,
    ProtocolError,
    UserRequested,
    Updating,
    Suspending,
    Restarting,
    AuthExpired,
}

impl ShutdownReason {
    pub fn as_wire_value(self) -> &'static str {
        match self {
            Self::UpdateRequired => "update_required",
            Self::ProtocolError => "protocol_error",
            Self::UserRequested => "user_requested",
            Self::Updating => "updating",
            Self::Suspending => "suspending",
            Self::Restarting => "restarting",
            Self::AuthExpired => "auth_expired",
        }
    }

    pub fn from_wire_value(value: &str) -> Option<Self> {
        match value {
            "update_required" => Some(Self::UpdateRequired),
            "protocol_error" => Some(Self::ProtocolError),
            "user_requested" => Some(Self::UserRequested),
            "updating" => Some(Self::Updating),
            "suspending" => Some(Self::Suspending),
            "restarting" => Some(Self::Restarting),
            "auth_expired" => Some(Self::AuthExpired),
            _ => None,
        }
    }
}

impl std::fmt::Display for ShutdownReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UpdateRequired => "amux update required",
            Self::ProtocolError => "protocol error",
            Self::UserRequested => "server shutting down",
            Self::Updating => "server updating",
            Self::Suspending => "server suspending",
            Self::Restarting => "server restarting",
            Self::AuthExpired => "authentication expired",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshPairingPeer {
    pub host_id: HostId,
    pub pubkey: Vec<u8>,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshPairingProfile {
    pub identity: SshPairingPeer,
    pub profile: crate::ProfileId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshTarget {
    pub target: String,
    pub profile: crate::ProfileId,
}

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
    pub input: crate::SessionInput,
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
    pub args: crate::SessionArgs,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    SnapshotComplete,
    /// The complete inventory of one remote host as the daemon last saw it,
    /// attributed to the authenticated source host. Only the client service
    /// carries this; a host never asserts another host's inventory.
    HostInventory {
        host_id: Uuid,
        agent_ids: Vec<Uuid>,
    },
    AgentUp {
        agent: Agent,
    },
    AgentUpdated {
        agent: Agent,
    },
    AgentDown {
        agent_id: Uuid,
    },
}

impl AgentEvent {
    pub fn type_label(&self) -> &'static str {
        match self {
            Self::HostInventory { .. } => "Agent::HostInventory",
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
    /// What kind of machine this is, in its own words: the operating system
    /// the daemon was built for. A peer built before this field existed says
    /// nothing, which is why it is optional — a machine whose kind is unknown
    /// is not the same as one that claims to be nothing in particular.
    #[serde(default)]
    pub platform: Option<String>,
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
    /// The peer's operating system as it reported it. Older hosts report
    /// nothing, which is why it is optional: a machine whose kind is unknown
    /// is not the same as one that claims to be nothing in particular.
    #[serde(default)]
    pub platform: Option<String>,
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

/// SHA-256 of a device public key as lowercase hexadecimal: the form a person
/// compares across two screens when confirming a pairing.
pub fn public_key_fingerprint(pubkey: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(pubkey)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

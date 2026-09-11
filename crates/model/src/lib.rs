//! Transport-independent values shared by nodes, clients, runtimes, and UIs.
//!
//! This package owns data and validation only. It deliberately has no async
//! runtime, transport, filesystem owner, provider session, or process API.

mod agent;
mod agent_kind;
mod artifact;
mod session;
mod shared;

pub use agent::{
    AGENT_TYPE_CLAUDE, AGENT_TYPE_CODEX, AGENT_TYPE_TEST_AGENT, Agent, AgentParent, AgentType,
    CreateAgentRequest, HookEnvironment, RenameAgentRequest, SpawnInheritance, TerminalSize,
    WorkingOn,
};
pub use agent_kind::{AgentKind, ClaudeDriver, Protocol};
pub use artifact::{ArtifactId, ArtifactKind, InvalidArtifactId, id_of};
pub use session::{SessionCloseReason, SubscribeSessionEvent};
pub use shared::{
    AgentEvent, AgentId, AgentIdentifier, ArtifactRef, BaseIdentity, Capabilities, DiffBase,
    DiffFile, DiffResponse, Host, HostEntry, HostEvent, HostId, HostTrustStatus, PeerIdentifier,
    ProtocolError, QrPairingPayload, SendInputRequest, SendMessageRequest, SetAgentStatusRequest,
    SubscribeSessionRequest, SupportedAgentType,
};

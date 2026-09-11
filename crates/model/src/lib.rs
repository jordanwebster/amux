//! Transport-independent values shared by nodes, clients, runtimes, and UIs.
//!
//! This package owns data and validation only. It deliberately has no async
//! runtime, transport, filesystem owner, provider session, or process API.

mod agent;
mod agent_kind;
mod artifact;
pub mod envelope;
mod profile;
mod provider;
mod session;
mod shared;

pub use agent::{
    AGENT_TYPE_CLAUDE, AGENT_TYPE_CODEX, AGENT_TYPE_TEST_AGENT, Agent, AgentParent, AgentType,
    CreateAgentRequest, HookEnvironment, RenameAgentRequest, SpawnInheritance, TerminalSize,
    WorkingOn,
};
pub use agent_kind::{AgentKind, ClaudeDriver, Protocol};
pub use artifact::{ARTIFACT_SIZE_CAP, ArtifactId, ArtifactKind, InvalidArtifactId, id_of};
pub use profile::ProfileId;
pub use provider::{
    AGENT_TOOL_NAMES, AGENT_TOOL_SERVER_NAME, AskAnswer, CLAUDE_PTY_TRANSCRIPT_V1, CLAUDE_SDK_V1,
    CODEX_SDK_V1, ClaudePtyIntent, ClaudePtyTranscriptV1Args, ClaudePtyTranscriptV1Output,
    ClaudePtyTranscriptV1ReplayQuery, ClaudeSdkInput, ClaudeSdkV1Args, ClaudeSdkV1Output,
    ClaudeSdkV1ReplayQuery, CodexSdkInput, CodexSdkV1Args, CodexSdkV1Output, CodexSdkV1ReplayQuery,
    ContextMeter, ContextMeterSource, ContextUsage, ContextUsageCategory, McpServerFact,
    PermissionAnswer, PlanAnswer, QuestionAnswer, QuestionResponse,
};
pub use session::{SessionCloseReason, SubscribeSessionEvent};
pub use shared::{
    AgentEvent, AgentId, AgentIdentifier, ArtifactRef, BaseIdentity, Capabilities, DebugFormat,
    DiffBase, DiffFile, DiffResponse, Host, HostEntry, HostEvent, HostId, HostTrustStatus,
    PeerIdentifier, ProtocolError, QrPairingPayload, SendInputRequest, SendMessageRequest,
    SetAgentStatusRequest, ShutdownReason, SshPairingPeer, SshPairingProfile, SshTarget,
    SubscribeSessionRequest, SupportedAgentType,
};

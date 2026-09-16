//! Transport-independent values shared by nodes, clients, runtimes, and UIs.
//!
//! This package owns data and validation only. It deliberately has no async
//! runtime, transport, filesystem owner, provider session, or process API.

mod agent;
mod agent_kind;
mod artifact;
pub mod envelope;
mod observation;
mod profile;
mod provider;
mod relay;
mod repositories;
mod session;
mod shared;

pub use agent::{
    AGENT_TYPE_CLAUDE, AGENT_TYPE_CODEX, AGENT_TYPE_TEST_AGENT, Agent, AgentParent, AgentType,
    CreateAgentRequest, HookEnvironment, RenameAgentRequest, SpawnInheritance, TerminalSize,
    WorkingOn,
};
pub use agent_kind::{AgentKind, ClaudeDriver, Protocol};
pub use artifact::{ARTIFACT_SIZE_CAP, ArtifactId, ArtifactKind, InvalidArtifactId, id_of};
pub use observation::{
    AgentPhase, Attention, HostRevision, Progress, Seq, StructuredProtocol, Summary,
    SummaryEnvelope, SummaryField, TodoProgress, Why,
};
pub use profile::ProfileId;
pub use provider::{
    AGENT_TOOL_NAMES, AGENT_TOOL_SERVER_NAME, ApprovalPolicy, AskAnswer, CLAUDE_PTY_TRANSCRIPT_V1,
    CLAUDE_SDK_V1, CODEX_RAW_THREAD_NOT_READY, CODEX_SDK_V1, ClaudePtyIntent,
    ClaudePtyTranscriptV1Args, ClaudePtyTranscriptV1Input, ClaudeSdkInput, ClaudeSdkV1Args,
    CodexSdkInput, CodexSdkV1Args, ContextMeter, ContextMeterSource, ContextUsage,
    ContextUsageCategory, McpServerFact, ModelFact, PermissionAnswer, PlanAnswer, QuestionAnswer,
    QuestionResponse, SandboxPolicy, TERMINAL_V1, TerminalV1Args, TerminalV1ReplayQuery,
};
pub use relay::{DisconnectReason, RelayConnection};
pub use repositories::{ListRepositoriesRequest, ListRepositoriesResponse, ProjectEntry};
pub use session::{
    ReplayFacts, ReplayOutcome, ReplayQuery, SessionArgs, SessionCloseReason, SessionControl,
    SessionInput, SessionOutput, StructuredRow, SubscribeSessionEvent,
};
pub use shared::{
    AgentEvent, AgentId, AgentIdentifier, ArtifactRef, BaseIdentity, Capabilities, DebugFormat,
    DiffBase, DiffFile, DiffResponse, Host, HostEntry, HostEvent, HostId, HostTrustStatus,
    PeerIdentifier, ProtocolError, QrPairingPayload, SendInputRequest, SendMessageRequest,
    SetAgentStatusRequest, ShutdownReason, SshPairingPeer, SshPairingProfile, SshTarget,
    SubscribeSessionRequest, SupportedAgentType, public_key_fingerprint,
};

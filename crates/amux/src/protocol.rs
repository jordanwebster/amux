pub(crate) mod agent;
pub mod agent_lifecycle;
pub(crate) mod handshake;
pub(crate) mod link;
pub(crate) mod message;
pub(crate) mod method;
pub(crate) mod route;
pub mod session;
pub(crate) mod wire;

pub use agent::{Agent, AgentEntry};
pub use handshake::{Connect, ConnectResult, PROTOCOL_VERSION, RoutingRole};
pub use link::{InvalidLinkName, Link};
#[cfg(any(debug_assertions, test))]
pub use message::AGENT_TYPE_TEST_AGENT;
pub use message::{
    AGENT_TYPE_CLAUDE, AgentEvent, AgentType, CallId, Capabilities, CreateAgentRequest,
    DebugFormat, Frame, FrameBody, GoAway, HookProvider, Host, Message, ProtocolError,
    ReauthRequest, ReauthResponse, RenameAgentRequest, RequestFrame, ResponseFrame, RoutingEvent,
    SequencedReplayQuery, ShutdownReason, SupportedAgentType, TerminalSize,
};
pub use route::Route;

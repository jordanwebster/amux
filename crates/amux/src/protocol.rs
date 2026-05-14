pub(crate) mod agent;
pub mod agent_lifecycle;
pub(crate) mod handshake;
pub(crate) mod link;
pub(crate) mod message;
pub(crate) mod method;
pub mod open_session;
pub(crate) mod route;
pub(crate) mod wire;

pub use agent::Agent;
pub use handshake::{Connect, ConnectResult, PROTOCOL_VERSION, RoutingRole};
pub use link::{InvalidLinkName, Link};
#[cfg(any(debug_assertions, test))]
pub use message::AGENT_TYPE_TEST_AGENT;
pub use message::{
    AGENT_TYPE_CLAUDE, AgentEvent, AgentType, CallId, Capabilities, CreateAgentRequest,
    DebugFormat, FrameBody, GoAway, HookProvider, Host, LocalFrame, Message, PeerFrame,
    ProtocolError, ReauthRequest, ReauthResponse, RenameAgentRequest, RequestFrame, ResponseFrame,
    RoutedFrame, RoutedFrameMessage, RoutingEvent, SequencedReplayQuery, ShutdownReason,
    SupportedAgentType, TerminalSize,
};
pub use route::Route;

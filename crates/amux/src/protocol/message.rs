//! Protocol messages for all amux transports.
//!
//! Runtime protocol frame types. Shared payload types (errors, requests, enums)
//! live in `common`.

mod common;
mod envelope;
mod routing;

#[cfg(any(debug_assertions, test))]
pub use common::AGENT_TYPE_TEST_AGENT;
pub use common::{
    AGENT_TYPE_CLAUDE, AgentType, CallId, Capabilities, CreateAgentRequest, DebugFormat,
    HookProvider, Host, ProtocolError, RenameAgentRequest, SequencedReplayQuery, ShutdownReason,
    SupportedAgentType, TerminalSize,
};
pub use envelope::{
    FrameBody, GoAway, LocalFrame, Message, PeerFrame, ReauthRequest, ReauthResponse, RequestFrame,
    ResponseFrame, RoutedFrame, RoutedFrameMessage,
};
pub use routing::RoutingEvent;

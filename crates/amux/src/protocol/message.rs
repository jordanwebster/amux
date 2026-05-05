//! Protocol messages for all amux transports.
//!
//! Runtime protocol frame types. Shared payload types (errors, requests, enums)
//! live in `common`.

mod common;
mod envelope;
mod routing;

pub use common::{
    AgentType, CreateAgentRequest, DebugFormat, HookProvider, Host, ProtocolError,
    RenameAgentRequest, RoutedCallId, SequencedReplayQuery, ShutdownReason, TerminalSize,
};
pub use envelope::{
    FrameBody, GoAway, LocalFrame, Message, PeerFrame, ReauthRequest, ReauthResponse, RequestFrame,
    ResponseFrame, RoutedFrame, RoutedFrameMessage,
};
pub use routing::RoutingEvent;

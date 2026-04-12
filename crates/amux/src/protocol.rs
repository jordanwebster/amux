//! Curated protocol surface for clients.
//!
//! This module exports message and hook payload types without exposing internal
//! runtime modules such as transport, routing internals, or server internals.

pub use crate::claude::types::*;
pub use crate::message::{
    AgentProtocol, AgentType, ClaudeProtocol, Command, CreateAgentRequest, DebugFormat,
    DirectMessage, Host, Message, ProtocolError, RenameAgentRequest, RoutableMessage,
    ShutdownReason, SubscribeQuery, SubscriptionCloseReason, SubscriptionId, TerminalSize,
};
pub use crate::route::Route;

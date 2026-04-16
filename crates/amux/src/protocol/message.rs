//! Protocol messages for all amux transports.
//!
//! `Message` is the top-level envelope: routable (forwarded opaquely across hops),
//! direct (peer-to-peer), or command (CLI-only). `RoutableMessage`, `DirectMessage`,
//! and `Command` live in their own submodules; shared payload types (errors,
//! requests, enums) live in `common`.

mod command;
mod common;
mod direct;
mod envelope;
mod routable;

pub use command::Command;
pub use common::{
    AgentType, CreateAgentRequest, DebugFormat, HookProvider, Host, ProtocolError,
    RenameAgentRequest, ShutdownReason, SubscribeQuery, SubscriptionCloseReason, SubscriptionId,
    TerminalSize,
};
pub use direct::DirectMessage;
pub use envelope::Message;
pub use routable::RoutableMessage;

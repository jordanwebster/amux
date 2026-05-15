pub use amux::{
    Agent, AgentId, AgentIdentifier, AgentType, CreateAgentRequest, Host, HostId, TerminalSize,
};
// Internal — runtime uses these but they're not part of the public surface.
pub(crate) use amux::{AgentEntry, Route};

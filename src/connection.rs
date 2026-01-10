use crate::session::AgentId;

/// Unique identifier for a connection
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub struct ConnectionId(pub u64);

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "conn-{}", self.0)
    }
}

/// State of a local connection
#[derive(Debug)]
pub struct LocalConnectionState {
    /// Connection identifier
    pub id: ConnectionId,

    /// Agent this connection is subscribed to (if any)
    pub subscribed_agent: Option<AgentId>,

    /// Whether this connection is in raw mode (post-subscribe)
    pub raw_mode: bool,
}

impl LocalConnectionState {
    /// Create a new connection state
    pub fn new(id: ConnectionId) -> Self {
        Self {
            id,
            subscribed_agent: None,
            raw_mode: false,
        }
    }

    /// Subscribe to an agent
    pub fn subscribe(&mut self, agent_id: AgentId) {
        self.subscribed_agent = Some(agent_id);
    }

    /// Enter raw mode
    pub fn enter_raw_mode(&mut self) {
        self.raw_mode = true;
    }

    /// Unsubscribe from current agent
    pub fn unsubscribe(&mut self) {
        self.subscribed_agent = None;
        self.raw_mode = false;
    }
}

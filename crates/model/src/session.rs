#[derive(Debug, Clone, PartialEq)]
pub enum SubscribeSessionEvent {
    Opened,
    Output { payload: Vec<u8> },
    ReplayComplete { cursor: Option<Vec<u8>> },
    Closed { reason: SessionCloseReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCloseReason {
    AgentDeleted,
    AgentExited { exit_code: Option<i32> },
    HostUnreachable,
    InternalError { detail: String },
}

impl std::fmt::Display for SessionCloseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AgentDeleted => f.write_str("agent deleted"),
            Self::AgentExited {
                exit_code: Some(code),
            } => {
                write!(f, "agent exited with status {code}")
            }
            Self::AgentExited { exit_code: None } => f.write_str("agent exited"),
            Self::HostUnreachable => f.write_str("host unreachable"),
            Self::InternalError { detail } => write!(f, "internal error: {detail}"),
        }
    }
}

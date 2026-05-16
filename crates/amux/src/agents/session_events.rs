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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_output_event_decoder_accepts_closed_event() {
        assert_eq!(
            crate::agents::decode_session_output_event_payload(
                &crate::agents::encode_session_output_event_payload(
                    &SubscribeSessionEvent::Closed {
                        reason: SessionCloseReason::AgentDeleted,
                    }
                )
            )
            .unwrap(),
            SubscribeSessionEvent::Closed {
                reason: SessionCloseReason::AgentDeleted,
            }
        );
    }
}

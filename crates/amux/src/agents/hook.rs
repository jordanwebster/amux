use thiserror::Error;

use crate::protocol::ProtocolError;

pub(crate) enum ExternalHookBootstrap {
    Noop,
    Register(crate::agents::AgentSession),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookOutcome {
    Noop,
    KeepSession,
    WithdrawSession,
}

#[derive(Debug, Error)]
pub(crate) enum HookError {
    #[error("hooks are not supported for this agent type")]
    #[cfg(any(debug_assertions, test))]
    UnsupportedAgentType,
    #[error("invalid Claude hook payload: {message}")]
    InvalidPayload { message: String },
    #[error("external Claude hook missing required field '{field}'")]
    MissingBootstrapField { field: &'static str },
}

impl HookError {
    pub(crate) fn into_protocol_error(self) -> ProtocolError {
        ProtocolError::ServerError {
            message: self.to_string(),
        }
    }
}

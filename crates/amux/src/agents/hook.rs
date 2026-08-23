use std::collections::HashMap;

use thiserror::Error;

use crate::protocol::ProtocolError;

pub(crate) type HookEnvironment = HashMap<String, String>;

pub(crate) enum ExternalHookBootstrap {
    Noop,
    Register(crate::agents::AgentSession),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookOutcome {
    Noop,
    KeepSession,
    Completed { text: String },
    WithdrawSession,
}

#[derive(Debug, Error)]
pub(crate) enum HookError {
    #[error("hooks are not supported for this agent type")]
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

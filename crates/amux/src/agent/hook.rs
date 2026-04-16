use crate::protocol::message::{HookProvider, ProtocolError};
use thiserror::Error;

pub(crate) enum ExternalHookBootstrap {
    Noop,
    Register(crate::agent::AgentSession),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookOutcome {
    Noop,
    KeepSession,
    WithdrawSession,
}

#[derive(Debug, Error)]
pub(crate) enum HookError {
    #[error("hook provider mismatch: expected {expected:?}, got {actual:?}")]
    ProviderMismatch {
        expected: HookProvider,
        actual: HookProvider,
    },
    #[error("unsupported hook provider: {0:?}")]
    UnsupportedProvider(HookProvider),
    #[error("invalid {provider:?} hook payload: {message}")]
    InvalidPayload {
        provider: HookProvider,
        message: String,
    },
    #[error("external {provider:?} hook missing required field '{field}'")]
    MissingBootstrapField {
        provider: HookProvider,
        field: &'static str,
    },
}

impl HookError {
    pub(crate) fn into_protocol_error(self) -> ProtocolError {
        ProtocolError::ServerError {
            message: self.to_string(),
        }
    }
}

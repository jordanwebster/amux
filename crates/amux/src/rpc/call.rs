use uuid::Uuid;

use super::stream::RpcCallCancellation;
use crate::protocol::message::CallId;
use crate::protocol::method::MethodSpec;

/// Method-specific deduplication key for inbound calls.
///
/// Dedup is not a generic `(route, call_id)` property. Each method that wants
/// dedup defines the domain identity that makes a second active call duplicate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DedupKey {
    scope: &'static str,
    value: String,
}

impl DedupKey {
    pub(crate) fn new(scope: &'static str, value: impl Into<String>) -> Self {
        Self {
            scope,
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RegisterCallError {
    DuplicateCallId { call_id: CallId },
    DuplicateDedupKey { key: DedupKey, call_id: CallId },
}

#[derive(Debug, Clone)]
pub(crate) struct InboundCall {
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
    pub(crate) generation: Uuid,
    pub(crate) state: InboundCallState,
    pub(crate) dedup_key: Option<DedupKey>,
    pub(in crate::rpc) cancellation: RpcCallCancellation,
}

#[derive(Debug, Clone)]
pub(crate) struct RpcInboundCallHandle {
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
    pub(crate) generation: Uuid,
}

#[derive(Debug, Clone)]
pub(crate) struct RpcOutboundCallHandle {
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
}

#[derive(Debug)]
pub(crate) struct RpcInboundUnary {
    pub(crate) handle: RpcInboundCallHandle,
}

#[derive(Debug)]
pub(crate) struct RpcInboundServerStream {
    pub(crate) handle: RpcInboundCallHandle,
    pub(crate) cancellation: RpcCallCancellation,
}

#[derive(Debug, Clone)]
pub(crate) enum RpcInboundCallTarget {
    ActiveNoInput {
        method: MethodSpec,
    },
    NotAccepting {
        method: MethodSpec,
        state: InboundCallState,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct RpcInboundClosing {
    pub(crate) handle: RpcInboundCallHandle,
}

pub(crate) struct RpcInboundStart {
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
    pub(crate) dedup_key: Option<DedupKey>,
}

pub(crate) struct RpcOutboundStart {
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
    pub(crate) state: OutboundCallState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InboundCallState {
    Starting,
    Active,
    Closing,
}

impl InboundCallState {
    pub(in crate::rpc) fn as_str(self) -> &'static str {
        match self {
            InboundCallState::Starting => "starting",
            InboundCallState::Active => "active",
            InboundCallState::Closing => "closing",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OutboundCall {
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
    pub(crate) state: OutboundCallState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboundCallState {
    AwaitingResponse,
    ActiveStream,
    Closing,
}

impl OutboundCallState {
    pub(in crate::rpc) fn as_str(self) -> &'static str {
        match self {
            OutboundCallState::AwaitingResponse => "awaiting_response",
            OutboundCallState::ActiveStream => "active_stream",
            OutboundCallState::Closing => "closing",
        }
    }
}

use tokio::sync::mpsc;
use uuid::Uuid;

use super::stream::{
    RpcCallCancellation, RpcPeerStreamSink, RpcRoutedSink, RpcStreamReader, RpcStreamWriter,
    RpcTypedRoutedSink, RpcTypedStreamReader,
};
use crate::protocol::Route;
use crate::protocol::link::Link;
use crate::protocol::message::{CallId, Message};
use crate::protocol::method::MethodSpec;

/// Method-specific deduplication key for inbound calls.
///
/// Dedup is not a generic `(route, call_id)` property. Each method that wants
/// dedup defines the domain identity that makes a second active call duplicate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum DedupKey {
    OpenSession {
        counterparty_route: Route,
        agent_id: Uuid,
    },
    PeerRoutingSubscription {
        link: Link,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct InboundCall {
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
    pub(crate) generation: Uuid,
    pub(crate) state: InboundCallState,
    pub(crate) dedup_key: Option<DedupKey>,
    pub(crate) stream_writer: Option<RpcStreamWriter>,
    pub(crate) resources: Option<InboundCallResources>,
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
pub(crate) struct RpcInboundBidi {
    pub(crate) handle: RpcInboundCallHandle,
    pub(crate) input: RpcStreamReader,
    pub(crate) output: RpcRoutedSink,
    pub(crate) cancellation: RpcCallCancellation,
}

#[derive(Debug)]
pub(crate) struct RpcInboundUnary {
    pub(crate) handle: RpcInboundCallHandle,
}

#[derive(Debug)]
pub(crate) struct RpcInboundServerStream {
    pub(crate) handle: RpcInboundCallHandle,
    pub(crate) output: RpcPeerStreamSink,
}

#[derive(Debug, Clone)]
pub(crate) enum RpcInboundFrameTarget {
    ActiveStream {
        method: MethodSpec,
        stream_writer: RpcStreamWriter,
    },
    ActiveNoInput {
        method: MethodSpec,
    },
    NotAccepting {
        state: InboundCallState,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct RpcInboundClosing {
    pub(crate) handle: RpcInboundCallHandle,
    pub(in crate::rpc) output: RpcRoutedSink,
}

#[derive(Debug)]
pub(crate) struct RpcTypedInboundBidi<I, O> {
    pub(crate) handle: RpcInboundCallHandle,
    pub(crate) input: RpcTypedStreamReader<I>,
    pub(crate) output: RpcTypedRoutedSink<O>,
    pub(crate) cancellation: RpcCallCancellation,
}

pub(crate) struct RpcRoutedBidiStart {
    pub(crate) tx: mpsc::Sender<Message>,
    pub(crate) owner_link: Link,
    pub(crate) reply_src: Route,
    pub(crate) reply_dst: Route,
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
    pub(crate) dedup_key: Option<DedupKey>,
    pub(crate) stream_capacity: usize,
}

pub(crate) struct RpcRoutedUnaryStart {
    pub(crate) tx: mpsc::Sender<Message>,
    pub(crate) owner_link: Link,
    pub(crate) reply_src: Route,
    pub(crate) reply_dst: Route,
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
}

pub(crate) struct RpcServerStreamStart {
    pub(crate) tx: mpsc::Sender<Message>,
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
    pub(crate) dedup_key: Option<DedupKey>,
}

pub(crate) struct RpcClientOutboundStart {
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
    pub(crate) state: OutboundCallState,
    pub(crate) inbox_tx: mpsc::Sender<Message>,
}

pub(crate) struct RpcLocalOriginOutboundStart {
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
    pub(crate) state: OutboundCallState,
    pub(crate) owner_link: Link,
    pub(crate) request_src: Route,
    pub(crate) request_dst: Route,
}

pub(crate) struct RpcPeerStreamOutboundStart {
    pub(crate) link: Link,
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
}

#[derive(Debug, Clone)]
pub(crate) struct InboundCallResources {
    pub(crate) owner_link: Link,
    pub(crate) output: RpcRoutedSink,
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
    pub(crate) resources: Option<OutboundCallResources>,
}

#[derive(Debug, Clone)]
pub(crate) enum OutboundCallResources {
    LocalOriginRouted {
        owner_link: Link,
        request_src: Route,
        request_dst: Route,
    },
    ClientInbox {
        tx: mpsc::Sender<Message>,
    },
    PeerRoutingSubscription {
        link: Link,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct RpcLocalOriginOutboundCall {
    pub(crate) call_id: CallId,
    pub(crate) owner_link: Link,
    pub(crate) request_src: Route,
    pub(crate) request_dst: Route,
}

impl OutboundCallResources {
    pub(crate) fn local_origin(&self) -> Option<(&Link, &Route, &Route)> {
        match self {
            Self::LocalOriginRouted {
                owner_link,
                request_src,
                request_dst,
            } => Some((owner_link, request_src, request_dst)),
            Self::ClientInbox { .. } | Self::PeerRoutingSubscription { .. } => None,
        }
    }

    pub(crate) fn into_local_origin(self) -> Option<(Link, Route, Route)> {
        match self {
            Self::LocalOriginRouted {
                owner_link,
                request_src,
                request_dst,
            } => Some((owner_link, request_src, request_dst)),
            Self::ClientInbox { .. } | Self::PeerRoutingSubscription { .. } => None,
        }
    }
}

impl OutboundCall {
    pub(in crate::rpc) fn into_local_origin(self) -> Option<RpcLocalOriginOutboundCall> {
        let (owner_link, request_src, request_dst) = self.resources?.into_local_origin()?;
        Some(RpcLocalOriginOutboundCall {
            call_id: self.call_id,
            owner_link,
            request_src,
            request_dst,
        })
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RegisterCallError {
    DuplicateCallId { call_id: CallId },
    DuplicateDedupKey { key: DedupKey, call_id: CallId },
}

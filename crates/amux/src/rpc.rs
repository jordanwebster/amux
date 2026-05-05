use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tokio::sync::{Mutex, mpsc, watch};
use uuid::Uuid;

use crate::protocol::link::Link;
use crate::protocol::message::{
    FrameBody, Message, PeerFrame, ProtocolError, ResponseFrame, RoutedFrame, RoutedFrameMessage,
};
use crate::protocol::method::{MethodKind, MethodSpec};
use crate::protocol::{Route, RoutedCallId, wire};

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
        counterparty_route: Route,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct InboundCall {
    pub(crate) call_id: RoutedCallId,
    pub(crate) counterparty_route: Route,
    pub(crate) method: MethodSpec,
    pub(crate) generation: Uuid,
    pub(crate) state: InboundCallState,
    pub(crate) dedup_key: Option<DedupKey>,
    pub(crate) stream_writer: Option<RpcStreamWriter>,
    pub(crate) resources: Option<InboundCallResources>,
    cancellation: RpcCallCancellation,
}

#[derive(Debug, Clone)]
pub(crate) struct RpcStreamWriter {
    tx: mpsc::Sender<FrameBody>,
}

#[derive(Debug)]
pub(crate) struct RpcStreamReader {
    rx: mpsc::Receiver<FrameBody>,
}

#[derive(Clone)]
pub(crate) struct RpcCallCancellation {
    inner: Arc<RpcCallCancellationInner>,
}

struct RpcCallCancellationInner {
    cancelled: AtomicBool,
    tx: watch::Sender<bool>,
}

impl RpcCallCancellation {
    fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self {
            inner: Arc::new(RpcCallCancellationInner {
                cancelled: AtomicBool::new(false),
                tx,
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::new()
    }

    fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::SeqCst) {
            let _ = self.inner.tx.send(true);
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    pub(crate) async fn cancelled(&self) {
        let mut rx = self.inner.tx.subscribe();
        if self.is_cancelled() {
            return;
        }
        while rx.changed().await.is_ok() {
            if self.is_cancelled() {
                return;
            }
        }
    }
}

impl fmt::Debug for RpcCallCancellation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcCallCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

pub(crate) trait RpcStreamCodec {
    type Item;

    fn decode_frame(frame: FrameBody) -> Result<Self::Item, ProtocolError>;
}

pub(crate) trait RpcStreamEncoder {
    type Item;

    fn encode_item(item: &Self::Item) -> Vec<u8>;
}

#[derive(Debug)]
pub(crate) struct RpcTypedStreamReader<C> {
    inner: RpcStreamReader,
    _codec: PhantomData<C>,
}

#[derive(Debug, Clone)]
pub(crate) struct RpcRoutedSink {
    tx: mpsc::Sender<Message>,
    src: Route,
    dst: Route,
    call_id: RoutedCallId,
    send_gate: Arc<Mutex<()>>,
}

#[derive(Debug)]
pub(crate) struct RpcTypedRoutedSink<C> {
    inner: RpcRoutedSink,
    _codec: PhantomData<C>,
}

#[derive(Debug, Clone)]
pub(crate) struct RpcInboundCallHandle {
    pub(crate) counterparty_route: Route,
    pub(crate) call_id: RoutedCallId,
    pub(crate) method: MethodSpec,
    pub(crate) generation: Uuid,
}

#[derive(Debug, Clone)]
pub(crate) struct RpcOutboundCallHandle {
    pub(crate) counterparty_route: Route,
    pub(crate) call_id: RoutedCallId,
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
    output: RpcRoutedSink,
}

#[derive(Debug)]
pub(crate) struct RpcTypedInboundBidi<I, O> {
    pub(crate) handle: RpcInboundCallHandle,
    pub(crate) input: RpcTypedStreamReader<I>,
    pub(crate) output: RpcTypedRoutedSink<O>,
    pub(crate) cancellation: RpcCallCancellation,
}

#[derive(Debug, Clone)]
pub(crate) struct RpcPeerStreamSink {
    tx: mpsc::Sender<Message>,
    call_id: RoutedCallId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RpcPeerSnapshotSendError {
    Full,
    Closed,
}

pub(crate) struct RpcRoutedBidiStart {
    pub(crate) tx: mpsc::Sender<Message>,
    pub(crate) owner_link: Link,
    pub(crate) reply_src: Route,
    pub(crate) reply_dst: Route,
    pub(crate) counterparty_route: Route,
    pub(crate) call_id: RoutedCallId,
    pub(crate) method: MethodSpec,
    pub(crate) dedup_key: Option<DedupKey>,
    pub(crate) stream_capacity: usize,
}

pub(crate) struct RpcRoutedUnaryStart {
    pub(crate) tx: mpsc::Sender<Message>,
    pub(crate) owner_link: Link,
    pub(crate) reply_src: Route,
    pub(crate) reply_dst: Route,
    pub(crate) counterparty_route: Route,
    pub(crate) call_id: RoutedCallId,
    pub(crate) method: MethodSpec,
}

pub(crate) struct RpcServerStreamStart {
    pub(crate) tx: mpsc::Sender<Message>,
    pub(crate) counterparty_route: Route,
    pub(crate) call_id: RoutedCallId,
    pub(crate) method: MethodSpec,
    pub(crate) dedup_key: Option<DedupKey>,
}

pub(crate) struct RpcClientOutboundStart {
    pub(crate) call_id: RoutedCallId,
    pub(crate) counterparty_route: Route,
    pub(crate) method: MethodSpec,
    pub(crate) state: OutboundCallState,
    pub(crate) inbox_tx: mpsc::Sender<Message>,
}

pub(crate) struct RpcLocalOriginOutboundStart {
    pub(crate) call_id: RoutedCallId,
    pub(crate) counterparty_route: Route,
    pub(crate) method: MethodSpec,
    pub(crate) state: OutboundCallState,
    pub(crate) owner_link: Link,
    pub(crate) request_src: Route,
    pub(crate) request_dst: Route,
}

pub(crate) struct RpcPeerStreamOutboundStart {
    pub(crate) counterparty_route: Route,
    pub(crate) call_id: RoutedCallId,
    pub(crate) method: MethodSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RpcStreamClosed;

#[derive(Debug)]
pub(crate) enum RpcRoutedSendError {
    Encode(wire::EncodeError),
    Closed,
}

impl RpcStreamWriter {
    pub(crate) fn channel(capacity: usize) -> (Self, RpcStreamReader) {
        let (tx, rx) = mpsc::channel(capacity);
        (Self { tx }, RpcStreamReader { rx })
    }

    pub(crate) async fn send_frame_body(&self, frame: FrameBody) -> Result<(), RpcStreamClosed> {
        self.tx.send(frame).await.map_err(|_| RpcStreamClosed)
    }
}

impl RpcStreamReader {
    async fn recv_frame(&mut self) -> Option<FrameBody> {
        self.rx.recv().await
    }

    pub(crate) fn decode_with<C: RpcStreamCodec>(self) -> RpcTypedStreamReader<C> {
        RpcTypedStreamReader {
            inner: self,
            _codec: PhantomData,
        }
    }
}

impl RpcInboundBidi {
    pub(crate) fn into_typed<I, O>(self) -> RpcTypedInboundBidi<I, O>
    where
        I: RpcStreamCodec,
        O: RpcStreamEncoder,
    {
        RpcTypedInboundBidi {
            handle: self.handle,
            input: self.input.decode_with::<I>(),
            output: self.output.encode_with::<O>(),
            cancellation: self.cancellation,
        }
    }
}

impl RpcInboundClosing {
    pub(crate) async fn send_response(
        &self,
        response: ResponseFrame,
    ) -> Result<(), RpcRoutedSendError> {
        self.output.send_response(response).await
    }

    pub(crate) async fn send_empty_response_result(
        &self,
        result: Result<(), ProtocolError>,
    ) -> Result<(), RpcRoutedSendError> {
        self.output.send_empty_response_result(result).await
    }

    pub(crate) async fn with_send_gate<F, Fut, T>(&self, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        self.output.with_send_gate(f).await
    }
}

impl RpcPeerStreamSink {
    fn stream_item_message(&self, payload: Vec<u8>) -> Message {
        Message::Peer(PeerFrame {
            call_id: self.call_id.clone(),
            body: FrameBody::StreamItem(payload),
        })
    }

    pub(crate) fn try_send_snapshot(
        &self,
        payloads: impl IntoIterator<Item = Vec<u8>>,
    ) -> Result<(), RpcPeerSnapshotSendError> {
        let messages = payloads
            .into_iter()
            .map(|payload| self.stream_item_message(payload))
            .collect();
        self.tx
            .try_send(Message::PeerSnapshot { messages })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RpcPeerSnapshotSendError::Full,
                mpsc::error::TrySendError::Closed(_) => RpcPeerSnapshotSendError::Closed,
            })
    }
}

impl<C: RpcStreamCodec> RpcTypedStreamReader<C> {
    pub(crate) async fn recv(&mut self) -> Option<Result<C::Item, ProtocolError>> {
        self.inner.recv_frame().await.map(C::decode_frame)
    }
}

impl RpcRoutedSink {
    pub(crate) fn new(
        tx: mpsc::Sender<Message>,
        src: Route,
        dst: Route,
        call_id: RoutedCallId,
        send_gate: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            tx,
            src,
            dst,
            call_id,
            send_gate,
        }
    }

    pub(crate) fn encode_with<C: RpcStreamEncoder>(self) -> RpcTypedRoutedSink<C> {
        RpcTypedRoutedSink {
            inner: self,
            _codec: PhantomData,
        }
    }

    pub(crate) async fn with_send_gate<F, Fut, T>(&self, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let _guard = self.send_gate.lock().await;
        f().await
    }

    pub(crate) async fn send_stream_item_if_current<F, Fut>(
        &self,
        payload: Vec<u8>,
        is_current: F,
    ) -> Result<bool, RpcRoutedSendError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = bool>,
    {
        let _guard = self.send_gate.lock().await;
        if !is_current().await {
            return Ok(false);
        }
        self.send_frame_body_unlocked(FrameBody::StreamItem(payload))
            .await?;
        Ok(true)
    }

    pub(crate) async fn send_response(
        &self,
        response: ResponseFrame,
    ) -> Result<(), RpcRoutedSendError> {
        self.send_frame_body(FrameBody::Response(response)).await
    }

    pub(crate) async fn send_empty_response_result(
        &self,
        result: Result<(), ProtocolError>,
    ) -> Result<(), RpcRoutedSendError> {
        let response = match result {
            Ok(()) => ResponseFrame::Payload(Vec::new()),
            Err(error) => ResponseFrame::Error(error),
        };
        self.send_response(response).await
    }

    async fn send_frame_body(&self, body: FrameBody) -> Result<(), RpcRoutedSendError> {
        let _guard = self.send_gate.lock().await;
        self.send_frame_body_unlocked(body).await
    }

    async fn send_frame_body_unlocked(&self, body: FrameBody) -> Result<(), RpcRoutedSendError> {
        let payload = wire::encode_frame_body(&body).map_err(RpcRoutedSendError::Encode)?;
        self.tx
            .send(Message::Routed(RoutedFrame {
                src: self.src.clone(),
                dst: self.dst.clone(),
                call_id: self.call_id.clone(),
                message: RoutedFrameMessage::Payload(payload),
            }))
            .await
            .map_err(|_| RpcRoutedSendError::Closed)
    }
}

impl<C> Clone for RpcTypedRoutedSink<C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _codec: PhantomData,
        }
    }
}

impl<C: RpcStreamEncoder> RpcTypedRoutedSink<C> {
    pub(crate) async fn send_item_if_current<F, Fut>(
        &self,
        item: C::Item,
        is_current: F,
    ) -> Result<bool, RpcRoutedSendError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = bool>,
    {
        self.inner
            .send_stream_item_if_current(C::encode_item(&item), is_current)
            .await
    }
}

impl fmt::Display for RpcRoutedSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => write!(f, "{error}"),
            Self::Closed => write!(f, "routed RPC output channel is closed"),
        }
    }
}

impl std::error::Error for RpcRoutedSendError {}

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
    fn as_str(self) -> &'static str {
        match self {
            InboundCallState::Starting => "starting",
            InboundCallState::Active => "active",
            InboundCallState::Closing => "closing",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OutboundCall {
    pub(crate) call_id: RoutedCallId,
    pub(crate) counterparty_route: Route,
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
}

#[derive(Debug, Clone)]
pub(crate) struct RpcLocalOriginOutboundCall {
    pub(crate) call_id: RoutedCallId,
    pub(crate) counterparty_route: Route,
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
            Self::ClientInbox { .. } => None,
        }
    }

    pub(crate) fn into_local_origin(self) -> Option<(Link, Route, Route)> {
        match self {
            Self::LocalOriginRouted {
                owner_link,
                request_src,
                request_dst,
            } => Some((owner_link, request_src, request_dst)),
            Self::ClientInbox { .. } => None,
        }
    }
}

impl OutboundCall {
    fn into_local_origin(self) -> Option<RpcLocalOriginOutboundCall> {
        let (owner_link, request_src, request_dst) = self.resources?.into_local_origin()?;
        Some(RpcLocalOriginOutboundCall {
            call_id: self.call_id,
            counterparty_route: self.counterparty_route,
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
    fn as_str(self) -> &'static str {
        match self {
            OutboundCallState::AwaitingResponse => "awaiting_response",
            OutboundCallState::ActiveStream => "active_stream",
            OutboundCallState::Closing => "closing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RegisterCallError {
    DuplicateCallId {
        counterparty_route: Route,
        call_id: RoutedCallId,
    },
    DuplicateDedupKey {
        key: DedupKey,
        counterparty_route: Route,
        call_id: RoutedCallId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RpcCallKey {
    counterparty_route: Route,
    call_id: RoutedCallId,
}

impl RpcCallKey {
    fn new(counterparty_route: Route, call_id: RoutedCallId) -> Self {
        Self {
            counterparty_route,
            call_id,
        }
    }
}

fn inbound_call_matches_handle(call: &InboundCall, handle: &RpcInboundCallHandle) -> bool {
    call.counterparty_route == handle.counterparty_route
        && call.call_id == handle.call_id
        && call.method == handle.method
        && call.generation == handle.generation
}

fn outbound_call_matches_handle(call: &OutboundCall, handle: &RpcOutboundCallHandle) -> bool {
    call.counterparty_route == handle.counterparty_route
        && call.call_id == handle.call_id
        && call.method == handle.method
}

fn client_inbox(call: &OutboundCall) -> Option<mpsc::Sender<Message>> {
    match &call.resources {
        Some(OutboundCallResources::ClientInbox { tx }) => Some(tx.clone()),
        Some(OutboundCallResources::LocalOriginRouted { .. }) | None => None,
    }
}

#[derive(Debug, Default)]
pub(crate) struct RpcState {
    inbound_calls: HashMap<RpcCallKey, InboundCall>,
    outbound_calls: HashMap<RpcCallKey, OutboundCall>,
    inbound_dedup_index: HashMap<DedupKey, RpcCallKey>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct RpcDebugSnapshot {
    inbound_calls: RpcCallDebugSnapshot,
    outbound_calls: RpcCallDebugSnapshot,
    inbound_dedup_keys: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct RpcCallDebugSnapshot {
    total: usize,
    by_state: BTreeMap<&'static str, usize>,
    by_method: BTreeMap<&'static str, usize>,
    by_counterparty: BTreeMap<String, usize>,
}

impl RpcState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn inbound_len(&self) -> usize {
        self.inbound_calls.len()
    }

    pub(crate) fn outbound_len(&self) -> usize {
        self.outbound_calls.len()
    }

    pub(crate) fn dedup_len(&self) -> usize {
        self.inbound_dedup_index.len()
    }

    pub(crate) fn debug_snapshot(&self) -> RpcDebugSnapshot {
        let mut inbound = RpcCallDebugSnapshot::new([
            InboundCallState::Starting.as_str(),
            InboundCallState::Active.as_str(),
            InboundCallState::Closing.as_str(),
        ]);
        for call in self.inbound_calls.values() {
            inbound.record(
                call.state.as_str(),
                call.method.name,
                call.counterparty_route.to_string(),
            );
        }

        let mut outbound = RpcCallDebugSnapshot::new([
            OutboundCallState::AwaitingResponse.as_str(),
            OutboundCallState::ActiveStream.as_str(),
            OutboundCallState::Closing.as_str(),
        ]);
        for (key, call) in &self.outbound_calls {
            debug_assert_eq!(key.call_id, call.call_id);
            debug_assert_eq!(key.counterparty_route, call.counterparty_route);
            outbound.record(
                call.state.as_str(),
                call.method.name,
                call.counterparty_route.to_string(),
            );
        }

        RpcDebugSnapshot {
            inbound_calls: inbound,
            outbound_calls: outbound,
            inbound_dedup_keys: self.dedup_len(),
        }
    }

    pub(crate) fn register_routed_bidi(
        &mut self,
        start: RpcRoutedBidiStart,
    ) -> Result<RpcInboundBidi, RegisterCallError> {
        let (stream_writer, stream_reader) = RpcStreamWriter::channel(start.stream_capacity);
        let generation = Uuid::new_v4();
        let send_gate = Arc::new(Mutex::new(()));
        let cancellation = RpcCallCancellation::new();
        let output = RpcRoutedSink::new(
            start.tx,
            start.reply_src,
            start.reply_dst,
            start.call_id.clone(),
            send_gate,
        );
        let resources = InboundCallResources {
            owner_link: start.owner_link,
            output: output.clone(),
        };
        let handle = RpcInboundCallHandle {
            counterparty_route: start.counterparty_route.clone(),
            call_id: start.call_id.clone(),
            method: start.method,
            generation,
        };
        self.register_inbound(InboundCall {
            call_id: start.call_id.clone(),
            counterparty_route: start.counterparty_route,
            method: start.method,
            generation,
            state: InboundCallState::Active,
            dedup_key: start.dedup_key,
            stream_writer: Some(stream_writer),
            resources: Some(resources),
            cancellation: cancellation.clone(),
        })?;

        Ok(RpcInboundBidi {
            handle,
            input: stream_reader,
            output,
            cancellation,
        })
    }

    pub(crate) fn register_routed_unary(
        &mut self,
        start: RpcRoutedUnaryStart,
    ) -> Result<RpcInboundUnary, RegisterCallError> {
        let generation = Uuid::new_v4();
        let send_gate = Arc::new(Mutex::new(()));
        let cancellation = RpcCallCancellation::new();
        let output = RpcRoutedSink::new(
            start.tx,
            start.reply_src,
            start.reply_dst,
            start.call_id.clone(),
            send_gate,
        );
        let handle = RpcInboundCallHandle {
            counterparty_route: start.counterparty_route.clone(),
            call_id: start.call_id.clone(),
            method: start.method,
            generation,
        };
        self.register_inbound(InboundCall {
            call_id: start.call_id,
            counterparty_route: start.counterparty_route,
            method: start.method,
            generation,
            state: InboundCallState::Active,
            dedup_key: None,
            stream_writer: None,
            resources: Some(InboundCallResources {
                owner_link: start.owner_link,
                output,
            }),
            cancellation,
        })?;

        Ok(RpcInboundUnary { handle })
    }

    pub(crate) fn register_server_stream(
        &mut self,
        start: RpcServerStreamStart,
    ) -> Result<RpcInboundServerStream, RegisterCallError> {
        debug_assert_eq!(start.method.kind, MethodKind::ServerStreaming);
        let generation = Uuid::new_v4();
        let cancellation = RpcCallCancellation::new();
        let output = RpcPeerStreamSink {
            tx: start.tx,
            call_id: start.call_id.clone(),
        };
        let handle = RpcInboundCallHandle {
            counterparty_route: start.counterparty_route.clone(),
            call_id: start.call_id.clone(),
            method: start.method,
            generation,
        };
        self.register_inbound(InboundCall {
            call_id: start.call_id,
            counterparty_route: start.counterparty_route,
            method: start.method,
            generation,
            state: InboundCallState::Starting,
            dedup_key: start.dedup_key,
            stream_writer: None,
            resources: None,
            cancellation: cancellation.clone(),
        })?;

        Ok(RpcInboundServerStream { handle, output })
    }

    fn register_inbound(&mut self, call: InboundCall) -> Result<(), RegisterCallError> {
        let call_key = RpcCallKey::new(call.counterparty_route.clone(), call.call_id.clone());
        if self.inbound_calls.contains_key(&call_key) {
            return Err(RegisterCallError::DuplicateCallId {
                counterparty_route: call_key.counterparty_route,
                call_id: call_key.call_id,
            });
        }

        if let Some(key) = &call.dedup_key {
            if let Some(existing_call_key) = self.inbound_dedup_index.get(key)
                && self.inbound_calls.contains_key(existing_call_key)
            {
                return Err(RegisterCallError::DuplicateDedupKey {
                    key: key.clone(),
                    counterparty_route: existing_call_key.counterparty_route.clone(),
                    call_id: existing_call_key.call_id.clone(),
                });
            }
            self.inbound_dedup_index
                .insert(key.clone(), call_key.clone());
        }

        self.inbound_calls.insert(call_key, call);
        Ok(())
    }

    pub(crate) fn inbound_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<&InboundCall> {
        self.inbound_calls.get(&RpcCallKey::new(
            counterparty_route.clone(),
            call_id.clone(),
        ))
    }

    pub(crate) fn inbound_resources_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<InboundCallResources> {
        self.inbound_for_route(counterparty_route, call_id)
            .and_then(|call| call.resources.clone())
    }

    pub(crate) fn inbound_for_handle(&self, handle: &RpcInboundCallHandle) -> Option<&InboundCall> {
        self.inbound_for_route(&handle.counterparty_route, &handle.call_id)
            .filter(|call| inbound_call_matches_handle(call, handle))
    }

    pub(crate) fn inbound_call_is_active_for_handle(&self, handle: &RpcInboundCallHandle) -> bool {
        self.inbound_for_handle(handle)
            .is_some_and(|call| matches!(call.state, InboundCallState::Active))
    }

    pub(crate) fn activate_inbound_for_handle(&mut self, handle: &RpcInboundCallHandle) -> bool {
        let Some(call) = self.inbound_calls.get_mut(&RpcCallKey::new(
            handle.counterparty_route.clone(),
            handle.call_id.clone(),
        )) else {
            return false;
        };
        if !inbound_call_matches_handle(call, handle)
            || !matches!(call.state, InboundCallState::Starting)
        {
            return false;
        }
        call.state = InboundCallState::Active;
        true
    }

    pub(crate) fn inbound_call_keys_if(
        &self,
        mut predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Vec<(Route, RoutedCallId)> {
        self.inbound_calls
            .values()
            .filter(|call| predicate(call))
            .map(|call| (call.counterparty_route.clone(), call.call_id.clone()))
            .collect()
    }

    pub(crate) fn active_inbound_call_id_for_route_and_method(
        &self,
        counterparty_route: &Route,
        method: MethodSpec,
    ) -> Option<RoutedCallId> {
        self.inbound_calls.values().find_map(|call| {
            (call.counterparty_route == *counterparty_route
                && call.method == method
                && matches!(call.state, InboundCallState::Active))
            .then(|| call.call_id.clone())
        })
    }

    pub(crate) fn inbound_frame_target_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<RpcInboundFrameTarget> {
        self.inbound_for_route(counterparty_route, call_id)
            .map(|call| match call.state {
                InboundCallState::Active => match &call.stream_writer {
                    Some(stream_writer) => RpcInboundFrameTarget::ActiveStream {
                        method: call.method,
                        stream_writer: stream_writer.clone(),
                    },
                    None => RpcInboundFrameTarget::ActiveNoInput {
                        method: call.method,
                    },
                },
                InboundCallState::Starting | InboundCallState::Closing => {
                    RpcInboundFrameTarget::NotAccepting { state: call.state }
                }
            })
    }

    pub(crate) fn begin_inbound_closing_for_route_if(
        &mut self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        predicate: impl FnOnce(&InboundCall, &InboundCallResources) -> bool,
    ) -> Option<RpcInboundClosing> {
        let call_key = RpcCallKey::new(counterparty_route.clone(), call_id.clone());
        let call = self.inbound_calls.get_mut(&call_key)?;
        let resources = call.resources.clone()?;
        if matches!(call.state, InboundCallState::Closing) || !predicate(call, &resources) {
            return None;
        }
        call.state = InboundCallState::Closing;
        call.cancellation.cancel();
        Some(RpcInboundClosing {
            handle: RpcInboundCallHandle {
                counterparty_route: call.counterparty_route.clone(),
                call_id: call.call_id.clone(),
                method: call.method,
                generation: call.generation,
            },
            output: resources.output,
        })
    }

    pub(crate) fn begin_inbound_closing_for_handle_if(
        &mut self,
        handle: &RpcInboundCallHandle,
        predicate: impl FnOnce(&InboundCall, &InboundCallResources) -> bool,
    ) -> Option<RpcInboundClosing> {
        self.begin_inbound_closing_for_route_if(
            &handle.counterparty_route,
            &handle.call_id,
            |call, resources| {
                inbound_call_matches_handle(call, handle) && predicate(call, resources)
            },
        )
    }

    pub(crate) fn finish_inbound_closing(
        &mut self,
        closing: &RpcInboundClosing,
    ) -> Option<InboundCall> {
        let call_key = RpcCallKey::new(
            closing.handle.counterparty_route.clone(),
            closing.handle.call_id.clone(),
        );
        let call = self.inbound_calls.get(&call_key)?;
        let generation_matches = call.generation == closing.handle.generation;
        if !matches!(call.state, InboundCallState::Closing)
            || !generation_matches
            || call.method != closing.handle.method
        {
            return None;
        }
        self.remove_inbound_by_key(&call_key)
    }

    #[cfg(test)]
    pub(crate) fn set_inbound_state_for_route_if(
        &mut self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        mut predicate: impl FnMut(&InboundCall) -> bool,
        state: InboundCallState,
    ) -> bool {
        let Some(call) = self.inbound_calls.get_mut(&RpcCallKey::new(
            counterparty_route.clone(),
            call_id.clone(),
        )) else {
            return false;
        };
        if !predicate(call) {
            return false;
        }
        call.state = state;
        true
    }

    #[cfg(test)]
    pub(crate) fn remove_inbound_for_route(
        &mut self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<InboundCall> {
        self.remove_inbound_by_key(&RpcCallKey::new(
            counterparty_route.clone(),
            call_id.clone(),
        ))
    }

    pub(crate) fn remove_inbound_for_route_if(
        &mut self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        mut predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Option<InboundCall> {
        let key = RpcCallKey::new(counterparty_route.clone(), call_id.clone());
        if !self.inbound_calls.get(&key).is_some_and(&mut predicate) {
            return None;
        }
        self.remove_inbound_by_key(&key)
    }

    pub(crate) fn remove_inbound_for_handle(
        &mut self,
        handle: &RpcInboundCallHandle,
    ) -> Option<InboundCall> {
        let key = RpcCallKey::new(handle.counterparty_route.clone(), handle.call_id.clone());
        if !self
            .inbound_calls
            .get(&key)
            .is_some_and(|call| inbound_call_matches_handle(call, handle))
        {
            return None;
        }
        self.remove_inbound_by_key(&key)
    }

    pub(crate) fn remove_inbound_calls_if(
        &mut self,
        mut predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Vec<InboundCall> {
        let keys: Vec<_> = self
            .inbound_calls
            .iter()
            .filter_map(|(key, call)| predicate(call).then_some(key.clone()))
            .collect();
        keys.into_iter()
            .filter_map(|key| self.remove_inbound_by_key(&key))
            .collect()
    }

    fn remove_inbound_by_key(&mut self, key: &RpcCallKey) -> Option<InboundCall> {
        let call = self.inbound_calls.remove(key)?;
        call.cancellation.cancel();
        if let Some(key) = &call.dedup_key
            && self
                .inbound_dedup_index
                .get(key)
                .is_some_and(|indexed_call_key| {
                    indexed_call_key.call_id == call.call_id
                        && indexed_call_key.counterparty_route == call.counterparty_route
                })
        {
            self.inbound_dedup_index.remove(key);
        }
        Some(call)
    }

    #[cfg(test)]
    pub(crate) fn dedup_call_key(&self, key: &DedupKey) -> Option<(&Route, &RoutedCallId)> {
        self.inbound_dedup_index
            .get(key)
            .map(|call_key| (&call_key.counterparty_route, &call_key.call_id))
    }

    fn register_outbound(&mut self, call: OutboundCall) -> Result<(), RegisterCallError> {
        let call_key = RpcCallKey::new(call.counterparty_route.clone(), call.call_id.clone());
        if self.outbound_calls.contains_key(&call_key) {
            return Err(RegisterCallError::DuplicateCallId {
                counterparty_route: call_key.counterparty_route,
                call_id: call_key.call_id,
            });
        }
        self.outbound_calls.insert(call_key, call);
        Ok(())
    }

    fn register_outbound_tracked(
        &mut self,
        call: OutboundCall,
    ) -> Result<RpcOutboundCallHandle, RegisterCallError> {
        let handle = RpcOutboundCallHandle {
            counterparty_route: call.counterparty_route.clone(),
            call_id: call.call_id.clone(),
            method: call.method,
        };
        self.register_outbound(call)?;
        Ok(handle)
    }

    pub(crate) fn register_client_outbound(
        &mut self,
        start: RpcClientOutboundStart,
    ) -> Result<RpcOutboundCallHandle, RegisterCallError> {
        self.register_outbound_tracked(OutboundCall {
            call_id: start.call_id,
            counterparty_route: start.counterparty_route,
            method: start.method,
            state: start.state,
            resources: Some(OutboundCallResources::ClientInbox { tx: start.inbox_tx }),
        })
    }

    pub(crate) fn register_local_origin_outbound(
        &mut self,
        start: RpcLocalOriginOutboundStart,
    ) -> Result<RpcOutboundCallHandle, RegisterCallError> {
        self.register_outbound_tracked(OutboundCall {
            call_id: start.call_id,
            counterparty_route: start.counterparty_route,
            method: start.method,
            state: start.state,
            resources: Some(OutboundCallResources::LocalOriginRouted {
                owner_link: start.owner_link,
                request_src: start.request_src,
                request_dst: start.request_dst,
            }),
        })
    }

    pub(crate) fn register_peer_stream_outbound(
        &mut self,
        start: RpcPeerStreamOutboundStart,
    ) -> Result<RpcOutboundCallHandle, RegisterCallError> {
        debug_assert_eq!(start.method.kind, MethodKind::ServerStreaming);
        self.register_outbound_tracked(OutboundCall {
            call_id: start.call_id,
            counterparty_route: start.counterparty_route,
            method: start.method,
            state: OutboundCallState::AwaitingResponse,
            resources: None,
        })
    }

    pub(crate) fn outbound_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<&OutboundCall> {
        self.outbound_calls.get(&RpcCallKey::new(
            counterparty_route.clone(),
            call_id.clone(),
        ))
    }

    pub(crate) fn set_outbound_state_for_route(
        &mut self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        state: OutboundCallState,
    ) -> bool {
        let Some(call) = self.outbound_calls.get_mut(&RpcCallKey::new(
            counterparty_route.clone(),
            call_id.clone(),
        )) else {
            return false;
        };
        call.state = state;
        true
    }

    pub(crate) fn set_outbound_state_for_handle(
        &mut self,
        handle: &RpcOutboundCallHandle,
        state: OutboundCallState,
    ) -> bool {
        let Some(call) = self.outbound_calls.get_mut(&RpcCallKey::new(
            handle.counterparty_route.clone(),
            handle.call_id.clone(),
        )) else {
            return false;
        };
        if !outbound_call_matches_handle(call, handle) {
            return false;
        }
        call.state = state;
        true
    }

    pub(crate) fn set_outbound_state_for_handle_if(
        &mut self,
        handle: &RpcOutboundCallHandle,
        predicate: impl FnOnce(OutboundCallState) -> bool,
        state: OutboundCallState,
    ) -> bool {
        let Some(call) = self.outbound_calls.get_mut(&RpcCallKey::new(
            handle.counterparty_route.clone(),
            handle.call_id.clone(),
        )) else {
            return false;
        };
        if !outbound_call_matches_handle(call, handle) || !predicate(call.state) {
            return false;
        }
        call.state = state;
        true
    }

    pub(crate) fn outbound_state_for_handle(
        &self,
        handle: &RpcOutboundCallHandle,
    ) -> Option<OutboundCallState> {
        let call = self.outbound_calls.get(&RpcCallKey::new(
            handle.counterparty_route.clone(),
            handle.call_id.clone(),
        ))?;
        outbound_call_matches_handle(call, handle).then_some(call.state)
    }

    pub(crate) fn remove_outbound_for_handle(
        &mut self,
        handle: &RpcOutboundCallHandle,
    ) -> Option<OutboundCall> {
        self.remove_outbound_for_route_if(&handle.counterparty_route, &handle.call_id, |call| {
            outbound_call_matches_handle(call, handle)
        })
    }

    pub(crate) fn remove_outbound_for_route_if(
        &mut self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        mut predicate: impl FnMut(&OutboundCall) -> bool,
    ) -> Option<OutboundCall> {
        let key = RpcCallKey::new(counterparty_route.clone(), call_id.clone());
        if !self.outbound_calls.get(&key).is_some_and(&mut predicate) {
            return None;
        }
        self.outbound_calls.remove(&key)
    }

    pub(crate) fn remove_outbound_calls_if(
        &mut self,
        mut predicate: impl FnMut(&OutboundCall) -> bool,
    ) -> Vec<OutboundCall> {
        let keys: Vec<_> = self
            .outbound_calls
            .iter()
            .filter_map(|(key, call)| predicate(call).then_some(key.clone()))
            .collect();
        keys.into_iter()
            .filter_map(|key| self.outbound_calls.remove(&key))
            .collect()
    }

    pub(crate) fn remove_local_origin_outbound_for_owner_link(
        &mut self,
        owner_link: &Link,
    ) -> Vec<RpcLocalOriginOutboundCall> {
        self.remove_outbound_calls_if(|call| {
            call.resources
                .as_ref()
                .and_then(|resources| resources.local_origin())
                .is_some_and(|(call_owner_link, _, _)| call_owner_link == owner_link)
        })
        .into_iter()
        .filter_map(OutboundCall::into_local_origin)
        .collect()
    }

    pub(crate) fn remove_local_origin_outbound_for_route_prefix(
        &mut self,
        route_prefix: &Route,
    ) -> Vec<RpcLocalOriginOutboundCall> {
        self.remove_outbound_calls_if(|call| {
            call.resources
                .as_ref()
                .and_then(|resources| resources.local_origin())
                .is_some_and(|(_, _, request_dst)| request_dst.starts_with_route(route_prefix))
        })
        .into_iter()
        .filter_map(OutboundCall::into_local_origin)
        .collect()
    }

    pub(crate) fn remove_inbound_for_owner_link_except_method(
        &mut self,
        owner_link: &Link,
        excluded_method: MethodSpec,
    ) -> Vec<InboundCall> {
        self.remove_inbound_calls_if(|call| {
            call.method != excluded_method
                && call
                    .resources
                    .as_ref()
                    .is_some_and(|resources| resources.owner_link == *owner_link)
        })
    }

    pub(crate) fn client_inbox_for_message(
        &self,
        message: &Message,
    ) -> Option<mpsc::Sender<Message>> {
        match message {
            Message::Local(frame) => self
                .outbound_calls
                .values()
                .find(|call| call.call_id == frame.call_id)
                .and_then(client_inbox),
            Message::Routed(RoutedFrame {
                src,
                dst,
                call_id,
                message: RoutedFrameMessage::Payload(_),
            }) if dst.is_empty() => self
                .outbound_calls
                .get(&RpcCallKey::new(src.clone(), call_id.clone()))
                .and_then(client_inbox),
            Message::Routed(RoutedFrame {
                dst,
                call_id,
                message: RoutedFrameMessage::RoutingError { failed_route, .. },
                ..
            }) if dst.is_empty() => self
                .outbound_calls
                .get(&RpcCallKey::new(failed_route.clone(), call_id.clone()))
                .and_then(client_inbox),
            Message::Peer(_)
            | Message::Routed(_)
            | Message::Ping
            | Message::Pong
            | Message::Reauth(_)
            | Message::ReauthResponse(_)
            | Message::PeerSnapshot { .. }
            | Message::GoAway(_) => None,
        }
    }

    pub(crate) fn remove_client_inboxes(&mut self) -> Vec<mpsc::Sender<Message>> {
        self.remove_outbound_calls_if(|call| {
            matches!(
                call.resources,
                Some(OutboundCallResources::ClientInbox { .. })
            )
        })
        .into_iter()
        .filter_map(|call| client_inbox(&call))
        .collect()
    }
}

impl RpcCallDebugSnapshot {
    fn new<const N: usize>(states: [&'static str; N]) -> Self {
        Self {
            total: 0,
            by_state: states.into_iter().map(|state| (state, 0)).collect(),
            by_method: BTreeMap::new(),
            by_counterparty: BTreeMap::new(),
        }
    }

    fn record(&mut self, state: &'static str, method: &'static str, counterparty: String) {
        self.total += 1;
        *self.by_state.entry(state).or_default() += 1;
        *self.by_method.entry(method).or_default() += 1;
        *self.by_counterparty.entry(counterparty).or_default() += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::link::Link;
    use crate::protocol::message::ResponseFrame;
    use crate::protocol::method;

    fn route(link: &str) -> Route {
        Route::from_link(Link::new(link).unwrap())
    }

    fn call_id(n: u128) -> RoutedCallId {
        RoutedCallId::from(Uuid::from_u128(n))
    }

    fn routed_sink(tx: mpsc::Sender<Message>) -> RpcRoutedSink {
        RpcRoutedSink::new(
            tx,
            route("server"),
            route("client"),
            call_id(42),
            Arc::new(Mutex::new(())),
        )
    }

    fn routed_payload(message: Message) -> Vec<u8> {
        let Message::Routed(frame) = message else {
            panic!("expected routed message");
        };
        assert_eq!(frame.src, route("server"));
        assert_eq!(frame.dst, route("client"));
        assert_eq!(frame.call_id, call_id(42));
        let RoutedFrameMessage::Payload(payload) = frame.message else {
            panic!("expected routed payload");
        };
        payload
    }

    fn inbound_resources() -> InboundCallResources {
        let (tx, _rx) = mpsc::channel(1);
        InboundCallResources {
            owner_link: Link::new("owner").unwrap(),
            output: routed_sink(tx),
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum TestStreamItem {
        Bytes(Vec<u8>),
        Cancel,
    }

    struct TestStreamCodec;

    impl RpcStreamCodec for TestStreamCodec {
        type Item = TestStreamItem;

        fn decode_frame(frame: FrameBody) -> Result<Self::Item, ProtocolError> {
            match frame {
                FrameBody::StreamItem(payload) => Ok(TestStreamItem::Bytes(payload)),
                FrameBody::Cancel => Ok(TestStreamItem::Cancel),
                FrameBody::Request(_) | FrameBody::Response(_) => {
                    Err(ProtocolError::InvalidArgument {
                        message: "test stream accepts only stream items or cancel frames"
                            .to_string(),
                    })
                }
            }
        }
    }

    struct TestStreamEncoder;

    impl RpcStreamEncoder for TestStreamEncoder {
        type Item = TestStreamItem;

        fn encode_item(item: &Self::Item) -> Vec<u8> {
            match item {
                TestStreamItem::Bytes(bytes) => bytes.clone(),
                TestStreamItem::Cancel => b"cancel".to_vec(),
            }
        }
    }

    #[tokio::test]
    async fn rpc_stream_writer_delivers_frame_bodies() {
        let (writer, mut reader) = RpcStreamWriter::channel(2);

        writer
            .send_frame_body(FrameBody::StreamItem(b"hello".to_vec()))
            .await
            .unwrap();
        writer.send_frame_body(FrameBody::Cancel).await.unwrap();

        assert_eq!(
            reader.recv_frame().await,
            Some(FrameBody::StreamItem(b"hello".to_vec()))
        );
        assert_eq!(reader.recv_frame().await, Some(FrameBody::Cancel));
    }

    #[tokio::test]
    async fn typed_rpc_stream_reader_maps_frames_through_codec() {
        let (writer, reader) = RpcStreamWriter::channel(2);
        let mut reader = reader.decode_with::<TestStreamCodec>();

        writer
            .send_frame_body(FrameBody::StreamItem(b"hello".to_vec()))
            .await
            .unwrap();
        writer.send_frame_body(FrameBody::Cancel).await.unwrap();

        assert_eq!(
            reader.recv().await,
            Some(Ok(TestStreamItem::Bytes(b"hello".to_vec())))
        );
        assert_eq!(reader.recv().await, Some(Ok(TestStreamItem::Cancel)));
    }

    #[tokio::test]
    async fn routed_sink_sends_stream_item_when_current() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = routed_sink(tx);

        let sent = sink
            .send_stream_item_if_current(b"hello".to_vec(), || async { true })
            .await
            .unwrap();

        assert!(sent);
        let payload = routed_payload(rx.recv().await.unwrap());
        assert_eq!(
            crate::protocol::wire::decode_frame_body(&payload).unwrap(),
            FrameBody::StreamItem(b"hello".to_vec())
        );
    }

    #[tokio::test]
    async fn typed_routed_sink_encodes_stream_items() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = routed_sink(tx).encode_with::<TestStreamEncoder>();

        let sent = sink
            .send_item_if_current(TestStreamItem::Bytes(b"hello".to_vec()), || async { true })
            .await
            .unwrap();

        assert!(sent);
        let payload = routed_payload(rx.recv().await.unwrap());
        assert_eq!(
            crate::protocol::wire::decode_frame_body(&payload).unwrap(),
            FrameBody::StreamItem(b"hello".to_vec())
        );
    }

    #[tokio::test]
    async fn routed_sink_skips_stream_item_when_not_current() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = routed_sink(tx);

        let sent = sink
            .send_stream_item_if_current(b"hello".to_vec(), || async { false })
            .await
            .unwrap();

        assert!(!sent);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn routed_sink_sends_terminal_response() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = routed_sink(tx);

        sink.send_empty_response_result(Err(ProtocolError::Cancelled {
            message: "cancelled".to_string(),
        }))
        .await
        .unwrap();

        let payload = routed_payload(rx.recv().await.unwrap());
        assert_eq!(
            crate::protocol::wire::decode_frame_body(&payload).unwrap(),
            FrameBody::Response(ResponseFrame::Error(ProtocolError::Cancelled {
                message: "cancelled".to_string(),
            }))
        );
    }

    #[tokio::test]
    async fn register_routed_bidi_owns_call_state_stream_and_sink() {
        let (tx, mut outbound_rx) = mpsc::channel(1);
        let mut state = RpcState::new();
        let counterparty = route("client");
        let call_id = call_id(42);

        let mut call = state
            .register_routed_bidi(RpcRoutedBidiStart {
                tx,
                owner_link: Link::new("owner").unwrap(),
                reply_src: route("server"),
                reply_dst: counterparty.clone(),
                counterparty_route: counterparty.clone(),
                call_id: call_id.clone(),
                method: method::AGENT_OPEN_SESSION,
                dedup_key: None,
                stream_capacity: 1,
            })
            .unwrap();

        let inbound = state.inbound_for_route(&counterparty, &call_id).unwrap();
        assert_eq!(inbound.method, method::AGENT_OPEN_SESSION);
        assert_eq!(inbound.state, InboundCallState::Active);
        assert_eq!(inbound.generation, call.handle.generation);
        assert!(!call.cancellation.is_cancelled());
        let stream_writer = inbound.stream_writer.clone().unwrap();

        stream_writer
            .send_frame_body(FrameBody::Cancel)
            .await
            .unwrap();
        assert_eq!(call.input.recv_frame().await, Some(FrameBody::Cancel));

        call.output
            .send_empty_response_result(Ok(()))
            .await
            .unwrap();
        let payload = routed_payload(outbound_rx.recv().await.unwrap());
        assert_eq!(
            crate::protocol::wire::decode_frame_body(&payload).unwrap(),
            FrameBody::Response(ResponseFrame::Payload(Vec::new()))
        );

        let closing = state
            .begin_inbound_closing_for_handle_if(&call.handle, |_, _| true)
            .expect("active bidi call should move to closing");
        assert!(call.cancellation.is_cancelled());
        assert!(state.finish_inbound_closing(&closing).is_some());
    }

    #[tokio::test]
    async fn register_routed_unary_owns_call_state_and_terminal_sink() {
        let (tx, mut outbound_rx) = mpsc::channel(1);
        let mut state = RpcState::new();
        let counterparty = route("client");
        let call_id = call_id(42);

        let call = state
            .register_routed_unary(RpcRoutedUnaryStart {
                tx,
                owner_link: Link::new("owner").unwrap(),
                reply_src: route("server"),
                reply_dst: counterparty.clone(),
                counterparty_route: counterparty.clone(),
                call_id: call_id.clone(),
                method: method::AGENT_CREATE,
            })
            .unwrap();

        let inbound = state.inbound_for_route(&counterparty, &call_id).unwrap();
        assert_eq!(inbound.method, method::AGENT_CREATE);
        assert_eq!(inbound.state, InboundCallState::Active);
        assert!(inbound.stream_writer.is_none());
        assert_eq!(inbound.generation, call.handle.generation);
        assert!(!inbound.cancellation.is_cancelled());

        let closing = state
            .begin_inbound_closing_for_handle_if(&call.handle, |_, _| true)
            .expect("active unary call should move to closing");
        assert!(
            state
                .inbound_for_route(&counterparty, &call_id)
                .unwrap()
                .cancellation
                .is_cancelled()
        );
        closing
            .send_response(ResponseFrame::Payload(b"created".to_vec()))
            .await
            .unwrap();
        assert!(state.finish_inbound_closing(&closing).is_some());
        assert!(state.inbound_for_route(&counterparty, &call_id).is_none());

        let payload = routed_payload(outbound_rx.recv().await.unwrap());
        assert_eq!(
            crate::protocol::wire::decode_frame_body(&payload).unwrap(),
            FrameBody::Response(ResponseFrame::Payload(b"created".to_vec()))
        );
    }

    #[test]
    fn register_server_stream_owns_no_input_call_state() {
        let (tx, _rx) = mpsc::channel(1);
        let mut state = RpcState::new();
        let counterparty = route("peer");
        let call_id = call_id(42);

        let stream = state
            .register_server_stream(RpcServerStreamStart {
                tx,
                counterparty_route: counterparty.clone(),
                call_id: call_id.clone(),
                method: method::ROUTING_SUBSCRIBE_EVENTS,
                dedup_key: None,
            })
            .unwrap();

        let inbound = state.inbound_for_route(&counterparty, &call_id).unwrap();
        assert_eq!(inbound.method, method::ROUTING_SUBSCRIBE_EVENTS);
        assert_eq!(inbound.state, InboundCallState::Starting);
        assert_eq!(inbound.generation, stream.handle.generation);
        assert!(inbound.stream_writer.is_none());
        assert!(inbound.resources.is_none());
        assert_eq!(
            state.active_inbound_call_id_for_route_and_method(
                &counterparty,
                method::ROUTING_SUBSCRIBE_EVENTS
            ),
            None
        );
        assert!(matches!(
            state.inbound_frame_target_for_route(&counterparty, &call_id),
            Some(RpcInboundFrameTarget::NotAccepting {
                state: InboundCallState::Starting
            })
        ));

        assert!(state.activate_inbound_for_handle(&stream.handle));
        let inbound = state.inbound_for_route(&counterparty, &call_id).unwrap();
        assert_eq!(inbound.state, InboundCallState::Active);
        assert_eq!(
            state.active_inbound_call_id_for_route_and_method(
                &counterparty,
                method::ROUTING_SUBSCRIBE_EVENTS
            ),
            Some(call_id.clone())
        );
        assert!(matches!(
            state.inbound_frame_target_for_route(&counterparty, &call_id),
            Some(RpcInboundFrameTarget::ActiveNoInput {
                method: method::ROUTING_SUBSCRIBE_EVENTS
            })
        ));
    }

    #[tokio::test]
    async fn inbound_frame_target_identifies_active_stream_calls() {
        let (tx, _outbound_rx) = mpsc::channel(1);
        let mut state = RpcState::new();
        let counterparty = route("client");
        let call_id = call_id(42);

        let mut call = state
            .register_routed_bidi(RpcRoutedBidiStart {
                tx,
                owner_link: Link::new("owner").unwrap(),
                reply_src: route("server"),
                reply_dst: counterparty.clone(),
                counterparty_route: counterparty.clone(),
                call_id: call_id.clone(),
                method: method::AGENT_OPEN_SESSION,
                dedup_key: None,
                stream_capacity: 1,
            })
            .unwrap();

        let Some(RpcInboundFrameTarget::ActiveStream {
            method,
            stream_writer,
        }) = state.inbound_frame_target_for_route(&counterparty, &call_id)
        else {
            panic!("expected active stream target");
        };
        assert_eq!(method, method::AGENT_OPEN_SESSION);

        stream_writer
            .send_frame_body(FrameBody::Cancel)
            .await
            .unwrap();
        assert_eq!(call.input.recv_frame().await, Some(FrameBody::Cancel));
    }

    #[test]
    fn inbound_frame_target_identifies_active_no_input_calls() {
        let (tx, _outbound_rx) = mpsc::channel(1);
        let mut state = RpcState::new();
        let counterparty = route("client");
        let call_id = call_id(42);

        state
            .register_routed_unary(RpcRoutedUnaryStart {
                tx,
                owner_link: Link::new("owner").unwrap(),
                reply_src: route("server"),
                reply_dst: counterparty.clone(),
                counterparty_route: counterparty.clone(),
                call_id: call_id.clone(),
                method: method::AGENT_CREATE,
            })
            .unwrap();

        assert!(matches!(
            state.inbound_frame_target_for_route(&counterparty, &call_id),
            Some(RpcInboundFrameTarget::ActiveNoInput {
                method: method::AGENT_CREATE
            })
        ));
    }

    #[test]
    fn inbound_frame_target_reports_closing_calls_as_not_accepting() {
        let (tx, _outbound_rx) = mpsc::channel(1);
        let mut state = RpcState::new();
        let counterparty = route("client");
        let call_id = call_id(42);
        let call = state
            .register_routed_unary(RpcRoutedUnaryStart {
                tx,
                owner_link: Link::new("owner").unwrap(),
                reply_src: route("server"),
                reply_dst: counterparty.clone(),
                counterparty_route: counterparty.clone(),
                call_id: call_id.clone(),
                method: method::AGENT_CREATE,
            })
            .unwrap();

        state
            .begin_inbound_closing_for_handle_if(&call.handle, |_, _| true)
            .unwrap();

        assert!(matches!(
            state.inbound_frame_target_for_route(&counterparty, &call_id),
            Some(RpcInboundFrameTarget::NotAccepting {
                state: InboundCallState::Closing
            })
        ));
    }

    #[tokio::test]
    async fn register_routed_bidi_rejects_duplicate_dedup_key() {
        let (tx, _outbound_rx) = mpsc::channel(1);
        let mut state = RpcState::new();
        let counterparty = route("client");
        let dedup_key = DedupKey::OpenSession {
            counterparty_route: counterparty.clone(),
            agent_id: Uuid::new_v4(),
        };

        state
            .register_routed_bidi(RpcRoutedBidiStart {
                tx: tx.clone(),
                owner_link: Link::new("owner").unwrap(),
                reply_src: route("server"),
                reply_dst: counterparty.clone(),
                counterparty_route: counterparty.clone(),
                call_id: call_id(42),
                method: method::AGENT_OPEN_SESSION,
                dedup_key: Some(dedup_key.clone()),
                stream_capacity: 1,
            })
            .unwrap();

        let error = state
            .register_routed_bidi(RpcRoutedBidiStart {
                tx,
                owner_link: Link::new("owner").unwrap(),
                reply_src: route("server"),
                reply_dst: counterparty.clone(),
                counterparty_route: counterparty.clone(),
                call_id: call_id(43),
                method: method::AGENT_OPEN_SESSION,
                dedup_key: Some(dedup_key.clone()),
                stream_capacity: 1,
            })
            .unwrap_err();

        assert_eq!(
            error,
            RegisterCallError::DuplicateDedupKey {
                key: dedup_key,
                counterparty_route: counterparty,
                call_id: call_id(42),
            }
        );
        assert_eq!(state.inbound_len(), 1);
        assert_eq!(state.dedup_len(), 1);
    }

    #[test]
    fn begin_inbound_closing_for_route_moves_call_state_and_returns_close_token() {
        let mut state = RpcState::new();
        let counterparty = route("client");
        let call_id = call_id(42);
        let generation = Uuid::new_v4();
        state
            .register_inbound(InboundCall {
                call_id: call_id.clone(),
                counterparty_route: counterparty.clone(),
                method: method::AGENT_OPEN_SESSION,
                generation,
                state: InboundCallState::Active,
                dedup_key: None,
                stream_writer: None,
                resources: Some(inbound_resources()),
                cancellation: RpcCallCancellation::new(),
            })
            .unwrap();

        let closing = state
            .begin_inbound_closing_for_route_if(&counterparty, &call_id, |call, resources| {
                call.method == method::AGENT_OPEN_SESSION
                    && call.generation == generation
                    && resources.owner_link == Link::new("owner").unwrap()
            })
            .expect("active call should move to closing");

        assert_eq!(closing.handle.counterparty_route, counterparty);
        assert_eq!(closing.handle.call_id, call_id);
        assert_eq!(closing.handle.method, method::AGENT_OPEN_SESSION);
        assert_eq!(closing.handle.generation, generation);
        assert!(matches!(
            state
                .inbound_for_route(&counterparty, &call_id)
                .map(|call| call.state),
            Some(InboundCallState::Closing)
        ));
        assert!(
            state
                .begin_inbound_closing_for_route_if(&counterparty, &call_id, |_, _| true)
                .is_none()
        );
    }

    #[test]
    fn begin_inbound_closing_for_route_respects_predicate() {
        let mut state = RpcState::new();
        let counterparty = route("client");
        let call_id = call_id(42);
        state
            .register_inbound(InboundCall {
                call_id: call_id.clone(),
                counterparty_route: counterparty.clone(),
                method: method::AGENT_OPEN_SESSION,
                generation: Uuid::new_v4(),
                state: InboundCallState::Active,
                dedup_key: None,
                stream_writer: None,
                resources: Some(inbound_resources()),
                cancellation: RpcCallCancellation::new(),
            })
            .unwrap();

        assert!(
            state
                .begin_inbound_closing_for_route_if(&counterparty, &call_id, |_, _| false)
                .is_none()
        );
        assert!(matches!(
            state
                .inbound_for_route(&counterparty, &call_id)
                .map(|call| call.state),
            Some(InboundCallState::Active)
        ));
    }

    #[test]
    fn finish_inbound_closing_for_route_requires_generation_and_clears_dedup() {
        let mut state = RpcState::new();
        let counterparty = route("client");
        let call_id = call_id(42);
        let generation = Uuid::new_v4();
        let dedup_key = DedupKey::OpenSession {
            counterparty_route: counterparty.clone(),
            agent_id: Uuid::new_v4(),
        };
        state
            .register_inbound(InboundCall {
                call_id: call_id.clone(),
                counterparty_route: counterparty.clone(),
                method: method::AGENT_OPEN_SESSION,
                generation,
                state: InboundCallState::Active,
                dedup_key: Some(dedup_key.clone()),
                stream_writer: None,
                resources: Some(inbound_resources()),
                cancellation: RpcCallCancellation::new(),
            })
            .unwrap();

        let closing = state
            .begin_inbound_closing_for_route_if(&counterparty, &call_id, |_, _| true)
            .expect("active call should move to closing");
        let mut wrong_generation = closing.clone();
        wrong_generation.handle.generation = Uuid::new_v4();

        assert!(state.finish_inbound_closing(&wrong_generation).is_none());
        assert_eq!(
            state.dedup_call_key(&dedup_key),
            Some((&counterparty, &call_id))
        );

        let removed = state
            .finish_inbound_closing(&closing)
            .expect("matching closing generation should remove call");

        assert_eq!(removed.call_id, call_id);
        assert!(state.inbound_for_route(&counterparty, &call_id).is_none());
        assert!(state.dedup_call_key(&dedup_key).is_none());
    }

    #[tokio::test]
    async fn typed_rpc_stream_reader_returns_decode_errors() {
        let (writer, reader) = RpcStreamWriter::channel(1);
        let mut reader = reader.decode_with::<TestStreamCodec>();

        writer
            .send_frame_body(FrameBody::Response(ResponseFrame::Payload(Vec::new())))
            .await
            .unwrap();

        assert_eq!(
            reader.recv().await,
            Some(Err(ProtocolError::InvalidArgument {
                message: "test stream accepts only stream items or cancel frames".to_string(),
            }))
        );
    }

    #[test]
    fn inbound_dedup_key_rejects_second_active_call() {
        let mut state = RpcState::new();
        let agent_id = Uuid::new_v4();
        let key = DedupKey::OpenSession {
            counterparty_route: route("client-a"),
            agent_id,
        };
        state
            .register_inbound(InboundCall {
                call_id: call_id(1),
                counterparty_route: route("client-a"),
                method: method::AGENT_OPEN_SESSION,
                generation: Uuid::new_v4(),
                state: InboundCallState::Active,
                dedup_key: Some(key.clone()),
                stream_writer: None,
                resources: None,
                cancellation: RpcCallCancellation::new(),
            })
            .unwrap();

        let err = state
            .register_inbound(InboundCall {
                call_id: call_id(2),
                counterparty_route: route("client-a"),
                method: method::AGENT_OPEN_SESSION,
                generation: Uuid::new_v4(),
                state: InboundCallState::Active,
                dedup_key: Some(key.clone()),
                stream_writer: None,
                resources: None,
                cancellation: RpcCallCancellation::new(),
            })
            .unwrap_err();

        assert_eq!(
            err,
            RegisterCallError::DuplicateDedupKey {
                key,
                counterparty_route: route("client-a"),
                call_id: call_id(1),
            }
        );
        assert_eq!(state.inbound_len(), 1);
        assert_eq!(state.dedup_len(), 1);

        let snapshot = state.debug_snapshot();
        assert_eq!(snapshot.inbound_calls.total, 1);
        assert_eq!(
            snapshot
                .inbound_calls
                .by_method
                .get(method::AGENT_OPEN_SESSION.name),
            Some(&1)
        );
        assert_eq!(
            snapshot.inbound_calls.by_counterparty.get("client-a"),
            Some(&1)
        );
    }

    #[test]
    fn removing_inbound_call_clears_matching_dedup_key() {
        let mut state = RpcState::new();
        let agent_id = Uuid::new_v4();
        let key = DedupKey::OpenSession {
            counterparty_route: route("client-a"),
            agent_id,
        };
        state
            .register_inbound(InboundCall {
                call_id: call_id(1),
                counterparty_route: route("client-a"),
                method: method::AGENT_OPEN_SESSION,
                generation: Uuid::new_v4(),
                state: InboundCallState::Active,
                dedup_key: Some(key.clone()),
                stream_writer: None,
                resources: None,
                cancellation: RpcCallCancellation::new(),
            })
            .unwrap();

        let removed = state
            .remove_inbound_for_route(&route("client-a"), &call_id(1))
            .unwrap();

        assert_eq!(removed.call_id, call_id(1));
        assert!(state.dedup_call_key(&key).is_none());
        assert_eq!(state.inbound_len(), 0);
        assert_eq!(state.dedup_len(), 0);
    }

    #[test]
    fn outbound_calls_are_tracked_separately_from_inbound_calls() {
        let mut state = RpcState::new();
        state
            .register_inbound(InboundCall {
                call_id: call_id(1),
                counterparty_route: route("client-a"),
                method: method::AGENT_LIST,
                generation: Uuid::new_v4(),
                state: InboundCallState::Active,
                dedup_key: None,
                stream_writer: None,
                resources: None,
                cancellation: RpcCallCancellation::new(),
            })
            .unwrap();
        state
            .register_outbound(OutboundCall {
                call_id: call_id(2),
                counterparty_route: route("server-a"),
                method: method::AGENT_LIST,
                state: OutboundCallState::AwaitingResponse,
                resources: None,
            })
            .unwrap();

        assert!(
            state
                .inbound_for_route(&route("client-a"), &call_id(1))
                .is_some()
        );
        assert!(
            state
                .outbound_for_route(&route("server-a"), &call_id(2))
                .is_some()
        );
        assert_eq!(state.inbound_len(), 1);
        assert_eq!(state.outbound_len(), 1);

        assert!(state.set_inbound_state_for_route_if(
            &route("client-a"),
            &call_id(1),
            |_| true,
            InboundCallState::Closing
        ));
        assert!(matches!(
            state
                .inbound_for_route(&route("client-a"), &call_id(1))
                .map(|call| call.state),
            Some(InboundCallState::Closing)
        ));
        assert!(state.set_outbound_state_for_route(
            &route("server-a"),
            &call_id(2),
            OutboundCallState::ActiveStream
        ));
        assert!(matches!(
            state
                .outbound_for_route(&route("server-a"), &call_id(2))
                .map(|call| call.state),
            Some(OutboundCallState::ActiveStream)
        ));
        assert!(
            state
                .remove_outbound_for_route_if(&route("server-a"), &call_id(2), |_| true)
                .is_some()
        );
        assert!(
            state
                .inbound_for_route(&route("client-a"), &call_id(1))
                .is_some()
        );
        assert_eq!(state.inbound_len(), 1);
        assert_eq!(state.outbound_len(), 0);
    }

    #[test]
    fn outbound_call_handle_guards_state_changes_by_route_call_and_method() {
        let mut state = RpcState::new();
        let handle = state
            .register_outbound_tracked(OutboundCall {
                call_id: call_id(2),
                counterparty_route: route("server-a"),
                method: method::AGENT_OPEN_SESSION,
                state: OutboundCallState::AwaitingResponse,
                resources: None,
            })
            .unwrap();
        let wrong_method = RpcOutboundCallHandle {
            method: method::AGENT_CREATE,
            ..handle.clone()
        };

        assert!(
            !state.set_outbound_state_for_handle(&wrong_method, OutboundCallState::ActiveStream)
        );
        assert!(matches!(
            state
                .outbound_for_route(&route("server-a"), &call_id(2))
                .map(|call| call.state),
            Some(OutboundCallState::AwaitingResponse)
        ));

        assert!(state.set_outbound_state_for_handle(&handle, OutboundCallState::ActiveStream));
        assert!(matches!(
            state
                .outbound_for_route(&route("server-a"), &call_id(2))
                .map(|call| call.state),
            Some(OutboundCallState::ActiveStream)
        ));
        assert!(matches!(
            state.outbound_state_for_handle(&handle),
            Some(OutboundCallState::ActiveStream)
        ));
        assert!(!state.set_outbound_state_for_handle_if(
            &wrong_method,
            |_| true,
            OutboundCallState::Closing
        ));
        assert!(state.set_outbound_state_for_handle_if(
            &handle,
            |state| state == OutboundCallState::ActiveStream,
            OutboundCallState::Closing
        ));
        assert!(matches!(
            state.outbound_state_for_handle(&handle),
            Some(OutboundCallState::Closing)
        ));
        assert!(state.remove_outbound_for_handle(&wrong_method).is_none());
        assert!(state.remove_outbound_for_handle(&handle).is_some());
        assert!(state.outbound_state_for_handle(&handle).is_none());
        assert_eq!(state.outbound_len(), 0);
    }

    #[test]
    fn same_call_id_is_allowed_for_different_counterparty_routes() {
        let mut state = RpcState::new();
        let call_id = call_id(1);
        state
            .register_inbound(InboundCall {
                call_id: call_id.clone(),
                counterparty_route: route("client-a"),
                method: method::AGENT_OPEN_SESSION,
                generation: Uuid::new_v4(),
                state: InboundCallState::Active,
                dedup_key: Some(DedupKey::OpenSession {
                    counterparty_route: route("client-a"),
                    agent_id: Uuid::new_v4(),
                }),
                stream_writer: None,
                resources: None,
                cancellation: RpcCallCancellation::new(),
            })
            .unwrap();
        state
            .register_inbound(InboundCall {
                call_id: call_id.clone(),
                counterparty_route: route("client-b"),
                method: method::AGENT_OPEN_SESSION,
                generation: Uuid::new_v4(),
                state: InboundCallState::Active,
                dedup_key: Some(DedupKey::OpenSession {
                    counterparty_route: route("client-b"),
                    agent_id: Uuid::new_v4(),
                }),
                stream_writer: None,
                resources: None,
                cancellation: RpcCallCancellation::new(),
            })
            .unwrap();

        assert_eq!(state.inbound_len(), 2);
        assert!(
            state
                .remove_inbound_for_route(&route("client-a"), &call_id)
                .is_some()
        );
        assert_eq!(state.inbound_len(), 1);
        assert!(
            state
                .remove_inbound_for_route(&route("client-b"), &call_id)
                .is_some()
        );
        assert_eq!(state.inbound_len(), 0);
    }

    #[test]
    fn debug_snapshot_reports_all_call_states() {
        let mut state = RpcState::new();
        state
            .register_inbound(InboundCall {
                call_id: call_id(1),
                counterparty_route: route("client-a"),
                method: method::AGENT_LIST,
                generation: Uuid::new_v4(),
                state: InboundCallState::Closing,
                dedup_key: None,
                stream_writer: None,
                resources: None,
                cancellation: RpcCallCancellation::new(),
            })
            .unwrap();
        state
            .register_outbound(OutboundCall {
                call_id: call_id(2),
                counterparty_route: route("server-a"),
                method: method::ROUTING_SUBSCRIBE_EVENTS,
                state: OutboundCallState::ActiveStream,
                resources: None,
            })
            .unwrap();
        state
            .register_outbound(OutboundCall {
                call_id: call_id(3),
                counterparty_route: route("server-a"),
                method: method::AGENT_OPEN_SESSION,
                state: OutboundCallState::Closing,
                resources: None,
            })
            .unwrap();

        let snapshot = state.debug_snapshot();

        assert_eq!(snapshot.inbound_calls.by_state.get("starting"), Some(&0));
        assert_eq!(snapshot.inbound_calls.by_state.get("active"), Some(&0));
        assert_eq!(snapshot.inbound_calls.by_state.get("closing"), Some(&1));
        assert_eq!(
            snapshot.outbound_calls.by_state.get("awaiting_response"),
            Some(&0)
        );
        assert_eq!(
            snapshot.outbound_calls.by_state.get("active_stream"),
            Some(&1)
        );
        assert_eq!(snapshot.outbound_calls.by_state.get("closing"), Some(&1));
        assert_eq!(
            snapshot
                .outbound_calls
                .by_method
                .get(method::ROUTING_SUBSCRIBE_EVENTS.name),
            Some(&1)
        );
        assert_eq!(
            snapshot.outbound_calls.by_counterparty.get("server-a"),
            Some(&2)
        );
    }
}

use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{Mutex, mpsc, watch};

use super::call::{RpcInboundBidi, RpcInboundClosing, RpcTypedInboundBidi};
use crate::protocol::message::{
    FrameBody, Message, PeerFrame, ProtocolError, ResponseFrame, RoutedFrame, RoutedFrameMessage,
};
use crate::protocol::{CallId, Route, wire};

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
    pub(in crate::rpc) fn new() -> Self {
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

    pub(in crate::rpc) fn cancel(&self) {
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
    call_id: CallId,
    send_gate: Arc<Mutex<()>>,
}

#[derive(Debug)]
pub(crate) struct RpcTypedRoutedSink<C> {
    inner: RpcRoutedSink,
    _codec: PhantomData<C>,
}

#[derive(Debug, Clone)]
pub(crate) struct RpcPeerStreamSink {
    pub(in crate::rpc) tx: mpsc::Sender<Message>,
    pub(in crate::rpc) call_id: CallId,
}

#[derive(Debug)]
pub(crate) enum RpcRoutedSnapshotSendError {
    Encode(wire::EncodeError),
    Full,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RpcPeerSnapshotSendError {
    Full,
    Closed,
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
    pub(in crate::rpc) async fn recv_frame(&mut self) -> Option<FrameBody> {
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
        call_id: CallId,
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

    pub(crate) fn try_send_stream_item(&self, payload: Vec<u8>) -> bool {
        let Ok(message) = self.stream_item_message(payload) else {
            return false;
        };
        self.tx.try_send(message).is_ok()
    }

    pub(crate) fn try_send_snapshot(
        &self,
        payloads: impl IntoIterator<Item = Vec<u8>>,
    ) -> Result<(), RpcRoutedSnapshotSendError> {
        let messages: Vec<_> = payloads
            .into_iter()
            .map(|payload| self.stream_item_message(payload))
            .collect::<Result<_, _>>()
            .map_err(RpcRoutedSnapshotSendError::Encode)?;
        let permits = self
            .tx
            .try_reserve_many(messages.len())
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RpcRoutedSnapshotSendError::Full,
                mpsc::error::TrySendError::Closed(_) => RpcRoutedSnapshotSendError::Closed,
            })?;
        for (permit, message) in permits.zip(messages) {
            permit.send(message);
        }
        Ok(())
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

    fn stream_item_message(&self, payload: Vec<u8>) -> Result<Message, wire::EncodeError> {
        let payload = wire::encode_frame_body(&FrameBody::StreamItem(payload))?;
        Ok(Message::Routed(RoutedFrame {
            src: self.src.clone(),
            dst: self.dst.clone(),
            call_id: self.call_id.clone(),
            message: RoutedFrameMessage::Payload(payload),
        }))
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

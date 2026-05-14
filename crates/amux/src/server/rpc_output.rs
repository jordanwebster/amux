use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use crate::protocol::message::{CallId, Frame, FrameBody, Message, ProtocolError, ResponseFrame};
use crate::protocol::route::Route;

pub(crate) trait ServerStreamEncoder {
    type Item;

    fn encode_item(item: &Self::Item) -> Vec<u8>;
}

#[derive(Debug, Clone)]
pub(crate) struct ServerStreamSink {
    tx: mpsc::Sender<Message>,
    src: Route,
    dst: Route,
    call_id: CallId,
    send_gate: Arc<Mutex<()>>,
}

#[derive(Debug)]
pub(crate) struct TypedServerStreamSink<C> {
    inner: ServerStreamSink,
    _codec: PhantomData<C>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerStreamSnapshotSendError {
    Full,
    Closed,
}

#[derive(Debug)]
pub(crate) enum ServerStreamSendError {
    Closed,
}

impl ServerStreamSink {
    pub(crate) fn new(tx: mpsc::Sender<Message>, src: Route, dst: Route, call_id: CallId) -> Self {
        Self {
            tx,
            src,
            dst,
            call_id,
            send_gate: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn encode_with<C: ServerStreamEncoder>(self) -> TypedServerStreamSink<C> {
        TypedServerStreamSink {
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
    ) -> Result<bool, ServerStreamSendError>
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
    ) -> Result<(), ServerStreamSendError> {
        self.send_frame_body(FrameBody::Response(response)).await
    }

    pub(crate) async fn send_empty_response_result(
        &self,
        result: Result<(), ProtocolError>,
    ) -> Result<(), ServerStreamSendError> {
        let response = match result {
            Ok(()) => ResponseFrame::Payload(Vec::new()),
            Err(error) => ResponseFrame::Error(error),
        };
        self.send_response(response).await
    }

    pub(crate) fn try_send_stream_item(&self, payload: Vec<u8>) -> bool {
        self.tx.try_send(self.stream_item_message(payload)).is_ok()
    }

    pub(crate) fn try_send_snapshot(
        &self,
        payloads: impl IntoIterator<Item = Vec<u8>>,
    ) -> Result<(), ServerStreamSnapshotSendError> {
        let messages: Vec<_> = payloads
            .into_iter()
            .map(|payload| self.stream_item_message(payload))
            .collect();
        let permits = self
            .tx
            .try_reserve_many(messages.len())
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => ServerStreamSnapshotSendError::Full,
                mpsc::error::TrySendError::Closed(_) => ServerStreamSnapshotSendError::Closed,
            })?;
        for (permit, message) in permits.zip(messages) {
            permit.send(message);
        }
        Ok(())
    }

    async fn send_frame_body(&self, body: FrameBody) -> Result<(), ServerStreamSendError> {
        let _guard = self.send_gate.lock().await;
        self.send_frame_body_unlocked(body).await
    }

    async fn send_frame_body_unlocked(&self, body: FrameBody) -> Result<(), ServerStreamSendError> {
        self.tx
            .send(Message::Frame(Frame {
                src: self.src.clone(),
                dst: self.dst.clone(),
                call_id: self.call_id.clone(),
                body,
            }))
            .await
            .map_err(|_| ServerStreamSendError::Closed)
    }

    fn stream_item_message(&self, payload: Vec<u8>) -> Message {
        Message::Frame(Frame {
            src: self.src.clone(),
            dst: self.dst.clone(),
            call_id: self.call_id.clone(),
            body: FrameBody::StreamItem(payload),
        })
    }
}

impl<C> Clone for TypedServerStreamSink<C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _codec: PhantomData,
        }
    }
}

impl<C: ServerStreamEncoder> TypedServerStreamSink<C> {
    pub(crate) async fn send_item_if_current<F, Fut>(
        &self,
        item: C::Item,
        is_current: F,
    ) -> Result<bool, ServerStreamSendError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = bool>,
    {
        self.inner
            .send_stream_item_if_current(C::encode_item(&item), is_current)
            .await
    }
}

impl fmt::Display for ServerStreamSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "server stream output channel is closed"),
        }
    }
}

impl std::error::Error for ServerStreamSendError {}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use super::*;
    use crate::protocol::link::Link;

    fn route(link: &str) -> Route {
        Route::from_link(Link::new(link).unwrap())
    }

    fn call_id(n: u128) -> CallId {
        CallId::from(Uuid::from_u128(n))
    }

    fn sink(tx: mpsc::Sender<Message>) -> ServerStreamSink {
        ServerStreamSink::new(tx, route("server"), route("client"), call_id(42))
    }

    fn frame_body_from_message(message: Message) -> FrameBody {
        let Message::Frame(Frame {
            src,
            dst,
            call_id: frame_call_id,
            body,
        }) = message
        else {
            panic!("expected application frame message");
        };
        assert_eq!(src, route("server"));
        assert_eq!(dst, route("client"));
        assert_eq!(frame_call_id, call_id(42));
        body
    }

    #[derive(Debug, PartialEq, Eq)]
    enum TestStreamItem {
        Bytes(Vec<u8>),
    }

    struct TestStreamEncoder;

    impl ServerStreamEncoder for TestStreamEncoder {
        type Item = TestStreamItem;

        fn encode_item(item: &Self::Item) -> Vec<u8> {
            match item {
                TestStreamItem::Bytes(bytes) => bytes.clone(),
            }
        }
    }

    #[tokio::test]
    async fn sends_stream_item_when_call_is_current() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = sink(tx);

        let sent = sink
            .send_stream_item_if_current(b"hello".to_vec(), || async { true })
            .await
            .unwrap();

        assert!(sent);
        assert_eq!(
            frame_body_from_message(rx.recv().await.unwrap()),
            FrameBody::StreamItem(b"hello".to_vec())
        );
    }

    #[tokio::test]
    async fn typed_sink_encodes_stream_items() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = sink(tx).encode_with::<TestStreamEncoder>();

        let sent = sink
            .send_item_if_current(TestStreamItem::Bytes(b"hello".to_vec()), || async { true })
            .await
            .unwrap();

        assert!(sent);
        assert_eq!(
            frame_body_from_message(rx.recv().await.unwrap()),
            FrameBody::StreamItem(b"hello".to_vec())
        );
    }

    #[tokio::test]
    async fn skips_stream_item_when_call_is_stale() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = sink(tx);

        let sent = sink
            .send_stream_item_if_current(b"hello".to_vec(), || async { false })
            .await
            .unwrap();

        assert!(!sent);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn sends_terminal_response() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = sink(tx);

        sink.send_empty_response_result(Err(ProtocolError::Cancelled {
            message: "cancelled".to_string(),
        }))
        .await
        .unwrap();

        assert_eq!(
            frame_body_from_message(rx.recv().await.unwrap()),
            FrameBody::Response(ResponseFrame::Error(ProtocolError::Cancelled {
                message: "cancelled".to_string(),
            }))
        );
    }

    #[test]
    fn snapshot_send_is_all_or_nothing_when_channel_is_full() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = sink(tx);
        sink.try_send_snapshot([b"first".to_vec()]).unwrap();

        let error = sink.try_send_snapshot([b"second".to_vec()]).unwrap_err();

        assert_eq!(error, ServerStreamSnapshotSendError::Full);
        assert_eq!(
            frame_body_from_message(rx.try_recv().unwrap()),
            FrameBody::StreamItem(b"first".to_vec())
        );
        assert!(rx.try_recv().is_err());
    }
}

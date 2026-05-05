use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::RpcClientError;
use crate::client::Connection;
use crate::protocol::link::Link;
use crate::protocol::message::{
    FrameBody, LocalFrame, Message, RequestFrame, ResponseFrame, RoutedCallId, RoutedFrame,
    RoutedFrameMessage,
};
use crate::protocol::{Route, method, wire};
use crate::rpc::{
    OutboundCall, OutboundCallState, RegisterCallError, RpcClientOutboundStart,
    RpcOutboundCallHandle, RpcState,
};

pub(super) struct ClientRuntime {
    connection: Connection,
    state: Arc<Mutex<RpcState>>,
    reader_closed: Arc<AtomicBool>,
    reader_task: Arc<RpcClientReaderTask>,
}

impl ClientRuntime {
    pub(super) fn new(connection: Connection) -> Self {
        let state = Arc::new(Mutex::new(RpcState::new()));
        let reader_closed = Arc::new(AtomicBool::new(false));
        let reader_task = Arc::new(spawn_client_reader(
            connection.clone_for_reader(),
            state.clone(),
            reader_closed.clone(),
        ));
        Self {
            connection,
            state,
            reader_closed,
            reader_task,
        }
    }

    pub(super) fn link(&self) -> &Link {
        self.connection.link()
    }

    pub(super) async fn open_routed_stream(
        &self,
        spec: method::MethodSpec,
        full_route: Route,
        payload: Vec<u8>,
    ) -> Result<OutboundRoutedStream, RpcClientError> {
        let call_id = RoutedCallId::from(Uuid::new_v4());
        let outbound = register_outbound(
            &self.state,
            &self.reader_closed,
            spec,
            call_id.clone(),
            full_route.clone(),
            OutboundCallState::AwaitingResponse,
        )?;
        let stream = OutboundRoutedStream::new(
            self.connection.clone_for_reader(),
            outbound,
            self.reader_task.clone(),
        );

        stream.send_request_payload(payload).await?;
        Ok(stream)
    }

    pub(super) async fn routed_unary_payload(
        &self,
        spec: method::MethodSpec,
        full_route: Route,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, RpcClientError> {
        let call_id = RoutedCallId::from(Uuid::new_v4());
        let message = routed_payload_message(full_route.clone(), call_id.clone(), payload)?;
        let mut outbound = register_outbound(
            &self.state,
            &self.reader_closed,
            spec,
            call_id.clone(),
            full_route.clone(),
            OutboundCallState::AwaitingResponse,
        )?;
        if let Err(error) = self.connection.send(&message).await {
            outbound.call.remove();
            return Err(error.into());
        }

        loop {
            match recv_message_or_remove(&mut outbound.rx, &outbound.call).await? {
                Message::Routed(RoutedFrame {
                    src,
                    dst,
                    call_id: response_call_id,
                    message: RoutedFrameMessage::Payload(payload),
                    ..
                }) if routed_response_matches_call(
                    &src,
                    &dst,
                    &response_call_id,
                    outbound.call.handle(),
                ) =>
                {
                    outbound.call.remove();
                    return Ok(payload);
                }
                Message::Routed(RoutedFrame {
                    dst,
                    call_id: response_call_id,
                    message:
                        RoutedFrameMessage::RoutingError {
                            failed_route,
                            error,
                        },
                    ..
                }) if routed_error_matches_call(
                    &dst,
                    &response_call_id,
                    &failed_route,
                    outbound.call.handle(),
                ) =>
                {
                    outbound.call.remove();
                    return Err(error.into());
                }
                Message::GoAway(goaway) => {
                    outbound.call.remove();
                    return Err(RpcClientError::ServerShutdown(goaway.reason));
                }
                Message::Local(_)
                | Message::Peer(_)
                | Message::Routed(_)
                | Message::Ping
                | Message::Pong
                | Message::Reauth(_)
                | Message::ReauthResponse(_)
                | Message::PeerSnapshot { .. } => {}
            }
        }
    }

    pub(super) async fn local_send_only(
        &self,
        method: &'static str,
        payload: Vec<u8>,
    ) -> Result<(), RpcClientError> {
        let call_id = RoutedCallId::from(Uuid::new_v4());
        self.connection
            .send(&Message::Local(LocalFrame {
                call_id,
                body: FrameBody::Request(RequestFrame {
                    method: method.to_string(),
                    payload,
                }),
            }))
            .await?;
        Ok(())
    }

    pub(super) async fn local_unary_payload(
        &self,
        spec: method::MethodSpec,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, RpcClientError> {
        match self.local_unary_response(spec, payload).await? {
            ResponseFrame::Payload(payload) => Ok(payload),
            ResponseFrame::Error(error) => Err(error.into()),
        }
    }

    async fn local_unary_response(
        &self,
        spec: method::MethodSpec,
        payload: Vec<u8>,
    ) -> Result<ResponseFrame, RpcClientError> {
        let call_id = RoutedCallId::from(Uuid::new_v4());
        let counterparty_route = Route::from_link(self.connection.link().clone());
        let mut outbound = register_outbound(
            &self.state,
            &self.reader_closed,
            spec,
            call_id.clone(),
            counterparty_route,
            OutboundCallState::AwaitingResponse,
        )?;
        if let Err(error) = self
            .connection
            .send(&Message::Local(LocalFrame {
                call_id: call_id.clone(),
                body: FrameBody::Request(RequestFrame {
                    method: spec.name.to_string(),
                    payload,
                }),
            }))
            .await
        {
            outbound.call.remove();
            return Err(error.into());
        }

        loop {
            match recv_message_or_remove(&mut outbound.rx, &outbound.call).await? {
                Message::Local(LocalFrame {
                    call_id: response_call_id,
                    body: FrameBody::Response(response),
                }) if response_call_id == call_id => {
                    outbound.call.remove();
                    return Ok(response);
                }
                Message::Local(LocalFrame {
                    call_id: response_call_id,
                    body,
                }) if response_call_id == call_id => {
                    outbound.call.remove();
                    return Err(RpcClientError::Unexpected {
                        method: spec.name,
                        message: format!("expected response frame, got {body:?}"),
                    });
                }
                Message::GoAway(goaway) => {
                    outbound.call.remove();
                    return Err(RpcClientError::ServerShutdown(goaway.reason));
                }
                Message::Local(_)
                | Message::Peer(_)
                | Message::Routed(_)
                | Message::Ping
                | Message::Pong
                | Message::Reauth(_)
                | Message::ReauthResponse(_)
                | Message::PeerSnapshot { .. } => {}
            }
        }
    }
}

struct RpcClientReaderTask {
    task: JoinHandle<()>,
    closed: Arc<AtomicBool>,
}

impl Drop for RpcClientReaderTask {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        self.task.abort();
    }
}

struct RegisteredOutboundCall {
    call: OutboundCallGuard,
    rx: mpsc::Receiver<Message>,
}

struct OutboundRoutedSink {
    connection: Connection,
    call: RpcOutboundCallHandle,
}

impl OutboundRoutedSink {
    fn new(connection: Connection, call: &OutboundCallGuard) -> Self {
        Self {
            connection,
            call: call.handle().clone(),
        }
    }

    async fn send_payload(&self, payload: Vec<u8>) -> Result<(), RpcClientError> {
        self.connection
            .send(&routed_payload_message(
                self.call.counterparty_route.clone(),
                self.call.call_id.clone(),
                payload,
            )?)
            .await
            .map_err(Into::into)
    }
}

pub(super) struct OutboundRoutedStream {
    call: OutboundCallGuard,
    sink: OutboundRoutedSink,
    rx: AsyncMutex<mpsc::Receiver<Message>>,
    _reader_task: Arc<RpcClientReaderTask>,
}

impl OutboundRoutedStream {
    fn new(
        connection: Connection,
        outbound: RegisteredOutboundCall,
        reader_task: Arc<RpcClientReaderTask>,
    ) -> Self {
        let sink = OutboundRoutedSink::new(connection, &outbound.call);
        Self {
            call: outbound.call,
            sink,
            rx: AsyncMutex::new(outbound.rx),
            _reader_task: reader_task,
        }
    }

    pub(super) fn set_active(&self) {
        self.call.set_state(OutboundCallState::ActiveStream);
    }

    pub(super) fn finish(&self) {
        self.call.remove();
    }

    async fn send_request_payload(&self, payload: Vec<u8>) -> Result<(), RpcClientError> {
        self.send_payload_or_remove(payload).await
    }

    pub(super) async fn send_stream_payload(
        &self,
        payload: Vec<u8>,
        method: &'static str,
    ) -> Result<(), RpcClientError> {
        require_outbound_state(&self.call, OutboundCallState::ActiveStream, method)?;
        self.send_payload_or_remove(payload).await
    }

    pub(super) async fn send_cancel_payload(
        &self,
        payload: Vec<u8>,
        method: &'static str,
    ) -> Result<(), RpcClientError> {
        if !self.call.set_state_if(
            |state| state == OutboundCallState::ActiveStream,
            OutboundCallState::Closing,
        ) {
            return Err(outbound_call_not_active_error(method));
        }
        self.send_payload_or_remove(payload).await
    }

    pub(super) async fn recv_frame_body(&self) -> Result<FrameBody, RpcClientError> {
        loop {
            match self.recv_message().await? {
                Message::Routed(RoutedFrame {
                    src,
                    dst,
                    call_id,
                    message: RoutedFrameMessage::Payload(payload),
                }) if routed_response_matches_call(&src, &dst, &call_id, self.call.handle()) => {
                    match wire::decode_frame_body(&payload) {
                        Ok(body) => return Ok(body),
                        Err(error) => {
                            self.finish();
                            return Err(RpcClientError::Decode {
                                method: self.call.handle().method.name,
                                message: error.to_string(),
                            });
                        }
                    }
                }
                Message::Routed(RoutedFrame {
                    dst,
                    call_id,
                    message:
                        RoutedFrameMessage::RoutingError {
                            failed_route,
                            error,
                        },
                    ..
                }) if routed_error_matches_call(
                    &dst,
                    &call_id,
                    &failed_route,
                    self.call.handle(),
                ) =>
                {
                    self.finish();
                    return Err(error.into());
                }
                Message::GoAway(goaway) => {
                    self.finish();
                    return Err(RpcClientError::ServerShutdown(goaway.reason));
                }
                Message::Local(_)
                | Message::Peer(_)
                | Message::Routed(_)
                | Message::Ping
                | Message::Pong
                | Message::Reauth(_)
                | Message::ReauthResponse(_)
                | Message::PeerSnapshot { .. } => {}
            }
        }
    }

    async fn recv_message(&self) -> Result<Message, RpcClientError> {
        let mut rx = self.rx.lock().await;
        recv_message_or_remove(&mut rx, &self.call).await
    }

    async fn send_payload_or_remove(&self, payload: Vec<u8>) -> Result<(), RpcClientError> {
        match self.sink.send_payload(payload).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.call.remove();
                Err(error)
            }
        }
    }
}

struct OutboundCallGuard {
    state: Arc<Mutex<RpcState>>,
    handle: RpcOutboundCallHandle,
    removed: AtomicBool,
}

impl OutboundCallGuard {
    fn new(state: Arc<Mutex<RpcState>>, handle: RpcOutboundCallHandle) -> Self {
        Self {
            state,
            handle,
            removed: AtomicBool::new(false),
        }
    }

    fn handle(&self) -> &RpcOutboundCallHandle {
        &self.handle
    }

    fn state(&self) -> Option<OutboundCallState> {
        lock_rpc_state(&self.state).outbound_state_for_handle(&self.handle)
    }

    fn set_state(&self, state: OutboundCallState) -> bool {
        lock_rpc_state(&self.state).set_outbound_state_for_handle(&self.handle, state)
    }

    fn set_state_if(
        &self,
        predicate: impl FnOnce(OutboundCallState) -> bool,
        state: OutboundCallState,
    ) -> bool {
        lock_rpc_state(&self.state).set_outbound_state_for_handle_if(&self.handle, predicate, state)
    }

    fn remove(&self) -> Option<OutboundCall> {
        if self.removed.swap(true, Ordering::AcqRel) {
            return None;
        }
        lock_rpc_state(&self.state).remove_outbound_for_handle(&self.handle)
    }
}

impl Drop for OutboundCallGuard {
    fn drop(&mut self) {
        if self.removed.swap(true, Ordering::AcqRel) {
            return;
        }
        lock_rpc_state(&self.state).remove_outbound_for_handle(&self.handle);
    }
}

fn register_outbound(
    state: &Arc<Mutex<RpcState>>,
    reader_closed: &AtomicBool,
    method: method::MethodSpec,
    call_id: RoutedCallId,
    counterparty_route: Route,
    call_state: OutboundCallState,
) -> Result<RegisteredOutboundCall, RpcClientError> {
    let mut rpc_state = lock_rpc_state(state);
    if reader_closed.load(Ordering::Acquire) {
        return Err(RpcClientError::Unexpected {
            method: method.name,
            message: "RPC reader is closed".to_string(),
        });
    }
    let (tx, rx) = mpsc::channel(16);
    let handle = rpc_state
        .register_client_outbound(RpcClientOutboundStart {
            call_id,
            counterparty_route,
            method,
            state: call_state,
            inbox_tx: tx,
        })
        .map_err(|error| call_state_error(method.name, error))?;
    Ok(RegisteredOutboundCall {
        call: OutboundCallGuard::new(state.clone(), handle),
        rx,
    })
}

fn require_outbound_state(
    call: &OutboundCallGuard,
    expected: OutboundCallState,
    method: &'static str,
) -> Result<(), RpcClientError> {
    match call.state() {
        Some(actual) if actual == expected => Ok(()),
        _ => Err(outbound_call_not_active_error(method)),
    }
}

async fn recv_message_or_remove(
    rx: &mut mpsc::Receiver<Message>,
    call: &OutboundCallGuard,
) -> Result<Message, RpcClientError> {
    match rx.recv().await {
        Some(message) => Ok(message),
        None => {
            call.remove();
            Err(RpcClientError::Unexpected {
                method: call.handle().method.name,
                message: "RPC reader closed before response".to_string(),
            })
        }
    }
}

fn lock_rpc_state(state: &Arc<Mutex<RpcState>>) -> std::sync::MutexGuard<'_, RpcState> {
    state.lock().expect("RPC client state mutex poisoned")
}

fn spawn_client_reader(
    connection: Connection,
    state: Arc<Mutex<RpcState>>,
    closed: Arc<AtomicBool>,
) -> RpcClientReaderTask {
    let task_closed = closed.clone();
    RpcClientReaderTask {
        task: tokio::spawn(async move {
            loop {
                let message = match connection.recv().await {
                    Ok(message) => message,
                    Err(error) => {
                        tracing::debug!(error = %error, "RPC client reader stopped");
                        task_closed.store(true, Ordering::Release);
                        lock_rpc_state(&state).remove_client_inboxes();
                        break;
                    }
                };

                if matches!(message, Message::GoAway(_)) {
                    task_closed.store(true, Ordering::Release);
                    let inboxes = lock_rpc_state(&state).remove_client_inboxes();
                    for tx in inboxes {
                        let _ = tx.send(message.clone()).await;
                    }
                    break;
                }

                let inbox = lock_rpc_state(&state).client_inbox_for_message(&message);
                if let Some(tx) = inbox {
                    let _ = tx.send(message).await;
                }
            }
        }),
        closed,
    }
}

fn routed_response_matches_call(
    src: &Route,
    dst: &Route,
    call_id: &RoutedCallId,
    call: &RpcOutboundCallHandle,
) -> bool {
    call_id == &call.call_id && dst.is_empty() && src == &call.counterparty_route
}

fn routed_error_matches_call(
    dst: &Route,
    call_id: &RoutedCallId,
    failed_route: &Route,
    call: &RpcOutboundCallHandle,
) -> bool {
    call_id == &call.call_id && dst.is_empty() && failed_route == &call.counterparty_route
}

fn call_state_error(method: &'static str, error: RegisterCallError) -> RpcClientError {
    RpcClientError::Unexpected {
        method,
        message: format!("RPC call state error: {error:?}"),
    }
}

fn outbound_call_not_active_error(method: &'static str) -> RpcClientError {
    RpcClientError::Unexpected {
        method,
        message: "RPC call is not active".to_string(),
    }
}

fn routed_payload_message(
    full_route: Route,
    call_id: RoutedCallId,
    payload: Vec<u8>,
) -> Result<Message, RpcClientError> {
    let (src, dst) = Route::send(full_route).ok_or_else(|| RpcClientError::Unexpected {
        method: "routed request",
        message: "agent route did not include the local connection link".to_string(),
    })?;
    Ok(Message::Routed(RoutedFrame {
        src,
        dst,
        call_id,
        message: RoutedFrameMessage::Payload(payload),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(links: &[&str]) -> Route {
        Route::from_links(links.iter().map(|link| (*link).to_string())).unwrap()
    }

    fn call_id(n: u128) -> RoutedCallId {
        RoutedCallId::from(Uuid::from_u128(n))
    }

    #[test]
    fn outbound_call_guard_removes_call_on_drop() {
        let state = Arc::new(Mutex::new(RpcState::new()));
        let reader_closed = AtomicBool::new(false);
        {
            let _guard = register_outbound(
                &state,
                &reader_closed,
                method::AGENT_LIST,
                call_id(1),
                Route::from_link(Link::new("local").unwrap()),
                OutboundCallState::AwaitingResponse,
            )
            .unwrap();

            assert_eq!(lock_rpc_state(&state).outbound_len(), 1);
        }

        assert_eq!(lock_rpc_state(&state).outbound_len(), 0);
    }

    #[test]
    fn registering_after_reader_closed_fails_without_call_state() {
        let state = Arc::new(Mutex::new(RpcState::new()));
        let reader_closed = AtomicBool::new(true);

        let result = register_outbound(
            &state,
            &reader_closed,
            method::AGENT_LIST,
            call_id(1),
            Route::from_link(Link::new("local").unwrap()),
            OutboundCallState::AwaitingResponse,
        );

        assert!(matches!(result, Err(RpcClientError::Unexpected { .. })));
        assert_eq!(lock_rpc_state(&state).outbound_len(), 0);
    }

    #[test]
    fn routed_response_matching_uses_route_scoped_call_identity() {
        let handle = RpcOutboundCallHandle {
            counterparty_route: route(&["local", "peer"]),
            call_id: call_id(1),
            method: method::AGENT_OPEN_SESSION,
        };

        assert!(routed_response_matches_call(
            &route(&["local", "peer"]),
            &Route::empty(),
            &call_id(1),
            &handle
        ));
        assert!(!routed_response_matches_call(
            &route(&["local", "other"]),
            &Route::empty(),
            &call_id(1),
            &handle
        ));
        assert!(!routed_response_matches_call(
            &route(&["local", "peer"]),
            &Route::from_link(Link::new("next").unwrap()),
            &call_id(1),
            &handle
        ));
        assert!(routed_error_matches_call(
            &Route::empty(),
            &call_id(1),
            &route(&["local", "peer"]),
            &handle
        ));
        assert!(!routed_error_matches_call(
            &Route::empty(),
            &call_id(1),
            &route(&["local", "other"]),
            &handle
        ));
    }
}

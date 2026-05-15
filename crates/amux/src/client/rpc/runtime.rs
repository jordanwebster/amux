use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::ClientError;
use crate::client::Connection;
use crate::protocol::link::Link;
use crate::protocol::message::{CallId, Frame, FrameBody, Message, RequestFrame, ResponseFrame};
use crate::protocol::{Route, method};
use crate::rpc::{
    OutboundCall, OutboundCallState, RegisterCallError, RpcOutboundCallHandle, RpcOutboundStart,
    RpcState,
};

#[derive(Clone)]
pub(super) struct ClientRuntime {
    connection: Connection,
    state: Arc<Mutex<RpcState>>,
    inboxes: Arc<Mutex<HashMap<CallId, mpsc::Sender<Message>>>>,
    reader_closed: Arc<AtomicBool>,
    reader_task: Arc<ClientReaderTask>,
}

impl ClientRuntime {
    pub(super) fn new(connection: Connection) -> Self {
        let state = Arc::new(Mutex::new(RpcState::new()));
        let inboxes = Arc::new(Mutex::new(HashMap::new()));
        let reader_closed = Arc::new(AtomicBool::new(false));
        let reader_task = Arc::new(spawn_client_reader(
            connection.clone_for_reader(),
            state.clone(),
            inboxes.clone(),
            reader_closed.clone(),
        ));
        Self {
            connection,
            state,
            inboxes,
            reader_closed,
            reader_task,
        }
    }

    pub(super) fn link(&self) -> &Link {
        self.connection.link()
    }

    pub(super) async fn start_endpoint_stream(
        &self,
        spec: method::MethodSpec,
        full_route: Route,
        request_payload: Vec<u8>,
    ) -> Result<EndpointOutputStream, ClientError> {
        let call_id = CallId::from(Uuid::new_v4());
        let outbound = register_outbound(
            &self.state,
            &self.inboxes,
            &self.reader_closed,
            spec,
            call_id.clone(),
            OutboundCallState::AwaitingResponse,
        )?;
        let stream = EndpointOutputStream::new(
            self.connection.clone_for_reader(),
            outbound,
            full_route,
            self.reader_task.clone(),
        );

        stream.send_request_payload(request_payload).await?;
        Ok(stream)
    }

    pub(super) async fn call_endpoint_unary_payload(
        &self,
        spec: method::MethodSpec,
        full_route: Route,
        request_payload: Vec<u8>,
    ) -> Result<Vec<u8>, ClientError> {
        match self
            .call_endpoint_unary(spec, full_route, request_payload)
            .await?
        {
            ResponseFrame::Payload(payload) => Ok(payload),
            ResponseFrame::Error(error) => Err(error.into()),
        }
    }

    pub(super) async fn call_endpoint_unary(
        &self,
        spec: method::MethodSpec,
        full_route: Route,
        request_payload: Vec<u8>,
    ) -> Result<ResponseFrame, ClientError> {
        let call_id = CallId::from(Uuid::new_v4());
        let message =
            endpoint_request_message(spec, full_route.clone(), call_id.clone(), request_payload)?;
        let mut outbound = register_outbound(
            &self.state,
            &self.inboxes,
            &self.reader_closed,
            spec,
            call_id.clone(),
            OutboundCallState::AwaitingResponse,
        )?;
        if let Err(error) = self.connection.send(&message).await {
            outbound.call.remove();
            return Err(error.into());
        }

        loop {
            match recv_message_or_remove(&mut outbound.rx, &outbound.call).await? {
                Message::Frame(Frame {
                    dst,
                    call_id: response_call_id,
                    body: FrameBody::Response(response),
                    ..
                }) if endpoint_frame_matches_call(
                    &dst,
                    &response_call_id,
                    outbound.call.handle(),
                ) =>
                {
                    outbound.call.remove();
                    return Ok(response);
                }
                Message::Frame(Frame {
                    dst,
                    call_id: response_call_id,
                    body: FrameBody::RoutingError { error, .. },
                    ..
                }) if endpoint_frame_matches_call(
                    &dst,
                    &response_call_id,
                    outbound.call.handle(),
                ) =>
                {
                    outbound.call.remove();
                    return Err(error.into());
                }
                Message::GoAway(goaway) => {
                    outbound.call.remove();
                    return Err(ClientError::ServerShutdown(goaway.reason));
                }
                Message::Frame(Frame {
                    dst,
                    call_id: response_call_id,
                    body,
                    ..
                }) if endpoint_frame_matches_call(
                    &dst,
                    &response_call_id,
                    outbound.call.handle(),
                ) =>
                {
                    outbound.call.remove();
                    return Err(ClientError::Unexpected {
                        method: spec.name,
                        message: format!("expected endpoint response frame, got {body:?}"),
                    });
                }
                Message::Frame(_)
                | Message::Ping
                | Message::Pong
                | Message::Reauth(_)
                | Message::ReauthResponse(_) => {}
            }
        }
    }

    pub(super) async fn local_unary_payload(
        &self,
        spec: method::MethodSpec,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, ClientError> {
        match self.local_unary_response(spec, payload).await? {
            ResponseFrame::Payload(payload) => Ok(payload),
            ResponseFrame::Error(error) => Err(error.into()),
        }
    }

    async fn local_unary_response(
        &self,
        spec: method::MethodSpec,
        payload: Vec<u8>,
    ) -> Result<ResponseFrame, ClientError> {
        let call_id = CallId::from(Uuid::new_v4());
        let mut outbound = register_outbound(
            &self.state,
            &self.inboxes,
            &self.reader_closed,
            spec,
            call_id.clone(),
            OutboundCallState::AwaitingResponse,
        )?;
        if let Err(error) = self
            .connection
            .send(&Message::Frame(Frame {
                src: Route::from_link(self.connection.link().clone()),
                dst: Route::empty(),
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
                Message::Frame(Frame {
                    dst,
                    call_id: response_call_id,
                    body: FrameBody::Response(response),
                    ..
                }) if dst.is_empty() && response_call_id == call_id => {
                    outbound.call.remove();
                    return Ok(response);
                }
                Message::Frame(Frame {
                    dst,
                    call_id: response_call_id,
                    body,
                    ..
                }) if dst.is_empty() && response_call_id == call_id => {
                    outbound.call.remove();
                    return Err(ClientError::Unexpected {
                        method: spec.name,
                        message: format!("expected response frame, got {body:?}"),
                    });
                }
                Message::GoAway(goaway) => {
                    outbound.call.remove();
                    return Err(ClientError::ServerShutdown(goaway.reason));
                }
                Message::Frame(_)
                | Message::Ping
                | Message::Pong
                | Message::Reauth(_)
                | Message::ReauthResponse(_) => {}
            }
        }
    }
}

struct ClientReaderTask {
    task: JoinHandle<()>,
    closed: Arc<AtomicBool>,
}

impl Drop for ClientReaderTask {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        self.task.abort();
    }
}

struct RegisteredOutboundCall {
    call: OutboundCallGuard,
    rx: mpsc::Receiver<Message>,
}

struct EndpointFrameSink {
    connection: Connection,
    call: RpcOutboundCallHandle,
    full_route: Route,
}

impl EndpointFrameSink {
    fn new(connection: Connection, call: &OutboundCallGuard, full_route: Route) -> Self {
        Self {
            connection,
            call: call.handle().clone(),
            full_route,
        }
    }

    async fn send_request_payload(&self, payload: Vec<u8>) -> Result<(), ClientError> {
        self.send_frame_body(FrameBody::Request(RequestFrame {
            method: self.call.method.name.to_string(),
            payload,
        }))
        .await
    }

    async fn send_frame_body(&self, body: FrameBody) -> Result<(), ClientError> {
        let (src, dst) =
            Route::send(self.full_route.clone()).ok_or_else(|| ClientError::Unexpected {
                method: self.call.method.name,
                message: "agent route did not include the local connection link".to_string(),
            })?;
        self.connection
            .send(&Message::Frame(Frame {
                src,
                dst,
                call_id: self.call.call_id.clone(),
                body,
            }))
            .await
            .map_err(Into::into)
    }
}

pub(super) struct EndpointOutputStream {
    call: OutboundCallGuard,
    sink: EndpointFrameSink,
    rx: AsyncMutex<mpsc::Receiver<Message>>,
    _reader_task: Arc<ClientReaderTask>,
}

impl EndpointOutputStream {
    fn new(
        connection: Connection,
        outbound: RegisteredOutboundCall,
        full_route: Route,
        reader_task: Arc<ClientReaderTask>,
    ) -> Self {
        let sink = EndpointFrameSink::new(connection, &outbound.call, full_route);
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

    async fn send_request_payload(&self, payload: Vec<u8>) -> Result<(), ClientError> {
        self.send_request_payload_or_remove(payload).await
    }

    pub(super) async fn send_cancel(&self, method: &'static str) -> Result<(), ClientError> {
        if !self.call.set_state_if(
            |state| {
                state == OutboundCallState::AwaitingResponse
                    || state == OutboundCallState::ActiveStream
            },
            OutboundCallState::Closing,
        ) {
            return Err(outbound_call_not_active_error(method));
        }
        match self.sink.send_frame_body(FrameBody::Cancel).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.call.remove();
                Err(error)
            }
        }
    }

    pub(super) async fn recv_frame_body(&self) -> Result<FrameBody, ClientError> {
        loop {
            match self.recv_message().await? {
                Message::Frame(Frame {
                    dst, call_id, body, ..
                }) if endpoint_frame_matches_call(&dst, &call_id, self.call.handle()) => match body
                {
                    FrameBody::RoutingError { error, .. } => {
                        self.finish();
                        return Err(error.into());
                    }
                    body => return Ok(body),
                },
                Message::GoAway(goaway) => {
                    self.finish();
                    return Err(ClientError::ServerShutdown(goaway.reason));
                }
                Message::Frame(_)
                | Message::Ping
                | Message::Pong
                | Message::Reauth(_)
                | Message::ReauthResponse(_) => {}
            }
        }
    }

    async fn recv_message(&self) -> Result<Message, ClientError> {
        let mut rx = self.rx.lock().await;
        recv_message_or_remove(&mut rx, &self.call).await
    }

    async fn send_request_payload_or_remove(&self, payload: Vec<u8>) -> Result<(), ClientError> {
        match self.sink.send_request_payload(payload).await {
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
    inboxes: Arc<Mutex<HashMap<CallId, mpsc::Sender<Message>>>>,
    handle: RpcOutboundCallHandle,
    removed: AtomicBool,
}

impl OutboundCallGuard {
    fn new(
        state: Arc<Mutex<RpcState>>,
        inboxes: Arc<Mutex<HashMap<CallId, mpsc::Sender<Message>>>>,
        handle: RpcOutboundCallHandle,
    ) -> Self {
        Self {
            state,
            inboxes,
            handle,
            removed: AtomicBool::new(false),
        }
    }

    fn handle(&self) -> &RpcOutboundCallHandle {
        &self.handle
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
        lock_client_inboxes(&self.inboxes).remove(&self.handle.call_id);
        lock_rpc_state(&self.state).remove_outbound_for_handle(&self.handle)
    }
}

impl Drop for OutboundCallGuard {
    fn drop(&mut self) {
        if self.removed.swap(true, Ordering::AcqRel) {
            return;
        }
        lock_client_inboxes(&self.inboxes).remove(&self.handle.call_id);
        lock_rpc_state(&self.state).remove_outbound_for_handle(&self.handle);
    }
}

fn register_outbound(
    state: &Arc<Mutex<RpcState>>,
    inboxes: &Arc<Mutex<HashMap<CallId, mpsc::Sender<Message>>>>,
    reader_closed: &AtomicBool,
    method: method::MethodSpec,
    call_id: CallId,
    call_state: OutboundCallState,
) -> Result<RegisteredOutboundCall, ClientError> {
    let (tx, rx) = mpsc::channel(16);
    let mut inbox_guard = lock_client_inboxes(inboxes);
    if reader_closed.load(Ordering::Acquire) {
        return Err(ClientError::Unexpected {
            method: method.name,
            message: "RPC reader is closed".to_string(),
        });
    }

    let start = RpcOutboundStart {
        call_id: call_id.clone(),
        method,
        state: call_state,
    };
    let mut rpc_state = lock_rpc_state(state);
    let handle = match method.kind {
        method::MethodKind::Unary => rpc_state.register_outbound(start),
        method::MethodKind::ServerStreaming => rpc_state.register_outbound_stream(start),
    }
    .map_err(|error| call_state_error(method.name, error))?;
    inbox_guard.insert(call_id, tx);
    Ok(RegisteredOutboundCall {
        call: OutboundCallGuard::new(state.clone(), inboxes.clone(), handle),
        rx,
    })
}

async fn recv_message_or_remove(
    rx: &mut mpsc::Receiver<Message>,
    call: &OutboundCallGuard,
) -> Result<Message, ClientError> {
    match rx.recv().await {
        Some(message) => Ok(message),
        None => {
            call.remove();
            Err(ClientError::Unexpected {
                method: call.handle().method.name,
                message: "RPC reader closed before response".to_string(),
            })
        }
    }
}

fn lock_rpc_state(state: &Arc<Mutex<RpcState>>) -> std::sync::MutexGuard<'_, RpcState> {
    state.lock().expect("RPC client state mutex poisoned")
}

fn lock_client_inboxes(
    inboxes: &Arc<Mutex<HashMap<CallId, mpsc::Sender<Message>>>>,
) -> std::sync::MutexGuard<'_, HashMap<CallId, mpsc::Sender<Message>>> {
    inboxes.lock().expect("RPC client inbox mutex poisoned")
}

fn client_inbox_for_message(
    inboxes: &Arc<Mutex<HashMap<CallId, mpsc::Sender<Message>>>>,
    message: &Message,
) -> Option<mpsc::Sender<Message>> {
    match message {
        Message::Frame(frame) if frame.dst.is_empty() => {
            lock_client_inboxes(inboxes).get(&frame.call_id).cloned()
        }
        Message::Frame(_)
        | Message::Ping
        | Message::Pong
        | Message::Reauth(_)
        | Message::ReauthResponse(_)
        | Message::GoAway(_) => None,
    }
}

fn remove_client_inboxes(
    inboxes: &Arc<Mutex<HashMap<CallId, mpsc::Sender<Message>>>>,
) -> Vec<mpsc::Sender<Message>> {
    lock_client_inboxes(inboxes)
        .drain()
        .map(|(_, tx)| tx)
        .collect()
}

fn spawn_client_reader(
    connection: Connection,
    _state: Arc<Mutex<RpcState>>,
    inboxes: Arc<Mutex<HashMap<CallId, mpsc::Sender<Message>>>>,
    closed: Arc<AtomicBool>,
) -> ClientReaderTask {
    let task_closed = closed.clone();
    ClientReaderTask {
        task: tokio::spawn(async move {
            loop {
                let message = match connection.recv().await {
                    Ok(message) => message,
                    Err(error) => {
                        tracing::debug!(error = %error, "RPC client reader stopped");
                        task_closed.store(true, Ordering::Release);
                        remove_client_inboxes(&inboxes);
                        break;
                    }
                };

                if matches!(message, Message::GoAway(_)) {
                    task_closed.store(true, Ordering::Release);
                    for tx in remove_client_inboxes(&inboxes) {
                        let _ = tx.send(message.clone()).await;
                    }
                    break;
                }

                let inbox = client_inbox_for_message(&inboxes, &message);
                if let Some(tx) = inbox {
                    let _ = tx.send(message).await;
                }
            }
        }),
        closed,
    }
}

fn endpoint_frame_matches_call(
    dst: &Route,
    call_id: &CallId,
    call: &RpcOutboundCallHandle,
) -> bool {
    call_id == &call.call_id && dst.is_empty()
}

fn call_state_error(method: &'static str, error: RegisterCallError) -> ClientError {
    ClientError::Unexpected {
        method,
        message: format!("RPC call state error: {error:?}"),
    }
}

fn outbound_call_not_active_error(method: &'static str) -> ClientError {
    ClientError::Unexpected {
        method,
        message: "RPC call is not active".to_string(),
    }
}

fn endpoint_request_message(
    spec: method::MethodSpec,
    full_route: Route,
    call_id: CallId,
    payload: Vec<u8>,
) -> Result<Message, ClientError> {
    let (src, dst) = Route::send(full_route).ok_or_else(|| ClientError::Unexpected {
        method: spec.name,
        message: "agent route did not include the local connection link".to_string(),
    })?;
    Ok(Message::Frame(Frame {
        src,
        dst,
        call_id,
        body: FrameBody::Request(RequestFrame {
            method: spec.name.to_string(),
            payload,
        }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call_id(n: u128) -> CallId {
        CallId::from(Uuid::from_u128(n))
    }

    #[test]
    fn outbound_call_guard_removes_call_on_drop() {
        let state = Arc::new(Mutex::new(RpcState::new()));
        let inboxes = Arc::new(Mutex::new(HashMap::new()));
        let reader_closed = AtomicBool::new(false);
        {
            let _guard = register_outbound(
                &state,
                &inboxes,
                &reader_closed,
                method::AGENT_LIST,
                call_id(1),
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
        let inboxes = Arc::new(Mutex::new(HashMap::new()));
        let reader_closed = AtomicBool::new(true);

        let result = register_outbound(
            &state,
            &inboxes,
            &reader_closed,
            method::AGENT_LIST,
            call_id(1),
            OutboundCallState::AwaitingResponse,
        );

        assert!(matches!(result, Err(ClientError::Unexpected { .. })));
        assert_eq!(lock_rpc_state(&state).outbound_len(), 0);
    }

    #[test]
    fn endpoint_frame_matching_uses_call_id_identity() {
        let handle = RpcOutboundCallHandle {
            call_id: call_id(1),
            method: method::AGENT_SUBSCRIBE_SESSION,
        };

        assert!(endpoint_frame_matches_call(
            &Route::empty(),
            &call_id(1),
            &handle
        ));
        assert!(!endpoint_frame_matches_call(
            &Route::from_link(Link::new("next").unwrap()),
            &call_id(1),
            &handle
        ));
        assert!(endpoint_frame_matches_call(
            &Route::empty(),
            &call_id(1),
            &handle
        ));
    }
}

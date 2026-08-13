use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{Mutex, Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::approval::{ApprovalHandler, ApprovalRequest, RequestId};
use crate::error::Error;
use crate::init::InitializationResult;
use crate::notification::{self, ServerNotification, ThreadEvent, TurnEvent};
use crate::transport::{
    OutgoingErrorResponse, OutgoingNotification, OutgoingRequest, OutgoingResponse, RawMessage,
    RpcError,
};
use crate::types::DynamicToolCallRequest;

// ── ServerInner ──────────────────────────────────────────────────

pub(crate) struct ServerInner {
    pub stdin_tx: mpsc::Sender<Vec<u8>>,
    pub pending_requests: Mutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value, RpcError>>>>,
    pub thread_channels: Mutex<HashMap<String, Weak<ThreadRegistration>>>,
    pub approval_handler: Option<Arc<dyn ApprovalHandler>>,
    pub global_notif_tx: mpsc::Sender<ServerNotification>,
    pub init_result: OnceLock<InitializationResult>,
    pub request_counter: AtomicU64,
    pub cancel: CancellationToken,
    pub child_waiter: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

const THREAD_CHANNEL_CAPACITY: usize = 256;
const THREAD_CHANNEL_OPEN: u8 = 0;
const THREAD_CHANNEL_OVERFLOW: u8 = 1;
const THREAD_CHANNEL_CLOSED: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThreadChannelState {
    Open,
    Overflow,
    Closed,
}

pub(crate) struct ThreadRegistration {
    tx: mpsc::Sender<ThreadEvent>,
    event_rx: Mutex<Option<ThreadEventReceiver>>,
    route: StdMutex<ThreadEventRoute>,
    state: AtomicU8,
    state_changed: Notify,
}

enum ThreadEventRoute {
    Live,
    Staging(VecDeque<ThreadEvent>),
}

pub(crate) struct ThreadEventReceiver {
    staged: VecDeque<ThreadEvent>,
    live: mpsc::Receiver<ThreadEvent>,
}

impl ThreadEventReceiver {
    pub(crate) fn try_recv(&mut self) -> Result<ThreadEvent, mpsc::error::TryRecvError> {
        self.staged
            .pop_front()
            .map_or_else(|| self.live.try_recv(), Ok)
    }

    pub(crate) async fn recv(&mut self) -> Option<ThreadEvent> {
        match self.staged.pop_front() {
            Some(event) => Some(event),
            None => self.live.recv().await,
        }
    }
}

impl ThreadRegistration {
    pub(crate) fn new() -> Arc<Self> {
        Self::new_with_route(ThreadEventRoute::Live)
    }

    fn new_staging() -> Arc<Self> {
        Self::new_with_route(ThreadEventRoute::Staging(VecDeque::new()))
    }

    fn new_with_route(route: ThreadEventRoute) -> Arc<Self> {
        let (tx, event_rx) = mpsc::channel(THREAD_CHANNEL_CAPACITY);
        Arc::new(Self {
            tx,
            event_rx: Mutex::new(Some(ThreadEventReceiver {
                staged: VecDeque::new(),
                live: event_rx,
            })),
            route: StdMutex::new(route),
            state: AtomicU8::new(THREAD_CHANNEL_OPEN),
            state_changed: Notify::new(),
        })
    }

    pub(crate) async fn take_receiver(&self) -> Option<ThreadEventReceiver> {
        self.event_rx.lock().await.take()
    }

    pub(crate) async fn restore_receiver(&self, rx: ThreadEventReceiver) {
        *self.event_rx.lock().await = Some(rx);
    }

    pub(crate) fn try_restore_receiver(
        &self,
        rx: ThreadEventReceiver,
    ) -> Result<(), ThreadEventReceiver> {
        let Ok(mut receiver) = self.event_rx.try_lock() else {
            return Err(rx);
        };
        *receiver = Some(rx);
        Ok(())
    }

    pub(crate) fn state(&self) -> ThreadChannelState {
        match self.state.load(Ordering::Acquire) {
            THREAD_CHANNEL_OPEN => ThreadChannelState::Open,
            THREAD_CHANNEL_OVERFLOW => ThreadChannelState::Overflow,
            _ => ThreadChannelState::Closed,
        }
    }

    pub(crate) fn state_changed(&self) -> &Notify {
        &self.state_changed
    }

    pub(crate) fn send(&self, event: ThreadEvent) -> bool {
        if self.state.load(Ordering::Acquire) != THREAD_CHANNEL_OPEN {
            return false;
        }
        let mut route = self
            .route
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let ThreadEventRoute::Staging(events) = &mut *route {
            events.push_back(event);
            return true;
        }
        match self.tx.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                if self
                    .state
                    .compare_exchange(
                        THREAD_CHANNEL_OPEN,
                        THREAD_CHANNEL_OVERFLOW,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    self.state_changed.notify_waiters();
                }
                false
            }
            Err(TrySendError::Closed(_)) => false,
        }
    }

    pub(crate) async fn finish_staging(&self) {
        let staged = {
            let mut route = self
                .route
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            match std::mem::replace(&mut *route, ThreadEventRoute::Live) {
                ThreadEventRoute::Staging(events) => events,
                ThreadEventRoute::Live => return,
            }
        };
        self.event_rx
            .lock()
            .await
            .as_mut()
            .expect("resume staging receiver was taken before resume completed")
            .staged = staged;
    }

    fn close(&self) {
        if self
            .state
            .compare_exchange(
                THREAD_CHANNEL_OPEN,
                THREAD_CHANNEL_CLOSED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.state_changed.notify_waiters();
        }
    }
}

impl ServerInner {
    /// Send a JSON-RPC request and wait for the response.
    pub async fn request<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, Error> {
        let id = self.request_counter.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending_requests.lock().await.insert(id, tx);

        let msg = OutgoingRequest {
            id,
            method: method.to_owned(),
            params: serde_json::to_value(params)?,
        };
        let json = serde_json::to_vec(&msg)?;

        self.stdin_tx
            .send(json)
            .await
            .map_err(|_| Error::TransportClosed)?;

        let result = rx.await.map_err(|_| Error::TransportClosed)?;

        match result {
            Ok(value) => {
                let parsed = serde_json::from_value(value)?;
                Ok(parsed)
            }
            Err(rpc_err) => Err(Error::Rpc {
                code: rpc_err.code,
                message: rpc_err.message,
                codex_error_info: rpc_err
                    .data
                    .as_ref()
                    .and_then(|d| d.get("codexErrorInfo"))
                    .and_then(|v| v.as_str())
                    .map(String::from),
                data: rpc_err.data,
            }),
        }
    }

    /// Send a JSON-RPC request where the response body is ignored.
    pub async fn request_unit<P: Serialize>(&self, method: &str, params: P) -> Result<(), Error> {
        self.request::<_, serde_json::Value>(method, params)
            .await
            .map(|_| ())
    }

    /// Send a JSON-RPC notification (no response expected).
    #[allow(dead_code)]
    pub async fn notify<P: Serialize>(&self, method: &str, params: P) -> Result<(), Error> {
        let msg = OutgoingNotification {
            method: method.to_owned(),
            params: Some(serde_json::to_value(params)?),
        };
        let json = serde_json::to_vec(&msg)?;
        self.stdin_tx
            .send(json)
            .await
            .map_err(|_| Error::TransportClosed)?;
        Ok(())
    }

    /// Send a JSON-RPC notification without a params field.
    pub async fn notify_no_params(&self, method: &str) -> Result<(), Error> {
        let msg = OutgoingNotification {
            method: method.to_owned(),
            params: None,
        };
        let json = serde_json::to_vec(&msg)?;
        self.stdin_tx
            .send(json)
            .await
            .map_err(|_| Error::TransportClosed)?;
        Ok(())
    }

    /// Send a JSON-RPC response (for server-initiated requests like approvals).
    pub async fn respond(&self, id: RequestId, result: serde_json::Value) -> Result<(), Error> {
        let msg = OutgoingResponse { id, result };
        let json = serde_json::to_vec(&msg)?;
        self.stdin_tx
            .send(json)
            .await
            .map_err(|_| Error::TransportClosed)?;
        Ok(())
    }

    /// Return the live registration for a thread, creating one if needed.
    pub async fn register_thread(&self, thread_id: &str) -> Arc<ThreadRegistration> {
        let mut channels = self.thread_channels.lock().await;
        if let Some(registration) = channels.get(thread_id).and_then(Weak::upgrade) {
            return registration;
        }
        let registration = ThreadRegistration::new();
        channels.insert(thread_id.to_owned(), Arc::downgrade(&registration));
        registration
    }

    /// Replace any existing registration with a fresh queue.
    ///
    /// Used by `thread/resume` to recover from overflow and connection-local
    /// terminal state. The old consumer is closed and wakes promptly.
    #[cfg(test)]
    pub async fn reregister_thread(&self, thread_id: &str) -> Arc<ThreadRegistration> {
        self.replace_thread_registration(thread_id, ThreadRegistration::new())
            .await
    }

    /// Install an unbounded pre-response staging registration for `thread/resume`.
    pub async fn reregister_thread_for_resume(&self, thread_id: &str) -> Arc<ThreadRegistration> {
        self.replace_thread_registration(thread_id, ThreadRegistration::new_staging())
            .await
    }

    async fn replace_thread_registration(
        &self,
        thread_id: &str,
        registration: Arc<ThreadRegistration>,
    ) -> Arc<ThreadRegistration> {
        let mut channels = self.thread_channels.lock().await;
        if let Some(old) = channels.insert(thread_id.to_owned(), Arc::downgrade(&registration))
            && let Some(old) = old.upgrade()
        {
            old.close();
        }
        registration
    }

    pub async fn close_thread_channels(&self) {
        let channels = std::mem::take(&mut *self.thread_channels.lock().await);
        for registration in channels.values().filter_map(Weak::upgrade) {
            registration.close();
        }
    }

    async fn send_thread_event(&self, thread_id: &str, event: ThreadEvent) -> bool {
        let registration = self
            .thread_channels
            .lock()
            .await
            .get(thread_id)
            .and_then(Weak::upgrade);
        registration.is_some_and(|registration| registration.send(event))
    }

    pub(crate) async fn shutdown(&self) {
        self.cancel.cancel();
        if let Some(waiter) = self.child_waiter.lock().await.take() {
            let _ = waiter.await;
        }
    }

    /// Dispatch a single JSON line from stdout.
    pub(crate) async fn dispatch_line(&self, line: &str) {
        let Ok(raw) = serde_json::from_str::<RawMessage>(line) else {
            return;
        };

        // 1. Response: has `id` + (`result` or `error`)
        if let Some(ref id_val) = raw.id {
            if raw.result.is_some() || raw.error.is_some() {
                // Client request IDs are always the u64 counter; anything else
                // cannot correlate with a pending request.
                if let Some(id) = id_val.as_u64()
                    && let Some(tx) = self.pending_requests.lock().await.remove(&id)
                {
                    let result = if let Some(err) = raw.error {
                        Err(err)
                    } else {
                        Ok(raw.result.unwrap_or(serde_json::Value::Null))
                    };
                    let _ = tx.send(result);
                }
                return;
            }

            // 2. Server-initiated request: has `id` + `method`
            if let Some(ref method) = raw.method {
                let Ok(id) = serde_json::from_value::<RequestId>(id_val.clone()) else {
                    return;
                };
                let params = raw.params.unwrap_or(serde_json::Value::Null);
                self.handle_server_request(id, method, params).await;
                return;
            }
        }

        // 3. Notification: has `method` only (no `id`)
        if let Some(ref method) = raw.method {
            let params = raw.params.clone().unwrap_or(serde_json::Value::Null);
            self.handle_notification(method, &params).await;
        }
    }

    /// Handle a server-initiated request.
    async fn handle_server_request(&self, id: RequestId, method: &str, params: serde_json::Value) {
        // `item/tool/requestUserInput` is deliberately not an approval: it always
        // needs a consumer, so it takes the generic correlated-request path below.
        if let Some(approval) = self.parse_approval_request(id.clone(), method, &params) {
            if let Some(ref handler) = self.approval_handler {
                let handler = Arc::clone(handler);
                let stdin_tx = self.stdin_tx.clone();
                tokio::spawn(async move {
                    let response = handler.handle(approval).await;
                    if let Ok(json) = serde_json::to_vec(&OutgoingResponse {
                        id: id.clone(),
                        result: response.to_wire_value(),
                    }) {
                        let _ = stdin_tx.send(json).await;
                    }
                });
                return;
            }

            let thread_id = approval.thread_id().to_owned();
            let turn_id = Some(approval.turn_id().to_owned());
            let event = TurnEvent::ApprovalRequired(approval);
            self.deliver_or_error(
                &thread_id,
                ThreadEvent {
                    method: method.to_owned(),
                    params: params.clone(),
                    turn_id,
                    event,
                },
                id,
                -32000,
            )
            .await;
            return;
        }

        if method == "item/tool/call" {
            match parse_tool_call_request(id.clone(), &params) {
                Some(request) => {
                    let thread_id = request.thread_id.clone();
                    let turn_id = Some(request.turn_id.clone());
                    let event = TurnEvent::ToolCallRequired(request);
                    self.deliver_or_error(
                        &thread_id,
                        ThreadEvent {
                            method: method.to_owned(),
                            params: params.clone(),
                            turn_id,
                            event,
                        },
                        id,
                        -32000,
                    )
                    .await;
                }
                None => self.respond_unhandled(id, method, -32602),
            }
            return;
        }

        // A user-input request the client cannot deliver is a handling failure;
        // any other unmodeled server request is a genuine "method not found".
        let code = if method == "item/tool/requestUserInput" {
            -32000
        } else {
            -32601
        };
        let thread_id = params
            .get("threadId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let turn_id = turn_id(&params);
        let event = TurnEvent::ServerRequest {
            id: id.clone(),
            method: method.to_owned(),
            params: params.clone(),
        };
        self.deliver_or_error(
            &thread_id,
            ThreadEvent {
                method: method.to_owned(),
                params,
                turn_id,
                event,
            },
            id,
            code,
        )
        .await;
    }

    /// Route a server-initiated request to its thread consumer, or answer it
    /// with an explicit JSON-RPC error when no consumer can take it.
    async fn deliver_or_error(
        &self,
        thread_id: &str,
        event: ThreadEvent,
        id: RequestId,
        code: i64,
    ) {
        let method = event.method.clone();
        if !thread_id.is_empty() && self.send_thread_event(thread_id, event).await {
            return;
        }
        self.respond_unhandled(id, &method, code);
    }

    fn respond_unhandled(&self, id: RequestId, method: &str, code: i64) {
        let response = OutgoingErrorResponse {
            id,
            error: RpcError {
                code,
                message: format!("client did not handle server request `{method}`"),
                data: None,
            },
        };
        let Ok(json) = serde_json::to_vec(&response) else {
            return;
        };
        let stdin_tx = self.stdin_tx.clone();
        tokio::spawn(async move {
            let _ = stdin_tx.send(json).await;
        });
    }

    /// Try to parse a server request into a typed ApprovalRequest.
    fn parse_approval_request(
        &self,
        id: RequestId,
        method: &str,
        params: &serde_json::Value,
    ) -> Option<ApprovalRequest> {
        let thread_id = params.get("threadId")?.as_str()?.to_owned();
        let turn_id = params
            .get("turnId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let item_id = params
            .get("itemId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .map(String::from);

        match method {
            "item/commandExecution/requestApproval" => {
                // v2 sends `command` as a single String.
                let command: Vec<String> = match params.get("command") {
                    Some(serde_json::Value::Array(_)) => {
                        serde_json::from_value(params["command"].clone()).unwrap_or_default()
                    }
                    Some(serde_json::Value::String(s)) => vec![s.clone()],
                    _ => Vec::new(),
                };
                let cwd = params
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(std::path::PathBuf::from);
                let command_actions = params
                    .get("commandActions")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let network_approval = params
                    .get("networkApprovalContext")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let additional_permissions = params
                    .get("additionalPermissions")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let skill_metadata = params
                    .get("skillMetadata")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let proposed_execpolicy_amendment = params
                    .get("proposedExecpolicyAmendment")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let proposed_network_policy_amendments = params
                    .get("proposedNetworkPolicyAmendments")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let available_decisions = params
                    .get("availableDecisions")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                Some(ApprovalRequest::CommandExecution {
                    thread_id,
                    turn_id,
                    item_id,
                    request_id: id,
                    approval_id: params
                        .get("approvalId")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                    command,
                    cwd,
                    reason,
                    command_actions,
                    network_approval,
                    additional_permissions,
                    skill_metadata,
                    proposed_execpolicy_amendment,
                    proposed_network_policy_amendments,
                    available_decisions,
                })
            }
            "item/fileChange/requestApproval" => Some(ApprovalRequest::FileChange {
                thread_id,
                turn_id,
                item_id,
                request_id: id,
                reason,
                grant_root: params
                    .get("grantRoot")
                    .and_then(|v| v.as_str())
                    .map(std::path::PathBuf::from),
            }),
            "item/permissions/requestApproval" => Some(ApprovalRequest::Permissions {
                thread_id,
                turn_id,
                item_id,
                request_id: id,
                reason,
                permissions: params
                    .get("permissions")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())?,
            }),
            _ => None,
        }
    }

    /// Handle a notification (no response needed). Route to thread or global.
    async fn handle_notification(&self, method: &str, params: &serde_json::Value) {
        // Try to extract threadId and route to thread channel
        let thread_id = params
            .get("threadId")
            .and_then(|v| v.as_str())
            .or_else(|| params.get("conversationId").and_then(|v| v.as_str()))
            .or_else(|| {
                params
                    .get("thread")
                    .and_then(|v| v.get("id"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("");

        if !thread_id.is_empty() {
            let event = notification::parse_turn_event(method, params);
            let _ = self
                .send_thread_event(
                    thread_id,
                    ThreadEvent {
                        method: method.to_owned(),
                        params: params.clone(),
                        turn_id: turn_id(params),
                        event,
                    },
                )
                .await;
            return;
        }

        // Non-thread notification → global channel (non-blocking to avoid
        // stalling the reader loop if no consumer is attached).
        let notif = match method {
            "account/updated" => serde_json::from_value(params.clone())
                .map(ServerNotification::AccountUpdated)
                .unwrap_or_else(|_| ServerNotification::Unknown {
                    method: method.to_owned(),
                    params: params.clone(),
                }),
            "command/exec/outputDelta" => serde_json::from_value(params.clone())
                .map(ServerNotification::CommandExecOutputDelta)
                .unwrap_or_else(|_| ServerNotification::Unknown {
                    method: method.to_owned(),
                    params: params.clone(),
                }),
            "warning" => ServerNotification::Warning {
                message: params
                    .get("message")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                thread_id: params
                    .get("threadId")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
            },
            _ => ServerNotification::Unknown {
                method: method.to_owned(),
                params: params.clone(),
            },
        };
        let _ = self.global_notif_tx.try_send(notif);
    }
}

fn turn_id(params: &serde_json::Value) -> Option<String> {
    params
        .get("turnId")
        .and_then(|value| value.as_str())
        .or_else(|| {
            params
                .get("turn")
                .and_then(|turn| turn.get("id"))
                .and_then(|value| value.as_str())
        })
        .map(str::to_owned)
}

fn parse_tool_call_request(
    id: RequestId,
    params: &serde_json::Value,
) -> Option<DynamicToolCallRequest> {
    Some(DynamicToolCallRequest {
        request_id: id,
        thread_id: params.get("threadId")?.as_str()?.to_owned(),
        turn_id: params.get("turnId")?.as_str()?.to_owned(),
        call_id: params.get("callId")?.as_str()?.to_owned(),
        tool: params.get("tool")?.as_str()?.to_owned(),
        namespace: params
            .get("namespace")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        arguments: params.get("arguments").cloned()?,
    })
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    use super::*;
    use crate::approval::ApprovalResponse;

    fn test_inner_with_handler(
        approval_handler: Option<Arc<dyn ApprovalHandler>>,
    ) -> (ServerInner, mpsc::Receiver<Vec<u8>>) {
        let (stdin_tx, stdin_rx) = mpsc::channel(8);
        let (global_notif_tx, _global_notif_rx) = mpsc::channel(1);
        (
            ServerInner {
                stdin_tx,
                pending_requests: Mutex::new(HashMap::new()),
                thread_channels: Mutex::new(HashMap::new()),
                approval_handler,
                global_notif_tx,
                init_result: OnceLock::new(),
                request_counter: AtomicU64::new(1),
                cancel: CancellationToken::new(),
                child_waiter: Mutex::new(None),
            },
            stdin_rx,
        )
    }

    fn test_inner() -> (ServerInner, mpsc::Receiver<Vec<u8>>) {
        test_inner_with_handler(None)
    }

    fn warning(turn_id: &str) -> ThreadEvent {
        ThreadEvent {
            method: "warning".into(),
            params: serde_json::json!({"threadId": "thread-1", "turnId": turn_id, "message": "fill"}),
            turn_id: Some(turn_id.to_owned()),
            event: TurnEvent::Warning {
                message: "fill".into(),
            },
        }
    }

    fn tool_call_json(thread_id: &str) -> String {
        serde_json::json!({
            "id": 41,
            "method": "item/tool/call",
            "params": {
                "threadId": thread_id,
                "turnId": "turn-1",
                "callId": "call-1",
                "tool": "lookup",
                "namespace": "demo",
                "arguments": {"key": "value"}
            }
        })
        .to_string()
    }

    #[tokio::test]
    async fn tool_call_is_surfaced_to_thread_consumer() {
        let (inner, _stdin_rx) = test_inner();
        let registration = inner.register_thread("thread-1").await;
        let mut rx = registration.take_receiver().await.unwrap();

        inner.dispatch_line(&tool_call_json("thread-1")).await;

        let event = rx.recv().await.expect("tool call event");
        assert_eq!(event.method, "item/tool/call");
        assert_eq!(event.params["arguments"]["key"], "value");
        assert!(matches!(
            event,
            ThreadEvent {
                event: TurnEvent::ToolCallRequired(request),
                ..
            }
                if request.request_id == RequestId::Integer(41)
                    && request.call_id == "call-1"
                    && request.tool == "lookup"
        ));
    }

    #[tokio::test]
    async fn string_request_id_is_preserved_in_response() {
        let (inner, mut stdin_rx) = test_inner();
        let registration = inner.register_thread("thread-1").await;
        let mut rx = registration.take_receiver().await.unwrap();
        let request = serde_json::json!({
            "id": "approval-41",
            "method": "item/tool/call",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "callId": "call-1",
                "tool": "lookup",
                "arguments": {}
            }
        });

        inner.dispatch_line(&request.to_string()).await;
        let event = rx.recv().await.expect("tool call event");
        let ThreadEvent {
            event: TurnEvent::ToolCallRequired(request),
            ..
        } = event
        else {
            panic!("unexpected event")
        };
        assert_eq!(request.request_id, RequestId::String("approval-41".into()));

        inner
            .respond(request.request_id, serde_json::json!({"success": true}))
            .await
            .unwrap();
        let response = stdin_rx.recv().await.expect("tool call response");
        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["id"], "approval-41");
    }

    #[tokio::test]
    async fn unhandled_tool_call_gets_json_rpc_error() {
        let (inner, mut stdin_rx) = test_inner();

        inner.dispatch_line(&tool_call_json("missing")).await;

        let response = stdin_rx.recv().await.expect("error response");
        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["id"], 41);
        assert_eq!(response["error"]["code"], -32000);
        assert!(response.get("result").is_none());
    }

    #[tokio::test]
    async fn full_consumer_channel_does_not_block_reader() {
        let (inner, mut stdin_rx) = test_inner();
        let registration = inner.register_thread("thread-1").await;
        for _ in 0..THREAD_CHANNEL_CAPACITY {
            assert!(registration.send(warning("turn-1")));
        }

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            inner.dispatch_line(&tool_call_json("thread-1")),
        )
        .await
        .expect("dispatch blocked on consumer");

        let response = stdin_rx.recv().await.expect("error response");
        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["error"]["code"], -32000);
        assert_eq!(registration.state(), ThreadChannelState::Overflow);
    }

    #[tokio::test]
    async fn terminal_notification_marks_full_thread_queue_as_overflowed() {
        let (inner, _stdin_rx) = test_inner();
        let registration = inner.register_thread("thread-1").await;
        for _ in 0..THREAD_CHANNEL_CAPACITY {
            assert!(registration.send(warning("turn-1")));
        }

        inner
            .dispatch_line(
                &serde_json::json!({
                    "method": "turn/completed",
                    "params": {
                        "threadId": "thread-1",
                        "turn": {
                            "id": "turn-1",
                            "items": [],
                            "status": "completed",
                            "error": null
                        }
                    }
                })
                .to_string(),
            )
            .await;

        assert_eq!(registration.state(), ThreadChannelState::Overflow);
    }

    struct RecordingApprovalHandler {
        called: Arc<AtomicBool>,
    }

    impl ApprovalHandler for RecordingApprovalHandler {
        fn handle(
            &self,
            _request: ApprovalRequest,
        ) -> Pin<Box<dyn Future<Output = ApprovalResponse> + Send + '_>> {
            self.called.store(true, Ordering::Release);
            Box::pin(async { ApprovalResponse::Accept })
        }
    }

    #[tokio::test]
    async fn user_input_is_surfaced_even_with_approval_handler() {
        let called = Arc::new(AtomicBool::new(false));
        let (inner, mut stdin_rx) =
            test_inner_with_handler(Some(Arc::new(RecordingApprovalHandler {
                called: called.clone(),
            })));
        let registration = inner.register_thread("thread-1").await;
        let mut rx = registration.take_receiver().await.unwrap();
        let request = serde_json::json!({
            "id": 52,
            "method": "item/tool/requestUserInput",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "isBlocking": true,
                "questions": [{
                    "id": "choice",
                    "header": "Choice",
                    "question": "Pick one",
                    "options": null
                }]
            }
        });

        inner.dispatch_line(&request.to_string()).await;

        assert!(matches!(
            rx.recv().await,
            Some(ThreadEvent {
                turn_id: Some(turn_id),
                event: TurnEvent::ServerRequest { id: RequestId::Integer(52), ref method, .. },
                ..
            }) if turn_id == "turn-1" && method == "item/tool/requestUserInput"
        ));
        assert!(!called.load(Ordering::Acquire));
        assert!(matches!(
            stdin_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn duplicate_thread_registration_reuses_live_channel() {
        let (inner, _stdin_rx) = test_inner();
        let first = inner.register_thread("thread-1").await;
        let second = inner.register_thread("thread-1").await;

        assert!(Arc::ptr_eq(&first, &second));
        drop(first);
        let third = inner.register_thread("thread-1").await;
        assert!(Arc::ptr_eq(&second, &third));
        assert!(third.send(warning("turn-1")));
        let mut rx = second.take_receiver().await.unwrap();
        assert!(matches!(rx.recv().await, Some(ThreadEvent { .. })));
    }

    #[tokio::test]
    async fn reregister_replaces_overflowed_registration() {
        let (inner, _stdin_rx) = test_inner();
        let old = inner.register_thread("thread-1").await;
        for _ in 0..THREAD_CHANNEL_CAPACITY {
            assert!(old.send(warning("turn-1")));
        }
        assert!(!old.send(warning("turn-1")));
        assert_eq!(old.state(), ThreadChannelState::Overflow);

        let fresh = inner.reregister_thread("thread-1").await;
        assert!(!Arc::ptr_eq(&old, &fresh));
        assert_eq!(fresh.state(), ThreadChannelState::Open);
        assert!(fresh.send(warning("turn-2")));
        let mut rx = fresh.take_receiver().await.unwrap();
        assert_eq!(rx.recv().await.unwrap().turn_id.as_deref(), Some("turn-2"));
    }

    #[tokio::test]
    async fn legacy_thread_notification_reaches_raw_tap() {
        let (inner, _stdin_rx) = test_inner();
        let registration = inner.register_thread("thread-1").await;
        let mut rx = registration.take_receiver().await.unwrap();
        let params = serde_json::json!({
            "conversationId": "thread-1",
            "msg": {"type": "agent_message_delta", "delta": "raw"}
        });
        inner
            .dispatch_line(
                &serde_json::json!({
                    "method": "codex/event/agent_message_delta",
                    "params": params,
                })
                .to_string(),
            )
            .await;
        let event = rx.recv().await.unwrap();
        assert_eq!(event.method, "codex/event/agent_message_delta");
        assert_eq!(event.params, params);
    }
}

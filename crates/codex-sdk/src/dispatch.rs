use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, mpsc, oneshot};
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
    pub thread_channels: Mutex<HashMap<String, mpsc::Sender<ThreadEvent>>>,
    pub approval_handler: Option<Arc<dyn ApprovalHandler>>,
    pub global_notif_tx: mpsc::Sender<ServerNotification>,
    pub init_result: OnceLock<InitializationResult>,
    pub request_counter: AtomicU64,
    pub cancel: CancellationToken,
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

    /// Register a per-thread event channel.
    pub async fn register_thread(&self, thread_id: &str, tx: mpsc::Sender<ThreadEvent>) {
        self.thread_channels
            .lock()
            .await
            .insert(thread_id.to_owned(), tx);
    }

    /// Unregister a per-thread event channel.
    pub async fn unregister_thread(&self, thread_id: &str) {
        self.thread_channels.lock().await.remove(thread_id);
    }

    async fn send_thread_event(&self, thread_id: &str, event: ThreadEvent) -> bool {
        let tx = self.thread_channels.lock().await.get(thread_id).cloned();
        if let Some(tx) = tx {
            tx.try_send(event).is_ok()
        } else {
            false
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
                let id = id_val.as_u64().unwrap_or(0);
                if let Some(tx) = self.pending_requests.lock().await.remove(&id) {
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

    /// Handle a server-initiated request (approval).
    async fn handle_server_request(&self, id: RequestId, method: &str, params: serde_json::Value) {
        let approval = self.parse_approval_request(id.clone(), method, &params);

        if let Some(approval) = approval {
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
            if self
                .send_thread_event(
                    &thread_id,
                    ThreadEvent::Turn(TurnEvent::ApprovalRequired(approval)),
                )
                .await
            {
                return;
            }
            self.respond_unhandled(id, method, -32000);
            return;
        }

        if method == "item/tool/call" {
            let request = parse_tool_call_request(id.clone(), &params);
            if let Some(request) = request {
                let thread_id = request.thread_id.clone();
                if self
                    .send_thread_event(
                        &thread_id,
                        ThreadEvent::Turn(TurnEvent::ToolCallRequired(request)),
                    )
                    .await
                {
                    return;
                }
                self.respond_unhandled(id, method, -32000);
            } else {
                self.respond_unhandled(id, method, -32602);
            }
            return;
        }

        let thread_id = params
            .get("threadId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if !thread_id.is_empty()
            && self
                .send_thread_event(
                    &thread_id,
                    ThreadEvent::Turn(TurnEvent::ServerRequest {
                        id: id.clone(),
                        method: method.to_owned(),
                        params,
                    }),
                )
                .await
        {
            return;
        }

        self.respond_unhandled(id, method, -32601);
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
            "item/tool/requestUserInput" => {
                let questions = params
                    .get("questions")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                Some(ApprovalRequest::UserInput {
                    thread_id,
                    turn_id,
                    item_id,
                    request_id: id,
                    questions,
                })
            }
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
        // Skip bespoke codex/event/* notifications — these are legacy duplicates
        // of the v2 protocol events and use `conversationId` instead of `threadId`.
        // Routing them to the global channel would fill it up and block the reader.
        if method.starts_with("codex/event/") {
            return;
        }

        // Try to extract threadId and route to thread channel
        let thread_id = params
            .get("threadId")
            .and_then(|v| v.as_str())
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
                .send_thread_event(thread_id, ThreadEvent::Turn(event))
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
    use std::sync::atomic::AtomicU64;

    use super::*;

    fn test_inner() -> (ServerInner, mpsc::Receiver<Vec<u8>>) {
        let (stdin_tx, stdin_rx) = mpsc::channel(8);
        let (global_notif_tx, _global_notif_rx) = mpsc::channel(1);
        (
            ServerInner {
                stdin_tx,
                pending_requests: Mutex::new(HashMap::new()),
                thread_channels: Mutex::new(HashMap::new()),
                approval_handler: None,
                global_notif_tx,
                init_result: OnceLock::new(),
                request_counter: AtomicU64::new(1),
                cancel: CancellationToken::new(),
            },
            stdin_rx,
        )
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
        let (tx, mut rx) = mpsc::channel(1);
        inner.register_thread("thread-1", tx).await;

        inner.dispatch_line(&tool_call_json("thread-1")).await;

        let event = rx.recv().await.expect("tool call event");
        assert!(matches!(
            event,
            ThreadEvent::Turn(TurnEvent::ToolCallRequired(request))
                if request.request_id == RequestId::Integer(41)
                    && request.call_id == "call-1"
                    && request.tool == "lookup"
        ));
    }

    #[tokio::test]
    async fn string_request_id_is_preserved_in_response() {
        let (inner, mut stdin_rx) = test_inner();
        let (tx, mut rx) = mpsc::channel(1);
        inner.register_thread("thread-1", tx).await;
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
        let ThreadEvent::Turn(TurnEvent::ToolCallRequired(request)) = event else {
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
        let (tx, _rx) = mpsc::channel(1);
        tx.try_send(ThreadEvent::Turn(TurnEvent::Warning {
            message: "fill".into(),
        }))
        .unwrap();
        inner.register_thread("thread-1", tx).await;

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            inner.dispatch_line(&tool_call_json("thread-1")),
        )
        .await
        .expect("dispatch blocked on consumer");

        let response = stdin_rx.recv().await.expect("error response");
        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["error"]["code"], -32000);
    }
}

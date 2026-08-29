use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::sdk::abort::{Shutdown, ShutdownReason};
use crate::sdk::control::{ControlRequest, ControlResponseEnvelope, ControlResponseInner};
use crate::sdk::error::{Error, ProtocolError};
use crate::sdk::init::InitializationResult;
use crate::sdk::mcp::SdkMcpServer;
use crate::sdk::message::Message;
use crate::sdk::options::{
    ElicitationRequest, HookCallbackContext, HookDecision, HookEventData, HookInput, HookOutput,
    HookPermissionDecision, HookSpecificOutput, SyncHookOutput, UserDialogRequest,
};
use crate::sdk::session::SdkEvent;
use crate::sdk::types::{CanUseToolOptions, PermissionResult, PermissionUpdate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IncomingRequestKind {
    Permission,
    Hook,
    Elicitation,
    UserDialog,
}

// ── QueryInner ───────────────────────────────────────────────────

pub(crate) struct QueryInner {
    pub session_id: String,
    pub stdin_tx: mpsc::UnboundedSender<WriteCommand>,
    pub pending_controls: Mutex<HashMap<String, oneshot::Sender<ControlResponseInner>>>,
    pub init_result: OnceLock<InitializationResult>,
    pub request_counter: AtomicU64,
    pub initialize_request: serde_json::Value,
    pub pending_incoming: Mutex<HashMap<String, IncomingRequestKind>>,
    pub hook_callback_ids: HashSet<String>,
    pub sdk_mcp_servers: std::sync::RwLock<HashMap<String, SdkMcpServer>>,
}

impl QueryInner {
    pub(crate) async fn write(&self, data: Vec<u8>) -> Result<(), Error> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.stdin_tx
            .send(WriteCommand::Data {
                data,
                ack: Some(ack_tx),
            })
            .map_err(|_| Error::Send("Claude stdin is closed".to_owned()))?;
        ack_rx
            .await
            .map_err(|_| Error::Send("Claude stdin writer stopped".to_owned()))?
            .map_err(Error::Send)
    }

    /// Send a control request and wait for the matching response.
    pub async fn send_control<T: serde::Serialize>(
        &self,
        body: T,
    ) -> Result<ControlResponseInner, Error> {
        let id = format!(
            "req_{}",
            self.request_counter.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = oneshot::channel();
        self.pending_controls.lock().await.insert(id.clone(), tx);

        let req = ControlRequest {
            r#type: "control_request",
            request_id: id.clone(),
            request: body,
        };
        let json = serde_json::to_vec(&req)?;

        if let Err(error) = self.write(json).await {
            self.pending_controls.lock().await.remove(&id);
            return Err(error);
        }

        let resp = rx
            .await
            .map_err(|_| Error::Control("reader closed before control response".into()))?;

        if let Some(err) = &resp.error {
            return Err(Error::Control(err.clone()));
        }
        if resp.subtype != "success" {
            return Err(Error::Control(format!(
                "unexpected control response subtype {}",
                resp.subtype
            )));
        }

        Ok(resp)
    }

    pub(crate) async fn answer_incoming(
        &self,
        id: String,
        expected: IncomingRequestKind,
        response: serde_json::Value,
    ) -> Result<(), Error> {
        let mut pending = self.pending_incoming.lock().await;
        match pending.get(&id).copied() {
            Some(kind) if kind == expected => {
                pending.remove(&id);
            }
            _ => return Err(Error::UnknownRequest(id)),
        }
        drop(pending);
        send_control_success(self, &id, response)
            .await
            .map_err(|()| Error::Send("failed to answer Claude control request".into()))
    }
}

// ── Background tasks ───────────────────────────────────────────────

/// Spawn the background reader task that demuxes stdout into turn messages
/// and control responses.
pub(crate) fn spawn_reader_task(
    reader: impl AsyncBufRead + Unpin + Send + 'static,
    turn_tx: mpsc::Sender<Result<SdkEvent, Error>>,
    inner: Arc<QueryInner>,
    shutdown: Arc<Shutdown>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        reader_loop(reader, turn_tx, inner, shutdown).await;
    })
}

async fn reader_loop(
    mut reader: impl AsyncBufRead + Unpin,
    turn_tx: mpsc::Sender<Result<SdkEvent, Error>>,
    inner: Arc<QueryInner>,
    shutdown: Arc<Shutdown>,
) {
    let cancel = shutdown.token();
    let mut line = String::new();
    loop {
        line.clear();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            tokio::select! {
                                biased;
                                _ = cancel.cancelled() => break,
                                _ = turn_tx.send(Err(Error::Protocol(ProtocolError::new(
                                    "empty line from Claude stdout",
                                )))) => {}
                            }
                        } else {
                            tokio::select! {
                                biased;
                                _ = cancel.cancelled() => break,
                                _ = dispatch_line(trimmed, &turn_tx, &inner) => {}
                            }
                        }
                    }
                    Err(error) => {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => {},
                            _ = turn_tx.send(Err(Error::Stream(format!(
                                "I/O error reading Claude stdout: {error}"
                            )))) => {}
                        }
                        shutdown.request(ShutdownReason::TransportFailed);
                        break;
                    }
                }
            }
        }
    }

    drop_pending_controls(inner).await;
}

async fn drop_pending_controls(inner: Arc<QueryInner>) {
    let pending = {
        let mut guard = inner.pending_controls.lock().await;
        std::mem::take(&mut *guard)
    };
    drop(pending);
}

async fn dispatch_line(
    line: &str,
    turn_tx: &mpsc::Sender<Result<SdkEvent, Error>>,
    inner: &QueryInner,
) {
    let value = match serde_json::from_str::<serde_json::Value>(line) {
        Ok(value) => value,
        Err(error) => {
            let _ = turn_tx
                .send(Err(Error::Protocol(ProtocolError::new(format!(
                    "invalid JSON from Claude: {error}; line: {line}"
                )))))
                .await;
            return;
        }
    };

    let msg_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");

    if msg_type == "control_response" {
        let envelope = match serde_json::from_value::<ControlResponseEnvelope>(value.clone()) {
            Ok(envelope) => envelope,
            Err(error) => {
                let _ = turn_tx
                    .send(Err(Error::Protocol(ProtocolError::with_frame(
                        format!("malformed control response: {error}"),
                        value,
                    ))))
                    .await;
                return;
            }
        };
        let request_id = envelope.response.request_id.clone();
        if let Some(tx) = inner.pending_controls.lock().await.remove(&request_id) {
            let _ = tx.send(envelope.response);
        } else {
            let _ = turn_tx
                .send(Err(Error::Protocol(ProtocolError::with_frame(
                    format!("control response has no pending request `{request_id}`"),
                    value,
                ))))
                .await;
        }
        return;
    }

    if msg_type == "control_request" {
        handle_incoming_control_request(&value, inner, turn_tx).await;
        return;
    }

    match Message::parse(value) {
        Ok(msg) => {
            let _ = turn_tx.send(Ok(SdkEvent::Message(msg))).await;
        }
        Err(error) => {
            let _ = turn_tx
                .send(Err(Error::Protocol(ProtocolError::new(format!(
                    "invalid Claude message: {error}; line: {line}"
                )))))
                .await;
        }
    }
}

/// Parse incoming requests without running host code in the transport task.
async fn handle_incoming_control_request(
    value: &serde_json::Value,
    inner: &QueryInner,
    turn_tx: &mpsc::Sender<Result<SdkEvent, Error>>,
) {
    let Some(request_id) = value
        .get("request_id")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
    else {
        let _ = turn_tx
            .send(Err(Error::Protocol(ProtocolError::with_frame(
                "control request requires string field `request_id`",
                value.clone(),
            ))))
            .await;
        return;
    };
    let request = value.get("request");
    let Some(subtype) = request
        .and_then(|r| r.get("subtype"))
        .and_then(|s| s.as_str())
    else {
        let _ = turn_tx
            .send(Err(Error::Protocol(ProtocolError::with_frame(
                "control request requires string field `request.subtype`",
                value.clone(),
            ))))
            .await;
        return;
    };

    if subtype == "can_use_tool" {
        let parsed = match parse_permission_request(request.expect("request checked"), &request_id)
        {
            Ok(parsed) => parsed,
            Err(error) => {
                let _ = turn_tx
                    .send(Err(Error::Protocol(ProtocolError::with_frame(
                        error.clone(),
                        value.clone(),
                    ))))
                    .await;
                let _ = send_control_error(inner, &request_id, &error).await;
                return;
            }
        };
        let event = SdkEvent::PermissionRequest {
            id: request_id.clone(),
            tool_name: parsed.tool_name,
            input: parsed.input,
            suggestions: parsed.options.suggestions,
            blocked_path: parsed.options.blocked_path,
        };
        emit_incoming(
            inner,
            turn_tx,
            request_id,
            IncomingRequestKind::Permission,
            event,
        )
        .await;
        return;
    }

    if subtype == "hook_callback" {
        match parse_hook_callback_request(request, &request_id, inner) {
            Ok((input, context)) => {
                let event = SdkEvent::HookCallback {
                    id: request_id.clone(),
                    input,
                    context,
                };
                emit_incoming(inner, turn_tx, request_id, IncomingRequestKind::Hook, event).await;
            }
            Err(error) => {
                let _ = send_control_error(inner, &request_id, &error).await;
                let _ = turn_tx
                    .send(Err(Error::Protocol(ProtocolError::with_frame(
                        error,
                        value.clone(),
                    ))))
                    .await;
            }
        }
        return;
    }

    if subtype == "mcp_message" {
        let request = request.expect("request checked");
        let server_name = request
            .get("server_name")
            .and_then(serde_json::Value::as_str);
        let message = request.get("message");
        let Some((server_name, message)) = server_name.zip(message) else {
            let error = "mcp_message requires server_name and message";
            let _ = send_control_error(inner, &request_id, error).await;
            let _ = turn_tx
                .send(Err(Error::Protocol(ProtocolError::with_frame(
                    error,
                    value.clone(),
                ))))
                .await;
            return;
        };
        let server = inner
            .sdk_mcp_servers
            .read()
            .expect("SDK MCP server lock poisoned")
            .get(server_name)
            .cloned();
        let Some(server) = server else {
            let _ = send_control_error(
                inner,
                &request_id,
                &format!("SDK MCP server not found: {server_name}"),
            )
            .await;
            return;
        };
        let mcp_response = server
            .handle_message(message)
            .await
            .unwrap_or_else(|| serde_json::json!({ "jsonrpc": "2.0", "id": 0, "result": {} }));
        let _ = send_control_success(
            inner,
            &request_id,
            serde_json::json!({ "mcp_response": mcp_response }),
        )
        .await;
        return;
    }

    if subtype == "elicitation" {
        let request = match parse_elicitation_request(request.expect("request checked")) {
            Ok(request) => request,
            Err(error) => {
                let _ = send_control_error(inner, &request_id, &error).await;
                let _ = turn_tx
                    .send(Err(Error::Protocol(ProtocolError::with_frame(
                        error,
                        value.clone(),
                    ))))
                    .await;
                return;
            }
        };
        let event = SdkEvent::Elicitation {
            id: request_id.clone(),
            request,
        };
        emit_incoming(
            inner,
            turn_tx,
            request_id,
            IncomingRequestKind::Elicitation,
            event,
        )
        .await;
        return;
    }

    if subtype == "request_user_dialog" {
        let request = match parse_user_dialog_request(request.expect("request checked")) {
            Ok(request) => request,
            Err(error) => {
                let _ = send_control_error(inner, &request_id, &error).await;
                let _ = turn_tx
                    .send(Err(Error::Protocol(ProtocolError::with_frame(
                        error,
                        value.clone(),
                    ))))
                    .await;
                return;
            }
        };
        let event = SdkEvent::UserDialog {
            id: request_id.clone(),
            request,
        };
        emit_incoming(
            inner,
            turn_tx,
            request_id,
            IncomingRequestKind::UserDialog,
            event,
        )
        .await;
        return;
    }

    let _ = turn_tx
        .send(Err(Error::Protocol(ProtocolError::with_frame(
            format!("unsupported control request subtype `{subtype}`"),
            value.clone(),
        ))))
        .await;
    let _ = send_control_error(
        inner,
        &request_id,
        &format!("unsupported control request subtype `{subtype}`"),
    )
    .await;
}

async fn emit_incoming(
    inner: &QueryInner,
    turn_tx: &mpsc::Sender<Result<SdkEvent, Error>>,
    request_id: String,
    kind: IncomingRequestKind,
    event: SdkEvent,
) {
    inner
        .pending_incoming
        .lock()
        .await
        .insert(request_id.clone(), kind);
    if turn_tx.send(Ok(event)).await.is_err() {
        inner.pending_incoming.lock().await.remove(&request_id);
    }
}

fn deserialize_permission_updates(
    value: serde_json::Value,
) -> Result<Vec<PermissionUpdate>, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("invalid permission_suggestions: {error}"))
}

struct ParsedPermissionRequest {
    tool_name: String,
    input: serde_json::Value,
    options: CanUseToolOptions,
}

#[derive(serde::Deserialize)]
struct IncomingPermissionRequest {
    #[serde(rename = "subtype")]
    _subtype: String,
    tool_name: String,
    input: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    permission_suggestions: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    blocked_path: Option<String>,
    #[serde(default)]
    decision_reason: Option<String>,
    #[serde(default)]
    decision_reason_type: Option<String>,
    #[serde(default)]
    classifier_approvable: Option<bool>,
    #[serde(default)]
    suppress_always_allow_rule: Option<bool>,
    #[serde(default)]
    default_to_no: Option<bool>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    tool_use_id: String,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    matched_ask_rule: Option<IncomingMatchedAskRule>,
    #[serde(default)]
    requires_user_interaction: Option<bool>,
    #[serde(flatten)]
    extensions: serde_json::Map<String, serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct IncomingMatchedAskRule {
    source: String,
    tool_name: String,
    #[serde(default)]
    rule_content: Option<String>,
    #[serde(flatten)]
    extensions: serde_json::Map<String, serde_json::Value>,
}

fn parse_permission_request(
    request: &serde_json::Value,
    request_id: &str,
) -> Result<ParsedPermissionRequest, String> {
    let parsed: IncomingPermissionRequest = serde_json::from_value(request.clone())
        .map_err(|error| format!("malformed can_use_tool request: {error}"))?;
    let permission_suggestions = parsed.permission_suggestions.unwrap_or_default();
    let suggestions =
        deserialize_permission_updates(serde_json::Value::Array(permission_suggestions.clone()))?;
    Ok(ParsedPermissionRequest {
        tool_name: parsed.tool_name,
        input: serde_json::Value::Object(parsed.input),
        options: CanUseToolOptions {
            suggestions,
            blocked_path: parsed.blocked_path,
            decision_reason: parsed.decision_reason,
            decision_reason_type: parsed.decision_reason_type,
            classifier_approvable: parsed.classifier_approvable,
            suppress_always_allow_rule: parsed.suppress_always_allow_rule,
            default_to_no: parsed.default_to_no,
            title: parsed.title,
            display_name: parsed.display_name,
            description: parsed.description,
            tool_use_id: parsed.tool_use_id,
            agent_id: parsed.agent_id,
            request_id: request_id.to_owned(),
            matched_ask_rule: parsed.matched_ask_rule.map(|rule| {
                crate::sdk::types::MatchedAskRule {
                    source: rule.source,
                    tool_name: rule.tool_name,
                    rule_content: rule.rule_content,
                    extensions: rule.extensions,
                }
            }),
            requires_user_interaction: parsed.requires_user_interaction,
            extensions: parsed.extensions,
        },
    })
}

#[derive(serde::Deserialize)]
struct IncomingElicitationRequest {
    #[serde(rename = "subtype")]
    _subtype: String,
    mcp_server_name: String,
    message: String,
    #[serde(default)]
    mode: Option<crate::sdk::options::ElicitationMode>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    elicitation_id: Option<String>,
    #[serde(default)]
    requested_schema: Option<serde_json::Value>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(flatten)]
    extensions: serde_json::Map<String, serde_json::Value>,
}

fn parse_elicitation_request(request: &serde_json::Value) -> Result<ElicitationRequest, String> {
    let parsed: IncomingElicitationRequest = serde_json::from_value(request.clone())
        .map_err(|error| format!("malformed elicitation request: {error}"))?;
    Ok(ElicitationRequest {
        server_name: parsed.mcp_server_name,
        message: parsed.message,
        mode: parsed.mode,
        url: parsed.url,
        elicitation_id: parsed.elicitation_id,
        requested_schema: parsed.requested_schema,
        title: parsed.title,
        display_name: parsed.display_name,
        description: parsed.description,
        extensions: parsed.extensions,
    })
}

#[derive(serde::Deserialize)]
struct IncomingUserDialogRequest {
    #[serde(rename = "subtype")]
    _subtype: String,
    dialog_kind: String,
    payload: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(flatten)]
    extensions: serde_json::Map<String, serde_json::Value>,
}

fn parse_user_dialog_request(request: &serde_json::Value) -> Result<UserDialogRequest, String> {
    let parsed: IncomingUserDialogRequest = serde_json::from_value(request.clone())
        .map_err(|error| format!("malformed request_user_dialog request: {error}"))?;
    Ok(UserDialogRequest {
        dialog_kind: parsed.dialog_kind,
        payload: serde_json::Value::Object(parsed.payload),
        tool_use_id: parsed.tool_use_id,
        extensions: parsed.extensions,
    })
}

fn object_extensions(
    value: &serde_json::Value,
    known: &[&str],
) -> serde_json::Map<String, serde_json::Value> {
    let mut extensions = value.as_object().cloned().unwrap_or_default();
    for field in known {
        extensions.remove(*field);
    }
    extensions
}

pub(crate) fn permission_result_to_control_value(result: PermissionResult) -> serde_json::Value {
    match result {
        PermissionResult::Allow {
            updated_input,
            updated_permissions,
            ..
        } => {
            let mut response = serde_json::Map::new();
            response.insert("behavior".to_string(), serde_json::json!("allow"));
            if let Some(updated_input) = updated_input {
                response.insert("updatedInput".to_string(), updated_input);
            }
            if let Some(updated_permissions) = updated_permissions {
                response.insert(
                    "updatedPermissions".to_string(),
                    serde_json::to_value(updated_permissions).unwrap_or_default(),
                );
            }
            serde_json::Value::Object(response)
        }
        PermissionResult::Deny {
            message, interrupt, ..
        } => {
            let mut response = serde_json::Map::new();
            response.insert("behavior".to_string(), serde_json::json!("deny"));
            response.insert("message".to_string(), serde_json::json!(message));
            if let Some(interrupt) = interrupt {
                response.insert("interrupt".to_string(), serde_json::json!(interrupt));
            }
            serde_json::Value::Object(response)
        }
    }
}

fn permission_result_to_hook_value(result: PermissionResult) -> serde_json::Value {
    match result {
        PermissionResult::Allow {
            updated_input,
            updated_permissions,
            ..
        } => {
            let mut response = serde_json::Map::new();
            response.insert("behavior".to_string(), serde_json::json!("allow"));
            if let Some(updated_input) = updated_input {
                response.insert("updatedInput".to_string(), updated_input);
            }
            if let Some(updated_permissions) = updated_permissions {
                response.insert(
                    "updatedPermissions".to_string(),
                    serde_json::to_value(updated_permissions).unwrap_or_default(),
                );
            }
            serde_json::Value::Object(response)
        }
        PermissionResult::Deny {
            message, interrupt, ..
        } => {
            let mut response = serde_json::Map::new();
            response.insert("behavior".to_string(), serde_json::json!("deny"));
            response.insert("message".to_string(), serde_json::json!(message));
            if let Some(interrupt) = interrupt {
                response.insert("interrupt".to_string(), serde_json::json!(interrupt));
            }
            serde_json::Value::Object(response)
        }
    }
}

fn parse_hook_callback_request(
    request: Option<&serde_json::Value>,
    request_id: &str,
    inner: &QueryInner,
) -> Result<(HookInput, HookCallbackContext), String> {
    let callback_id = request
        .and_then(|r| r.get("callback_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "hook callback is missing callback_id".to_string())?;
    if !inner.hook_callback_ids.contains(callback_id) {
        return Err(format!("no hook subscription found for ID: {callback_id}"));
    }
    let input_value = request
        .and_then(|r| r.get("input"))
        .cloned()
        .ok_or_else(|| "hook callback is missing input".to_string())?;
    let input = parse_hook_input(&input_value)?;
    let tool_use_id = request
        .and_then(|r| r.get("tool_use_id"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let context = HookCallbackContext {
        request_id: request_id.to_owned(),
        tool_use_id,
        extensions: object_extensions(
            request.expect("hook request checked"),
            &["subtype", "callback_id", "input", "tool_use_id"],
        ),
    };
    Ok((input, context))
}

fn parse_hook_input(value: &serde_json::Value) -> Result<HookInput, String> {
    let event_name = string_field(value, "hook_event_name")
        .or_else(|| string_field(value, "hookEventName"))
        .ok_or_else(|| "hook callback is missing hook_event_name".to_string())?;

    let event = match event_name.as_str() {
        "PreToolUse" => HookEventData::PreToolUse {
            tool_name: required_string_field(value, "tool_name")?,
            tool_input: value.get("tool_input").cloned().unwrap_or_default(),
            tool_use_id: required_string_field(value, "tool_use_id")?,
        },
        "PostToolUse" => HookEventData::PostToolUse {
            tool_name: required_string_field(value, "tool_name")?,
            tool_input: value.get("tool_input").cloned().unwrap_or_default(),
            tool_response: value.get("tool_response").cloned().unwrap_or_default(),
            tool_use_id: required_string_field(value, "tool_use_id")?,
        },
        "PostToolUseFailure" => HookEventData::PostToolUseFailure {
            tool_name: required_string_field(value, "tool_name")?,
            tool_input: value.get("tool_input").cloned().unwrap_or_default(),
            tool_use_id: required_string_field(value, "tool_use_id")?,
            error: required_string_field(value, "error")?,
            is_interrupt: bool_field(value, "is_interrupt"),
        },
        "PostToolBatch" => HookEventData::PostToolBatch {
            tool_calls: deserialize_field(value, "tool_calls")?,
        },
        "Notification" => HookEventData::Notification {
            message: required_string_field(value, "message")?,
            title: string_field(value, "title"),
            notification_type: required_string_field(value, "notification_type")?,
        },
        "UserPromptSubmit" => HookEventData::UserPromptSubmit {
            prompt: required_string_field(value, "prompt")?,
        },
        "UserPromptExpansion" => HookEventData::UserPromptExpansion {
            expansion_type: required_string_field(value, "expansion_type")?,
            command_name: required_string_field(value, "command_name")?,
            command_args: required_string_field(value, "command_args")?,
            command_source: string_field(value, "command_source"),
            prompt: required_string_field(value, "prompt")?,
        },
        "SessionStart" => HookEventData::SessionStart {
            source: deserialize_field(value, "source")?,
            model: string_field(value, "model"),
        },
        "SessionEnd" => HookEventData::SessionEnd {
            reason: required_string_field(value, "reason")?,
        },
        "Stop" => HookEventData::Stop {
            stop_hook_active: required_bool_field(value, "stop_hook_active")?,
            last_assistant_message: string_field(value, "last_assistant_message"),
        },
        "StopFailure" => HookEventData::StopFailure {
            error: required_string_field(value, "error")?,
            error_details: string_field(value, "error_details"),
            last_assistant_message: string_field(value, "last_assistant_message"),
        },
        "SubagentStart" => HookEventData::SubagentStart {
            agent_id: required_string_field(value, "agent_id")?,
            agent_type: required_string_field(value, "agent_type")?,
        },
        "SubagentStop" => HookEventData::SubagentStop {
            stop_hook_active: required_bool_field(value, "stop_hook_active")?,
            agent_id: required_string_field(value, "agent_id")?,
            agent_transcript_path: required_string_field(value, "agent_transcript_path")?,
            agent_type: required_string_field(value, "agent_type")?,
            last_assistant_message: string_field(value, "last_assistant_message"),
        },
        "PreCompact" => HookEventData::PreCompact {
            trigger: deserialize_field(value, "trigger")?,
            custom_instructions: string_field(value, "custom_instructions"),
        },
        "PostCompact" => HookEventData::PostCompact {
            trigger: deserialize_field(value, "trigger")?,
            compact_summary: required_string_field(value, "compact_summary")?,
        },
        "PermissionRequest" => HookEventData::PermissionRequest {
            tool_name: required_string_field(value, "tool_name")?,
            tool_input: value.get("tool_input").cloned().unwrap_or_default(),
            permission_suggestions: value
                .get("permission_suggestions")
                .cloned()
                .map(deserialize_permission_updates)
                .transpose()?
                .filter(|items| !items.is_empty()),
        },
        "PermissionDenied" => HookEventData::PermissionDenied {
            tool_name: required_string_field(value, "tool_name")?,
            tool_input: value.get("tool_input").cloned().unwrap_or_default(),
            tool_use_id: required_string_field(value, "tool_use_id")?,
            reason: required_string_field(value, "reason")?,
        },
        "Setup" => HookEventData::Setup {
            trigger: deserialize_field(value, "trigger")?,
        },
        "TeammateIdle" => HookEventData::TeammateIdle {
            teammate_name: required_string_field(value, "teammate_name")?,
            team_name: required_string_field(value, "team_name")?,
        },
        "TaskCreated" => HookEventData::TaskCreated {
            task_id: required_string_field(value, "task_id")?,
            task_subject: required_string_field(value, "task_subject")?,
            task_description: string_field(value, "task_description"),
            teammate_name: string_field(value, "teammate_name"),
            team_name: string_field(value, "team_name"),
        },
        "TaskCompleted" => HookEventData::TaskCompleted {
            task_id: required_string_field(value, "task_id")?,
            task_subject: required_string_field(value, "task_subject")?,
            task_description: string_field(value, "task_description"),
            teammate_name: string_field(value, "teammate_name"),
            team_name: string_field(value, "team_name"),
        },
        "Elicitation" => HookEventData::Elicitation {
            mcp_server_name: required_string_field(value, "mcp_server_name")?,
            message: required_string_field(value, "message")?,
            mode: value
                .get("mode")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| format!("invalid mode: {error}"))?,
            url: string_field(value, "url"),
            elicitation_id: string_field(value, "elicitation_id"),
            requested_schema: value.get("requested_schema").cloned(),
        },
        "ElicitationResult" => HookEventData::ElicitationResult {
            mcp_server_name: required_string_field(value, "mcp_server_name")?,
            elicitation_id: string_field(value, "elicitation_id"),
            mode: value
                .get("mode")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| format!("invalid mode: {error}"))?,
            action: required_string_field(value, "action")?,
            content: value.get("content").cloned(),
        },
        "ConfigChange" => HookEventData::ConfigChange {
            source: deserialize_field(value, "source")?,
            file_path: string_field(value, "file_path"),
        },
        "WorktreeCreate" => HookEventData::WorktreeCreate {
            name: required_string_field(value, "name")?,
        },
        "WorktreeRemove" => HookEventData::WorktreeRemove {
            worktree_path: required_string_field(value, "worktree_path")?,
        },
        "InstructionsLoaded" => HookEventData::InstructionsLoaded {
            file_path: required_string_field(value, "file_path")?,
            memory_type: required_string_field(value, "memory_type")?,
            load_reason: required_string_field(value, "load_reason")?,
            globs: value
                .get("globs")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| format!("invalid globs: {error}"))?,
            trigger_file_path: string_field(value, "trigger_file_path"),
            parent_file_path: string_field(value, "parent_file_path"),
        },
        "CwdChanged" => HookEventData::CwdChanged {
            old_cwd: required_string_field(value, "old_cwd")?,
            new_cwd: required_string_field(value, "new_cwd")?,
        },
        "FileChanged" => HookEventData::FileChanged {
            file_path: required_string_field(value, "file_path")?,
            event: required_string_field(value, "event")?,
        },
        "DirectoryAdded" => HookEventData::DirectoryAdded {
            directory: required_string_field(value, "directory")?,
            source: required_string_field(value, "source")?,
        },
        "MessageDisplay" => HookEventData::MessageDisplay {
            turn_id: required_string_field(value, "turn_id")?,
            message_id: required_string_field(value, "message_id")?,
            index: deserialize_field(value, "index")?,
            final_delta: required_bool_field(value, "final")?,
            delta: required_string_field(value, "delta")?,
        },
        _ => HookEventData::Unknown(crate::sdk::types::RawFrame::new(value.clone())),
    };

    Ok(HookInput {
        session_id: required_string_field(value, "session_id")?,
        transcript_path: required_string_field(value, "transcript_path")?,
        cwd: required_string_field(value, "cwd")?,
        prompt_id: string_field(value, "prompt_id"),
        permission_mode: string_field(value, "permission_mode"),
        agent_id: string_field(value, "agent_id"),
        agent_type: string_field(value, "agent_type"),
        effort: value
            .get("effort")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| format!("invalid effort: {error}"))?,
        event,
        extensions: object_extensions(
            value,
            &[
                "hook_event_name",
                "hookEventName",
                "session_id",
                "transcript_path",
                "cwd",
                "prompt_id",
                "permission_mode",
                "agent_id",
                "agent_type",
                "effort",
            ],
        ),
    })
}

pub(crate) fn serialize_hook_output(output: HookOutput) -> Result<serde_json::Value, String> {
    match output {
        HookOutput::Async { timeout } => {
            let mut response = serde_json::Map::new();
            response.insert("async".to_string(), serde_json::json!(true));
            if let Some(timeout) = timeout {
                response.insert(
                    "asyncTimeout".to_string(),
                    serde_json::json!(timeout.as_millis() as u64),
                );
            }
            Ok(serde_json::Value::Object(response))
        }
        HookOutput::Sync(sync) => Ok(serialize_sync_hook_output(sync)),
    }
}

fn serialize_sync_hook_output(sync: SyncHookOutput) -> serde_json::Value {
    let mut response = serde_json::Map::new();
    if let Some(should_continue) = sync.r#continue {
        response.insert("continue".to_string(), serde_json::json!(should_continue));
    }
    if let Some(suppress_output) = sync.suppress_output {
        response.insert(
            "suppressOutput".to_string(),
            serde_json::json!(suppress_output),
        );
    }
    if let Some(stop_reason) = sync.stop_reason {
        response.insert("stopReason".to_string(), serde_json::json!(stop_reason));
    }
    if let Some(decision) = sync.decision {
        response.insert(
            "decision".to_string(),
            serde_json::json!(match decision {
                HookDecision::Approve => "approve",
                HookDecision::Block => "block",
            }),
        );
    }
    if let Some(system_message) = sync.system_message {
        response.insert(
            "systemMessage".to_string(),
            serde_json::json!(system_message),
        );
    }
    if let Some(reason) = sync.reason {
        response.insert("reason".to_string(), serde_json::json!(reason));
    }
    if let Some(hook_specific_output) = sync.hook_specific_output {
        response.insert(
            "hookSpecificOutput".to_string(),
            serialize_hook_specific_output(hook_specific_output),
        );
    }
    serde_json::Value::Object(response)
}

fn serialize_hook_specific_output(output: HookSpecificOutput) -> serde_json::Value {
    let mut response = serde_json::Map::new();
    match output {
        HookSpecificOutput::PreToolUse {
            permission_decision,
            permission_decision_reason,
            updated_input,
            additional_context,
        } => {
            response.insert("hookEventName".to_string(), serde_json::json!("PreToolUse"));
            if let Some(permission_decision) = permission_decision {
                response.insert(
                    "permissionDecision".to_string(),
                    serde_json::json!(match permission_decision {
                        HookPermissionDecision::Allow => "allow",
                        HookPermissionDecision::Deny => "deny",
                        HookPermissionDecision::Ask => "ask",
                    }),
                );
            }
            if let Some(permission_decision_reason) = permission_decision_reason {
                response.insert(
                    "permissionDecisionReason".to_string(),
                    serde_json::json!(permission_decision_reason),
                );
            }
            if let Some(updated_input) = updated_input {
                response.insert("updatedInput".to_string(), updated_input);
            }
            if let Some(additional_context) = additional_context {
                response.insert(
                    "additionalContext".to_string(),
                    serde_json::json!(additional_context),
                );
            }
        }
        HookSpecificOutput::UserPromptSubmit { additional_context } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("UserPromptSubmit"),
            );
            if let Some(additional_context) = additional_context {
                response.insert(
                    "additionalContext".to_string(),
                    serde_json::json!(additional_context),
                );
            }
        }
        HookSpecificOutput::UserPromptExpansion {
            additional_context,
            suppress_original_prompt,
        } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("UserPromptExpansion"),
            );
            if let Some(value) = additional_context {
                response.insert("additionalContext".to_string(), serde_json::json!(value));
            }
            if let Some(value) = suppress_original_prompt {
                response.insert(
                    "suppressOriginalPrompt".to_string(),
                    serde_json::json!(value),
                );
            }
        }
        HookSpecificOutput::SessionStart { additional_context } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("SessionStart"),
            );
            if let Some(additional_context) = additional_context {
                response.insert(
                    "additionalContext".to_string(),
                    serde_json::json!(additional_context),
                );
            }
        }
        HookSpecificOutput::Setup { additional_context } => {
            response.insert("hookEventName".to_string(), serde_json::json!("Setup"));
            if let Some(additional_context) = additional_context {
                response.insert(
                    "additionalContext".to_string(),
                    serde_json::json!(additional_context),
                );
            }
        }
        HookSpecificOutput::SubagentStart { additional_context } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("SubagentStart"),
            );
            if let Some(additional_context) = additional_context {
                response.insert(
                    "additionalContext".to_string(),
                    serde_json::json!(additional_context),
                );
            }
        }
        HookSpecificOutput::Stop { additional_context } => {
            response.insert("hookEventName".to_string(), serde_json::json!("Stop"));
            if let Some(value) = additional_context {
                response.insert("additionalContext".to_string(), serde_json::json!(value));
            }
        }
        HookSpecificOutput::SubagentStop { additional_context } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("SubagentStop"),
            );
            if let Some(value) = additional_context {
                response.insert("additionalContext".to_string(), serde_json::json!(value));
            }
        }
        HookSpecificOutput::PostToolUse {
            additional_context,
            updated_mcp_tool_output,
        } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("PostToolUse"),
            );
            if let Some(additional_context) = additional_context {
                response.insert(
                    "additionalContext".to_string(),
                    serde_json::json!(additional_context),
                );
            }
            if let Some(updated_mcp_tool_output) = updated_mcp_tool_output {
                response.insert("updatedMCPToolOutput".to_string(), updated_mcp_tool_output);
            }
        }
        HookSpecificOutput::PostToolUseFailure { additional_context } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("PostToolUseFailure"),
            );
            if let Some(additional_context) = additional_context {
                response.insert(
                    "additionalContext".to_string(),
                    serde_json::json!(additional_context),
                );
            }
        }
        HookSpecificOutput::PostToolBatch { additional_context } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("PostToolBatch"),
            );
            if let Some(value) = additional_context {
                response.insert("additionalContext".to_string(), serde_json::json!(value));
            }
        }
        HookSpecificOutput::Notification { additional_context } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("Notification"),
            );
            if let Some(additional_context) = additional_context {
                response.insert(
                    "additionalContext".to_string(),
                    serde_json::json!(additional_context),
                );
            }
        }
        HookSpecificOutput::PermissionRequest { decision } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("PermissionRequest"),
            );
            response.insert(
                "decision".to_string(),
                permission_result_to_hook_value(decision),
            );
        }
        HookSpecificOutput::PermissionDenied { retry } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("PermissionDenied"),
            );
            if let Some(value) = retry {
                response.insert("retry".to_string(), serde_json::json!(value));
            }
        }
        HookSpecificOutput::Elicitation { action, content } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("Elicitation"),
            );
            if let Some(value) = action {
                response.insert("action".to_string(), serde_json::json!(value));
            }
            if let Some(value) = content {
                response.insert("content".to_string(), value);
            }
        }
        HookSpecificOutput::ElicitationResult { action, content } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("ElicitationResult"),
            );
            if let Some(value) = action {
                response.insert("action".to_string(), serde_json::json!(value));
            }
            if let Some(value) = content {
                response.insert("content".to_string(), value);
            }
        }
        HookSpecificOutput::CwdChanged { watch_paths } => {
            response.insert("hookEventName".to_string(), serde_json::json!("CwdChanged"));
            if let Some(value) = watch_paths {
                response.insert("watchPaths".to_string(), serde_json::json!(value));
            }
        }
        HookSpecificOutput::FileChanged { watch_paths } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("FileChanged"),
            );
            if let Some(value) = watch_paths {
                response.insert("watchPaths".to_string(), serde_json::json!(value));
            }
        }
        HookSpecificOutput::WorktreeCreate { worktree_path } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("WorktreeCreate"),
            );
            response.insert("worktreePath".to_string(), serde_json::json!(worktree_path));
        }
        HookSpecificOutput::MessageDisplay { display_content } => {
            response.insert(
                "hookEventName".to_string(),
                serde_json::json!("MessageDisplay"),
            );
            if let Some(value) = display_content {
                response.insert("displayContent".to_string(), serde_json::json!(value));
            }
        }
    }
    serde_json::Value::Object(response)
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_owned)
}

fn required_string_field(value: &serde_json::Value, key: &str) -> Result<String, String> {
    string_field(value, key).ok_or_else(|| format!("hook callback is missing {key}"))
}

fn bool_field(value: &serde_json::Value, key: &str) -> Option<bool> {
    value.get(key).and_then(|v| v.as_bool())
}

fn required_bool_field(value: &serde_json::Value, key: &str) -> Result<bool, String> {
    bool_field(value, key).ok_or_else(|| format!("hook callback is missing {key}"))
}

fn deserialize_field<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
    key: &str,
) -> Result<T, String> {
    serde_json::from_value(
        value
            .get(key)
            .cloned()
            .ok_or_else(|| format!("hook callback is missing {key}"))?,
    )
    .map_err(|error| format!("failed to parse {key}: {error}"))
}

async fn send_control_success(
    inner: &QueryInner,
    request_id: &str,
    response: serde_json::Value,
) -> Result<(), ()> {
    let envelope = serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        }
    });
    let json = serde_json::to_vec(&envelope).map_err(|_| ())?;
    inner.write(json).await.map_err(|_| ())
}

async fn send_control_error(inner: &QueryInner, request_id: &str, error: &str) -> Result<(), ()> {
    let envelope = serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "error",
            "request_id": request_id,
            "error": error,
        }
    });
    let json = serde_json::to_vec(&envelope).map_err(|_| ())?;
    inner.write(json).await.map_err(|_| ())
}

pub(crate) enum WriteCommand {
    Data {
        data: Vec<u8>,
        ack: Option<oneshot::Sender<Result<(), String>>>,
    },
    Close,
}

/// Spawn the background writer task that serializes stdin writes.
pub(crate) fn spawn_writer_task(
    mut stdin: impl AsyncWrite + Unpin + Send + 'static,
    mut rx: mpsc::UnboundedReceiver<WriteCommand>,
    shutdown: Arc<Shutdown>,
    output_tx: mpsc::Sender<Result<SdkEvent, Error>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let cancel = shutdown.token();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                msg = rx.recv() => {
                    match msg {
                        Some(WriteCommand::Data { data, ack }) => {
                            let result = async {
                                stdin.write_all(&data).await?;
                                stdin.write_all(b"\n").await?;
                                stdin.flush().await
                            }.await;
                            if let Err(error) = result {
                                let message = format!("failed writing Claude stdin: {error}");
                                if let Some(ack) = ack {
                                    let _ = ack.send(Err(message.clone()));
                                }
                                let _ = output_tx.send(Err(Error::Send(format!(
                                    "failed writing Claude stdin: {error}"
                                )))).await;
                                shutdown.request(ShutdownReason::TransportFailed);
                                break;
                            }
                            if let Some(ack) = ack {
                                let _ = ack.send(Ok(()));
                            }
                        }
                        Some(WriteCommand::Close) => break,
                        None => break,
                    }
                }
            }
        }
        drop(stdin);
    })
}

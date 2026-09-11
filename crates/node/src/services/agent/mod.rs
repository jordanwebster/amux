//! AgentService implementation for the protobuf AgentService surface.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::{Stream, StreamExt};
use host_api::{HostSessionEvent, HostSessionInput, LocalAgentHost};
use model::ProtocolError;
use tokio::sync::mpsc;
use uuid::Uuid;
use wire::{self, protocol_status};

use crate::agents::{
    Agent, AgentEvent, ArtifactRef, CreateAgentRpcRequest, RenameAgentRequest, SendInputRequest,
    SessionInputEvent, SetAgentStatusRequest, SpawnInheritance, SubscribeSessionRequest,
};
use crate::envelope::Envelope;
use crate::server::ShutdownReason;
#[cfg(test)]
use crate::tunnel::TunnelTransport;

type TonicResult<T> = Result<tonic::Response<T>, tonic::Status>;
pub(crate) type ResponseStream<T> =
    Pin<Box<dyn Stream<Item = Result<T, tonic::Status>> + Send + 'static>>;

fn local_agents_disabled() -> ProtocolError {
    ProtocolError::FailedPrecondition {
        message: "local agent support is disabled".to_string(),
    }
}

fn no_supported_agent_types() -> ProtocolError {
    ProtocolError::FailedPrecondition {
        message: "host has no supported agent types".to_string(),
    }
}

/// The tonic `AgentService`, and the core's handle to the local runtime.
///
/// Holds the runtime behind `Option<dyn LocalAgentHost>` (`None` in the
/// embedded client) plus the host's identity. Every RPC delegates to the
/// host; the `None` arm is ordinary control flow, not conditional
/// compilation.
#[derive(Clone)]
pub(crate) struct AgentServiceCtx {
    host: Option<Arc<dyn LocalAgentHost>>,
    host_id: Uuid,
    is_cloud_server: bool,
    operations: Arc<crate::installation::OperationGate>,
}

impl AgentServiceCtx {
    pub(crate) fn new(
        host: Option<Arc<dyn LocalAgentHost>>,
        host_id: Uuid,
        is_cloud_server: bool,
    ) -> Self {
        Self {
            host,
            host_id,
            is_cloud_server,
            operations: Arc::default(),
        }
    }

    pub(crate) fn with_operations(
        mut self,
        operations: Arc<crate::installation::OperationGate>,
    ) -> Self {
        self.operations = operations;
        self
    }

    pub(crate) fn host(&self) -> Option<&Arc<dyn LocalAgentHost>> {
        self.host.as_ref()
    }

    pub(crate) fn host_id(&self) -> Uuid {
        self.host_id
    }

    pub(crate) fn is_cloud_server(&self) -> bool {
        self.is_cloud_server
    }

    pub(crate) fn has_supported_agent_types(&self) -> bool {
        self.host.as_ref().is_some_and(|host| !host.capabilities().supported_agent_types.is_empty())
    }

    fn require_host(&self) -> Result<&Arc<dyn LocalAgentHost>, ProtocolError> {
        self.host.as_ref().ok_or_else(local_agents_disabled)
    }

    // Opening an owner creates directories even for reads and replay. Callers
    // must hold a shared operation guard until storage preparation finishes,
    // so deletion drains accepted writes and closed profiles cannot recreate
    // storage. Release it before delivering prepared input to an agent.
    #[cfg(test)]
    pub(crate) async fn agent(&self, agent_id: Uuid) -> Result<Agent, ProtocolError> {
        self.require_host()?.agent(agent_id).await
    }

    pub(crate) async fn subscribe_agent_events(
        &self,
    ) -> Result<mpsc::Receiver<AgentEvent>, ProtocolError> {
        if !self.has_supported_agent_types() {
            return Err(no_supported_agent_types());
        }
        Ok(self.require_host()?.subscribe_agent_events().await)
    }

    pub(crate) async fn subscribe_agent_events_with_snapshot(
        &self,
    ) -> Result<(Vec<AgentEvent>, mpsc::Receiver<AgentEvent>), ProtocolError> {
        if !self.has_supported_agent_types() {
            return Err(no_supported_agent_types());
        }
        Ok(self.require_host()?.agent_events_snapshot().await)
    }

    pub(crate) async fn subscribe_outbound_envelopes(
        &self,
    ) -> Result<mpsc::Receiver<Envelope>, ProtocolError> {
        Ok(self.require_host()?.subscribe_outbound_envelopes().await)
    }

    pub(crate) async fn create(
        &self,
        request: CreateAgentRpcRequest,
    ) -> Result<Agent, ProtocolError> {
        let _operation = self.operations.admit_mutation().await?;
        if self.is_cloud_server() || !self.has_supported_agent_types() {
            return Err(no_supported_agent_types());
        }
        drop(_operation);
        self.require_host()?.create(create_rpc_to_domain_request(request)?, &self.operations).await
    }

    pub(crate) async fn spawn_inheritance(
        &self,
        agent_id: Uuid,
    ) -> Result<SpawnInheritance, ProtocolError> {
        self.require_host()?.spawn_inheritance(agent_id).await
    }

    pub(crate) async fn rename(&self, request: RenameAgentRequest) -> Result<Agent, ProtocolError> {
        let _operation = self.operations.admit_mutation().await?;
        self.require_host()?.rename(request).await
    }

    pub(crate) async fn delete(&self, agent_id: Uuid) -> Result<(), ProtocolError> {
        let _operation = self.operations.barrier().await;
        self.operations.check_mutation()?;
        match self.host() {
            Some(host) => host.delete(agent_id, _operation).await,
            None => Err(ProtocolError::NoAgentFound),
        }
    }

    pub(crate) async fn send_message(&self, envelope: Envelope) -> Result<(), ProtocolError> {
        self.require_host()?.send_message(envelope).await
    }

    pub(crate) async fn send_message_waiting(
        &self,
        envelope: Envelope,
        timeout: std::time::Duration,
    ) -> Result<(), ProtocolError> {
        self.require_host()?
            .send_message_waiting(envelope, timeout)
            .await
    }

    pub(crate) async fn set_agent_status(
        &self,
        request: SetAgentStatusRequest,
    ) -> Result<(), ProtocolError> {
        self.require_host()?.set_agent_status(host_api::HostSetAgentStatus {
            agent_id: request.agent_id,
            working_on: request.working_on,
        }).await
    }

    pub(crate) async fn send_input(&self, request: SendInputRequest) -> Result<(), ProtocolError> {
        let _operation = self.operations.admit().await?;
        self.require_host()?
            .send_input(host_input_request(request)?, _operation)
            .await
    }

    /// Stores an artifact produced by a managed agent, pins it immediately,
    /// and announces its immutable metadata before the tool call returns.
    pub(crate) async fn put_artifact_by_agent(
        &self,
        caller: Uuid,
        kind: model::ArtifactKind,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, ProtocolError> {
        let _operation = self.operations.admit().await?;
        self.require_host()?.put_artifact_by_agent(
            caller, kind, name.to_owned(), mime.to_owned(), bytes, _operation,
        ).await
    }

    pub(crate) async fn subscribe_session_response_stream(
        &self,
        request: SubscribeSessionRequest,
    ) -> Result<ResponseStream<wire::SubscribeSessionResponse>, ProtocolError> {
        let _operation = self.operations.admit().await?;
        let protocol = request.protocol;
        let request = host_session_request(request)?;
        drop(_operation);
        let stream = self.require_host()?.subscribe_session(request).await?;
        Ok(host_stream_to_wire(stream, protocol))
    }
}

#[cfg(test)]
pub(crate) fn spawn_agent_tonic_server(
    ctx: AgentServiceCtx,
    incoming_rx: mpsc::Receiver<TunnelTransport>,
) -> tokio::task::JoinHandle<Result<(), tonic::transport::Error>> {
    let incoming = futures_util::stream::unfold(
        incoming_rx,
        |mut rx: mpsc::Receiver<TunnelTransport>| async {
            rx.recv()
                .await
                .map(|transport| (Ok::<_, std::io::Error>(transport), rx))
        },
    );

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(wire::agent_service_server(ctx))
            .serve_with_incoming(incoming)
            .await
    })
}

#[tonic::async_trait]
impl wire::agent_service_server::AgentService for AgentServiceCtx {
    type SubscribeAgentEventsStream = ResponseStream<wire::SubscribeAgentEventsResponse>;

    async fn subscribe_agent_events(
        &self,
        request: tonic::Request<wire::SubscribeAgentEventsRequest>,
    ) -> TonicResult<Self::SubscribeAgentEventsStream> {
        let _request = request.into_inner();
        let (snapshot, rx) = self
            .subscribe_agent_events_with_snapshot()
            .await
            .map_err(protocol_status)?;
        Ok(tonic::Response::new(agent_event_response_stream(
            snapshot, rx,
        )))
    }

    async fn create_agent(
        &self,
        request: tonic::Request<wire::CreateAgentRequest>,
    ) -> TonicResult<wire::CreateAgentResponse> {
        let request = decode_create_request(request.into_inner())?;
        let agent = self.create(request).await.map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::CreateAgentResponse {
            agent: Some(crate::agents::agent_to_wire(&agent).map_err(encode_status)?),
        }))
    }

    async fn rename_agent(
        &self,
        request: tonic::Request<wire::RenameAgentRequest>,
    ) -> TonicResult<wire::RenameAgentResponse> {
        let request = decode_rename_request(request.into_inner())?;
        let agent = self.rename(request).await.map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::RenameAgentResponse {
            agent: Some(crate::agents::agent_to_wire(&agent).map_err(encode_status)?),
        }))
    }

    async fn delete_agent(
        &self,
        request: tonic::Request<wire::DeleteAgentRequest>,
    ) -> TonicResult<wire::DeleteAgentResponse> {
        let agent_id = decode_delete_request(request.into_inner())?;
        self.delete(agent_id).await.map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::DeleteAgentResponse {
            removed_children: Vec::new(),
            unreachable_children: Vec::new(),
        }))
    }

    async fn send_message(
        &self,
        request: tonic::Request<wire::Envelope>,
    ) -> TonicResult<wire::SendMessageResponse> {
        let wait_for_readiness = request
            .metadata()
            .contains_key(INITIAL_PROMPT_WAIT_METADATA);
        let envelope =
            crate::agents::envelope_from_wire(request.into_inner()).map_err(decode_status)?;
        if envelope.to.host_id != self.host_id() {
            return Err(tonic::Status::not_found(format!(
                "SendMessage target host {} is not local",
                envelope.to.host_id
            )));
        }
        let envelope_id = envelope.id;
        if wait_for_readiness {
            self.send_message_waiting(envelope, INITIAL_PROMPT_READINESS_TIMEOUT)
                .await
        } else {
            self.send_message(envelope).await
        }
        .map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::SendMessageResponse {
            envelope_id: envelope_id.as_bytes().to_vec(),
        }))
    }

    async fn set_agent_status(
        &self,
        request: tonic::Request<wire::SetAgentStatusRequest>,
    ) -> TonicResult<wire::SetAgentStatusResponse> {
        let request = crate::agents::set_agent_status_request_from_wire(request.into_inner())
            .map_err(decode_status)?;
        self.set_agent_status(request)
            .await
            .map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::SetAgentStatusResponse {}))
    }

    type SubscribeSessionStream = ResponseStream<wire::SubscribeSessionResponse>;

    async fn subscribe_session(
        &self,
        request: tonic::Request<wire::pb::SubscribeSessionRequest>,
    ) -> TonicResult<Self::SubscribeSessionStream> {
        let request = decode_subscribe_session_request(request.into_inner())?;
        let stream = self
            .subscribe_session_response_stream(request)
            .await
            .map_err(protocol_status)?;
        Ok(tonic::Response::new(stream))
    }

    async fn send_input(
        &self,
        request: tonic::Request<wire::pb::SendInputRequest>,
    ) -> TonicResult<wire::SendInputResponse> {
        let request = decode_send_input_request(request.into_inner())?;
        self.send_input(request).await.map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::SendInputResponse {}))
    }

    async fn put_artifact(
        &self,
        request: tonic::Request<wire::PutArtifactRequest>,
    ) -> TonicResult<wire::PutArtifactResponse> {
        let _operation = self.operations.admit().await.map_err(protocol_status)?;
        let request = request.into_inner();
        let agent_id = decode_agent_id("PutArtifactRequest.agent_id", request.agent_id)?;
        let kind = crate::agents::artifact_kind_from_wire(request.kind).map_err(decode_status)?;
        let artifact = self.require_host().map_err(protocol_status)?.put_artifact(
            agent_id, kind, request.name, request.mime, request.bytes, _operation,
        ).await.map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::PutArtifactResponse {
            artifact: Some(crate::agents::artifact_ref_to_wire(&artifact)),
        }))
    }

    async fn get_artifact(
        &self,
        request: tonic::Request<wire::GetArtifactRequest>,
    ) -> TonicResult<wire::GetArtifactResponse> {
        let _operation = self.operations.admit().await.map_err(protocol_status)?;
        let request = request.into_inner();
        let agent_id = decode_agent_id("GetArtifactRequest.agent_id", request.agent_id)?;
        let id = request.id.parse().map_err(|error| {
            decode_status(wire::DecodeError::Invalid(format!(
                "GetArtifactRequest.id is invalid: {error}"
            )))
        })?;
        let blob = self.require_host().map_err(protocol_status)?.get_artifact(agent_id, id, _operation).await.map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::GetArtifactResponse {
            artifact: Some(crate::agents::artifact_ref_to_wire(&blob.artifact)),
            bytes: blob.bytes,
        }))
    }

    async fn diff(
        &self,
        request: tonic::Request<wire::DiffRequest>,
    ) -> TonicResult<wire::DiffResponse> {
        let _operation = self.operations.admit().await.map_err(protocol_status)?;
        let request = request.into_inner();
        let agent_id = decode_agent_id("DiffRequest.agent_id", request.agent_id)?;
        let base = request
            .base
            .ok_or_else(|| {
                decode_status(wire::DecodeError::Invalid(
                    "DiffRequest.base is required".to_string(),
                ))
            })
            .and_then(|base| crate::agents::diff_base_from_wire(base).map_err(decode_status))?;
        let response = self.require_host().map_err(protocol_status)?.diff(agent_id, base, _operation).await.map_err(protocol_status)?;
        Ok(tonic::Response::new(crate::agents::diff_response_to_wire(
            &response,
        )))
    }
}

pub(crate) const INITIAL_PROMPT_READINESS_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30);
pub(crate) const INITIAL_PROMPT_WAIT_METADATA: &str = "x-amux-wait-for-agent-ready";

fn decode_create_request(
    request: wire::CreateAgentRequest,
) -> Result<CreateAgentRpcRequest, tonic::Status> {
    crate::agents::create_agent_request_from_wire(request).map_err(decode_status)
}

fn decode_rename_request(
    request: wire::RenameAgentRequest,
) -> Result<RenameAgentRequest, tonic::Status> {
    crate::agents::rename_agent_request_from_wire(request).map_err(decode_status)
}

fn decode_delete_request(request: wire::DeleteAgentRequest) -> Result<Uuid, tonic::Status> {
    crate::agents::delete_agent_id_from_wire(request).map_err(decode_status)
}

fn decode_agent_id(field: &str, bytes: Vec<u8>) -> Result<Uuid, tonic::Status> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        decode_status(wire::DecodeError::Invalid(format!(
            "{field} must be 16 bytes, got {}",
            bytes.len()
        )))
    })?;
    Ok(Uuid::from_bytes(bytes))
}

fn decode_send_input_request(
    request: wire::pb::SendInputRequest,
) -> Result<SendInputRequest, tonic::Status> {
    crate::agents::send_input_request_from_wire(request).map_err(decode_status)
}

fn decode_subscribe_session_request(
    request: wire::pb::SubscribeSessionRequest,
) -> Result<SubscribeSessionRequest, tonic::Status> {
    crate::agents::subscribe_session_request_from_wire(request).map_err(decode_status)
}

fn create_rpc_to_domain_request(request: CreateAgentRpcRequest) -> Result<model::CreateAgentRequest, ProtocolError> {
    use crate::agents::CreateAgentConfig;
    let (agent_type, working_dir, terminal_size, args) = match request.agent {
        CreateAgentConfig::Claude { driver, working_dir, terminal_size, args } => (model::AgentType::Claude { driver }, working_dir, terminal_size, args),
        CreateAgentConfig::Codex { cwd, model, approval_policy, sandbox_policy, resume_thread_id } =>
            (model::AgentType::Codex { model, approval_policy, sandbox_policy, resume_thread_id }, cwd, None, Vec::new()),
        #[cfg(any(debug_assertions, test))]
        CreateAgentConfig::TestAgent { command, working_dir, terminal_size } => (model::AgentType::TestAgent { command }, working_dir, terminal_size, Vec::new()),
        #[cfg(not(any(debug_assertions, test)))]
        CreateAgentConfig::TestAgent { .. } => return Err(ProtocolError::Unimplemented { message: "test-agent creation is unavailable in release builds".into() }),
    };
    Ok(model::CreateAgentRequest {
        agent_id: request.agent_id,
        host_id: None,
        name: request.name,
        agent_type,
        working_dir,
        terminal_size,
        args,
        parent: request.parent,
        initial_prompt: request.initial_prompt,
    })
}

fn host_session_request(request: SubscribeSessionRequest) -> Result<host_api::SessionRequest, ProtocolError> {
    let args = match request.protocol {
        model::Protocol::TerminalV1 => host_api::HostSessionArgs::Terminal(wire::decode_terminal_args(request.args.as_deref()).map_err(decode_error)?),
        model::Protocol::ClaudePtyTranscriptV1 => host_api::HostSessionArgs::ClaudePty(wire::decode_claude_pty_args(request.args.as_deref()).map_err(decode_error)?),
        model::Protocol::ClaudeSdkV1 => host_api::HostSessionArgs::ClaudeSdk(wire::decode_claude_sdk_args(request.args.as_deref()).map_err(decode_error)?),
        model::Protocol::CodexSdkV1 => host_api::HostSessionArgs::Codex(wire::decode_codex_sdk_args(request.args.as_deref()).map_err(decode_error)?),
        model::Protocol::TestEchoV1 if request.args.is_none() => host_api::HostSessionArgs::TestEcho,
        model::Protocol::TestEchoV1 => return Err(ProtocolError::InvalidArgument { message: "test echo does not accept arguments".into() }),
    };
    Ok(host_api::SessionRequest { agent_id: request.agent_id, args })
}

fn host_input_request(request: SendInputRequest) -> Result<host_api::SessionInputRequest, ProtocolError> {
    let input_id = match &request.event {
        SessionInputEvent::Input { input_id, .. } => input_id.clone(),
        SessionInputEvent::Control { .. } => Vec::new(),
    };
    let input = match (request.protocol, request.event) {
        (model::Protocol::TerminalV1, SessionInputEvent::Input { payload, .. }) => HostSessionInput::TerminalBytes(payload),
        (model::Protocol::TerminalV1, SessionInputEvent::Control { payload }) => HostSessionInput::TerminalControl(wire::decode_terminal_control(&payload).map_err(decode_error)?),
        (model::Protocol::ClaudePtyTranscriptV1, SessionInputEvent::Input { payload, .. }) => HostSessionInput::ClaudePty(wire::decode_claude_pty_input(&payload).map_err(decode_error)?),
        (model::Protocol::ClaudeSdkV1, SessionInputEvent::Input { payload, .. }) => HostSessionInput::ClaudeSdk(wire::decode_claude_sdk_input(&payload).map_err(decode_error)?),
        (model::Protocol::CodexSdkV1, SessionInputEvent::Input { payload, .. }) => HostSessionInput::Codex(wire::decode_codex_sdk_input(&payload).map_err(decode_error)?),
        (model::Protocol::TestEchoV1, SessionInputEvent::Input { payload, .. }) => HostSessionInput::TestEcho(payload),
        (protocol, SessionInputEvent::Control { .. }) => return Err(ProtocolError::InvalidArgument { message: format!("{protocol} does not accept control input") }),
    };
    let pin = request.pin.into_iter().map(|id| id.parse().map_err(|error| ProtocolError::InvalidArgument { message: format!("invalid attachment id `{id}`: {error}") })).collect::<Result<Vec<_>, _>>()?;
    Ok(host_api::SessionInputRequest { agent_id: request.agent_id, input_id, input, pin })
}

fn decode_error(error: wire::DecodeError) -> ProtocolError {
    ProtocolError::InvalidArgument { message: error.to_string() }
}

fn host_stream_to_wire(stream: host_api::HostSessionStream, protocol: model::Protocol) -> ResponseStream<wire::SubscribeSessionResponse> {
    Box::pin(stream.map(move |item| {
        let event = match item {
            Ok(HostSessionEvent::Opened) => model::SubscribeSessionEvent::Opened,
            Ok(HostSessionEvent::Output { sequence, payload }) => model::SubscribeSessionEvent::Output {
                payload: wire::encode_provider_output(protocol, sequence, payload).map_err(encode_status)?,
            },
            Ok(HostSessionEvent::ReplayComplete { sequence }) => model::SubscribeSessionEvent::ReplayComplete {
                cursor: wire::encode_provider_cursor(protocol, sequence).map_err(encode_status)?,
            },
            Ok(HostSessionEvent::Closed { reason }) => model::SubscribeSessionEvent::Closed { reason },
            Err(host_api::HostStreamError::Protocol(error)) => return Err(protocol_status(error)),
            Err(host_api::HostStreamError::Shutdown(reason)) => return Err(server_shutdown_status(reason)),
        };
        crate::agents::session_output_event_to_wire(&event, protocol).map_err(|error| tonic::Status::internal(error.to_string()))
    }))
}

fn server_shutdown_status(reason: ShutdownReason) -> tonic::Status {
    let mut metadata = tonic::metadata::MetadataMap::new();
    metadata.insert(
        crate::server::SHUTDOWN_REASON_METADATA_KEY,
        tonic::metadata::MetadataValue::from_static(reason.as_wire_value()),
    );
    tonic::Status::with_metadata(tonic::Code::Unavailable, reason.to_string(), metadata)
}

struct AgentEventStreamState {
    snapshot: VecDeque<AgentEvent>,
    rx: mpsc::Receiver<AgentEvent>,
    snapshot_complete_sent: bool,
    done: bool,
}

fn agent_event_response_stream(
    snapshot: Vec<AgentEvent>,
    rx: mpsc::Receiver<AgentEvent>,
) -> ResponseStream<wire::SubscribeAgentEventsResponse> {
    let state = AgentEventStreamState {
        snapshot: snapshot.into_iter().collect(),
        rx,
        snapshot_complete_sent: false,
        done: false,
    };
    Box::pin(futures_util::stream::unfold(
        state,
        |mut state| async move {
            if state.done {
                return None;
            }
            let event = if let Some(event) = state.snapshot.pop_front() {
                event
            } else if !state.snapshot_complete_sent {
                state.snapshot_complete_sent = true;
                AgentEvent::SnapshotComplete
            } else {
                let Some(event) = state.rx.recv().await else {
                    state.done = true;
                    return Some((
                        Err(tonic::Status::resource_exhausted(
                            "agent event subscriber queue closed",
                        )),
                        state,
                    ));
                };
                event
            };
            let item = crate::agents::agent_event_to_wire(&event).map_err(encode_status);
            Some((item, state))
        },
    ))
}

fn encode_status(error: wire::EncodeError) -> tonic::Status {
    tonic::Status::internal(error.to_string())
}

fn decode_status(error: wire::DecodeError) -> tonic::Status {
    tonic::Status::invalid_argument(error.to_string())
}

#[cfg(all(test, feature = "local-agents"))]
mod tests {
    use std::io;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    use chrono::Utc;
    use futures_util::StreamExt;
    use hyper_util::rt::TokioIo;
    use tonic::codegen::http::Uri;
    use tonic::transport::{Channel, Endpoint};
    use tower::service_fn;

    use super::*;
    use crate::agents::{CreateAgentConfig, TEST_ECHO_COMMAND};

    fn service_host() -> Arc<PtyAgentHost> {
        PtyAgentHost::new(Uuid::from_u128(1))
    }

    fn service_ctx() -> AgentServiceCtx {
        let host = service_host();
        AgentServiceCtx::new(Some(host.clone()), host.host_id(), false)
    }

    fn agent(agent_id: Uuid, host_id: Uuid, name: &str) -> crate::agents::AgentRecord {
        crate::agents::AgentRecord {
            id: agent_id,
            host_id,
            name: Some(name.to_string()),
            command: "test-agent".to_string(),
            working_dir: PathBuf::from("/tmp"),
            kind: crate::agents::AgentKind::TestAgent,
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
            parent: None,
            working_on: None,
        }
    }

    async fn create_test_echo_agent(ctx: &AgentServiceCtx, agent_id: Uuid) {
        ctx.create(CreateAgentRpcRequest {
            agent_id,
            name: Some("echo".to_string()),
            parent: None,
            initial_prompt: None,
            agent: CreateAgentConfig::TestAgent {
                command: TEST_ECHO_COMMAND.to_string(),
                working_dir: std::env::temp_dir(),
                terminal_size: None,
            },
        })
        .await
        .unwrap();
    }

    fn test_echo_subscribe_request(agent_id: Uuid) -> wire::pb::SubscribeSessionRequest {
        wire::pb::SubscribeSessionRequest {
            agent_id: agent_id.as_bytes().to_vec(),
            protocol: Some(wire::pb::subscribe_session_request::Protocol::TestEchoV1(
                wire::pb::TestEchoV1Args {},
            )),
        }
    }

    fn test_echo_send_input_request(agent_id: Uuid, payload: &[u8]) -> wire::pb::SendInputRequest {
        wire::pb::SendInputRequest {
            agent_id: agent_id.as_bytes().to_vec(),
            input_id: b"input-1".to_vec(),
            pin: Vec::new(),
            event: Some(wire::pb::send_input_request::Event::TestEchoV1(
                wire::pb::TestEchoV1Input {
                    payload: payload.to_vec(),
                },
            )),
        }
    }

    #[tokio::test]
    async fn agent_event_stream_reports_resource_exhausted_when_receiver_closes() {
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        let mut stream = agent_event_response_stream(Vec::new(), rx);

        assert!(matches!(
            stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_agent_events_response::Event::SnapshotComplete(_))
        ));
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.code(), tonic::Code::ResourceExhausted);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn tonic_agent_service_unary_methods_map_missing_agent_to_not_found() {
        let ctx = service_ctx();
        let missing_agent_id = Uuid::from_u128(2);

        let delete_error =
            <AgentServiceCtx as wire::agent_service_server::AgentService>::delete_agent(
                &ctx,
                tonic::Request::new(wire::DeleteAgentRequest {
                    agent_id: missing_agent_id.as_bytes().to_vec(),
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(delete_error.code(), tonic::Code::NotFound);

        let send_error = <AgentServiceCtx as wire::agent_service_server::AgentService>::send_input(
            &ctx,
            tonic::Request::new(wire::pb::SendInputRequest {
                agent_id: missing_agent_id.as_bytes().to_vec(),
                input_id: vec![1],
                pin: Vec::new(),
                event: Some(wire::pb::send_input_request::Event::TerminalV1(
                    wire::pb::TerminalV1Input {
                        payload: b"input".to_vec(),
                    },
                )),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(send_error.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn tonic_agent_service_rejects_invalid_request_shapes() {
        let ctx = service_ctx();

        let delete_error =
            <AgentServiceCtx as wire::agent_service_server::AgentService>::delete_agent(
                &ctx,
                tonic::Request::new(wire::DeleteAgentRequest {
                    agent_id: vec![1, 2, 3],
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(delete_error.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn configured_artifact_service_puts_gets_and_deletes_with_the_agent() {
        let data_dir = tempfile::tempdir().unwrap();
        let host = service_host();
        let owners = Arc::new(
            ArtifactOwners::open(
                data_dir.path().to_path_buf(),
                Arc::new(artifacts::SystemClock),
            )
            .unwrap(),
        );
        let ctx = AgentServiceCtx::new(Some(host.clone()), host.host_id(), false)
            .with_artifact_owners(owners);
        let agent_id = Uuid::new_v4();
        create_test_echo_agent(&ctx, agent_id).await;

        let put = <AgentServiceCtx as wire::agent_service_server::AgentService>::put_artifact(
            &ctx,
            tonic::Request::new(wire::PutArtifactRequest {
                agent_id: agent_id.as_bytes().to_vec(),
                kind: wire::ArtifactKind::File as i32,
                name: "notes.txt".to_string(),
                mime: "text/plain".to_string(),
                bytes: b"artifact bytes".to_vec(),
            }),
        )
        .await
        .unwrap()
        .into_inner()
        .artifact
        .unwrap();
        let artifact_root = data_dir
            .path()
            .join("agents")
            .join(agent_id.to_string())
            .join("artifacts");
        assert!(artifact_root.is_dir());

        let get = <AgentServiceCtx as wire::agent_service_server::AgentService>::get_artifact(
            &ctx,
            tonic::Request::new(wire::GetArtifactRequest {
                agent_id: agent_id.as_bytes().to_vec(),
                id: put.id,
            }),
        )
        .await
        .unwrap()
        .into_inner();
        assert_eq!(get.bytes, b"artifact bytes");
        assert_eq!(get.artifact.unwrap().name, "notes.txt");

        <AgentServiceCtx as wire::agent_service_server::AgentService>::delete_agent(
            &ctx,
            tonic::Request::new(wire::DeleteAgentRequest {
                agent_id: agent_id.as_bytes().to_vec(),
            }),
        )
        .await
        .unwrap();
        assert!(!artifact_root.exists());
    }

    #[tokio::test]
    async fn put_artifact_by_agent_pins_and_publishes_the_ref_before_returning() {
        let data_dir = tempfile::tempdir().unwrap();
        let host = service_host();
        let owners = Arc::new(
            ArtifactOwners::open(
                data_dir.path().to_path_buf(),
                Arc::new(artifacts::SystemClock),
            )
            .unwrap(),
        );
        let ctx = AgentServiceCtx::new(Some(host.clone()), host.host_id(), false)
            .with_artifact_owners(owners.clone());
        let agent_id = Uuid::new_v4();
        create_test_echo_agent(&ctx, agent_id).await;
        let log = host.attachment_log(agent_id).await.unwrap();
        let (mut rows, seq) = log.subscribe_with_query(None).await.unwrap();
        assert_eq!(seq, 0);

        let artifact = ctx
            .put_artifact_by_agent(
                agent_id,
                model::ArtifactKind::Image,
                "screen.png",
                "image/png",
                b"png bytes".to_vec(),
            )
            .await
            .unwrap();

        assert!(
            owners
                .owner(agent_id)
                .unwrap()
                .meta(&artifact.id)
                .unwrap()
                .pinned_at
                .is_some()
        );
        assert_eq!(
            rows.read().await.unwrap().payload,
            attachments_row(None, std::slice::from_ref(&artifact))
        );
    }

    #[tokio::test]
    async fn tonic_agent_service_subscribe_agent_events_streams_snapshot_and_live() {
        let host = service_host();
        let host_id = host.host_id();
        let ctx = AgentServiceCtx::new(Some(host.clone()), host_id, false);

        let response =
            <AgentServiceCtx as wire::agent_service_server::AgentService>::subscribe_agent_events(
                &ctx,
                tonic::Request::new(wire::SubscribeAgentEventsRequest::default()),
            )
            .await
            .unwrap();
        let mut stream = response.into_inner();

        let first = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            first.event,
            Some(wire::subscribe_agent_events_response::Event::SnapshotComplete(_))
        ));

        let live_agent = agent(Uuid::from_u128(20), host_id, "live");
        {
            let mut state = host.state().write().await;
            state.local_agent_events.emit(live_agent.agent_event());
        }

        let next = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("timed out waiting for live agent event")
            .expect("agent event stream closed")
            .expect("agent event stream returned error");
        let Some(wire::subscribe_agent_events_response::Event::AgentUp(up)) = next.event else {
            panic!("expected AgentUp");
        };
        assert_eq!(up.agent.unwrap().agent_id, live_agent.id.as_bytes());
    }

    #[tokio::test]
    async fn lifecycle_methods_emit_agent_events_before_return_and_enforce_name_uniqueness() {
        let ctx = service_ctx();
        let mut events = ctx.subscribe_agent_events().await.unwrap();
        let first_id = Uuid::from_u128(31);
        let second_id = Uuid::from_u128(32);

        let created = ctx
            .create(CreateAgentRpcRequest {
                agent_id: first_id,
                name: Some("alpha".to_string()),
                parent: None,
                initial_prompt: None,
                agent: CreateAgentConfig::TestAgent {
                    command: TEST_ECHO_COMMAND.to_string(),
                    working_dir: std::env::temp_dir(),
                    terminal_size: None,
                },
            })
            .await
            .unwrap();
        assert_eq!(created.host_id, ctx.host_id());
        assert!(!created.readonly);
        assert!(matches!(
            events.try_recv().unwrap(),
            AgentEvent::AgentUp {
                agent
            } if agent.id == first_id
                && agent.host_id == ctx.host_id()
                && agent.name.as_deref() == Some("alpha")
                && !agent.readonly
        ));

        let renamed = ctx
            .rename(RenameAgentRequest {
                agent_id: first_id,
                name: "beta".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(renamed.name.as_deref(), Some("beta"));
        assert!(matches!(
            events.try_recv().unwrap(),
            AgentEvent::AgentUpdated {
                agent
            } if agent.id == first_id
                && agent.host_id == ctx.host_id()
                && agent.name.as_deref() == Some("beta")
        ));

        let _second = ctx
            .create(CreateAgentRpcRequest {
                agent_id: second_id,
                name: Some("gamma".to_string()),
                parent: None,
                initial_prompt: None,
                agent: CreateAgentConfig::TestAgent {
                    command: TEST_ECHO_COMMAND.to_string(),
                    working_dir: std::env::temp_dir(),
                    terminal_size: None,
                },
            })
            .await
            .unwrap();
        assert!(matches!(
            events.try_recv().unwrap(),
            AgentEvent::AgentUp { agent } if agent.id == second_id
        ));

        let duplicate = ctx
            .rename(RenameAgentRequest {
                agent_id: second_id,
                name: "beta".to_string(),
            })
            .await
            .unwrap_err();
        assert!(matches!(duplicate, ProtocolError::AlreadyExists { .. }));

        ctx.delete(first_id).await.unwrap();
        assert!(matches!(
            events.try_recv().unwrap(),
            AgentEvent::AgentDown { agent_id } if agent_id == first_id
        ));
    }

    #[tokio::test]
    async fn create_waiting_for_host_preparation_does_not_hold_gate_and_rechecks_close() {
        let host = service_host();
        let ctx = AgentServiceCtx::new(Some(host.clone()), host.host_id(), false);
        let state = host.state().write().await;
        let mut create = Box::pin(ctx.create(CreateAgentRpcRequest {
            agent_id: Uuid::new_v4(),
            name: None,
            parent: None,
            initial_prompt: None,
            agent: CreateAgentConfig::TestAgent {
                command: TEST_ECHO_COMMAND.into(),
                working_dir: std::env::temp_dir(),
                terminal_size: None,
            },
        }));
        assert!(futures_util::poll!(create.as_mut()).is_pending());
        let exclusive = tokio::time::timeout(Duration::from_secs(1), ctx.operations.barrier())
            .await
            .expect("startup preparation must not hold the profile gate");
        ctx.operations.close();
        drop(exclusive);
        drop(state);
        assert!(matches!(
            create.await,
            Err(ProtocolError::FailedPrecondition { .. })
        ));
        assert_eq!(host.agent_count().await, 0);
    }

    #[tokio::test]
    async fn resume_waiting_for_host_preparation_does_not_hold_gate_or_recreate_closed_storage() {
        let host = service_host();
        let operations = crate::installation::OperationGate::default();
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("profile");
        let state_path = directory.join("state.yaml");
        crate::suspend::save_suspended(
            &state_path,
            &crate::suspend::SuspendedServerState {
                agents: vec![crate::suspend::SuspendedAgent::TestAgent {
                    agent_id: Uuid::new_v4(),
                    name: None,
                    command: TEST_ECHO_COMMAND.into(),
                    working_dir: std::env::temp_dir(),
                    terminal_size: None,
                    created_at: Utc::now(),
                    parent: None,
                    working_on: None,
                }],
            },
        )
        .unwrap();
        let state = host.state().write().await;
        let mut resume = Box::pin(host.resume(state_path, &operations));
        assert!(futures_util::poll!(resume.as_mut()).is_pending());
        let exclusive = tokio::time::timeout(Duration::from_secs(1), operations.barrier())
            .await
            .expect("resume preparation must not hold the profile gate");
        operations.close();
        std::fs::remove_dir_all(&directory).unwrap();
        drop(exclusive);
        drop(state);
        assert!(matches!(
            resume.await,
            Err(ProtocolError::FailedPrecondition { .. })
        ));
        assert_eq!(host.agent_count().await, 0);
        assert!(
            !directory.exists(),
            "failed resume must not recreate closed storage"
        );
    }

    #[tokio::test]
    async fn tonic_agent_service_subscribe_session_streams_test_echo_output() {
        let ctx = service_ctx();
        let agent_id = Uuid::from_u128(2);
        create_test_echo_agent(&ctx, agent_id).await;

        let mut stream =
            <AgentServiceCtx as wire::agent_service_server::AgentService>::subscribe_session(
                &ctx,
                tonic::Request::new(test_echo_subscribe_request(agent_id)),
            )
            .await
            .unwrap()
            .into_inner();

        let opened = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            opened.event,
            Some(wire::subscribe_session_response::Event::Opened(_))
        ));
        let replay_complete = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            replay_complete.event,
            Some(wire::subscribe_session_response::Event::ReplayComplete(_))
        ));

        <AgentServiceCtx as wire::agent_service_server::AgentService>::send_input(
            &ctx,
            tonic::Request::new(test_echo_send_input_request(agent_id, b"hello")),
        )
        .await
        .unwrap();

        let output = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("timed out waiting for session output")
            .expect("session stream closed")
            .expect("session stream returned error");
        let Some(wire::subscribe_session_response::Event::Output(output)) = output.event else {
            panic!("expected SessionOutput");
        };
        let Some(wire::session_output::Output::TestEchoV1(output)) = output.output else {
            panic!("expected test echo output");
        };
        assert_eq!(output.payload, b"hello");

        ctx.delete(agent_id).await.unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("timed out waiting for session close")
            .expect("session stream closed before close event")
            .expect("session close returned error");
        let Some(wire::subscribe_session_response::Event::Closed(closed)) = closed.event else {
            panic!("expected SessionClosed");
        };
        assert!(matches!(
            closed.reason,
            Some(wire::session_closed::Reason::AgentDeleted(_))
        ));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn target_side_tonic_server_serves_agent_service_over_tunnel_transport() {
        let ctx = service_ctx();
        let (incoming_tx, incoming_rx) = mpsc::channel(1);
        let server_task = spawn_agent_tonic_server(ctx, incoming_rx);

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        incoming_tx
            .send(TunnelTransport::new(server_io, Uuid::from_u128(20)))
            .await
            .unwrap();

        let channel = channel_from_transport(TunnelTransport::new(client_io, Uuid::from_u128(10)));
        let mut client = wire::agent_service_client(channel);
        let mut stream = client
            .subscribe_agent_events(wire::SubscribeAgentEventsRequest::default())
            .await
            .unwrap()
            .into_inner();

        let first = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            first.event,
            Some(wire::subscribe_agent_events_response::Event::SnapshotComplete(_))
        ));

        let error = client
            .put_artifact(wire::PutArtifactRequest {
                agent_id: Uuid::from_u128(1).as_bytes().to_vec(),
                kind: wire::ArtifactKind::File as i32,
                name: "large.bin".to_string(),
                mime: "application/octet-stream".to_string(),
                bytes: vec![0; 5 * 1024 * 1024],
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unimplemented);
        assert_eq!(
            error.message(),
            "PutArtifact is not configured on this host"
        );
        server_task.abort();
    }

    fn channel_from_transport(transport: TunnelTransport) -> Channel {
        let transport = Arc::new(Mutex::new(Some(transport)));
        Endpoint::from_static("http://tunnel").connect_with_connector_lazy(service_fn(
            move |_uri: Uri| {
                let transport = Arc::clone(&transport);
                async move {
                    transport
                        .lock()
                        .expect("tunnel transport mutex poisoned")
                        .take()
                        .map(TokioIo::new)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::AlreadyExists,
                                "TunnelTransport already consumed",
                            )
                        })
                }
            },
        ))
    }
}

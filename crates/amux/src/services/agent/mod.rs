//! AgentService implementation for the protobuf AgentService surface.

#[cfg(feature = "local-agents")]
mod lifecycle;
mod session_rpc;

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::Arc;

use futures_util::Stream;
#[cfg(feature = "local-agents")]
pub(crate) use lifecycle::{
    CreateAgentError, RenameAgentError, commit_server_suspend, create_agent_record,
    delete_local_agent, prepare_server_suspend, rename_local_agent_record, resume_agents,
    shutdown_server, spawn_session_event_loop, withdraw_agent,
};
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

#[cfg(feature = "local-agents")]
use crate::agents::AgentSession;
use crate::agents::{
    Agent, AgentEvent, AgentRecord, AgentType, CreateAgentConfig, CreateAgentRequest,
    CreateAgentRpcRequest, RenameAgentRequest, SendInputRequest, SessionCloseReason, SessionEvent,
    StopPolicy, SubscribeSessionRequest,
};
use crate::protocol::{ProtocolError, protocol_status, wire};
use crate::routing::EventSource;
use crate::server::ShutdownReason;
#[cfg(test)]
use crate::tunnel::TunnelTransport;

type TonicResult<T> = Result<tonic::Response<T>, tonic::Status>;
type ResponseStream<T> = Pin<Box<dyn Stream<Item = Result<T, tonic::Status>> + Send + 'static>>;
pub(crate) type SharedAgentServiceState = Arc<RwLock<AgentServiceState>>;

#[derive(Default)]
pub(crate) struct AgentServiceState {
    #[cfg(feature = "local-agents")]
    pub(crate) local_agents: HashMap<Uuid, LocalAgentContext>,
    pub(crate) local_agent_events: EventSource<AgentEvent>,
    pub(crate) local_session_close_events: EventSource<(Uuid, SessionCloseReason)>,
    pub(crate) local_shutdown_events: EventSource<ShutdownReason>,
}

#[cfg(feature = "local-agents")]
pub(crate) struct LocalAgentContext {
    pub(crate) session: AgentSession,
}

impl AgentServiceState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Number of locally-hosted agents (always `0` in client-only builds).
    pub(crate) fn local_agent_count(&self) -> usize {
        #[cfg(feature = "local-agents")]
        {
            self.local_agents.len()
        }
        #[cfg(not(feature = "local-agents"))]
        {
            0
        }
    }

    #[cfg(feature = "local-agents")]
    pub(crate) fn agent_session_mut(&mut self, agent_id: &Uuid) -> Option<&mut AgentSession> {
        self.local_agents
            .get_mut(agent_id)
            .map(|context| &mut context.session)
    }

    #[cfg(feature = "local-agents")]
    pub(crate) fn local_agent_info(&self, host_id: Uuid, agent_id: &Uuid) -> Option<AgentRecord> {
        self.local_agents
            .get(agent_id)
            .map(|context| context.session.to_agent(host_id))
    }

    #[cfg(feature = "local-agents")]
    pub(crate) fn insert_registered_local_agent(
        &mut self,
        host_id: Uuid,
        agent_id: Uuid,
        session: AgentSession,
    ) -> Result<AgentEvent, String> {
        self.register_local_agent_context(host_id, agent_id, session)
    }

    #[cfg(feature = "local-agents")]
    pub(crate) fn register_local_agent_context(
        &mut self,
        host_id: Uuid,
        agent_id: Uuid,
        session: AgentSession,
    ) -> Result<AgentEvent, String> {
        if self.contains_agent_id(&agent_id) {
            return Err(format!("Agent already exists: {agent_id}"));
        }
        if let Some(name) = session.name()
            && self.name_taken_by_other(name, agent_id)
        {
            return Err(format!("Agent already exists: {name}"));
        }

        let event = session.to_agent(host_id).agent_event();
        self.local_agents
            .insert(agent_id, LocalAgentContext { session });
        Ok(event)
    }

    #[cfg(feature = "local-agents")]
    pub(crate) fn contains_agent_id(&self, agent_id: &Uuid) -> bool {
        self.local_agents.contains_key(agent_id)
    }

    #[cfg(feature = "local-agents")]
    pub(crate) fn name_taken_by_other(&self, name: &str, agent_id: Uuid) -> bool {
        self.local_agents.values().any(|context| {
            context.session.agent_id() != agent_id && context.session.name() == Some(name)
        })
    }
}

#[derive(Clone)]
pub(crate) struct AgentServiceCtx {
    state: SharedAgentServiceState,
    event_tx: mpsc::Sender<SessionEvent>,
    host_id: Uuid,
    is_cloud_server: bool,
}

impl AgentServiceCtx {
    pub(crate) fn new(
        state: SharedAgentServiceState,
        host_id: Uuid,
        is_cloud_server: bool,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);
        #[cfg(feature = "local-agents")]
        spawn_session_event_loop(state.clone(), event_rx, host_id);
        #[cfg(not(feature = "local-agents"))]
        drop(event_rx);
        Self {
            state,
            event_tx,
            host_id,
            is_cloud_server,
        }
    }

    pub(crate) fn state(&self) -> &SharedAgentServiceState {
        &self.state
    }

    pub(crate) fn event_tx(&self) -> &mpsc::Sender<SessionEvent> {
        &self.event_tx
    }

    pub(crate) fn host_id(&self) -> Uuid {
        self.host_id
    }

    pub(crate) fn is_cloud_server(&self) -> bool {
        self.is_cloud_server
    }

    pub(crate) fn has_supported_agent_types(&self) -> bool {
        !crate::routing::local_capabilities(self.is_cloud_server)
            .supported_agent_types
            .is_empty()
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
            .add_service(wire::agent_service_server::AgentServiceServer::new(ctx))
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
        Ok(tonic::Response::new(wire::DeleteAgentResponse {}))
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
}

impl AgentServiceCtx {
    pub(crate) async fn subscribe_agent_events(
        &self,
    ) -> Result<mpsc::Receiver<AgentEvent>, ProtocolError> {
        if !self.has_supported_agent_types() {
            return Err(ProtocolError::FailedPrecondition {
                message: "host has no supported agent types".to_string(),
            });
        }

        Ok(self.state().write().await.local_agent_events.subscribe())
    }

    pub(crate) async fn subscribe_agent_events_with_snapshot(
        &self,
    ) -> Result<(Vec<AgentEvent>, mpsc::Receiver<AgentEvent>), ProtocolError> {
        if !self.has_supported_agent_types() {
            return Err(ProtocolError::FailedPrecondition {
                message: "host has no supported agent types".to_string(),
            });
        }

        let mut state = self.state().write().await;
        #[cfg(feature = "local-agents")]
        let mut snapshot: Vec<_> = state
            .local_agents
            .values()
            .map(|context| context.session.to_agent(self.host_id()).agent_event())
            .collect();
        #[cfg(not(feature = "local-agents"))]
        let snapshot: Vec<AgentEvent> = Vec::new();
        #[cfg(feature = "local-agents")]
        snapshot.sort_unstable_by_key(agent_event_sort_key);
        let rx = state.local_agent_events.subscribe_drop_on_overflow();
        Ok((snapshot, rx))
    }

    #[cfg(feature = "local-agents")]
    pub(crate) async fn create(
        &self,
        request: CreateAgentRpcRequest,
    ) -> Result<Agent, ProtocolError> {
        if self.is_cloud_server() || !self.has_supported_agent_types() {
            return Err(ProtocolError::FailedPrecondition {
                message: "host has no supported agent types".to_string(),
            });
        }

        let req = create_rpc_to_domain_request(request.agent_id, request)?;

        create_agent_record(self.state(), self.event_tx(), req, self.host_id())
            .await
            .map(Into::into)
            .map_err(create_error_to_protocol)
    }

    #[cfg(not(feature = "local-agents"))]
    pub(crate) async fn create(
        &self,
        _request: CreateAgentRpcRequest,
    ) -> Result<Agent, ProtocolError> {
        Err(ProtocolError::FailedPrecondition {
            message: "local agent support is disabled".to_string(),
        })
    }

    #[cfg(feature = "local-agents")]
    pub(crate) async fn rename(&self, request: RenameAgentRequest) -> Result<Agent, ProtocolError> {
        if request.name.is_empty() {
            return Err(ProtocolError::InvalidArgument {
                message: "RenameAgentRequest.name must not be empty".to_string(),
            });
        }
        let host_id = self.host_id();
        let mut us = self.state().write().await;
        rename_local_agent_record(&mut us, host_id, &request)
            .map(Into::into)
            .map_err(rename_error_to_protocol)
    }

    #[cfg(not(feature = "local-agents"))]
    pub(crate) async fn rename(
        &self,
        _request: RenameAgentRequest,
    ) -> Result<Agent, ProtocolError> {
        Err(ProtocolError::FailedPrecondition {
            message: "local agent support is disabled".to_string(),
        })
    }

    #[cfg(feature = "local-agents")]
    pub(crate) async fn delete(&self, agent_id: Uuid) -> Result<(), ProtocolError> {
        let session_to_stop = {
            let mut us = self.state().write().await;
            delete_local_agent_and_emit_session_close(&mut us, agent_id)
        };

        match session_to_stop {
            Some(session) => {
                session.stop(StopPolicy::Interrupt).await;
                Ok(())
            }
            None => Err(ProtocolError::NoAgentFound),
        }
    }

    #[cfg(not(feature = "local-agents"))]
    pub(crate) async fn delete(&self, _agent_id: Uuid) -> Result<(), ProtocolError> {
        Err(ProtocolError::NoAgentFound)
    }
}

#[cfg(feature = "local-agents")]
fn delete_local_agent_and_emit_session_close(
    us: &mut AgentServiceState,
    agent_id: Uuid,
) -> Option<AgentSession> {
    let session = delete_local_agent(us, agent_id);
    if session.is_some() {
        us.local_session_close_events
            .emit((agent_id, SessionCloseReason::AgentDeleted));
    }
    session
}

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

fn agent_event_sort_key(event: &AgentEvent) -> (String, u128) {
    match event {
        AgentEvent::AgentUp { agent } => {
            (agent.name.clone().unwrap_or_default(), agent.id.as_u128())
        }
        AgentEvent::AgentUpdated { agent } => {
            (agent.name.clone().unwrap_or_default(), agent.id.as_u128())
        }
        AgentEvent::AgentDown { agent_id } => (String::new(), agent_id.as_u128()),
        AgentEvent::SnapshotComplete => (String::new(), 0),
    }
}

#[cfg(feature = "local-agents")]
fn create_error_to_protocol(error: CreateAgentError) -> ProtocolError {
    match error {
        err @ CreateAgentError::LimitReached { .. } => ProtocolError::ResourceExhausted {
            message: err.to_string(),
        },
        err @ CreateAgentError::AlreadyExists(_) => ProtocolError::AlreadyExists {
            message: err.to_string(),
        },
        err @ (CreateAgentError::Start(_) | CreateAgentError::Register(_)) => {
            ProtocolError::ServerError {
                message: err.to_string(),
            }
        }
    }
}

#[cfg(feature = "local-agents")]
fn rename_error_to_protocol(error: RenameAgentError) -> ProtocolError {
    match error {
        RenameAgentError::NotFound(_) => ProtocolError::NoAgentFound,
        err @ RenameAgentError::AlreadyExists(_) => ProtocolError::AlreadyExists {
            message: err.to_string(),
        },
        err @ RenameAgentError::Update(_) => ProtocolError::ServerError {
            message: err.to_string(),
        },
    }
}

#[cfg(feature = "local-agents")]
fn create_rpc_to_domain_request(
    agent_id: Uuid,
    request: CreateAgentRpcRequest,
) -> Result<CreateAgentRequest, ProtocolError> {
    match request.agent {
        CreateAgentConfig::ClaudePty {
            working_dir,
            args,
            terminal_size,
        } => Ok(CreateAgentRequest {
            agent_id,
            host_id: None,
            name: request.name,
            agent_type: AgentType::Claude,
            working_dir,
            terminal_size,
            args,
        }),
        #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
        CreateAgentConfig::TestAgent {
            command,
            working_dir,
            terminal_size,
        } => Ok(CreateAgentRequest {
            agent_id,
            host_id: None,
            name: request.name,
            agent_type: AgentType::TestAgent { command },
            working_dir,
            terminal_size,
            args: Vec::new(),
        }),
        #[cfg(not(all(feature = "local-agents", any(debug_assertions, test))))]
        CreateAgentConfig::TestAgent {
            command,
            working_dir,
            terminal_size,
        } => {
            let _ = (command, working_dir, terminal_size);
            Err(ProtocolError::Unimplemented {
                message: "test-agent creation over protobuf is not available in release builds"
                    .to_string(),
            })
        }
    }
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
    use crate::agents::{TEST_ECHO_COMMAND, TEST_ECHO_V1};

    fn service_ctx() -> AgentServiceCtx {
        AgentServiceCtx::new(
            Arc::new(RwLock::new(AgentServiceState::new())),
            Uuid::from_u128(1),
            false,
        )
    }

    fn agent(agent_id: Uuid, host_id: Uuid, name: &str) -> crate::agents::AgentRecord {
        crate::agents::AgentRecord {
            id: agent_id,
            host_id,
            name: Some(name.to_string()),
            command: "test-agent".to_string(),
            working_dir: PathBuf::from("/tmp"),
            agent_type: "test-agent".to_string(),
            io_protocols: vec!["test_echo_v1".to_string()],
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
        }
    }

    async fn create_test_echo_agent(ctx: &AgentServiceCtx, agent_id: Uuid) {
        ctx.create(CreateAgentRpcRequest {
            agent_id,
            name: Some("echo".to_string()),
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
            io_protocol: TEST_ECHO_V1.to_string(),
            args: None,
        }
    }

    fn test_echo_send_input_request(agent_id: Uuid, payload: &[u8]) -> wire::pb::SendInputRequest {
        wire::pb::SendInputRequest {
            agent_id: agent_id.as_bytes().to_vec(),
            io_protocol: TEST_ECHO_V1.to_string(),
            event: Some(wire::pb::send_input_request::Event::Input(
                wire::pb::SessionInput {
                    input_id: b"input-1".to_vec(),
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
                io_protocol: "claude_raw_v1".to_string(),
                event: Some(wire::pb::send_input_request::Event::Input(
                    wire::pb::SessionInput {
                        input_id: vec![1],
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
    async fn tonic_agent_service_subscribe_agent_events_streams_snapshot_and_live() {
        let ctx = service_ctx();
        let host_id = ctx.host_id();

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
            let mut state = ctx.state().write().await;
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
        let mut client = wire::agent_service_client::AgentServiceClient::new(channel);
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

//! AgentService implementation for the protobuf AgentService surface.

#[cfg(feature = "local-agents")]
mod host;
#[cfg(feature = "local-agents")]
mod lifecycle;
#[cfg(feature = "local-agents")]
mod session_rpc;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::Stream;
#[cfg(feature = "local-agents")]
pub(crate) use host::PtyAgentHost;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::agents::{
    Agent, AgentEvent, AgentRecord, CreateAgentRpcRequest, RenameAgentRequest, SendInputRequest,
    SubscribeSessionRequest,
};
use crate::protocol::{ProtocolError, protocol_status, wire};
use crate::server::ShutdownReason;
#[cfg(test)]
use crate::tunnel::TunnelTransport;

#[cfg(feature = "local-agents")]
mod state;
#[cfg(feature = "local-agents")]
pub(crate) use state::{AgentServiceState, SharedAgentServiceState};

type TonicResult<T> = Result<tonic::Response<T>, tonic::Status>;
pub(crate) type ResponseStream<T> =
    Pin<Box<dyn Stream<Item = Result<T, tonic::Status>> + Send + 'static>>;

/// The seam between the core and the local agent runtime.
///
/// The runtime (sessions, PTY, hooks, lifecycle, suspend/resume) lives behind
/// this trait; the core holds an `Option<Arc<dyn LocalAgentHost>>` and a
/// `None` means "this build hosts no local agents" (the embedded client). The
/// only implementor is [`PtyAgentHost`], compiled with `local-agents`.
#[async_trait]
pub(crate) trait LocalAgentHost: Send + Sync {
    async fn create(&self, request: CreateAgentRpcRequest) -> Result<Agent, ProtocolError>;
    async fn rename(&self, request: RenameAgentRequest) -> Result<Agent, ProtocolError>;
    async fn delete(&self, agent_id: Uuid) -> Result<(), ProtocolError>;
    async fn send_input(&self, request: SendInputRequest) -> Result<(), ProtocolError>;
    async fn subscribe_session(
        &self,
        request: SubscribeSessionRequest,
    ) -> Result<ResponseStream<wire::SubscribeSessionResponse>, ProtocolError>;
    /// Snapshot of currently-hosted agents plus a live event subscription.
    async fn agent_events_snapshot(&self) -> (Vec<AgentEvent>, mpsc::Receiver<AgentEvent>);
    /// Live event subscription without a snapshot (for the client bridge).
    async fn subscribe_agent_events(&self) -> mpsc::Receiver<AgentEvent>;
    async fn handle_hook(
        &self,
        agent_id: Uuid,
        payload: Vec<u8>,
        external: bool,
    ) -> Result<(), ProtocolError>;
    async fn resume(&self, state_path: PathBuf) -> Result<(u64, u64), ProtocolError>;
    async fn stop_all(&self);
    async fn prepare_suspend(&self, state_path: PathBuf) -> Result<u64, ProtocolError>;
    async fn commit_suspend(&self);
    async fn notify_shutdown(&self, reason: ShutdownReason);
    async fn agent_count(&self) -> usize;
    /// Owned, serializable view of hosted agents for the debug dump.
    async fn debug_dump(&self, verbose: bool) -> Vec<DebugAgent>;
}

/// One hosted agent rendered for the debug dump: its record plus the
/// runtime-private session detail (rendered to JSON inside the host, only
/// when verbose).
pub(crate) struct DebugAgent {
    pub(crate) record: AgentRecord,
    pub(crate) session: Option<serde_json::Value>,
}

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
        }
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
        !crate::routing::local_capabilities(self.is_cloud_server)
            .supported_agent_types
            .is_empty()
    }

    fn require_host(&self) -> Result<&Arc<dyn LocalAgentHost>, ProtocolError> {
        self.host.as_ref().ok_or_else(local_agents_disabled)
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

    pub(crate) async fn create(
        &self,
        request: CreateAgentRpcRequest,
    ) -> Result<Agent, ProtocolError> {
        if self.is_cloud_server() || !self.has_supported_agent_types() {
            return Err(no_supported_agent_types());
        }
        self.require_host()?.create(request).await
    }

    pub(crate) async fn rename(&self, request: RenameAgentRequest) -> Result<Agent, ProtocolError> {
        self.require_host()?.rename(request).await
    }

    pub(crate) async fn delete(&self, agent_id: Uuid) -> Result<(), ProtocolError> {
        match self.host() {
            Some(host) => host.delete(agent_id).await,
            None => Err(ProtocolError::NoAgentFound),
        }
    }

    pub(crate) async fn send_input(&self, request: SendInputRequest) -> Result<(), ProtocolError> {
        self.require_host()?.send_input(request).await
    }

    pub(crate) async fn subscribe_session_response_stream(
        &self,
        request: SubscribeSessionRequest,
    ) -> Result<ResponseStream<wire::SubscribeSessionResponse>, ProtocolError> {
        self.require_host()?.subscribe_session(request).await
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
    use crate::agents::{CreateAgentConfig, TEST_ECHO_COMMAND, TEST_ECHO_V1};

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
                io_protocol: "terminal_v1".to_string(),
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

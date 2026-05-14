use std::sync::Arc;
use std::time::Duration;

use prost::Message as _;
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::connection::{
    ConnectionContext, ConnectionError, HeartbeatRole, HeartbeatSetup, RunConnection,
    run_connection,
};
use super::routing::{broadcast_topology_event, maybe_start_agent_subscription, withdraw_agent};
use super::{
    ConnectionHandle, LOCAL_USER_ID, ServerState, ServerUserState, local_host, test_helpers,
};
use crate::agent::{AgentSession, SessionEvent, StopPolicy, TEST_ECHO_COMMAND, TestAgentSession};
use crate::client::{Connection, OpenSessionClient, RpcClient, RpcClientError};
use crate::config::Config;
use crate::protocol::handshake::RoutingRole;
use crate::protocol::link::Link;
use crate::protocol::message::{
    AgentEvent, CallId, FrameBody, GoAway, Host, LocalFrame, Message, PeerFrame, ProtocolError,
    ReauthRequest, RequestFrame, ResponseFrame, RoutedFrame, RoutedFrameMessage, RoutingEvent,
    ShutdownReason,
};
use crate::protocol::open_session::{self, OpenSessionOutputEvent, OpenSessionServerFrame};
use crate::protocol::{Agent, AgentType, CreateAgentRequest, Route, method, wire};
use crate::rpc::RpcPeerStreamOutboundStart;
use crate::transport::memory::{MemoryTransport, pair as memory_transport_pair};
use crate::transport::{Transport, TransportError};

const EXPECT_TIMEOUT: Duration = Duration::from_secs(1);
const PEER_LINK_CLOSE_REASON: &str = "test peer link closed";

pub(super) struct Topology {
    state: Arc<RwLock<ServerState>>,
    user_state: Arc<RwLock<ServerUserState>>,
    event_tx: mpsc::Sender<SessionEvent>,
}

impl Topology {
    pub(super) async fn new() -> Self {
        Self::named("test-host").await
    }

    pub(super) async fn named(host_name: &str) -> Self {
        let (state, user_state) = test_helpers::test_state().await;
        state.write().await.config = Config {
            host_name: host_name.to_string(),
            ..Config::default()
        };
        let (event_tx, _event_rx) = mpsc::channel(64);
        Self {
            state,
            user_state,
            event_tx,
        }
    }

    pub(super) async fn connect_local(&self, link: &str) -> TestConnection {
        self.connect(link, true).await
    }

    pub(super) async fn connect_local_client(&self, link: &str) -> TestRpcClient {
        self.connect_local(link).await.into_rpc_client()
    }

    pub(super) async fn connect_peer(&self, link: &str) -> TestConnection {
        self.connect(link, false).await
    }

    pub(super) async fn connect_peer_with_heartbeat(
        &self,
        link: &str,
        idle_timeout: Duration,
    ) -> TestConnection {
        self.connect_with_heartbeat(
            link,
            false,
            Some(HeartbeatSetup {
                role: HeartbeatRole::Dialer,
                idle_timeout,
            }),
        )
        .await
    }

    pub(super) async fn require_minimum_client_version(
        &self,
        client_id: &str,
        minimum_version: &str,
    ) {
        self.state
            .write()
            .await
            .config
            .minimum_client_versions
            .insert(client_id.to_string(), minimum_version.to_string());
    }

    pub(super) async fn host_id(&self) -> Uuid {
        self.state.read().await.host_id
    }

    pub(super) async fn set_cloud_server(&self, is_cloud_server: bool) {
        self.state.write().await.is_cloud_server = is_cloud_server;
    }

    async fn local_host(&self) -> Host {
        let state = self.state.read().await;
        local_host(
            state.host_id,
            &state.config.host_name,
            state.is_cloud_server,
        )
    }

    pub(super) async fn connect_peer_topology(
        &self,
        link: &str,
        peer: &Topology,
    ) -> PeerTopologyLink {
        let link = Link::new(link).unwrap();
        let (local_transport, peer_transport) = memory_transport_pair(256);
        let local_host = self.local_host().await;
        let peer_host = peer.local_host().await;

        let local_task = self
            .start_peer_connection(link.clone(), local_transport, peer_host)
            .await;
        let peer_task = peer
            .start_peer_connection(link.clone(), peer_transport, local_host)
            .await;

        PeerTopologyLink {
            link: link.clone(),
            local_task,
            peer_task,
            local_handle: self
                .user_state
                .read()
                .await
                .route(&link)
                .expect("local peer route should be registered"),
            peer_handle: peer
                .user_state
                .read()
                .await
                .route(&link)
                .expect("remote peer route should be registered"),
        }
    }

    pub(super) async fn spawn_test_echo_agent(&self, name: &str) -> Uuid {
        let agent_id = Uuid::new_v4();
        let session = AgentSession::TestAgent(TestAgentSession::echo_for_tests(
            agent_id,
            Some(name.to_string()),
        ));
        let host_id = self.state.read().await.host_id;
        let agent = session.to_agent(host_id);

        let mut user_state = self.user_state.write().await;
        let announcement = user_state
            .register_local_agent_context(agent_id, session, agent)
            .unwrap();
        broadcast_topology_event(&mut user_state, &announcement, None);
        agent_id
    }

    pub(super) async fn withdraw_agent(&self, agent_id: Uuid) {
        let session = {
            let mut user_state = self.user_state.write().await;
            withdraw_agent(&mut user_state, agent_id)
                .unwrap_or_else(|| panic!("expected test agent {agent_id} to exist"))
        };
        session.stop(StopPolicy::Interrupt).await;
    }

    pub(super) async fn expect_no_open_sessions(&self) {
        tokio::time::timeout(EXPECT_TIMEOUT, async {
            loop {
                let user_state = self.user_state.read().await;
                let has_open_session_rpc = !user_state
                    .inbound_call_ids_if(|call| call.method == method::AGENT_OPEN_SESSION)
                    .is_empty();
                if !has_open_session_rpc {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for OpenSession state to clear");
    }

    async fn start_peer_connection(
        &self,
        link: Link,
        transport: MemoryTransport,
        peer_host: Host,
    ) -> JoinHandle<super::connection::Result<()>> {
        let (route_handle, outgoing_rx, initial_messages, routing_call_id) = {
            let mut user_state = self.user_state.write().await;
            let (route_handle, outgoing_rx) = user_state
                .try_reserve_link(link.clone())
                .expect("test peer link should be unique");
            user_state.mark_peer_link(link.clone());
            let change = user_state.apply_direct_peer_host_up(&link, peer_host);
            for event in &change.events {
                broadcast_topology_event(&mut user_state, event, Some(&link));
                if let super::routing::TopologyEvent::HostUp { host, .. } = event {
                    maybe_start_agent_subscription(&mut user_state, host.id, false);
                }
            }
            let routing_call_id = CallId::from(Uuid::new_v4());
            let initial_messages = vec![Message::Peer(PeerFrame {
                call_id: routing_call_id.clone(),
                body: FrameBody::Request(RequestFrame {
                    method: method::ROUTING_SUBSCRIBE_EVENTS_NAME.to_string(),
                    payload: wire::SubscribeRoutingEventsRequest {}.encode_to_vec(),
                }),
            })];
            (route_handle, outgoing_rx, initial_messages, routing_call_id)
        };
        self.user_state
            .read()
            .await
            .rpc_for_link(&link)
            .expect("test peer route should have RPC state")
            .register_peer_stream_outbound(RpcPeerStreamOutboundStart {
                call_id: routing_call_id,
                link: link.clone(),
                method: method::ROUTING_SUBSCRIBE_EVENTS,
            })
            .expect("fresh routing call id should not collide");

        let ctx = ConnectionContext {
            state: self.state.clone(),
            user_state: self.user_state.clone(),
            rpc: self
                .user_state
                .read()
                .await
                .rpc_for_link(&link)
                .expect("test peer route should have RPC state"),
            user_id: LOCAL_USER_ID,
            event_tx: self.event_tx.clone(),
            link: link.clone(),
            is_local: false,
            heartbeat: None,
            routing_role: RoutingRole::Host,
        };
        let response_tx = route_handle.sender();
        let close_rx = route_handle.close_receiver();
        tokio::spawn(run_connection(RunConnection {
            transport,
            outgoing_rx,
            initial_messages,
            response_tx,
            close_rx,
            ctx,
            token_refresh: None,
            span: tracing::Span::none(),
        }))
    }

    async fn connect(&self, link: &str, is_local: bool) -> TestConnection {
        self.connect_with_heartbeat(link, is_local, None).await
    }

    async fn connect_with_heartbeat(
        &self,
        link: &str,
        is_local: bool,
        heartbeat: Option<HeartbeatSetup>,
    ) -> TestConnection {
        let link = Link::new(link).unwrap();
        let (client_transport, server_transport) = memory_transport_pair(256);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(super::state::OUTGOING_MESSAGE_BUFFER);
        let route_handle = ConnectionHandle::new(outgoing_tx);

        {
            let mut user_state = self.user_state.write().await;
            let (_reserved_handle, _reserved_rx) = user_state
                .try_reserve_link(link.clone())
                .expect("test link should be unique");
            let context = user_state
                .connections
                .get_mut(&link)
                .expect("reserved connection should exist");
            context.handle = route_handle.clone();
            if !is_local {
                user_state.mark_peer_link(link.clone());
                let change = user_state.apply_direct_peer_host_up(
                    &link,
                    Host {
                        id: Uuid::new_v4(),
                        name: link.as_str().to_string(),
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        capabilities: Default::default(),
                    },
                );
                for event in &change.events {
                    broadcast_topology_event(&mut user_state, event, Some(&link));
                }
            }
        }

        let ctx = ConnectionContext {
            state: self.state.clone(),
            user_state: self.user_state.clone(),
            rpc: self
                .user_state
                .read()
                .await
                .rpc_for_link(&link)
                .expect("test route should have RPC state"),
            user_id: LOCAL_USER_ID,
            event_tx: self.event_tx.clone(),
            link: link.clone(),
            is_local,
            heartbeat,
            routing_role: if is_local {
                RoutingRole::Observer
            } else {
                RoutingRole::Host
            },
        };
        let response_tx = route_handle.sender();
        let close_rx = route_handle.close_receiver();
        let task = tokio::spawn(run_connection(RunConnection {
            transport: server_transport,
            outgoing_rx,
            initial_messages: Vec::new(),
            response_tx,
            close_rx,
            ctx,
            token_refresh: None,
            span: tracing::Span::none(),
        }));

        TestConnection {
            link,
            transport: Some(client_transport),
            task,
            next_call_id: 1,
        }
    }
}

pub(super) struct PeerTopologyLink {
    link: Link,
    local_task: JoinHandle<super::connection::Result<()>>,
    peer_task: JoinHandle<super::connection::Result<()>>,
    local_handle: ConnectionHandle,
    peer_handle: ConnectionHandle,
}

impl PeerTopologyLink {
    pub(super) fn local_link(&self) -> Link {
        self.link.clone()
    }

    pub(super) async fn close(self) {
        self.local_handle.request_close(PEER_LINK_CLOSE_REASON);
        self.peer_handle.request_close(PEER_LINK_CLOSE_REASON);
        drop(self.local_handle);
        drop(self.peer_handle);
        expect_peer_connection_closed("local", self.local_task).await;
        expect_peer_connection_closed("remote", self.peer_task).await;
    }
}

async fn expect_peer_connection_closed(
    label: &'static str,
    task: JoinHandle<super::connection::Result<()>>,
) {
    let result = tokio::time::timeout(EXPECT_TIMEOUT, task)
        .await
        .unwrap_or_else(|_| panic!("{label} peer task did not close"))
        .unwrap_or_else(|error| panic!("{label} peer task panicked: {error}"));

    match result {
        Ok(()) => {}
        Err(ConnectionError::Protocol(reason)) if reason == PEER_LINK_CLOSE_REASON => {}
        Err(error) => panic!("{label} peer task closed with unexpected error: {error}"),
    }
}

pub(super) struct TestConnection {
    link: Link,
    transport: Option<MemoryTransport>,
    task: JoinHandle<super::connection::Result<()>>,
    next_call_id: u128,
}

impl TestConnection {
    pub(super) fn link(&self) -> Link {
        self.link.clone()
    }

    async fn send(&mut self, msg: Message) {
        self.transport
            .as_mut()
            .expect("test connection transport should be open")
            .write_message(&msg)
            .await
            .unwrap();
    }

    pub(super) async fn write_malformed_runtime_frame(&mut self, data: &[u8]) {
        self.transport
            .as_mut()
            .expect("test connection transport should be open")
            .write_frame(data)
            .await
            .unwrap();
    }

    async fn recv(&mut self) -> Message {
        tokio::time::timeout(EXPECT_TIMEOUT, async {
            self.transport
                .as_mut()
                .expect("test connection transport should be open")
                .read_message()
                .await
        })
        .await
        .expect("timed out waiting for protocol message")
        .unwrap()
    }

    pub(super) async fn close(mut self) -> super::connection::Result<()> {
        drop(self.transport.take());
        self.task.await.expect("connection task panicked")
    }

    pub(super) async fn expect_closed_after_protocol_decode_error(self) {
        let result = self.close().await;
        assert!(matches!(
            result,
            Err(ConnectionError::Transport(TransportError::ProtocolDecode(
                _
            )))
        ));
    }

    fn into_rpc_client(mut self) -> TestRpcClient {
        let transport = self
            .transport
            .take()
            .expect("test connection transport should be open");
        let connection = Connection::new_memory(transport, self.link.clone());
        TestRpcClient {
            rpc: RpcClient::new(connection),
            task: self.task,
        }
    }

    fn next_call_id(&mut self) -> CallId {
        let id = self.next_call_id;
        self.next_call_id += 1;
        call_id(id)
    }

    pub(super) async fn expect_protocol_error_goaway(&mut self) {
        assert_eq!(
            self.recv().await,
            Message::GoAway(GoAway {
                reason: ShutdownReason::ProtocolError,
            })
        );
    }

    pub(super) async fn expect_ping(&mut self) {
        assert_eq!(self.recv().await, Message::Ping);
    }

    pub(super) async fn send_pong(&mut self) {
        self.send(Message::Pong).await;
    }

    pub(super) async fn send_reauth(&mut self, token: &str) {
        self.send(Message::Reauth(ReauthRequest {
            token: token.to_string(),
        }))
        .await;
    }

    pub(super) async fn send_local_list_agents_request(&mut self) {
        let call_id = self.next_call_id();
        self.send(Message::Local(LocalFrame {
            call_id,
            body: FrameBody::Request(RequestFrame {
                method: method::AGENT_LIST_NAME.to_string(),
                payload: wire::ListAgentsRequest {}.encode_to_vec(),
            }),
        }))
        .await;
    }

    pub(super) async fn open_session_with_queued_raw_input(
        &mut self,
        agent_id: Uuid,
        io_protocol: &str,
        input: &[u8],
    ) -> QueuedOpenSession {
        let call_id = self.next_call_id();
        self.send_local_routed_payload(
            call_id.clone(),
            open_session::encode_open_session_request().unwrap(),
        )
        .await;
        self.send_local_routed_payload(
            call_id.clone(),
            open_session::encode_open_session_open(agent_id, io_protocol, None).unwrap(),
        )
        .await;
        self.send_local_routed_payload(
            call_id.clone(),
            open_session::encode_open_session_input(Vec::new(), input.to_vec()).unwrap(),
        )
        .await;
        QueuedOpenSession { call_id }
    }

    async fn send_local_routed_payload(&mut self, call_id: CallId, payload: Vec<u8>) {
        let (src, dst) = Route::send(Route::from_link(self.link()))
            .expect("local test route should include the client link");
        self.send(routed_payload(src, dst, call_id, payload)).await;
    }

    pub(super) async fn expect_reauth_accepted(&mut self) {
        assert_eq!(
            self.recv().await,
            Message::ReauthResponse(crate::protocol::message::ReauthResponse { error: None })
        );
    }

    pub(super) async fn expect_heartbeat_timeout(self) {
        let TestConnection {
            task, transport, ..
        } = self;
        let _transport = transport;
        let result = tokio::time::timeout(EXPECT_TIMEOUT, task)
            .await
            .expect("timed out waiting for heartbeat timeout")
            .expect("connection task panicked");
        assert!(matches!(result, Err(ConnectionError::HeartbeatTimeout)));
    }

    pub(super) async fn send_to_missing_route(
        &mut self,
        missing: &str,
        payload: &[u8],
    ) -> MissingRouteProbe {
        let missing = Link::new(missing).unwrap();
        let call_id = self.next_call_id();
        self.send(routed_payload(
            Route::from_link(self.link()),
            Route::from_link(missing.clone()),
            call_id.clone(),
            payload.to_vec(),
        ))
        .await;

        MissingRouteProbe {
            call_id,
            source: self.link(),
            missing,
        }
    }

    pub(super) async fn expect_unreachable(&mut self, probe: MissingRouteProbe) {
        let msg = self.recv().await;
        let Message::Routed(RoutedFrame {
            src,
            dst,
            call_id: response_call_id,
            message:
                RoutedFrameMessage::RoutingError {
                    failed_route,
                    error:
                        ProtocolError::Unreachable {
                            message: error_message,
                        },
                },
        }) = msg
        else {
            panic!("expected routed unreachable error, got {msg:?}");
        };
        assert_eq!(response_call_id, probe.call_id);
        assert_eq!(src, Route::from_link(probe.source.clone()));
        assert!(dst.is_empty());
        assert_eq!(
            failed_route,
            Route::from_links([
                probe.source.as_str().to_string(),
                probe.missing.as_str().to_string()
            ])
            .unwrap()
        );
        assert_eq!(error_message, format!("route not found: {}", probe.missing));
    }

    pub(super) async fn send_opaque_to(
        &mut self,
        next_hop: Link,
        payload: &[u8],
    ) -> ForwardedProbe {
        let call_id = self.next_call_id();
        self.send(routed_payload(
            Route::from_link(self.link()),
            Route::from_link(next_hop.clone()),
            call_id.clone(),
            payload.to_vec(),
        ))
        .await;

        ForwardedProbe {
            call_id,
            source: self.link(),
            next_hop,
        }
    }

    pub(super) async fn send_opaque_with_spoofed_source(
        &mut self,
        spoofed_source: Link,
        next_hop: Link,
        payload: &[u8],
    ) {
        let call_id = self.next_call_id();
        self.send(routed_payload(
            Route::from_link(spoofed_source),
            Route::from_link(next_hop),
            call_id,
            payload.to_vec(),
        ))
        .await;
    }

    pub(super) async fn expect_no_message(&mut self) {
        let result = tokio::time::timeout(EXPECT_TIMEOUT / 10, async {
            self.transport
                .as_mut()
                .expect("test connection transport should be open")
                .read_message()
                .await
        })
        .await;
        assert!(
            result.is_err(),
            "expected no protocol message, got {result:?}"
        );
    }

    pub(super) async fn expect_forwarded_opaque(&mut self, probe: ForwardedProbe, payload: &[u8]) {
        let msg = self.recv().await;
        let Message::Routed(RoutedFrame {
            src,
            dst,
            call_id: forwarded_call_id,
            message: RoutedFrameMessage::Payload(forwarded_payload),
        }) = msg
        else {
            panic!("expected forwarded routed payload, got {msg:?}");
        };
        assert_eq!(forwarded_call_id, probe.call_id);
        assert_eq!(dst, Route::empty());
        assert_eq!(
            src,
            Route::from_links([
                probe.next_hop.as_str().to_string(),
                probe.source.as_str().to_string()
            ])
            .unwrap()
        );
        assert_eq!(forwarded_payload, payload);
    }

    pub(super) async fn expect_routed_request_method(&mut self, expected_method: &str) {
        let msg = self.recv().await;
        let Message::Routed(RoutedFrame {
            message: RoutedFrameMessage::Payload(payload),
            ..
        }) = msg
        else {
            panic!("expected routed request payload, got {msg:?}");
        };
        let FrameBody::Request(request) = wire::decode_frame_body(&payload).unwrap() else {
            panic!("expected routed request frame");
        };
        assert_eq!(request.method, expected_method);
    }

    pub(super) async fn subscribe_routing_events(&mut self) -> RoutingSubscription {
        let call_id = self.next_call_id();
        self.send(peer_request(
            call_id.clone(),
            method::ROUTING_SUBSCRIBE_EVENTS_NAME,
            wire::SubscribeRoutingEventsRequest {}.encode_to_vec(),
        ))
        .await;
        RoutingSubscription { call_id }
    }

    pub(super) async fn subscribe_agent_events(&mut self, host_id: Uuid) -> AgentSubscription {
        let call_id = self.next_call_id();
        let request = wire::SubscribeAgentEventsRequest {
            host_id: host_id.as_bytes().to_vec(),
        }
        .encode_to_vec();
        let payload = wire::encode_frame_body(&FrameBody::Request(RequestFrame {
            method: method::AGENT_SUBSCRIBE_EVENTS_NAME.to_string(),
            payload: request,
        }))
        .unwrap();
        self.send(routed_payload(
            Route::from_link(self.link.clone()),
            Route::empty(),
            call_id.clone(),
            payload,
        ))
        .await;
        AgentSubscription { call_id }
    }

    pub(super) async fn expect_duplicate_routing_subscription_rejected(&mut self) {
        let call_id = self.next_call_id();
        self.send(peer_request(
            call_id.clone(),
            method::ROUTING_SUBSCRIBE_EVENTS_NAME,
            wire::SubscribeRoutingEventsRequest {}.encode_to_vec(),
        ))
        .await;

        let error = expect_peer_response_error(self.recv().await, &call_id);
        assert!(matches!(error, ProtocolError::AlreadyExists { .. }));
    }
}

pub(super) struct TestRpcClient {
    rpc: RpcClient,
    task: JoinHandle<super::connection::Result<()>>,
}

impl TestRpcClient {
    pub(super) async fn create_test_agent(&mut self, agent_id: Uuid, name: &str) -> Agent {
        tokio::time::timeout(
            EXPECT_TIMEOUT,
            self.rpc.create_agent(&CreateAgentRequest {
                agent_id,
                name: Some(name.to_string()),
                agent_type: AgentType::TestAgent {
                    command: TEST_ECHO_COMMAND.to_string(),
                },
                working_dir: std::env::temp_dir(),
                terminal_size: None,
                args: Vec::new(),
            }),
        )
        .await
        .expect("timed out waiting for CreateAgent response")
        .unwrap()
    }

    pub(super) async fn rename_agent(&mut self, agent_id: Uuid, route: Route, name: &str) -> Agent {
        tokio::time::timeout(
            EXPECT_TIMEOUT,
            self.rpc.rename_agent(agent_id, route, name.to_string()),
        )
        .await
        .expect("timed out waiting for RenameAgent response")
        .unwrap()
    }

    pub(super) async fn rename_agent_result(
        &mut self,
        agent_id: Uuid,
        route: Route,
        name: &str,
    ) -> Result<Agent, RpcClientError> {
        tokio::time::timeout(
            EXPECT_TIMEOUT,
            self.rpc.rename_agent(agent_id, route, name.to_string()),
        )
        .await
        .expect("timed out waiting for RenameAgent response")
    }

    pub(super) async fn delete_agent(&mut self, agent_id: Uuid, route: Route) {
        tokio::time::timeout(EXPECT_TIMEOUT, self.rpc.delete_agent(agent_id, route))
            .await
            .expect("timed out waiting for DeleteAgent response")
            .unwrap();
    }

    pub(super) async fn list_agents(&mut self) -> Vec<crate::protocol::Agent> {
        tokio::time::timeout(EXPECT_TIMEOUT, self.rpc.list_agents())
            .await
            .expect("timed out waiting for ListAgents response")
            .unwrap()
    }

    pub(super) async fn expect_agent_named(&mut self, name: &str) -> Agent {
        tokio::time::timeout(EXPECT_TIMEOUT, async {
            loop {
                if let Some(agent) = self
                    .list_agents()
                    .await
                    .into_iter()
                    .find(|agent| agent.name.as_deref() == Some(name))
                {
                    return agent;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for agent named {name}"))
    }

    pub(super) async fn expect_no_agent_named(&mut self, name: &str) {
        tokio::time::timeout(EXPECT_TIMEOUT, async {
            loop {
                if self
                    .list_agents()
                    .await
                    .into_iter()
                    .all(|agent| agent.name.as_deref() != Some(name))
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for agent named {name} to disappear"));
    }

    pub(super) async fn open_session(
        &mut self,
        agent_id: Uuid,
        io_protocol: &str,
    ) -> TestRpcSession {
        self.open_session_result(agent_id, io_protocol)
            .await
            .unwrap()
    }

    pub(super) async fn open_session_result(
        &mut self,
        agent_id: Uuid,
        io_protocol: &str,
    ) -> Result<TestRpcSession, RpcClientError> {
        let session = tokio::time::timeout(
            EXPECT_TIMEOUT,
            self.rpc
                .open_session(agent_id, Route::empty(), io_protocol, None),
        )
        .await
        .expect("timed out opening local OpenSession")?;
        Ok(TestRpcSession { inner: session })
    }

    pub(super) async fn open_agent_session(
        &mut self,
        agent: &Agent,
        io_protocol: &str,
    ) -> TestRpcSession {
        let session = tokio::time::timeout(
            EXPECT_TIMEOUT,
            self.rpc
                .open_session(agent.id, agent.route.clone(), io_protocol, None),
        )
        .await
        .expect("timed out opening agent OpenSession")
        .unwrap();
        TestRpcSession { inner: session }
    }

    pub(super) async fn close(self) -> super::connection::Result<()> {
        drop(self.rpc);
        self.task.await.expect("connection task panicked")
    }

    pub(super) async fn close_after_session(
        self,
        session: TestRpcSession,
    ) -> super::connection::Result<()> {
        drop(session);
        self.close().await
    }
}

pub(super) struct TestRpcSession {
    inner: OpenSessionClient,
}

impl TestRpcSession {
    async fn recv(&self) -> OpenSessionServerFrame {
        tokio::time::timeout(EXPECT_TIMEOUT, self.inner.recv())
            .await
            .expect("timed out waiting for OpenSession frame")
            .unwrap()
    }

    pub(super) async fn expect_replay_complete(&self) {
        assert_eq!(
            self.recv().await,
            OpenSessionServerFrame::Event(OpenSessionOutputEvent::ReplayComplete { cursor: None })
        );
    }

    pub(super) async fn send_bytes(&self, bytes: &[u8]) {
        self.inner.send_raw_input(bytes.to_vec()).await.unwrap();
    }

    pub(super) async fn expect_output_bytes(&self, bytes: &[u8]) {
        assert_eq!(
            self.recv().await,
            OpenSessionServerFrame::Event(OpenSessionOutputEvent::Output {
                payload: bytes.to_vec(),
            })
        );
    }

    pub(super) async fn cancel(&self) {
        self.inner.cancel().await.unwrap();
    }

    pub(super) async fn expect_terminal_cancelled(&self) {
        let OpenSessionServerFrame::Response(Err(ProtocolError::Cancelled { .. })) =
            self.recv().await
        else {
            panic!("expected cancelled OpenSession response");
        };
    }

    pub(super) async fn expect_send_bytes_rejected(&self, bytes: &[u8]) {
        match self.inner.send_raw_input(bytes.to_vec()).await {
            Err(RpcClientError::Unexpected { method, message })
                if method == crate::protocol::method::AGENT_OPEN_SESSION_NAME
                    && message.contains("not active") => {}
            Ok(()) => panic!("expected OpenSession send after terminal response to be rejected"),
            Err(error) => panic!("expected inactive OpenSession error, got {error}"),
        }
    }

    pub(super) async fn expect_route_unreachable(&self) {
        match tokio::time::timeout(EXPECT_TIMEOUT, self.inner.recv())
            .await
            .expect("timed out waiting for OpenSession routing error")
        {
            Err(RpcClientError::Protocol(ProtocolError::Unreachable { .. })) => {}
            Ok(frame) => panic!("expected OpenSession routing error, got {frame:?}"),
            Err(error) => panic!("expected OpenSession unreachable error, got {error}"),
        }
    }
}

pub(super) struct QueuedOpenSession {
    call_id: CallId,
}

impl QueuedOpenSession {
    async fn recv(&self, client: &mut TestConnection) -> OpenSessionServerFrame {
        let msg = client.recv().await;
        let Message::Routed(RoutedFrame {
            call_id,
            message: RoutedFrameMessage::Payload(payload),
            ..
        }) = msg
        else {
            panic!("expected OpenSession routed frame, got {msg:?}");
        };
        assert_eq!(call_id, self.call_id);
        open_session::decode_open_session_server_frame(&payload).unwrap()
    }

    pub(super) async fn expect_opened(&self, client: &mut TestConnection) {
        assert_eq!(
            self.recv(client).await,
            OpenSessionServerFrame::Event(OpenSessionOutputEvent::Opened)
        );
    }

    pub(super) async fn expect_replay_complete(&self, client: &mut TestConnection) {
        assert_eq!(
            self.recv(client).await,
            OpenSessionServerFrame::Event(OpenSessionOutputEvent::ReplayComplete { cursor: None })
        );
    }

    pub(super) async fn expect_output_bytes(&self, client: &mut TestConnection, bytes: &[u8]) {
        assert_eq!(
            self.recv(client).await,
            OpenSessionServerFrame::Event(OpenSessionOutputEvent::Output {
                payload: bytes.to_vec(),
            })
        );
    }

    pub(super) async fn cancel(&self, client: &mut TestConnection) {
        client
            .send_local_routed_payload(
                self.call_id.clone(),
                open_session::encode_open_session_cancel().unwrap(),
            )
            .await;
    }

    pub(super) async fn expect_terminal_cancelled(&self, client: &mut TestConnection) {
        let OpenSessionServerFrame::Response(Err(ProtocolError::Cancelled { .. })) =
            self.recv(client).await
        else {
            panic!("expected cancelled OpenSession response");
        };
    }
}

pub(super) struct MissingRouteProbe {
    call_id: CallId,
    source: Link,
    missing: Link,
}

pub(super) struct ForwardedProbe {
    call_id: CallId,
    source: Link,
    next_hop: Link,
}

pub(super) struct RoutingSubscription {
    call_id: CallId,
}

impl RoutingSubscription {
    pub(super) async fn expect_snapshot_complete(&self, peer: &mut TestConnection) {
        let event = expect_peer_routing_event(peer.recv().await, &self.call_id);
        assert!(matches!(event, RoutingEvent::SnapshotComplete));
    }

    pub(super) async fn expect_host_up(
        &self,
        peer: &mut TestConnection,
        host_id: Uuid,
        name: &str,
        route: Route,
    ) {
        let event = expect_peer_routing_event(peer.recv().await, &self.call_id);
        let RoutingEvent::HostUp {
            host,
            route: event_route,
        } = event
        else {
            panic!("expected HostUp live event, got {event:?}");
        };
        assert_eq!(host.id, host_id);
        assert_eq!(host.name, name);
        assert_eq!(event_route, route);
    }

    pub(super) async fn expect_host_down(
        &self,
        peer: &mut TestConnection,
        host_id: Uuid,
        route: Route,
    ) {
        let event = expect_peer_routing_event(peer.recv().await, &self.call_id);
        let RoutingEvent::HostDown {
            id,
            route: event_route,
        } = event
        else {
            panic!("expected HostDown live event, got {event:?}");
        };
        assert_eq!(id, host_id);
        assert_eq!(event_route, route);
    }
}

pub(super) struct AgentSubscription {
    call_id: CallId,
}

impl AgentSubscription {
    pub(super) async fn expect_snapshot_complete(&self, peer: &mut TestConnection) {
        let event = expect_routed_agent_event(peer.recv().await, &self.call_id);
        assert!(matches!(event, AgentEvent::SnapshotComplete));
    }

    pub(super) async fn expect_agent_up(
        &self,
        peer: &mut TestConnection,
        agent_id: Uuid,
        name: &str,
        io_protocol: &str,
    ) {
        let event = expect_routed_agent_event(peer.recv().await, &self.call_id);
        let AgentEvent::AgentUp {
            agent_id: event_agent_id,
            name: event_name,
            io_protocols,
            ..
        } = event
        else {
            panic!("expected AgentUp live event, got {event:?}");
        };
        assert_eq!(event_agent_id, agent_id);
        assert_eq!(event_name.as_deref(), Some(name));
        assert!(
            io_protocols.iter().any(|protocol| protocol == io_protocol),
            "expected io_protocols {io_protocols:?} to include {io_protocol}"
        );
    }

    pub(super) async fn expect_agent_down(&self, peer: &mut TestConnection, agent_id: Uuid) {
        let event = expect_routed_agent_event(peer.recv().await, &self.call_id);
        let AgentEvent::AgentDown {
            agent_id: event_agent_id,
            ..
        } = event
        else {
            panic!("expected AgentDown live event, got {event:?}");
        };
        assert_eq!(event_agent_id, agent_id);
    }

    pub(super) async fn expect_error(&self, peer: &mut TestConnection) -> ProtocolError {
        expect_routed_response_error(peer.recv().await, &self.call_id)
    }
}

fn call_id(value: u128) -> CallId {
    CallId::from(Uuid::from_u128(value))
}

fn peer_request(call_id: CallId, method: &'static str, payload: Vec<u8>) -> Message {
    Message::Peer(PeerFrame {
        call_id,
        body: FrameBody::Request(RequestFrame {
            method: method.to_string(),
            payload,
        }),
    })
}

fn routed_payload(src: Route, dst: Route, call_id: CallId, payload: Vec<u8>) -> Message {
    Message::Routed(RoutedFrame {
        src,
        dst,
        call_id,
        message: RoutedFrameMessage::Payload(payload),
    })
}

fn expect_peer_routing_event(msg: Message, call_id: &CallId) -> RoutingEvent {
    let Message::Peer(PeerFrame {
        call_id: response_call_id,
        body: FrameBody::StreamItem(payload),
    }) = msg
    else {
        panic!("expected peer routing stream item, got {msg:?}");
    };
    assert_eq!(&response_call_id, call_id);
    wire::decode_routing_event(&payload).unwrap()
}

fn expect_routed_agent_event(msg: Message, call_id: &CallId) -> AgentEvent {
    let Message::Routed(RoutedFrame {
        call_id: response_call_id,
        message: RoutedFrameMessage::Payload(payload),
        ..
    }) = msg
    else {
        panic!("expected routed agent stream item, got {msg:?}");
    };
    assert_eq!(&response_call_id, call_id);
    let FrameBody::StreamItem(payload) = wire::decode_frame_body(&payload).unwrap() else {
        panic!("expected routed stream item frame");
    };
    wire::decode_agent_event(&payload).unwrap()
}

fn expect_routed_response_error(msg: Message, call_id: &CallId) -> ProtocolError {
    let Message::Routed(RoutedFrame {
        call_id: response_call_id,
        message: RoutedFrameMessage::Payload(payload),
        ..
    }) = msg
    else {
        panic!("expected routed response error, got {msg:?}");
    };
    assert_eq!(&response_call_id, call_id);
    let FrameBody::Response(ResponseFrame::Error(error)) =
        wire::decode_frame_body(&payload).unwrap()
    else {
        panic!("expected routed response error frame");
    };
    error
}

fn expect_peer_response_error(msg: Message, call_id: &CallId) -> ProtocolError {
    let Message::Peer(PeerFrame {
        call_id: response_call_id,
        body: FrameBody::Response(ResponseFrame::Error(error)),
    }) = msg
    else {
        panic!("expected peer response error, got {msg:?}");
    };
    assert_eq!(&response_call_id, call_id);
    error
}

mod runtime;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::Stream;
use prost::Message as _;
use thiserror::Error;
use uuid::Uuid;

use super::Connection;
use crate::protocol::message::{
    AgentEvent, CreateAgentRequest, DebugFormat, FrameBody, HookProvider, ProtocolError,
    RenameAgentRequest, ResponseFrame, RoutingEvent, ShutdownReason,
};
use crate::protocol::session::{self, SubscribeSessionEvent, SubscribeSessionFrame};
use crate::protocol::{AgentEntry, Route, agent_lifecycle, method, wire};
use crate::transport::TransportError;
use crate::{AgentIdentifier, ConnectToServerTarget, SendInputRequest, SubscribeSessionRequest};

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("server shutdown: {0}")]
    ServerShutdown(ShutdownReason),
    #[error("failed to encode {method} request: {message}")]
    Encode {
        method: &'static str,
        message: String,
    },
    #[error("failed to decode {method} response: {message}")]
    Decode {
        method: &'static str,
        message: String,
    },
    #[error("unexpected response to {method}: {message}")]
    Unexpected {
        method: &'static str,
        message: String,
    },
    #[error("agent not found: {0:?}")]
    AgentNotFound(AgentIdentifier),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SuspendSummary {
    pub suspended_count: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResumeSummary {
    pub resumed_count: u64,
    pub failed_count: u64,
}

type StreamFuture<T> = Pin<Box<dyn Future<Output = Result<T, ClientError>> + Send>>;

pub struct SubscribeSessionClient {
    stream: Arc<runtime::EndpointOutputStream>,
    pending: Mutex<Option<StreamFuture<SubscribeSessionFrame>>>,
    done: Mutex<bool>,
}

pub type SessionStream = SubscribeSessionClient;

pub struct RoutingEventStream {
    stream: Arc<runtime::EndpointOutputStream>,
    pending: Mutex<Option<StreamFuture<RoutingEvent>>>,
    done: Mutex<bool>,
}

pub struct AgentEventStream {
    stream: Arc<runtime::EndpointOutputStream>,
    pending: Mutex<Option<StreamFuture<AgentEvent>>>,
    done: Mutex<bool>,
}

impl SubscribeSessionClient {
    pub async fn cancel(&self) -> Result<(), ClientError> {
        self.stream
            .send_cancel(method::AGENT_SUBSCRIBE_SESSION_NAME)
            .await
    }

    pub async fn recv(&self) -> Result<SubscribeSessionFrame, ClientError> {
        recv_subscribe_session_frame(self.stream.clone()).await
    }
}

impl Stream for SubscribeSessionClient {
    type Item = Result<SubscribeSessionFrame, ClientError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        poll_stream_item(
            self.get_mut(),
            |stream| Box::pin(recv_subscribe_session_frame(stream)),
            |this| &this.pending,
            |this| this.stream.clone(),
            |this| &this.done,
            subscribe_session_stream_item_is_terminal,
            cx,
        )
    }
}

impl RoutingEventStream {
    pub async fn cancel(&self) -> Result<(), ClientError> {
        self.stream
            .send_cancel(method::ROUTING_SUBSCRIBE_EVENTS_NAME)
            .await
    }

    pub async fn recv(&self) -> Result<RoutingEvent, ClientError> {
        recv_routing_event(self.stream.clone()).await
    }
}

impl Stream for RoutingEventStream {
    type Item = Result<RoutingEvent, ClientError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        poll_stream_item(
            self.get_mut(),
            |stream| Box::pin(recv_routing_event(stream)),
            |this| &this.pending,
            |this| this.stream.clone(),
            |this| &this.done,
            result_is_terminal,
            cx,
        )
    }
}

impl AgentEventStream {
    pub async fn cancel(&self) -> Result<(), ClientError> {
        self.stream
            .send_cancel(method::AGENT_SUBSCRIBE_EVENTS_NAME)
            .await
    }

    pub async fn recv(&self) -> Result<AgentEvent, ClientError> {
        recv_agent_event(self.stream.clone()).await
    }
}

impl Stream for AgentEventStream {
    type Item = Result<AgentEvent, ClientError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        poll_stream_item(
            self.get_mut(),
            |stream| Box::pin(recv_agent_event(stream)),
            |this| &this.pending,
            |this| this.stream.clone(),
            |this| &this.done,
            result_is_terminal,
            cx,
        )
    }
}

fn poll_stream_item<T, S>(
    this: &mut S,
    start: impl FnOnce(Arc<runtime::EndpointOutputStream>) -> StreamFuture<T>,
    pending: impl FnOnce(&S) -> &Mutex<Option<StreamFuture<T>>>,
    stream: impl FnOnce(&S) -> Arc<runtime::EndpointOutputStream>,
    done: impl FnOnce(&S) -> &Mutex<bool>,
    is_terminal: impl FnOnce(&Result<T, ClientError>) -> bool,
    cx: &mut Context<'_>,
) -> Poll<Option<Result<T, ClientError>>> {
    let done = done(this);
    if *done.lock().expect("stream done mutex poisoned") {
        return Poll::Ready(None);
    }
    let stream = stream(this);
    let mut pending = pending(this).lock().expect("stream future mutex poisoned");
    if pending.is_none() {
        *pending = Some(start(stream));
    }
    let result = pending
        .as_mut()
        .expect("stream future should be present")
        .as_mut()
        .poll(cx);
    match result {
        Poll::Ready(result) => {
            *pending = None;
            if is_terminal(&result) {
                *done.lock().expect("stream done mutex poisoned") = true;
            }
            Poll::Ready(Some(result))
        }
        Poll::Pending => Poll::Pending,
    }
}

fn result_is_terminal<T>(result: &Result<T, ClientError>) -> bool {
    result.is_err()
}

fn subscribe_session_stream_item_is_terminal(
    result: &Result<SubscribeSessionFrame, ClientError>,
) -> bool {
    match result {
        Ok(SubscribeSessionFrame::Response(_)) | Err(_) => true,
        Ok(SubscribeSessionFrame::Event(_)) => false,
    }
}

async fn recv_subscribe_session_frame(
    stream: Arc<runtime::EndpointOutputStream>,
) -> Result<SubscribeSessionFrame, ClientError> {
    let body = stream.recv_frame_body().await?;
    let frame = session::decode_subscribe_session_frame_body(body).map_err(|error| {
        stream.finish();
        ClientError::Decode {
            method: method::AGENT_SUBSCRIBE_SESSION_NAME,
            message: error.to_string(),
        }
    })?;
    if matches!(frame, SubscribeSessionFrame::Response(_)) {
        stream.finish();
    }
    Ok(frame)
}

async fn recv_routing_event(
    stream: Arc<runtime::EndpointOutputStream>,
) -> Result<RoutingEvent, ClientError> {
    let body = stream.recv_frame_body().await?;
    match body {
        FrameBody::StreamItem(payload) => wire::decode_routing_event(&payload).map_err(|error| {
            stream.finish();
            ClientError::Decode {
                method: method::ROUTING_SUBSCRIBE_EVENTS_NAME,
                message: error.to_string(),
            }
        }),
        FrameBody::Response(ResponseFrame::Error(error)) => {
            stream.finish();
            Err(error.into())
        }
        other => {
            stream.finish();
            Err(ClientError::Unexpected {
                method: method::ROUTING_SUBSCRIBE_EVENTS_NAME,
                message: format!("expected routing stream item, got {other:?}"),
            })
        }
    }
}

async fn recv_agent_event(
    stream: Arc<runtime::EndpointOutputStream>,
) -> Result<AgentEvent, ClientError> {
    let body = stream.recv_frame_body().await?;
    match body {
        FrameBody::StreamItem(payload) => wire::decode_agent_event(&payload).map_err(|error| {
            stream.finish();
            ClientError::Decode {
                method: method::AGENT_SUBSCRIBE_EVENTS_NAME,
                message: error.to_string(),
            }
        }),
        FrameBody::Response(ResponseFrame::Error(error)) => {
            stream.finish();
            Err(error.into())
        }
        other => {
            stream.finish();
            Err(ClientError::Unexpected {
                method: method::AGENT_SUBSCRIBE_EVENTS_NAME,
                message: format!("expected agent stream item, got {other:?}"),
            })
        }
    }
}

/// Operation-oriented client for the local amux RPC surface.
///
/// This is intentionally not a frame helper. Callers invoke domain operations;
/// the client owns local call IDs and response matching.
#[derive(Clone)]
pub struct Client {
    runtime: runtime::ClientRuntime,
    guard: Option<Arc<dyn Send + Sync>>,
}

impl Client {
    pub(crate) fn new(connection: Connection) -> Self {
        Self {
            runtime: runtime::ClientRuntime::new(connection),
            guard: None,
        }
    }

    pub(crate) fn new_with_guard(connection: Connection, guard: Arc<dyn Send + Sync>) -> Self {
        Self {
            runtime: runtime::ClientRuntime::new(connection),
            guard: Some(guard),
        }
    }

    pub fn owns_embedded_server(&self) -> bool {
        self.guard.is_some()
    }

    pub async fn create_agent(
        &self,
        request: CreateAgentRequest,
    ) -> Result<AgentEntry, ClientError> {
        let payload = agent_lifecycle_request_payload(
            method::AGENT_CREATE_NAME,
            agent_lifecycle::encode_create_agent_request(&request),
        )?;
        let agent_route = Route::empty();
        let response = self
            .runtime
            .call_endpoint_unary(
                method::AGENT_CREATE,
                self.route_to_agent(agent_route.clone()),
                payload,
            )
            .await?;
        match agent_lifecycle::decode_create_agent_response(response, agent_route).map_err(
            |error| ClientError::Decode {
                method: method::AGENT_CREATE_NAME,
                message: error.to_string(),
            },
        )? {
            Ok(agent) => Ok(agent),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn rename_agent(
        &self,
        agent_id: Uuid,
        name: String,
    ) -> Result<AgentEntry, ClientError> {
        let entry = self
            .resolve_agent(AgentIdentifier::Id(agent_id))
            .await
            .map_err(|error| match error {
                ClientError::AgentNotFound(_) => {
                    ClientError::AgentNotFound(AgentIdentifier::Id(agent_id))
                }
                other => other,
            })?;
        self.rename_agent_on_route(agent_id, entry.route, name)
            .await
    }

    pub(crate) async fn rename_agent_on_route(
        &self,
        agent_id: Uuid,
        agent_route: Route,
        name: String,
    ) -> Result<AgentEntry, ClientError> {
        let payload = agent_lifecycle_request_payload(
            method::AGENT_RENAME_NAME,
            agent_lifecycle::encode_rename_agent_request(&RenameAgentRequest { agent_id, name }),
        )?;
        let response = self
            .runtime
            .call_endpoint_unary(
                method::AGENT_RENAME,
                self.route_to_agent(agent_route.clone()),
                payload,
            )
            .await?;
        match agent_lifecycle::decode_rename_agent_response(response, agent_route).map_err(
            |error| ClientError::Decode {
                method: method::AGENT_RENAME_NAME,
                message: error.to_string(),
            },
        )? {
            Ok(agent) => Ok(agent),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn delete_agent(&self, agent_id: Uuid) -> Result<(), ClientError> {
        let entry = self.resolve_agent(AgentIdentifier::Id(agent_id)).await?;
        self.delete_agent_on_route(agent_id, entry.route).await
    }

    pub(crate) async fn delete_agent_on_route(
        &self,
        agent_id: Uuid,
        agent_route: Route,
    ) -> Result<(), ClientError> {
        let payload = agent_lifecycle_request_payload(
            method::AGENT_DELETE_NAME,
            agent_lifecycle::encode_delete_agent_request(agent_id),
        )?;
        let response = self
            .runtime
            .call_endpoint_unary(
                method::AGENT_DELETE,
                self.route_to_agent(agent_route),
                payload,
            )
            .await?;
        match agent_lifecycle::decode_delete_agent_response(response).map_err(|error| {
            ClientError::Decode {
                method: method::AGENT_DELETE_NAME,
                message: error.to_string(),
            }
        })? {
            Ok(()) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn subscribe_session(
        &self,
        request: SubscribeSessionRequest,
    ) -> Result<SessionStream, ClientError> {
        self.subscribe_session_on_route(
            request.id,
            request.route,
            request.io_protocol,
            request.args.map(|args| args.to_vec()),
        )
        .await
    }

    async fn subscribe_session_on_route(
        &self,
        agent_id: Uuid,
        agent_route: Route,
        io_protocol: impl Into<String>,
        args: Option<Vec<u8>>,
    ) -> Result<SessionStream, ClientError> {
        let full_route = route_to_agent_with_link(agent_route, self.runtime.link().clone());
        let io_protocol = io_protocol.into();
        let stream = self
            .runtime
            .start_endpoint_stream(
                method::AGENT_SUBSCRIBE_SESSION,
                full_route,
                session_request_payload(
                    method::AGENT_SUBSCRIBE_SESSION_NAME,
                    session::encode_subscribe_session_request(agent_id, io_protocol, args),
                )?,
            )
            .await?;
        let session = SubscribeSessionClient {
            stream: Arc::new(stream),
            pending: Mutex::new(None),
            done: Mutex::new(false),
        };

        match session.recv().await? {
            SubscribeSessionFrame::Event(SubscribeSessionEvent::Opened) => {
                session.stream.set_active();
                Ok(session)
            }
            SubscribeSessionFrame::Event(event) => {
                session.stream.finish();
                Err(ClientError::Unexpected {
                    method: method::AGENT_SUBSCRIBE_SESSION_NAME,
                    message: format!("expected SessionOpened, got {event:?}"),
                })
            }
            SubscribeSessionFrame::Response(Ok(())) => Err(ClientError::Unexpected {
                method: method::AGENT_SUBSCRIBE_SESSION_NAME,
                message: "session ended before opening".to_string(),
            }),
            SubscribeSessionFrame::Response(Err(error)) => Err(error.into()),
        }
    }

    pub async fn send_input(&self, request: SendInputRequest) -> Result<(), ClientError> {
        self.send_input_to_route(
            request.id,
            request.route,
            request.io_protocol,
            request.payload,
        )
        .await
    }

    async fn send_input_to_route(
        &self,
        agent_id: Uuid,
        agent_route: Route,
        io_protocol: impl Into<String>,
        payload: impl Into<Bytes>,
    ) -> Result<(), ClientError> {
        let full_route = self.route_to_agent(agent_route);
        let payload = payload.into();
        let payload = session_request_payload(
            method::AGENT_SEND_INPUT_NAME,
            session::encode_send_input_request(agent_id, io_protocol, payload.to_vec()),
        )?;
        let response = self
            .runtime
            .call_endpoint_unary_payload(method::AGENT_SEND_INPUT, full_route, payload)
            .await?;
        decode_payload::<wire::SendInputResponse>(method::AGENT_SEND_INPUT_NAME, response)
            .map(|_| ())
    }

    pub async fn list_agents(&self) -> Result<Vec<AgentEntry>, ClientError> {
        let body = self
            .runtime
            .local_unary_payload(
                method::AGENT_LIST,
                wire::ListAgentsRequest {}.encode_to_vec(),
            )
            .await?;
        let response = decode_payload::<wire::ListAgentsResponse>(method::AGENT_LIST_NAME, body)?;
        response
            .agents
            .into_iter()
            .map(|entry| decode_agent_entry(method::AGENT_LIST_NAME, entry))
            .collect()
    }

    pub async fn resolve_agent(
        &self,
        identifier: AgentIdentifier,
    ) -> Result<AgentEntry, ClientError> {
        self.try_resolve_agent(identifier.clone())
            .await?
            .ok_or(ClientError::AgentNotFound(identifier))
    }

    async fn try_resolve_agent(
        &self,
        identifier: impl Into<AgentIdentifier>,
    ) -> Result<Option<AgentEntry>, ClientError> {
        let identifier = match identifier.into() {
            AgentIdentifier::Id(agent_id) => {
                wire::resolve_agent_request::Identifier::AgentId(agent_id.as_bytes().to_vec())
            }
            AgentIdentifier::Name(name) => wire::resolve_agent_request::Identifier::AgentName(name),
        };
        let body = self
            .runtime
            .local_unary_payload(
                method::AGENT_RESOLVE,
                wire::ResolveAgentRequest {
                    identifier: Some(identifier),
                }
                .encode_to_vec(),
            )
            .await?;
        let response =
            decode_payload::<wire::ResolveAgentResponse>(method::AGENT_RESOLVE_NAME, body)?;
        response
            .agent
            .map(|entry| decode_agent_entry(method::AGENT_RESOLVE_NAME, entry))
            .transpose()
    }

    pub async fn shutdown(&self, _reason: ShutdownReason) -> Result<(), ClientError> {
        let body = self
            .runtime
            .local_unary_payload(
                method::ADMIN_SHUTDOWN,
                wire::ShutdownRequest {}.encode_to_vec(),
            )
            .await?;
        decode_empty(method::ADMIN_SHUTDOWN_NAME, body)
    }

    pub async fn suspend(&self) -> Result<SuspendSummary, ClientError> {
        self.suspend_with_reason(wire::SuspendReason::User).await
    }

    pub async fn suspend_for_update(&self) -> Result<SuspendSummary, ClientError> {
        self.suspend_with_reason(wire::SuspendReason::Update).await
    }

    async fn suspend_with_reason(
        &self,
        reason: wire::SuspendReason,
    ) -> Result<SuspendSummary, ClientError> {
        let body = self
            .runtime
            .local_unary_payload(
                method::ADMIN_SUSPEND,
                wire::SuspendRequest {
                    reason: reason as i32,
                }
                .encode_to_vec(),
            )
            .await?;
        let response = decode_payload::<wire::SuspendResponse>(method::ADMIN_SUSPEND_NAME, body)?;
        Ok(SuspendSummary {
            suspended_count: response.suspended_count,
        })
    }

    pub async fn resume(&self) -> Result<ResumeSummary, ClientError> {
        let body = self
            .runtime
            .local_unary_payload(method::ADMIN_RESUME, wire::ResumeRequest {}.encode_to_vec())
            .await?;
        let response = decode_payload::<wire::ResumeResponse>(method::ADMIN_RESUME_NAME, body)?;
        Ok(ResumeSummary {
            resumed_count: response.resumed_count,
            failed_count: response.failed_count,
        })
    }

    pub async fn connect_to_server(
        &self,
        address: ConnectToServerTarget,
    ) -> Result<(), ClientError> {
        let body = self
            .runtime
            .local_unary_payload(
                method::ADMIN_CONNECT_TO_SERVER,
                wire::ConnectToServerRequest { address }.encode_to_vec(),
            )
            .await?;
        decode_empty(method::ADMIN_CONNECT_TO_SERVER_NAME, body)
    }

    pub async fn debug_dump(&self, format: DebugFormat) -> Result<String, ClientError> {
        self.debug_dump_verbose(false, format).await
    }

    pub async fn debug_dump_verbose(
        &self,
        verbose: bool,
        format: DebugFormat,
    ) -> Result<String, ClientError> {
        let body = self
            .runtime
            .local_unary_payload(
                method::ADMIN_DEBUG,
                wire::DebugRequest {
                    verbose,
                    format: debug_format_to_wire(format),
                }
                .encode_to_vec(),
            )
            .await?;
        let response = decode_payload::<wire::DebugResponse>(method::ADMIN_DEBUG_NAME, body)?;
        Ok(response.dump)
    }

    pub async fn handle_hook(
        &self,
        provider: HookProvider,
        payload: Bytes,
    ) -> Result<Bytes, ClientError> {
        let (agent_id, external) = hook_target_from_payload(provider, &payload)?;
        self.enqueue_hook_for_agent(agent_id, provider, payload, external)
            .await?;
        Ok(Bytes::new())
    }

    async fn enqueue_hook_for_agent(
        &self,
        agent_id: Uuid,
        provider: HookProvider,
        payload: impl Into<Bytes>,
        external: bool,
    ) -> Result<(), ClientError> {
        let body = self
            .runtime
            .local_unary_payload(
                method::HOOK_HANDLE,
                wire::HandleHookRequest {
                    agent_id: agent_id.as_bytes().to_vec(),
                    provider: hook_provider_to_wire(provider),
                    payload: payload.into().to_vec(),
                    external,
                }
                .encode_to_vec(),
            )
            .await?;
        decode_payload::<wire::HandleHookResponse>(method::HOOK_HANDLE_NAME, body).map(|_| ())
    }

    pub async fn subscribe_routing_events(&self) -> Result<RoutingEventStream, ClientError> {
        let mut full_route = Route::empty();
        full_route.push(self.runtime.link().clone());
        let stream = self
            .runtime
            .start_endpoint_stream(
                method::ROUTING_SUBSCRIBE_EVENTS,
                full_route,
                wire::SubscribeRoutingEventsRequest {}.encode_to_vec(),
            )
            .await?;
        stream.set_active();
        Ok(RoutingEventStream {
            stream: Arc::new(stream),
            pending: Mutex::new(None),
            done: Mutex::new(false),
        })
    }

    pub async fn subscribe_agent_events(
        &self,
        host_id: Uuid,
    ) -> Result<AgentEventStream, ClientError> {
        let mut full_route = Route::empty();
        full_route.push(self.runtime.link().clone());
        let stream = self
            .runtime
            .start_endpoint_stream(
                method::AGENT_SUBSCRIBE_EVENTS,
                full_route,
                wire::SubscribeAgentEventsRequest {
                    host_id: host_id.as_bytes().to_vec(),
                }
                .encode_to_vec(),
            )
            .await?;
        stream.set_active();
        Ok(AgentEventStream {
            stream: Arc::new(stream),
            pending: Mutex::new(None),
            done: Mutex::new(false),
        })
    }

    fn route_to_agent(&self, agent_route: Route) -> Route {
        route_to_agent_with_link(agent_route, self.runtime.link().clone())
    }
}

fn hook_target_from_payload(
    provider: HookProvider,
    payload: &[u8],
) -> Result<(Uuid, bool), ClientError> {
    if let Ok(agent_id) = std::env::var("AMUX_AGENT_ID") {
        return agent_id
            .parse::<Uuid>()
            .map(|agent_id| (agent_id, false))
            .map_err(|error| ClientError::Encode {
                method: method::HOOK_HANDLE_NAME,
                message: format!("invalid AMUX_AGENT_ID: {error}"),
            });
    }

    match provider {
        HookProvider::Claude => serde_json::from_slice::<serde_json::Value>(payload)
            .map_err(|error| ClientError::Encode {
                method: method::HOOK_HANDLE_NAME,
                message: format!("invalid Claude hook payload: {error}"),
            })
            .and_then(|value| {
                value
                    .get("session_id")
                    .and_then(|id| id.as_str())
                    .ok_or_else(|| ClientError::Encode {
                        method: method::HOOK_HANDLE_NAME,
                        message: "Claude hook payload missing session_id".to_string(),
                    })
                    .and_then(|id| {
                        id.parse::<Uuid>().map_err(|error| ClientError::Encode {
                            method: method::HOOK_HANDLE_NAME,
                            message: format!("invalid Claude hook session_id: {error}"),
                        })
                    })
            })
            .map(|agent_id| (agent_id, true)),
        HookProvider::Unknown => Err(ClientError::Encode {
            method: method::HOOK_HANDLE_NAME,
            message: "unknown hook provider".to_string(),
        }),
    }
}

fn debug_format_to_wire(format: DebugFormat) -> i32 {
    match format {
        DebugFormat::Yaml => wire::DebugFormat::Yaml as i32,
        DebugFormat::Json => wire::DebugFormat::Json as i32,
    }
}

fn hook_provider_to_wire(provider: HookProvider) -> i32 {
    match provider {
        HookProvider::Claude => wire::HookProvider::Claude as i32,
        HookProvider::Unknown => wire::HookProvider::Unspecified as i32,
    }
}

fn decode_empty(method: &'static str, body: Vec<u8>) -> Result<(), ClientError> {
    decode_payload::<wire::Empty>(method, body).map(|_| ())
}

fn decode_payload<M>(method: &'static str, body: Vec<u8>) -> Result<M, ClientError>
where
    M: prost::Message + Default,
{
    M::decode(body.as_slice()).map_err(|error| ClientError::Decode {
        method,
        message: error.to_string(),
    })
}

fn decode_agent_entry(
    method: &'static str,
    entry: wire::AgentEntry,
) -> Result<AgentEntry, ClientError> {
    wire::agent_entry_to_domain(entry).map_err(|error| ClientError::Decode {
        method,
        message: error.to_string(),
    })
}

fn route_to_agent_with_link(mut agent_route: Route, local_link: crate::protocol::Link) -> Route {
    agent_route.push(local_link);
    agent_route
}

fn agent_lifecycle_request_payload(
    method: &'static str,
    payload: Result<Vec<u8>, agent_lifecycle::AgentLifecycleCodecError>,
) -> Result<Vec<u8>, ClientError> {
    payload.map_err(|error| ClientError::Encode {
        method,
        message: error.to_string(),
    })
}

fn session_request_payload(
    method: &'static str,
    payload: Result<Vec<u8>, session::SessionCodecError>,
) -> Result<Vec<u8>, ClientError> {
    payload.map_err(|error| ClientError::Encode {
        method,
        message: error.to_string(),
    })
}

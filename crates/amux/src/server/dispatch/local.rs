use tokio::sync::mpsc;
use uuid::Uuid;

use crate::protocol::message::{
    CallId, DebugFormat, FrameBody, HookProvider, LocalFrame, Message, ProtocolError, RequestFrame,
    ResponseFrame, ShutdownReason,
};
use crate::protocol::method::{MethodLookupError, MethodScope};
use crate::protocol::{method, wire};
use crate::server::ShutdownRequest;
use crate::server::accept::tcp_connect;
use crate::server::connection::{ConnectionContext, ConnectionError};
use crate::server::routing::resume_agents;
use crate::services::{
    AdminService, AdminServiceCtx, AgentService, AgentServiceCtx, HookService, HookServiceCtx,
};

pub(super) async fn handle_local_request(
    tx: &mpsc::Sender<Message>,
    call_id: CallId,
    request: RequestFrame,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let endpoint = LocalEndpointCall::new(tx, call_id);
    match handle_local_request_inner(&endpoint, request, ctx).await {
        Ok(()) => Ok(()),
        Err(LocalRequestError::Rpc(error)) => endpoint.send_error(error).await,
        Err(LocalRequestError::Connection(error)) => Err(error),
    }
}

async fn handle_local_request_inner(
    endpoint: &LocalEndpointCall<'_>,
    request: RequestFrame,
    ctx: &ConnectionContext,
) -> LocalResult<()> {
    match method::find_for_scope(&request.method, MethodScope::Local) {
        Ok(_) => {}
        Err(MethodLookupError::WrongScope {
            spec,
            requested_scope,
        }) => {
            return Err(ProtocolError::PermissionDenied {
                message: format!(
                    "method {} is {} scoped and not valid in {} scope",
                    request.method,
                    spec.scope.as_str(),
                    requested_scope.as_str()
                ),
            }
            .into());
        }
        Err(MethodLookupError::Unknown) => {
            return Err(ProtocolError::Unimplemented {
                message: format!("unknown local method {}", request.method),
            }
            .into());
        }
    }

    match request.method.as_str() {
        method::ADMIN_SHUTDOWN_NAME => {
            decode_payload::<wire::ShutdownRequest>(&request)?;
            tracing::info!("shutdown requested");
            let shutdown_tx = {
                let state = ctx.state.read().await;
                state.shutdown_tx.clone()
            };
            let _ = shutdown_tx
                .send(ShutdownRequest::Shutdown {
                    reply: endpoint.reply_sender(),
                    reply_call_id: endpoint.call_id(),
                    link: ctx.link.clone(),
                })
                .await;
            Ok(())
        }
        method::ADMIN_CONNECT_TO_SERVER_NAME => {
            let request = decode_payload::<wire::ConnectToServerRequest>(&request)?;
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(tcp_connect(
                    &request.address,
                    &ctx.state,
                    &ctx.user_state,
                    ctx.user_id,
                    ctx.event_tx.clone(),
                ))
            });
            match result {
                Ok(()) => endpoint.send_proto(wire::Empty {}).await,
                Err(error) => {
                    endpoint
                        .send_error(ProtocolError::ServerError {
                            message: error.to_string(),
                        })
                        .await?;
                    Ok(())
                }
            }
        }
        method::ADMIN_DEBUG_NAME => {
            let request = decode_payload::<wire::DebugRequest>(&request)?;
            let service_ctx = admin_service_ctx(ctx);
            let dump = AdminService::debug(
                &service_ctx,
                debug_format_from_wire(request.format),
                request.verbose,
            )
            .await;
            endpoint.send_proto(wire::DebugResponse { dump }).await
        }
        method::AGENT_LIST_NAME => {
            decode_payload::<wire::ListAgentsRequest>(&request)?;
            let agent_ctx = agent_service_ctx(ctx).await;
            let agents = AgentService::list(&agent_ctx).await;
            endpoint
                .send_proto(wire::ListAgentsResponse {
                    agents: agents_to_wire_entries(agents)?,
                })
                .await
        }
        method::HOOK_HANDLE_NAME => {
            let request = decode_payload::<wire::HandleHookRequest>(&request)?;
            let agent_id = uuid_from_bytes("agent_id", request.agent_id)?;
            let provider = hook_provider_from_wire(request.provider);
            let service_ctx = hook_service_ctx(ctx).await;
            match HookService::handle(
                &service_ctx,
                agent_id,
                provider,
                request.payload,
                request.external,
            )
            .await
            {
                Ok(()) => endpoint.send_proto(wire::Empty {}).await,
                Err(error) => {
                    endpoint.send_error(error).await?;
                    Ok(())
                }
            }
        }
        method::AGENT_RESOLVE_NAME => {
            let request = decode_payload::<wire::ResolveAgentRequest>(&request)?;
            let identifier =
                match request
                    .identifier
                    .ok_or_else(|| ProtocolError::InvalidArgument {
                        message: "ResolveAgentRequest missing identifier".to_string(),
                    })? {
                    wire::resolve_agent_request::Identifier::AgentId(bytes) => {
                        uuid_from_bytes("agent_id", bytes)?.to_string()
                    }
                    wire::resolve_agent_request::Identifier::AgentName(name) => name,
                };
            let agent_ctx = agent_service_ctx(ctx).await;
            let agent = AgentService::resolve(&agent_ctx, &identifier).await;
            let agent = agent
                .map(wire::agent_entry_from_domain)
                .transpose()
                .map_err(|error| {
                    ConnectionError::Protocol(format!("failed to encode agent entry: {error}"))
                })?;
            endpoint
                .send_proto(wire::ResolveAgentResponse { agent })
                .await
        }
        method::ADMIN_SUSPEND_NAME => {
            let request = decode_payload::<wire::SuspendRequest>(&request)?;
            let reason = suspend_reason_from_wire(request.reason)?;
            tracing::info!("suspend requested");
            let shutdown_tx = {
                let state = ctx.state.read().await;
                state.shutdown_tx.clone()
            };
            let _ = shutdown_tx
                .send(ShutdownRequest::Suspend {
                    reply: endpoint.reply_sender(),
                    reply_call_id: endpoint.call_id(),
                    link: ctx.link.clone(),
                    reason,
                })
                .await;
            Ok(())
        }
        method::ADMIN_RESUME_NAME => {
            decode_payload::<wire::ResumeRequest>(&request)?;
            handle_resume(endpoint, ctx).await?;
            Ok(())
        }
        method => {
            endpoint
                .send_error(ProtocolError::Unimplemented {
                    message: format!("unsupported local method {method}"),
                })
                .await?;
            Ok(())
        }
    }
}

type LocalResult<T> = Result<T, LocalRequestError>;

enum LocalRequestError {
    Rpc(ProtocolError),
    Connection(ConnectionError),
}

impl From<ProtocolError> for LocalRequestError {
    fn from(error: ProtocolError) -> Self {
        Self::Rpc(error)
    }
}

impl From<ConnectionError> for LocalRequestError {
    fn from(error: ConnectionError) -> Self {
        Self::Connection(error)
    }
}

struct LocalEndpointCall<'a> {
    tx: &'a mpsc::Sender<Message>,
    call_id: CallId,
}

impl<'a> LocalEndpointCall<'a> {
    fn new(tx: &'a mpsc::Sender<Message>, call_id: CallId) -> Self {
        Self { tx, call_id }
    }

    fn reply_sender(&self) -> mpsc::Sender<Message> {
        self.tx.clone()
    }

    fn call_id(&self) -> CallId {
        self.call_id.clone()
    }

    async fn send_proto<M>(&self, message: M) -> LocalResult<()>
    where
        M: prost::Message,
    {
        self.send_payload(message.encode_to_vec()).await
    }

    async fn send_payload(&self, payload: Vec<u8>) -> LocalResult<()> {
        self.send_response(ResponseFrame::Payload(payload)).await
    }

    async fn send_error(&self, error: ProtocolError) -> crate::server::connection::Result<()> {
        self.send_response_message(ResponseFrame::Error(error))
            .await
    }

    async fn send_response(&self, response: ResponseFrame) -> LocalResult<()> {
        self.send_response_message(response).await?;
        Ok(())
    }

    async fn send_response_message(
        &self,
        response: ResponseFrame,
    ) -> crate::server::connection::Result<()> {
        self.tx
            .send(Message::Local(LocalFrame {
                call_id: self.call_id.clone(),
                body: FrameBody::Response(response),
            }))
            .await
            .map_err(|_| {
                crate::server::connection::ConnectionError::Transport(
                    crate::transport::TransportError::Io(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "outgoing channel closed while sending local response",
                    )),
                )
            })
    }
}

fn admin_service_ctx(ctx: &ConnectionContext) -> AdminServiceCtx {
    AdminServiceCtx::new(ctx.state.clone())
}

async fn hook_service_ctx(ctx: &ConnectionContext) -> HookServiceCtx {
    let host_id = {
        let state = ctx.state.read().await;
        state.host_id()
    };
    HookServiceCtx::new(
        ctx.user_state.clone(),
        ctx.event_tx.clone(),
        ctx.user_id,
        host_id,
    )
}

async fn agent_service_ctx(ctx: &ConnectionContext) -> AgentServiceCtx {
    let (host_id, is_cloud_server) = {
        let state = ctx.state.read().await;
        (state.host_id(), state.is_cloud_server())
    };
    AgentServiceCtx::new(
        ctx.user_state.clone(),
        ctx.event_tx.clone(),
        ctx.user_id,
        host_id,
        is_cloud_server,
    )
}

async fn handle_resume(
    endpoint: &LocalEndpointCall<'_>,
    ctx: &ConnectionContext,
) -> LocalResult<()> {
    tracing::info!("resume requested");
    let (state_path, host_id, is_cloud_server) = {
        let state = ctx.state.read().await;
        (
            state.config.state_path.clone(),
            state.host_id,
            state.is_cloud_server,
        )
    };
    if is_cloud_server {
        endpoint
            .send_error(ProtocolError::ServerError {
                message: "cloud relays do not host local agents".to_string(),
            })
            .await?;
        return Ok(());
    }
    let suspended = match crate::suspend::load_and_remove_suspended(&state_path) {
        Ok(s) => s,
        Err(error) => {
            tracing::error!(error = %error, "failed to load suspended agents");
            endpoint
                .send_error(ProtocolError::ServerError {
                    message: format!("failed to load state: {error}"),
                })
                .await?;
            return Ok(());
        }
    };
    let (resumed_count, failed_count) = resume_agents(
        &ctx.user_state,
        &ctx.event_tx,
        ctx.user_id,
        suspended.agents,
        host_id,
    )
    .await;
    endpoint
        .send_proto(wire::ResumeResponse {
            resumed_count: resumed_count as u64,
            failed_count: failed_count as u64,
        })
        .await
}

fn decode_payload<M>(request: &RequestFrame) -> LocalResult<M>
where
    M: prost::Message + Default,
{
    M::decode(request.payload.as_slice()).map_err(|error| {
        ProtocolError::InvalidArgument {
            message: format!("invalid {} request: {error}", request.method),
        }
        .into()
    })
}

fn agents_to_wire_entries(
    agents: impl IntoIterator<Item = crate::protocol::Agent>,
) -> crate::server::connection::Result<Vec<wire::AgentEntry>> {
    agents
        .into_iter()
        .map(wire::agent_entry_from_domain)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            ConnectionError::Protocol(format!("failed to encode agent entry: {error}"))
        })
}

fn uuid_from_bytes(name: &str, bytes: Vec<u8>) -> LocalResult<Uuid> {
    let bytes: [u8; 16] =
        bytes
            .try_into()
            .map_err(|bytes: Vec<u8>| ProtocolError::InvalidArgument {
                message: format!("{name} must be 16 bytes, got {}", bytes.len()),
            })?;
    Ok(Uuid::from_bytes(bytes))
}

fn debug_format_from_wire(format: i32) -> DebugFormat {
    match wire::DebugFormat::try_from(format).unwrap_or(wire::DebugFormat::Unspecified) {
        wire::DebugFormat::Json => DebugFormat::Json,
        wire::DebugFormat::Yaml | wire::DebugFormat::Unspecified => DebugFormat::Yaml,
    }
}

fn hook_provider_from_wire(provider: i32) -> HookProvider {
    match wire::HookProvider::try_from(provider).unwrap_or(wire::HookProvider::Unspecified) {
        wire::HookProvider::Claude => HookProvider::Claude,
        wire::HookProvider::Unspecified => HookProvider::Unknown,
    }
}

fn suspend_reason_from_wire(reason: i32) -> LocalResult<ShutdownReason> {
    match wire::SuspendReason::try_from(reason).map_err(|_| ProtocolError::InvalidArgument {
        message: format!("invalid SuspendRequest reason: {reason}"),
    })? {
        wire::SuspendReason::Unspecified | wire::SuspendReason::User => {
            Ok(ShutdownReason::Suspending)
        }
        wire::SuspendReason::Update => Ok(ShutdownReason::Updating),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use prost::Message as _;
    use tokio::sync::{RwLock, mpsc};

    use super::*;
    use crate::config::Config;
    use crate::protocol::link::Link;
    use crate::server::{
        LOCAL_USER_ID, ServerState, ServerUserState, ShutdownRequest, test_helpers,
    };

    async fn test_ctx() -> ConnectionContext {
        let (state, user_state) = test_helpers::test_state().await;
        test_ctx_from_state(state, user_state).await
    }

    async fn test_ctx_with_shutdown_tx(
        shutdown_tx: mpsc::Sender<ShutdownRequest>,
    ) -> ConnectionContext {
        let state = Arc::new(RwLock::new(ServerState::new(
            Config::default(),
            Uuid::new_v4(),
            shutdown_tx,
        )));
        let user_state = {
            let s = state.read().await;
            s.user_state(&LOCAL_USER_ID)
                .expect("local user state is always initialized")
        };
        test_ctx_from_state(state, user_state).await
    }

    async fn test_ctx_from_state(
        state: Arc<RwLock<ServerState>>,
        user_state: Arc<RwLock<ServerUserState>>,
    ) -> ConnectionContext {
        let (event_tx, _event_rx) = mpsc::channel(16);
        let link = Link::new("test-link").unwrap();
        let rpc = {
            let mut us = user_state.write().await;
            if us.route(&link).is_none() {
                let (_handle, _rx) = us.try_reserve_link(link.clone()).unwrap();
            }
            us.rpc_for_link(&link).unwrap()
        };
        ConnectionContext {
            state,
            rpc,
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link,
            is_local: true,
            heartbeat: None,
            routing_role: crate::protocol::handshake::RoutingRole::Observer,
        }
    }

    async fn expect_local_error(
        mut rx: mpsc::Receiver<Message>,
        call_id: &CallId,
    ) -> ProtocolError {
        let msg = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for local response")
            .expect("local response should be sent");
        let Message::Local(LocalFrame {
            call_id: response_call_id,
            body: FrameBody::Response(ResponseFrame::Error(error)),
        }) = msg
        else {
            panic!("expected local error response");
        };
        assert_eq!(&response_call_id, call_id);
        error
    }

    async fn suspend_reason_for(request: wire::SuspendRequest) -> ShutdownReason {
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        let (tx, _rx) = mpsc::channel(1);
        let request = RequestFrame {
            method: method::ADMIN_SUSPEND_NAME.to_string(),
            payload: request.encode_to_vec(),
        };

        handle_local_request(
            &tx,
            CallId::from(Uuid::new_v4()),
            request,
            &test_ctx_with_shutdown_tx(shutdown_tx).await,
        )
        .await
        .unwrap();

        let shutdown_request = tokio::time::timeout(Duration::from_secs(1), shutdown_rx.recv())
            .await
            .expect("timed out waiting for shutdown request")
            .expect("shutdown request should be sent");

        let ShutdownRequest::Suspend { reason, .. } = shutdown_request else {
            panic!("expected suspend shutdown request");
        };
        reason
    }

    #[tokio::test]
    async fn known_wrong_scope_local_method_returns_permission_denied() {
        let (tx, rx) = mpsc::channel(1);
        let call_id = CallId::from(Uuid::new_v4());
        let request = RequestFrame {
            method: method::AGENT_CREATE_NAME.to_string(),
            payload: Vec::new(),
        };

        handle_local_request(&tx, call_id.clone(), request, &test_ctx().await)
            .await
            .unwrap();

        assert!(matches!(
            expect_local_error(rx, &call_id).await,
            ProtocolError::PermissionDenied { message }
                if message.contains("not valid in local scope")
        ));
    }

    #[tokio::test]
    async fn unknown_local_method_returns_unimplemented() {
        let (tx, rx) = mpsc::channel(1);
        let call_id = CallId::from(Uuid::new_v4());
        let request = RequestFrame {
            method: "/amux.v1.Missing/Nope".to_string(),
            payload: Vec::new(),
        };

        handle_local_request(&tx, call_id.clone(), request, &test_ctx().await)
            .await
            .unwrap();

        assert!(matches!(
            expect_local_error(rx, &call_id).await,
            ProtocolError::Unimplemented { message }
                if message.contains("unknown local method")
        ));
    }

    #[tokio::test]
    async fn list_agents_rejects_invalid_request_payload() {
        let (tx, rx) = mpsc::channel(1);
        let call_id = CallId::from(Uuid::new_v4());
        let request = RequestFrame {
            method: method::AGENT_LIST_NAME.to_string(),
            payload: vec![0xff],
        };

        handle_local_request(&tx, call_id.clone(), request, &test_ctx().await)
            .await
            .unwrap();

        assert!(
            matches!(expect_local_error(rx, &call_id).await, ProtocolError::InvalidArgument { message } if message.contains("invalid"))
        );
    }

    #[tokio::test]
    async fn resolve_missing_identifier_returns_invalid_argument_response() {
        let (tx, rx) = mpsc::channel(1);
        let call_id = CallId::from(Uuid::new_v4());
        let request = RequestFrame {
            method: method::AGENT_RESOLVE_NAME.to_string(),
            payload: wire::ResolveAgentRequest { identifier: None }.encode_to_vec(),
        };

        handle_local_request(&tx, call_id.clone(), request, &test_ctx().await)
            .await
            .unwrap();

        assert!(
            matches!(expect_local_error(rx, &call_id).await, ProtocolError::InvalidArgument { message } if message.contains("missing identifier"))
        );
    }

    #[tokio::test]
    async fn suspend_request_defaults_to_suspending_notification() {
        let reason = suspend_reason_for(wire::SuspendRequest {
            reason: wire::SuspendReason::Unspecified as i32,
        })
        .await;

        assert_eq!(reason, ShutdownReason::Suspending);
    }

    #[tokio::test]
    async fn suspend_request_can_request_update_notification() {
        let reason = suspend_reason_for(wire::SuspendRequest {
            reason: wire::SuspendReason::Update as i32,
        })
        .await;

        assert_eq!(reason, ShutdownReason::Updating);
    }
}

mod runtime;

use prost::Message as _;
use thiserror::Error;
use uuid::Uuid;

use super::Connection;
use crate::agent::claude::io as claude_io;
use crate::agent::claude::io::{ClaudeRawV1Args, ClaudeRawV1ReplayQuery};
use crate::protocol::message::{
    CreateAgentRequest, DebugFormat, HookProvider, ProtocolError, RenameAgentRequest,
    ShutdownReason, TerminalSize,
};
use crate::protocol::session::{self, SubscribeSessionEvent, SubscribeSessionFrame};
use crate::protocol::{Agent, Route, agent_lifecycle, method, wire};
use crate::transport::TransportError;

#[derive(Debug, Error)]
pub enum RpcClientError {
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

pub struct SubscribeSessionClient {
    stream: runtime::EndpointOutputStream,
}

impl SubscribeSessionClient {
    pub async fn cancel(&self) -> Result<(), RpcClientError> {
        self.stream
            .send_cancel(method::AGENT_SUBSCRIBE_SESSION_NAME)
            .await
    }

    pub async fn recv(&self) -> Result<SubscribeSessionFrame, RpcClientError> {
        let body = self.stream.recv_frame_body().await?;
        let frame = session::decode_subscribe_session_frame_body(body).map_err(|error| {
            self.stream.finish();
            RpcClientError::Decode {
                method: method::AGENT_SUBSCRIBE_SESSION_NAME,
                message: error.to_string(),
            }
        })?;
        if matches!(frame, SubscribeSessionFrame::Response(_)) {
            self.stream.finish();
        }
        Ok(frame)
    }
}

/// Operation-oriented client for the local amux RPC surface.
///
/// This is intentionally not a frame helper. Callers invoke domain operations;
/// the client owns local call IDs and response matching.
pub struct RpcClient {
    runtime: runtime::ClientRuntime,
}

impl RpcClient {
    pub fn new(connection: Connection) -> Self {
        Self {
            runtime: runtime::ClientRuntime::new(connection),
        }
    }

    pub async fn create_agent(
        &mut self,
        request: &CreateAgentRequest,
    ) -> Result<Agent, RpcClientError> {
        let payload = agent_lifecycle_request_payload(
            method::AGENT_CREATE_NAME,
            agent_lifecycle::encode_create_agent_request(request),
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
            |error| RpcClientError::Decode {
                method: method::AGENT_CREATE_NAME,
                message: error.to_string(),
            },
        )? {
            Ok(agent) => Ok(agent),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn rename_agent(
        &mut self,
        agent_id: Uuid,
        agent_route: Route,
        name: String,
    ) -> Result<Agent, RpcClientError> {
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
            |error| RpcClientError::Decode {
                method: method::AGENT_RENAME_NAME,
                message: error.to_string(),
            },
        )? {
            Ok(agent) => Ok(agent),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn delete_agent(
        &mut self,
        agent_id: Uuid,
        agent_route: Route,
    ) -> Result<(), RpcClientError> {
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
            RpcClientError::Decode {
                method: method::AGENT_DELETE_NAME,
                message: error.to_string(),
            }
        })? {
            Ok(()) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn subscribe_raw_session(
        &mut self,
        agent_id: Uuid,
        agent_route: Route,
        terminal_size: Option<TerminalSize>,
        replay_query: Option<ClaudeRawV1ReplayQuery>,
    ) -> Result<SubscribeSessionClient, RpcClientError> {
        let args = claude_io::encode_raw_v1_args(ClaudeRawV1Args {
            terminal_size,
            replay_query,
        });
        self.subscribe_session(agent_id, agent_route, claude_io::RAW_V1, args)
            .await
    }

    pub async fn subscribe_session(
        &mut self,
        agent_id: Uuid,
        agent_route: Route,
        io_protocol: impl Into<String>,
        args: Option<Vec<u8>>,
    ) -> Result<SubscribeSessionClient, RpcClientError> {
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
        let session = SubscribeSessionClient { stream };

        match session.recv().await? {
            SubscribeSessionFrame::Event(SubscribeSessionEvent::Opened) => {
                session.stream.set_active();
                Ok(session)
            }
            SubscribeSessionFrame::Event(event) => {
                session.stream.finish();
                Err(RpcClientError::Unexpected {
                    method: method::AGENT_SUBSCRIBE_SESSION_NAME,
                    message: format!("expected SessionOpened, got {event:?}"),
                })
            }
            SubscribeSessionFrame::Response(Ok(())) => Err(RpcClientError::Unexpected {
                method: method::AGENT_SUBSCRIBE_SESSION_NAME,
                message: "session ended before opening".to_string(),
            }),
            SubscribeSessionFrame::Response(Err(error)) => Err(error.into()),
        }
    }

    pub async fn send_input(
        &mut self,
        agent_id: Uuid,
        agent_route: Route,
        io_protocol: impl Into<String>,
        input_id: Vec<u8>,
        payload: Vec<u8>,
    ) -> Result<(), RpcClientError> {
        let full_route = self.route_to_agent(agent_route);
        let payload = session_request_payload(
            method::AGENT_SEND_INPUT_NAME,
            session::encode_send_input_request(agent_id, io_protocol, input_id, payload),
        )?;
        let response = self
            .runtime
            .call_endpoint_unary_payload(method::AGENT_SEND_INPUT, full_route, payload)
            .await?;
        decode_payload::<wire::SendInputResponse>(method::AGENT_SEND_INPUT_NAME, response)
            .map(|_| ())
    }

    pub async fn send_control(
        &mut self,
        agent_id: Uuid,
        agent_route: Route,
        io_protocol: impl Into<String>,
        payload: Vec<u8>,
    ) -> Result<(), RpcClientError> {
        let full_route = self.route_to_agent(agent_route);
        let payload = session_request_payload(
            method::AGENT_SEND_INPUT_NAME,
            session::encode_send_control_request(agent_id, io_protocol, payload),
        )?;
        let response = self
            .runtime
            .call_endpoint_unary_payload(method::AGENT_SEND_INPUT, full_route, payload)
            .await?;
        decode_payload::<wire::SendInputResponse>(method::AGENT_SEND_INPUT_NAME, response)
            .map(|_| ())
    }

    pub async fn list_agents(&mut self) -> Result<Vec<Agent>, RpcClientError> {
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
        &mut self,
        identifier: &str,
    ) -> Result<Option<Agent>, RpcClientError> {
        let identifier = Uuid::parse_str(identifier)
            .map(|agent_id| {
                wire::resolve_agent_request::Identifier::AgentId(agent_id.as_bytes().to_vec())
            })
            .unwrap_or_else(|_| {
                wire::resolve_agent_request::Identifier::AgentName(identifier.to_string())
            });
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

    pub async fn shutdown(&mut self) -> Result<(), RpcClientError> {
        let body = self
            .runtime
            .local_unary_payload(
                method::ADMIN_SHUTDOWN,
                wire::ShutdownRequest {}.encode_to_vec(),
            )
            .await?;
        decode_empty(method::ADMIN_SHUTDOWN_NAME, body)
    }

    pub async fn suspend(&mut self) -> Result<SuspendSummary, RpcClientError> {
        self.suspend_with_reason(wire::SuspendReason::User).await
    }

    pub async fn suspend_for_update(&mut self) -> Result<SuspendSummary, RpcClientError> {
        self.suspend_with_reason(wire::SuspendReason::Update).await
    }

    async fn suspend_with_reason(
        &mut self,
        reason: wire::SuspendReason,
    ) -> Result<SuspendSummary, RpcClientError> {
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

    pub async fn resume(&mut self) -> Result<ResumeSummary, RpcClientError> {
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

    pub async fn connect_to_server(&mut self, address: String) -> Result<(), RpcClientError> {
        let body = self
            .runtime
            .local_unary_payload(
                method::ADMIN_CONNECT_TO_SERVER,
                wire::ConnectToServerRequest { address }.encode_to_vec(),
            )
            .await?;
        decode_empty(method::ADMIN_CONNECT_TO_SERVER_NAME, body)
    }

    pub async fn debug(
        &mut self,
        verbose: bool,
        format: DebugFormat,
    ) -> Result<String, RpcClientError> {
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

    pub async fn enqueue_hook(
        &mut self,
        agent_id: Uuid,
        provider: HookProvider,
        payload: Vec<u8>,
        external: bool,
    ) -> Result<(), RpcClientError> {
        self.runtime
            .local_send_only(
                method::HOOK_HANDLE_NAME,
                wire::HandleHookRequest {
                    agent_id: agent_id.as_bytes().to_vec(),
                    provider: hook_provider_to_wire(provider),
                    payload,
                    external,
                }
                .encode_to_vec(),
            )
            .await
    }

    fn route_to_agent(&self, agent_route: Route) -> Route {
        route_to_agent_with_link(agent_route, self.runtime.link().clone())
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

fn decode_empty(method: &'static str, body: Vec<u8>) -> Result<(), RpcClientError> {
    decode_payload::<wire::Empty>(method, body).map(|_| ())
}

fn decode_payload<M>(method: &'static str, body: Vec<u8>) -> Result<M, RpcClientError>
where
    M: prost::Message + Default,
{
    M::decode(body.as_slice()).map_err(|error| RpcClientError::Decode {
        method,
        message: error.to_string(),
    })
}

fn decode_agent_entry(
    method: &'static str,
    entry: wire::AgentEntry,
) -> Result<Agent, RpcClientError> {
    wire::agent_entry_to_domain(entry).map_err(|error| RpcClientError::Decode {
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
) -> Result<Vec<u8>, RpcClientError> {
    payload.map_err(|error| RpcClientError::Encode {
        method,
        message: error.to_string(),
    })
}

fn session_request_payload(
    method: &'static str,
    payload: Result<Vec<u8>, session::SessionCodecError>,
) -> Result<Vec<u8>, RpcClientError> {
    payload.map_err(|error| RpcClientError::Encode {
        method,
        message: error.to_string(),
    })
}

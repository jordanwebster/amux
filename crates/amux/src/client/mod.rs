use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures_util::Stream;
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::agents::{
    Agent, AgentEvent, CreateAgentRequest, SessionCloseReason, SubscribeSessionEvent,
};
use crate::debug::DebugFormat;
use crate::pairing::ssh::SshPairingPeer;
use crate::protocol::{ProtocolError, protocol_error_from_status_details, wire};
use crate::routing::{HostEntry, HostEvent, HostTrustStatus, capabilities_from_wire};
use crate::server::{SHUTDOWN_REASON_METADATA_KEY, ShutdownReason};
use crate::transport::TransportError;
use crate::{
    AgentIdentifier, ArtifactId, ArtifactKind, ArtifactRef, DiffBase, DiffResponse, PeerIdentifier,
    SendInputRequest, SendMessageRequest, SetAgentStatusRequest, SubscribeSessionRequest,
};

const PAIRING_PUBKEY_LEN: usize = 32;
const MAX_PAIRING_NAME_BYTES: usize = 256;

mod connect;

mod method {
    pub(super) const CLIENT_LIST_HOSTS_NAME: &str = "/amux.v1.ClientService/ListHosts";
    pub(super) const CLIENT_LIST_AGENTS_NAME: &str = "/amux.v1.ClientService/ListAgents";
    pub(super) const CLIENT_SUBSCRIBE_HOSTS_NAME: &str = "/amux.v1.ClientService/SubscribeHosts";
    pub(super) const CLIENT_SUBSCRIBE_AGENTS_NAME: &str = "/amux.v1.ClientService/SubscribeAgents";
    pub(super) const CLIENT_CREATE_NAME: &str = "/amux.v1.ClientService/CreateAgent";
    pub(super) const CLIENT_RENAME_NAME: &str = "/amux.v1.ClientService/RenameAgent";
    pub(super) const CLIENT_DELETE_NAME: &str = "/amux.v1.ClientService/DeleteAgent";
    pub(super) const CLIENT_SEND_MESSAGE_NAME: &str = "/amux.v1.ClientService/SendMessage";
    pub(super) const CLIENT_SEND_INPUT_NAME: &str = "/amux.v1.ClientService/SendInput";
    pub(super) const CLIENT_PUT_ARTIFACT_NAME: &str = "/amux.v1.ClientService/PutArtifact";
    pub(super) const CLIENT_GET_ARTIFACT_NAME: &str = "/amux.v1.ClientService/GetArtifact";
    pub(super) const CLIENT_DIFF_NAME: &str = "/amux.v1.ClientService/Diff";
    pub(super) const CLIENT_SUBSCRIBE_SESSION_NAME: &str =
        "/amux.v1.ClientService/SubscribeSession";
    pub(super) const CLIENT_HANDLE_HOOK_NAME: &str = "/amux.v1.ClientService/HandleHook";
    #[cfg(test)]
    pub(super) const PROFILE_START_PAIRING_NAME: &str = "/amux.v1.ProfileService/StartPairing";
}

pub use connect::ConnectError;
#[cfg(unix)]
pub(crate) use connect::connect_existing_client_service;

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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteAgentSummary {
    pub removed_children: Vec<Agent>,
    pub unreachable_children: Vec<Agent>,
}

pub struct SubscribeSessionClient {
    inner: ClientServiceResponseStream<wire::SubscribeSessionResponse>,
    done: bool,
}

pub type SessionStream = SubscribeSessionClient;

pub struct HostEventStream {
    inner: ClientServiceResponseStream<wire::SubscribeHostsResponse>,
    done: bool,
}

pub struct AgentEventStream {
    inner: ClientServiceResponseStream<wire::SubscribeAgentsResponse>,
    done: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingStart {
    pub identity: SshPairingPeer,
    pub ttl_seconds: u64,
    pub tcp_port: Option<u16>,
    pub cloud_url: String,
    pub secret: PairingSecret,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PairingSecret {
    Pin(String),
    QrSecret(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerEntry {
    pub host_id: uuid::Uuid,
    pub name: String,
    pub pubkey: Vec<u8>,
    pub paired_at: DateTime<Utc>,
    pub reachabilities: Vec<PeerReachability>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PeerReachability {
    Cloud,
    Ssh {
        target: String,
        profile: crate::installation::ProfileId,
    },
    DirectTcp {
        addr: SocketAddr,
    },
}

struct ClientServiceResponseStream<T> {
    stream: tonic::Streaming<T>,
}

impl SubscribeSessionClient {
    pub async fn recv(&mut self) -> Result<SubscribeSessionEvent, ClientError> {
        if self.done {
            return Err(stream_already_done_error(
                method::CLIENT_SUBSCRIBE_SESSION_NAME,
            ));
        }
        let event = recv_subscribe_session_event(&mut self.inner).await;
        if session_event_stream_item_is_terminal(&event) {
            self.done = true;
        }
        event
    }
}

impl Stream for SubscribeSessionClient {
    type Item = Result<SubscribeSessionEvent, ClientError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        poll_response_stream(
            &mut this.inner,
            &mut this.done,
            method::CLIENT_SUBSCRIBE_SESSION_NAME,
            client_service_session_response_to_event,
            cx,
            session_event_stream_item_is_terminal,
        )
    }
}

impl HostEventStream {
    pub async fn recv(&mut self) -> Result<HostEvent, ClientError> {
        if self.done {
            return Err(stream_already_done_error(
                method::CLIENT_SUBSCRIBE_HOSTS_NAME,
            ));
        }
        let event = recv_host_event(&mut self.inner).await;
        if result_is_terminal(&event) {
            self.done = true;
        }
        event
    }
}

impl Stream for HostEventStream {
    type Item = Result<HostEvent, ClientError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        poll_response_stream(
            &mut this.inner,
            &mut this.done,
            method::CLIENT_SUBSCRIBE_HOSTS_NAME,
            client_service_host_response_to_host_event,
            cx,
            result_is_terminal,
        )
    }
}

impl AgentEventStream {
    pub async fn recv(&mut self) -> Result<AgentEvent, ClientError> {
        if self.done {
            return Err(stream_already_done_error(
                method::CLIENT_SUBSCRIBE_AGENTS_NAME,
            ));
        }
        let event = recv_agent_event(&mut self.inner).await;
        if result_is_terminal(&event) {
            self.done = true;
        }
        event
    }
}

impl Stream for AgentEventStream {
    type Item = Result<AgentEvent, ClientError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        poll_response_stream(
            &mut this.inner,
            &mut this.done,
            method::CLIENT_SUBSCRIBE_AGENTS_NAME,
            |response| {
                Ok(client_service_agent_response_to_agent_event(response)?
                    .unwrap_or(AgentEvent::SnapshotComplete))
            },
            cx,
            result_is_terminal,
        )
    }
}

fn poll_response_stream<T, U>(
    stream: &mut ClientServiceResponseStream<T>,
    done: &mut bool,
    method: &'static str,
    map: impl FnOnce(T) -> Result<U, ClientError>,
    cx: &mut Context<'_>,
    is_terminal: impl FnOnce(&Result<U, ClientError>) -> bool,
) -> Poll<Option<Result<U, ClientError>>> {
    if *done {
        return Poll::Ready(None);
    }

    match Pin::new(&mut stream.stream).poll_next(cx) {
        Poll::Ready(Some(Ok(response))) => {
            let result = map(response);
            if is_terminal(&result) {
                *done = true;
            }
            Poll::Ready(Some(result))
        }
        Poll::Ready(Some(Err(status))) => {
            *done = true;
            Poll::Ready(Some(Err(status_to_client_error(status))))
        }
        Poll::Ready(None) => {
            *done = true;
            Poll::Ready(Some(Err(stream_ended_error(method))))
        }
        Poll::Pending => Poll::Pending,
    }
}

fn stream_already_done_error(method: &'static str) -> ClientError {
    ClientError::Unexpected {
        method,
        message: "stream already ended".to_string(),
    }
}

fn stream_ended_error(method: &'static str) -> ClientError {
    ClientError::Unexpected {
        method,
        message: "stream ended".to_string(),
    }
}

fn result_is_terminal<T>(result: &Result<T, ClientError>) -> bool {
    result.is_err()
}

fn session_event_stream_item_is_terminal(
    result: &Result<SubscribeSessionEvent, ClientError>,
) -> bool {
    match result {
        Ok(SubscribeSessionEvent::Closed { .. }) | Err(_) => true,
        Ok(_) => false,
    }
}

async fn recv_subscribe_session_event(
    inner: &mut ClientServiceResponseStream<wire::SubscribeSessionResponse>,
) -> Result<SubscribeSessionEvent, ClientError> {
    recv_client_service_subscribe_session_event(inner).await
}

async fn recv_host_event(
    inner: &mut ClientServiceResponseStream<wire::SubscribeHostsResponse>,
) -> Result<HostEvent, ClientError> {
    recv_client_service_host_event(inner).await
}

async fn recv_agent_event(
    inner: &mut ClientServiceResponseStream<wire::SubscribeAgentsResponse>,
) -> Result<AgentEvent, ClientError> {
    recv_client_service_agent_event(inner).await
}

async fn recv_client_service_subscribe_session_event(
    stream: &mut ClientServiceResponseStream<wire::SubscribeSessionResponse>,
) -> Result<SubscribeSessionEvent, ClientError> {
    let response = stream
        .stream
        .message()
        .await
        .map_err(status_to_client_error)?;
    let Some(response) = response else {
        return Err(ClientError::Unexpected {
            method: method::CLIENT_SUBSCRIBE_SESSION_NAME,
            message: "session event stream ended before SessionClosed".to_string(),
        });
    };
    client_service_session_response_to_event(response)
}

async fn recv_client_service_host_event(
    stream: &mut ClientServiceResponseStream<wire::SubscribeHostsResponse>,
) -> Result<HostEvent, ClientError> {
    let response = stream
        .stream
        .message()
        .await
        .map_err(status_to_client_error)?;
    let Some(response) = response else {
        return Err(ClientError::Unexpected {
            method: method::CLIENT_SUBSCRIBE_HOSTS_NAME,
            message: "host event stream ended".to_string(),
        });
    };
    client_service_host_response_to_host_event(response)
}

async fn recv_client_service_agent_event(
    stream: &mut ClientServiceResponseStream<wire::SubscribeAgentsResponse>,
) -> Result<AgentEvent, ClientError> {
    let response = stream
        .stream
        .message()
        .await
        .map_err(status_to_client_error)?;
    let Some(response) = response else {
        return Err(ClientError::Unexpected {
            method: method::CLIENT_SUBSCRIBE_AGENTS_NAME,
            message: "agent event stream ended".to_string(),
        });
    };
    Ok(client_service_agent_response_to_agent_event(response)?
        .unwrap_or(AgentEvent::SnapshotComplete))
}

/// Operation-oriented client for the local amux RPC surface.
#[derive(Clone)]
pub struct Client {
    inner: Arc<AsyncMutex<wire::client_service_client::ClientServiceClient<Channel>>>,
    closed: Arc<AtomicBool>,
}

impl Client {
    pub(crate) fn from_client_service_channel(channel: Channel) -> Self {
        Self {
            inner: Arc::new(AsyncMutex::new(wire::client_service_client(channel))),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

impl Client {
    pub async fn create_agent(&self, request: CreateAgentRequest) -> Result<Agent, ClientError> {
        self.ensure_open()?;
        let request = client_create_request_to_wire(request)?;
        let response = self
            .inner
            .lock()
            .await
            .create_agent(request)
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        agent_response_to_agent(method::CLIENT_CREATE_NAME, response.agent)
    }

    pub async fn rename_agent(
        &self,
        identifier: impl Into<AgentIdentifier>,
        name: String,
    ) -> Result<Agent, ClientError> {
        self.ensure_open()?;
        let identifier = identifier.into();
        let response = self
            .inner
            .lock()
            .await
            .rename_agent(wire::ClientRenameAgentRequest {
                agent: Some(agent_ref(identifier)),
                name,
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        agent_response_to_agent(method::CLIENT_RENAME_NAME, response.agent)
    }

    pub async fn delete_agent(
        &self,
        identifier: impl Into<AgentIdentifier>,
    ) -> Result<(), ClientError> {
        self.delete_agent_with_summary(identifier).await.map(|_| ())
    }

    /// Delete a direct child on behalf of an authenticated agent.
    pub async fn delete_child_agent(
        &self,
        identifier: impl Into<AgentIdentifier>,
        caller_agent_id: Uuid,
    ) -> Result<(), ClientError> {
        self.delete_agent_with_summary_for_caller(identifier, Some(caller_agent_id))
            .await
            .map(|_| ())
    }

    pub async fn delete_agent_with_summary(
        &self,
        identifier: impl Into<AgentIdentifier>,
    ) -> Result<DeleteAgentSummary, ClientError> {
        self.delete_agent_with_summary_for_caller(identifier, None)
            .await
    }

    async fn delete_agent_with_summary_for_caller(
        &self,
        identifier: impl Into<AgentIdentifier>,
        caller_agent_id: Option<Uuid>,
    ) -> Result<DeleteAgentSummary, ClientError> {
        self.ensure_open()?;
        let identifier = identifier.into();
        let response = self
            .inner
            .lock()
            .await
            .delete_agent(wire::ClientDeleteAgentRequest {
                agent: Some(agent_ref(identifier)),
                caller_agent_id: caller_agent_id.map(|id| id.as_bytes().to_vec()),
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let decode = |agents: Vec<wire::Agent>| {
            agents
                .into_iter()
                .map(|agent| {
                    crate::agents::agent_from_wire(agent).map_err(|error| ClientError::Decode {
                        method: method::CLIENT_DELETE_NAME,
                        message: error.to_string(),
                    })
                })
                .collect::<Result<Vec<_>, ClientError>>()
        };
        Ok(DeleteAgentSummary {
            removed_children: decode(response.removed_children)?,
            unreachable_children: decode(response.unreachable_children)?,
        })
    }

    pub async fn subscribe_session(
        &self,
        request: SubscribeSessionRequest,
    ) -> Result<SessionStream, ClientError> {
        self.ensure_open()?;
        let protocol = request
            .io_protocol
            .parse()
            .map_err(|message| ClientError::Encode {
                method: method::CLIENT_SUBSCRIBE_SESSION_NAME,
                message,
            })?;
        let protocol =
            crate::agents::subscribe_protocol_to_client_wire(protocol, request.args.as_deref())
                .map_err(|error| ClientError::Encode {
                    method: method::CLIENT_SUBSCRIBE_SESSION_NAME,
                    message: error.to_string(),
                })?;
        let response = self
            .inner
            .lock()
            .await
            .subscribe_session(wire::ClientSubscribeSessionRequest {
                agent: Some(agent_ref(request.agent)),
                protocol: Some(protocol),
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let session = SubscribeSessionClient {
            inner: ClientServiceResponseStream { stream: response },
            done: false,
        };
        Ok(session)
    }

    pub async fn send_input(&self, request: SendInputRequest) -> Result<(), ClientError> {
        self.ensure_open()?;
        let event = crate::agents::SessionInputEvent::Input {
            input_id: request.input_id,
            payload: request.payload.to_vec(),
        };
        let protocol = request
            .io_protocol
            .parse()
            .map_err(|message| ClientError::Encode {
                method: method::CLIENT_SEND_INPUT_NAME,
                message,
            })?;
        let (input_id, event) = crate::agents::send_input_event_to_client_wire(protocol, &event)
            .map_err(|error| ClientError::Encode {
                method: method::CLIENT_SEND_INPUT_NAME,
                message: error.to_string(),
            })?;
        self.inner
            .lock()
            .await
            .send_input(wire::ClientSendInputRequest {
                agent: Some(agent_ref(request.agent)),
                input_id,
                pin: request.pin,
                event: Some(event),
            })
            .await
            .map_err(status_to_client_error)?;
        Ok(())
    }

    pub async fn put_artifact(
        &self,
        agent: AgentIdentifier,
        kind: ArtifactKind,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, ClientError> {
        self.put_artifact_inner(agent, kind, name, mime, bytes, false)
            .await
    }

    /// Stores an artifact produced by the calling managed agent. Unlike a
    /// draft attachment put, this pins and publishes the artifact immediately
    /// so a following reply mention can be rendered from the stream alone.
    pub async fn put_artifact_by_agent(
        &self,
        caller: Uuid,
        kind: ArtifactKind,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, ClientError> {
        self.put_artifact_inner(AgentIdentifier::Id(caller), kind, name, mime, bytes, true)
            .await
    }

    async fn put_artifact_inner(
        &self,
        agent: AgentIdentifier,
        kind: ArtifactKind,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
        agent_attach: bool,
    ) -> Result<ArtifactRef, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .put_artifact(wire::ClientPutArtifactRequest {
                agent: Some(agent_ref(agent)),
                kind: crate::agents::artifact_kind_to_wire(kind) as i32,
                name: name.to_string(),
                mime: mime.to_string(),
                bytes,
                agent_attach,
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let artifact = response.artifact.ok_or_else(|| ClientError::Decode {
            method: method::CLIENT_PUT_ARTIFACT_NAME,
            message: "missing PutArtifactResponse.artifact".to_string(),
        })?;
        crate::agents::artifact_ref_from_wire(artifact).map_err(|error| ClientError::Decode {
            method: method::CLIENT_PUT_ARTIFACT_NAME,
            message: error.to_string(),
        })
    }

    pub async fn get_artifact(
        &self,
        agent: AgentIdentifier,
        id: &ArtifactId,
    ) -> Result<(ArtifactRef, Vec<u8>), ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .get_artifact(wire::ClientGetArtifactRequest {
                agent: Some(agent_ref(agent)),
                id: id.to_string(),
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let artifact = response.artifact.ok_or_else(|| ClientError::Decode {
            method: method::CLIENT_GET_ARTIFACT_NAME,
            message: "missing GetArtifactResponse.artifact".to_string(),
        })?;
        let artifact = crate::agents::artifact_ref_from_wire(artifact).map_err(|error| {
            ClientError::Decode {
                method: method::CLIENT_GET_ARTIFACT_NAME,
                message: error.to_string(),
            }
        })?;
        Ok((artifact, response.bytes))
    }

    pub async fn diff(
        &self,
        agent: AgentIdentifier,
        base: DiffBase,
    ) -> Result<DiffResponse, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .diff(wire::ClientDiffRequest {
                agent: Some(agent_ref(agent)),
                base: Some(crate::agents::diff_base_to_wire(&base)),
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        crate::agents::diff_response_from_wire(response).map_err(|error| ClientError::Decode {
            method: method::CLIENT_DIFF_NAME,
            message: error.to_string(),
        })
    }

    pub async fn send_message(&self, request: SendMessageRequest) -> Result<Uuid, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .send_message(wire::ClientSendMessageRequest {
                to: Some(agent_ref(request.to)),
                text: request.text,
                context: request.context.map(|id| id.as_bytes().to_vec()),
                from_agent_id: request.from_agent_id.map(|id| id.as_bytes().to_vec()),
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        uuid_from_wire_bytes(
            method::CLIENT_SEND_MESSAGE_NAME,
            "SendMessageResponse.envelope_id",
            response.envelope_id,
        )
    }

    pub async fn set_agent_status(
        &self,
        request: SetAgentStatusRequest,
    ) -> Result<(), ClientError> {
        self.ensure_open()?;
        self.inner
            .lock()
            .await
            .set_agent_status(wire::ClientSetAgentStatusRequest {
                agent: Some(agent_ref(request.agent)),
                working_on: request.working_on,
            })
            .await
            .map_err(status_to_client_error)?;
        Ok(())
    }

    pub async fn list_agents(&self) -> Result<Vec<Agent>, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .list_agents(wire::ListAgentsRequest {})
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        response
            .agents
            .into_iter()
            .map(|agent| wire_agent_to_agent(method::CLIENT_LIST_AGENTS_NAME, agent))
            .collect()
    }

    pub async fn list_hosts(&self) -> Result<Vec<HostEntry>, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .list_hosts(wire::ListHostsRequest {})
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        response
            .hosts
            .into_iter()
            .map(|host| host_entry_from_wire(method::CLIENT_LIST_HOSTS_NAME, host))
            .collect()
    }

    pub async fn subscribe_hosts(&self) -> Result<HostEventStream, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .subscribe_hosts(wire::SubscribeHostsRequest {})
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        Ok(HostEventStream {
            inner: ClientServiceResponseStream { stream: response },
            done: false,
        })
    }

    pub async fn subscribe_agents(&self) -> Result<AgentEventStream, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .subscribe_agents(wire::SubscribeAgentsRequest {})
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        Ok(AgentEventStream {
            inner: ClientServiceResponseStream { stream: response },
            done: false,
        })
    }

    pub async fn debug_dump(&self, format: DebugFormat) -> Result<String, ClientError> {
        self.debug_dump_verbose(false, format).await
    }

    pub async fn debug_dump_verbose(
        &self,
        verbose: bool,
        format: DebugFormat,
    ) -> Result<String, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .debug(wire::DebugRequest {
                verbose,
                format: debug_format_to_wire(format),
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        Ok(response.dump)
    }

    pub async fn handle_hook(
        &self,
        payload: Bytes,
        env: HashMap<String, String>,
    ) -> Result<(), ClientError> {
        self.ensure_open()?;
        let (agent_id, external) = hook_target_from_payload(&payload)?;
        self.inner
            .lock()
            .await
            .handle_hook(wire::HandleHookRequest {
                agent_id: agent_id.as_bytes().to_vec(),
                payload: payload.to_vec(),
                external,
                env,
            })
            .await
            .map_err(status_to_client_error)?;
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), ClientError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ClientError::Protocol(ProtocolError::Unreachable {
                message: "ClientService connection is closed".to_string(),
            }));
        }
        Ok(())
    }
}

pub(crate) fn client_create_request_to_wire(
    request: CreateAgentRequest,
) -> Result<wire::ClientCreateAgentRequest, ClientError> {
    let agent = match request.agent_type {
        crate::agents::AgentType::Claude { driver } => {
            wire::client_create_agent_request::Agent::Claude(wire::ClaudeCreateConfig {
                working_dir: path_to_wire_string(
                    method::CLIENT_CREATE_NAME,
                    "ClientCreateAgentRequest.working_dir",
                    &request.working_dir,
                )?,
                args: request.args,
                initial_terminal_size: request.terminal_size.map(terminal_size_to_wire),
                driver: crate::agents::claude_driver_to_wire(driver) as i32,
            })
        }
        crate::agents::AgentType::Codex {
            model,
            approval_policy,
            sandbox_policy,
            resume_thread_id,
        } => {
            if !request.args.is_empty() {
                return Err(ClientError::Encode {
                    method: method::CLIENT_CREATE_NAME,
                    message: "Codex agents take no argv; CreateAgentRequest.args must be empty"
                        .to_string(),
                });
            }
            wire::client_create_agent_request::Agent::Codex(wire::CodexCreateConfig {
                cwd: path_to_wire_string(
                    method::CLIENT_CREATE_NAME,
                    "ClientCreateAgentRequest.cwd",
                    &request.working_dir,
                )?,
                model,
                approval_policy,
                sandbox_policy,
                resume_thread_id,
            })
        }
        #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
        crate::agents::AgentType::TestAgent { command } => {
            wire::client_create_agent_request::Agent::TestAgent(wire::TestAgentCreateConfig {
                command,
                working_dir: path_to_wire_string(
                    method::CLIENT_CREATE_NAME,
                    "ClientCreateAgentRequest.working_dir",
                    &request.working_dir,
                )?,
                initial_terminal_size: request.terminal_size.map(terminal_size_to_wire),
            })
        }
    };

    Ok(wire::ClientCreateAgentRequest {
        agent_id: request.agent_id.as_bytes().to_vec(),
        name: request.name,
        host_id: request.host_id.map(|host_id| host_id.as_bytes().to_vec()),
        parent: request.parent.map(|parent| wire::AgentParent {
            agent_id: parent.agent_id.as_bytes().to_vec(),
            host_id: parent.host_id.as_bytes().to_vec(),
        }),
        initial_prompt: request.initial_prompt,
        agent: Some(agent),
    })
}

fn terminal_size_to_wire(size: crate::agents::TerminalSize) -> wire::TerminalSize {
    wire::TerminalSize {
        rows: u32::from(size.rows),
        cols: u32::from(size.cols),
    }
}

fn path_to_wire_string(
    method: &'static str,
    field: &'static str,
    path: &std::path::Path,
) -> Result<String, ClientError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| ClientError::Encode {
            method,
            message: format!("{field} must be valid UTF-8"),
        })
}

pub(crate) fn agent_ref(identifier: AgentIdentifier) -> wire::AgentRef {
    let identifier = match identifier {
        AgentIdentifier::Id(agent_id) => {
            wire::agent_ref::Identifier::AgentId(agent_id.as_bytes().to_vec())
        }
        AgentIdentifier::Name(name) => wire::agent_ref::Identifier::Name(name),
    };
    wire::AgentRef {
        identifier: Some(identifier),
    }
}

pub(crate) fn peer_ref(identifier: PeerIdentifier) -> wire::PeerRef {
    let identifier = match identifier {
        PeerIdentifier::Id(host_id) => {
            wire::peer_ref::Identifier::HostId(host_id.as_bytes().to_vec())
        }
        PeerIdentifier::Name(name) => wire::peer_ref::Identifier::Name(name),
    };
    wire::PeerRef {
        identifier: Some(identifier),
    }
}

fn agent_response_to_agent(
    method: &'static str,
    agent: Option<wire::Agent>,
) -> Result<Agent, ClientError> {
    let agent = agent.ok_or_else(|| ClientError::Decode {
        method,
        message: "missing Agent response field".to_string(),
    })?;
    wire_agent_to_agent(method, agent)
}

fn wire_agent_to_agent(method: &'static str, agent: wire::Agent) -> Result<Agent, ClientError> {
    crate::agents::agent_from_wire(agent).map_err(|error| ClientError::Decode {
        method,
        message: error.to_string(),
    })
}

pub(crate) fn peer_entry_from_wire(
    method: &'static str,
    peer: wire::PeerEntry,
) -> Result<PeerEntry, ClientError> {
    let host_id = uuid_from_wire_bytes(method, "PeerEntry.host_id", peer.host_id)?;
    if peer.pubkey.len() != PAIRING_PUBKEY_LEN {
        return Err(ClientError::Decode {
            method,
            message: format!(
                "PeerEntry.pubkey must be 32 bytes, got {}",
                peer.pubkey.len()
            ),
        });
    }
    let paired_at =
        DateTime::<Utc>::from_timestamp_millis(peer.paired_at_unix_ms).ok_or_else(|| {
            ClientError::Decode {
                method,
                message: format!(
                    "PeerEntry.paired_at_unix_ms is out of range: {}",
                    peer.paired_at_unix_ms
                ),
            }
        })?;
    let reachabilities = peer
        .reachabilities
        .into_iter()
        .map(|reachability| peer_reachability_from_wire(method, reachability))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PeerEntry {
        host_id,
        name: peer.name,
        pubkey: peer.pubkey,
        paired_at,
        reachabilities,
    })
}

fn peer_reachability_from_wire(
    method: &'static str,
    reachability: wire::PeerReachability,
) -> Result<PeerReachability, ClientError> {
    match reachability.kind.ok_or_else(|| ClientError::Decode {
        method,
        message: "PeerReachability.kind is missing".to_string(),
    })? {
        wire::peer_reachability::Kind::Cloud(_) => Ok(PeerReachability::Cloud),
        wire::peer_reachability::Kind::SshTarget(target) => Ok(PeerReachability::Ssh {
            profile: crate::installation::ProfileId(target.profile_id.parse().map_err(
                |error| ClientError::Decode {
                    method,
                    message: format!("PeerReachability.ssh_target.profile_id is invalid: {error}"),
                },
            )?),
            target: target.target,
        }),
        wire::peer_reachability::Kind::DirectTcpAddr(addr) => {
            let addr = addr
                .parse::<SocketAddr>()
                .map_err(|error| ClientError::Decode {
                    method,
                    message: format!("PeerReachability.direct_tcp_addr is invalid: {error}"),
                })?;
            Ok(PeerReachability::DirectTcp { addr })
        }
    }
}

fn client_service_session_response_to_event(
    response: wire::SubscribeSessionResponse,
) -> Result<SubscribeSessionEvent, ClientError> {
    let event = response.event.ok_or_else(|| ClientError::Decode {
        method: method::CLIENT_SUBSCRIBE_SESSION_NAME,
        message: "missing SubscribeSessionResponse event".to_string(),
    })?;
    let event = match event {
        wire::subscribe_session_response::Event::Opened(_) => SubscribeSessionEvent::Opened,
        wire::subscribe_session_response::Event::Output(output) => SubscribeSessionEvent::Output {
            payload: crate::agents::session_output_payload_from_wire(output).map_err(|error| {
                ClientError::Decode {
                    method: method::CLIENT_SUBSCRIBE_SESSION_NAME,
                    message: error.to_string(),
                }
            })?,
        },
        wire::subscribe_session_response::Event::ReplayComplete(replay_complete) => {
            SubscribeSessionEvent::ReplayComplete {
                cursor: replay_complete.cursor,
            }
        }
        wire::subscribe_session_response::Event::Closed(closed) => SubscribeSessionEvent::Closed {
            reason: client_service_session_close_reason(closed)?,
        },
    };
    Ok(event)
}

fn client_service_session_close_reason(
    closed: wire::SessionClosed,
) -> Result<SessionCloseReason, ClientError> {
    let reason = closed.reason.ok_or_else(|| ClientError::Decode {
        method: method::CLIENT_SUBSCRIBE_SESSION_NAME,
        message: "missing SessionClosed reason".to_string(),
    })?;
    match reason {
        wire::session_closed::Reason::AgentDeleted(_) => Ok(SessionCloseReason::AgentDeleted),
        wire::session_closed::Reason::AgentExited(exited) => Ok(SessionCloseReason::AgentExited {
            exit_code: exited.exit_code,
        }),
        wire::session_closed::Reason::HostUnreachable(_) => Ok(SessionCloseReason::HostUnreachable),
        wire::session_closed::Reason::InternalError(error) => {
            Ok(SessionCloseReason::InternalError {
                detail: error.detail,
            })
        }
    }
}

pub(crate) fn host_entry_from_wire(
    method: &'static str,
    host: wire::HostEntry,
) -> Result<HostEntry, ClientError> {
    let id = uuid_from_wire_bytes(method, "HostEntry.host_id", host.host_id)?;
    let trust_status = match wire::HostTrustStatus::try_from(host.trust_status).map_err(|_| {
        ClientError::Decode {
            method,
            message: format!("invalid HostEntry.trust_status {}", host.trust_status),
        }
    })? {
        wire::HostTrustStatus::Trusted => HostTrustStatus::Trusted,
        wire::HostTrustStatus::UntrustedButOnline => HostTrustStatus::UntrustedButOnline,
        wire::HostTrustStatus::Unspecified => {
            return Err(ClientError::Decode {
                method,
                message: "HostEntry.trust_status is unspecified".to_string(),
            });
        }
    };
    if host.online && (host.version.is_none() || host.capabilities.is_none()) {
        return Err(ClientError::Decode {
            method,
            message: "online HostEntry requires version and capabilities".to_string(),
        });
    }
    if !host.online && (host.version.is_some() || host.capabilities.is_some()) {
        return Err(ClientError::Decode {
            method,
            message: "non-online HostEntry must not include version or capabilities".to_string(),
        });
    }
    if trust_status == HostTrustStatus::UntrustedButOnline && !host.online {
        return Err(ClientError::Decode {
            method,
            message: "untrusted HostEntry must be online".to_string(),
        });
    }
    let capabilities = host
        .capabilities
        .map(|capabilities| capabilities_from_wire(Some(capabilities)))
        .transpose()
        .map_err(|error| ClientError::Decode {
            method,
            message: error.to_string(),
        })?;
    Ok(HostEntry {
        id,
        name: host.name,
        online: host.online,
        version: host.version,
        capabilities,
        trust_status,
        last_dial_error: host.last_dial_error,
    })
}

fn client_service_host_response_to_host_event(
    response: wire::SubscribeHostsResponse,
) -> Result<HostEvent, ClientError> {
    let event = response.event.ok_or_else(|| ClientError::Decode {
        method: method::CLIENT_SUBSCRIBE_HOSTS_NAME,
        message: "missing SubscribeHostsResponse event".to_string(),
    })?;
    match event {
        wire::subscribe_hosts_response::Event::HostUpdated(updated) => {
            let host = updated.host.ok_or_else(|| ClientError::Decode {
                method: method::CLIENT_SUBSCRIBE_HOSTS_NAME,
                message: "missing HostUpdated.host".to_string(),
            })?;
            let host = host_entry_from_wire(method::CLIENT_SUBSCRIBE_HOSTS_NAME, host)?;
            Ok(HostEvent::HostUpdated { host })
        }
        wire::subscribe_hosts_response::Event::HostRemoved(removed) => Ok(HostEvent::HostRemoved {
            id: uuid_from_wire_bytes(
                method::CLIENT_SUBSCRIBE_HOSTS_NAME,
                "HostRemoved.host_id",
                removed.host_id,
            )?,
        }),
        wire::subscribe_hosts_response::Event::SnapshotComplete(_) => {
            Ok(HostEvent::SnapshotComplete)
        }
    }
}

fn client_service_agent_response_to_agent_event(
    response: wire::SubscribeAgentsResponse,
) -> Result<Option<AgentEvent>, ClientError> {
    let event = response.event.ok_or_else(|| ClientError::Decode {
        method: method::CLIENT_SUBSCRIBE_AGENTS_NAME,
        message: "missing SubscribeAgentsResponse event".to_string(),
    })?;
    let event = match event {
        wire::subscribe_agents_response::Event::AgentUp(up) => Some(AgentEvent::AgentUp {
            agent: required_wire_agent(
                method::CLIENT_SUBSCRIBE_AGENTS_NAME,
                "AgentUp.agent",
                up.agent,
            )?,
        }),
        wire::subscribe_agents_response::Event::AgentUpdated(updated) => {
            Some(AgentEvent::AgentUpdated {
                agent: required_wire_agent(
                    method::CLIENT_SUBSCRIBE_AGENTS_NAME,
                    "AgentUpdated.agent",
                    updated.agent,
                )?,
            })
        }
        wire::subscribe_agents_response::Event::AgentDown(down) => Some(AgentEvent::AgentDown {
            agent_id: uuid_from_wire_bytes(
                method::CLIENT_SUBSCRIBE_AGENTS_NAME,
                "AgentDown.agent_id",
                down.agent_id,
            )?,
        }),
        wire::subscribe_agents_response::Event::SnapshotComplete(_) => None,
    };
    Ok(event)
}

fn required_wire_agent(
    method: &'static str,
    field: &'static str,
    agent: Option<wire::Agent>,
) -> Result<Agent, ClientError> {
    let agent = agent.ok_or_else(|| ClientError::Decode {
        method,
        message: format!("missing {field}"),
    })?;
    crate::agents::agent_from_wire(agent).map_err(|error| ClientError::Decode {
        method,
        message: error.to_string(),
    })
}

pub(crate) fn pairing_start_from_wire(
    method: &'static str,
    response: wire::StartPairingResponse,
) -> Result<PairingStart, ClientError> {
    let identity = response.identity.ok_or_else(|| ClientError::Decode {
        method,
        message: "missing StartPairingResponse.identity".to_string(),
    })?;
    let secret = match response.secret.ok_or_else(|| ClientError::Decode {
        method,
        message: "missing StartPairingResponse.secret".to_string(),
    })? {
        wire::start_pairing_response::Secret::Pin(pin) => PairingSecret::Pin(pin),
        wire::start_pairing_response::Secret::QrSecret(secret) => PairingSecret::QrSecret(secret),
    };
    Ok(PairingStart {
        identity: pairing_identity_to_peer(method, identity)?,
        ttl_seconds: response.ttl_seconds,
        tcp_port: response
            .tcp_port
            .map(u16::try_from)
            .transpose()
            .map_err(|_| ClientError::Decode {
                method,
                message: "StartPairingResponse.tcp_port exceeds u16".to_string(),
            })?,
        cloud_url: response.cloud_url,
        secret,
    })
}

pub(crate) fn pairing_identity_from_wire(
    method: &'static str,
    identity: wire::PairingIdentity,
) -> Result<(Uuid, Vec<u8>, String), ClientError> {
    let peer = pairing_identity_to_peer(method, identity)?;
    Ok((peer.host_id, peer.pubkey, peer.name))
}

fn pairing_identity_to_peer(
    method: &'static str,
    identity: wire::PairingIdentity,
) -> Result<SshPairingPeer, ClientError> {
    if identity.pubkey.len() != PAIRING_PUBKEY_LEN {
        return Err(ClientError::Decode {
            method,
            message: format!(
                "PairingIdentity.pubkey must be {PAIRING_PUBKEY_LEN} bytes, got {}",
                identity.pubkey.len()
            ),
        });
    }
    if identity.name.len() > MAX_PAIRING_NAME_BYTES {
        return Err(ClientError::Decode {
            method,
            message: format!("PairingIdentity.name must be at most {MAX_PAIRING_NAME_BYTES} bytes"),
        });
    }
    Ok(SshPairingPeer {
        host_id: uuid_from_wire_bytes(method, "PairingIdentity.host_id", identity.host_id)?,
        pubkey: identity.pubkey,
        name: identity.name,
    })
}

fn uuid_from_wire_bytes(
    method: &'static str,
    field: &'static str,
    bytes: Vec<u8>,
) -> Result<Uuid, ClientError> {
    Uuid::from_slice(&bytes).map_err(|error| ClientError::Decode {
        method,
        message: format!("invalid {field}: {error}"),
    })
}

pub(crate) fn status_to_client_error(status: tonic::Status) -> ClientError {
    let message = status.message().to_string();
    if status.code() == tonic::Code::Unavailable
        && let Some(reason) = shutdown_reason_from_status_metadata(&status)
    {
        return ClientError::ServerShutdown(reason);
    }
    if let Some(error) = protocol_error_from_status_details(&status) {
        return ClientError::Protocol(error);
    }
    let error = match status.code() {
        tonic::Code::NotFound => ProtocolError::NoAgentFound,
        tonic::Code::Unimplemented => ProtocolError::Unimplemented { message },
        tonic::Code::Cancelled => ProtocolError::Cancelled { message },
        tonic::Code::InvalidArgument => ProtocolError::InvalidArgument { message },
        tonic::Code::AlreadyExists => ProtocolError::AlreadyExists { message },
        tonic::Code::PermissionDenied => ProtocolError::PermissionDenied { message },
        tonic::Code::FailedPrecondition => ProtocolError::FailedPrecondition { message },
        tonic::Code::Unavailable => ProtocolError::Unreachable { message },
        tonic::Code::Unauthenticated => ProtocolError::InvalidCredentials,
        tonic::Code::ResourceExhausted => ProtocolError::ResourceExhausted { message },
        _ => ProtocolError::ServerError { message },
    };
    ClientError::Protocol(error)
}

fn shutdown_reason_from_status_metadata(status: &tonic::Status) -> Option<ShutdownReason> {
    status
        .metadata()
        .get(SHUTDOWN_REASON_METADATA_KEY)
        .and_then(|value| value.to_str().ok())
        .and_then(ShutdownReason::from_wire_value)
}

fn hook_target_from_payload(payload: &[u8]) -> Result<(Uuid, bool), ClientError> {
    if let Ok(agent_id) = std::env::var("AMUX_AGENT_ID") {
        return agent_id
            .parse::<Uuid>()
            .map(|agent_id| (agent_id, false))
            .map_err(|error| ClientError::Encode {
                method: method::CLIENT_HANDLE_HOOK_NAME,
                message: format!("invalid AMUX_AGENT_ID: {error}"),
            });
    }

    serde_json::from_slice::<serde_json::Value>(payload)
        .map_err(|error| ClientError::Encode {
            method: method::CLIENT_HANDLE_HOOK_NAME,
            message: format!("invalid Claude hook payload: {error}"),
        })
        .and_then(|value| {
            value
                .get("session_id")
                .and_then(|id| id.as_str())
                .ok_or_else(|| ClientError::Encode {
                    method: method::CLIENT_HANDLE_HOOK_NAME,
                    message: "Claude hook payload missing session_id".to_string(),
                })
                .and_then(|id| {
                    id.parse::<Uuid>().map_err(|error| ClientError::Encode {
                        method: method::CLIENT_HANDLE_HOOK_NAME,
                        message: format!("invalid Claude hook session_id: {error}"),
                    })
                })
        })
        .map(|agent_id| (agent_id, true))
}

pub(crate) fn debug_format_to_wire(format: DebugFormat) -> i32 {
    match format {
        DebugFormat::Yaml => wire::DebugFormat::Yaml as i32,
        DebugFormat::Json => wire::DebugFormat::Json as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_create_request_encodes_each_claude_driver() {
        for (driver, expected) in [
            (crate::agents::ClaudeDriver::Pty, wire::ClaudeDriver::Pty),
            (crate::agents::ClaudeDriver::Sdk, wire::ClaudeDriver::Sdk),
        ] {
            let request = client_create_request_to_wire(CreateAgentRequest {
                agent_id: Uuid::from_u128(7),
                host_id: None,
                name: Some("claude".into()),
                agent_type: crate::agents::AgentType::Claude { driver },
                working_dir: "/tmp/work".into(),
                terminal_size: None,
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            })
            .unwrap();

            let Some(wire::client_create_agent_request::Agent::Claude(config)) = request.agent
            else {
                panic!("expected Claude create config");
            };
            assert_eq!(
                wire::ClaudeDriver::try_from(config.driver).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn client_create_request_encodes_codex_config() {
        let request = client_create_request_to_wire(CreateAgentRequest {
            agent_id: Uuid::from_u128(7),
            host_id: None,
            name: Some("codex".into()),
            agent_type: crate::agents::AgentType::Codex {
                model: Some("gpt-5.6-sol".into()),
                approval_policy: Some("on-request".into()),
                sandbox_policy: Some("workspace-write".into()),
                resume_thread_id: Some("thread-7".into()),
            },
            working_dir: "/tmp/work".into(),
            terminal_size: None,
            args: Vec::new(),
            parent: Some(crate::AgentParent {
                agent_id: Uuid::from_u128(8),
                host_id: Uuid::from_u128(9),
            }),
            initial_prompt: Some("inspect the protocol".into()),
        })
        .unwrap();

        let Some(wire::client_create_agent_request::Agent::Codex(config)) = request.agent else {
            panic!("expected Codex create config");
        };
        assert_eq!(config.cwd, "/tmp/work");
        assert_eq!(config.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(config.approval_policy.as_deref(), Some("on-request"));
        assert_eq!(config.sandbox_policy.as_deref(), Some("workspace-write"));
        assert_eq!(config.resume_thread_id.as_deref(), Some("thread-7"));
        assert_eq!(
            request.parent,
            Some(wire::AgentParent {
                agent_id: Uuid::from_u128(8).as_bytes().to_vec(),
                host_id: Uuid::from_u128(9).as_bytes().to_vec(),
            })
        );
        assert_eq!(
            request.initial_prompt.as_deref(),
            Some("inspect the protocol")
        );
    }

    #[test]
    fn client_create_request_rejects_codex_args() {
        let error = client_create_request_to_wire(CreateAgentRequest {
            agent_id: Uuid::from_u128(7),
            host_id: None,
            name: Some("codex".into()),
            agent_type: crate::agents::AgentType::Codex {
                model: None,
                approval_policy: None,
                sandbox_policy: None,
                resume_thread_id: None,
            },
            working_dir: "/tmp/work".into(),
            terminal_size: None,
            args: vec!["--model".into(), "gpt-5.6-sol".into()],
            parent: None,
            initial_prompt: None,
        })
        .unwrap_err();

        assert!(error.to_string().contains("Codex agents take no argv"));
        assert!(
            error
                .to_string()
                .contains("CreateAgentRequest.args must be empty")
        );
    }

    #[cfg(unix)]
    #[test]
    fn client_create_request_rejects_non_utf8_working_dir() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let error = client_create_request_to_wire(CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            host_id: None,
            name: None,
            agent_type: crate::agents::AgentType::Claude {
                driver: crate::agents::ClaudeDriver::Pty,
            },
            working_dir: OsString::from_vec(vec![0xff]).into(),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        })
        .unwrap_err();

        assert!(error.to_string().contains("must be valid UTF-8"));
    }

    #[test]
    fn pairing_start_response_decodes_identity_transport_metadata_and_secret() {
        let host_id = Uuid::from_u128(42);
        let start = pairing_start_from_wire(
            method::PROFILE_START_PAIRING_NAME,
            wire::StartPairingResponse {
                identity: Some(wire::PairingIdentity {
                    host_id: host_id.as_bytes().to_vec(),
                    pubkey: vec![7; 32],
                    name: "laptop".to_string(),
                }),
                ttl_seconds: 300,
                tcp_port: Some(4242),
                cloud_url: "https://cloud.example".to_string(),
                secret: Some(wire::start_pairing_response::Secret::Pin(
                    "123456".to_string(),
                )),
            },
        )
        .unwrap();

        assert_eq!(start.identity.host_id, host_id);
        assert_eq!(start.identity.pubkey, vec![7; 32]);
        assert_eq!(start.identity.name, "laptop");
        assert_eq!(start.ttl_seconds, 300);
        assert_eq!(start.tcp_port, Some(4242));
        assert_eq!(start.cloud_url, "https://cloud.example");
        assert_eq!(start.secret, PairingSecret::Pin("123456".to_string()));
    }

    #[test]
    fn pairing_start_response_rejects_invalid_tcp_port() {
        let error = pairing_start_from_wire(
            method::PROFILE_START_PAIRING_NAME,
            wire::StartPairingResponse {
                identity: Some(wire::PairingIdentity {
                    host_id: Uuid::from_u128(42).as_bytes().to_vec(),
                    pubkey: vec![7; 32],
                    name: "laptop".to_string(),
                }),
                ttl_seconds: 300,
                tcp_port: Some(u32::from(u16::MAX) + 1),
                cloud_url: "https://cloud.example".to_string(),
                secret: Some(wire::start_pairing_response::Secret::QrSecret(vec![1; 32])),
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("tcp_port"));
    }
}

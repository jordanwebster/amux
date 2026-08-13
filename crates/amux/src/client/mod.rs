use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures_util::Stream;
use prost::Message as ProstMessage;
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::agents::{
    Agent, AgentEvent, CreateAgentRequest, SessionCloseReason, SubscribeSessionEvent,
};
use crate::debug::DebugFormat;
use crate::pairing::ssh::SshPairingPeer;
use crate::protocol::{ProtocolError, wire};
use crate::routing::{HostEntry, HostEvent, HostTrustStatus, capabilities_from_wire};
use crate::server::{SHUTDOWN_REASON_METADATA_KEY, ShutdownReason};
use crate::transport::TransportError;
use crate::{AgentIdentifier, PeerIdentifier, SendInputRequest, SubscribeSessionRequest};

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
    pub(super) const CLIENT_SUBSCRIBE_SESSION_NAME: &str =
        "/amux.v1.ClientService/SubscribeSession";
    pub(super) const CLIENT_HANDLE_HOOK_NAME: &str = "/amux.v1.ClientService/HandleHook";
    pub(super) const CLIENT_START_PAIRING_NAME: &str = "/amux.v1.ClientService/StartPairing";
    pub(super) const CLIENT_PAIR_PIN_CLOUD_PEER_NAME: &str =
        "/amux.v1.ClientService/PairPinCloudPeer";
    pub(super) const CLIENT_PAIR_QR_CLOUD_PEER_NAME: &str =
        "/amux.v1.ClientService/PairQrCloudPeer";
    pub(super) const CLIENT_LIST_PEERS_NAME: &str = "/amux.v1.ClientService/ListPeers";
    pub(super) const CLIENT_GET_PEER_NAME: &str = "/amux.v1.ClientService/GetPeer";
    pub(super) const CLIENT_UNPAIR_NAME: &str = "/amux.v1.ClientService/Unpair";
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
    Ssh { target: String },
    DirectTcp { addr: SocketAddr },
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
    guard: Option<Arc<dyn Send + Sync>>,
}

impl Client {
    pub(crate) fn from_client_service_channel(
        channel: Channel,
        guard: Option<Arc<dyn Send + Sync>>,
    ) -> Self {
        Self {
            inner: Arc::new(AsyncMutex::new(
                wire::client_service_client::ClientServiceClient::new(channel),
            )),
            closed: Arc::new(AtomicBool::new(false)),
            guard,
        }
    }

    pub fn owns_embedded_server(&self) -> bool {
        self.guard.is_some()
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
        self.ensure_open()?;
        let identifier = identifier.into();
        self.inner
            .lock()
            .await
            .delete_agent(wire::ClientDeleteAgentRequest {
                agent: Some(agent_ref(identifier)),
            })
            .await
            .map_err(status_to_client_error)?;
        Ok(())
    }

    pub async fn subscribe_session(
        &self,
        request: SubscribeSessionRequest,
    ) -> Result<SessionStream, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .subscribe_session(wire::ClientSubscribeSessionRequest {
                agent: Some(agent_ref(request.agent)),
                io_protocol: request.io_protocol,
                args: request.args.map(|args| args.to_vec()),
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
        self.inner
            .lock()
            .await
            .send_input(wire::ClientSendInputRequest {
                agent: Some(agent_ref(request.agent)),
                io_protocol: request.io_protocol,
                event: Some(wire::client_send_input_request::Event::Input(
                    wire::SessionInput {
                        input_id: Uuid::new_v4().as_bytes().to_vec(),
                        payload: request.payload.to_vec(),
                    },
                )),
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
        self.list_hosts_scoped(wire::list_hosts_request::Scope::All)
            .await
    }

    pub async fn list_pairing_hosts(&self) -> Result<Vec<HostEntry>, ClientError> {
        self.list_hosts_scoped(wire::list_hosts_request::Scope::PairingCandidates)
            .await
    }

    async fn list_hosts_scoped(
        &self,
        scope: wire::list_hosts_request::Scope,
    ) -> Result<Vec<HostEntry>, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .list_hosts(wire::ListHostsRequest {
                scope: scope as i32,
            })
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

    pub async fn shutdown(&self) -> Result<(), ClientError> {
        self.ensure_open()?;
        self.inner
            .lock()
            .await
            .shutdown(wire::ShutdownRequest {})
            .await
            .map_err(status_to_client_error)?;
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
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
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .suspend(wire::SuspendRequest {
                reason: reason as i32,
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        self.closed.store(true, Ordering::SeqCst);
        Ok(SuspendSummary {
            suspended_count: response.suspended_count,
        })
    }

    pub async fn resume(&self) -> Result<ResumeSummary, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .resume(wire::ResumeRequest {})
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        Ok(ResumeSummary {
            resumed_count: response.resumed_count,
            failed_count: response.failed_count,
        })
    }

    pub async fn start_pin_pairing(&self) -> Result<PairingStart, ClientError> {
        self.start_pairing(wire::start_pairing_request::Mode::Pin, false)
            .await
    }

    pub async fn start_lan_pin_pairing(&self) -> Result<PairingStart, ClientError> {
        self.start_pairing(wire::start_pairing_request::Mode::Pin, true)
            .await
    }

    pub async fn start_qr_pairing(&self) -> Result<PairingStart, ClientError> {
        self.start_pairing(wire::start_pairing_request::Mode::Qr, false)
            .await
    }

    async fn start_pairing(
        &self,
        mode: wire::start_pairing_request::Mode,
        require_lan_direct: bool,
    ) -> Result<PairingStart, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .start_pairing(wire::StartPairingRequest {
                mode: mode as i32,
                require_lan_direct,
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        pairing_start_from_wire(response)
    }

    pub async fn cancel_pairing(&self) -> Result<(), ClientError> {
        self.ensure_open()?;
        self.inner
            .lock()
            .await
            .cancel_pairing(wire::CancelPairingRequest {})
            .await
            .map_err(status_to_client_error)?;
        Ok(())
    }

    pub async fn pairing_is_active(&self) -> Result<bool, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .get_pairing_status(wire::GetPairingStatusRequest {})
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        Ok(response.active)
    }

    pub async fn pair_ssh_peer(
        &self,
        peer: SshPairingPeer,
        ssh_target: Option<String>,
    ) -> Result<(), ClientError> {
        let reachability = ssh_target.map(wire::pair_peer_request::Reachability::SshTarget);
        self.pair_peer(peer, reachability).await
    }

    pub async fn pair_direct_peer(
        &self,
        peer: SshPairingPeer,
        address: SocketAddr,
    ) -> Result<(), ClientError> {
        self.pair_peer(
            peer,
            Some(wire::pair_peer_request::Reachability::DirectTcpAddr(
                address.to_string(),
            )),
        )
        .await
    }

    pub async fn pair_pin_cloud_peer(
        &self,
        host_id: uuid::Uuid,
        pin: String,
    ) -> Result<SshPairingPeer, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .pair_pin_cloud_peer(wire::PairPinCloudPeerRequest {
                host_id: host_id.as_bytes().to_vec(),
                pin,
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let peer = response.peer.ok_or_else(|| ClientError::Decode {
            method: method::CLIENT_PAIR_PIN_CLOUD_PEER_NAME,
            message: "missing PairPinCloudPeerResponse.peer".to_string(),
        })?;
        let (host_id, pubkey, name) =
            pairing_identity_from_wire(method::CLIENT_PAIR_PIN_CLOUD_PEER_NAME, peer)?;
        Ok(SshPairingPeer {
            host_id,
            pubkey,
            name,
        })
    }

    pub async fn pair_qr_cloud_peer(
        &self,
        host_id: uuid::Uuid,
        secret: Vec<u8>,
    ) -> Result<SshPairingPeer, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .pair_qr_cloud_peer(wire::PairQrCloudPeerRequest {
                host_id: host_id.as_bytes().to_vec(),
                secret,
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let peer = response.peer.ok_or_else(|| ClientError::Decode {
            method: method::CLIENT_PAIR_QR_CLOUD_PEER_NAME,
            message: "missing PairQrCloudPeerResponse.peer".to_string(),
        })?;
        let (host_id, pubkey, name) =
            pairing_identity_from_wire(method::CLIENT_PAIR_QR_CLOUD_PEER_NAME, peer)?;
        Ok(SshPairingPeer {
            host_id,
            pubkey,
            name,
        })
    }

    pub async fn list_peers(&self) -> Result<Vec<PeerEntry>, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .list_peers(wire::ListPeersRequest {})
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        response
            .peers
            .into_iter()
            .map(|peer| peer_entry_from_wire(method::CLIENT_LIST_PEERS_NAME, peer))
            .collect()
    }

    pub async fn get_peer(
        &self,
        peer: impl Into<PeerIdentifier>,
    ) -> Result<PeerEntry, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .get_peer(wire::GetPeerRequest {
                peer: Some(peer_ref(peer.into())),
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let peer = response.peer.ok_or_else(|| ClientError::Decode {
            method: method::CLIENT_GET_PEER_NAME,
            message: "missing GetPeerResponse.peer".to_string(),
        })?;
        peer_entry_from_wire(method::CLIENT_GET_PEER_NAME, peer)
    }

    pub async fn unpair(
        &self,
        peer: impl Into<PeerIdentifier>,
        reason: impl Into<String>,
    ) -> Result<PeerEntry, ClientError> {
        self.ensure_open()?;
        let response = self
            .inner
            .lock()
            .await
            .unpair(wire::UnpairRequest {
                peer: Some(peer_ref(peer.into())),
                reason: reason.into(),
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let peer = response.removed_peer.ok_or_else(|| ClientError::Decode {
            method: method::CLIENT_UNPAIR_NAME,
            message: "missing UnpairResponse.removed_peer".to_string(),
        })?;
        peer_entry_from_wire(method::CLIENT_UNPAIR_NAME, peer)
    }

    async fn pair_peer(
        &self,
        peer: SshPairingPeer,
        reachability: Option<wire::pair_peer_request::Reachability>,
    ) -> Result<(), ClientError> {
        self.ensure_open()?;
        self.inner
            .lock()
            .await
            .pair_peer(wire::PairPeerRequest {
                peer: Some(wire::PairingIdentity {
                    host_id: peer.host_id.as_bytes().to_vec(),
                    pubkey: peer.pubkey,
                    name: peer.name,
                }),
                reachability,
            })
            .await
            .map_err(status_to_client_error)?;
        Ok(())
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

    pub async fn handle_hook(&self, payload: Bytes) -> Result<(), ClientError> {
        self.ensure_open()?;
        let (agent_id, external) = hook_target_from_payload(&payload)?;
        self.inner
            .lock()
            .await
            .handle_hook(wire::HandleHookRequest {
                agent_id: agent_id.as_bytes().to_vec(),
                payload: payload.to_vec(),
                external,
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

fn client_create_request_to_wire(
    request: CreateAgentRequest,
) -> Result<wire::ClientCreateAgentRequest, ClientError> {
    let agent = match request.agent_type {
        crate::agents::AgentType::Claude => {
            wire::client_create_agent_request::Agent::Claude(wire::ClaudeCreateConfig {
                working_dir: path_to_wire_string(
                    method::CLIENT_CREATE_NAME,
                    "ClientCreateAgentRequest.working_dir",
                    &request.working_dir,
                )?,
                args: request.args,
                initial_terminal_size: request.terminal_size.map(terminal_size_to_wire),
            })
        }
        crate::agents::AgentType::Codex {
            model,
            approval_policy,
            sandbox_policy,
            resume_thread_id,
        } => wire::client_create_agent_request::Agent::Codex(wire::CodexCreateConfig {
            cwd: path_to_wire_string(
                method::CLIENT_CREATE_NAME,
                "ClientCreateAgentRequest.cwd",
                &request.working_dir,
            )?,
            model,
            approval_policy,
            sandbox_policy,
            resume_thread_id,
        }),
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

fn agent_ref(identifier: AgentIdentifier) -> wire::AgentRef {
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

fn peer_ref(identifier: PeerIdentifier) -> wire::PeerRef {
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

fn peer_entry_from_wire(
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
        wire::peer_reachability::Kind::SshTarget(target) => Ok(PeerReachability::Ssh { target }),
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
            payload: output.payload,
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

fn host_entry_from_wire(
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
    Ok(HostEntry {
        id,
        name: host.name,
        online: host.online,
        version: host.version,
        capabilities: host
            .capabilities
            .map(|capabilities| capabilities_from_wire(Some(capabilities))),
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

fn pairing_start_from_wire(
    response: wire::StartPairingResponse,
) -> Result<PairingStart, ClientError> {
    let identity = response.identity.ok_or_else(|| ClientError::Decode {
        method: method::CLIENT_START_PAIRING_NAME,
        message: "missing StartPairingResponse.identity".to_string(),
    })?;
    let secret = match response.secret.ok_or_else(|| ClientError::Decode {
        method: method::CLIENT_START_PAIRING_NAME,
        message: "missing StartPairingResponse.secret".to_string(),
    })? {
        wire::start_pairing_response::Secret::Pin(pin) => PairingSecret::Pin(pin),
        wire::start_pairing_response::Secret::QrSecret(secret) => PairingSecret::QrSecret(secret),
    };
    Ok(PairingStart {
        identity: pairing_identity_to_peer(method::CLIENT_START_PAIRING_NAME, identity)?,
        ttl_seconds: response.ttl_seconds,
        tcp_port: response
            .tcp_port
            .map(u16::try_from)
            .transpose()
            .map_err(|_| ClientError::Decode {
                method: method::CLIENT_START_PAIRING_NAME,
                message: "StartPairingResponse.tcp_port exceeds u16".to_string(),
            })?,
        cloud_url: response.cloud_url,
        secret,
    })
}

fn pairing_identity_from_wire(
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

fn status_to_client_error(status: tonic::Status) -> ClientError {
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

fn protocol_error_from_status_details(status: &tonic::Status) -> Option<ProtocolError> {
    if status.details().is_empty() {
        return None;
    }
    wire::Error::decode(status.details())
        .ok()
        .map(wire::decode_protocol_error)
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

fn debug_format_to_wire(format: DebugFormat) -> i32 {
    match format {
        DebugFormat::Yaml => wire::DebugFormat::Yaml as i32,
        DebugFormat::Json => wire::DebugFormat::Json as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            agent_type: crate::agents::AgentType::Claude,
            working_dir: OsString::from_vec(vec![0xff]).into(),
            terminal_size: None,
            args: Vec::new(),
        })
        .unwrap_err();

        assert!(error.to_string().contains("must be valid UTF-8"));
    }

    #[test]
    fn pairing_start_response_decodes_identity_transport_metadata_and_secret() {
        let host_id = Uuid::from_u128(42);
        let start = pairing_start_from_wire(wire::StartPairingResponse {
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
        })
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
        let error = pairing_start_from_wire(wire::StartPairingResponse {
            identity: Some(wire::PairingIdentity {
                host_id: Uuid::from_u128(42).as_bytes().to_vec(),
                pubkey: vec![7; 32],
                name: "laptop".to_string(),
            }),
            ttl_seconds: 300,
            tcp_port: Some(u32::from(u16::MAX) + 1),
            cloud_url: "https://cloud.example".to_string(),
            secret: Some(wire::start_pairing_response::Secret::QrSecret(vec![1; 32])),
        })
        .unwrap_err();

        assert!(error.to_string().contains("tcp_port"));
    }
}

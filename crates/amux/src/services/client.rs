//! ClientService aggregation model.
//!
//! This module holds the state transitions behind the ClientService gRPC shim.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures_util::{Stream, StreamExt, stream};
use tokio::sync::{RwLock, mpsc, oneshot};
use tonic::transport::Channel;
use uuid::Uuid;

use crate::agents::{
    Agent, AgentEvent, AgentSession, CreateAgentConfig, CreateAgentRpcRequest,
    ExternalHookBootstrap, HookOutcome, SendInputRequest, SessionInputEvent, StopPolicy,
    SubscribeSessionEvent, SubscribeSessionRequest, TerminalSize,
};
use crate::connection::ConnectionManager;
use crate::debug::DebugFormat;
use crate::identity::IdentityError;
use crate::pairing::{PAIR_MODE_TTL, PairMode, PairModeError};
use crate::protocol::{ProtocolError, protocol_status, wire};
use crate::routing::{
    EventSource, FEATURE_CLOUD_RELAY, Host, HostEntry, HostEvent, HostReachabilityEvent,
    HostReachabilityStatus, HostTrustStatus, RoutingCore, capabilities_to_wire,
};
use crate::server::{SHUTDOWN_REASON_METADATA_KEY, ShutdownReason};
use crate::services::agent::AgentServiceCtx;
use crate::services::pairing::{
    LocalPairingIdentity, PeerTrustCommitContext, PeerTrustUpdate, SharedTrustCommitLock,
    commit_peer_trust, pair_by_spake2_initiator, pair_by_token_initiator,
};
use crate::services::{ReachabilityLinkConnector, resume_agents};
use crate::transport::{BoxedGrpcAuth, BoxedGrpcConnectInfo};
use crate::trust::{Reachability, SharedTrustStore, TrustEntry, TrustStore};
use crate::tunnel::TunnelPoolError;
use crate::user_state::{ServerState, ShutdownRequest};
use crate::{HostId, audit};

type TonicResult<T> = Result<tonic::Response<T>, tonic::Status>;
type ResponseStream<T> = Pin<Box<dyn Stream<Item = Result<T, tonic::Status>> + Send + 'static>>;

const REMOTE_AGENT_SUBSCRIPTION_RETRY_DELAY: Duration = Duration::from_millis(100);
const HOST_ID_LEN: usize = 16;
const PUBKEY_LEN: usize = 32;
const TOKEN_LEN: usize = 32;
const MAX_PAIRING_NAME_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentRef {
    Id(Uuid),
    Name(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostEventOutcome {
    Added,
    Updated,
    Removed { removed_agents: usize },
    IgnoredRelayOrUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentEventOutcome {
    Upserted,
    Removed,
    Ignored,
}

#[derive(Default)]
struct ClientServiceState {
    hosts_model: HashMap<Uuid, Host>,
    agents_model: HashMap<Uuid, Agent>,
    host_events: EventSource<HostEvent>,
    agent_events: EventSource<AgentEvent>,
    remote_agent_subs: HashMap<Uuid, tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) struct ClientService {
    state: Arc<RwLock<ClientServiceState>>,
    local_agents: AgentServiceCtx,
    server_state: Arc<RwLock<ServerState>>,
    remote_agent_connections: Arc<ConnectionManager>,
    pairing_trust: PairingTrustAccess,
    pair_mode: Arc<PairMode>,
    reachability_links: ReachabilityLinkConnector,
}

#[derive(Clone)]
pub(crate) struct PairingTrustAccess {
    local_pubkey: Vec<u8>,
    trust_store: SharedTrustStore,
    trust_commit_lock: SharedTrustCommitLock,
    data_dir: PathBuf,
}

impl PairingTrustAccess {
    pub(crate) fn new(
        local_pubkey: Vec<u8>,
        trust_store: SharedTrustStore,
        trust_commit_lock: SharedTrustCommitLock,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            local_pubkey,
            trust_store,
            trust_commit_lock,
            data_dir,
        }
    }
}

impl ClientService {
    pub(crate) fn new(
        local_agents: AgentServiceCtx,
        server_state: Arc<RwLock<ServerState>>,
        remote_agent_connections: Arc<ConnectionManager>,
        pairing_trust: PairingTrustAccess,
        pair_mode: Arc<PairMode>,
        reachability_links: ReachabilityLinkConnector,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(ClientServiceState::default())),
            local_agents,
            server_state,
            remote_agent_connections,
            pairing_trust,
            pair_mode,
            reachability_links,
        }
    }

    #[cfg(test)]
    pub(crate) async fn list_hosts(&self) -> Vec<Host> {
        self.hosts_snapshot().await
    }

    async fn hosts_snapshot(&self) -> Vec<Host> {
        {
            let state = self.state.read().await;
            sorted_values_by_id(&state.hosts_model, |host| host.id)
        }
    }

    pub(crate) async fn list_agents(&self) -> Vec<Agent> {
        let state = self.state.read().await;
        sorted_values_by_id(&state.agents_model, |agent| agent.id)
    }

    #[cfg(test)]
    pub(crate) async fn subscribe_hosts(&self) -> mpsc::Receiver<HostEvent> {
        self.state.write().await.host_events.subscribe()
    }

    pub(crate) async fn subscribe_hosts_with_snapshot(
        &self,
    ) -> (Vec<HostEntry>, mpsc::Receiver<HostEvent>) {
        let (snapshot, rx) = {
            let mut state = self.state.write().await;
            let snapshot = sorted_values_by_id(&state.hosts_model, |host| host.id);
            let rx = state.host_events.subscribe_drop_on_overflow();
            (snapshot, rx)
        };
        let snapshot = self.host_entries_for_online_hosts(snapshot, true).await;
        (snapshot, rx)
    }

    #[cfg(test)]
    pub(crate) async fn subscribe_agents(&self) -> mpsc::Receiver<AgentEvent> {
        self.state.write().await.agent_events.subscribe()
    }

    pub(crate) async fn subscribe_agents_with_snapshot(
        &self,
    ) -> (Vec<Agent>, mpsc::Receiver<AgentEvent>) {
        let mut state = self.state.write().await;
        let snapshot = sorted_values_by_id(&state.agents_model, |agent| agent.id);
        let rx = state.agent_events.subscribe_drop_on_overflow();
        (snapshot, rx)
    }

    pub(crate) async fn apply_host_event(&self, event: HostReachabilityEvent) -> HostEventOutcome {
        match event {
            HostReachabilityEvent::Added { host } => self.add_host(host).await,
            HostReachabilityEvent::Removed { host_id } => self.remove_host(host_id).await,
            HostReachabilityEvent::StatusChanged { host_id } => {
                if self.publish_host_status_update(host_id).await {
                    HostEventOutcome::Updated
                } else {
                    HostEventOutcome::IgnoredRelayOrUnknown
                }
            }
        }
    }

    async fn mark_client_visible_host_entries(&self, hosts: &[HostEntry]) {
        let host_ids = hosts
            .iter()
            .filter_map(|host| host.online.then_some(host.id))
            .collect::<Vec<_>>();
        self.remote_agent_connections
            .mark_client_visible_hosts(&host_ids)
            .await;
    }

    pub(crate) async fn apply_agent_event(&self, event: AgentEvent) -> AgentEventOutcome {
        match event {
            AgentEvent::AgentUp { agent } => self.upsert_agent(agent, AgentChangeKind::Up).await,
            AgentEvent::AgentUpdated { agent } => {
                self.upsert_agent(agent, AgentChangeKind::Updated).await
            }
            AgentEvent::AgentDown { agent_id } => {
                let mut state = self.state.write().await;
                if state.agents_model.remove(&agent_id).is_none() {
                    return AgentEventOutcome::Ignored;
                }
                state.agent_events.emit(AgentEvent::AgentDown { agent_id });
                AgentEventOutcome::Removed
            }
            AgentEvent::SnapshotComplete => AgentEventOutcome::Ignored,
        }
    }

    async fn apply_remote_agent_event(
        &self,
        source_host_id: Uuid,
        event: AgentEvent,
    ) -> AgentEventOutcome {
        match &event {
            AgentEvent::AgentUp { agent } | AgentEvent::AgentUpdated { agent }
                if agent.host_id != source_host_id =>
            {
                tracing::warn!(
                    source_host_id = %source_host_id,
                    event_host_id = %agent.host_id,
                    agent_id = %agent.id,
                    "ignoring remote agent event for a different host"
                );
                AgentEventOutcome::Ignored
            }
            AgentEvent::AgentDown { agent_id } => {
                let existing = self.state.read().await.agents_model.get(agent_id).cloned();
                if existing
                    .as_ref()
                    .is_some_and(|agent| agent.host_id != source_host_id)
                {
                    tracing::warn!(
                        source_host_id = %source_host_id,
                        event_agent_id = %agent_id,
                        existing_host_id = %existing.expect("checked above").host_id,
                        "ignoring remote AgentDown for an agent owned by a different host"
                    );
                    return AgentEventOutcome::Ignored;
                }
                self.apply_agent_event(event).await
            }
            _ => self.apply_agent_event(event).await,
        }
    }

    pub(crate) async fn attach_routing_events(
        &self,
        routing: Arc<RoutingCore>,
    ) -> tokio::task::JoinHandle<()> {
        let rx = routing.subscribe_hosts().await;
        self.spawn_host_event_task(rx)
    }

    pub(crate) async fn attach_local_agent_events(
        &self,
        ctx: AgentServiceCtx,
    ) -> Result<tokio::task::JoinHandle<()>, ProtocolError> {
        let rx = ctx.subscribe_agent_events().await?;
        Ok(self.spawn_agent_event_task(rx))
    }

    pub(crate) async fn resolve_agent(&self, agent: AgentRef) -> Result<Agent, ProtocolError> {
        let state = self.state.read().await;
        match agent {
            AgentRef::Id(agent_id) => state
                .agents_model
                .get(&agent_id)
                .cloned()
                .ok_or(ProtocolError::NoAgentFound),
            AgentRef::Name(name) => {
                let mut matches = state
                    .agents_model
                    .values()
                    .filter(|agent| agent.name.as_deref() == Some(name.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                matches.sort_unstable_by_key(|agent| agent.id);
                match matches.as_slice() {
                    [] => Err(ProtocolError::NoAgentFound),
                    [agent] => Ok(agent.clone()),
                    _ => Err(ProtocolError::AmbiguousAgentName {
                        name,
                        agent_ids: matches.iter().map(|agent| agent.id).collect(),
                    }),
                }
            }
        }
    }

    async fn add_host(&self, host: Host) -> HostEventOutcome {
        if is_cloud_relay_host(&host) {
            return HostEventOutcome::IgnoredRelayOrUnknown;
        }

        let host_id = host.id;
        let should_subscribe_remote = !self.is_local_host(host_id) && host_is_agent_capable(&host);
        let host_event_entry = self.host_entry_for_online_host(host.clone()).await;
        {
            let mut state = self.state.write().await;
            if let Some(existing) = state.remote_agent_subs.remove(&host_id) {
                existing.abort();
            }
            state.hosts_model.insert(host_id, host);
            state.host_events.emit(HostEvent::HostUpdated {
                host: host_event_entry,
            });
            if should_subscribe_remote {
                state.remote_agent_subs.insert(
                    host_id,
                    tokio::spawn(self.clone().run_remote_agent_subscription(host_id)),
                );
            }
        }
        HostEventOutcome::Added
    }

    async fn host_entries_for_online_hosts(
        &self,
        hosts: Vec<Host>,
        include_trust_only: bool,
    ) -> Vec<HostEntry> {
        let trusted_hosts = self.trusted_host_names();
        let trusted_names = trusted_hosts.iter().cloned().collect::<HashMap<_, _>>();
        let mut seen = HashSet::new();
        let mut entries = Vec::with_capacity(
            hosts.len()
                + if include_trust_only {
                    trusted_hosts.len()
                } else {
                    0
                },
        );
        for host in hosts {
            seen.insert(host.id);
            entries.push(
                self.host_entry_for_online_host_with_trust(host, &trusted_names)
                    .await,
            );
        }
        if include_trust_only {
            for (host_id, name) in trusted_hosts {
                if seen.contains(&host_id) || self.is_local_host(host_id) {
                    continue;
                }
                entries.push(self.trusted_host_entry(host_id, name).await);
            }
        }
        entries.sort_unstable_by_key(|host| host.id);
        entries
    }

    async fn host_entry_for_online_host(&self, host: Host) -> HostEntry {
        let trusted_names = self.trusted_host_names().into_iter().collect();
        self.host_entry_for_online_host_with_trust(host, &trusted_names)
            .await
    }

    async fn host_entry_for_online_host_with_trust(
        &self,
        host: Host,
        trusted_names: &HashMap<Uuid, String>,
    ) -> HostEntry {
        let host_id = host.id;
        let trusted = self.is_local_host(host_id) || trusted_names.contains_key(&host_id);
        let reachability_status = if self.is_local_host(host_id) || trusted {
            Some(HostReachabilityStatus::Reachable)
        } else {
            None
        };
        let name = host.name.clone();
        let version = host.version.clone();
        let capabilities = host.capabilities.clone();
        HostEntry {
            id: host_id,
            name,
            online: true,
            version: Some(version),
            capabilities: Some(capabilities),
            trust_status: if trusted {
                HostTrustStatus::Trusted
            } else {
                HostTrustStatus::UntrustedButOnline
            },
            reachability_status,
        }
    }

    async fn trusted_host_entry(&self, host_id: Uuid, name: String) -> HostEntry {
        let reachability_status = match self
            .remote_agent_connections
            .stored_reachability_error(host_id)
            .await
        {
            Some(last_error) => HostReachabilityStatus::Unreachable { last_error },
            None => HostReachabilityStatus::Unknown,
        };
        HostEntry {
            id: host_id,
            name,
            online: false,
            version: None,
            capabilities: None,
            trust_status: HostTrustStatus::Trusted,
            reachability_status: Some(reachability_status),
        }
    }

    fn trusted_host_names(&self) -> Vec<(Uuid, String)> {
        let Ok(store) = self.pairing_trust.trust_store.read() else {
            tracing::warn!("failed to read trust store for host listing status");
            return Vec::new();
        };
        store
            .entries()
            .map(|(host_id, entry)| (host_id, entry.name.clone()))
            .collect()
    }

    fn trusted_host_name(&self, host_id: Uuid) -> Option<String> {
        let Ok(store) = self.pairing_trust.trust_store.read() else {
            tracing::warn!("failed to read trust store for host listing status");
            return None;
        };
        store.entries().find_map(|(entry_host_id, entry)| {
            (entry_host_id == host_id).then(|| entry.name.clone())
        })
    }

    fn peer_entries(&self) -> Result<Vec<(Uuid, TrustEntry)>, tonic::Status> {
        let store = self
            .pairing_trust
            .trust_store
            .read()
            .map_err(|_| identity_status(crate::identity::IdentityError::TrustStorePoisoned))?;
        let mut entries = store
            .entries()
            .map(|(host_id, entry)| (host_id, entry.clone()))
            .collect::<Vec<_>>();
        entries.sort_unstable_by(|(left_id, left), (right_id, right)| {
            left.name
                .cmp(&right.name)
                .then_with(|| left_id.cmp(right_id))
        });
        Ok(entries)
    }

    fn peer_entry(&self, peer: wire::PeerRef) -> Result<(Uuid, TrustEntry), tonic::Status> {
        let store = self
            .pairing_trust
            .trust_store
            .read()
            .map_err(|_| identity_status(crate::identity::IdentityError::TrustStorePoisoned))?;
        resolve_peer_ref(&store, peer)
    }

    async fn unpair_peer(
        &self,
        peer: wire::PeerRef,
        reason: String,
    ) -> Result<(Uuid, TrustEntry), tonic::Status> {
        let reason = normalized_unpair_reason(reason);
        let trust_commit_lock = self.pairing_trust.trust_commit_lock.lock().await;
        let (host_id, removed_entry, staged) = {
            let store =
                self.pairing_trust.trust_store.read().map_err(|_| {
                    identity_status(crate::identity::IdentityError::TrustStorePoisoned)
                })?;
            let (host_id, _) = resolve_peer_ref(&store, peer)?;
            let mut staged = store.clone();
            let removed_entry = staged.remove(host_id).ok_or_else(|| {
                tonic::Status::not_found(format!("peer {host_id} is not trusted"))
            })?;
            staged
                .save_in(&self.pairing_trust.data_dir)
                .map_err(identity_status)?;
            (host_id, removed_entry, staged)
        };
        {
            let mut store =
                self.pairing_trust.trust_store.write().map_err(|_| {
                    identity_status(crate::identity::IdentityError::TrustStorePoisoned)
                })?;
            *store = staged;
        }
        drop(trust_commit_lock);

        self.remote_agent_connections
            .send_link_close_to_host(host_id, wire::pb::LinkCloseReason::UserRevoked)
            .await;
        self.remote_agent_connections.teardown_host(host_id).await;
        self.remove_peer_from_client_model(host_id).await;
        audit::trust_remove(
            host_id,
            &removed_entry.name,
            removed_entry.paired_at,
            Utc::now(),
            &reason,
        );
        Ok((host_id, removed_entry))
    }

    async fn remove_peer_from_client_model(&self, host_id: Uuid) {
        if !matches!(
            self.remove_host(host_id).await,
            HostEventOutcome::IgnoredRelayOrUnknown
        ) {
            return;
        }
        self.state
            .write()
            .await
            .host_events
            .emit(HostEvent::HostRemoved { id: host_id });
    }

    async fn publish_host_status_update(&self, host_id: Uuid) -> bool {
        let online_host = self.state.read().await.hosts_model.get(&host_id).cloned();
        let entry = match online_host {
            Some(host) => self.host_entry_for_online_host(host).await,
            None => {
                let Some(name) = self.trusted_host_name(host_id) else {
                    return false;
                };
                self.trusted_host_entry(host_id, name).await
            }
        };
        self.state
            .write()
            .await
            .host_events
            .emit(HostEvent::HostUpdated { host: entry });
        true
    }

    async fn remove_host(&self, host_id: Uuid) -> HostEventOutcome {
        let trusted_replacement = match self.trusted_host_name(host_id) {
            Some(name) => Some(self.trusted_host_entry(host_id, name).await),
            None => None,
        };
        let mut state = self.state.write().await;
        if state.hosts_model.remove(&host_id).is_none() {
            return HostEventOutcome::IgnoredRelayOrUnknown;
        }
        if let Some(remote_agent_sub) = state.remote_agent_subs.remove(&host_id) {
            remote_agent_sub.abort();
        }

        let mut removed_agent_ids = state
            .agents_model
            .values()
            .filter_map(|agent| (agent.host_id == host_id).then_some(agent.id))
            .collect::<Vec<_>>();
        removed_agent_ids.sort_unstable();
        for agent_id in &removed_agent_ids {
            state.agents_model.remove(agent_id);
            state.agent_events.emit(AgentEvent::AgentDown {
                agent_id: *agent_id,
            });
        }
        if let Some(host) = trusted_replacement {
            state.host_events.emit(HostEvent::HostUpdated { host });
        } else {
            state
                .host_events
                .emit(HostEvent::HostRemoved { id: host_id });
        }
        HostEventOutcome::Removed {
            removed_agents: removed_agent_ids.len(),
        }
    }

    async fn upsert_agent(&self, agent: Agent, kind: AgentChangeKind) -> AgentEventOutcome {
        let mut state = self.state.write().await;
        if state.agents_model.get(&agent.id) == Some(&agent) {
            return AgentEventOutcome::Ignored;
        }
        state.agents_model.insert(agent.id, agent.clone());
        match kind {
            AgentChangeKind::Up => state.agent_events.emit(AgentEvent::AgentUp { agent }),
            AgentChangeKind::Updated => state.agent_events.emit(AgentEvent::AgentUpdated { agent }),
        };
        AgentEventOutcome::Upserted
    }

    fn spawn_host_event_task(
        &self,
        mut rx: mpsc::Receiver<HostReachabilityEvent>,
    ) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                service.apply_host_event(event).await;
            }
            tracing::error!("ClientService host event subscription ended");
        })
    }

    fn spawn_agent_event_task(
        &self,
        mut rx: mpsc::Receiver<AgentEvent>,
    ) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                service.apply_agent_event(event).await;
            }
            tracing::error!("ClientService local agent event subscription ended");
        })
    }
}

#[tonic::async_trait]
impl wire::client_service_server::ClientService for ClientService {
    async fn list_hosts(
        &self,
        request: tonic::Request<wire::ListHostsRequest>,
    ) -> TonicResult<wire::ListHostsResponse> {
        let include_untrusted = can_see_untrusted_hosts(&request);
        let request = request.into_inner();
        let scope = wire::list_hosts_request::Scope::try_from(request.scope)
            .unwrap_or(wire::list_hosts_request::Scope::Unspecified);
        if scope == wire::list_hosts_request::Scope::PairingCandidates && !include_untrusted {
            return Err(tonic::Status::permission_denied(
                "pairing candidate inventory is only available to local clients",
            ));
        }
        let mut hosts = self.hosts_snapshot().await;
        if scope == wire::list_hosts_request::Scope::PairingCandidates {
            hosts.retain(|host| !self.is_local_host(host.id));
            let mut cloud_hosts = Vec::new();
            for host in hosts {
                if self.remote_agent_connections.has_cloud_route(host.id).await {
                    cloud_hosts.push(host);
                }
            }
            hosts = cloud_hosts;
        }
        let entries = self
            .host_entries_for_online_hosts(
                hosts,
                scope != wire::list_hosts_request::Scope::PairingCandidates,
            )
            .await;
        let entries = filter_host_entries_for_scope(entries, scope, include_untrusted);
        self.mark_client_visible_host_entries(&entries).await;
        Ok(tonic::Response::new(wire::ListHostsResponse {
            hosts: entries.iter().map(host_entry_to_wire).collect::<Vec<_>>(),
        }))
    }

    async fn list_agents(
        &self,
        _request: tonic::Request<wire::ListAgentsRequest>,
    ) -> TonicResult<wire::ListAgentsResponse> {
        let agents = self
            .list_agents()
            .await
            .into_iter()
            .map(|agent| agent_to_wire(&agent))
            .collect::<Result<Vec<_>, wire::EncodeError>>()
            .map_err(encode_status)?;
        Ok(tonic::Response::new(wire::ListAgentsResponse { agents }))
    }

    type SubscribeHostsStream = ResponseStream<wire::SubscribeHostsResponse>;

    async fn subscribe_hosts(
        &self,
        request: tonic::Request<wire::SubscribeHostsRequest>,
    ) -> TonicResult<Self::SubscribeHostsStream> {
        let include_untrusted = can_see_untrusted_hosts(&request);
        let (snapshot, rx) = self.subscribe_hosts_with_snapshot().await;
        let snapshot = filter_host_entries_for_scope(
            snapshot,
            wire::list_hosts_request::Scope::All,
            include_untrusted,
        );
        self.mark_client_visible_host_entries(&snapshot).await;
        let snapshot = stream::iter(host_snapshot_to_wire(snapshot).into_iter().map(Ok));
        let live =
            host_receiver_stream(rx, include_untrusted, self.remote_agent_connections.clone());
        Ok(tonic::Response::new(Box::pin(snapshot.chain(live))))
    }

    type SubscribeAgentsStream = ResponseStream<wire::SubscribeAgentsResponse>;

    async fn subscribe_agents(
        &self,
        _request: tonic::Request<wire::SubscribeAgentsRequest>,
    ) -> TonicResult<Self::SubscribeAgentsStream> {
        let (snapshot, rx) = self.subscribe_agents_with_snapshot().await;
        let snapshot = stream::iter(
            agent_snapshot_to_wire(snapshot)
                .map_err(encode_status)?
                .into_iter()
                .map(Ok),
        );
        let live = receiver_stream(rx, |event| {
            client_agent_event_to_wire(&event).map_err(encode_status)
        });
        Ok(tonic::Response::new(Box::pin(snapshot.chain(live))))
    }

    async fn create_agent(
        &self,
        request: tonic::Request<wire::ClientCreateAgentRequest>,
    ) -> TonicResult<wire::CreateAgentResponse> {
        let request = request.into_inner();
        if let Some(host_id) =
            optional_uuid_from_bytes("CreateAgentRequest.host_id", request.host_id.as_deref())?
            && !self.is_local_host(host_id)
        {
            self.ensure_remote_create_target(host_id).await?;
            return self
                .remote_create_agent(host_id, client_create_to_agent_create_request(request))
                .await;
        }

        let ctx = self.local_agent_service();
        ensure_local_create_target(&ctx, &request)?;

        let agent = ctx
            .create(client_create_to_create_rpc_request(request)?)
            .await
            .map_err(protocol_status)?;
        self.upsert_agent(agent.clone(), AgentChangeKind::Up).await;
        Ok(tonic::Response::new(wire::CreateAgentResponse {
            agent: Some(agent_to_wire(&agent).map_err(encode_status)?),
        }))
    }

    async fn rename_agent(
        &self,
        request: tonic::Request<wire::ClientRenameAgentRequest>,
    ) -> TonicResult<wire::RenameAgentResponse> {
        let request = request.into_inner();
        if request.name.is_empty() {
            return Err(tonic::Status::invalid_argument(
                "ClientRenameAgentRequest.name must not be empty",
            ));
        }
        let agent = self
            .resolve_agent(client_agent_ref(
                "ClientRenameAgentRequest.agent",
                request.agent,
            )?)
            .await
            .map_err(protocol_status)?;
        let agent_request = wire::RenameAgentRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            name: request.name,
        };
        if !self.is_local_host(agent.host_id) {
            return self.remote_rename_agent(agent.host_id, agent_request).await;
        }

        let ctx = self.local_agent_service();
        let request = crate::agents::RenameAgentRequest {
            agent_id: agent.id,
            name: agent_request.name,
        };

        let agent = ctx.rename(request).await.map_err(protocol_status)?;
        self.upsert_agent(agent.clone(), AgentChangeKind::Updated)
            .await;
        Ok(tonic::Response::new(wire::RenameAgentResponse {
            agent: Some(agent_to_wire(&agent).map_err(encode_status)?),
        }))
    }

    async fn delete_agent(
        &self,
        request: tonic::Request<wire::ClientDeleteAgentRequest>,
    ) -> TonicResult<wire::DeleteAgentResponse> {
        let caller = audit_caller(&request);
        let request = request.into_inner();
        let agent = self
            .resolve_agent(client_agent_ref(
                "ClientDeleteAgentRequest.agent",
                request.agent,
            )?)
            .await
            .map_err(protocol_status)?;
        audit::client_service_disruptive_call("ClientService.DeleteAgent", &caller, Some(agent.id));
        let agent_request = wire::DeleteAgentRequest {
            agent_id: agent.id.as_bytes().to_vec(),
        };
        if !self.is_local_host(agent.host_id) {
            return self
                .remote_delete_agent(agent.host_id, agent_request, agent.id)
                .await;
        }

        let ctx = self.local_agent_service();

        ctx.delete(agent.id).await.map_err(protocol_status)?;
        self.apply_agent_event(AgentEvent::AgentDown { agent_id: agent.id })
            .await;
        Ok(tonic::Response::new(wire::DeleteAgentResponse {}))
    }

    type SubscribeSessionStream = ResponseStream<wire::SubscribeSessionResponse>;

    async fn subscribe_session(
        &self,
        request: tonic::Request<wire::ClientSubscribeSessionRequest>,
    ) -> TonicResult<Self::SubscribeSessionStream> {
        let request = request.into_inner();
        let agent = self
            .resolve_agent(client_agent_ref(
                "ClientSubscribeSessionRequest.agent",
                request.agent,
            )?)
            .await
            .map_err(protocol_status)?;
        if !self.is_local_host(agent.host_id) {
            let agent_request = wire::pb::SubscribeSessionRequest {
                agent_id: agent.id.as_bytes().to_vec(),
                io_protocol: request.io_protocol,
                args: request.args,
            };
            return self
                .remote_subscribe_session(agent.host_id, agent_request)
                .await;
        }

        let ctx = self.local_agent_service();
        let decoded = SubscribeSessionRequest {
            agent_id: agent.id,
            io_protocol: request.io_protocol,
            args: request.args,
        };
        let stream = ctx
            .subscribe_session_response_stream(decoded)
            .await
            .map_err(protocol_status)?;
        Ok(tonic::Response::new(stream))
    }

    async fn send_input(
        &self,
        request: tonic::Request<wire::ClientSendInputRequest>,
    ) -> TonicResult<wire::SendInputResponse> {
        let request = request.into_inner();
        let agent = self
            .resolve_agent(client_agent_ref(
                "ClientSendInputRequest.agent",
                request.agent,
            )?)
            .await
            .map_err(protocol_status)?;
        if !self.is_local_host(agent.host_id) {
            let event = client_send_input_event_to_agent_event(request.event)?;
            let agent_request = wire::pb::SendInputRequest {
                agent_id: agent.id.as_bytes().to_vec(),
                io_protocol: request.io_protocol,
                event,
            };
            return self.remote_send_input(agent.host_id, agent_request).await;
        }

        let ctx = self.local_agent_service();
        ctx.send_input(SendInputRequest {
            agent_id: agent.id,
            io_protocol: request.io_protocol,
            event: client_send_input_event_to_session_event(request.event)?,
        })
        .await
        .map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::SendInputResponse {}))
    }

    async fn debug(
        &self,
        request: tonic::Request<wire::DebugRequest>,
    ) -> TonicResult<wire::DebugResponse> {
        let request = request.into_inner();
        let dump = self
            .debug_dump(debug_format_from_wire(request.format)?, request.verbose)
            .await;
        Ok(tonic::Response::new(wire::DebugResponse { dump }))
    }

    async fn shutdown(
        &self,
        request: tonic::Request<wire::ShutdownRequest>,
    ) -> TonicResult<wire::ShutdownResponse> {
        let caller = audit_caller(&request);
        audit::client_service_disruptive_call("ClientService.Shutdown", &caller, None);
        self.request_shutdown().await.map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::ShutdownResponse {}))
    }

    async fn suspend(
        &self,
        request: tonic::Request<wire::SuspendRequest>,
    ) -> TonicResult<wire::SuspendResponse> {
        let caller = audit_caller(&request);
        let reason = suspend_reason_from_wire(request.into_inner().reason)?;
        audit::client_service_disruptive_call("ClientService.Suspend", &caller, None);
        let suspended_count = self
            .request_suspend(reason)
            .await
            .map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::SuspendResponse {
            suspended_count,
        }))
    }

    async fn resume(
        &self,
        request: tonic::Request<wire::ResumeRequest>,
    ) -> TonicResult<wire::ResumeResponse> {
        let caller = audit_caller(&request);
        audit::client_service_disruptive_call("ClientService.Resume", &caller, None);
        let (resumed_count, failed_count) =
            self.resume_local_agents().await.map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::ResumeResponse {
            resumed_count,
            failed_count,
        }))
    }

    async fn start_pairing(
        &self,
        request: tonic::Request<wire::StartPairingRequest>,
    ) -> TonicResult<wire::StartPairingResponse> {
        require_local_admin_client(&request)?;
        let request = request.into_inner();
        let mode = wire::start_pairing_request::Mode::try_from(request.mode).map_err(|_| {
            tonic::Status::invalid_argument(format!(
                "invalid StartPairingRequest mode: {}",
                request.mode
            ))
        })?;
        let (name, tcp_port, cloud_url, cloud_enabled) = {
            let state = self.server_state.read().await;
            (
                state.config.host_name.clone(),
                state.config.tcp_port,
                state.config.cloud_url.clone(),
                crate::setup::cloud_enabled(&state.config),
            )
        };
        if name.len() > MAX_PAIRING_NAME_BYTES {
            return Err(tonic::Status::invalid_argument(
                "host_name is too long for pairing",
            ));
        }
        if request.require_lan_direct && tcp_port.is_none() {
            return Err(tonic::Status::failed_precondition(
                "set `tcp_port` in your config, or use cloud / SSH pairing",
            ));
        }
        if mode == wire::start_pairing_request::Mode::Qr && !cloud_enabled {
            return Err(tonic::Status::failed_precondition(
                "QR pairing requires cloud mode",
            ));
        }
        let method = pairing_mode_name(mode);
        let secret = start_pairing_secret(&self.pair_mode, mode).inspect_err(|error| {
            audit::pairing_failure(method, error);
        })?;
        audit::pairing_start(method);
        Ok(tonic::Response::new(wire::StartPairingResponse {
            identity: Some(wire::PairingIdentity {
                host_id: self.local_agents.host_id().as_bytes().to_vec(),
                pubkey: self.pairing_trust.local_pubkey.clone(),
                name,
            }),
            ttl_seconds: PAIR_MODE_TTL.as_secs(),
            tcp_port: tcp_port.map(u32::from),
            cloud_url,
            secret: Some(secret),
        }))
    }

    async fn get_pairing_status(
        &self,
        request: tonic::Request<wire::GetPairingStatusRequest>,
    ) -> TonicResult<wire::GetPairingStatusResponse> {
        require_local_admin_client(&request)?;
        Ok(tonic::Response::new(wire::GetPairingStatusResponse {
            active: self.pair_mode.is_active(),
        }))
    }

    async fn cancel_pairing(
        &self,
        request: tonic::Request<wire::CancelPairingRequest>,
    ) -> TonicResult<wire::CancelPairingResponse> {
        require_local_admin_client(&request)?;
        if self.pair_mode.cancel() {
            audit::pairing_cancel("admin");
        }
        Ok(tonic::Response::new(wire::CancelPairingResponse {}))
    }

    async fn pair_peer(
        &self,
        request: tonic::Request<wire::PairPeerRequest>,
    ) -> TonicResult<wire::PairPeerResponse> {
        require_local_admin_client(&request)?;
        let trust = &self.pairing_trust;
        let request = request.into_inner();
        let peer = request
            .peer
            .ok_or_else(|| tonic::Status::invalid_argument("PairPeerRequest.peer is required"))?;
        let (host_id, pubkey, name) = ssh_pairing_identity_from_wire(peer)?;
        if host_id == self.local_agents.host_id() || pubkey == trust.local_pubkey {
            return Err(tonic::Status::invalid_argument("SELF_PAIRING"));
        }
        let reachability = pair_peer_reachability_from_wire(request.reachability)?;
        let link_reachability = reachability.clone();
        let method = pair_peer_audit_method(&link_reachability);

        audit::pairing_start(method);
        commit_peer_trust(
            PeerTrustCommitContext::new(
                trust.trust_store.clone(),
                trust.trust_commit_lock.clone(),
                self.remote_agent_connections.clone(),
                trust.data_dir.clone(),
            ),
            PeerTrustUpdate::new(host_id, pubkey, name, reachability),
        )
        .await
        .inspect_err(|error| {
            audit::pairing_failure(method, error);
        })?;
        audit::pairing_success(method, host_id);
        self.publish_host_status_update(host_id).await;
        if let Some(reachability) = link_reachability {
            self.reachability_links
                .spawn_pair_time_link(host_id, reachability);
        }
        Ok(tonic::Response::new(wire::PairPeerResponse {}))
    }

    async fn pair_pin_cloud_peer(
        &self,
        request: tonic::Request<wire::PairPinCloudPeerRequest>,
    ) -> TonicResult<wire::PairPinCloudPeerResponse> {
        require_local_admin_client(&request)?;
        let trust = &self.pairing_trust;
        let request = request.into_inner();
        let peer_host_id = uuid_from_bytes("PairPinCloudPeerRequest.host_id", &request.host_id)?;
        if peer_host_id == self.local_agents.host_id() {
            return Err(tonic::Status::invalid_argument("SELF_PAIRING"));
        }
        audit::pairing_start("cloud_pin");
        let local_name = {
            let state = self.server_state.read().await;
            state.host_name().to_string()
        };
        let local_identity =
            LocalPairingIdentity::new(self.local_agents.host_id(), trust.local_pubkey.clone());
        let channel = self
            .remote_agent_connections
            .cloud_pin_pairing_channel_to(peer_host_id)
            .await
            .map_err(|error| {
                audit::pairing_failure("cloud_pin", &error);
                tonic::Status::unavailable(format!(
                    "cloud pairing target {peer_host_id} is not reachable: {error}"
                ))
            })?;
        let mut pairing_client = wire::pairing_service_client::PairingServiceClient::new(channel);
        let peer = pair_by_spake2_initiator(
            &mut pairing_client,
            &local_identity,
            &local_name,
            &request.pin,
        )
        .await
        .inspect_err(|error| {
            audit::pairing_failure("cloud_pin", error);
        })?;
        if peer.host_id != peer_host_id {
            audit::pairing_failure("cloud_pin", "paired identity did not match requested host");
            return Err(tonic::Status::invalid_argument(
                "PROTOCOL_VIOLATION: paired identity did not match requested host",
            ));
        }
        if peer.pubkey == trust.local_pubkey {
            audit::pairing_failure("cloud_pin", "SELF_PAIRING");
            return Err(tonic::Status::invalid_argument("SELF_PAIRING"));
        }

        commit_peer_trust(
            PeerTrustCommitContext::new(
                trust.trust_store.clone(),
                trust.trust_commit_lock.clone(),
                self.remote_agent_connections.clone(),
                trust.data_dir.clone(),
            ),
            PeerTrustUpdate::new(
                peer.host_id,
                peer.pubkey.clone(),
                peer.name.clone(),
                Some(Reachability::Cloud),
            ),
        )
        .await
        .inspect_err(|error| {
            audit::pairing_failure("cloud_pin", error);
        })?;
        audit::pairing_success("cloud_pin", peer.host_id);
        self.publish_host_status_update(peer.host_id).await;
        Ok(tonic::Response::new(wire::PairPinCloudPeerResponse {
            peer: Some(wire::PairingIdentity {
                host_id: peer.host_id.as_bytes().to_vec(),
                pubkey: peer.pubkey,
                name: peer.name,
            }),
        }))
    }

    async fn pair_qr_cloud_peer(
        &self,
        request: tonic::Request<wire::PairQrCloudPeerRequest>,
    ) -> TonicResult<wire::PairQrCloudPeerResponse> {
        require_local_admin_client(&request)?;
        let trust = &self.pairing_trust;
        let request = request.into_inner();
        let peer_host_id = uuid_from_bytes("PairQrCloudPeerRequest.host_id", &request.host_id)?;
        validate_pairing_pubkey("PairQrCloudPeerRequest.pubkey", &request.pubkey)?;
        validate_pairing_token(
            "PairQrCloudPeerRequest.one_shot_token",
            &request.one_shot_token,
        )?;
        preflight_pairing_peer(
            trust,
            self.local_agents.host_id(),
            peer_host_id,
            &request.pubkey,
        )?;
        audit::pairing_start("cloud_qr");
        let local_name = {
            let state = self.server_state.read().await;
            state.host_name().to_string()
        };
        let local_identity =
            LocalPairingIdentity::new(self.local_agents.host_id(), trust.local_pubkey.clone());
        let channel = self
            .remote_agent_connections
            .cloud_qr_pairing_channel_to(peer_host_id, request.pubkey.clone())
            .await
            .map_err(|error| {
                audit::pairing_failure("cloud_qr", &error);
                tonic::Status::unavailable(format!(
                    "cloud QR pairing target {peer_host_id} is not reachable: {error}"
                ))
            })?;
        let mut pairing_client = wire::pairing_service_client::PairingServiceClient::new(channel);
        let peer = pair_by_token_initiator(
            &mut pairing_client,
            &local_identity,
            &local_name,
            peer_host_id,
            request.pubkey,
            request.one_shot_token,
        )
        .await
        .inspect_err(|error| {
            audit::pairing_failure("cloud_qr", error);
        })?;

        commit_peer_trust(
            PeerTrustCommitContext::new(
                trust.trust_store.clone(),
                trust.trust_commit_lock.clone(),
                self.remote_agent_connections.clone(),
                trust.data_dir.clone(),
            ),
            PeerTrustUpdate::new(
                peer.host_id,
                peer.pubkey.clone(),
                peer.name.clone(),
                Some(Reachability::Cloud),
            ),
        )
        .await
        .inspect_err(|error| {
            audit::pairing_failure("cloud_qr", error);
        })?;
        audit::pairing_success("cloud_qr", peer.host_id);
        self.publish_host_status_update(peer.host_id).await;
        Ok(tonic::Response::new(wire::PairQrCloudPeerResponse {
            peer: Some(wire::PairingIdentity {
                host_id: peer.host_id.as_bytes().to_vec(),
                pubkey: peer.pubkey,
                name: peer.name,
            }),
        }))
    }

    async fn list_peers(
        &self,
        request: tonic::Request<wire::ListPeersRequest>,
    ) -> TonicResult<wire::ListPeersResponse> {
        require_local_admin_client(&request)?;
        let peers = self
            .peer_entries()?
            .into_iter()
            .map(|(host_id, entry)| peer_entry_to_wire(host_id, &entry))
            .collect();
        Ok(tonic::Response::new(wire::ListPeersResponse { peers }))
    }

    async fn get_peer(
        &self,
        request: tonic::Request<wire::GetPeerRequest>,
    ) -> TonicResult<wire::GetPeerResponse> {
        require_local_admin_client(&request)?;
        let request = request.into_inner();
        let peer = request
            .peer
            .ok_or_else(|| tonic::Status::invalid_argument("GetPeerRequest.peer is required"))?;
        let (host_id, entry) = self.peer_entry(peer)?;
        Ok(tonic::Response::new(wire::GetPeerResponse {
            peer: Some(peer_entry_to_wire(host_id, &entry)),
        }))
    }

    async fn unpair(
        &self,
        request: tonic::Request<wire::UnpairRequest>,
    ) -> TonicResult<wire::UnpairResponse> {
        require_local_admin_client(&request)?;
        let caller = audit_caller(&request);
        audit::client_service_disruptive_call("ClientService.Unpair", &caller, None);
        let request = request.into_inner();
        let peer = request
            .peer
            .ok_or_else(|| tonic::Status::invalid_argument("UnpairRequest.peer is required"))?;
        let (host_id, entry) = self.unpair_peer(peer, request.reason).await?;
        Ok(tonic::Response::new(wire::UnpairResponse {
            removed_peer: Some(peer_entry_to_wire(host_id, &entry)),
        }))
    }

    async fn handle_hook(
        &self,
        request: tonic::Request<wire::HandleHookRequest>,
    ) -> TonicResult<wire::HandleHookResponse> {
        let request = request.into_inner();
        let agent_id = uuid_from_bytes("HandleHookRequest.agent_id", &request.agent_id)?;
        self.handle_local_hook(agent_id, request.payload, request.external)
            .await
            .map_err(protocol_status)?;
        Ok(tonic::Response::new(wire::HandleHookResponse {}))
    }
}

pub(crate) fn host_snapshot_to_wire(hosts: Vec<HostEntry>) -> Vec<wire::SubscribeHostsResponse> {
    hosts
        .into_iter()
        .map(|host| client_host_event_to_wire(&HostEvent::HostUpdated { host }))
        .chain(std::iter::once(subscribe_hosts_snapshot_complete()))
        .collect()
}

pub(crate) fn agent_snapshot_to_wire(
    agents: Vec<Agent>,
) -> Result<Vec<wire::SubscribeAgentsResponse>, wire::EncodeError> {
    agents
        .into_iter()
        .map(|agent| client_agent_event_to_wire(&AgentEvent::AgentUp { agent }))
        .chain(std::iter::once(Ok(subscribe_agents_snapshot_complete())))
        .collect()
}

pub(crate) fn client_host_event_to_wire(event: &HostEvent) -> wire::SubscribeHostsResponse {
    let event = match event {
        HostEvent::HostUpdated { host } => {
            wire::subscribe_hosts_response::Event::HostUpdated(wire::HostUpdated {
                host: Some(host_entry_to_wire(host)),
            })
        }
        HostEvent::HostRemoved { id } => {
            wire::subscribe_hosts_response::Event::HostRemoved(wire::HostRemoved {
                host_id: uuid_to_bytes(*id),
            })
        }
        HostEvent::SnapshotComplete => {
            wire::subscribe_hosts_response::Event::SnapshotComplete(wire::SnapshotComplete {})
        }
    };
    wire::SubscribeHostsResponse { event: Some(event) }
}

fn host_entry_to_wire(host: &HostEntry) -> wire::HostEntry {
    wire::HostEntry {
        host_id: uuid_to_bytes(host.id),
        name: host.name.clone(),
        online: host.online,
        version: host.version.clone(),
        capabilities: host.capabilities.as_ref().map(capabilities_to_wire),
        trust_status: match host.trust_status {
            HostTrustStatus::Trusted => wire::HostTrustStatus::Trusted as i32,
            HostTrustStatus::UntrustedButOnline => wire::HostTrustStatus::UntrustedButOnline as i32,
        },
        reachability_status: host
            .reachability_status
            .as_ref()
            .map(host_reachability_status_to_wire),
    }
}

fn peer_entry_to_wire(host_id: Uuid, entry: &TrustEntry) -> wire::PeerEntry {
    wire::PeerEntry {
        host_id: uuid_to_bytes(host_id),
        name: entry.name.clone(),
        pubkey: entry.pubkey.clone(),
        paired_at_unix_ms: entry.paired_at.timestamp_millis(),
        reachabilities: entry
            .reachabilities
            .iter()
            .map(peer_reachability_to_wire)
            .collect(),
    }
}

fn peer_reachability_to_wire(reachability: &Reachability) -> wire::PeerReachability {
    let target = match reachability {
        Reachability::Cloud => wire::peer_reachability::Kind::Cloud(wire::Empty {}),
        Reachability::Ssh { target } => wire::peer_reachability::Kind::SshTarget(target.clone()),
        Reachability::DirectTcp { addr } => {
            wire::peer_reachability::Kind::DirectTcpAddr(addr.to_string())
        }
    };
    wire::PeerReachability { kind: Some(target) }
}

fn host_reachability_status_to_wire(
    status: &HostReachabilityStatus,
) -> wire::HostReachabilityStatus {
    match status {
        HostReachabilityStatus::Reachable => wire::HostReachabilityStatus {
            status: Some(wire::host_reachability_status::Status::Reachable(
                wire::HostReachabilityReachable {},
            )),
        },
        HostReachabilityStatus::Unreachable { last_error } => wire::HostReachabilityStatus {
            status: Some(wire::host_reachability_status::Status::Unreachable(
                wire::HostReachabilityUnreachable {
                    last_error: last_error.clone(),
                },
            )),
        },
        HostReachabilityStatus::Unknown => wire::HostReachabilityStatus {
            status: Some(wire::host_reachability_status::Status::Unknown(
                wire::HostReachabilityUnknown {},
            )),
        },
    }
}

pub(crate) fn client_agent_event_to_wire(
    event: &AgentEvent,
) -> Result<wire::SubscribeAgentsResponse, wire::EncodeError> {
    let event = match event {
        AgentEvent::AgentUp { agent } => {
            wire::subscribe_agents_response::Event::AgentUp(wire::AgentUp {
                agent: Some(agent_to_wire(agent)?),
            })
        }
        AgentEvent::AgentUpdated { agent } => {
            wire::subscribe_agents_response::Event::AgentUpdated(wire::AgentUpdated {
                agent: Some(agent_to_wire(agent)?),
            })
        }
        AgentEvent::AgentDown { agent_id } => {
            wire::subscribe_agents_response::Event::AgentDown(wire::AgentDown {
                agent_id: uuid_to_bytes(*agent_id),
                reason: None,
            })
        }
        AgentEvent::SnapshotComplete => {
            wire::subscribe_agents_response::Event::SnapshotComplete(wire::SnapshotComplete {})
        }
    };
    Ok(wire::SubscribeAgentsResponse { event: Some(event) })
}

fn subscribe_hosts_snapshot_complete() -> wire::SubscribeHostsResponse {
    wire::SubscribeHostsResponse {
        event: Some(wire::subscribe_hosts_response::Event::SnapshotComplete(
            wire::SnapshotComplete {},
        )),
    }
}

fn subscribe_agents_snapshot_complete() -> wire::SubscribeAgentsResponse {
    wire::SubscribeAgentsResponse {
        event: Some(wire::subscribe_agents_response::Event::SnapshotComplete(
            wire::SnapshotComplete {},
        )),
    }
}

fn agent_to_wire(agent: &Agent) -> Result<wire::Agent, wire::EncodeError> {
    crate::agents::agent_to_wire(agent)
}

fn uuid_to_bytes(uuid: Uuid) -> Vec<u8> {
    uuid.as_bytes().to_vec()
}

fn resolve_peer_ref(
    store: &TrustStore,
    peer: wire::PeerRef,
) -> Result<(Uuid, TrustEntry), tonic::Status> {
    let Some(identifier) = peer.identifier else {
        return Err(tonic::Status::invalid_argument(
            "PeerRef.identifier is required",
        ));
    };
    match identifier {
        wire::peer_ref::Identifier::HostId(bytes) => {
            let host_id = uuid_from_bytes("PeerRef.host_id", &bytes)?;
            let entry = store
                .entries()
                .find_map(|(entry_host_id, entry)| {
                    (entry_host_id == host_id).then(|| entry.clone())
                })
                .ok_or_else(|| {
                    tonic::Status::not_found(format!("peer {host_id} is not trusted"))
                })?;
            Ok((host_id, entry))
        }
        wire::peer_ref::Identifier::Name(name) => {
            if name.is_empty() {
                return Err(tonic::Status::invalid_argument(
                    "PeerRef.name must not be empty",
                ));
            }
            let matches = store
                .entries()
                .filter(|(_, entry)| entry.name == name)
                .map(|(host_id, entry)| (host_id, entry.clone()))
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [(host_id, entry)] => Ok((*host_id, entry.clone())),
                [] => Err(tonic::Status::not_found(format!(
                    "peer named {name} is not trusted"
                ))),
                _ => Err(tonic::Status::invalid_argument(format!(
                    "peer name {name} is ambiguous; use host_id"
                ))),
            }
        }
    }
}

fn normalized_unpair_reason(reason: String) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        "user".to_string()
    } else {
        reason.to_string()
    }
}

fn identity_status(error: IdentityError) -> tonic::Status {
    tonic::Status::internal(error.to_string())
}

fn ssh_pairing_identity_from_wire(
    identity: wire::PairingIdentity,
) -> Result<(HostId, Vec<u8>, String), tonic::Status> {
    if identity.host_id.len() != HOST_ID_LEN {
        return Err(tonic::Status::invalid_argument(
            "PairingIdentity.host_id must be 16 bytes",
        ));
    }
    if identity.pubkey.len() != PUBKEY_LEN {
        return Err(tonic::Status::invalid_argument(
            "PairingIdentity.pubkey must be 32 bytes",
        ));
    }
    if identity.name.len() > MAX_PAIRING_NAME_BYTES {
        return Err(tonic::Status::invalid_argument(
            "PairingIdentity.name is too long",
        ));
    }

    let mut host_id = [0_u8; HOST_ID_LEN];
    host_id.copy_from_slice(&identity.host_id);
    Ok((HostId::from_bytes(host_id), identity.pubkey, identity.name))
}

fn pair_peer_reachability_from_wire(
    reachability: Option<wire::pair_peer_request::Reachability>,
) -> Result<Option<Reachability>, tonic::Status> {
    match reachability {
        Some(wire::pair_peer_request::Reachability::SshTarget(target)) => {
            validate_ssh_target(&target)?;
            Ok(Some(Reachability::Ssh { target }))
        }
        Some(wire::pair_peer_request::Reachability::DirectTcpAddr(addr)) => {
            let addr = addr.parse::<SocketAddr>().map_err(|error| {
                tonic::Status::invalid_argument(format!(
                    "PairPeerRequest.direct_tcp_addr is invalid: {error}"
                ))
            })?;
            Ok(Some(Reachability::DirectTcp { addr }))
        }
        None => Ok(None),
    }
}

fn pair_peer_audit_method(reachability: &Option<Reachability>) -> &'static str {
    match reachability {
        Some(Reachability::Ssh { .. }) => "ssh",
        Some(Reachability::DirectTcp { .. }) => "direct_pin",
        Some(Reachability::Cloud) => "cloud",
        None => "manual",
    }
}

fn validate_ssh_target(target: &str) -> Result<(), tonic::Status> {
    if target.trim().is_empty() {
        return Err(tonic::Status::invalid_argument(
            "PairPeerRequest.ssh_target must not be empty",
        ));
    }
    if target.starts_with('-') {
        return Err(tonic::Status::invalid_argument(
            "PairPeerRequest.ssh_target must not begin with '-'",
        ));
    }
    Ok(())
}

fn require_local_admin_client<T>(request: &tonic::Request<T>) -> Result<(), tonic::Status> {
    match request.extensions().get::<BoxedGrpcConnectInfo>() {
        Some(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        }) => Ok(()),
        Some(_) => Err(tonic::Status::permission_denied(
            "local admin RPC is only available to local clients",
        )),
        None => Err(tonic::Status::failed_precondition(
            "local admin RPC requires local connection metadata",
        )),
    }
}

fn audit_caller<T>(request: &tonic::Request<T>) -> String {
    match request.extensions().get::<BoxedGrpcConnectInfo>() {
        Some(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        }) => "local".to_string(),
        Some(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::TlsTrusted { peer },
        }) => format!("tls_trusted:{peer}"),
        Some(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::PreTrustPairing { .. },
        }) => "pre_trust_pairing".to_string(),
        None => "unknown".to_string(),
    }
}

fn can_see_untrusted_hosts<T>(request: &tonic::Request<T>) -> bool {
    request
        .extensions()
        .get::<BoxedGrpcConnectInfo>()
        .map(|info| matches!(info.auth, BoxedGrpcAuth::LocalTrusted))
        .unwrap_or(false)
}

fn filter_host_entries_for_scope(
    mut hosts: Vec<HostEntry>,
    scope: wire::list_hosts_request::Scope,
    include_untrusted: bool,
) -> Vec<HostEntry> {
    if !include_untrusted {
        hosts.retain(|host| host.trust_status == HostTrustStatus::Trusted);
    }
    if scope == wire::list_hosts_request::Scope::PairingCandidates {
        hosts.retain(|host| {
            host.online
                && host.trust_status == HostTrustStatus::UntrustedButOnline
                && host.reachability_status.is_none()
        });
    }
    hosts
}

fn receiver_stream<E, T>(
    rx: mpsc::Receiver<E>,
    map: impl Fn(E) -> Result<T, tonic::Status> + Send + 'static,
) -> ResponseStream<T>
where
    E: Send + 'static,
    T: Send + 'static,
{
    Box::pin(stream::unfold(
        (rx, map, false),
        |(mut rx, map, done)| async move {
            if done {
                return None;
            }
            let (response, done) = match rx.recv().await {
                Some(event) => (map(event), false),
                None => (
                    Err(tonic::Status::resource_exhausted(
                        "event subscriber queue closed",
                    )),
                    true,
                ),
            };
            Some((response, (rx, map, done)))
        },
    ))
}

fn host_receiver_stream(
    rx: mpsc::Receiver<HostEvent>,
    include_untrusted: bool,
    remote_agent_connections: Arc<ConnectionManager>,
) -> ResponseStream<wire::SubscribeHostsResponse> {
    Box::pin(stream::unfold(
        (rx, include_untrusted, remote_agent_connections, false),
        |(mut rx, include_untrusted, remote_agent_connections, done)| async move {
            if done {
                return None;
            }
            loop {
                let event = match rx.recv().await {
                    Some(event) => event,
                    None => {
                        return Some((
                            Err(tonic::Status::resource_exhausted(
                                "event subscriber queue closed",
                            )),
                            (rx, include_untrusted, remote_agent_connections, true),
                        ));
                    }
                };
                if host_event_is_visible_to_subscriber(&event, include_untrusted) {
                    if let HostEvent::HostUpdated { host } = &event
                        && host.online
                    {
                        remote_agent_connections
                            .mark_client_visible_hosts(&[host.id])
                            .await;
                    }
                    return Some((
                        Ok(client_host_event_to_wire(&event)),
                        (rx, include_untrusted, remote_agent_connections, false),
                    ));
                }
            }
        },
    ))
}

fn host_event_is_visible_to_subscriber(event: &HostEvent, include_untrusted: bool) -> bool {
    if include_untrusted {
        return true;
    }
    match event {
        HostEvent::HostUpdated { host } => host.trust_status == HostTrustStatus::Trusted,
        HostEvent::HostRemoved { .. } => false,
        HostEvent::SnapshotComplete => true,
    }
}

fn remote_session_response_stream<S>(upstream: S) -> ResponseStream<wire::SubscribeSessionResponse>
where
    S: Stream<Item = Result<wire::SubscribeSessionResponse, tonic::Status>>
        + Send
        + Unpin
        + 'static,
{
    Box::pin(stream::unfold(
        (upstream, false),
        |(mut upstream, done)| async move {
            if done {
                return None;
            }
            match upstream.next().await {
                Some(Ok(response)) => Some((Ok(response), (upstream, false))),
                Some(Err(status))
                    if status.code() == tonic::Code::Unavailable
                        && !has_shutdown_reason_metadata(&status) =>
                {
                    Some((Ok(host_unreachable_session_closed()), (upstream, true)))
                }
                Some(Err(status)) => Some((Err(status), (upstream, true))),
                None => None,
            }
        },
    ))
}

fn host_unreachable_session_response_stream() -> ResponseStream<wire::SubscribeSessionResponse> {
    Box::pin(stream::once(async {
        Ok(host_unreachable_session_closed())
    }))
}

fn host_unreachable_session_closed() -> wire::SubscribeSessionResponse {
    crate::agents::session_output_event_to_wire(&SubscribeSessionEvent::Closed {
        reason: crate::agents::SessionCloseReason::HostUnreachable,
    })
}

fn has_shutdown_reason_metadata(status: &tonic::Status) -> bool {
    status
        .metadata()
        .get(SHUTDOWN_REASON_METADATA_KEY)
        .and_then(|value| value.to_str().ok())
        .and_then(ShutdownReason::from_wire_value)
        .is_some()
}

impl ClientService {
    fn is_local_host(&self, host_id: Uuid) -> bool {
        self.local_agents.host_id() == host_id
    }

    fn local_agent_service(&self) -> AgentServiceCtx {
        self.local_agents.clone()
    }

    async fn ensure_remote_create_target(&self, host_id: Uuid) -> Result<(), tonic::Status> {
        if self
            .state
            .read()
            .await
            .hosts_model
            .get(&host_id)
            .is_some_and(host_is_agent_capable)
        {
            Ok(())
        } else {
            Err(protocol_status(ProtocolError::Unreachable {
                message: format!(
                    "CreateAgent target host {host_id} is not reachable as an agent-capable host"
                ),
            }))
        }
    }

    async fn debug_dump(&self, format: DebugFormat, verbose: bool) -> String {
        crate::debug::dump_server_debug_info(&self.server_state, format, verbose).await
    }

    async fn request_shutdown(&self) -> Result<(), ProtocolError> {
        let shutdown_tx = { self.server_state.read().await.shutdown_tx() };
        let (reply, rx) = oneshot::channel();
        shutdown_tx
            .send(ShutdownRequest::Shutdown { reply })
            .await
            .map_err(|_| ProtocolError::ServerError {
                message: "shutdown channel is closed".to_string(),
            })?;
        rx.await.map_err(|_| ProtocolError::ServerError {
            message: "shutdown response channel is closed".to_string(),
        })?
    }

    async fn request_suspend(&self, reason: ShutdownReason) -> Result<u64, ProtocolError> {
        let shutdown_tx = { self.server_state.read().await.shutdown_tx() };
        let (reply, rx) = oneshot::channel();
        shutdown_tx
            .send(ShutdownRequest::Suspend { reason, reply })
            .await
            .map_err(|_| ProtocolError::ServerError {
                message: "shutdown channel is closed".to_string(),
            })?;
        rx.await.map_err(|_| ProtocolError::ServerError {
            message: "suspend response channel is closed".to_string(),
        })?
    }

    async fn resume_local_agents(&self) -> Result<(u64, u64), ProtocolError> {
        let (state_path, host_id, is_cloud_server) = {
            let state = self.server_state.read().await;
            (state.state_path(), state.host_id(), state.is_cloud_server())
        };
        if is_cloud_server {
            return Err(ProtocolError::FailedPrecondition {
                message: "cloud relays do not host local agents".to_string(),
            });
        }

        let suspended = crate::suspend::load_suspended(&state_path).map_err(|error| {
            ProtocolError::ServerError {
                message: format!("failed to load state: {error}"),
            }
        })?;
        let result = resume_agents(
            self.local_agents.state(),
            self.local_agents.event_tx(),
            suspended.agents,
            host_id,
        )
        .await;
        if result.failed_agents.is_empty() {
            crate::suspend::remove_suspended(&state_path).map_err(|error| {
                ProtocolError::ServerError {
                    message: format!("failed to remove state: {error}"),
                }
            })?;
        } else {
            crate::suspend::save_suspended(
                &state_path,
                &crate::suspend::SuspendedServerState {
                    agents: result.failed_agents,
                },
            )
            .map_err(|error| ProtocolError::ServerError {
                message: format!("failed to save remaining state: {error}"),
            })?;
        }
        Ok((result.resumed_count as u64, result.failed_count as u64))
    }

    async fn handle_local_hook(
        &self,
        agent_id: Uuid,
        payload: Vec<u8>,
        external: bool,
    ) -> Result<(), ProtocolError> {
        tracing::debug!(%agent_id, external, "received Claude hook event");

        let mut session_to_stop = None;
        let result = {
            let mut state = self.local_agents.state().write().await;
            if let Some(session) = state.agent_session_mut(&agent_id) {
                match session.handle_hook(&payload).await {
                    Ok(HookOutcome::Noop | HookOutcome::KeepSession) => Ok(()),
                    Ok(HookOutcome::WithdrawSession) => {
                        session_to_stop = crate::services::withdraw_agent(&mut state, agent_id);
                        Ok(())
                    }
                    Err(error) => Err(error.into_protocol_error()),
                }
            } else if !external {
                tracing::warn!(%agent_id, "hook target not found");
                Err(ProtocolError::NoAgentFound)
            } else {
                match AgentSession::bootstrap_external_hook(agent_id, &payload).await {
                    Ok(ExternalHookBootstrap::Noop) => Ok(()),
                    Ok(ExternalHookBootstrap::Register(session)) => {
                        match state.insert_registered_local_agent(
                            self.local_agents.host_id(),
                            agent_id,
                            session,
                        ) {
                            Ok(announce) => {
                                if let Some(session) = state.agent_session_mut(&agent_id) {
                                    session.maybe_start_name_sniffer(self.local_agents.event_tx());
                                }
                                state.local_agent_events.emit(announce);
                                tracing::info!(%agent_id, "created readonly session from external hook");
                                Ok(())
                            }
                            Err(e) => Err(ProtocolError::ServerError {
                                message: format!(
                                    "failed to register readonly agent {agent_id}: {e}"
                                ),
                            }),
                        }
                    }
                    Err(error) => Err(error.into_protocol_error()),
                }
            }
        };

        if let Some(session) = session_to_stop {
            session.stop(StopPolicy::Interrupt).await;
        }

        result
    }

    async fn remote_agent_client(
        &self,
        method: &'static str,
        host_id: Uuid,
    ) -> Result<wire::agent_service_client::AgentServiceClient<Channel>, tonic::Status> {
        let channel = match self.remote_agent_connections.channel_to(host_id).await {
            Ok(channel) => {
                self.remote_agent_connections
                    .clear_reachability_error(host_id)
                    .await;
                channel
            }
            Err(error) => {
                let status = remote_tunnel_status(method, host_id, error);
                self.remote_agent_connections
                    .record_reachability_error(host_id, status.message().to_string())
                    .await;
                return Err(status);
            }
        };
        Ok(wire::agent_service_client::AgentServiceClient::new(channel))
    }

    async fn remote_create_agent(
        &self,
        host_id: Uuid,
        request: wire::CreateAgentRequest,
    ) -> TonicResult<wire::CreateAgentResponse> {
        let expected_agent_id = uuid_from_bytes("CreateAgentRequest.agent_id", &request.agent_id)?;
        let expected_name = request.name.clone();
        let mut client = self
            .remote_agent_client("ClientService.CreateAgent", host_id)
            .await?;
        let response = client.create_agent(request).await?.into_inner();
        let agent =
            agent_from_remote_response(response.agent.clone(), "CreateAgentResponse.agent")?;
        validate_remote_agent_response(
            &agent,
            host_id,
            expected_agent_id,
            expected_name.as_deref(),
            "CreateAgentResponse.agent",
        )?;
        self.upsert_agent(agent, AgentChangeKind::Up).await;
        Ok(tonic::Response::new(response))
    }

    async fn remote_rename_agent(
        &self,
        host_id: Uuid,
        request: wire::RenameAgentRequest,
    ) -> TonicResult<wire::RenameAgentResponse> {
        let expected_agent_id = uuid_from_bytes("RenameAgentRequest.agent_id", &request.agent_id)?;
        let expected_name = request.name.clone();
        let mut client = self
            .remote_agent_client("ClientService.RenameAgent", host_id)
            .await?;
        let response = client.rename_agent(request).await?.into_inner();
        let agent =
            agent_from_remote_response(response.agent.clone(), "RenameAgentResponse.agent")?;
        validate_remote_agent_response(
            &agent,
            host_id,
            expected_agent_id,
            Some(expected_name.as_str()),
            "RenameAgentResponse.agent",
        )?;
        self.upsert_agent(agent, AgentChangeKind::Updated).await;
        Ok(tonic::Response::new(response))
    }

    async fn remote_delete_agent(
        &self,
        host_id: Uuid,
        request: wire::DeleteAgentRequest,
        agent_id: Uuid,
    ) -> TonicResult<wire::DeleteAgentResponse> {
        let mut client = self
            .remote_agent_client("ClientService.DeleteAgent", host_id)
            .await?;
        let response = client.delete_agent(request).await?.into_inner();
        self.apply_agent_event(AgentEvent::AgentDown { agent_id })
            .await;
        Ok(tonic::Response::new(response))
    }

    async fn remote_subscribe_session(
        &self,
        host_id: Uuid,
        request: wire::pb::SubscribeSessionRequest,
    ) -> TonicResult<ResponseStream<wire::SubscribeSessionResponse>> {
        let mut client = match self
            .remote_agent_client("ClientService.SubscribeSession", host_id)
            .await
        {
            Ok(client) => client,
            Err(status)
                if status.code() == tonic::Code::Unavailable
                    && !has_shutdown_reason_metadata(&status) =>
            {
                return Ok(tonic::Response::new(
                    host_unreachable_session_response_stream(),
                ));
            }
            Err(status) => return Err(status),
        };
        let stream = match client.subscribe_session(request).await {
            Ok(response) => response.into_inner(),
            Err(status)
                if status.code() == tonic::Code::Unavailable
                    && !has_shutdown_reason_metadata(&status) =>
            {
                return Ok(tonic::Response::new(
                    host_unreachable_session_response_stream(),
                ));
            }
            Err(status) => return Err(status),
        };
        Ok(tonic::Response::new(remote_session_response_stream(stream)))
    }

    async fn remote_send_input(
        &self,
        host_id: Uuid,
        request: wire::pb::SendInputRequest,
    ) -> TonicResult<wire::SendInputResponse> {
        let mut client = self
            .remote_agent_client("ClientService.SendInput", host_id)
            .await?;
        let response = client.send_input(request).await?.into_inner();
        Ok(tonic::Response::new(response))
    }

    async fn run_remote_agent_subscription(self, host_id: Uuid) {
        loop {
            if !self.has_host(host_id).await {
                break;
            }
            if let Err(error) = self.run_remote_agent_subscription_once(host_id).await {
                tracing::warn!(
                    host_id = %host_id,
                    error = %error,
                    "remote AgentService.SubscribeAgentEvents ended; keeping cached agents and retrying while host remains reachable"
                );
            }
            tokio::time::sleep(REMOTE_AGENT_SUBSCRIPTION_RETRY_DELAY).await;
        }
    }

    async fn run_remote_agent_subscription_once(&self, host_id: Uuid) -> Result<(), tonic::Status> {
        let mut client = self
            .remote_agent_client("ClientService.SubscribeAgentEvents", host_id)
            .await?;
        let mut stream = client
            .subscribe_agent_events(wire::SubscribeAgentEventsRequest::default())
            .await?
            .into_inner();

        while let Some(response) = stream.next().await {
            let event = response.and_then(|response| {
                crate::agents::agent_event_from_wire(response).map_err(decode_remote_status)
            })?;
            self.apply_remote_agent_event(host_id, event).await;
        }

        Err(tonic::Status::unavailable(format!(
            "ClientService.SubscribeAgentEvents stream for host {host_id} closed"
        )))
    }

    async fn has_host(&self, host_id: Uuid) -> bool {
        self.state.read().await.hosts_model.contains_key(&host_id)
    }
}

fn client_agent_ref(
    field: &'static str,
    agent: Option<wire::AgentRef>,
) -> Result<AgentRef, tonic::Status> {
    let agent = agent.ok_or_else(|| tonic::Status::invalid_argument(format!("{field} missing")))?;
    let identifier = agent
        .identifier
        .ok_or_else(|| tonic::Status::invalid_argument(format!("{field} missing identifier")))?;
    match identifier {
        wire::agent_ref::Identifier::AgentId(agent_id) => {
            uuid_from_bytes(&format!("{field}.agent_id"), &agent_id).map(AgentRef::Id)
        }
        wire::agent_ref::Identifier::Name(name) => Ok(AgentRef::Name(name)),
    }
}

fn client_send_input_event_to_agent_event(
    event: Option<wire::client_send_input_request::Event>,
) -> Result<Option<wire::pb::send_input_request::Event>, tonic::Status> {
    let event = event
        .ok_or_else(|| tonic::Status::invalid_argument("ClientSendInputRequest missing event"))?;
    Ok(Some(match event {
        wire::client_send_input_request::Event::Input(input) => {
            wire::pb::send_input_request::Event::Input(input)
        }
        wire::client_send_input_request::Event::Control(control) => {
            wire::pb::send_input_request::Event::Control(control)
        }
    }))
}

fn client_send_input_event_to_session_event(
    event: Option<wire::client_send_input_request::Event>,
) -> Result<SessionInputEvent, tonic::Status> {
    let event = event
        .ok_or_else(|| tonic::Status::invalid_argument("ClientSendInputRequest missing event"))?;
    Ok(match event {
        wire::client_send_input_request::Event::Input(input) => SessionInputEvent::Input {
            input_id: input.input_id,
            payload: input.payload,
        },
        wire::client_send_input_request::Event::Control(control) => SessionInputEvent::Control {
            payload: control.payload,
        },
    })
}

fn ensure_local_create_target(
    ctx: &AgentServiceCtx,
    request: &wire::ClientCreateAgentRequest,
) -> Result<(), tonic::Status> {
    let Some(host_id) = optional_uuid_from_bytes(
        "ClientCreateAgentRequest.host_id",
        request.host_id.as_deref(),
    )?
    else {
        return Ok(());
    };

    if host_id == ctx.host_id() {
        Ok(())
    } else {
        Err(tonic::Status::not_found(format!(
            "CreateAgent target host {host_id} is not local"
        )))
    }
}

fn client_create_to_create_rpc_request(
    request: wire::ClientCreateAgentRequest,
) -> Result<CreateAgentRpcRequest, tonic::Status> {
    let agent_id = uuid_from_bytes("ClientCreateAgentRequest.agent_id", &request.agent_id)?;
    let agent = request
        .agent
        .ok_or_else(|| tonic::Status::invalid_argument("ClientCreateAgentRequest missing agent"))?;
    let agent = match agent {
        wire::client_create_agent_request::Agent::Claude(claude) => CreateAgentConfig::ClaudePty {
            working_dir: claude.working_dir.into(),
            args: claude.args,
            terminal_size: claude
                .initial_terminal_size
                .map(client_terminal_size_from_wire)
                .transpose()?,
        },
        wire::client_create_agent_request::Agent::TestAgent(test_agent) => {
            CreateAgentConfig::TestAgent {
                command: test_agent.command,
                working_dir: test_agent.working_dir.into(),
                terminal_size: test_agent
                    .initial_terminal_size
                    .map(client_terminal_size_from_wire)
                    .transpose()?,
            }
        }
    };
    Ok(CreateAgentRpcRequest {
        agent_id,
        name: request.name,
        agent,
    })
}

fn client_create_to_agent_create_request(
    request: wire::ClientCreateAgentRequest,
) -> wire::CreateAgentRequest {
    wire::CreateAgentRequest {
        agent_id: request.agent_id,
        name: request.name,
        agent: request.agent.map(|agent| match agent {
            wire::client_create_agent_request::Agent::Claude(config) => {
                wire::create_agent_request::Agent::Claude(config)
            }
            wire::client_create_agent_request::Agent::TestAgent(config) => {
                wire::create_agent_request::Agent::TestAgent(config)
            }
        }),
    }
}

fn client_terminal_size_from_wire(size: wire::TerminalSize) -> Result<TerminalSize, tonic::Status> {
    Ok(TerminalSize {
        rows: size.rows.try_into().map_err(|_| {
            tonic::Status::invalid_argument(format!("terminal rows out of range: {}", size.rows))
        })?,
        cols: size.cols.try_into().map_err(|_| {
            tonic::Status::invalid_argument(format!("terminal cols out of range: {}", size.cols))
        })?,
    })
}

fn optional_uuid_from_bytes(
    field: &'static str,
    bytes: Option<&[u8]>,
) -> Result<Option<Uuid>, tonic::Status> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    Uuid::from_slice(bytes)
        .map(Some)
        .map_err(|error| tonic::Status::invalid_argument(format!("{field} is invalid: {error}")))
}

fn uuid_from_bytes(field: &str, bytes: &[u8]) -> Result<Uuid, tonic::Status> {
    Uuid::from_slice(bytes)
        .map_err(|error| tonic::Status::invalid_argument(format!("{field} is invalid: {error}")))
}

fn validate_pairing_pubkey(field: &str, bytes: &[u8]) -> Result<(), tonic::Status> {
    if bytes.len() == PUBKEY_LEN {
        Ok(())
    } else {
        Err(tonic::Status::invalid_argument(format!(
            "{field} must be 32 bytes"
        )))
    }
}

fn validate_pairing_token(field: &str, bytes: &[u8]) -> Result<(), tonic::Status> {
    if bytes.len() == TOKEN_LEN {
        Ok(())
    } else {
        Err(tonic::Status::invalid_argument(format!(
            "{field} must be 32 bytes"
        )))
    }
}

fn preflight_pairing_peer(
    trust: &PairingTrustAccess,
    local_host_id: HostId,
    peer_host_id: HostId,
    peer_pubkey: &[u8],
) -> Result<(), tonic::Status> {
    if peer_host_id == local_host_id || peer_pubkey == trust.local_pubkey.as_slice() {
        return Err(tonic::Status::invalid_argument("SELF_PAIRING"));
    }
    let store = trust
        .trust_store
        .read()
        .map_err(|_| tonic::Status::internal("trust store lock is poisoned"))?;
    if let Some(existing_host_id) = store.host_id_for_pubkey(peer_pubkey)
        && existing_host_id != peer_host_id
    {
        return Err(tonic::Status::invalid_argument(
            "PROTOCOL_VIOLATION: pubkey is already trusted",
        ));
    }
    Ok(())
}

fn debug_format_from_wire(format: i32) -> Result<DebugFormat, tonic::Status> {
    match wire::DebugFormat::try_from(format).map_err(|_| {
        tonic::Status::invalid_argument(format!("DebugRequest.format has unknown value {format}"))
    })? {
        wire::DebugFormat::Json => Ok(DebugFormat::Json),
        wire::DebugFormat::Yaml => Ok(DebugFormat::Yaml),
        wire::DebugFormat::Unspecified => Err(tonic::Status::invalid_argument(
            "DebugRequest.format is required",
        )),
    }
}

fn suspend_reason_from_wire(reason: i32) -> Result<ShutdownReason, tonic::Status> {
    match wire::SuspendReason::try_from(reason).map_err(|_| {
        tonic::Status::invalid_argument(format!("invalid SuspendRequest reason: {reason}"))
    })? {
        wire::SuspendReason::Unspecified => Err(tonic::Status::invalid_argument(
            "SuspendRequest.reason is required",
        )),
        wire::SuspendReason::User => Ok(ShutdownReason::Suspending),
        wire::SuspendReason::Update => Ok(ShutdownReason::Updating),
    }
}

fn start_pairing_secret(
    pair_mode: &PairMode,
    mode: wire::start_pairing_request::Mode,
) -> Result<wire::start_pairing_response::Secret, tonic::Status> {
    match mode {
        wire::start_pairing_request::Mode::Unspecified => Err(tonic::Status::invalid_argument(
            "StartPairingRequest.mode is required",
        )),
        wire::start_pairing_request::Mode::Pin => pair_mode
            .start_pin()
            .map(wire::start_pairing_response::Secret::Pin)
            .map_err(pair_mode_admin_status),
        wire::start_pairing_request::Mode::Qr => pair_mode
            .start_token()
            .map(|token| wire::start_pairing_response::Secret::OneShotToken(token.to_vec()))
            .map_err(pair_mode_admin_status),
    }
}

fn pairing_mode_name(mode: wire::start_pairing_request::Mode) -> &'static str {
    match mode {
        wire::start_pairing_request::Mode::Pin => "pin",
        wire::start_pairing_request::Mode::Qr => "qr",
        wire::start_pairing_request::Mode::Unspecified => "unspecified",
    }
}

fn pair_mode_admin_status(error: PairModeError) -> tonic::Status {
    match error {
        PairModeError::AlreadyActive => {
            tonic::Status::failed_precondition("PAIR_MODE_ALREADY_ACTIVE")
        }
        PairModeError::SecretGeneration => tonic::Status::internal("PAIR_MODE_ERROR"),
        PairModeError::InvalidPinFormat
        | PairModeError::InvalidToken
        | PairModeError::NotActive
        | PairModeError::NotPinMode
        | PairModeError::NotTokenMode => tonic::Status::internal("PAIR_MODE_ERROR"),
    }
}

fn encode_status(error: wire::EncodeError) -> tonic::Status {
    tonic::Status::internal(error.to_string())
}

fn decode_remote_status(error: wire::DecodeError) -> tonic::Status {
    tonic::Status::internal(error.to_string())
}

fn remote_tunnel_status(
    method: &'static str,
    host_id: Uuid,
    error: TunnelPoolError,
) -> tonic::Status {
    let message = format!("{method} remote dispatch to host {host_id} failed: {error}");
    match error {
        TunnelPoolError::NotFound { .. } => protocol_status(ProtocolError::Unreachable { message }),
        TunnelPoolError::LinkUnavailable { .. }
        | TunnelPoolError::IncomingTunnelsClosed
        | TunnelPoolError::InboundClosed
        | TunnelPoolError::Identity(_)
        | TunnelPoolError::Tls(_) => tonic::Status::unavailable(message),
        TunnelPoolError::MissingTunnelId
        | TunnelPoolError::InvalidDestination { .. }
        | TunnelPoolError::InvalidTunnelId(_)
        | TunnelPoolError::PayloadTooLarge { .. }
        | TunnelPoolError::DeviceTlsRequired => tonic::Status::internal(message),
    }
}

fn agent_from_remote_response(
    agent: Option<wire::Agent>,
    field: &'static str,
) -> Result<Agent, tonic::Status> {
    let agent = agent.ok_or_else(|| tonic::Status::internal(format!("{field} is missing")))?;
    crate::agents::agent_from_wire(agent).map_err(decode_remote_status)
}

fn validate_remote_agent_response(
    agent: &Agent,
    expected_host_id: Uuid,
    expected_agent_id: Uuid,
    expected_name: Option<&str>,
    field: &'static str,
) -> Result<(), tonic::Status> {
    if agent.host_id != expected_host_id {
        return Err(tonic::Status::internal(format!(
            "{field}.host_id mismatch: expected {expected_host_id}, got {}",
            agent.host_id
        )));
    }
    if agent.id != expected_agent_id {
        return Err(tonic::Status::internal(format!(
            "{field}.agent_id mismatch: expected {expected_agent_id}, got {}",
            agent.id
        )));
    }
    if let Some(expected_name) = expected_name
        && agent.name.as_deref() != Some(expected_name)
    {
        return Err(tonic::Status::internal(format!(
            "{field}.name mismatch: expected {expected_name:?}, got {:?}",
            agent.name
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentChangeKind {
    Up,
    Updated,
}

fn sorted_values_by_id<T: Clone>(map: &HashMap<Uuid, T>, id: impl Fn(&T) -> Uuid) -> Vec<T> {
    let mut values = map.values().cloned().collect::<Vec<_>>();
    values.sort_unstable_by_key(id);
    values
}

fn host_is_agent_capable(host: &Host) -> bool {
    !host.capabilities.supported_agent_types.is_empty()
}

fn is_cloud_relay_host(host: &Host) -> bool {
    host.capabilities
        .features
        .iter()
        .any(|feature| feature == FEATURE_CLOUD_RELAY)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};
    use std::time::Duration;

    use chrono::{TimeZone, Utc};
    use futures_util::StreamExt as _;
    use tempfile::TempDir;
    use tokio::task::JoinHandle;
    use tonic::transport::Endpoint;

    use super::*;
    use crate::agents::{AGENT_TYPE_CLAUDE, TEST_ECHO_COMMAND, TEST_ECHO_V1};
    use crate::config::Config;
    use crate::identity::DeviceIdentity;
    use crate::routing::{
        Capabilities, LinkCloseRequest, LinkId, LinkRole, RoutingCore, SupportedAgentType,
    };
    use crate::services::agent::{AgentServiceState, spawn_agent_tonic_server};
    use crate::trust::{TrustEntry, TrustStore};
    use crate::tunnel::TunnelPool;
    use crate::user_state::{ServerState, ShutdownRequest};

    fn host(id: u128, supported_agent_types: Vec<SupportedAgentType>) -> Host {
        Host {
            id: Uuid::from_u128(id),
            name: format!("host-{id}"),
            version: "test".to_string(),
            capabilities: Capabilities {
                features: Vec::new(),
                supported_agent_types,
            },
        }
    }

    fn cloud_relay_host(id: u128) -> Host {
        let mut host = host(id, Vec::new());
        host.capabilities.features = vec![FEATURE_CLOUD_RELAY.to_string()];
        host
    }

    fn untrusted_online_host_entry(host: Host) -> HostEntry {
        HostEntry {
            id: host.id,
            name: host.name.clone(),
            online: true,
            version: Some(host.version.clone()),
            capabilities: Some(host.capabilities.clone()),
            trust_status: HostTrustStatus::UntrustedButOnline,
            reachability_status: None,
        }
    }

    fn trust_entry(name: &str, pubkey_byte: u8) -> TrustEntry {
        TrustEntry {
            pubkey: vec![pubkey_byte; 32],
            name: name.to_string(),
            paired_at: Utc::now(),
            reachabilities: Vec::new(),
        }
    }

    fn agent(id: u128, host_id: u128, name: &str) -> Agent {
        Agent {
            id: Uuid::from_u128(id),
            host_id: Uuid::from_u128(host_id),
            name: Some(name.to_string()),
            command: "test-agent".to_string(),
            working_dir: PathBuf::from("/tmp"),
            agent_type: "test-agent".to_string(),
            io_protocols: vec!["test_echo_v1".to_string()],
            readonly: false,
            args: Vec::new(),
            created_at: Utc.timestamp_millis_opt(0).single().unwrap(),
        }
    }

    #[test]
    fn remote_agent_response_validation_rejects_wrong_host_or_agent_id() {
        let expected_host_id = Uuid::from_u128(2);
        let expected_agent_id = Uuid::from_u128(42);

        let mut response_agent = agent(42, 2, "remote");
        assert!(
            validate_remote_agent_response(
                &response_agent,
                expected_host_id,
                expected_agent_id,
                Some("remote"),
                "CreateAgentResponse.agent",
            )
            .is_ok()
        );

        response_agent.host_id = Uuid::from_u128(3);
        let error = validate_remote_agent_response(
            &response_agent,
            expected_host_id,
            expected_agent_id,
            Some("remote"),
            "CreateAgentResponse.agent",
        )
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::Internal);
        assert!(error.message().contains("host_id mismatch"));

        response_agent.host_id = expected_host_id;
        response_agent.id = Uuid::from_u128(43);
        let error = validate_remote_agent_response(
            &response_agent,
            expected_host_id,
            expected_agent_id,
            Some("remote"),
            "CreateAgentResponse.agent",
        )
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::Internal);
        assert!(error.message().contains("agent_id mismatch"));

        response_agent.id = expected_agent_id;
        response_agent.name = Some("stale".to_string());
        let error = validate_remote_agent_response(
            &response_agent,
            expected_host_id,
            expected_agent_id,
            Some("remote"),
            "CreateAgentResponse.agent",
        )
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::Internal);
        assert!(error.message().contains("name mismatch"));
    }

    fn client_service_for_tests() -> ClientService {
        client_service_with_local_services()
    }

    fn client_service_with_local_services() -> ClientService {
        let host_id = Uuid::from_u128(1);
        let agent_state = Arc::new(RwLock::new(AgentServiceState::new()));
        let agent_service = AgentServiceCtx::new(agent_state.clone(), host_id, false);
        let (shutdown_tx, _shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
        let server_state = Arc::new(RwLock::new(ServerState::new(
            Config::default(),
            host_id,
            shutdown_tx,
            None,
            None,
        )));
        let (routing, tunnels) = test_routing_and_tunnels(host_id);
        client_service_from_parts(agent_service, server_state, routing, tunnels)
    }

    fn client_service_with_admin_shutdown_rx() -> (ClientService, mpsc::Receiver<ShutdownRequest>) {
        let host_id = Uuid::from_u128(1);
        let agent_state = Arc::new(RwLock::new(AgentServiceState::new()));
        let agent_service = AgentServiceCtx::new(agent_state.clone(), host_id, false);
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
        let server_state = Arc::new(RwLock::new(ServerState::new(
            Config::default(),
            host_id,
            shutdown_tx,
            None,
            None,
        )));
        let (routing, tunnels) = test_routing_and_tunnels(host_id);
        (
            client_service_from_parts(agent_service, server_state, routing, tunnels),
            shutdown_rx,
        )
    }

    fn agent_service_ctx(host_id: Uuid) -> AgentServiceCtx {
        AgentServiceCtx::new(
            Arc::new(RwLock::new(AgentServiceState::new())),
            host_id,
            false,
        )
    }

    fn client_service_with_agent_and_tunnels(
        agent_service: AgentServiceCtx,
        routing: Arc<RoutingCore>,
        tunnels: Arc<TunnelPool>,
    ) -> ClientService {
        let host_id = agent_service.host_id();
        let (shutdown_tx, _shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
        let server_state = Arc::new(RwLock::new(ServerState::new(
            Config::default(),
            host_id,
            shutdown_tx,
            None,
            None,
        )));
        client_service_from_parts(agent_service, server_state, routing, tunnels)
    }

    fn client_service_from_parts(
        agent_service: AgentServiceCtx,
        server_state: Arc<RwLock<ServerState>>,
        routing: Arc<RoutingCore>,
        tunnels: Arc<TunnelPool>,
    ) -> ClientService {
        let connections = Arc::new(ConnectionManager::new(routing, tunnels));
        let identity = DeviceIdentity::for_test(agent_service.host_id());
        ClientService::new(
            agent_service,
            server_state,
            connections,
            PairingTrustAccess::new(
                identity.public_key().to_vec(),
                Arc::new(std::sync::RwLock::new(TrustStore::default())),
                Arc::new(tokio::sync::Mutex::new(())),
                std::env::temp_dir().join(format!("amux-client-service-{}", Uuid::new_v4())),
            ),
            Arc::new(PairMode::new()),
            ReachabilityLinkConnector::disabled(),
        )
    }

    fn client_service_with_pairing_trust(
        data_dir: &std::path::Path,
        local_identity: &DeviceIdentity,
        trust_store: crate::trust::SharedTrustStore,
    ) -> ClientService {
        let agent_state = Arc::new(RwLock::new(AgentServiceState::new()));
        let agent_service = AgentServiceCtx::new(agent_state, local_identity.host_id, false);
        let (shutdown_tx, _shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
        let server_state = Arc::new(RwLock::new(ServerState::new(
            Config::default(),
            local_identity.host_id,
            shutdown_tx,
            None,
            None,
        )));
        let (routing, tunnels) = test_routing_and_tunnels(local_identity.host_id);
        ClientService::new(
            agent_service,
            server_state,
            Arc::new(ConnectionManager::new(routing, tunnels)),
            PairingTrustAccess::new(
                local_identity.public_key().to_vec(),
                trust_store,
                Arc::new(tokio::sync::Mutex::new(())),
                data_dir.to_path_buf(),
            ),
            Arc::new(PairMode::new()),
            ReachabilityLinkConnector::disabled(),
        )
    }

    fn test_routing_and_tunnels(host_id: Uuid) -> (Arc<RoutingCore>, Arc<TunnelPool>) {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(8);
        let tunnels = Arc::new(TunnelPool::new(host_id, routing.clone(), incoming_tx));
        (routing, tunnels)
    }

    struct RemoteDispatchHarness {
        service: ClientService,
        _remote_server: JoinHandle<Result<(), tonic::transport::Error>>,
        bridges: Vec<JoinHandle<()>>,
    }

    impl Drop for RemoteDispatchHarness {
        fn drop(&mut self) {
            self._remote_server.abort();
            for bridge in &self.bridges {
                bridge.abort();
            }
        }
    }

    async fn remote_dispatch_harness() -> RemoteDispatchHarness {
        let local_host_id = Uuid::from_u128(1);
        let remote_host_id = Uuid::from_u128(2);
        let relay_host_id = Uuid::from_u128(3);
        // Each daemon's own link to its neighbor, plus the relay's links to
        // both endpoints. The relay forwards by adjacency alone.
        let local_to_relay = LinkId::new(relay_host_id);
        let remote_to_relay = LinkId::new(relay_host_id);
        let relay_to_remote = LinkId::new(remote_host_id);
        let relay_to_local = LinkId::new(local_host_id);

        let local_routing = Arc::new(RoutingCore::new());
        let remote_routing = Arc::new(RoutingCore::new());
        let relay_routing = Arc::new(RoutingCore::new());
        local_routing
            .apply_claim_up(relay_host_id, host(2, non_relay_types()))
            .await;
        remote_routing
            .apply_claim_up(relay_host_id, host(1, non_relay_types()))
            .await;

        let (local_incoming_tx, _local_incoming_rx) = mpsc::channel(8);
        let (remote_incoming_tx, remote_incoming_rx) = mpsc::channel(8);
        let (relay_incoming_tx, _relay_incoming_rx) = mpsc::channel(8);
        let local_tunnels = Arc::new(TunnelPool::new(
            local_host_id,
            local_routing.clone(),
            local_incoming_tx,
        ));
        let remote_tunnels = Arc::new(TunnelPool::new(
            remote_host_id,
            remote_routing,
            remote_incoming_tx,
        ));
        let relay_tunnels = Arc::new(TunnelPool::new(
            relay_host_id,
            relay_routing,
            relay_incoming_tx,
        ));

        let (local_to_relay_tx, local_to_relay_rx) = mpsc::channel(32);
        let (relay_to_remote_tx, relay_to_remote_rx) = mpsc::channel(32);
        let (remote_to_relay_tx, remote_to_relay_rx) = mpsc::channel(32);
        let (relay_to_local_tx, relay_to_local_rx) = mpsc::channel(32);
        local_tunnels
            .link_registry()
            .register(
                local_to_relay,
                host(3, Vec::new()),
                local_to_relay_tx,
                LinkRole::Peer,
                &[],
            )
            .await;
        remote_tunnels
            .link_registry()
            .register(
                remote_to_relay,
                host(3, Vec::new()),
                remote_to_relay_tx,
                LinkRole::Peer,
                &[],
            )
            .await;
        relay_tunnels
            .link_registry()
            .register(
                relay_to_remote,
                host(2, non_relay_types()),
                relay_to_remote_tx,
                LinkRole::Peer,
                &[],
            )
            .await;
        relay_tunnels
            .link_registry()
            .register(
                relay_to_local,
                host(1, non_relay_types()),
                relay_to_local_tx,
                LinkRole::Peer,
                &[],
            )
            .await;

        let bridges = vec![
            spawn_tunnel_bridge(local_to_relay_rx, relay_tunnels.clone(), relay_to_local),
            spawn_tunnel_bridge(relay_to_remote_rx, remote_tunnels.clone(), remote_to_relay),
            spawn_tunnel_bridge(remote_to_relay_rx, relay_tunnels, relay_to_remote),
            spawn_tunnel_bridge(relay_to_local_rx, local_tunnels.clone(), local_to_relay),
        ];
        let remote_server =
            spawn_agent_tonic_server(agent_service_ctx(remote_host_id), remote_incoming_rx);

        let service = client_service_with_agent_and_tunnels(
            agent_service_ctx(local_host_id),
            local_routing.clone(),
            local_tunnels,
        );

        RemoteDispatchHarness {
            service,
            _remote_server: remote_server,
            bridges,
        }
    }

    fn spawn_tunnel_bridge(
        mut rx: mpsc::Receiver<wire::pb::Message>,
        target_pool: Arc<TunnelPool>,
        arrival_link: LinkId,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                let Some(wire::pb::message::Body::TunnelFrame(frame)) = message.body else {
                    continue;
                };
                target_pool
                    .handle_inbound_frame_from_link(frame, &arrival_link)
                    .await
                    .unwrap();
            }
        })
    }

    async fn recv_agent_event(rx: &mut mpsc::Receiver<AgentEvent>) -> AgentEvent {
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for client agent event")
            .expect("client agent event stream closed")
    }

    async fn expect_session_opened_and_replay_complete(
        stream: &mut ResponseStream<wire::SubscribeSessionResponse>,
    ) {
        assert!(matches!(
            stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_session_response::Event::Opened(_))
        ));
        assert!(matches!(
            stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_session_response::Event::ReplayComplete(_))
        ));
    }

    async fn expect_session_output_payload(
        stream: &mut ResponseStream<wire::SubscribeSessionResponse>,
        expected: &[u8],
    ) {
        let output = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("timed out waiting for session output")
            .expect("session stream closed")
            .expect("session stream returned error");
        let Some(wire::subscribe_session_response::Event::Output(output)) = output.event else {
            panic!("expected SessionOutput");
        };
        assert_eq!(output.payload, expected);
    }

    struct DropNotifyingPendingStream {
        dropped: Arc<AtomicBool>,
    }

    impl Stream for DropNotifyingPendingStream {
        type Item = Result<wire::SubscribeSessionResponse, tonic::Status>;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Pending
        }
    }

    impl Drop for DropNotifyingPendingStream {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    fn test_agent_create_request(
        agent_id: Uuid,
        name: &str,
        host_id: Option<Uuid>,
    ) -> wire::ClientCreateAgentRequest {
        wire::ClientCreateAgentRequest {
            agent_id: agent_id.as_bytes().to_vec(),
            name: Some(name.to_string()),
            host_id: host_id.map(|host_id| host_id.as_bytes().to_vec()),
            agent: Some(wire::client_create_agent_request::Agent::TestAgent(
                wire::TestAgentCreateConfig {
                    command: TEST_ECHO_COMMAND.to_string(),
                    working_dir: std::env::temp_dir().to_string_lossy().into_owned(),
                    initial_terminal_size: None,
                },
            )),
        }
    }

    fn test_agent_service_create_request(agent_id: Uuid, name: &str) -> wire::CreateAgentRequest {
        wire::CreateAgentRequest {
            agent_id: agent_id.as_bytes().to_vec(),
            name: Some(name.to_string()),
            agent: Some(wire::create_agent_request::Agent::TestAgent(
                wire::TestAgentCreateConfig {
                    command: TEST_ECHO_COMMAND.to_string(),
                    working_dir: std::env::temp_dir().to_string_lossy().into_owned(),
                    initial_terminal_size: None,
                },
            )),
        }
    }

    fn agent_ref_id(agent_id: Uuid) -> wire::AgentRef {
        wire::AgentRef {
            identifier: Some(wire::agent_ref::Identifier::AgentId(
                agent_id.as_bytes().to_vec(),
            )),
        }
    }

    fn agent_ref_name(name: &str) -> wire::AgentRef {
        wire::AgentRef {
            identifier: Some(wire::agent_ref::Identifier::Name(name.to_string())),
        }
    }

    fn test_agent_send_input_request(
        agent_id: Uuid,
        payload: &[u8],
    ) -> wire::ClientSendInputRequest {
        wire::ClientSendInputRequest {
            agent: Some(agent_ref_id(agent_id)),
            io_protocol: TEST_ECHO_V1.to_string(),
            event: Some(wire::client_send_input_request::Event::Input(
                wire::pb::SessionInput {
                    input_id: b"input-1".to_vec(),
                    payload: payload.to_vec(),
                },
            )),
        }
    }

    fn test_agent_subscribe_session_request(agent_id: Uuid) -> wire::ClientSubscribeSessionRequest {
        wire::ClientSubscribeSessionRequest {
            agent: Some(agent_ref_id(agent_id)),
            io_protocol: TEST_ECHO_V1.to_string(),
            args: None,
        }
    }

    fn agent_up(agent: Agent) -> AgentEvent {
        AgentEvent::AgentUp { agent }
    }

    fn local_client_request<T>(message: T) -> tonic::Request<T> {
        let mut request = tonic::Request::new(message);
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });
        request
    }

    async fn tonic_list_hosts(service: &ClientService) -> wire::ListHostsResponse {
        <ClientService as wire::client_service_server::ClientService>::list_hosts(
            service,
            local_client_request(wire::ListHostsRequest {
                scope: wire::list_hosts_request::Scope::All as i32,
            }),
        )
        .await
        .unwrap()
        .into_inner()
    }

    async fn tonic_list_agents(service: &ClientService) -> wire::ListAgentsResponse {
        <ClientService as wire::client_service_server::ClientService>::list_agents(
            service,
            tonic::Request::new(wire::ListAgentsRequest {}),
        )
        .await
        .unwrap()
        .into_inner()
    }

    fn non_relay_types() -> Vec<SupportedAgentType> {
        vec![SupportedAgentType {
            agent_type: AGENT_TYPE_CLAUDE.to_string(),
        }]
    }

    #[tokio::test]
    async fn host_model_filters_relays_and_snapshots_non_relays() {
        let service = client_service_with_local_services();
        let mut rx = service.subscribe_hosts().await;

        let relay_host = cloud_relay_host(1);
        assert_eq!(
            service
                .apply_host_event(HostReachabilityEvent::Added {
                    host: relay_host.clone(),
                })
                .await,
            HostEventOutcome::IgnoredRelayOrUnknown
        );
        assert!(service.list_hosts().await.is_empty());
        assert!(rx.try_recv().is_err());

        let real_host = host(2, non_relay_types());
        assert_eq!(
            service
                .apply_host_event(HostReachabilityEvent::Added {
                    host: real_host.clone(),
                })
                .await,
            HostEventOutcome::Added
        );
        assert_eq!(
            rx.recv().await,
            Some(HostEvent::HostUpdated {
                host: untrusted_online_host_entry(real_host.clone())
            })
        );

        let (snapshot, _) = service.subscribe_hosts_with_snapshot().await;
        assert_eq!(snapshot, vec![untrusted_online_host_entry(real_host)]);
    }

    #[tokio::test]
    async fn non_agent_online_hosts_remain_pairing_candidates() {
        let service = client_service_with_local_services();
        let pairing_peer = host(2, Vec::new());

        assert_eq!(
            service
                .apply_host_event(HostReachabilityEvent::Added {
                    host: pairing_peer.clone(),
                })
                .await,
            HostEventOutcome::Added
        );

        let hosts = <ClientService as wire::client_service_server::ClientService>::list_hosts(
            &service,
            local_client_request(wire::ListHostsRequest {
                scope: wire::list_hosts_request::Scope::All as i32,
            }),
        )
        .await
        .unwrap()
        .into_inner();
        assert_eq!(hosts.hosts.len(), 1);
        assert_eq!(hosts.hosts[0].host_id, pairing_peer.id.as_bytes().to_vec());
        assert_eq!(
            hosts.hosts[0].trust_status,
            wire::HostTrustStatus::UntrustedButOnline as i32
        );
    }

    #[tokio::test]
    async fn internal_host_model_insert_without_client_delivery_does_not_mark_visible_activity() {
        let local_host_id = Uuid::from_u128(2);
        let relay = Uuid::from_u128(9_999);
        let routing = Arc::new(RoutingCore::new());
        routing
            .apply_direct_up(host(1, non_relay_types()), LinkId::new(Uuid::from_u128(1)))
            .await;
        for id in 2..=crate::resource_limits::ROUTING_HOST_CAP as u128 {
            routing
                .apply_claim_up(relay, host(id, non_relay_types()))
                .await;
        }
        let (incoming_tx, _incoming_rx) = mpsc::channel(8);
        let tunnels = Arc::new(TunnelPool::new(local_host_id, routing.clone(), incoming_tx));
        let service = client_service_with_agent_and_tunnels(
            agent_service_ctx(local_host_id),
            routing.clone(),
            tunnels,
        );

        assert_eq!(
            service
                .apply_host_event(HostReachabilityEvent::Added {
                    host: host(2, non_relay_types()),
                })
                .await,
            HostEventOutcome::Added
        );
        assert_eq!(
            routing
                .apply_claim_up(relay, host(1001, non_relay_types()))
                .await,
            crate::routing::RouteUpdateOutcome::Inserted
        );

        assert!(routing.host_entry(HostId::from_u128(1)).await.is_some());
        assert!(routing.host_entry(HostId::from_u128(2)).await.is_none());
        assert!(routing.host_entry(HostId::from_u128(1001)).await.is_some());
    }

    #[tokio::test]
    async fn remote_subscriber_hidden_untrusted_live_event_does_not_mark_visible_activity() {
        let local_host_id = Uuid::from_u128(10_000);
        let relay = Uuid::from_u128(9_999);
        let routing = Arc::new(RoutingCore::new());
        routing
            .apply_direct_up(host(1, non_relay_types()), LinkId::new(Uuid::from_u128(1)))
            .await;
        for id in 2..=crate::resource_limits::ROUTING_HOST_CAP as u128 {
            routing
                .apply_claim_up(relay, host(id, non_relay_types()))
                .await;
        }
        let (incoming_tx, _incoming_rx) = mpsc::channel(8);
        let tunnels = Arc::new(TunnelPool::new(local_host_id, routing.clone(), incoming_tx));
        let service = client_service_with_agent_and_tunnels(
            agent_service_ctx(local_host_id),
            routing.clone(),
            tunnels,
        );
        let mut request = tonic::Request::new(wire::SubscribeHostsRequest {});
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::TlsTrusted {
                peer: Uuid::from_u128(99),
            },
        });
        let mut stream =
            <ClientService as wire::client_service_server::ClientService>::subscribe_hosts(
                &service, request,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(matches!(
            stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_hosts_response::Event::SnapshotComplete(_))
        ));

        assert_eq!(
            service
                .apply_host_event(HostReachabilityEvent::Added {
                    host: host(2, non_relay_types()),
                })
                .await,
            HostEventOutcome::Added
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), stream.next())
                .await
                .is_err()
        );
        assert_eq!(
            routing
                .apply_claim_up(relay, host(1001, non_relay_types()))
                .await,
            crate::routing::RouteUpdateOutcome::Inserted
        );

        assert!(routing.host_entry(HostId::from_u128(1)).await.is_some());
        assert!(routing.host_entry(HostId::from_u128(2)).await.is_none());
        assert!(routing.host_entry(HostId::from_u128(1001)).await.is_some());
    }

    #[tokio::test]
    async fn create_agent_rejects_relay_or_unknown_host_targets() {
        let service = client_service_for_tests();
        let non_agent_host_id = Uuid::from_u128(2);
        assert_eq!(
            service
                .apply_host_event(HostReachabilityEvent::Added {
                    host: host(2, Vec::new()),
                })
                .await,
            HostEventOutcome::Added
        );

        let error = <ClientService as wire::client_service_server::ClientService>::create_agent(
            &service,
            tonic::Request::new(test_agent_create_request(
                Uuid::from_u128(20),
                "relay-target",
                Some(non_agent_host_id),
            )),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code(), tonic::Code::Unavailable);
        assert!(error.message().contains("agent-capable host"));
    }

    #[tokio::test]
    async fn host_removed_removes_remote_agents_and_emits_agent_downs() {
        let service = client_service_with_local_services();
        let removed_host = host(10, non_relay_types());
        service
            .apply_host_event(HostReachabilityEvent::Added {
                host: removed_host.clone(),
            })
            .await;
        service
            .apply_agent_event(agent_up(agent(1, 10, "gone")))
            .await;
        service
            .apply_agent_event(agent_up(agent(2, 20, "stays")))
            .await;

        let mut host_rx = service.subscribe_hosts().await;
        let mut agent_rx = service.subscribe_agents().await;

        assert_eq!(
            service
                .apply_host_event(HostReachabilityEvent::Removed {
                    host_id: removed_host.id,
                })
                .await,
            HostEventOutcome::Removed { removed_agents: 1 }
        );

        assert_eq!(
            agent_rx.recv().await,
            Some(AgentEvent::AgentDown {
                agent_id: Uuid::from_u128(1),
            })
        );
        assert_eq!(
            host_rx.recv().await,
            Some(HostEvent::HostRemoved {
                id: removed_host.id,
            })
        );
        assert_eq!(
            service
                .list_agents()
                .await
                .into_iter()
                .map(|agent| agent.id)
                .collect::<Vec<_>>(),
            vec![Uuid::from_u128(2)]
        );
    }

    #[tokio::test]
    async fn remote_agent_subscription_error_leaves_cached_agents_until_host_removed() {
        let service = client_service_with_local_services();
        let remote_host_id = Uuid::from_u128(10);
        service
            .apply_host_event(HostReachabilityEvent::Added {
                host: host(10, non_relay_types()),
            })
            .await;
        service
            .apply_agent_event(agent_up(agent(1, 10, "gone")))
            .await;
        let mut agent_rx = service.subscribe_agents().await;

        let task = tokio::spawn(
            service
                .clone()
                .run_remote_agent_subscription(remote_host_id),
        );

        tokio::time::sleep(REMOTE_AGENT_SUBSCRIPTION_RETRY_DELAY * 2).await;
        assert!(agent_rx.try_recv().is_err());
        assert_eq!(
            service
                .list_agents()
                .await
                .into_iter()
                .map(|agent| agent.id)
                .collect::<Vec<_>>(),
            vec![Uuid::from_u128(1)]
        );

        service
            .apply_host_event(HostReachabilityEvent::Removed {
                host_id: remote_host_id,
            })
            .await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), agent_rx.recv())
                .await
                .expect("timed out waiting for host removal cleanup"),
            Some(AgentEvent::AgentDown {
                agent_id: Uuid::from_u128(1)
            })
        );
        assert!(service.list_agents().await.is_empty());
        task.abort();
    }

    #[tokio::test]
    async fn duplicate_agent_upsert_is_ignored_without_rebroadcasting() {
        let service = client_service_with_local_services();
        let agent = agent(1, 10, "same");
        let mut agent_rx = service.subscribe_agents().await;

        assert_eq!(
            service.apply_agent_event(agent_up(agent.clone())).await,
            AgentEventOutcome::Upserted
        );
        assert_eq!(
            agent_rx.recv().await,
            Some(AgentEvent::AgentUp {
                agent: agent.clone(),
            })
        );
        assert_eq!(
            service.apply_agent_event(agent_up(agent)).await,
            AgentEventOutcome::Ignored
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), agent_rx.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn remote_agent_subscription_ignores_events_for_other_hosts() {
        let service = client_service_with_local_services();
        let expected_host = Uuid::from_u128(2);
        let mismatched = agent(1, 3, "wrong-host");
        let mut agent_rx = service.subscribe_agents().await;

        assert_eq!(
            service
                .apply_remote_agent_event(expected_host, agent_up(mismatched.clone()))
                .await,
            AgentEventOutcome::Ignored
        );
        assert!(service.list_agents().await.is_empty());
        assert!(agent_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn remote_agent_subscription_ignores_down_for_other_host_agent() {
        let service = client_service_with_local_services();
        let owner_host = Uuid::from_u128(3);
        let source_host = Uuid::from_u128(2);
        let existing = agent(1, owner_host.as_u128(), "owned-elsewhere");

        service.apply_agent_event(agent_up(existing.clone())).await;
        assert_eq!(
            service
                .apply_remote_agent_event(
                    source_host,
                    AgentEvent::AgentDown {
                        agent_id: existing.id
                    },
                )
                .await,
            AgentEventOutcome::Ignored
        );

        assert_eq!(service.list_agents().await, vec![existing]);
    }

    #[tokio::test]
    async fn attach_routing_events_consumes_startup_deltas_only() {
        let service = client_service_with_local_services();
        let routing = Arc::new(RoutingCore::new());
        let existing = host(10, non_relay_types());
        routing
            .apply_claim_up(Uuid::from_u128(9), existing.clone())
            .await;

        let task = service.attach_routing_events(routing.clone()).await;
        assert!(service.list_hosts().await.is_empty());

        let live = host(11, non_relay_types());
        routing
            .apply_claim_up(Uuid::from_u128(9), live.clone())
            .await;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if service.list_hosts().await == vec![live.clone()] {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for live host event");

        routing.apply_claim_down(Uuid::from_u128(9), live.id).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if service.list_hosts().await.is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for live host removal");
        task.abort();
    }

    #[tokio::test]
    async fn attach_local_agent_events_populates_client_agent_model() {
        let service = client_service_with_local_services();
        let ctx = agent_service_ctx(Uuid::from_u128(1));
        let task = service
            .attach_local_agent_events(ctx.clone())
            .await
            .unwrap();
        let agent_id = Uuid::from_u128(129);

        ctx.create(crate::agents::CreateAgentRpcRequest {
            agent_id,
            name: Some("attached".to_string()),
            agent: crate::agents::CreateAgentConfig::TestAgent {
                command: TEST_ECHO_COMMAND.to_string(),
                working_dir: std::env::temp_dir(),
                terminal_size: None,
            },
        })
        .await
        .unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if service
                    .list_agents()
                    .await
                    .into_iter()
                    .any(|agent| agent.id == agent_id)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for local agent event");

        ctx.delete(agent_id).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if service.list_agents().await.is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for local agent removal");
        task.abort();
    }

    #[tokio::test]
    async fn resolve_agent_finds_ids_names_and_reports_ambiguous_names() {
        let service = client_service_with_local_services();
        let first = agent(1, 10, "review");
        let second = agent(2, 20, "review");

        assert!(matches!(
            service
                .resolve_agent(AgentRef::Id(Uuid::from_u128(1)))
                .await,
            Err(ProtocolError::NoAgentFound)
        ));

        service.apply_agent_event(agent_up(first.clone())).await;
        service.apply_agent_event(agent_up(second.clone())).await;

        assert_eq!(
            service
                .resolve_agent(AgentRef::Id(second.id))
                .await
                .unwrap()
                .id,
            second.id
        );
        assert!(matches!(
            service
                .resolve_agent(AgentRef::Name("missing".to_string()))
                .await,
            Err(ProtocolError::NoAgentFound)
        ));
        assert!(matches!(
            service
                .resolve_agent(AgentRef::Name("review".to_string()))
                .await,
            Err(ProtocolError::AmbiguousAgentName { name, agent_ids })
                if name == "review" && agent_ids == vec![first.id, second.id]
        ));

        service
            .apply_agent_event(AgentEvent::AgentDown {
                agent_id: second.id,
            })
            .await;
        assert_eq!(
            service
                .resolve_agent(AgentRef::Name("review".to_string()))
                .await
                .unwrap()
                .id,
            first.id
        );
    }

    #[test]
    fn host_snapshot_and_events_encode_to_client_service_wire() {
        let host = host(1, non_relay_types());
        let responses = host_snapshot_to_wire(vec![untrusted_online_host_entry(host.clone())]);
        assert_eq!(responses.len(), 2);

        let wire::subscribe_hosts_response::Event::HostUpdated(added) =
            responses[0].event.clone().unwrap()
        else {
            panic!("expected HostUpdated");
        };
        let added = added.host.unwrap();
        assert_eq!(added.host_id, host.id.as_bytes().to_vec());
        assert_eq!(
            added.trust_status,
            wire::HostTrustStatus::UntrustedButOnline as i32
        );
        assert!(added.reachability_status.is_none());
        assert!(matches!(
            responses[1].event,
            Some(wire::subscribe_hosts_response::Event::SnapshotComplete(_))
        ));

        let removed = client_host_event_to_wire(&HostEvent::HostRemoved { id: host.id });
        let Some(wire::subscribe_hosts_response::Event::HostRemoved(removed)) = removed.event
        else {
            panic!("expected HostRemoved");
        };
        assert_eq!(removed.host_id, host.id.as_bytes().to_vec());
    }

    #[test]
    fn agent_snapshot_and_events_encode_to_client_service_wire() {
        let first = agent(1, 10, "first");
        let second = agent(2, 10, "second");
        let responses = agent_snapshot_to_wire(vec![first.clone()]).unwrap();
        assert_eq!(responses.len(), 2);

        let wire::subscribe_agents_response::Event::AgentUp(up) =
            responses[0].event.clone().unwrap()
        else {
            panic!("expected AgentUp");
        };
        assert_eq!(up.agent.unwrap().agent_id, first.id.as_bytes().to_vec());
        assert!(matches!(
            responses[1].event,
            Some(wire::subscribe_agents_response::Event::SnapshotComplete(_))
        ));

        let updated = client_agent_event_to_wire(&AgentEvent::AgentUpdated {
            agent: second.clone(),
        })
        .unwrap();
        let Some(wire::subscribe_agents_response::Event::AgentUpdated(updated)) = updated.event
        else {
            panic!("expected AgentUpdated");
        };
        assert_eq!(
            updated.agent.unwrap().agent_id,
            second.id.as_bytes().to_vec()
        );

        let down = client_agent_event_to_wire(&AgentEvent::AgentDown {
            agent_id: second.id,
        })
        .unwrap();
        let Some(wire::subscribe_agents_response::Event::AgentDown(down)) = down.event else {
            panic!("expected AgentDown");
        };
        assert_eq!(down.agent_id, second.id.as_bytes().to_vec());
        assert_eq!(down.reason, None);
    }

    #[tokio::test]
    async fn remote_session_stream_maps_unavailable_to_host_unreachable_close() {
        let opened = wire::SubscribeSessionResponse {
            event: Some(wire::subscribe_session_response::Event::Opened(
                wire::SessionOpened {},
            )),
        };
        let mut stream = remote_session_response_stream(futures_util::stream::iter(vec![
            Ok(opened.clone()),
            Err(tonic::Status::unavailable("host lost")),
            Ok(opened.clone()),
        ]));

        assert_eq!(stream.next().await.unwrap().unwrap(), opened);
        let closed = stream.next().await.unwrap().unwrap();
        let Some(wire::subscribe_session_response::Event::Closed(closed)) = closed.event else {
            panic!("expected SessionClosed");
        };
        assert!(matches!(
            closed.reason,
            Some(wire::session_closed::Reason::HostUnreachable(_))
        ));
        assert!(stream.next().await.is_none());

        let mut stream = remote_session_response_stream(futures_util::stream::iter(vec![Err(
            tonic::Status::internal("not a route failure"),
        )]));
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.code(), tonic::Code::Internal);
        assert!(stream.next().await.is_none());

        let mut shutdown = tonic::Status::unavailable("server suspending");
        shutdown.metadata_mut().insert(
            SHUTDOWN_REASON_METADATA_KEY,
            tonic::metadata::MetadataValue::from_static("suspending"),
        );
        let mut stream =
            remote_session_response_stream(futures_util::stream::iter(vec![Err(shutdown)]));
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unavailable);
        assert_eq!(
            error
                .metadata()
                .get(SHUTDOWN_REASON_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some("suspending")
        );
        assert!(stream.next().await.is_none());

        let mut stream = host_unreachable_session_response_stream();
        let closed = stream.next().await.unwrap().unwrap();
        let Some(wire::subscribe_session_response::Event::Closed(closed)) = closed.event else {
            panic!("expected pre-stream SessionClosed");
        };
        assert!(matches!(
            closed.reason,
            Some(wire::session_closed::Reason::HostUnreachable(_))
        ));
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn dropping_remote_session_stream_drops_upstream_stream() {
        let dropped = Arc::new(AtomicBool::new(false));
        let stream = remote_session_response_stream(DropNotifyingPendingStream {
            dropped: dropped.clone(),
        });

        assert!(!dropped.load(Ordering::SeqCst));
        drop(stream);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn tonic_client_service_lists_and_streams_model() {
        let service = client_service_with_local_services();
        let first_host = host(10, non_relay_types());
        let first_agent = agent(1, 10, "first");
        service
            .apply_host_event(HostReachabilityEvent::Added {
                host: first_host.clone(),
            })
            .await;
        service
            .apply_agent_event(agent_up(first_agent.clone()))
            .await;

        let hosts = tonic_list_hosts(&service).await;
        assert_eq!(hosts.hosts.len(), 1);
        assert_eq!(hosts.hosts[0].host_id, first_host.id.as_bytes().to_vec());
        assert_eq!(
            hosts.hosts[0].trust_status,
            wire::HostTrustStatus::UntrustedButOnline as i32
        );
        assert!(hosts.hosts[0].reachability_status.is_none());

        let agents = tonic_list_agents(&service).await;
        assert_eq!(agents.agents.len(), 1);
        assert_eq!(
            agents.agents[0].agent_id,
            first_agent.id.as_bytes().to_vec()
        );

        let mut host_stream =
            <ClientService as wire::client_service_server::ClientService>::subscribe_hosts(
                &service,
                local_client_request(wire::SubscribeHostsRequest {}),
            )
            .await
            .unwrap()
            .into_inner();
        assert!(matches!(
            host_stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_hosts_response::Event::HostUpdated(_))
        ));
        assert!(matches!(
            host_stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_hosts_response::Event::SnapshotComplete(_))
        ));
        service
            .apply_host_event(HostReachabilityEvent::Removed {
                host_id: first_host.id,
            })
            .await;
        assert!(matches!(
            host_stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_hosts_response::Event::HostRemoved(_))
        ));

        let mut agent_stream =
            <ClientService as wire::client_service_server::ClientService>::subscribe_agents(
                &service,
                tonic::Request::new(wire::SubscribeAgentsRequest {}),
            )
            .await
            .unwrap()
            .into_inner();
        assert!(matches!(
            agent_stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_agents_response::Event::SnapshotComplete(_))
        ));

        let second_agent = agent(2, 20, "second");
        service
            .apply_agent_event(agent_up(second_agent.clone()))
            .await;
        assert!(matches!(
            agent_stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_agents_response::Event::AgentUp(_))
        ));
    }

    #[tokio::test]
    async fn tonic_list_hosts_reports_trusted_offline_hosts_as_unknown() {
        let data_dir = tempfile::tempdir().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        trust_store
            .write()
            .unwrap()
            .insert_for_test(Uuid::from_u128(2), trust_entry("trusted-offline", 2));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());

        let hosts = tonic_list_hosts(&service).await;

        assert_eq!(hosts.hosts.len(), 1);
        let host = &hosts.hosts[0];
        assert_eq!(host.host_id, Uuid::from_u128(2).as_bytes().to_vec());
        assert_eq!(host.name, "trusted-offline");
        assert!(!host.online);
        assert!(host.version.is_none());
        assert!(host.capabilities.is_none());
        assert_eq!(host.trust_status, wire::HostTrustStatus::Trusted as i32);
        let status = host.reachability_status.as_ref().unwrap();
        assert!(matches!(
            status.status,
            Some(wire::host_reachability_status::Status::Unknown(_))
        ));
    }

    #[tokio::test]
    async fn tonic_list_hosts_reports_cached_trusted_reachability_error() {
        let data_dir = tempfile::tempdir().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let peer = Uuid::from_u128(2);
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        trust_store
            .write()
            .unwrap()
            .insert_for_test(peer, trust_entry("trusted-peer", 2));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());
        service
            .remote_agent_connections
            .record_reachability_error(peer, "ssh alias did not resolve")
            .await;

        let hosts = tonic_list_hosts(&service).await;

        let host = hosts
            .hosts
            .iter()
            .find(|host| host.host_id == peer.as_bytes())
            .unwrap();
        assert!(!host.online);
        assert_eq!(host.trust_status, wire::HostTrustStatus::Trusted as i32);
        let status = host.reachability_status.as_ref().unwrap();
        assert!(matches!(
            &status.status,
            Some(wire::host_reachability_status::Status::Unreachable(unreachable))
                if unreachable.last_error == "ssh alias did not resolve"
        ));
    }

    #[tokio::test]
    async fn host_status_change_publishes_cached_reachability_error() {
        let data_dir = tempfile::tempdir().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let peer = Uuid::from_u128(2);
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        trust_store
            .write()
            .unwrap()
            .insert_for_test(peer, trust_entry("trusted-peer", 2));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());
        let mut rx = service.subscribe_hosts().await;
        service
            .remote_agent_connections
            .record_reachability_error(peer, "tcp connect refused")
            .await;

        assert_eq!(
            service
                .apply_host_event(HostReachabilityEvent::StatusChanged { host_id: peer })
                .await,
            HostEventOutcome::Updated
        );

        assert!(matches!(
            rx.recv().await,
            Some(HostEvent::HostUpdated { host })
                if host.id == peer
                    && !host.online
                    && host.reachability_status
                        == Some(HostReachabilityStatus::Unreachable {
                            last_error: "tcp connect refused".to_string(),
                        })
        ));
    }

    #[tokio::test]
    async fn remote_host_inventory_filters_untrusted_online_hosts() {
        let data_dir = tempfile::tempdir().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let trusted = Uuid::from_u128(2);
        let untrusted = host(3, Vec::new());
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        trust_store
            .write()
            .unwrap()
            .insert_for_test(trusted, trust_entry("trusted-peer", 2));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());
        service
            .apply_host_event(HostReachabilityEvent::Added {
                host: untrusted.clone(),
            })
            .await;
        let mut request = tonic::Request::new(wire::ListHostsRequest {
            scope: wire::list_hosts_request::Scope::All as i32,
        });
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::TlsTrusted {
                peer: Uuid::from_u128(99),
            },
        });

        let response = <ClientService as wire::client_service_server::ClientService>::list_hosts(
            &service, request,
        )
        .await
        .unwrap()
        .into_inner();

        assert_eq!(response.hosts.len(), 1);
        assert_eq!(response.hosts[0].host_id, trusted.as_bytes().to_vec());

        let metadata_less_response =
            <ClientService as wire::client_service_server::ClientService>::list_hosts(
                &service,
                tonic::Request::new(wire::ListHostsRequest {
                    scope: wire::list_hosts_request::Scope::All as i32,
                }),
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(metadata_less_response.hosts.len(), 1);
        assert_eq!(
            metadata_less_response.hosts[0].host_id,
            trusted.as_bytes().to_vec()
        );

        let mut request = tonic::Request::new(wire::SubscribeHostsRequest {});
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::TlsTrusted {
                peer: Uuid::from_u128(99),
            },
        });
        let mut stream =
            <ClientService as wire::client_service_server::ClientService>::subscribe_hosts(
                &service, request,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(matches!(
            stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_hosts_response::Event::HostUpdated(updated))
                if updated.host.as_ref().unwrap().host_id == trusted.as_bytes().to_vec()
        ));
        assert!(matches!(
            stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_hosts_response::Event::SnapshotComplete(_))
        ));
    }

    #[tokio::test]
    async fn remote_pairing_candidate_inventory_is_rejected() {
        let service = client_service_with_local_services();
        let mut request = tonic::Request::new(wire::ListHostsRequest {
            scope: wire::list_hosts_request::Scope::PairingCandidates as i32,
        });
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::TlsTrusted {
                peer: Uuid::from_u128(99),
            },
        });

        let error = <ClientService as wire::client_service_server::ClientService>::list_hosts(
            &service, request,
        )
        .await
        .unwrap_err();

        assert_eq!(error.code(), tonic::Code::PermissionDenied);

        let error = <ClientService as wire::client_service_server::ClientService>::list_hosts(
            &service,
            tonic::Request::new(wire::ListHostsRequest {
                scope: wire::list_hosts_request::Scope::PairingCandidates as i32,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn trust_transition_publishes_host_status_update() {
        let data_dir = tempfile::tempdir().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let peer = host(2, Vec::new());
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());
        let mut rx = service.subscribe_hosts().await;
        service
            .apply_host_event(HostReachabilityEvent::Added { host: peer.clone() })
            .await;
        assert!(matches!(
            rx.recv().await,
            Some(HostEvent::HostUpdated { host })
                if host.id == peer.id
                    && host.online
                    && host.trust_status == HostTrustStatus::UntrustedButOnline
                    && host.reachability_status.is_none()
        ));

        trust_store
            .write()
            .unwrap()
            .insert_for_test(peer.id, trust_entry("trusted-peer", 2));
        service.publish_host_status_update(peer.id).await;

        assert!(matches!(
            rx.recv().await,
            Some(HostEvent::HostUpdated { host })
                if host.id == peer.id
                    && host.online
                    && host.trust_status == HostTrustStatus::Trusted
                    && host.reachability_status == Some(HostReachabilityStatus::Reachable)
        ));
    }

    #[tokio::test]
    async fn trusted_route_loss_publishes_offline_update_not_removal() {
        let data_dir = tempfile::tempdir().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let peer = host(2, non_relay_types());
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        trust_store
            .write()
            .unwrap()
            .insert_for_test(peer.id, trust_entry("trusted-peer", 2));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());
        let mut rx = service.subscribe_hosts().await;
        service
            .apply_host_event(HostReachabilityEvent::Added { host: peer.clone() })
            .await;
        assert!(matches!(
            rx.recv().await,
            Some(HostEvent::HostUpdated { host })
                if host.id == peer.id
                    && host.online
                    && host.trust_status == HostTrustStatus::Trusted
        ));

        service
            .apply_host_event(HostReachabilityEvent::Removed { host_id: peer.id })
            .await;

        assert!(matches!(
            rx.recv().await,
            Some(HostEvent::HostUpdated { host })
                if host.id == peer.id
                    && !host.online
                    && host.trust_status == HostTrustStatus::Trusted
                    && host.reachability_status == Some(HostReachabilityStatus::Unknown)
        ));
    }

    #[tokio::test]
    async fn tonic_list_hosts_all_includes_local_host() {
        let service = client_service_with_local_services();
        let local = host(1, non_relay_types());
        let remote = host(2, non_relay_types());
        service
            .apply_host_event(HostReachabilityEvent::Added {
                host: local.clone(),
            })
            .await;
        service
            .apply_host_event(HostReachabilityEvent::Added {
                host: remote.clone(),
            })
            .await;

        let response = <ClientService as wire::client_service_server::ClientService>::list_hosts(
            &service,
            local_client_request(wire::ListHostsRequest {
                scope: wire::list_hosts_request::Scope::All as i32,
            }),
        )
        .await
        .unwrap()
        .into_inner();

        assert_eq!(response.hosts.len(), 2);
        assert!(
            response
                .hosts
                .iter()
                .any(|host| host.host_id == local.id.as_bytes())
        );
        assert!(
            response
                .hosts
                .iter()
                .any(|host| host.host_id == remote.id.as_bytes())
        );
    }

    #[tokio::test]
    async fn tonic_list_hosts_cloud_routable_filter_matches_connection_manager() {
        let host_id = Uuid::from_u128(1);
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(8);
        let tunnels = Arc::new(TunnelPool::new(host_id, routing.clone(), incoming_tx));
        let service = client_service_with_agent_and_tunnels(
            agent_service_ctx(host_id),
            routing.clone(),
            tunnels.clone(),
        );
        let cloud_peer = host(2, non_relay_types());
        let direct_peer = host(3, non_relay_types());
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        let cloud_relay = Host {
            id: Uuid::from_u128(99),
            name: "cloud".to_string(),
            version: "test".to_string(),
            capabilities: Capabilities {
                features: vec![crate::routing::FEATURE_CLOUD_RELAY.to_string()],
                supported_agent_types: Vec::new(),
            },
        };
        tunnels
            .link_registry()
            .register(
                LinkId::new(cloud_relay.id),
                cloud_relay.clone(),
                cloud_tx,
                LinkRole::CloudRelay,
                &[],
            )
            .await;
        service
            .apply_host_event(HostReachabilityEvent::Added {
                host: cloud_peer.clone(),
            })
            .await;
        service
            .apply_host_event(HostReachabilityEvent::Added { host: direct_peer })
            .await;
        // The cloud relay claims adjacency to the cloud peer; the direct
        // peer has a channel-backed link of our own.
        routing
            .apply_claim_up(cloud_relay.id, cloud_peer.clone())
            .await;
        routing
            .apply_direct_up(host(3, non_relay_types()), LinkId::new(Uuid::from_u128(3)))
            .await;

        let response = <ClientService as wire::client_service_server::ClientService>::list_hosts(
            &service,
            local_client_request(wire::ListHostsRequest {
                scope: wire::list_hosts_request::Scope::PairingCandidates as i32,
            }),
        )
        .await
        .unwrap()
        .into_inner();

        assert_eq!(response.hosts.len(), 1);
        assert_eq!(response.hosts[0].host_id, cloud_peer.id.as_bytes().to_vec());
    }

    #[tokio::test]
    async fn tonic_client_service_subscribe_agents_reports_resource_exhausted_when_queue_closes() {
        let service = client_service_with_local_services();
        let mut stream =
            <ClientService as wire::client_service_server::ClientService>::subscribe_agents(
                &service,
                tonic::Request::new(wire::SubscribeAgentsRequest {}),
            )
            .await
            .unwrap()
            .into_inner();

        for index in 0..300 {
            let agent_id = 10_000 + index;
            service
                .apply_agent_event(agent_up(agent(agent_id, 1, &format!("overflow-{index}"))))
                .await;
        }

        let mut agent_up_count = 0;
        loop {
            let item = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("timed out waiting for subscribe-agents stream")
                .expect("subscribe-agents stream closed unexpectedly");
            match item {
                Ok(response) => match response.event {
                    Some(wire::subscribe_agents_response::Event::SnapshotComplete(_)) => {}
                    Some(wire::subscribe_agents_response::Event::AgentUp(_)) => {
                        agent_up_count += 1;
                    }
                    other => panic!("unexpected subscribe-agents event: {other:?}"),
                },
                Err(status) => {
                    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
                    break;
                }
            }
        }

        assert_eq!(agent_up_count, 256);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn tonic_client_service_dispatches_local_lifecycle_methods() {
        let service = client_service_for_tests();
        let agent_id = Uuid::from_u128(123);
        let mut events = service.subscribe_agents().await;

        let created = <ClientService as wire::client_service_server::ClientService>::create_agent(
            &service,
            tonic::Request::new(test_agent_create_request(agent_id, "draft", None)),
        )
        .await
        .unwrap()
        .into_inner();
        assert_eq!(
            created.agent.as_ref().unwrap().agent_id,
            agent_id.as_bytes()
        );
        assert_eq!(
            created.agent.as_ref().unwrap().name.as_deref(),
            Some("draft")
        );
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::AgentUp { agent }) if agent.id == agent_id
        ));

        let renamed = <ClientService as wire::client_service_server::ClientService>::rename_agent(
            &service,
            tonic::Request::new(wire::ClientRenameAgentRequest {
                agent: Some(agent_ref_name("draft")),
                name: "renamed".to_string(),
            }),
        )
        .await
        .unwrap()
        .into_inner();
        assert_eq!(
            renamed.agent.as_ref().unwrap().name.as_deref(),
            Some("renamed")
        );
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::AgentUpdated { agent })
                if agent.id == agent_id && agent.name.as_deref() == Some("renamed")
        ));

        <ClientService as wire::client_service_server::ClientService>::send_input(
            &service,
            tonic::Request::new(test_agent_send_input_request(agent_id, b"hello")),
        )
        .await
        .unwrap();

        <ClientService as wire::client_service_server::ClientService>::delete_agent(
            &service,
            tonic::Request::new(wire::ClientDeleteAgentRequest {
                agent: Some(agent_ref_name("renamed")),
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::AgentDown { agent_id: down_id }) if down_id == agent_id
        ));
        assert!(service.list_agents().await.is_empty());
    }

    #[tokio::test]
    async fn tonic_client_service_dispatches_local_subscribe_session() {
        let service = client_service_for_tests();
        let agent_id = Uuid::from_u128(126);

        <ClientService as wire::client_service_server::ClientService>::create_agent(
            &service,
            tonic::Request::new(test_agent_create_request(agent_id, "echo", None)),
        )
        .await
        .unwrap();

        let mut stream =
            <ClientService as wire::client_service_server::ClientService>::subscribe_session(
                &service,
                tonic::Request::new(test_agent_subscribe_session_request(agent_id)),
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

        <ClientService as wire::client_service_server::ClientService>::send_input(
            &service,
            tonic::Request::new(test_agent_send_input_request(agent_id, b"through-client")),
        )
        .await
        .unwrap();

        let output = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .expect("timed out waiting for client session output")
            .expect("client session stream closed")
            .expect("client session stream returned error");
        let Some(wire::subscribe_session_response::Event::Output(output)) = output.event else {
            panic!("expected SessionOutput");
        };
        assert_eq!(output.payload, b"through-client");

        <ClientService as wire::client_service_server::ClientService>::delete_agent(
            &service,
            tonic::Request::new(wire::ClientDeleteAgentRequest {
                agent: Some(agent_ref_id(agent_id)),
            }),
        )
        .await
        .unwrap();
        let closed = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .expect("timed out waiting for client session close")
            .expect("client session stream closed before close event")
            .expect("client session close returned error");
        let Some(wire::subscribe_session_response::Event::Closed(closed)) = closed.event else {
            panic!("expected SessionClosed");
        };
        assert!(matches!(
            closed.reason,
            Some(wire::session_closed::Reason::AgentDeleted(_))
        ));
    }

    #[tokio::test]
    async fn tonic_client_service_dispatches_remote_agent_methods_over_tunnel() {
        let harness = remote_dispatch_harness().await;
        let service = &harness.service;
        let remote_host_id = Uuid::from_u128(2);
        let agent_id = Uuid::from_u128(127);
        let mut events = service.subscribe_agents().await;
        service
            .apply_host_event(HostReachabilityEvent::Added {
                host: host(2, non_relay_types()),
            })
            .await;

        let created = <ClientService as wire::client_service_server::ClientService>::create_agent(
            service,
            tonic::Request::new(test_agent_create_request(
                agent_id,
                "remote-echo",
                Some(remote_host_id),
            )),
        )
        .await
        .unwrap()
        .into_inner();
        assert_eq!(
            created.agent.as_ref().unwrap().host_id,
            remote_host_id.as_bytes()
        );
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::AgentUp { agent })
                if agent.id == agent_id && agent.host_id == remote_host_id
        ));

        let renamed = <ClientService as wire::client_service_server::ClientService>::rename_agent(
            service,
            tonic::Request::new(wire::ClientRenameAgentRequest {
                agent: Some(agent_ref_id(agent_id)),
                name: "renamed-remote".to_string(),
            }),
        )
        .await
        .unwrap()
        .into_inner();
        assert_eq!(
            renamed.agent.as_ref().unwrap().name.as_deref(),
            Some("renamed-remote")
        );
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::AgentUpdated { agent })
                if agent.id == agent_id && agent.name.as_deref() == Some("renamed-remote")
        ));

        let mut stream =
            <ClientService as wire::client_service_server::ClientService>::subscribe_session(
                service,
                tonic::Request::new(test_agent_subscribe_session_request(agent_id)),
            )
            .await
            .unwrap()
            .into_inner();
        assert!(matches!(
            stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_session_response::Event::Opened(_))
        ));
        assert!(matches!(
            stream.next().await.unwrap().unwrap().event,
            Some(wire::subscribe_session_response::Event::ReplayComplete(_))
        ));

        <ClientService as wire::client_service_server::ClientService>::send_input(
            service,
            tonic::Request::new(test_agent_send_input_request(agent_id, b"remote-input")),
        )
        .await
        .unwrap();

        let output = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("timed out waiting for remote session output")
            .expect("remote session stream closed")
            .expect("remote session stream returned error");
        let Some(wire::subscribe_session_response::Event::Output(output)) = output.event else {
            panic!("expected remote SessionOutput");
        };
        assert_eq!(output.payload, b"remote-input");

        <ClientService as wire::client_service_server::ClientService>::delete_agent(
            service,
            tonic::Request::new(wire::ClientDeleteAgentRequest {
                agent: Some(agent_ref_id(agent_id)),
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::AgentDown { agent_id: down_id }) if down_id == agent_id
        ));
        let closed = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("timed out waiting for remote session close")
            .expect("remote session stream closed before close event")
            .expect("remote session close returned error");
        let Some(wire::subscribe_session_response::Event::Closed(closed)) = closed.event else {
            panic!("expected remote SessionClosed");
        };
        assert!(matches!(
            closed.reason,
            Some(wire::session_closed::Reason::AgentDeleted(_))
        ));
    }

    #[tokio::test]
    async fn tonic_client_service_allows_independent_remote_subscribe_sessions() {
        let harness = remote_dispatch_harness().await;
        let service = &harness.service;
        let remote_host_id = Uuid::from_u128(2);
        let agent_id = Uuid::from_u128(130);
        service
            .apply_host_event(HostReachabilityEvent::Added {
                host: host(2, non_relay_types()),
            })
            .await;

        <ClientService as wire::client_service_server::ClientService>::create_agent(
            service,
            tonic::Request::new(test_agent_create_request(
                agent_id,
                "remote-fanout",
                Some(remote_host_id),
            )),
        )
        .await
        .unwrap();

        let mut first =
            <ClientService as wire::client_service_server::ClientService>::subscribe_session(
                service,
                tonic::Request::new(test_agent_subscribe_session_request(agent_id)),
            )
            .await
            .unwrap()
            .into_inner();
        let mut second =
            <ClientService as wire::client_service_server::ClientService>::subscribe_session(
                service,
                tonic::Request::new(test_agent_subscribe_session_request(agent_id)),
            )
            .await
            .unwrap()
            .into_inner();

        expect_session_opened_and_replay_complete(&mut first).await;
        expect_session_opened_and_replay_complete(&mut second).await;

        <ClientService as wire::client_service_server::ClientService>::send_input(
            service,
            tonic::Request::new(test_agent_send_input_request(agent_id, b"fanout-one")),
        )
        .await
        .unwrap();

        expect_session_output_payload(&mut first, b"fanout-one").await;
        expect_session_output_payload(&mut second, b"fanout-one").await;

        drop(first);
        <ClientService as wire::client_service_server::ClientService>::send_input(
            service,
            tonic::Request::new(test_agent_send_input_request(agent_id, b"fanout-two")),
        )
        .await
        .unwrap();
        expect_session_output_payload(&mut second, b"fanout-two").await;
    }

    #[tokio::test]
    async fn host_added_starts_remote_agent_subscription_over_tunnel() {
        let harness = remote_dispatch_harness().await;
        let service = &harness.service;
        let remote_host_id = Uuid::from_u128(2);
        let agent_id = Uuid::from_u128(128);
        let mut events = service.subscribe_agents().await;

        assert_eq!(
            service
                .apply_host_event(HostReachabilityEvent::Added {
                    host: host(2, non_relay_types()),
                })
                .await,
            HostEventOutcome::Added
        );

        let mut remote_agent_client = service
            .remote_agent_client("test.RemoteAgentService", remote_host_id)
            .await
            .unwrap();
        remote_agent_client
            .create_agent(test_agent_service_create_request(agent_id, "subscribed"))
            .await
            .unwrap();
        assert!(matches!(
            recv_agent_event(&mut events).await,
            AgentEvent::AgentUp { agent }
                if agent.id == agent_id && agent.host_id == remote_host_id
        ));
        assert_eq!(service.list_agents().await.len(), 1);

        remote_agent_client
            .rename_agent(wire::RenameAgentRequest {
                agent_id: agent_id.as_bytes().to_vec(),
                name: "subscribed-rename".to_string(),
            })
            .await
            .unwrap();
        assert!(matches!(
            recv_agent_event(&mut events).await,
            AgentEvent::AgentUpdated { agent }
                if agent.id == agent_id && agent.name.as_deref() == Some("subscribed-rename")
        ));

        remote_agent_client
            .delete_agent(wire::DeleteAgentRequest {
                agent_id: agent_id.as_bytes().to_vec(),
            })
            .await
            .unwrap();
        assert!(matches!(
            recv_agent_event(&mut events).await,
            AgentEvent::AgentDown { agent_id: down_id } if down_id == agent_id
        ));
        assert!(service.list_agents().await.is_empty());
    }

    #[tokio::test]
    async fn tonic_client_service_remote_lifecycle_dispatch_requires_reachable_tunnel_route() {
        let service = client_service_for_tests();
        service
            .apply_host_event(HostReachabilityEvent::Added {
                host: host(2, non_relay_types()),
            })
            .await;
        let err = <ClientService as wire::client_service_server::ClientService>::create_agent(
            &service,
            tonic::Request::new(test_agent_create_request(
                Uuid::from_u128(124),
                "remote",
                Some(Uuid::from_u128(2)),
            )),
        )
        .await
        .unwrap_err();

        assert_eq!(err.code(), tonic::Code::Unavailable);
        assert!(err.message().contains("remote dispatch"));

        let remote_agent = agent(125, 2, "remote");
        service
            .apply_agent_event(agent_up(remote_agent.clone()))
            .await;
        let err = <ClientService as wire::client_service_server::ClientService>::send_input(
            &service,
            tonic::Request::new(test_agent_send_input_request(remote_agent.id, b"hello")),
        )
        .await
        .unwrap_err();

        assert_eq!(err.code(), tonic::Code::Unavailable);
        assert!(err.message().contains("remote dispatch"));

        let mut stream =
            <ClientService as wire::client_service_server::ClientService>::subscribe_session(
                &service,
                tonic::Request::new(test_agent_subscribe_session_request(remote_agent.id)),
            )
            .await
            .unwrap()
            .into_inner();
        let response = stream.next().await.unwrap().unwrap();
        let Some(wire::subscribe_session_response::Event::Closed(closed)) = response.event else {
            panic!("expected host-unreachable SessionClosed");
        };
        assert!(matches!(
            closed.reason.unwrap(),
            wire::session_closed::Reason::HostUnreachable(_)
        ));
    }

    #[tokio::test]
    async fn tonic_start_pairing_arms_pin_and_qr_modes_for_local_clients() {
        let data_dir = TempDir::new().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());

        let mut pin_request = tonic::Request::new(wire::StartPairingRequest {
            mode: wire::start_pairing_request::Mode::Pin as i32,
            require_lan_direct: false,
        });
        pin_request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });
        let response =
            <ClientService as wire::client_service_server::ClientService>::start_pairing(
                &service,
                pin_request,
            )
            .await
            .unwrap()
            .into_inner();

        let identity = response.identity.as_ref().unwrap();
        assert_eq!(identity.host_id, local.host_id.as_bytes().to_vec());
        assert_eq!(identity.pubkey, local.public_key());
        assert_eq!(response.ttl_seconds, PAIR_MODE_TTL.as_secs());
        let Some(wire::start_pairing_response::Secret::Pin(pin)) = response.secret else {
            panic!("expected PIN secret");
        };
        assert_eq!(pin.len(), 6);
        assert!(pin.chars().all(|ch| ch.is_ascii_digit()));
        assert!(service.pair_mode.is_active());
        let mut status_request = tonic::Request::new(wire::GetPairingStatusRequest {});
        status_request
            .extensions_mut()
            .insert(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::LocalTrusted,
            });
        let status =
            <ClientService as wire::client_service_server::ClientService>::get_pairing_status(
                &service,
                status_request,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(status.active);

        let mut duplicate_request = tonic::Request::new(wire::StartPairingRequest {
            mode: wire::start_pairing_request::Mode::Qr as i32,
            require_lan_direct: false,
        });
        duplicate_request
            .extensions_mut()
            .insert(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::LocalTrusted,
            });
        let duplicate_error =
            <ClientService as wire::client_service_server::ClientService>::start_pairing(
                &service,
                duplicate_request,
            )
            .await
            .unwrap_err();
        assert_eq!(duplicate_error.code(), tonic::Code::FailedPrecondition);

        let mut cancel_request = tonic::Request::new(wire::CancelPairingRequest {});
        cancel_request
            .extensions_mut()
            .insert(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::LocalTrusted,
            });
        <ClientService as wire::client_service_server::ClientService>::cancel_pairing(
            &service,
            cancel_request,
        )
        .await
        .unwrap();
        assert!(!service.pair_mode.is_active());

        service.server_state.write().await.config.enable_cloud_mode = Some(true);
        let mut qr_request = tonic::Request::new(wire::StartPairingRequest {
            mode: wire::start_pairing_request::Mode::Qr as i32,
            require_lan_direct: false,
        });
        qr_request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });
        let response =
            <ClientService as wire::client_service_server::ClientService>::start_pairing(
                &service, qr_request,
            )
            .await
            .unwrap()
            .into_inner();
        let Some(wire::start_pairing_response::Secret::OneShotToken(token)) = response.secret
        else {
            panic!("expected QR token secret");
        };
        assert_eq!(token.len(), 32);
        assert!(service.pair_mode.is_active());
    }

    #[tokio::test]
    async fn tonic_start_pairing_validates_runtime_config_before_arming() {
        let data_dir = TempDir::new().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());

        let mut qr_request = tonic::Request::new(wire::StartPairingRequest {
            mode: wire::start_pairing_request::Mode::Qr as i32,
            require_lan_direct: false,
        });
        qr_request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });
        let error = <ClientService as wire::client_service_server::ClientService>::start_pairing(
            &service, qr_request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert!(!service.pair_mode.is_active());

        let mut lan_request = tonic::Request::new(wire::StartPairingRequest {
            mode: wire::start_pairing_request::Mode::Pin as i32,
            require_lan_direct: true,
        });
        lan_request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });
        let error = <ClientService as wire::client_service_server::ClientService>::start_pairing(
            &service,
            lan_request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert!(!service.pair_mode.is_active());

        {
            let mut state = service.server_state.write().await;
            state.config.host_name = "x".repeat(MAX_PAIRING_NAME_BYTES + 1);
            state.config.tcp_port = Some(4242);
        }
        let mut bad_name_request = tonic::Request::new(wire::StartPairingRequest {
            mode: wire::start_pairing_request::Mode::Pin as i32,
            require_lan_direct: true,
        });
        bad_name_request
            .extensions_mut()
            .insert(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::LocalTrusted,
            });
        let error = <ClientService as wire::client_service_server::ClientService>::start_pairing(
            &service,
            bad_name_request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(!service.pair_mode.is_active());

        service.server_state.write().await.config.host_name = "ok".to_string();
        let mut lan_request = tonic::Request::new(wire::StartPairingRequest {
            mode: wire::start_pairing_request::Mode::Pin as i32,
            require_lan_direct: true,
        });
        lan_request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });
        let response =
            <ClientService as wire::client_service_server::ClientService>::start_pairing(
                &service,
                lan_request,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.tcp_port, Some(4242));
        assert!(service.pair_mode.is_active());
    }

    #[tokio::test]
    async fn tonic_pairing_admin_rpcs_reject_paired_remote_callers() {
        let data_dir = TempDir::new().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let remote = Uuid::from_u128(2);
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());

        let mut start_request = tonic::Request::new(wire::StartPairingRequest {
            mode: wire::start_pairing_request::Mode::Pin as i32,
            require_lan_direct: false,
        });
        start_request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::TlsTrusted { peer: remote },
        });
        let error = <ClientService as wire::client_service_server::ClientService>::start_pairing(
            &service,
            start_request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::PermissionDenied);

        let mut status_request = tonic::Request::new(wire::GetPairingStatusRequest {});
        status_request
            .extensions_mut()
            .insert(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::TlsTrusted { peer: remote },
            });
        let error =
            <ClientService as wire::client_service_server::ClientService>::get_pairing_status(
                &service,
                status_request,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::PermissionDenied);

        let mut cancel_request = tonic::Request::new(wire::CancelPairingRequest {});
        cancel_request
            .extensions_mut()
            .insert(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::TlsTrusted { peer: remote },
            });
        let error = <ClientService as wire::client_service_server::ClientService>::cancel_pairing(
            &service,
            cancel_request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::PermissionDenied);

        let mut direct_request = tonic::Request::new(wire::PairPeerRequest {
            peer: Some(wire::PairingIdentity {
                host_id: Uuid::from_u128(2).as_bytes().to_vec(),
                pubkey: vec![7; 32],
                name: "remote".to_string(),
            }),
            reachability: Some(wire::pair_peer_request::Reachability::DirectTcpAddr(
                "127.0.0.1:4242".to_string(),
            )),
        });
        direct_request
            .extensions_mut()
            .insert(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::TlsTrusted { peer: remote },
            });
        let error = <ClientService as wire::client_service_server::ClientService>::pair_peer(
            &service,
            direct_request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::PermissionDenied);

        let mut cloud_request = tonic::Request::new(wire::PairPinCloudPeerRequest {
            host_id: Uuid::from_u128(2).as_bytes().to_vec(),
            pin: "123456".to_string(),
        });
        cloud_request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::TlsTrusted { peer: remote },
        });
        let error =
            <ClientService as wire::client_service_server::ClientService>::pair_pin_cloud_peer(
                &service,
                cloud_request,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::PermissionDenied);

        let mut qr_cloud_request = tonic::Request::new(wire::PairQrCloudPeerRequest {
            host_id: Uuid::from_u128(2).as_bytes().to_vec(),
            pubkey: vec![7; 32],
            one_shot_token: vec![8; 32],
        });
        qr_cloud_request
            .extensions_mut()
            .insert(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::TlsTrusted { peer: remote },
            });
        let error =
            <ClientService as wire::client_service_server::ClientService>::pair_qr_cloud_peer(
                &service,
                qr_cloud_request,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::PermissionDenied);

        let mut list_peers_request = tonic::Request::new(wire::ListPeersRequest {});
        list_peers_request
            .extensions_mut()
            .insert(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::TlsTrusted { peer: remote },
            });
        let error = <ClientService as wire::client_service_server::ClientService>::list_peers(
            &service,
            list_peers_request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::PermissionDenied);

        let peer_ref = wire::PeerRef {
            identifier: Some(wire::peer_ref::Identifier::HostId(
                remote.as_bytes().to_vec(),
            )),
        };
        let mut get_peer_request = tonic::Request::new(wire::GetPeerRequest {
            peer: Some(peer_ref.clone()),
        });
        get_peer_request
            .extensions_mut()
            .insert(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::TlsTrusted { peer: remote },
            });
        let error = <ClientService as wire::client_service_server::ClientService>::get_peer(
            &service,
            get_peer_request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::PermissionDenied);

        let mut unpair_request = tonic::Request::new(wire::UnpairRequest {
            peer: Some(peer_ref),
            reason: "test".to_string(),
        });
        unpair_request
            .extensions_mut()
            .insert(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::TlsTrusted { peer: remote },
            });
        let error = <ClientService as wire::client_service_server::ClientService>::unpair(
            &service,
            unpair_request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::PermissionDenied);
        assert!(!service.pair_mode.is_active());
    }

    #[tokio::test]
    async fn tonic_pair_pin_cloud_peer_rejects_self_host_id() {
        let data_dir = TempDir::new().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());

        let mut request = tonic::Request::new(wire::PairPinCloudPeerRequest {
            host_id: local.host_id.as_bytes().to_vec(),
            pin: "123456".to_string(),
        });
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });

        let error =
            <ClientService as wire::client_service_server::ClientService>::pair_pin_cloud_peer(
                &service, request,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(error.message(), "SELF_PAIRING");
        assert!(trust_store.read().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tonic_pair_qr_cloud_peer_rejects_self_identity_before_dialing() {
        let data_dir = TempDir::new().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());

        let mut request = tonic::Request::new(wire::PairQrCloudPeerRequest {
            host_id: local.host_id.as_bytes().to_vec(),
            pubkey: vec![7; 32],
            one_shot_token: vec![8; 32],
        });
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });
        let error =
            <ClientService as wire::client_service_server::ClientService>::pair_qr_cloud_peer(
                &service, request,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(error.message(), "SELF_PAIRING");

        let mut request = tonic::Request::new(wire::PairQrCloudPeerRequest {
            host_id: Uuid::from_u128(2).as_bytes().to_vec(),
            pubkey: local.public_key().to_vec(),
            one_shot_token: vec![8; 32],
        });
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });
        let error =
            <ClientService as wire::client_service_server::ClientService>::pair_qr_cloud_peer(
                &service, request,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(error.message(), "SELF_PAIRING");
        assert!(trust_store.read().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tonic_pair_qr_cloud_peer_preflights_token_length_and_duplicate_pubkey() {
        let data_dir = TempDir::new().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let duplicate = DeviceIdentity::for_test(Uuid::from_u128(2));
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        {
            let mut store = trust_store.write().unwrap();
            store.insert_for_test(
                duplicate.host_id,
                TrustEntry {
                    pubkey: duplicate.public_key().to_vec(),
                    name: "duplicate".to_string(),
                    paired_at: Utc::now(),
                    reachabilities: vec![Reachability::Cloud],
                },
            );
        }
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());

        let mut short_token = tonic::Request::new(wire::PairQrCloudPeerRequest {
            host_id: Uuid::from_u128(3).as_bytes().to_vec(),
            pubkey: vec![7; 32],
            one_shot_token: vec![8; 31],
        });
        short_token.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });
        let error =
            <ClientService as wire::client_service_server::ClientService>::pair_qr_cloud_peer(
                &service,
                short_token,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            error.message(),
            "PairQrCloudPeerRequest.one_shot_token must be 32 bytes"
        );

        let mut duplicate_pubkey = tonic::Request::new(wire::PairQrCloudPeerRequest {
            host_id: Uuid::from_u128(3).as_bytes().to_vec(),
            pubkey: duplicate.public_key().to_vec(),
            one_shot_token: vec![8; 32],
        });
        duplicate_pubkey
            .extensions_mut()
            .insert(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::LocalTrusted,
            });
        let error =
            <ClientService as wire::client_service_server::ClientService>::pair_qr_cloud_peer(
                &service,
                duplicate_pubkey,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            error.message(),
            "PROTOCOL_VIOLATION: pubkey is already trusted"
        );
    }

    #[tokio::test]
    async fn tonic_pair_ssh_peer_updates_live_and_persisted_trust() {
        let data_dir = TempDir::new().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let peer = DeviceIdentity::for_test(Uuid::from_u128(2));
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());

        let mut request = tonic::Request::new(wire::PairPeerRequest {
            peer: Some(wire::PairingIdentity {
                host_id: peer.host_id.as_bytes().to_vec(),
                pubkey: peer.public_key().to_vec(),
                name: "workstation".to_string(),
            }),
            reachability: Some(wire::pair_peer_request::Reachability::SshTarget(
                "workstation".to_string(),
            )),
        });
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });
        <ClientService as wire::client_service_server::ClientService>::pair_peer(&service, request)
            .await
            .unwrap();

        let expected = Reachability::Ssh {
            target: "workstation".to_string(),
        };
        let live = trust_store.read().unwrap();
        let live_entry = live.entry(peer.host_id).unwrap();
        assert_eq!(live_entry.pubkey.as_slice(), peer.public_key());
        assert_eq!(live_entry.reachabilities, vec![expected.clone()]);
        drop(live);

        let persisted = TrustStore::load_or_create_in(data_dir.path()).unwrap();
        let persisted_entry = persisted.entry(peer.host_id).unwrap();
        assert_eq!(persisted_entry.pubkey.as_slice(), peer.public_key());
        assert_eq!(persisted_entry.reachabilities, vec![expected]);
    }

    #[tokio::test]
    async fn tonic_pair_direct_peer_updates_live_and_persisted_trust() {
        let data_dir = TempDir::new().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let peer = DeviceIdentity::for_test(Uuid::from_u128(2));
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());
        let addr = SocketAddr::from(([127, 0, 0, 1], 4242));

        let mut request = tonic::Request::new(wire::PairPeerRequest {
            peer: Some(wire::PairingIdentity {
                host_id: peer.host_id.as_bytes().to_vec(),
                pubkey: peer.public_key().to_vec(),
                name: "phone".to_string(),
            }),
            reachability: Some(wire::pair_peer_request::Reachability::DirectTcpAddr(
                addr.to_string(),
            )),
        });
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });
        <ClientService as wire::client_service_server::ClientService>::pair_peer(&service, request)
            .await
            .unwrap();

        let expected = Reachability::DirectTcp { addr };
        let live = trust_store.read().unwrap();
        let live_entry = live.entry(peer.host_id).unwrap();
        assert_eq!(live_entry.pubkey.as_slice(), peer.public_key());
        assert_eq!(live_entry.reachabilities, vec![expected.clone()]);
        drop(live);

        let persisted = TrustStore::load_or_create_in(data_dir.path()).unwrap();
        let persisted_entry = persisted.entry(peer.host_id).unwrap();
        assert_eq!(persisted_entry.pubkey.as_slice(), peer.public_key());
        assert_eq!(persisted_entry.reachabilities, vec![expected]);
    }

    #[tokio::test]
    async fn tonic_unpair_removes_trust_sends_link_close_and_tears_down_routes() {
        let data_dir = TempDir::new().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let peer = DeviceIdentity::for_test(Uuid::from_u128(2));
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let paired_at = Utc.timestamp_millis_opt(200_000).single().unwrap();
        {
            let mut store = trust_store.write().unwrap();
            store
                .upsert_paired_peer(
                    peer.host_id,
                    peer.public_key().to_vec(),
                    "phone".to_string(),
                    Reachability::Cloud,
                    paired_at,
                )
                .unwrap();
            store.save_in(data_dir.path()).unwrap();
        }

        let agent_state = Arc::new(RwLock::new(AgentServiceState::new()));
        let agent_service = AgentServiceCtx::new(agent_state, local.host_id, false);
        let (shutdown_tx, _shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
        let server_state = Arc::new(RwLock::new(ServerState::new(
            Config::default(),
            local.host_id,
            shutdown_tx,
            None,
            None,
        )));
        let (routing, tunnels) = test_routing_and_tunnels(local.host_id);
        let service = ClientService::new(
            agent_service,
            server_state,
            Arc::new(ConnectionManager::new(routing.clone(), tunnels.clone())),
            PairingTrustAccess::new(
                local.public_key().to_vec(),
                trust_store.clone(),
                Arc::new(tokio::sync::Mutex::new(())),
                data_dir.path().to_path_buf(),
            ),
            Arc::new(PairMode::new()),
            ReachabilityLinkConnector::disabled(),
        );
        let link = LinkId::new(peer.host_id);
        let (tx, mut rx) = mpsc::channel(8);
        let mut close_rx = tunnels
            .link_registry()
            .register(link, host(2, non_relay_types()), tx, LinkRole::Peer, &[])
            .await;
        let link_registry = tunnels.link_registry();
        tokio::spawn(async move {
            assert_eq!(close_rx.recv().await, Some(LinkCloseRequest::TrustReplaced));
            link_registry.remove(&link).await;
        });
        routing
            .apply_direct_up(host(2, non_relay_types()), link)
            .await;
        service
            .remote_agent_connections
            .route_runtime()
            .register_direct(
                link,
                Endpoint::from_static("http://example.com").connect_lazy(),
            )
            .await;
        assert_eq!(
            service
                .remote_agent_connections
                .known_routes(peer.host_id)
                .await,
            vec![crate::routing::Route::Direct(link)]
        );

        let response = <ClientService as wire::client_service_server::ClientService>::unpair(
            &service,
            local_client_request(wire::UnpairRequest {
                peer: Some(wire::PeerRef {
                    identifier: Some(wire::peer_ref::Identifier::Name("phone".to_string())),
                }),
                reason: "test".to_string(),
            }),
        )
        .await
        .unwrap()
        .into_inner();

        let removed = response.removed_peer.unwrap();
        assert_eq!(removed.host_id, peer.host_id.as_bytes().to_vec());
        assert!(trust_store.read().unwrap().entry(peer.host_id).is_none());
        assert!(
            TrustStore::load_or_create_in(data_dir.path())
                .unwrap()
                .entry(peer.host_id)
                .is_none()
        );
        let Some(wire::pb::Message {
            body: Some(wire::pb::message::Body::LinkClose(close)),
        }) = rx.recv().await
        else {
            panic!("expected user-revoked LinkClose");
        };
        assert_eq!(close.reason, wire::pb::LinkCloseReason::UserRevoked as i32);
        assert!(
            service
                .remote_agent_connections
                .known_routes(peer.host_id)
                .await
                .is_empty()
        );
        assert!(routing.route_to(peer.host_id).await.is_none());
        assert_eq!(service.remote_agent_connections.pool().len().await, 0);
    }

    #[tokio::test]
    async fn tonic_unpair_list_and_get_peers_are_local_admin() {
        let data_dir = TempDir::new().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let peer = DeviceIdentity::for_test(Uuid::from_u128(2));
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        trust_store
            .write()
            .unwrap()
            .upsert_paired_peer(
                peer.host_id,
                peer.public_key().to_vec(),
                "phone".to_string(),
                Reachability::Ssh {
                    target: "phone".to_string(),
                },
                Utc.timestamp_millis_opt(200_000).single().unwrap(),
            )
            .unwrap();
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());

        let peers = <ClientService as wire::client_service_server::ClientService>::list_peers(
            &service,
            local_client_request(wire::ListPeersRequest {}),
        )
        .await
        .unwrap()
        .into_inner()
        .peers;
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].name, "phone");

        let peer_response =
            <ClientService as wire::client_service_server::ClientService>::get_peer(
                &service,
                local_client_request(wire::GetPeerRequest {
                    peer: Some(wire::PeerRef {
                        identifier: Some(wire::peer_ref::Identifier::HostId(
                            peer.host_id.as_bytes().to_vec(),
                        )),
                    }),
                }),
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(peer_response.peer.unwrap().name, "phone");
    }

    #[tokio::test]
    async fn tonic_pair_ssh_peer_rejects_self_pairing_pubkey() {
        let data_dir = TempDir::new().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());

        let mut request = tonic::Request::new(wire::PairPeerRequest {
            peer: Some(wire::PairingIdentity {
                host_id: Uuid::from_u128(2).as_bytes().to_vec(),
                pubkey: local.public_key().to_vec(),
                name: "self-key".to_string(),
            }),
            reachability: None,
        });
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::LocalTrusted,
        });
        let error = <ClientService as wire::client_service_server::ClientService>::pair_peer(
            &service, request,
        )
        .await
        .unwrap_err();

        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(error.message(), "SELF_PAIRING");
    }

    #[tokio::test]
    async fn tonic_pair_ssh_peer_rejects_paired_remote_callers() {
        let data_dir = TempDir::new().unwrap();
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let peer = DeviceIdentity::for_test(Uuid::from_u128(2));
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let service =
            client_service_with_pairing_trust(data_dir.path(), &local, trust_store.clone());
        let mut request = tonic::Request::new(wire::PairPeerRequest {
            peer: Some(wire::PairingIdentity {
                host_id: peer.host_id.as_bytes().to_vec(),
                pubkey: peer.public_key().to_vec(),
                name: "remote".to_string(),
            }),
            reachability: None,
        });
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::TlsTrusted { peer: peer.host_id },
        });

        let error = <ClientService as wire::client_service_server::ClientService>::pair_peer(
            &service, request,
        )
        .await
        .unwrap_err();

        assert_eq!(error.code(), tonic::Code::PermissionDenied);
        assert!(trust_store.read().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tonic_client_service_handles_debug_and_hooks() {
        let service = client_service_with_local_services();

        let debug = <ClientService as wire::client_service_server::ClientService>::debug(
            &service,
            tonic::Request::new(wire::DebugRequest {
                verbose: false,
                format: wire::DebugFormat::Json as i32,
            }),
        )
        .await
        .unwrap()
        .into_inner();
        assert!(debug.dump.contains("is_cloud_server"));

        let debug_error = <ClientService as wire::client_service_server::ClientService>::debug(
            &service,
            tonic::Request::new(wire::DebugRequest {
                verbose: false,
                format: 99,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(debug_error.code(), tonic::Code::InvalidArgument);
        assert!(debug_error.message().contains("unknown value 99"));

        let missing_debug_format =
            <ClientService as wire::client_service_server::ClientService>::debug(
                &service,
                tonic::Request::new(wire::DebugRequest {
                    verbose: false,
                    format: wire::DebugFormat::Unspecified as i32,
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(missing_debug_format.code(), tonic::Code::InvalidArgument);
        assert!(
            missing_debug_format
                .message()
                .contains("format is required")
        );

        let hook_error =
            <ClientService as wire::client_service_server::ClientService>::handle_hook(
                &service,
                tonic::Request::new(wire::HandleHookRequest {
                    agent_id: Uuid::from_u128(999).as_bytes().to_vec(),
                    payload: Vec::new(),
                    external: false,
                }),
            )
            .await
            .unwrap_err();

        assert_eq!(hook_error.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn tonic_client_service_handles_server_lifecycle_methods() {
        let (service, mut shutdown_rx) = client_service_with_admin_shutdown_rx();

        let shutdown_task = tokio::spawn(async move {
            <ClientService as wire::client_service_server::ClientService>::shutdown(
                &service,
                tonic::Request::new(wire::ShutdownRequest {}),
            )
            .await
            .unwrap()
            .into_inner()
        });
        let Some(ShutdownRequest::Shutdown { reply }) = shutdown_rx.recv().await else {
            panic!("expected shutdown request");
        };
        reply.send(Ok(())).unwrap();
        shutdown_task.await.unwrap();

        let (service, mut shutdown_rx) = client_service_with_admin_shutdown_rx();
        let suspend_task = tokio::spawn(async move {
            <ClientService as wire::client_service_server::ClientService>::suspend(
                &service,
                tonic::Request::new(wire::SuspendRequest {
                    reason: wire::SuspendReason::User as i32,
                }),
            )
            .await
            .unwrap()
            .into_inner()
        });
        let Some(ShutdownRequest::Suspend { reason, reply }) = shutdown_rx.recv().await else {
            panic!("expected suspend request");
        };
        assert_eq!(reason, ShutdownReason::Suspending);
        reply.send(Ok(4)).unwrap();
        let response = suspend_task.await.unwrap();
        assert_eq!(response.suspended_count, 4);

        let (service, _shutdown_rx) = client_service_with_admin_shutdown_rx();
        let missing_reason =
            <ClientService as wire::client_service_server::ClientService>::suspend(
                &service,
                tonic::Request::new(wire::SuspendRequest {
                    reason: wire::SuspendReason::Unspecified as i32,
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(missing_reason.code(), tonic::Code::InvalidArgument);
        assert!(missing_reason.message().contains("reason is required"));
    }

    #[tokio::test]
    async fn tonic_client_service_resume_keeps_failed_suspended_agents_on_disk() {
        let temp = TempDir::new().unwrap();
        let state_path = temp.path().join("state.yaml");
        let config = Config {
            state_path: state_path.clone(),
            ..Config::default()
        };

        let host_id = Uuid::from_u128(1);
        let agent_state = Arc::new(RwLock::new(AgentServiceState::new()));
        let agent_service = AgentServiceCtx::new(agent_state, host_id, false);
        let (shutdown_tx, _shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
        let server_state = Arc::new(RwLock::new(ServerState::new(
            config,
            host_id,
            shutdown_tx,
            None,
            None,
        )));
        let (routing, tunnels) = test_routing_and_tunnels(host_id);
        let service = client_service_from_parts(agent_service, server_state, routing, tunnels);
        let suspended = crate::suspend::SuspendedAgent::TestAgent {
            agent_id: Uuid::new_v4(),
            name: Some("will-fail".to_string()),
            command: "definitely-not-an-amux-test-agent-command".to_string(),
            working_dir: std::env::temp_dir(),
            terminal_size: None,
            created_at: Utc::now(),
        };
        crate::suspend::save_suspended(
            &state_path,
            &crate::suspend::SuspendedServerState {
                agents: vec![suspended],
            },
        )
        .unwrap();

        let response = <ClientService as wire::client_service_server::ClientService>::resume(
            &service,
            tonic::Request::new(wire::ResumeRequest {}),
        )
        .await
        .unwrap()
        .into_inner();

        assert_eq!(response.resumed_count, 0);
        assert_eq!(response.failed_count, 1);
        assert_eq!(
            crate::suspend::load_suspended(&state_path)
                .unwrap()
                .agents
                .len(),
            1
        );
    }
}

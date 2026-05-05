use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{RwLock, mpsc, watch};
use uuid::Uuid;

use crate::agent::{Agent, AgentSession};
use crate::auth::jwt::JwtValidator;
use crate::config::Config;
use crate::protocol::link::Link;
use crate::protocol::message::{Message, RoutedCallId};
use crate::server::RpcDispatcher;
use crate::server::routing::Topology;

pub(in crate::server) const LOCAL_USER_ID: Uuid = Uuid::nil();

/// Request from a connection handler to shut down or suspend the server.
pub(in crate::server) enum ShutdownRequest {
    Shutdown {
        reply: mpsc::Sender<Message>,
        reply_call_id: RoutedCallId,
        link: Link,
    },
    Suspend {
        reply: mpsc::Sender<Message>,
        reply_call_id: RoutedCallId,
        link: Link,
        reason: crate::protocol::message::ShutdownReason,
    },
}

#[derive(Clone)]
pub(in crate::server) struct ConnectionHandle {
    tx: mpsc::Sender<Message>,
    close_tx: watch::Sender<Option<String>>,
    close_rx: watch::Receiver<Option<String>>,
}

impl ConnectionHandle {
    pub(in crate::server) fn new(tx: mpsc::Sender<Message>) -> Self {
        let (close_tx, close_rx) = watch::channel(None);
        Self {
            tx,
            close_tx,
            close_rx,
        }
    }

    pub(in crate::server) fn sender(&self) -> mpsc::Sender<Message> {
        self.tx.clone()
    }

    pub(in crate::server) fn close_receiver(&self) -> watch::Receiver<Option<String>> {
        self.close_rx.clone()
    }

    pub(in crate::server) fn request_close(&self, reason: impl Into<String>) {
        let _ = self.close_tx.send(Some(reason.into()));
    }

    pub(in crate::server) async fn send(
        &self,
        msg: Message,
    ) -> std::result::Result<(), mpsc::error::SendError<Message>> {
        self.tx.send(msg).await
    }

    pub(in crate::server) fn try_send(&self, msg: Message) -> bool {
        self.tx.try_send(msg).is_ok()
    }

    pub(in crate::server) fn try_send_or_close(
        &self,
        msg: Message,
        close_reason: impl Into<String>,
    ) -> bool {
        match self.tx.try_send(msg) {
            Ok(()) => true,
            Err(_) => {
                self.request_close(close_reason);
                false
            }
        }
    }
}

pub(crate) struct ServerUserState {
    pub(crate) rpc: RpcDispatcher,
    pub(in crate::server) topology: Topology,
    pub(crate) agents: HashMap<Uuid, AgentSession>,
}

impl ServerUserState {
    pub(in crate::server) fn new() -> Self {
        Self {
            rpc: RpcDispatcher::new(),
            topology: Topology::new(),
            agents: HashMap::new(),
        }
    }

    pub(crate) fn is_peer_link(&self, link: &Link) -> bool {
        self.topology.peer_links.contains(link)
    }

    /// Atomically register a connection under `link`, returning the
    /// handle and the receive half of its outgoing channel.
    ///
    /// Fails with `Err(link)` when the link is already in use — the
    /// original name is returned so the caller can reply to the peer without
    /// reconstructing it.
    pub(in crate::server) fn try_reserve_link(
        &mut self,
        link: Link,
    ) -> Result<(ConnectionHandle, mpsc::Receiver<Message>), Link> {
        self.topology.try_reserve_link(link)
    }

    pub(crate) fn list_agents(&self) -> Vec<crate::protocol::Agent> {
        self.topology
            .registry
            .list_all(&self.topology.hosts)
            .into_iter()
            .map(Into::into)
            .collect()
    }

    pub(crate) fn resolve_agent(&self, identifier: &str) -> Option<crate::protocol::Agent> {
        self.topology
            .registry
            .resolve(&self.topology.hosts, identifier)
            .map(Into::into)
    }

    pub(crate) fn agent_session_mut(&mut self, agent_id: &Uuid) -> Option<&mut AgentSession> {
        self.agents.get_mut(agent_id)
    }

    pub(crate) fn insert_registered_local_agent(
        &mut self,
        agent_id: Uuid,
        session: AgentSession,
        info: Agent,
    ) -> Result<crate::server::routing::TopologyEvent, String> {
        self.agents.insert(agent_id, session);
        match self.topology.register_local_agent(info) {
            Ok(event) => Ok(event),
            Err(error) => {
                self.agents.remove(&agent_id);
                Err(error.to_string())
            }
        }
    }
}

pub(crate) struct ServerState {
    pub(in crate::server) config: Config,
    pub(in crate::server) host_id: Uuid,
    pub(in crate::server) is_cloud_server: bool,
    pub(in crate::server) jwt_validator: Option<Arc<JwtValidator>>,
    pub(in crate::server) users: HashMap<Uuid, Arc<RwLock<ServerUserState>>>,
    pub(in crate::server) shutdown_tx: mpsc::Sender<ShutdownRequest>,
}

impl ServerState {
    pub(in crate::server) fn new(
        config: Config,
        host_id: Uuid,
        shutdown_tx: mpsc::Sender<ShutdownRequest>,
    ) -> Self {
        let mut users = HashMap::new();
        users.insert(LOCAL_USER_ID, Arc::new(RwLock::new(ServerUserState::new())));
        Self {
            config,
            host_id,
            is_cloud_server: false,
            jwt_validator: None,
            users,
            shutdown_tx,
        }
    }

    pub(in crate::server) fn user_state(
        &self,
        user_id: &Uuid,
    ) -> Option<Arc<RwLock<ServerUserState>>> {
        self.users.get(user_id).cloned()
    }

    pub(crate) fn host_id(&self) -> Uuid {
        self.host_id
    }

    pub(crate) fn host_name(&self) -> &str {
        &self.config.host_name
    }

    pub(crate) fn is_cloud_server(&self) -> bool {
        self.is_cloud_server
    }
}

pub(in crate::server) async fn ensure_user_state(
    state: &Arc<RwLock<ServerState>>,
    user_id: Uuid,
) -> Arc<RwLock<ServerUserState>> {
    {
        let s = state.read().await;
        if let Some(us) = s.users.get(&user_id) {
            return us.clone();
        }
    }

    let mut s = state.write().await;
    s.users
        .entry(user_id)
        .or_insert_with(|| Arc::new(RwLock::new(ServerUserState::new())))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Route, method};
    use crate::rpc::{OutboundCallState, RpcLocalOriginOutboundStart, RpcRoutedUnaryStart};

    #[test]
    fn connection_handle_retains_close_request_before_subscription() {
        let (tx, _rx) = mpsc::channel(1);
        let handle = ConnectionHandle::new(tx);

        handle.request_close("closing");

        let receiver = handle.close_receiver();
        assert_eq!(receiver.borrow().as_deref(), Some("closing"));
    }

    #[test]
    fn finishing_inbound_peer_routing_subscription_does_not_remove_unrelated_inbound_call() {
        let mut user_state = ServerUserState::new();
        let link = Link::new("peer-a").unwrap();
        let route = Route::from_link(link.clone());
        let call_id = RoutedCallId::from(Uuid::new_v4());
        user_state.topology.mark_peer_link(link.clone());
        let (tx, _rx) = mpsc::channel(1);
        user_state
            .rpc
            .register_routed_unary(RpcRoutedUnaryStart {
                tx,
                owner_link: link.clone(),
                reply_src: Route::empty(),
                reply_dst: route.clone(),
                counterparty_route: route.clone(),
                call_id: call_id.clone(),
                method: method::AGENT_CREATE,
            })
            .unwrap();

        assert!(
            !user_state
                .rpc
                .finish_inbound_peer_routing_subscription(&link, &call_id)
        );

        assert!(matches!(
            user_state
                .rpc
                .inbound_for_route(&route, &call_id)
                .map(|call| call.method),
            Some(method::AGENT_CREATE)
        ));
    }

    #[test]
    fn finishing_outbound_peer_routing_subscription_does_not_remove_unrelated_outbound_call() {
        let mut user_state = ServerUserState::new();
        let link = Link::new("peer-a").unwrap();
        let route = Route::from_link(link.clone());
        let call_id = RoutedCallId::from(Uuid::new_v4());
        user_state.topology.mark_peer_link(link.clone());
        user_state
            .rpc
            .register_local_origin_outbound(RpcLocalOriginOutboundStart {
                counterparty_route: route.clone(),
                call_id: call_id.clone(),
                method: method::AGENT_CREATE,
                state: OutboundCallState::AwaitingResponse,
                owner_link: Link::new("local").unwrap(),
                request_src: Route::from_link(Link::new("local").unwrap()),
                request_dst: route.clone(),
            })
            .unwrap();

        assert!(
            !user_state
                .rpc
                .finish_outbound_peer_routing_subscription(&link, &call_id)
        );

        assert!(matches!(
            user_state
                .rpc
                .outbound_for_route(&route, &call_id)
                .map(|call| call.method),
            Some(method::AGENT_CREATE)
        ));
    }
}

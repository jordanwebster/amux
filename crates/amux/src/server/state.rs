use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{RwLock, mpsc, watch};
use uuid::Uuid;

use crate::agent::{Agent, AgentSession};
use crate::auth::jwt::JwtValidator;
use crate::config::Config;
use crate::protocol::link::Link;
use crate::protocol::message::{CallId, Host, Message};
use crate::protocol::method::MethodSpec;
use crate::protocol::route::Route;
use crate::rpc::{InboundCall, OutboundCall};
use crate::server::routing::{
    LinkClosedChange, PeerAgentDownChange, PeerAgentDownIgnored, PeerAgentUpChange,
    PeerAgentUpIgnored, PeerHostDownChange, PeerHostUpChange, TopologyEvent,
};
use crate::server::{ActiveEndpointStreamSink, LocalOriginOutboundCall, RpcDispatcher};

pub(in crate::server) const LOCAL_USER_ID: Uuid = Uuid::nil();
pub(in crate::server) const OUTGOING_MESSAGE_BUFFER: usize = 2048;

/// Request from a connection handler to shut down or suspend the server.
pub(in crate::server) enum ShutdownRequest {
    Shutdown {
        reply: mpsc::Sender<Message>,
        reply_call_id: CallId,
        link: Link,
    },
    Suspend {
        reply: mpsc::Sender<Message>,
        reply_call_id: CallId,
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
    pub(in crate::server) connections: HashMap<Link, ConnectionEntry>,
    pub(in crate::server) routes: HashMap<Route, RouteContext>,
    pub(in crate::server) hosts: HashMap<Uuid, HostContext>,
    pub(crate) session_subscriptions: HashMap<CallId, SessionSubscriptionState>,
    remote_name_owners: HashMap<String, Uuid>,
    pub(crate) local_agents: HashMap<Uuid, LocalAgentContext>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server) enum ConnectionKind {
    LocalClient,
    Peer,
}

pub(in crate::server) struct ConnectionEntry {
    pub(in crate::server) handle: ConnectionHandle,
    pub(in crate::server) kind: ConnectionKind,
    pub(in crate::server) rpc: RpcDispatcher,
}

impl ConnectionEntry {
    pub(in crate::server) fn new(handle: ConnectionHandle, kind: ConnectionKind) -> Self {
        Self {
            handle,
            kind,
            rpc: RpcDispatcher::new(),
        }
    }

    pub(in crate::server) fn handle(&self) -> ConnectionHandle {
        self.handle.clone()
    }

    pub(in crate::server) fn rpc(&self) -> RpcDispatcher {
        self.rpc.clone()
    }

    pub(in crate::server) fn is_peer(&self) -> bool {
        self.kind == ConnectionKind::Peer
    }
}

pub(crate) struct RouteContext {
    pub(in crate::server) host_id: Uuid,
    pub(in crate::server) rpc: RpcDispatcher,
}

impl RouteContext {
    pub(in crate::server) fn new(host_id: Uuid) -> Self {
        Self {
            host_id,
            rpc: RpcDispatcher::new(),
        }
    }

    pub(in crate::server) fn rpc(&self) -> RpcDispatcher {
        self.rpc.clone()
    }
}

pub(crate) struct HostContext {
    pub(in crate::server) host: Host,
    pub(in crate::server) agents: HashMap<Uuid, Agent>,
    pub(in crate::server) agent_subscription: Option<AgentSubscriptionState>,
}

impl HostContext {
    pub(in crate::server) fn new(host: Host) -> Self {
        Self {
            host,
            agents: HashMap::new(),
            agent_subscription: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::server) struct AgentSubscriptionState {
    pub(in crate::server) route: Route,
    pub(in crate::server) call_id: CallId,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionSubscriptionState {
    pub(crate) agent_id: Uuid,
    pub(crate) counterparty: Route,
}

pub(crate) struct LocalAgentContext {
    pub(crate) session: AgentSession,
    pub(crate) info: Agent,
}

impl ServerUserState {
    pub(in crate::server) fn new() -> Self {
        Self {
            connections: HashMap::new(),
            routes: HashMap::new(),
            hosts: HashMap::new(),
            session_subscriptions: HashMap::new(),
            remote_name_owners: HashMap::new(),
            local_agents: HashMap::new(),
        }
    }

    pub(in crate::server) fn rpc_for_link(&self, link: &Link) -> Option<RpcDispatcher> {
        self.connections.get(link).map(ConnectionEntry::rpc)
    }

    pub(in crate::server) fn host_for_link(&self, link: &Link) -> Option<&Host> {
        let route = Route::from_link(link.clone());
        let host_id = self.routes.get(&route)?.host_id;
        self.hosts.get(&host_id).map(|context| &context.host)
    }

    #[cfg(test)]
    pub(crate) fn route_rpc(&self, route: &Route) -> Option<RpcDispatcher> {
        self.routes.get(route).map(RouteContext::rpc)
    }

    pub(crate) fn route_rpc_for_counterparty(&self, route: &Route) -> Option<RpcDispatcher> {
        self.route_context_for_counterparty(route)
            .map(RouteContext::rpc)
    }

    pub(crate) fn rpc_for_inbound_call(&self, call_id: &CallId) -> Option<RpcDispatcher> {
        self.rpc_contexts_sorted().into_iter().find_map(|(_, rpc)| {
            rpc.inbound_call_target_for_call(call_id)
                .is_some()
                .then_some(rpc)
        })
    }

    pub(crate) fn rpc_for_outbound_route(&self, route: &Route) -> Option<RpcDispatcher> {
        self.route_rpc_for_counterparty(route).or_else(|| {
            route
                .peek()
                .and_then(|first_hop| self.rpc_for_link(first_hop))
        })
    }

    fn route_context_for_counterparty(&self, route: &Route) -> Option<&RouteContext> {
        if let Some(context) = self.routes.get(route) {
            return Some(context);
        }

        self.routes
            .iter()
            .filter(|(prefix, _)| !prefix.is_empty() && route.starts_with_route(prefix))
            .max_by(|(route_a, _), (route_b, _)| {
                route_a
                    .len()
                    .cmp(&route_b.len())
                    .then_with(|| route_b.to_string().cmp(&route_a.to_string()))
            })
            .map(|(_, context)| context)
    }

    #[cfg(test)]
    pub(crate) fn test_rpc(&self) -> RpcDispatcher {
        self.route_rpc(&Route::empty())
            .expect("test user state should initialize an empty-route RPC context")
    }

    #[cfg(test)]
    pub(crate) fn ensure_route_rpc(&mut self, route: Route) -> RpcDispatcher {
        self.routes
            .entry(route)
            .or_insert_with(|| RouteContext::new(Uuid::nil()))
            .rpc()
    }

    pub(crate) fn is_peer_link(&self, link: &Link) -> bool {
        self.connections
            .get(link)
            .is_some_and(ConnectionEntry::is_peer)
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
        if self.connections.contains_key(&link) {
            return Err(link);
        }
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(OUTGOING_MESSAGE_BUFFER);
        let handle = ConnectionHandle::new(outgoing_tx);
        self.connections.insert(
            link.clone(),
            ConnectionEntry::new(handle.clone(), ConnectionKind::LocalClient),
        );
        Ok((handle, outgoing_rx))
    }

    pub(in crate::server) fn mark_peer_link(&mut self, link: Link) {
        if let Some(connection) = self.connections.get_mut(&link) {
            connection.kind = ConnectionKind::Peer;
        }
    }

    pub(in crate::server) fn remove_link(&mut self, link: &Link) {
        self.connections.remove(link);
        self.remove_route_subtree(&Route::from_link(link.clone()));
    }

    pub(in crate::server) fn route(&self, link: &Link) -> Option<ConnectionHandle> {
        self.connections.get(link).map(ConnectionEntry::handle)
    }

    pub(in crate::server) fn connection_for_route(
        &self,
        route: &Route,
    ) -> Option<ConnectionHandle> {
        let first_hop = route.peek()?;
        self.route(first_hop)
    }

    pub(in crate::server) fn send_via_route(
        &self,
        route: &Route,
        msg: Message,
        close_reason: impl Into<String>,
    ) -> bool {
        self.connection_for_route(route)
            .is_some_and(|handle| handle.try_send_or_close(msg, close_reason))
    }

    pub(in crate::server) fn peer_links(&self) -> Vec<Link> {
        let mut links: Vec<_> = self
            .connections
            .iter()
            .filter(|(_, connection)| connection.is_peer())
            .map(|(link, _)| link.clone())
            .collect();
        links.sort_unstable();
        links
    }

    pub(in crate::server) fn connected_links(&self) -> Vec<(Link, ConnectionHandle)> {
        let mut links: Vec<_> = self
            .connections
            .iter()
            .map(|(link, connection)| (link.clone(), connection.handle()))
            .collect();
        links.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        links
    }

    pub(in crate::server) fn connected_local_links(&self) -> Vec<(Link, ConnectionHandle)> {
        self.connected_links()
            .into_iter()
            .filter(|(link, _)| !self.is_peer_link(link))
            .collect()
    }

    pub(in crate::server) fn route_contexts_sorted(&self) -> Vec<(&Route, &RouteContext)> {
        let mut contexts: Vec<_> = self.routes.iter().collect();
        contexts.sort_unstable_by(|(route_a, _), (route_b, _)| {
            route_a.to_string().cmp(&route_b.to_string())
        });
        contexts
    }

    pub(in crate::server) fn rpc_contexts_sorted(&self) -> Vec<(Route, RpcDispatcher)> {
        let mut contexts: Vec<_> = self
            .routes
            .iter()
            .map(|(route, context)| (route.clone(), context.rpc()))
            .chain(
                self.connections
                    .iter()
                    .map(|(link, connection)| (Route::from_link(link.clone()), connection.rpc())),
            )
            .collect();
        contexts.sort_unstable_by(|(route_a, _), (route_b, _)| {
            route_a.to_string().cmp(&route_b.to_string())
        });
        contexts
    }

    pub(in crate::server) fn active_endpoint_stream_sinks_for_method(
        &self,
        method: MethodSpec,
    ) -> Vec<(RpcDispatcher, ActiveEndpointStreamSink)> {
        self.rpc_contexts_sorted()
            .into_iter()
            .flat_map(|(_, rpc)| {
                rpc.active_endpoint_stream_sinks_for_method(method)
                    .into_iter()
                    .map(move |sink| (rpc.clone(), sink))
            })
            .collect()
    }

    pub(in crate::server) fn host_contexts_sorted(&self) -> Vec<(&Route, &Host, &RouteContext)> {
        self.route_contexts_sorted()
            .into_iter()
            .filter_map(|(route, context)| {
                self.hosts
                    .get(&context.host_id)
                    .map(|host_context| (route, &host_context.host, context))
            })
            .collect()
    }

    pub(in crate::server) fn remote_agent_count(&self) -> usize {
        self.hosts
            .values()
            .map(|context| context.agents.len())
            .sum()
    }

    pub(in crate::server) fn host_count(&self) -> usize {
        self.hosts.len()
    }

    pub(in crate::server) fn peer_connection_count(&self) -> usize {
        self.connections
            .values()
            .filter(|connection| connection.is_peer())
            .count()
    }

    pub(in crate::server) fn remove_local_origin_outbound_for_return_hop(
        &self,
        call_id: &CallId,
        owner_link: &Link,
        response_route: &Route,
    ) -> Option<OutboundCall> {
        self.rpc_contexts_sorted().into_iter().find_map(|(_, rpc)| {
            rpc.remove_local_origin_outbound_for_return_hop(call_id, owner_link, response_route)
        })
    }

    pub(in crate::server) fn remove_local_origin_outbound_for_return_hop_and_failed_route(
        &self,
        call_id: &CallId,
        owner_link: &Link,
        failed_route: &Route,
    ) -> Option<OutboundCall> {
        self.rpc_contexts_sorted().into_iter().find_map(|(_, rpc)| {
            rpc.remove_local_origin_outbound_for_return_hop_and_failed_route(
                call_id,
                owner_link,
                failed_route,
            )
        })
    }

    pub(in crate::server) fn remove_tracked_outbound_for_call(
        &self,
        call_id: &CallId,
        failed_route: &Route,
    ) -> Option<OutboundCall> {
        self.rpc_contexts_sorted()
            .into_iter()
            .find_map(|(_, rpc)| rpc.remove_tracked_outbound_for_call(call_id, failed_route))
    }

    pub(in crate::server) fn remove_local_origin_outbound_for_owner_link(
        &self,
        owner_link: &Link,
    ) -> Vec<LocalOriginOutboundCall> {
        self.rpc_contexts_sorted()
            .into_iter()
            .flat_map(|(_, rpc)| rpc.remove_local_origin_outbound_for_owner_link(owner_link))
            .collect()
    }

    pub(in crate::server) fn remove_local_origin_outbound_for_route_prefix(
        &self,
        route_prefix: &Route,
    ) -> Vec<LocalOriginOutboundCall> {
        self.rpc_contexts_sorted()
            .into_iter()
            .flat_map(|(_, rpc)| rpc.remove_local_origin_outbound_for_route_prefix(route_prefix))
            .collect()
    }

    pub(in crate::server) fn remove_inbound_for_owner_link_except_method(
        &self,
        owner_link: &Link,
        excluded_method: MethodSpec,
    ) -> Vec<InboundCall> {
        self.rpc_contexts_sorted()
            .into_iter()
            .flat_map(|(_, rpc)| {
                rpc.remove_inbound_for_owner_link_except_method(owner_link, excluded_method)
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn inbound_call_ids_if(
        &self,
        mut predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Vec<CallId> {
        self.rpc_contexts_sorted()
            .into_iter()
            .flat_map(|(_, rpc)| rpc.inbound_call_ids_if(&mut predicate))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn total_inbound_len(&self) -> usize {
        self.rpc_contexts_sorted()
            .into_iter()
            .map(|(_, rpc)| rpc.inbound_len())
            .sum()
    }

    #[cfg(test)]
    pub(crate) fn total_outbound_len(&self) -> usize {
        self.rpc_contexts_sorted()
            .into_iter()
            .map(|(_, rpc)| rpc.outbound_len())
            .sum()
    }

    pub(crate) fn list_agents(&self) -> Vec<crate::protocol::Agent> {
        let mut agents: Vec<crate::protocol::Agent> =
            self.all_agents().into_iter().map(Into::into).collect();
        agents.sort_unstable_by(|a, b| {
            a.route
                .to_string()
                .cmp(&b.route.to_string())
                .then_with(|| {
                    a.name
                        .as_deref()
                        .unwrap_or("")
                        .cmp(b.name.as_deref().unwrap_or(""))
                })
                .then_with(|| a.id.as_u128().cmp(&b.id.as_u128()))
        });
        agents
    }

    pub(crate) fn resolve_agent(&self, identifier: &str) -> Option<crate::protocol::Agent> {
        self.resolve_agent_domain(identifier).map(Into::into)
    }

    pub(crate) fn agent_session_mut(&mut self, agent_id: &Uuid) -> Option<&mut AgentSession> {
        self.local_agents
            .get_mut(agent_id)
            .map(|context| &mut context.session)
    }

    pub(crate) fn local_agent_info(&self, agent_id: &Uuid) -> Option<&Agent> {
        self.local_agents.get(agent_id).map(|context| &context.info)
    }

    pub(crate) fn insert_registered_local_agent(
        &mut self,
        agent_id: Uuid,
        session: AgentSession,
        info: Agent,
    ) -> Result<crate::server::routing::TopologyEvent, String> {
        self.register_local_agent_context(agent_id, session, info)
    }

    pub(in crate::server) fn register_local_agent_context(
        &mut self,
        agent_id: Uuid,
        session: AgentSession,
        info: Agent,
    ) -> Result<TopologyEvent, String> {
        if self.contains_agent_id(&agent_id) {
            return Err(format!("Agent already exists: {agent_id}"));
        }
        if let Some(name) = &info.name
            && self.name_taken_by_other(name, agent_id)
        {
            return Err(format!("Agent already exists: {name}"));
        }

        self.local_agents.insert(
            agent_id,
            LocalAgentContext {
                session,
                info: info.clone(),
            },
        );
        Ok(TopologyEvent::AgentUp { agent: info })
    }

    pub(in crate::server) fn update_local_agent_info(
        &mut self,
        updated: Agent,
    ) -> Result<TopologyEvent, String> {
        if !self.local_agents.contains_key(&updated.id) {
            return Err(format!("Agent not found: {}", updated.id));
        }
        if let Some(name) = &updated.name
            && self.name_taken_by_other(name, updated.id)
        {
            return Err(format!("Agent already exists: {name}"));
        }
        let context = self
            .local_agents
            .get_mut(&updated.id)
            .expect("local agent existence checked above");
        context.info = updated.clone();
        Ok(TopologyEvent::AgentUp { agent: updated })
    }

    pub(in crate::server) fn contains_agent_id(&self, agent_id: &Uuid) -> bool {
        self.local_agents.contains_key(agent_id)
            || self
                .hosts
                .values()
                .any(|context| context.agents.contains_key(agent_id))
    }

    pub(in crate::server) fn name_taken_by_other(&self, name: &str, agent_id: Uuid) -> bool {
        self.local_agents.values().any(|context| {
            context.info.id != agent_id && context.info.name.as_deref() == Some(name)
        }) || self
            .remote_name_owners
            .get(name)
            .is_some_and(|owner| *owner != agent_id)
    }

    pub(in crate::server) fn all_agents(&self) -> Vec<Agent> {
        let mut agents: Vec<_> = self
            .local_agents
            .values()
            .map(|context| context.info.clone())
            .collect();
        let mut hosts: Vec<_> = self.hosts.iter().collect();
        hosts.sort_unstable_by(|(host_id_a, context_a), (host_id_b, context_b)| {
            self.route_for_host(**host_id_a)
                .map(|route| route.to_string())
                .cmp(
                    &self
                        .route_for_host(**host_id_b)
                        .map(|route| route.to_string()),
                )
                .then_with(|| context_a.host.name.cmp(&context_b.host.name))
                .then_with(|| host_id_a.as_u128().cmp(&host_id_b.as_u128()))
        });
        for (host_id, context) in hosts {
            let Some(route) = self.route_for_host(*host_id) else {
                continue;
            };
            let mut remote_agents: Vec<_> = context.agents.values().cloned().collect();
            remote_agents.sort_unstable_by(|a, b| {
                a.name
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.name.as_deref().unwrap_or(""))
                    .then_with(|| a.id.as_u128().cmp(&b.id.as_u128()))
            });
            for mut agent in remote_agents {
                agent.route = route.clone();
                agents.push(agent);
            }
        }
        agents
    }

    fn resolve_agent_domain(&self, identifier: &str) -> Option<Agent> {
        match identifier.rsplit_once(':') {
            Some((route_str, id)) => {
                let supplied_route = parse_route(route_str)?;
                self.resolve_agent_inner_on_route(id, &supplied_route)
            }
            None => self.resolve_agent_inner(identifier),
        }
    }

    fn resolve_agent_inner(&self, identifier: &str) -> Option<Agent> {
        if let Ok(agent_id) = Uuid::parse_str(identifier) {
            return self.agent_by_id(agent_id);
        }
        self.agent_by_name(identifier)
    }

    fn resolve_agent_inner_on_route(&self, identifier: &str, route: &Route) -> Option<Agent> {
        if let Ok(agent_id) = Uuid::parse_str(identifier) {
            return self.agent_by_id_on_route(agent_id, route);
        }
        self.agent_by_name_on_route(identifier, route)
    }

    fn agent_by_id(&self, agent_id: Uuid) -> Option<Agent> {
        if let Some(context) = self.local_agents.get(&agent_id) {
            return Some(context.info.clone());
        }
        for (host_id, context) in self.host_contexts_by_id_sorted() {
            if let Some(agent) = context.agents.get(&agent_id) {
                let Some(route) = self.route_for_host(*host_id) else {
                    continue;
                };
                let mut agent = agent.clone();
                agent.route = route;
                return Some(agent);
            }
        }
        None
    }

    fn agent_by_id_on_route(&self, agent_id: Uuid, route: &Route) -> Option<Agent> {
        if route.is_empty() {
            return self
                .local_agents
                .get(&agent_id)
                .map(|context| context.info.clone());
        }

        let host_id = self.routes.get(route)?.host_id;
        let mut agent = self.hosts.get(&host_id)?.agents.get(&agent_id)?.clone();
        agent.route = route.clone();
        Some(agent)
    }

    fn agent_by_name(&self, name: &str) -> Option<Agent> {
        let mut local: Vec<_> = self
            .local_agents
            .values()
            .filter(|context| context.info.name.as_deref() == Some(name))
            .map(|context| context.info.clone())
            .collect();
        local.sort_unstable_by_key(|agent| agent.id.as_u128());
        if let Some(agent) = local.into_iter().next() {
            return Some(agent);
        }

        let agent_id = *self.remote_name_owners.get(name)?;
        self.agent_by_id(agent_id)
    }

    fn agent_by_name_on_route(&self, name: &str, route: &Route) -> Option<Agent> {
        if route.is_empty() {
            let mut local: Vec<_> = self
                .local_agents
                .values()
                .filter(|context| context.info.name.as_deref() == Some(name))
                .map(|context| context.info.clone())
                .collect();
            local.sort_unstable_by_key(|agent| agent.id.as_u128());
            return local.into_iter().next();
        }

        let host_id = self.routes.get(route)?.host_id;
        let mut remote: Vec<_> = self
            .hosts
            .get(&host_id)?
            .agents
            .values()
            .filter(|agent| agent.name.as_deref() == Some(name))
            .cloned()
            .collect();
        remote.sort_unstable_by_key(|agent| agent.id.as_u128());
        remote.into_iter().next().map(|mut agent| {
            agent.route = route.clone();
            agent
        })
    }

    fn host_contexts_by_id_sorted(&self) -> Vec<(&Uuid, &HostContext)> {
        let mut hosts: Vec<_> = self.hosts.iter().collect();
        hosts.sort_unstable_by(|(host_id_a, context_a), (host_id_b, context_b)| {
            context_a
                .host
                .name
                .cmp(&context_b.host.name)
                .then_with(|| host_id_a.as_u128().cmp(&host_id_b.as_u128()))
        });
        hosts
    }

    fn upsert_host_context(&mut self, host: Host) {
        self.hosts
            .entry(host.id)
            .and_modify(|context| {
                context.host = host.clone();
            })
            .or_insert_with(|| HostContext::new(host));
    }

    fn local_name_owner(&self, name: &str) -> Option<Uuid> {
        self.local_agents.values().find_map(|context| {
            (context.info.name.as_deref() == Some(name)).then_some(context.info.id)
        })
    }

    fn release_remote_names_for_agent(&mut self, agent_id: Uuid) {
        self.remote_name_owners
            .retain(|_, owner| *owner != agent_id);
    }

    fn release_remote_name_if_owned(&mut self, agent: &Agent) {
        if let Some(name) = &agent.name
            && self.remote_name_owners.get(name) == Some(&agent.id)
        {
            self.remote_name_owners.remove(name);
        }
    }

    fn claim_remote_name_for_agent(&mut self, agent: &Agent) {
        let Some(name) = &agent.name else {
            return;
        };
        if self.local_name_owner(name).is_none() {
            self.remote_name_owners
                .entry(name.clone())
                .or_insert(agent.id);
        }
    }

    pub(in crate::server) fn apply_peer_agent_up(
        &mut self,
        from: &Link,
        mut agent: Agent,
    ) -> PeerAgentUpChange {
        if self.local_agents.contains_key(&agent.id) {
            return PeerAgentUpChange::ignored(PeerAgentUpIgnored::LocalAgent);
        }

        let Some(_route) = self.route_for_host_from(agent.host_id, from) else {
            return if self.hosts.contains_key(&agent.host_id) {
                PeerAgentUpChange::ignored(PeerAgentUpIgnored::NonSelectedHostRoute)
            } else {
                PeerAgentUpChange::ignored(PeerAgentUpIgnored::UnknownHost)
            };
        };

        for (host_id, context) in &mut self.hosts {
            if *host_id != agent.host_id {
                context.agents.remove(&agent.id);
            }
        }
        self.release_remote_names_for_agent(agent.id);
        self.claim_remote_name_for_agent(&agent);

        let context = self
            .hosts
            .get_mut(&agent.host_id)
            .expect("host route lookup returned an existing host");
        agent.route = Route::empty();
        context.agents.insert(agent.id, agent.clone());
        PeerAgentUpChange { ignored: None }
    }

    pub(in crate::server) fn apply_peer_agent_down_for_host(
        &mut self,
        from: &Link,
        host_id: Uuid,
        agent_id: Uuid,
    ) -> PeerAgentDownChange {
        if self.local_agents.contains_key(&agent_id) {
            return PeerAgentDownChange::ignored(PeerAgentDownIgnored::LocalAgent);
        }

        let Some(actual_host_id) = self.remote_agent_host(agent_id) else {
            return PeerAgentDownChange::ignored(PeerAgentDownIgnored::UnknownAgent);
        };
        if actual_host_id != host_id {
            return PeerAgentDownChange::ignored(PeerAgentDownIgnored::UnknownAgent);
        }
        if self.route_for_host_from(host_id, from).is_none() {
            return if self.hosts.contains_key(&host_id) {
                PeerAgentDownChange::ignored(PeerAgentDownIgnored::NonSelectedHostRoute)
            } else {
                PeerAgentDownChange::ignored(PeerAgentDownIgnored::UnknownAgent)
            };
        };

        let removed = self
            .hosts
            .get_mut(&host_id)
            .and_then(|context| context.agents.remove(&agent_id))
            .expect("remote agent host lookup returned a present agent");
        debug_assert_eq!(removed.id, agent_id);
        self.release_remote_name_if_owned(&removed);
        PeerAgentDownChange {
            removed: true,
            ignored: None,
        }
    }

    pub(in crate::server) fn apply_peer_host_up(
        &mut self,
        from: &Link,
        host: Host,
        received_route: Route,
    ) -> PeerHostUpChange {
        let mut route = received_route;
        route.push(from.clone());

        self.upsert_host_at_route(host, route)
    }

    pub(in crate::server) fn apply_direct_peer_host_up(
        &mut self,
        link: &Link,
        host: Host,
    ) -> PeerHostUpChange {
        self.upsert_host_at_route(host, Route::from_link(link.clone()))
    }

    fn upsert_host_at_route(&mut self, host: Host, route: Route) -> PeerHostUpChange {
        let id = host.id;
        let route_host_id = self.routes.get(&route).map(|context| context.host_id);
        let event = match route_host_id {
            Some(existing_id) if existing_id == id => {
                self.upsert_host_context(host.clone());
                Some(TopologyEvent::HostUp {
                    host,
                    route: route.clone(),
                })
            }
            Some(_) => None,
            None => {
                self.routes.insert(route.clone(), RouteContext::new(id));
                self.upsert_host_context(host.clone());
                Some(TopologyEvent::HostUp {
                    host,
                    route: route.clone(),
                })
            }
        };
        let events = event.into_iter().collect();
        PeerHostUpChange {
            rewritten_descendants: 0,
            events,
        }
    }

    pub(in crate::server) fn apply_peer_host_down(
        &mut self,
        from: &Link,
        id: Uuid,
        received_route: Route,
    ) -> PeerHostDownChange {
        let mut route = received_route;
        route.push(from.clone());

        let root_matches = self
            .routes
            .get(&route)
            .is_some_and(|context| context.host_id == id);
        if !root_matches {
            return PeerHostDownChange {
                event: None,
                root_matches,
                removed_agents: 0,
                removed_descendants: 0,
            };
        }

        let removed = self.remove_route_subtree(&route);
        let root_host_removed = !self.hosts.contains_key(&id);
        PeerHostDownChange {
            event: Some(TopologyEvent::HostDown {
                id,
                route: route.clone(),
            }),
            root_matches,
            removed_agents: removed.agents,
            removed_descendants: removed.hosts.saturating_sub(usize::from(root_host_removed)),
        }
    }

    pub(in crate::server) fn apply_link_closed(&mut self, link: &Link) -> LinkClosedChange {
        self.connections.remove(link);
        let prefix = Route::from_link(link.clone());
        let host_roots = self.disconnected_host_roots(&prefix);
        let removed = self.remove_route_subtree(&prefix);
        let events = host_roots
            .into_iter()
            .map(|(id, route)| TopologyEvent::HostDown { id, route })
            .collect();

        LinkClosedChange {
            events,
            removed_agents: removed.agents,
            removed_hosts: removed.hosts,
        }
    }

    fn route_for_host(&self, host_id: Uuid) -> Option<Route> {
        self.routes_for_host(host_id).into_iter().next()
    }

    pub(in crate::server) fn agent_subscription_candidate(
        &self,
        host_id: Uuid,
    ) -> Option<(Host, Route)> {
        let context = self.hosts.get(&host_id)?;
        if context.host.capabilities.supported_agent_types.is_empty() {
            return None;
        }
        if let Some(subscription) = &context.agent_subscription
            && self.routes.contains_key(&subscription.route)
        {
            return None;
        }
        let route = self.route_for_host(host_id)?;
        Some((context.host.clone(), route))
    }

    pub(in crate::server) fn set_agent_subscription(
        &mut self,
        host_id: Uuid,
        route: Route,
        call_id: CallId,
    ) {
        if let Some(context) = self.hosts.get_mut(&host_id) {
            context.agent_subscription = Some(AgentSubscriptionState { route, call_id });
        }
    }

    pub(in crate::server) fn clear_agent_subscription_for_route(
        &mut self,
        call_id: &CallId,
        route: &Route,
    ) {
        for context in self.hosts.values_mut() {
            if context
                .agent_subscription
                .as_ref()
                .is_some_and(|subscription| {
                    &subscription.call_id == call_id && &subscription.route == route
                })
            {
                context.agent_subscription = None;
            }
        }
    }

    pub(in crate::server) fn agent_subscription_host_for_route_and_call(
        &self,
        call_id: &CallId,
        route: &Route,
    ) -> Option<Uuid> {
        self.hosts.iter().find_map(|(host_id, context)| {
            context
                .agent_subscription
                .as_ref()
                .is_some_and(|subscription| {
                    &subscription.call_id == call_id && &subscription.route == route
                })
                .then_some(*host_id)
        })
    }

    pub(in crate::server) fn remove_agent_subscription_for_route_and_call(
        &mut self,
        call_id: &CallId,
        route: &Route,
    ) -> bool {
        let removed = self
            .route_rpc_for_counterparty(route)
            .and_then(|rpc| {
                rpc.remove_server_origin_outbound(
                    call_id,
                    crate::protocol::method::AGENT_SUBSCRIBE_EVENTS,
                )
            })
            .is_some();
        if removed {
            self.clear_agent_subscription_for_route(call_id, route);
        }
        removed
    }

    fn route_for_host_from(&self, host_id: Uuid, from: &Link) -> Option<Route> {
        self.routes_for_host(host_id)
            .into_iter()
            .find(|route| route.peek() == Some(from))
    }

    fn routes_for_host(&self, host_id: Uuid) -> Vec<Route> {
        let mut routes: Vec<_> = self
            .routes
            .iter()
            .filter(|(_, context)| context.host_id == host_id)
            .map(|(route, _)| route.clone())
            .collect();
        routes.sort_unstable_by_key(|route| route.to_string());
        routes
    }

    fn remote_agent_host(&self, agent_id: Uuid) -> Option<Uuid> {
        self.hosts.iter().find_map(|(host_id, context)| {
            context.agents.contains_key(&agent_id).then_some(*host_id)
        })
    }

    fn disconnected_host_roots(&self, prefix: &Route) -> Vec<(Uuid, Route)> {
        let mut hosts: Vec<_> = self
            .routes
            .iter()
            .filter(|(route, _)| route.starts_with_route(prefix))
            .map(|(route, context)| (context.host_id, route.clone()))
            .collect();
        hosts.sort_unstable_by(|(id_a, route_a), (id_b, route_b)| {
            route_a
                .to_string()
                .cmp(&route_b.to_string())
                .then_with(|| id_a.as_u128().cmp(&id_b.as_u128()))
        });
        hosts
            .iter()
            .filter(|(_, route)| {
                !hosts.iter().any(|(_, other_route)| {
                    route != other_route && route.starts_with_route(other_route)
                })
            })
            .cloned()
            .collect()
    }

    fn remove_route_subtree(&mut self, prefix: &Route) -> RemovedRouteSubtree {
        self.remove_route_subtree_inner(prefix)
    }

    fn remove_route_subtree_inner(&mut self, prefix: &Route) -> RemovedRouteSubtree {
        let mut keys: Vec<_> = self
            .routes
            .keys()
            .filter(|route| route.starts_with_route(prefix))
            .cloned()
            .collect();
        keys.sort_unstable_by(|a, b| {
            b.len()
                .cmp(&a.len())
                .then_with(|| a.to_string().cmp(&b.to_string()))
        });

        let mut removed = RemovedRouteSubtree::default();
        let mut orphan_candidates = Vec::new();
        for key in keys {
            if let Some(context) = self.routes.remove(&key) {
                orphan_candidates.push(context.host_id);
                context.rpc.cancel_all();
            }
        }
        orphan_candidates.sort_unstable_by_key(|id| id.as_u128());
        orphan_candidates.dedup();
        for host_id in orphan_candidates {
            if self
                .routes
                .values()
                .any(|context| context.host_id == host_id)
            {
                if let Some(context) = self.hosts.get_mut(&host_id)
                    && context
                        .agent_subscription
                        .as_ref()
                        .is_some_and(|subscription| subscription.route.starts_with_route(prefix))
                {
                    context.agent_subscription = None;
                }
                continue;
            }
            if let Some(context) = self.hosts.remove(&host_id) {
                for agent in context.agents.values() {
                    self.release_remote_name_if_owned(agent);
                }
                removed.agents += context.agents.len();
                removed.hosts += 1;
            }
        }
        removed
    }
}

#[derive(Default)]
struct RemovedRouteSubtree {
    agents: usize,
    hosts: usize,
}

fn parse_route(route: &str) -> Option<Route> {
    use serde::Deserialize;

    let deserializer = serde::de::value::StrDeserializer::<serde::de::value::Error>::new(route);
    Route::deserialize(deserializer).ok()
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
    use chrono::Utc;

    use super::*;
    use crate::protocol::{Route, method};
    use crate::rpc::OutboundCallState;
    use crate::server::{EndpointUnaryStart, LocalOriginOutboundStart, PeerRoutingOutboundStart};

    fn test_host(id: Uuid, name: &str, version: &str) -> Host {
        Host {
            id,
            name: name.to_string(),
            version: version.to_string(),
            capabilities: Default::default(),
        }
    }

    #[test]
    fn connection_handle_retains_close_request_before_subscription() {
        let (tx, _rx) = mpsc::channel(1);
        let handle = ConnectionHandle::new(tx);

        handle.request_close("closing");

        let receiver = handle.close_receiver();
        assert_eq!(receiver.borrow().as_deref(), Some("closing"));
    }

    #[test]
    fn host_up_tracks_multiple_routes_without_rewriting_existing_contexts() {
        let mut user_state = ServerUserState::new();
        let peer_a = Link::new("peer-a").unwrap();
        let peer_b = Link::new("peer-b").unwrap();
        let root_id = Uuid::from_u128(1);

        let first = user_state.apply_peer_host_up(
            &peer_a,
            test_host(root_id, "old-root", "v1"),
            Route::empty(),
        );
        let second = user_state.apply_peer_host_up(
            &peer_b,
            test_host(root_id, "new-root", "v2"),
            Route::empty(),
        );

        assert_eq!(first.events.len(), 1);
        assert_eq!(second.events.len(), 1);
        assert_eq!(user_state.host_count(), 1);
        assert!(matches!(
            &first.events[0],
            TopologyEvent::HostUp { host, route }
                if host.id == root_id
                    && host.name == "old-root"
                    && route == &Route::from_link(peer_a.clone())
        ));
        assert!(matches!(
            &second.events[0],
            TopologyEvent::HostUp { host, route }
                if host.id == root_id
                    && host.name == "new-root"
                    && route == &Route::from_link(peer_b.clone())
        ));
    }

    #[test]
    fn direct_peer_host_up_tracks_host_at_connection_route() {
        let mut user_state = ServerUserState::new();
        let peer = Link::new("peer-a").unwrap();
        let host_id = Uuid::from_u128(10);

        let change =
            user_state.apply_direct_peer_host_up(&peer, test_host(host_id, "peer-host", "v1"));

        assert_eq!(change.events.len(), 1);
        assert!(matches!(
            &change.events[0],
            TopologyEvent::HostUp { host, route }
                if host.id == host_id
                    && host.name == "peer-host"
                    && route == &Route::from_link(peer.clone())
        ));
        assert!(user_state.route_rpc(&Route::from_link(peer)).is_some());
    }

    #[test]
    fn agent_up_tracks_agent_on_host_reachable_by_multiple_routes() {
        let mut user_state = ServerUserState::new();
        let peer_a = Link::new("peer-a").unwrap();
        let peer_b = Link::new("peer-b").unwrap();
        let host_id = Uuid::from_u128(1);
        let agent_id = Uuid::from_u128(2);
        user_state.apply_peer_host_up(&peer_a, test_host(host_id, "host-a", "v1"), Route::empty());
        user_state.apply_peer_host_up(&peer_b, test_host(host_id, "host-b", "v1"), Route::empty());

        let agent = Agent {
            id: agent_id,
            host_id,
            name: Some("echo".to_string()),
            command: "test".to_string(),
            working_dir: std::env::temp_dir(),
            route: Route::empty(),
            agent_type: "test".to_string(),
            io_protocols: Vec::new(),
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
        };

        let first = user_state.apply_peer_agent_up(&peer_a, agent.clone());
        let second = user_state.apply_peer_agent_up(&peer_b, agent);

        assert!(first.ignored.is_none());
        assert!(second.ignored.is_none());
        assert_eq!(user_state.remote_agent_count(), 1);
        assert!(
            user_state
                .hosts
                .get(&host_id)
                .unwrap()
                .agents
                .contains_key(&agent_id)
        );
    }

    #[test]
    fn route_qualified_agent_resolution_accepts_any_route_to_host() {
        let mut user_state = ServerUserState::new();
        let peer_a = Link::new("peer-a").unwrap();
        let peer_b = Link::new("peer-b").unwrap();
        let host_id = Uuid::from_u128(1);
        let agent_id = Uuid::from_u128(2);
        user_state.apply_peer_host_up(&peer_a, test_host(host_id, "host", "v1"), Route::empty());
        user_state.apply_peer_host_up(&peer_b, test_host(host_id, "host", "v1"), Route::empty());
        user_state.apply_peer_agent_up(
            &peer_a,
            Agent {
                id: agent_id,
                host_id,
                name: Some("echo".to_string()),
                command: "test".to_string(),
                working_dir: std::env::temp_dir(),
                route: Route::empty(),
                agent_type: "test".to_string(),
                io_protocols: Vec::new(),
                readonly: false,
                args: Vec::new(),
                created_at: Utc::now(),
            },
        );

        let route_b = Route::from_link(peer_b);
        let resolved_by_id = user_state
            .resolve_agent(&format!("{route_b}:{agent_id}"))
            .expect("agent should resolve through the second host route");
        let resolved_by_name = user_state
            .resolve_agent(&format!("{route_b}:echo"))
            .expect("named agent should resolve through the second host route");

        assert_eq!(resolved_by_id.route, route_b);
        assert_eq!(resolved_by_name.route, route_b);
    }

    #[test]
    fn remote_agent_reannounce_with_new_host_moves_existing_agent() {
        let mut user_state = ServerUserState::new();
        let peer_a = Link::new("peer-a").unwrap();
        let peer_b = Link::new("peer-b").unwrap();
        let host_a = Uuid::from_u128(1);
        let host_b = Uuid::from_u128(2);
        let agent_id = Uuid::from_u128(3);
        user_state.apply_peer_host_up(&peer_a, test_host(host_a, "host-a", "v1"), Route::empty());
        user_state.apply_peer_host_up(&peer_b, test_host(host_b, "host-b", "v1"), Route::empty());
        let agent = Agent {
            id: agent_id,
            host_id: host_a,
            name: Some("old".to_string()),
            command: "test".to_string(),
            working_dir: std::env::temp_dir(),
            route: Route::empty(),
            agent_type: "test".to_string(),
            io_protocols: Vec::new(),
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
        };
        user_state.apply_peer_agent_up(&peer_a, agent.clone());

        let mut moved = agent;
        moved.host_id = host_b;
        moved.name = Some("new".to_string());
        user_state.apply_peer_agent_up(&peer_b, moved);

        assert_eq!(user_state.remote_agent_count(), 1);
        assert!(
            !user_state
                .hosts
                .get(&host_a)
                .unwrap()
                .agents
                .contains_key(&agent_id)
        );
        assert!(
            user_state
                .hosts
                .get(&host_b)
                .unwrap()
                .agents
                .contains_key(&agent_id)
        );
        assert!(user_state.resolve_agent("old").is_none());
        let resolved = user_state
            .resolve_agent("new")
            .expect("moved agent should resolve by latest name");
        assert_eq!(resolved.host_id, host_b);
        assert_eq!(resolved.route, Route::from_link(peer_b));
    }

    #[test]
    fn duplicate_remote_names_keep_first_unqualified_owner() {
        let mut user_state = ServerUserState::new();
        let peer_a = Link::new("peer-a").unwrap();
        let peer_b = Link::new("peer-b").unwrap();
        let host_a = Uuid::from_u128(1);
        let host_b = Uuid::from_u128(2);
        let first_agent_id = Uuid::from_u128(3);
        let second_agent_id = Uuid::from_u128(4);
        user_state.apply_peer_host_up(&peer_b, test_host(host_b, "z-host", "v1"), Route::empty());
        user_state.apply_peer_host_up(&peer_a, test_host(host_a, "a-host", "v1"), Route::empty());

        let mut first = Agent {
            id: first_agent_id,
            host_id: host_b,
            name: Some("shared".to_string()),
            command: "test".to_string(),
            working_dir: std::env::temp_dir(),
            route: Route::empty(),
            agent_type: "test".to_string(),
            io_protocols: Vec::new(),
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
        };
        user_state.apply_peer_agent_up(&peer_b, first.clone());
        first.id = second_agent_id;
        first.host_id = host_a;
        user_state.apply_peer_agent_up(&peer_a, first);

        let unqualified = user_state
            .resolve_agent("shared")
            .expect("first remote name owner should resolve");
        let route_a = Route::from_link(peer_a);
        let route_b = Route::from_link(peer_b);
        let route_qualified = user_state
            .resolve_agent(&format!("{route_a}:shared"))
            .expect("route-qualified duplicate should resolve on that route");

        assert_eq!(unqualified.id, first_agent_id);
        assert_eq!(unqualified.route, route_b);
        assert_eq!(route_qualified.id, second_agent_id);
        assert_eq!(route_qualified.route, route_a);
    }

    #[test]
    fn agent_down_removes_host_agent_when_received_from_known_host_route() {
        let mut user_state = ServerUserState::new();
        let relay = Link::new("relay").unwrap();
        let host_a = Route::from_link(Link::new("host-a").unwrap());
        let host_b = Route::from_link(Link::new("host-b").unwrap());
        let host_id = Uuid::from_u128(1);
        let agent_id = Uuid::from_u128(2);
        user_state.apply_peer_host_up(&relay, test_host(host_id, "host-a", "v1"), host_a.clone());
        user_state.apply_peer_host_up(&relay, test_host(host_id, "host-b", "v1"), host_b.clone());

        let agent = Agent {
            id: agent_id,
            host_id,
            name: Some("echo".to_string()),
            command: "test".to_string(),
            working_dir: std::env::temp_dir(),
            route: Route::empty(),
            agent_type: "test".to_string(),
            io_protocols: Vec::new(),
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
        };

        user_state.apply_peer_agent_up(&relay, agent);

        assert!(
            user_state
                .hosts
                .get(&host_id)
                .unwrap()
                .agents
                .contains_key(&agent_id)
        );

        let removed = user_state.apply_peer_agent_down_for_host(&relay, host_id, agent_id);

        assert!(removed.removed);
        assert!(
            !user_state
                .hosts
                .get(&host_id)
                .unwrap()
                .agents
                .contains_key(&agent_id)
        );
    }

    #[test]
    fn agent_down_for_subscription_host_does_not_remove_agent_on_other_host() {
        let mut user_state = ServerUserState::new();
        let relay = Link::new("relay").unwrap();
        let host_a_route = Route::from_link(Link::new("host-a").unwrap());
        let host_b_route = Route::from_link(Link::new("host-b").unwrap());
        let host_a = Uuid::from_u128(1);
        let host_b = Uuid::from_u128(2);
        let agent_id = Uuid::from_u128(3);
        user_state.apply_peer_host_up(
            &relay,
            test_host(host_a, "host-a", "v1"),
            host_a_route.clone(),
        );
        user_state.apply_peer_host_up(
            &relay,
            test_host(host_b, "host-b", "v1"),
            host_b_route.clone(),
        );

        let agent = Agent {
            id: agent_id,
            host_id: host_b,
            name: Some("echo".to_string()),
            command: "test".to_string(),
            working_dir: std::env::temp_dir(),
            route: Route::empty(),
            agent_type: "test".to_string(),
            io_protocols: Vec::new(),
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
        };
        user_state.apply_peer_agent_up(&relay, agent);

        let removed = user_state.apply_peer_agent_down_for_host(&relay, host_a, agent_id);

        assert!(!removed.removed);
        assert!(
            user_state
                .hosts
                .get(&host_b)
                .unwrap()
                .agents
                .contains_key(&agent_id)
        );
    }

    #[test]
    fn direct_host_down_removes_route_context_but_preserves_connection_rpc() {
        let mut user_state = ServerUserState::new();
        let peer = Link::new("peer-a").unwrap();
        let host_id = Uuid::from_u128(1);
        let call_id = CallId::from(Uuid::from_u128(2));
        user_state.try_reserve_link(peer.clone()).unwrap();
        user_state.mark_peer_link(peer.clone());
        user_state.apply_peer_host_up(&peer, test_host(host_id, "peer-host", "v1"), Route::empty());
        let rpc = user_state.rpc_for_link(&peer).unwrap();
        rpc.register_peer_routing_outbound(PeerRoutingOutboundStart {
            link: peer.clone(),
            call_id: call_id.clone(),
            method: method::ROUTING_SUBSCRIBE_EVENTS,
        })
        .unwrap();

        let change = user_state.apply_peer_host_down(&peer, host_id, Route::empty());

        assert!(change.root_matches);
        assert!(user_state.is_peer_link(&peer));
        assert!(user_state.route(&peer).is_some());
        let route = Route::from_link(peer.clone());
        assert!(!user_state.routes.contains_key(&route));
        assert!(
            user_state
                .rpc_for_link(&peer)
                .unwrap()
                .outbound_for_call(&call_id)
                .is_some()
        );
    }

    #[test]
    fn finishing_inbound_peer_routing_subscription_does_not_remove_unrelated_inbound_call() {
        let mut user_state = ServerUserState::new();
        let link = Link::new("peer-a").unwrap();
        let route = Route::from_link(link.clone());
        let call_id = CallId::from(Uuid::new_v4());
        user_state.try_reserve_link(link.clone()).unwrap();
        user_state.mark_peer_link(link.clone());
        let (tx, _rx) = mpsc::channel(1);
        let rpc = user_state.rpc_for_link(&link).unwrap();
        user_state
            .rpc_for_link(&link)
            .unwrap()
            .register_endpoint_unary(EndpointUnaryStart {
                tx,
                owner_link: link.clone(),
                reply_src: Route::empty(),
                reply_dst: route.clone(),
                call_id: call_id.clone(),
                method: method::AGENT_CREATE,
            })
            .unwrap();

        assert!(!rpc.finish_inbound_peer_routing_subscription(&link, &call_id));

        assert!(matches!(
            rpc.inbound_for_call(&call_id).map(|call| call.method),
            Some(method::AGENT_CREATE)
        ));
    }

    #[test]
    fn finishing_outbound_peer_routing_subscription_does_not_remove_unrelated_outbound_call() {
        let mut user_state = ServerUserState::new();
        let link = Link::new("peer-a").unwrap();
        let route = Route::from_link(link.clone());
        let call_id = CallId::from(Uuid::new_v4());
        user_state.try_reserve_link(link.clone()).unwrap();
        user_state.mark_peer_link(link.clone());
        let rpc = user_state.rpc_for_link(&link).unwrap();
        user_state
            .rpc_for_link(&link)
            .unwrap()
            .register_local_origin_outbound(LocalOriginOutboundStart {
                call_id: call_id.clone(),
                method: method::AGENT_CREATE,
                state: OutboundCallState::AwaitingResponse,
                owner_link: Link::new("local").unwrap(),
                request_src: Route::from_link(Link::new("local").unwrap()),
                request_dst: route.clone(),
            })
            .unwrap();

        assert!(!rpc.finish_outbound_peer_routing_subscription(&link, &call_id));

        assert!(matches!(
            rpc.outbound_for_call(&call_id).map(|call| call.method),
            Some(method::AGENT_CREATE)
        ));
    }
}

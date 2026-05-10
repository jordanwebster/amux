use std::sync::Arc;

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use tokio::sync::{RwLock, RwLockReadGuard};
use uuid::Uuid;

use crate::agent::Agent;
use crate::debug::{DebugView, LossyPath};
use crate::protocol::message::{DebugFormat, Host};
use crate::server::{LOCAL_USER_ID, ServerState, ServerUserState};
use crate::setup;

/// Top-level entry point: gather read-locks across the server, then serialize
/// the resulting view in the requested format.
///
/// All async work happens up front (acquiring read guards on `ServerState` and
/// every per-user state). Once we have the guards, the rest is purely sync —
/// the `Serialize` impls cannot await, so every dumped value must be reachable
/// without an `.await`.
pub(crate) async fn dump_server_debug_info(
    state: &Arc<RwLock<ServerState>>,
    format: DebugFormat,
    verbose: bool,
) -> String {
    let state_guard = state.read().await;

    let use_cloud_mode = setup::cloud_enabled(&state_guard.config);

    // Acquire per-user read guards up front so the sync Serialize phase can
    // borrow them without further awaits. Sort by user_id for stable output.
    let mut users: Vec<(Uuid, Arc<RwLock<ServerUserState>>)> = state_guard
        .users
        .iter()
        .map(|(id, us)| (*id, us.clone()))
        .collect();
    users.sort_unstable_by_key(|a| a.0.as_u128());
    let mut user_guards: Vec<(Uuid, RwLockReadGuard<'_, ServerUserState>)> =
        Vec::with_capacity(users.len());
    for (id, us) in &users {
        user_guards.push((*id, us.read().await));
    }

    let view = ServerDebugView {
        state: &state_guard,
        use_cloud_mode,
        local_version: env!("CARGO_PKG_VERSION"),
        users: &user_guards,
        verbose,
    };

    match format {
        DebugFormat::Yaml => serde_yaml::to_string(&view).unwrap_or_default(),
        DebugFormat::Json => serde_json::to_string_pretty(&view).unwrap_or_default(),
    }
}

/// Top-level dump view. Built once per `Debug` command, holds borrowed
/// references to live state plus the few values that required async work
/// (e.g. `use_cloud_mode` from disk).
struct ServerDebugView<'a> {
    state: &'a ServerState,
    use_cloud_mode: bool,
    local_version: &'static str,
    users: &'a [(Uuid, RwLockReadGuard<'a, ServerUserState>)],
    verbose: bool,
}

impl Serialize for ServerDebugView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Aggregate counts across all per-user states.
        let mut agent_count = 0usize;
        let mut remote_agent_count = 0usize;
        let mut host_count = 0usize;
        let mut route_count = 0usize;
        let mut peer_link_count = 0usize;
        for (_, us) in self.users {
            agent_count += us.local_agents.len();
            remote_agent_count += us.remote_agent_count();
            host_count += us.host_count();
            route_count += us.routes.len();
            peer_link_count += us.peer_connection_count();
        }

        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("is_cloud_server", &self.state.is_cloud_server)?;
        map.serialize_entry("use_cloud_mode", &self.use_cloud_mode)?;
        map.serialize_entry("user_count", &self.users.len())?;
        map.serialize_entry("agent_count", &agent_count)?;
        map.serialize_entry("remote_agent_count", &remote_agent_count)?;
        map.serialize_entry("host_count", &host_count)?;
        map.serialize_entry("route_count", &route_count)?;
        map.serialize_entry("peer_link_count", &peer_link_count)?;
        map.serialize_entry("config", &self.state.config)?;

        if self.verbose {
            map.serialize_entry(
                "local_host",
                &LocalHostView {
                    id: self.state.host_id,
                    name: &self.state.config.host_name,
                    version: self.local_version,
                },
            )?;
            map.serialize_entry(
                "users",
                &UsersListView {
                    state: self.state,
                    users: self.users,
                    verbose: self.verbose,
                },
            )?;
        }

        map.end()
    }
}

struct LocalHostView<'a> {
    id: Uuid,
    name: &'a str,
    version: &'static str,
}

impl Serialize for LocalHostView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("version", &self.version)?;
        map.end()
    }
}

struct UsersListView<'a> {
    state: &'a ServerState,
    users: &'a [(Uuid, RwLockReadGuard<'a, ServerUserState>)],
    verbose: bool,
}

impl Serialize for UsersListView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.users.len()))?;
        for (user_id, us) in self.users {
            seq.serialize_element(&UserView {
                state: self.state,
                user_id: *user_id,
                user_state: us,
                verbose: self.verbose,
            })?;
        }
        seq.end()
    }
}

/// Per-user view: counts plus verbose details (peer links, routes, hosts,
/// agents). Reads `ServerUserState` directly via the guard.
struct UserView<'a> {
    state: &'a ServerState,
    user_id: Uuid,
    user_state: &'a ServerUserState,
    verbose: bool,
}

impl Serialize for UserView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let us = self.user_state;
        let agents = us.all_agents();

        let mut host_agent_counts: std::collections::HashMap<Uuid, usize> =
            std::collections::HashMap::new();
        for agent in &agents {
            *host_agent_counts.entry(agent.host_id).or_default() += 1;
        }

        let peer_links: Vec<String> = us
            .peer_links()
            .into_iter()
            .map(|link| link.as_str().to_string())
            .collect();

        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry(
            "user_id",
            &if self.user_id == LOCAL_USER_ID {
                "local".to_string()
            } else {
                self.user_id.to_string()
            },
        )?;
        map.serialize_entry("agent_count", &us.local_agents.len())?;
        map.serialize_entry("remote_agent_count", &us.remote_agent_count())?;
        map.serialize_entry("host_count", &us.host_count())?;
        map.serialize_entry("route_count", &us.routes.len())?;
        map.serialize_entry("peer_link_count", &us.peer_connection_count())?;
        map.serialize_entry("peer_links", &peer_links)?;
        map.serialize_entry("routes", &RoutesView { user_state: us })?;
        map.serialize_entry(
            "hosts",
            &HostsView {
                user_state: us,
                host_agent_counts: &host_agent_counts,
            },
        )?;
        map.serialize_entry(
            "agents",
            &AgentsView {
                state: self.state,
                user_state: us,
                agents: &agents,
                verbose: self.verbose,
            },
        )?;
        map.end()
    }
}

struct RoutesView<'a> {
    user_state: &'a ServerUserState,
}

impl Serialize for RoutesView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let us = self.user_state;
        let mut entries: Vec<_> = us
            .connections
            .iter()
            .map(|(link, connection)| {
                let route = crate::protocol::Route::from_link(link.clone());
                let direct_host_count = usize::from(us.routes.contains_key(&route));
                let routed_host_count = us
                    .host_contexts_sorted()
                    .into_iter()
                    .filter(|(route, _, _)| route.peek() == Some(link))
                    .count();
                (
                    link.as_str().to_string(),
                    connection.is_peer(),
                    direct_host_count,
                    routed_host_count,
                )
            })
            .collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        let mut seq = serializer.serialize_seq(Some(entries.len()))?;
        for (link, is_peer, direct_host_count, routed_host_count) in &entries {
            seq.serialize_element(&RouteEntry {
                link,
                kind: if *is_peer { "peer" } else { "local_client" },
                direct_host_count: *direct_host_count,
                routed_host_count: *routed_host_count,
            })?;
        }
        seq.end()
    }
}

struct RouteEntry<'a> {
    link: &'a str,
    kind: &'static str,
    direct_host_count: usize,
    routed_host_count: usize,
}

impl Serialize for RouteEntry<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry("link", self.link)?;
        map.serialize_entry("kind", self.kind)?;
        map.serialize_entry("direct_host_count", &self.direct_host_count)?;
        map.serialize_entry("routed_host_count", &self.routed_host_count)?;
        map.end()
    }
}

struct HostsView<'a> {
    user_state: &'a ServerUserState,
    host_agent_counts: &'a std::collections::HashMap<Uuid, usize>,
}

impl Serialize for HostsView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut hosts: Vec<(&crate::protocol::Route, &Host)> = self
            .user_state
            .host_contexts_sorted()
            .into_iter()
            .map(|(route, host, _)| (route, host))
            .collect();
        hosts.sort_unstable_by(|(route_a, host_a), (route_b, host_b)| {
            route_a
                .to_string()
                .cmp(&route_b.to_string())
                .then_with(|| host_a.name.cmp(&host_b.name))
                .then_with(|| host_a.id.as_u128().cmp(&host_b.id.as_u128()))
        });

        let mut seq = serializer.serialize_seq(Some(hosts.len()))?;
        for (route, host) in hosts {
            seq.serialize_element(&HostEntry {
                route,
                host,
                agent_count: self.host_agent_counts.get(&host.id).copied().unwrap_or(0),
            })?;
        }
        seq.end()
    }
}

struct HostEntry<'a> {
    route: &'a crate::protocol::Route,
    host: &'a Host,
    agent_count: usize,
}

impl Serialize for HostEntry<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", &self.host.id)?;
        map.serialize_entry("name", &self.host.name)?;
        map.serialize_entry("version", &self.host.version)?;
        map.serialize_entry("route", self.route)?;
        if let Some(via) = self.route.peek() {
            map.serialize_entry("via", via)?;
        }
        map.serialize_entry("agent_count", &self.agent_count)?;
        map.end()
    }
}

struct AgentsView<'a> {
    state: &'a ServerState,
    user_state: &'a ServerUserState,
    agents: &'a [Agent],
    verbose: bool,
}

impl Serialize for AgentsView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let us = self.user_state;

        let mut sorted: Vec<&Agent> = self.agents.iter().collect();
        sorted.sort_unstable_by(|a, b| {
            a.is_remote()
                .cmp(&b.is_remote())
                .then_with(|| a.route.to_string().cmp(&b.route.to_string()))
                .then_with(|| {
                    a.name
                        .as_deref()
                        .unwrap_or("")
                        .cmp(b.name.as_deref().unwrap_or(""))
                })
                .then_with(|| a.id.as_u128().cmp(&b.id.as_u128()))
        });

        let mut seq = serializer.serialize_seq(Some(sorted.len()))?;
        for agent in sorted {
            let host_name = if agent.host_id == self.state.host_id {
                Some(self.state.config.host_name.as_str())
            } else {
                us.host_contexts_sorted()
                    .into_iter()
                    .find_map(|(_, host, _)| {
                        (host.id == agent.host_id).then_some(host.name.as_str())
                    })
            };
            seq.serialize_element(&AgentEntry {
                agent,
                host_name,
                session: us
                    .local_agents
                    .get(&agent.id)
                    .map(|context| &context.session),
                verbose: self.verbose,
            })?;
        }
        seq.end()
    }
}

struct AgentEntry<'a> {
    agent: &'a Agent,
    host_name: Option<&'a str>,
    session: Option<&'a crate::agent::AgentSession>,
    verbose: bool,
}

impl Serialize for AgentEntry<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let agent = self.agent;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", &agent.id)?;
        map.serialize_entry("host_id", &agent.host_id)?;
        if let Some(host_name) = self.host_name {
            map.serialize_entry("host_name", host_name)?;
        }
        if let Some(name) = &agent.name {
            map.serialize_entry("name", name)?;
        }
        map.serialize_entry(
            "location",
            if agent.is_remote() { "remote" } else { "local" },
        )?;
        map.serialize_entry("route", &agent.route)?;
        if let Some(via) = agent.route.peek() {
            map.serialize_entry("via", via)?;
        }
        map.serialize_entry("agent_type", &agent.agent_type)?;
        map.serialize_entry("io_protocols", &agent.io_protocols)?;
        map.serialize_entry("readonly", &agent.readonly)?;
        map.serialize_entry("command", &agent.command)?;
        map.serialize_entry("working_dir", &LossyPath(&agent.working_dir))?;
        map.serialize_entry("args", &agent.args)?;
        map.serialize_entry("created_at", &agent.created_at)?;
        if let Some(session) = self.session {
            map.serialize_entry("session", &DebugView::new(session, self.verbose))?;
        }
        map.end()
    }
}

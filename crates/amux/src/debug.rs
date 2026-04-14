//! Server debug rendering — `DebugView` newtype + per-type Serialize impls.
//!
//! The `Debug` command produces a human-readable dump of server state in either
//! YAML or JSON. To keep the protocol module free of debug-only types and avoid
//! a centralized "builder" that has to know the shape of every subsystem, each
//! type that participates in the dump owns its own `Serialize for DebugView<…>`
//! impl, colocated with the type's source file.
//!
//! Format selection is purely a matter of which `Serializer` we hand to serde:
//! the `DebugView` impls don't know — and don't need to know — anything about
//! YAML vs JSON. The wire format is just `String`; the CLI prints it directly.

use crate::message::{DebugFormat, Host};
use crate::server::{
    LOCAL_USER_ID, ServerState, ServerUserState, SubscriptionEntry, SubscriptionMode,
};
use crate::state::State;
use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{RwLock, RwLockReadGuard};
use uuid::Uuid;

/// Serde-driven debug rendering wrapper.
///
/// Wrapping a borrowed `T` and a `verbose` flag lets each type expose its own
/// debug representation as `impl Serialize for DebugView<'_, T>`, with no
/// debug-only fields or accessors leaking onto `T` itself.
pub(crate) struct DebugView<'a, T: ?Sized> {
    pub inner: &'a T,
    pub verbose: bool,
}

impl<'a, T: ?Sized> DebugView<'a, T> {
    pub fn new(inner: &'a T, verbose: bool) -> Self {
        Self { inner, verbose }
    }
}

/// Infallible path serialization wrapper.
///
/// `Path`'s default `Serialize` impl returns an error for non-UTF8 byte
/// sequences, which would abort the entire debug dump for one bad agent
/// `working_dir`. `LossyPath` substitutes U+FFFD for invalid bytes via
/// `to_string_lossy()`, so debug output can never fail on path encoding.
pub(crate) struct LossyPath<'a>(pub &'a Path);

impl Serialize for LossyPath<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string_lossy())
    }
}

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

    let use_cloud_mode = State::load(&state_guard.config.state_path)
        .map(|s| s.cloud.use_cloud_mode == Some(true))
        .unwrap_or(false);

    // Acquire per-user read guards up front so the sync Serialize phase can
    // borrow them without further awaits. Sort by user_id for stable output.
    let mut users: Vec<(Uuid, Arc<RwLock<ServerUserState>>)> = state_guard
        .users
        .iter()
        .map(|(id, us)| (*id, us.clone()))
        .collect();
    users.sort_unstable_by(|a, b| a.0.as_u128().cmp(&b.0.as_u128()));
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
        let mut subscription_count = 0usize;
        for (_, us) in self.users {
            agent_count += us.agents.len();
            remote_agent_count += us.registry.count_remote();
            host_count += us.hosts.len();
            route_count += us.routes.len();
            peer_link_count += us.peer_links.len();
            subscription_count += us.active_subscriptions.len();
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
        map.serialize_entry("subscription_count", &subscription_count)?;
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

/// Per-user view: counts plus verbose details (peer_links, routes, hosts,
/// agents, subscriptions). Reads `ServerUserState` directly via the guard.
struct UserView<'a> {
    state: &'a ServerState,
    user_id: Uuid,
    user_state: &'a ServerUserState,
    verbose: bool,
}

impl Serialize for UserView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let us = self.user_state;
        let agents = us.registry.list_all(&us.hosts);

        let mut host_agent_counts: std::collections::HashMap<Uuid, usize> =
            std::collections::HashMap::new();
        let mut agent_name_by_id: std::collections::HashMap<Uuid, Option<String>> =
            std::collections::HashMap::new();
        for agent in &agents {
            *host_agent_counts.entry(agent.host_id).or_default() += 1;
            agent_name_by_id.insert(agent.id, agent.name.clone());
        }

        let mut peer_links: Vec<&str> = us.peer_links.iter().map(String::as_str).collect();
        peer_links.sort_unstable();

        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry(
            "user_id",
            &if self.user_id == LOCAL_USER_ID {
                "local".to_string()
            } else {
                self.user_id.to_string()
            },
        )?;
        map.serialize_entry("agent_count", &us.agents.len())?;
        map.serialize_entry("remote_agent_count", &us.registry.count_remote())?;
        map.serialize_entry("host_count", &us.hosts.len())?;
        map.serialize_entry("route_count", &us.routes.len())?;
        map.serialize_entry("peer_link_count", &us.peer_links.len())?;
        map.serialize_entry("subscription_count", &us.active_subscriptions.len())?;
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
        map.serialize_entry(
            "subscriptions",
            &SubscriptionsView {
                user_state: us,
                agent_name_by_id: &agent_name_by_id,
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
        let mut entries: Vec<(&String, bool, usize, usize)> = us
            .routes
            .keys()
            .map(|link_name| {
                let is_peer = us.peer_links.contains(link_name);
                let direct_host_count = us
                    .hosts
                    .values()
                    .filter(|host| host.route.to_string() == *link_name)
                    .count();
                let routed_host_count = us
                    .hosts
                    .values()
                    .filter(|host| host.route.peek() == Some(link_name.as_str()))
                    .count();
                (link_name, is_peer, direct_host_count, routed_host_count)
            })
            .collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(b.0));

        let mut seq = serializer.serialize_seq(Some(entries.len()))?;
        for (link_name, is_peer, direct_host_count, routed_host_count) in entries {
            seq.serialize_element(&RouteEntry {
                link_name,
                kind: if is_peer { "peer" } else { "local_client" },
                direct_host_count,
                routed_host_count,
            })?;
        }
        seq.end()
    }
}

struct RouteEntry<'a> {
    link_name: &'a str,
    kind: &'static str,
    direct_host_count: usize,
    routed_host_count: usize,
}

impl Serialize for RouteEntry<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry("link_name", self.link_name)?;
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
        let mut hosts: Vec<&Host> = self.user_state.hosts.values().collect();
        hosts.sort_unstable_by(|a, b| {
            a.route
                .to_string()
                .cmp(&b.route.to_string())
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.id.as_u128().cmp(&b.id.as_u128()))
        });

        let mut seq = serializer.serialize_seq(Some(hosts.len()))?;
        for host in hosts {
            seq.serialize_element(&HostEntry {
                host,
                agent_count: self.host_agent_counts.get(&host.id).copied().unwrap_or(0),
            })?;
        }
        seq.end()
    }
}

struct HostEntry<'a> {
    host: &'a Host,
    agent_count: usize,
}

impl Serialize for HostEntry<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", &self.host.id)?;
        map.serialize_entry("name", &self.host.name)?;
        map.serialize_entry("version", &self.host.version)?;
        map.serialize_entry("route", &self.host.route)?;
        if let Some(via) = self.host.route.peek() {
            map.serialize_entry("via", via)?;
        }
        map.serialize_entry("agent_count", &self.agent_count)?;
        map.end()
    }
}

struct AgentsView<'a> {
    state: &'a ServerState,
    user_state: &'a ServerUserState,
    agents: &'a [crate::agent_registry::Agent],
    verbose: bool,
}

impl Serialize for AgentsView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let us = self.user_state;

        // Subscription counts per agent — used by the per-agent entry below.
        let mut sub_counts: std::collections::HashMap<Uuid, (usize, usize, usize)> =
            std::collections::HashMap::new();
        for entry in us.active_subscriptions.values() {
            let counts = sub_counts.entry(entry.agent_id).or_default();
            counts.0 += 1;
            match entry.mode {
                SubscriptionMode::Raw => counts.1 += 1,
                SubscriptionMode::Structured => counts.2 += 1,
            }
        }

        let mut sorted: Vec<&crate::agent_registry::Agent> = self.agents.iter().collect();
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
                us.hosts.get(&agent.host_id).map(|host| host.name.as_str())
            };
            let counts = sub_counts.get(&agent.id).copied().unwrap_or_default();
            seq.serialize_element(&AgentEntry {
                agent,
                host_name,
                subscription_count: counts.0,
                raw_subscription_count: counts.1,
                structured_subscription_count: counts.2,
                session: us.agents.get(&agent.id),
                verbose: self.verbose,
            })?;
        }
        seq.end()
    }
}

struct AgentEntry<'a> {
    agent: &'a crate::agent_registry::Agent,
    host_name: Option<&'a str>,
    subscription_count: usize,
    raw_subscription_count: usize,
    structured_subscription_count: usize,
    session: Option<&'a crate::agents::AgentSession>,
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
        if let Some(structured_protocol) = &agent.structured_protocol {
            map.serialize_entry("structured_protocol", structured_protocol)?;
        }
        map.serialize_entry("readonly", &agent.readonly)?;
        map.serialize_entry("command", &agent.command)?;
        map.serialize_entry("working_dir", &LossyPath(&agent.working_dir))?;
        map.serialize_entry("args", &agent.args)?;
        map.serialize_entry("created_at", &agent.created_at)?;
        map.serialize_entry("subscription_count", &self.subscription_count)?;
        map.serialize_entry("raw_subscription_count", &self.raw_subscription_count)?;
        map.serialize_entry(
            "structured_subscription_count",
            &self.structured_subscription_count,
        )?;
        if let Some(session) = self.session {
            map.serialize_entry("session", &DebugView::new(session, self.verbose))?;
        }
        map.end()
    }
}

struct SubscriptionsView<'a> {
    user_state: &'a ServerUserState,
    agent_name_by_id: &'a std::collections::HashMap<Uuid, Option<String>>,
}

impl Serialize for SubscriptionsView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let now = tokio::time::Instant::now();
        let mut entries: Vec<&SubscriptionEntry> =
            self.user_state.active_subscriptions.values().collect();
        entries.sort_unstable_by(|a, b| {
            a.agent_id
                .as_u128()
                .cmp(&b.agent_id.as_u128())
                .then_with(|| {
                    a.subscription_id
                        .as_u128()
                        .cmp(&b.subscription_id.as_u128())
                })
        });

        let mut seq = serializer.serialize_seq(Some(entries.len()))?;
        for entry in entries {
            let lease_remaining_ms: u64 = entry
                .lease_deadline
                .checked_duration_since(now)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX);
            seq.serialize_element(&SubscriptionEntryView {
                entry,
                agent_name: self
                    .agent_name_by_id
                    .get(&entry.agent_id)
                    .and_then(|n| n.as_deref()),
                lease_remaining_ms,
            })?;
        }
        seq.end()
    }
}

struct SubscriptionEntryView<'a> {
    entry: &'a SubscriptionEntry,
    agent_name: Option<&'a str>,
    lease_remaining_ms: u64,
}

impl Serialize for SubscriptionEntryView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let entry = self.entry;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", &entry.subscription_id)?;
        map.serialize_entry("agent_id", &entry.agent_id)?;
        if let Some(name) = self.agent_name {
            map.serialize_entry("agent_name", name)?;
        }
        map.serialize_entry(
            "mode",
            match entry.mode {
                SubscriptionMode::Raw => "raw",
                SubscriptionMode::Structured => "structured",
            },
        )?;
        map.serialize_entry("dst", &entry.dst)?;
        if let Some(via) = entry.dst.peek() {
            map.serialize_entry("via", via)?;
        }
        map.serialize_entry("lease_remaining_ms", &self.lease_remaining_ms)?;
        map.end()
    }
}

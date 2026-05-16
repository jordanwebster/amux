use std::sync::Arc;

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use tokio::sync::{RwLock, RwLockReadGuard};
use uuid::Uuid;

use crate::agents::AgentRecord;
use crate::debug::{DebugFormat, DebugView, LossyPath};
use crate::services::AgentServiceState;
use crate::setup;
use crate::user_state::ServerState;

pub(crate) async fn dump_server_debug_info(
    state: &Arc<RwLock<ServerState>>,
    format: DebugFormat,
    verbose: bool,
) -> String {
    let state_guard = state.read().await;
    let use_cloud_mode = setup::cloud_enabled(&state_guard.config);

    let local_agent_state = state_guard.local_agent_service();
    let local_agent_guard = local_agent_state.read().await;
    let users = [("local", local_agent_guard)];

    let view = ServerDebugView {
        state: &state_guard,
        use_cloud_mode,
        local_version: env!("CARGO_PKG_VERSION"),
        users: &users,
        verbose,
    };

    match format {
        DebugFormat::Yaml => serde_yaml::to_string(&view).unwrap_or_default(),
        DebugFormat::Json => serde_json::to_string_pretty(&view).unwrap_or_default(),
    }
}

struct ServerDebugView<'a> {
    state: &'a ServerState,
    use_cloud_mode: bool,
    local_version: &'static str,
    users: &'a [(&'a str, RwLockReadGuard<'a, AgentServiceState>)],
    verbose: bool,
}

impl Serialize for ServerDebugView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let agent_count: usize = self.users.iter().map(|(_, us)| us.local_agents.len()).sum();

        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("is_cloud_server", &self.state.is_cloud_server)?;
        map.serialize_entry("use_cloud_mode", &self.use_cloud_mode)?;
        map.serialize_entry("user_count", &self.users.len())?;
        map.serialize_entry("agent_count", &agent_count)?;
        map.serialize_entry("remote_agent_count", &0usize)?;
        map.serialize_entry("host_count", &0usize)?;
        map.serialize_entry("route_count", &0usize)?;
        map.serialize_entry("peer_link_count", &0usize)?;
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

fn debug_agents(us: &AgentServiceState, host_id: Uuid) -> Vec<AgentRecord> {
    let mut agents: Vec<_> = us
        .local_agents
        .values()
        .map(|context| context.session.to_agent(host_id))
        .collect();
    agents.sort_unstable_by(|a, b| {
        a.name
            .as_deref()
            .unwrap_or("")
            .cmp(b.name.as_deref().unwrap_or(""))
            .then_with(|| a.id.as_u128().cmp(&b.id.as_u128()))
    });
    agents
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
    users: &'a [(&'a str, RwLockReadGuard<'a, AgentServiceState>)],
    verbose: bool,
}

impl Serialize for UsersListView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.users.len()))?;
        for (user_id, us) in self.users {
            seq.serialize_element(&UserView {
                state: self.state,
                user_id,
                user_state: us,
                verbose: self.verbose,
            })?;
        }
        seq.end()
    }
}

struct UserView<'a> {
    state: &'a ServerState,
    user_id: &'a str,
    user_state: &'a AgentServiceState,
    verbose: bool,
}

impl Serialize for UserView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let us = self.user_state;
        let agents = debug_agents(us, self.state.host_id);

        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("user_id", self.user_id)?;
        map.serialize_entry("agent_count", &us.local_agents.len())?;
        map.serialize_entry("remote_agent_count", &0usize)?;
        map.serialize_entry("host_count", &0usize)?;
        map.serialize_entry("route_count", &0usize)?;
        map.serialize_entry("peer_link_count", &0usize)?;
        map.serialize_entry("peer_links", &Vec::<String>::new())?;
        map.serialize_entry("routes", &EmptySeq)?;
        map.serialize_entry("hosts", &EmptySeq)?;
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

struct EmptySeq;

impl Serialize for EmptySeq {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_seq(Some(0))?.end()
    }
}

struct AgentsView<'a> {
    state: &'a ServerState,
    user_state: &'a AgentServiceState,
    agents: &'a [AgentRecord],
    verbose: bool,
}

impl Serialize for AgentsView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.agents.len()))?;
        for agent in self.agents {
            let host_name = (agent.host_id == self.state.host_id)
                .then_some(self.state.config.host_name.as_str());
            seq.serialize_element(&AgentDebugEntry {
                agent,
                host_name,
                session: self
                    .user_state
                    .local_agents
                    .get(&agent.id)
                    .map(|context| &context.session),
                verbose: self.verbose,
            })?;
        }
        seq.end()
    }
}

struct AgentDebugEntry<'a> {
    agent: &'a AgentRecord,
    host_name: Option<&'a str>,
    session: Option<&'a crate::agents::AgentSession>,
    verbose: bool,
}

impl Serialize for AgentDebugEntry<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let info = self.agent;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", &info.id)?;
        map.serialize_entry("host_id", &info.host_id)?;
        if let Some(host_name) = self.host_name {
            map.serialize_entry("host_name", host_name)?;
        }
        if let Some(name) = &info.name {
            map.serialize_entry("name", name)?;
        }
        map.serialize_entry("location", "local")?;
        map.serialize_entry("agent_type", &info.agent_type)?;
        map.serialize_entry("io_protocols", &info.io_protocols)?;
        map.serialize_entry("readonly", &info.readonly)?;
        map.serialize_entry("command", &info.command)?;
        map.serialize_entry("working_dir", &LossyPath(&info.working_dir))?;
        map.serialize_entry("args", &info.args)?;
        map.serialize_entry("created_at", &info.created_at)?;
        if let Some(session) = self.session {
            map.serialize_entry("session", &DebugView::new(session, self.verbose))?;
        }
        map.end()
    }
}

//! The phone's chats and fleet over the local runtime.
//!
//! [`AppRuntime`] hosts the session and fleet drivers a rich client needs
//! and gathers what they change until the host's main thread runs next: a
//! host is woken once per turn with the chat or the fleet that moved, and
//! takes every changed key together. Views are asked for by row key, so a
//! host reconfigures only the cells an update changed.
//!
//! Nothing here opens a store or knows the node: the runtime is reached
//! through a [`client::Client`], in process on the phone.

mod chat;
mod coalesce;
pub mod values;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

pub use chat::Chat;
use client::{Client, Clock, RpcError};
pub use coalesce::{Batch, Coalescer, WakeFn};
use model::AgentKey;
use tokio::task::JoinHandle;
use ui_runtime::Fleet;
use ui_view::{FamilyHeader, FleetCard, FleetRow};
use values::{AgentAct, Directories, Directory, NewAgent};
pub use values::{FleetChanges, HostView};
use wire::{
    CreateAgentRequest, DeleteAgentRequest, DumpRequest, Kind, ListRepositoriesRequest,
    ProjectEntry, RenameAgentRequest, StopAgentRequest, StopMode, Trust, create_agent_request,
};

/// How many rows a chat opens with unless the host says otherwise.
pub const DEFAULT_TAIL: u32 = 200;

/// What moved: the fleet, or the chat with this id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    Fleet,
    Chat(u64),
}

/// The host's wake. Called on a worker thread; it schedules the host's
/// read on its main thread and returns.
pub type HostWake = Arc<dyn Fn(Wake) + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("no agent {0:?} in the fleet")]
    NoAgent(AgentKey),
    #[error(transparent)]
    Rpc(#[from] RpcError),
}

/// The drivers of one profile's chats and fleet.
pub struct AppRuntime {
    client: Arc<dyn Client>,
    clock: Arc<dyn Clock>,
    local_host: Vec<u8>,
    wake: HostWake,
    fleet: Arc<Fleet>,
    fleet_changes: Arc<Coalescer<AgentKey>>,
    fleet_watcher: JoinHandle<()>,
    chats: Arc<Mutex<HashMap<u64, Weak<Chat>>>>,
    next_chat: AtomicU64,
}

impl AppRuntime {
    /// Opens the fleet and resolves once it has caught up with the local
    /// runtime. `local_host` is this device's host id.
    pub async fn open(
        client: Arc<dyn Client>,
        clock: Arc<dyn Clock>,
        local_host: Vec<u8>,
        wake: HostWake,
    ) -> Result<AppRuntime, RpcError> {
        let fleet = Arc::new(Fleet::open(client.clone(), clock.clone()).await?);
        let fleet_wake = wake.clone();
        let fleet_changes = Arc::new(Coalescer::new(Arc::new(move || fleet_wake(Wake::Fleet))));
        let chats: Arc<Mutex<HashMap<u64, Weak<Chat>>>> = Arc::default();
        let fleet_watcher = tokio::spawn(watch_fleet(
            Arc::downgrade(&fleet),
            fleet_changes.clone(),
            chats.clone(),
        ));
        Ok(AppRuntime {
            client,
            clock,
            local_host,
            wake,
            fleet,
            fleet_changes,
            fleet_watcher,
            chats,
            next_chat: AtomicU64::new(1),
        })
    }

    pub fn client(&self) -> &Arc<dyn Client> {
        &self.client
    }

    pub fn fleet(&self) -> &Fleet {
        &self.fleet
    }

    /// Opens a chat on a fleet agent. Resolves once the snapshot and the
    /// rows the runtime holds are applied, so the host's first read is
    /// correct: at once for rows this device already holds, even with the
    /// agent's host away.
    pub async fn open_chat(&self, agent: &AgentKey, tail: u32) -> Result<Arc<Chat>, OpenError> {
        let (entry, host) = {
            let fleet = self.fleet.state();
            let entry = fleet
                .agent(agent)
                .cloned()
                .ok_or_else(|| OpenError::NoAgent(agent.clone()))?;
            let host = fleet.host(&entry.host_id).cloned();
            (entry, host)
        };
        let session =
            ui_runtime::Session::open(self.client.clone(), entry, tail, self.clock.clone()).await?;
        if let Some(host) = host {
            session.set_host(host);
        }
        let id = self.next_chat.fetch_add(1, Ordering::Relaxed);
        let wake = self.wake.clone();
        let chat = Chat::start(
            id,
            session,
            Arc::new(move || wake(Wake::Chat(id))),
            self.clock.clone(),
        );
        let mut chats = self.chats.lock().unwrap_or_else(|p| p.into_inner());
        chats.retain(|_, chat| chat.strong_count() > 0);
        chats.insert(id, Arc::downgrade(&chat));
        Ok(chat)
    }

    /// The fleet as a list: families under their roots, expanded where
    /// `expand` names the root's agent id.
    pub fn fleet_rows(&self, expand: &[Vec<u8>]) -> Vec<FleetRow> {
        let expand = expand.iter().cloned().collect();
        ui_view::fleet_list(&self.fleet.state(), &expand)
    }

    pub fn fleet_card(&self, agent: &AgentKey) -> Option<FleetCard> {
        ui_view::fleet_card(&self.fleet.state(), &agent.agent)
    }

    pub fn family_header(&self, agent: &AgentKey) -> Option<FamilyHeader> {
        ui_view::family_header(&self.fleet.state(), &agent.agent)
    }

    pub fn hosts(&self) -> Vec<HostView> {
        let fleet = self.fleet.state();
        let mut hosts: Vec<HostView> = fleet
            .hosts()
            .map(|host| HostView {
                host_id: host.host_id.clone(),
                name: host.name.clone(),
                local: host.host_id == self.local_host,
                trusted: host.trust() == Trust::Trusted,
                candidate: host.trust() == Trust::Candidate,
                presence: host.presence(),
                away: ui_view::away(&fleet, &self.local_host, &host.host_id),
                platform: host.platform.clone(),
                version: host.version.clone(),
                last_dial_error: host.last_dial_error.clone(),
                addrs: host.addrs.clone(),
                via: host.via(),
                signed_in: host.signed_in,
            })
            .collect();
        hosts.sort_by(|a, b| (!a.local, &a.name).cmp(&(!b.local, &b.name)));
        hosts
    }

    pub fn take_fleet_changes(&self) -> FleetChanges {
        let batch = self.fleet_changes.take();
        FleetChanges {
            agents: batch.keys,
            hosts: batch.other,
        }
    }

    /// Starts an agent on a host and returns it as the fleet names it.
    pub async fn create_agent(&self, agent: &NewAgent) -> Result<AgentKey, RpcError> {
        let config = match agent.kind {
            Kind::Codex => Some(create_agent_request::Config::Codex(
                wire::CodexCreateConfig {
                    model: agent.model.clone(),
                    ..Default::default()
                },
            )),
            Kind::ClaudeSdk | Kind::ClaudePty => Some(create_agent_request::Config::Claude(
                wire::ClaudeCreateConfig {
                    model: agent.model.clone(),
                    ..Default::default()
                },
            )),
            Kind::Unspecified => None,
        };
        let created = self
            .client
            .create_agent(CreateAgentRequest {
                agent_id: ui_runtime::inputs::input_id(),
                host_id: Some(agent.host_id.clone()),
                name: Some(agent.name.clone()).filter(|name| !name.is_empty()),
                cwd: agent.cwd.clone(),
                kind: agent.kind as i32,
                config,
                ..Default::default()
            })
            .await?;
        Ok(ui_state::agent_key(&created))
    }

    /// Where a host offers to start an agent, filtered by `query` and at
    /// most `limit` of each.
    pub async fn directories(
        &self,
        host_id: &[u8],
        query: &str,
        limit: u32,
    ) -> Result<Directories, RpcError> {
        let listed = self
            .client
            .list_repositories(ListRepositoriesRequest {
                query: Some(query.to_owned()).filter(|query| !query.is_empty()),
                limit,
                host_id: Some(host_id.to_vec()),
            })
            .await?;
        let directory = |entry: ProjectEntry| Directory {
            path: entry.path,
            name: entry.name,
            last_used_ms: entry.last_used_unix_ms,
        };
        Ok(Directories {
            recent: listed.recent.into_iter().map(directory).collect(),
            repositories: listed.repositories.into_iter().map(directory).collect(),
            roots: listed.roots,
        })
    }

    /// Renames, stops or deletes an agent.
    pub async fn agent_act(&self, agent: &AgentKey, act: &AgentAct) -> Result<(), RpcError> {
        let agent_id = agent.agent.clone();
        match act {
            AgentAct::Rename(name) => self
                .client
                .rename_agent(RenameAgentRequest {
                    agent_id,
                    name: name.clone(),
                })
                .await
                .map(drop),
            AgentAct::Stop => {
                self.client
                    .stop_agent(StopAgentRequest {
                        agent_id,
                        mode: StopMode::Graceful as i32,
                    })
                    .await
            }
            AgentAct::Delete => self
                .client
                .delete_agent(DeleteAgentRequest { agent_id })
                .await
                .map(drop),
        }
    }

    /// Writes a dump of the profile, with the fleet's part and every open
    /// chat's, and returns the bundle's directory.
    pub async fn dump(&self, reason: &str) -> Result<PathBuf, RpcError> {
        let response = self
            .client
            .dump(DumpRequest {
                agent_ids: Vec::new(),
                reason: reason.to_owned(),
                automatic: false,
            })
            .await?;
        let bundle = PathBuf::from(response.report_path);
        let mut parts = vec![self.fleet.dump_part()];
        let chats: Vec<Arc<Chat>> = {
            let chats = self.chats.lock().unwrap_or_else(|p| p.into_inner());
            chats.values().filter_map(Weak::upgrade).collect()
        };
        parts.extend(chats.iter().map(|chat| chat.session().dump_part()));
        for part in &parts {
            write_part(&bundle, part)?;
        }
        Ok(bundle)
    }
}

fn write_part(bundle: &Path, part: &wire::DumpPart) -> Result<(), RpcError> {
    ui_runtime::write_part(bundle, part)
        .map_err(|error| RpcError::Transport(format!("writing the dump: {error}")))
}

impl Drop for AppRuntime {
    fn drop(&mut self) {
        self.fleet_watcher.abort();
    }
}

/// Gathers the fleet's changes for the host and keeps every open chat's
/// entry and host current, as the terminal does from the same inventory.
async fn watch_fleet(
    fleet: Weak<Fleet>,
    changes: Arc<Coalescer<AgentKey>>,
    chats: Arc<Mutex<HashMap<u64, Weak<Chat>>>>,
) {
    let Some(mut changed) = fleet.upgrade().map(|fleet| fleet.changed()) else {
        return;
    };
    loop {
        let Some(fleet) = fleet.upgrade() else {
            return;
        };
        let agents = fleet.take_changed();
        let hosts = fleet.take_hosts_changed();
        let open: Vec<Arc<Chat>> = {
            let chats = chats.lock().unwrap_or_else(|p| p.into_inner());
            chats.values().filter_map(Weak::upgrade).collect()
        };
        for chat in open {
            let session = chat.session();
            let key = ui_state::agent_key(session.state().agent());
            let (entry, host) = {
                let state = fleet.state();
                let entry = state.agent(&key).cloned();
                let host = entry
                    .as_ref()
                    .and_then(|entry| state.host(&entry.host_id).cloned());
                (entry, host)
            };
            if let Some(entry) = entry.filter(|_| agents.contains(&key)) {
                session.set_entry(entry);
            }
            if let Some(host) = host {
                let same = session.state().host() == Some(&host);
                if !same {
                    session.set_host(host);
                }
            }
        }
        changes.push(agents, false, hosts);
        drop(fleet);
        if changed.changed().await.is_err() {
            return;
        }
    }
}

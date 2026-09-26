//! Serving: what every client call reads. Subscribe, Fetch and Get read an
//! agent's rows; SubscribeInventory and ResolveAgent read the fleet. All of
//! them are store reads plus, for the streams, the fan-out channels; none
//! touches an agent process.

use std::collections::VecDeque;
use std::sync::Arc;

use prost::{Message as _, Name as _};
use store::{AgentKey, AgentRow, Marker, PageEnd, Sqlite, Store as _, StoreError};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::{self};
use wire::{
    Agent, AmbiguousAgentName, CaughtUp, ErrorCode, ErrorDetail, FetchRequest, FetchResponse,
    GetRequest, HostEntry, InventoryEvent, Item, Lagged, Presence, SessionEvent, Snapshot, Trust,
    inventory_event, session_event,
};

use crate::runtime::{ProfileRuntime, to_wire};

/// The most items one Fetch returns, whatever the limit asked for.
pub const MAX_PAGE: u32 = 500;

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("no agent with that id")]
    NoAgent,
    #[error("no agent named {0}")]
    NoAgentNamed(String),
    #[error("{} agents are named {}", .0.candidates.len(), .0.name)]
    Ambiguous(AmbiguousAgentName),
    #[error("the agent holds no item {0}")]
    NoItem(String),
    #[error("older history is held by the agent's host, which cannot be reached")]
    OriginUnreachable,
    #[error("the store: {0}")]
    Store(#[from] StoreError),
}

impl ServeError {
    /// The error as the wire carries it: a code, and for an ambiguous name
    /// the candidates as an `amux.v1.AmbiguousAgentName` detail.
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoAgent | Self::NoAgentNamed(_) | Self::NoItem(_) => ErrorCode::NotFound,
            Self::Ambiguous(_) => ErrorCode::FailedPrecondition,
            Self::OriginUnreachable => ErrorCode::Unreachable,
            Self::Store(_) => ErrorCode::Internal,
        };
        let details = match self {
            Self::Ambiguous(ambiguous) => vec![ErrorDetail {
                r#type: AmbiguousAgentName::full_name(),
                value: ambiguous.encode_to_vec(),
            }],
            _ => Vec::new(),
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details,
        }
    }
}

/// One Subscribe stream: the opening read under the store lock, then every
/// event the agent's channel carries, verbatim.
pub struct Subscription {
    opening: VecDeque<Arc<SessionEvent>>,
    live: broadcast::Receiver<Arc<SessionEvent>>,
    ended: bool,
}

impl Subscription {
    /// The next event, or None once the stream has ended: after Lagged, or
    /// when the agent is deleted.
    pub async fn next(&mut self) -> Option<Arc<SessionEvent>> {
        if let Some(event) = self.opening.pop_front() {
            return Some(event);
        }
        if self.ended {
            return None;
        }
        match self.live.recv().await {
            Ok(event) => Some(event),
            Err(RecvError::Lagged(_)) => {
                // The ring moved on without this reader; what it missed is
                // in the store, and the client re-tails to read it.
                self.ended = true;
                Some(Arc::new(event(session_event::Of::Lagged(Lagged {}))))
            }
            Err(RecvError::Closed) => {
                self.ended = true;
                None
            }
        }
    }
}

/// One SubscribeInventory stream: hosts and agent rows, CaughtUp, then
/// every change.
pub struct InventorySubscription {
    opening: VecDeque<Arc<InventoryEvent>>,
    live: broadcast::Receiver<Arc<InventoryEvent>>,
    ended: bool,
}

impl InventorySubscription {
    /// The next event, or None once the stream has ended. A reader that
    /// falls behind the ring is closed and subscribes again for a fresh
    /// current set.
    pub async fn next(&mut self) -> Option<Arc<InventoryEvent>> {
        if let Some(event) = self.opening.pop_front() {
            return Some(event);
        }
        if self.ended {
            return None;
        }
        match self.live.recv().await {
            Ok(event) => Some(event),
            Err(RecvError::Lagged(_) | RecvError::Closed) => {
                self.ended = true;
                None
            }
        }
    }
}

pub(crate) fn event(of: session_event::Of) -> SessionEvent {
    SessionEvent { of: Some(of) }
}

pub(crate) fn inventory(of: inventory_event::Of) -> InventoryEvent {
    InventoryEvent { of: Some(of) }
}

/// The row for an agent id, whichever host owns it.
fn row_by_id(store: &Sqlite, own_host: &[u8], agent_id: &[u8]) -> Result<AgentRow, ServeError> {
    if let Some(row) = store.agent(&AgentKey::new(own_host, agent_id))? {
        return Ok(row);
    }
    store
        .agents()?
        .into_iter()
        .find(|row| row.agent.agent == agent_id)
        .ok_or(ServeError::NoAgent)
}

impl ProfileRuntime {
    /// Opens a Subscribe stream on an agent: its Snapshot, the newest
    /// `tail` rows of its block, its marker if one is set, then every event
    /// its source broadcasts.
    pub async fn subscribe(&self, agent_id: &[u8], tail: u32) -> Result<Subscription, ServeError> {
        // The store stays locked from the join through the whole opening.
        // Commits and marker changes need the store exclusively and publish
        // while they hold it, so with the lock held nothing commits between
        // the join and the cut: every record is either in the opening or
        // arrives live, never both and never neither, and the marker the
        // cut reads describes the same point as its rows and Snapshot. A
        // Snapshot read on its own could pair a queue from before a
        // withdrawal with a CaughtUp from after it. The opening is built
        // into memory, so nothing here waits on the client with the lock
        // held; do not release it between the marker read and the row read.
        let store = self.store.lock().await;
        let row = row_by_id(&store, store.own_host(), agent_id)?;
        let live = self.fanout.join(&row.agent);
        let hook = self.join_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook().await;
        }
        let cut = store.cut(&row.agent, tail)?;
        let mut opening = VecDeque::with_capacity(cut.held.len() + 2);
        let snapshot = cut.snapshot.unwrap_or_else(|| Snapshot {
            // Nothing emitted yet: the kind tag and an empty body.
            agent: row.agent.agent.clone(),
            kind: row.kind.clone(),
            ..Snapshot::default()
        });
        opening.push_back(Arc::new(event(session_event::Of::Snapshot(snapshot))));
        for item in cut.held {
            opening.push_back(Arc::new(event(session_event::Of::Item(item))));
        }
        match cut.marker {
            Some(Marker::CaughtUp) => {
                let revision = if row.agent.host == store.own_host() {
                    row.next_revision.saturating_sub(1)
                } else {
                    row.source_cursor
                };
                opening.push_back(Arc::new(event(session_event::Of::CaughtUp(CaughtUp {
                    revision,
                }))));
            }
            Some(Marker::Detached) => {
                opening.push_back(Arc::new(event(session_event::Of::Detached(
                    wire::Detached {},
                ))));
            }
            None => {}
        }
        drop(store);
        Ok(Subscription {
            opening,
            live,
            ended: false,
        })
    }

    /// A page of older items by order, newest first. Held rows answer it;
    /// below a replica's block the history is at the origin, and an origin
    /// that cannot be asked is an error, never an empty page.
    pub async fn fetch(&self, request: &FetchRequest) -> Result<FetchResponse, ServeError> {
        let store = self.store.lock().await;
        let row = row_by_id(&store, store.own_host(), &request.agent_id)?;
        let page = store.page(
            &row.agent,
            request.before_order,
            request.limit.min(MAX_PAGE),
        )?;
        match page.end {
            PageEnd::Boundary if page.items.is_empty() => Err(ServeError::OriginUnreachable),
            end => Ok(FetchResponse {
                items: page.items,
                exhausted: end == PageEnd::Exhausted,
            }),
        }
    }

    /// One item in full.
    pub async fn get(&self, request: &GetRequest) -> Result<Item, ServeError> {
        let store = self.store.lock().await;
        let row = row_by_id(&store, store.own_host(), &request.agent_id)?;
        store
            .get(&row.agent, &request.key)?
            .ok_or_else(|| ServeError::NoItem(request.key.clone()))
    }

    /// Opens a SubscribeInventory stream: this host and every agent row the
    /// store holds, CaughtUp, then every change.
    pub async fn subscribe_inventory(&self) -> Result<InventorySubscription, ServeError> {
        // Locked across the join and the read for the reason Subscribe is:
        // every row change publishes while it holds the store.
        let store = self.store.lock().await;
        let live = self.fanout.join_inventory();
        let rows = store.agents()?;
        drop(store);
        let mut opening = VecDeque::with_capacity(rows.len() + 2);
        opening.push_back(Arc::new(inventory(inventory_event::Of::Host(
            self.host_entry(),
        ))));
        for row in &rows {
            opening.push_back(Arc::new(inventory(inventory_event::Of::Agent(to_wire(
                row,
            )))));
        }
        opening.push_back(Arc::new(inventory(inventory_event::Of::CaughtUp(
            CaughtUp::default(),
        ))));
        Ok(InventorySubscription {
            opening,
            live,
            ended: false,
        })
    }

    /// This host's entry: always trusted and online to itself.
    pub fn host_entry(&self) -> HostEntry {
        HostEntry {
            host_id: self.host().as_bytes().to_vec(),
            generation: self.generation(),
            trust: Trust::Trusted as i32,
            presence: Presence::Online as i32,
            version: Some(crate::VERSION.to_owned()),
            ..HostEntry::default()
        }
    }

    /// The one agent with this name across the fleet the store knows.
    pub async fn resolve_agent(&self, name: &str) -> Result<Agent, ServeError> {
        let mut matches: Vec<Agent> = self
            .store
            .lock()
            .await
            .agents()?
            .iter()
            .filter(|row| row.name.as_deref() == Some(name))
            .map(to_wire)
            .collect();
        match matches.len() {
            0 => Err(ServeError::NoAgentNamed(name.to_owned())),
            1 => Ok(matches.remove(0)),
            _ => Err(ServeError::Ambiguous(AmbiguousAgentName {
                name: name.to_owned(),
                candidates: matches,
            })),
        }
    }
}

//! Typed observations and the waiters built on them.
//!
//! An observer records a stream exactly as a client receives it and waits
//! for a predicate over everything seen so far. Waiting has three outcomes
//! and only one of them passes: the predicate held, the deadline passed
//! with it unmet ([`Stuck::Deadline`]), or the stream ended with it unmet
//! ([`Stuck::Closed`]), reported at once rather than at the deadline. The
//! polling waiters beside it hold to the same rule: a check that never
//! answers is a failure, never a pass.

use std::fmt::{self, Write as _};
use std::future::Future;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use wire::{InventoryEvent, SessionEvent, inventory_event, session_event};

/// How long an observation or a convergence waits by default. Real work on
/// loopback settles well inside it; a wait that reaches it is a hang.
pub const PATIENCE: Duration = Duration::from_secs(30);

/// How often the polling waiters look again.
const POLL: Duration = Duration::from_millis(20);

/// Why an observation or a wait did not pass.
#[derive(Debug, thiserror::Error)]
pub enum Stuck {
    #[error("{what}: nothing satisfied the predicate within {waited:?}; saw:\n{seen}")]
    Deadline {
        what: String,
        waited: Duration,
        seen: String,
    },
    #[error("{what}: the stream ended before the predicate held; saw:\n{seen}")]
    Closed { what: String, seen: String },
    #[error("{what}: the check itself did not answer within {waited:?}")]
    Hung { what: String, waited: Duration },
    #[error("{what}: the condition broke after {after:?} of {window:?}")]
    Broke {
        what: String,
        after: Duration,
        window: Duration,
    },
}

/// One line per event, the way a failure report and a transcript show it.
pub trait Describe {
    fn describe(&self) -> String;
}

/// A stream's events as a client saw them, in order.
pub struct ObserverOf<E> {
    what: String,
    events: Vec<E>,
    incoming: mpsc::UnboundedReceiver<E>,
    closed: bool,
    reader: JoinHandle<()>,
}

/// A Subscribe stream on one agent.
pub type Observer = ObserverOf<SessionEvent>;
/// A SubscribeInventory stream on one host.
pub type InventoryObserver = ObserverOf<InventoryEvent>;

impl<E: Describe + Clone + Send + 'static> ObserverOf<E> {
    /// An observer over events a reader task sends into `incoming`.
    pub(crate) fn new(
        what: String,
        incoming: mpsc::UnboundedReceiver<E>,
        reader: JoinHandle<()>,
    ) -> Self {
        Self {
            what,
            events: Vec::new(),
            incoming,
            closed: false,
            reader,
        }
    }

    /// Waits until `pred` holds over everything seen, and returns it.
    pub async fn observe_until(
        &mut self,
        pred: impl Fn(&[E]) -> bool,
        deadline: Duration,
    ) -> Result<&[E], Stuck> {
        let until = Instant::now() + deadline;
        loop {
            // Take everything already delivered before judging, so the
            // predicate sees the stream as far as it has come.
            while let Ok(event) = self.incoming.try_recv() {
                self.events.push(event);
            }
            if pred(&self.events) {
                return Ok(&self.events);
            }
            if self.closed {
                return Err(Stuck::Closed {
                    what: self.what.clone(),
                    seen: self.transcript(),
                });
            }
            match tokio::time::timeout_at(until, self.incoming.recv()).await {
                Ok(Some(event)) => self.events.push(event),
                Ok(None) => self.closed = true,
                Err(_) => {
                    return Err(Stuck::Deadline {
                        what: self.what.clone(),
                        waited: deadline,
                        seen: self.transcript(),
                    });
                }
            }
        }
    }

    /// Everything seen so far, without waiting.
    pub fn events(&mut self) -> &[E] {
        while let Ok(event) = self.incoming.try_recv() {
            self.events.push(event);
        }
        &self.events
    }

    /// Whether the stream has ended, as far as the observer has read.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// The events seen so far, one described line each.
    pub fn transcript(&self) -> String {
        let mut out = String::new();
        for event in &self.events {
            let _ = writeln!(out, "  {}", event.describe());
        }
        out
    }
}

impl<E> Drop for ObserverOf<E> {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl<E> fmt::Debug for ObserverOf<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Observer")
            .field("what", &self.what)
            .field("seen", &self.events.len())
            .field("closed", &self.closed)
            .finish()
    }
}

/// What a session event is, without its payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    Snapshot,
    Item(String),
    Append(String),
    CaughtUp(u64),
    Lagged,
    Reset,
    Detached,
    Empty,
}

pub fn mark(event: &SessionEvent) -> Mark {
    match &event.of {
        Some(session_event::Of::Snapshot(_)) => Mark::Snapshot,
        Some(session_event::Of::Item(item)) => Mark::Item(item.key.clone()),
        Some(session_event::Of::Append(append)) => Mark::Append(append.key.clone()),
        Some(session_event::Of::CaughtUp(caught_up)) => Mark::CaughtUp(caught_up.revision),
        Some(session_event::Of::Lagged(_)) => Mark::Lagged,
        Some(session_event::Of::Reset(_)) => Mark::Reset,
        Some(session_event::Of::Detached(_)) => Mark::Detached,
        None => Mark::Empty,
    }
}

/// The session events' marks in order.
pub fn marks(events: &[SessionEvent]) -> Vec<Mark> {
    events.iter().map(mark).collect()
}

/// Whether a CaughtUp has been seen.
pub fn caught_up(events: &[SessionEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event.of, Some(session_event::Of::CaughtUp(_))))
}

/// Whether an inventory stream has reached its CaughtUp.
pub fn inventory_caught_up(events: &[InventoryEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event.of, Some(inventory_event::Of::CaughtUp(_))))
}

/// The newest state of every agent row an inventory stream has named,
/// without the ones it removed since.
pub fn inventory_agents(events: &[InventoryEvent]) -> Vec<wire::Agent> {
    let mut agents: Vec<wire::Agent> = Vec::new();
    for event in events {
        match &event.of {
            Some(inventory_event::Of::Agent(agent)) => {
                match agents
                    .iter_mut()
                    .find(|held| held.agent_id == agent.agent_id && held.host_id == agent.host_id)
                {
                    Some(held) => *held = agent.clone(),
                    None => agents.push(agent.clone()),
                }
            }
            Some(inventory_event::Of::AgentRemoved(removed)) => {
                agents.retain(|held| held.agent_id != removed.agent_id);
            }
            _ => {}
        }
    }
    agents
}

impl Describe for SessionEvent {
    fn describe(&self) -> String {
        match &self.of {
            Some(session_event::Of::Snapshot(snapshot)) => format!(
                "Snapshot revision={} kind={} phase={:?} queue={}",
                snapshot.revision,
                snapshot.kind,
                wire::Phase::try_from(snapshot.phase).unwrap_or(wire::Phase::Starting),
                snapshot.queue.len()
            ),
            Some(session_event::Of::Item(item)) => format!(
                "Item {} order={} revision={} kind={} text={:?}",
                item.key,
                item.order,
                item.revision,
                item.kind,
                clip(&item.text)
            ),
            Some(session_event::Of::Append(append)) => format!(
                "Append {} base={} revision={} text={:?}",
                append.key,
                append.base_revision,
                append.revision,
                clip(&append.text)
            ),
            Some(session_event::Of::CaughtUp(caught_up)) => {
                format!("CaughtUp revision={}", caught_up.revision)
            }
            Some(session_event::Of::Lagged(_)) => "Lagged".to_owned(),
            Some(session_event::Of::Reset(_)) => "Reset".to_owned(),
            Some(session_event::Of::Detached(_)) => "Detached".to_owned(),
            None => "(empty event)".to_owned(),
        }
    }
}

impl Describe for InventoryEvent {
    fn describe(&self) -> String {
        match &self.of {
            Some(inventory_event::Of::Host(host)) => format!(
                "Host {} generation={} trust={:?} presence={:?}",
                short(&host.host_id),
                host.generation,
                wire::Trust::try_from(host.trust).unwrap_or_default(),
                wire::Presence::try_from(host.presence).unwrap_or_default(),
            ),
            Some(inventory_event::Of::HostRemoved(removed)) => {
                format!("HostRemoved {}", short(&removed.host_id))
            }
            Some(inventory_event::Of::Agent(agent)) => format!(
                "Agent {} {} on {} lifecycle={:?} phase={:?}",
                agent.name.as_deref().unwrap_or("(unnamed)"),
                short(&agent.agent_id),
                short(&agent.host_id),
                wire::Lifecycle::try_from(agent.lifecycle).unwrap_or_default(),
                wire::Phase::try_from(agent.phase).unwrap_or(wire::Phase::Starting),
            ),
            Some(inventory_event::Of::AgentRemoved(removed)) => {
                format!("AgentRemoved {}", short(&removed.agent_id))
            }
            Some(inventory_event::Of::CaughtUp(_)) => "CaughtUp".to_owned(),
            None => "(empty event)".to_owned(),
        }
    }
}

fn clip(text: &str) -> String {
    const MAX: usize = 60;
    match text.char_indices().nth(MAX) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// The first eight hex digits of an id, as failure reports show it.
pub fn short(id: &[u8]) -> String {
    id.iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Waits until `check` holds. Fails at `deadline`, and fails as well when a
/// single check does not answer by then: a hung probe is not a pass.
pub async fn eventually<F>(
    what: &str,
    deadline: Duration,
    mut check: impl FnMut() -> F,
) -> Result<(), Stuck>
where
    F: Future<Output = bool>,
{
    let until = Instant::now() + deadline;
    loop {
        match tokio::time::timeout_at(until, check()).await {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(_) => {
                return Err(Stuck::Hung {
                    what: what.to_owned(),
                    waited: deadline,
                });
            }
        }
        if Instant::now() + POLL >= until {
            return Err(Stuck::Deadline {
                what: what.to_owned(),
                waited: deadline,
                seen: String::new(),
            });
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Holds `check` true for all of `window`, looking every poll. A false
/// answer fails at once; so does a check that does not answer within
/// [`PATIENCE`], which the old stability waiter wrongly took for a pass.
pub async fn holds_for<F>(
    what: &str,
    window: Duration,
    mut check: impl FnMut() -> F,
) -> Result<(), Stuck>
where
    F: Future<Output = bool>,
{
    let start = Instant::now();
    loop {
        match tokio::time::timeout(PATIENCE, check()).await {
            Ok(true) => {}
            Ok(false) => {
                return Err(Stuck::Broke {
                    what: what.to_owned(),
                    after: start.elapsed(),
                    window,
                });
            }
            Err(_) => {
                return Err(Stuck::Hung {
                    what: what.to_owned(),
                    waited: PATIENCE,
                });
            }
        }
        if start.elapsed() >= window {
            return Ok(());
        }
        tokio::time::sleep(POLL).await;
    }
}

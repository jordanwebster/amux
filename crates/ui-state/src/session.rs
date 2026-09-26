//! One open chat's state: the agent's entry and snapshot, the transcript
//! window, and this client's inputs.

use std::collections::{BTreeSet, HashMap};

use wire::{Agent, HostEntry, Input, Item, Kind, QueuedInput, SessionEvent, session_event};

use crate::Key;
use crate::body::{AgentState, ItemClass, OpenAsk};
use crate::inputs::{InputId, InputOutcome, InputState, InputWhat, Inputs, SentInput};
use crate::transcript::{Appended, Changed, Transcript};

/// The client's connection to its local runtime, as the driver reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Connection {
    #[default]
    Connecting,
    Live,
    /// The stream ended; the driver is re-tailing.
    Reconnecting,
}

/// Everything the driver forwards to a session.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Event(SessionEvent),
    /// An older page, requested under `epoch`. A page requested before a
    /// Reset swap describes a transcript that is gone and is dropped.
    Page {
        items: Vec<Item>,
        exhausted: bool,
        epoch: u64,
    },
    Connection(Connection),
    /// This client is sending an input: the optimistic row exists from now.
    Send(Input),
    /// What became of it.
    Sent(InputId, InputOutcome),
    /// The person discarded an input that was not confirmed.
    Discard(InputId),
    /// The agent's inventory row: lifecycle and the fleet's phase.
    Entry(Agent),
    /// The agent's host, for the header.
    Host(HostEntry),
    /// An attachment's bytes changed state in the driver's blob cache.
    Blob {
        hash: Vec<u8>,
        status: BlobStatus,
    },
}

/// What one update did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Every key whose row may differ, including a run's members when its
    /// attributes moved, and nothing else.
    pub changed: Vec<Key>,
    /// An Append whose base this client does not hold: the driver answers
    /// with Get and feeds the item back.
    pub need_get: Option<Key>,
    /// A Reset's transcript was swapped in: every row id may be new.
    pub reloaded: bool,
    /// Something outside the rows moved: the snapshot, entry, host, inputs,
    /// connection or caught-up state.
    pub session: bool,
}

/// An attachment's bytes in the driver's cache; rows hold a placeholder from
/// the reference's name, type and size until they are ready.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BlobStatus {
    #[default]
    Missing,
    Fetching,
    Ready,
    Failed(String),
}

/// What the composer offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Composer {
    Send,
    /// The agent has exited: the draft goes through ResumeAgent as the new
    /// incarnation's first prompt, one tap.
    Resume,
    /// Drafting continues; sending waits.
    Disabled(Waiting),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waiting {
    /// Rows are painted but the origin has not been reached yet.
    CatchingUp,
    /// The origin is not being followed; rows may be stale.
    Detached,
    /// The local runtime is being reconnected.
    Reconnecting,
}

/// The header's phase: lifecycle from the entry, the rest from the snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhaseView {
    Starting,
    Idle,
    Working,
    NeedsYou,
    Exited { cause: Option<String> },
}

/// The line above the composer while the agent works. Not a row; timed
/// against item timestamps with the caller's clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activity {
    pub kind: ActivityKind,
    pub since_ms: i64,
    pub elapsed_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityKind {
    /// Busy and nothing more specific applies.
    Working,
    /// A thinking block is open.
    Thinking,
    /// A tool is in flight; the key names the call.
    Running {
        key: Key,
    },
    /// The agent waits on its subagents.
    Subagents {
        count: u32,
    },
    Compacting,
    Retrying {
        attempt: u32,
        max_attempts: u32,
        retry_at_ms: Option<i64>,
    },
}

/// A queued prompt as the composer draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct QueueRow<'a> {
    pub entry: &'a QueuedInput,
    /// Sent by this client.
    pub mine: bool,
    /// Sent now into the running turn, by the queue's word or by this
    /// client's Send now in flight; reads "steered" until its reflection.
    pub steered: bool,
}

/// One open chat. No I/O, no clock, no handles, no view state; never
/// persisted, because the runtime can re-serve every record it was built
/// from.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionState {
    agent: Agent,
    host: Option<HostEntry>,
    state: AgentState,
    has_snapshot: bool,
    transcript: Transcript,
    /// A Reset's fresh transcript, or a re-tail that does not meet the held
    /// window, swapped in at the next CaughtUp.
    pending: Option<Transcript>,
    /// The stream was reopened; a tail that leaves a gap above the head
    /// builds a pending transcript instead of appending.
    retail: bool,
    inputs: Inputs,
    caught_up: bool,
    detached: bool,
    connection: Connection,
    /// The incarnation a Send was rejected as exited under.
    exited_under: Option<u32>,
    epoch: u64,
    blobs: HashMap<Vec<u8>, BlobStatus>,
}

impl SessionState {
    pub fn new(agent: Agent) -> SessionState {
        let kind = agent.kind();
        SessionState {
            state: AgentState {
                kind,
                phase: agent.phase(),
                ..AgentState::default()
            },
            agent,
            host: None,
            has_snapshot: false,
            transcript: Transcript::new(kind),
            pending: None,
            retail: false,
            inputs: Inputs::default(),
            caught_up: false,
            detached: false,
            connection: Connection::Connecting,
            exited_under: None,
            epoch: 0,
            blobs: HashMap::new(),
        }
    }

    pub fn kind(&self) -> Kind {
        self.agent.kind()
    }

    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    pub fn host(&self) -> Option<&HostEntry> {
        self.host.as_ref()
    }

    /// The newest snapshot, decoded.
    pub fn agent_state(&self) -> &AgentState {
        &self.state
    }

    pub fn has_snapshot(&self) -> bool {
        self.has_snapshot
    }

    /// The window on screen. During a Reset it is the old one, until the
    /// next CaughtUp swaps the fresh one in.
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn reset_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn inputs(&self) -> &Inputs {
        &self.inputs
    }

    pub fn caught_up(&self) -> bool {
        self.caught_up
    }

    pub fn connection(&self) -> Connection {
        self.connection
    }

    /// Stamped on page requests; bumped at every Reset swap.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The page-older cursor.
    pub fn oldest_order(&self) -> Option<u64> {
        self.transcript.oldest_held()
    }

    pub fn blob(&self, hash: &[u8]) -> BlobStatus {
        self.blobs.get(hash).cloned().unwrap_or_default()
    }

    pub fn input_state(&self, id: &[u8]) -> Option<InputState> {
        self.inputs.get(id).map(|sent| sent.state.clone())
    }

    fn exited(&self) -> bool {
        self.agent.lifecycle() == wire::Lifecycle::Exited || self.exited_under.is_some()
    }

    /// Caught up and the entry says live. Drafting is never gated.
    pub fn can_send(&self) -> bool {
        self.caught_up && !self.exited()
    }

    pub fn composer(&self) -> Composer {
        if self.exited() {
            Composer::Resume
        } else if self.caught_up {
            Composer::Send
        } else if self.connection == Connection::Reconnecting {
            Composer::Disabled(Waiting::Reconnecting)
        } else if self.detached {
            Composer::Disabled(Waiting::Detached)
        } else {
            Composer::Disabled(Waiting::CatchingUp)
        }
    }

    pub fn phase(&self) -> PhaseView {
        if self.exited() {
            return PhaseView::Exited {
                cause: self.agent.exit_cause.clone(),
            };
        }
        let phase = if self.has_snapshot {
            self.state.phase
        } else {
            self.agent.phase()
        };
        match phase {
            wire::Phase::Starting => PhaseView::Starting,
            wire::Phase::Idle => PhaseView::Idle,
            wire::Phase::Working => PhaseView::Working,
            wire::Phase::NeedsYou => PhaseView::NeedsYou,
        }
    }

    /// The asks to draw, head first. Before CaughtUp an ask is drawn only if
    /// the entry says needs_you, so an ask answered elsewhere does not flash
    /// from a cached snapshot.
    pub fn open_asks(&self) -> &[OpenAsk] {
        if !self.caught_up && !self.exited() && self.agent.phase() != wire::Phase::NeedsYou {
            return &[];
        }
        &self.state.asks
    }

    /// An exited agent's open asks are drawn dismissed.
    pub fn asks_dismissed(&self) -> bool {
        self.exited()
    }

    /// This client's newest answer to an ask, while the card shows it.
    pub fn answering(&self, ask_key: &str) -> Option<&SentInput> {
        self.inputs
            .iter()
            .rev()
            .find(|sent| matches!(&sent.what, InputWhat::Answer { ask_key: key } if key == ask_key))
    }

    /// The shared queue as the composer draws it. An entry this client holds
    /// as uncertain is not shown as queued: it is drawn once, as not
    /// confirmed, until CaughtUp resolves it.
    pub fn queue(&self) -> Vec<QueueRow<'_>> {
        self.state
            .queue
            .iter()
            .filter_map(|entry| {
                let mine = self.inputs.get(&entry.input_id);
                if mine.is_some_and(|sent| sent.state == InputState::Uncertain) {
                    return None;
                }
                let sending_now = self.inputs.iter().any(|sent| {
                    sent.state == InputState::Sent
                        && matches!(&sent.what, InputWhat::SendNow { target } if *target == entry.input_id)
                });
                Some(QueueRow { entry, mine: mine.is_some(), steered: entry.steer || sending_now })
            })
            .collect()
    }

    /// Inputs to draw as "not confirmed", with resend and discard.
    pub fn not_confirmed(&self) -> impl Iterator<Item = &SentInput> {
        self.inputs
            .iter()
            .filter(|sent| sent.state == InputState::Uncertain)
    }

    /// Prompts to draw optimistically: sent and not yet reflected or queued.
    pub fn sending(&self) -> impl Iterator<Item = &SentInput> {
        self.inputs.iter().filter(|sent| {
            sent.state == InputState::Sent && matches!(sent.what, InputWhat::Prompt { .. })
        })
    }

    /// The activity line, derived from the newest items and the phase.
    pub fn activity(&self, now_ms: i64) -> Option<Activity> {
        if self.exited() || self.phase() != PhaseView::Working {
            return None;
        }
        let at = |since_ms: i64, kind| Activity {
            kind,
            since_ms,
            elapsed_ms: (now_ms - since_ms).max(0),
        };
        let transcript = &self.transcript;
        let running_tasks = self
            .state
            .active_tasks
            .iter()
            .filter(|task| task.state() == wire::TaskState::Running)
            .count() as u32;
        let mut subagents = 0u32;
        let mut turn_start = None;
        for held in transcript.iter().rev() {
            match &held.class {
                ItemClass::Tool(tool) if tool.subagent => subagents += 1,
                ItemClass::Prompt | ItemClass::Steer | ItemClass::AgentMessage => {
                    turn_start = Some(held.item.at_ms);
                    break;
                }
                ItemClass::Turn | ItemClass::Boundary => break,
                _ => {}
            }
        }
        let subagents = subagents.max(running_tasks);
        let turn_start = turn_start.unwrap_or(self.state.at_ms);
        let Some(newest) = transcript.iter().next_back() else {
            return Some(at(turn_start, ActivityKind::Working));
        };
        let since = newest.item.at_ms;
        Some(match &newest.class {
            ItemClass::Retrying {
                attempt,
                max_attempts,
                retry_at_ms,
            } => at(
                since,
                ActivityKind::Retrying {
                    attempt: *attempt,
                    max_attempts: *max_attempts,
                    retry_at_ms: *retry_at_ms,
                },
            ),
            ItemClass::Compacting => at(since, ActivityKind::Compacting),
            ItemClass::Thinking { complete: false } => at(since, ActivityKind::Thinking),
            ItemClass::Tool(tool) if tool.in_flight && !tool.subagent => at(
                since,
                ActivityKind::Running {
                    key: newest.item.key.clone(),
                },
            ),
            _ if subagents > 0 => at(turn_start, ActivityKind::Subagents { count: subagents }),
            _ => at(turn_start, ActivityKind::Working),
        })
    }

    pub fn update(&mut self, msg: Msg) -> Outcome {
        let mut outcome = Outcome::default();
        let mut changed = Changed::default();
        match msg {
            Msg::Event(event) => self.event(event, &mut changed, &mut outcome),
            Msg::Page {
                items,
                exhausted,
                epoch,
            } => {
                if epoch == self.epoch {
                    self.transcript.page(items, exhausted, &mut changed);
                }
            }
            Msg::Connection(connection) => {
                self.connection = connection;
                if connection == Connection::Reconnecting {
                    self.caught_up = false;
                    self.retail = true;
                }
                outcome.session = true;
            }
            Msg::Send(input) => {
                outcome.session = self.inputs.push(input);
                // A reflection can beat the optimistic row only on a resend
                // of a known id, which the push refuses.
            }
            Msg::Sent(id, result) => outcome.session = self.sent(&id, result),
            Msg::Discard(id) => outcome.session = self.inputs.remove(&id),
            Msg::Entry(agent) => {
                if agent.lifecycle() == wire::Lifecycle::Live
                    && self
                        .exited_under
                        .is_some_and(|incarnation| agent.incarnation > incarnation)
                {
                    self.exited_under = None;
                }
                self.agent = agent;
                outcome.session = true;
            }
            Msg::Host(host) => {
                self.host = Some(host);
                outcome.session = true;
            }
            Msg::Blob { hash, status } => {
                if self.blob(&hash) != status {
                    for held in self.transcript.iter() {
                        if references(&held.item, &hash) {
                            changed.key(&held.item.key);
                        }
                    }
                    self.blobs.insert(hash, status);
                }
            }
        }
        outcome.changed = changed.into_keys();
        outcome
    }

    fn event(&mut self, event: SessionEvent, changed: &mut Changed, outcome: &mut Outcome) {
        use session_event::Of;
        let Some(event) = event.of else { return };
        match event {
            Of::Snapshot(snapshot) => {
                self.state = AgentState::from_snapshot(self.kind(), &snapshot);
                self.has_snapshot = true;
                self.queue_moved();
                outcome.session = true;
            }
            Of::Item(item) => {
                let settles = !item.input_id.is_empty();
                let input_id = item.input_id.clone();
                let visible = self.item(item, changed);
                if settles && self.reflected(&input_id, visible) {
                    outcome.session = true;
                }
            }
            Of::Append(append) => {
                let target = if let Some(pending) = &mut self.pending {
                    pending.append(&append, &mut Changed::default())
                } else {
                    self.transcript.append(&append, changed)
                };
                if target == Appended::NeedGet {
                    outcome.need_get = Some(append.key);
                }
            }
            Of::CaughtUp(_) => {
                self.caught_up = true;
                self.detached = false;
                self.retail = false;
                if let Some(fresh) = self.pending.take() {
                    self.swap(fresh, changed);
                    outcome.reloaded = true;
                }
                self.resolve_uncertain();
                outcome.session = true;
            }
            Of::Lagged(_) => {
                self.caught_up = false;
                self.retail = true;
                outcome.session = true;
            }
            Of::Reset(_) => {
                self.caught_up = false;
                self.pending = Some(Transcript::new(self.kind()));
                outcome.session = true;
            }
            Of::Detached(_) => {
                self.caught_up = false;
                self.detached = true;
                outcome.session = true;
            }
        }
    }

    /// Applies an item to the transcript it belongs to; true when that is the
    /// one on screen.
    fn item(&mut self, item: Item, changed: &mut Changed) -> bool {
        if let Some(pending) = &mut self.pending {
            pending.upsert(item, &mut Changed::default());
            return false;
        }
        let gap = self.retail
            && self.transcript.get(&item.key).is_none()
            && self
                .transcript
                .head()
                .is_some_and(|head| item.order > head + 1);
        if gap {
            // A re-tail that does not meet the held window: build it apart
            // and swap it in at CaughtUp, as for a Reset.
            let mut fresh = Transcript::new(self.kind());
            fresh.upsert(item, &mut Changed::default());
            self.pending = Some(fresh);
            return false;
        }
        self.transcript.upsert(item, changed);
        true
    }

    fn swap(&mut self, fresh: Transcript, changed: &mut Changed) {
        let old = std::mem::replace(&mut self.transcript, fresh);
        let new = &self.transcript;
        let keys: BTreeSet<&Key> = old.keys().chain(new.keys()).collect();
        for key in keys {
            let before = old.get(key).map(|held| (held, old.run_at(held.item.order)));
            let after = new.get(key).map(|held| (held, new.run_at(held.item.order)));
            if before != after {
                changed.key(key);
            }
        }
        self.epoch += 1;
    }

    /// A reflection carries an input id: this client's prompt landed.
    fn reflected(&mut self, input_id: &[u8], visible: bool) -> bool {
        let caught_up = self.caught_up;
        let Some(sent) = self.inputs.get_mut(input_id) else {
            return false;
        };
        let settle = match sent.state {
            InputState::Sent | InputState::Queued => true,
            // An uncertain input is judged against caught-up state only.
            InputState::Uncertain => caught_up && visible,
            InputState::Settled | InputState::Rejected(_) => false,
        };
        if settle {
            sent.state = InputState::Settled;
        }
        settle
    }

    fn sent(&mut self, id: &[u8], result: InputOutcome) -> bool {
        use wire::send_input_response::Of;
        let incarnation = self.agent.incarnation;
        let reflected = self.transcript.key_for_input(id).is_some();
        let Some(sent) = self.inputs.get_mut(id) else {
            // A late result for an input this open never sent.
            return false;
        };
        if sent.state != InputState::Sent {
            return false;
        }
        sent.state = match result {
            InputOutcome::Lost => InputState::Uncertain,
            InputOutcome::Reply(reply) => match reply.of {
                Some(Of::Accepted(accepted)) if accepted.queued => {
                    if sent.seen_queued || !reflected {
                        InputState::Queued
                    } else {
                        InputState::Settled
                    }
                }
                Some(Of::Accepted(_)) => match sent.what {
                    InputWhat::Prompt { .. } if !reflected => InputState::Sent,
                    _ => InputState::Settled,
                },
                Some(Of::Rejected(rejected)) => {
                    if rejected.reason == "exited" {
                        self.exited_under = Some(incarnation);
                    }
                    InputState::Rejected(rejected.reason)
                }
                None => InputState::Uncertain,
            },
        };
        true
    }

    /// A new snapshot: queued inputs it lists were seen; one seen before and
    /// gone now was submitted or withdrawn, and is settled either way.
    fn queue_moved(&mut self) {
        let queue = &self.state.queue;
        for sent in self.inputs.iter_mut() {
            let listed = queue.iter().any(|entry| entry.input_id == sent.id);
            match sent.state {
                InputState::Queued if listed => sent.seen_queued = true,
                InputState::Queued if sent.seen_queued => sent.state = InputState::Settled,
                _ => {}
            }
        }
    }

    /// At CaughtUp: an uncertain input found in the queue is queued, one found
    /// among the items is settled, and the rest stay for the person.
    fn resolve_uncertain(&mut self) {
        let queue = &self.state.queue;
        let transcript = &self.transcript;
        for sent in self.inputs.iter_mut() {
            if sent.state != InputState::Uncertain {
                continue;
            }
            if queue.iter().any(|entry| entry.input_id == sent.id) {
                sent.state = InputState::Queued;
                sent.seen_queued = true;
            } else if transcript.key_for_input(&sent.id).is_some() {
                sent.state = InputState::Settled;
            }
        }
    }
}

fn references(item: &Item, hash: &[u8]) -> bool {
    use wire::attachment::Of;
    item.attachments
        .iter()
        .any(|attachment| match &attachment.of {
            Some(Of::Image(blob) | Of::File(blob)) => blob.hash == hash,
            Some(Of::Review(review)) => review
                .diff
                .as_ref()
                .and_then(|diff| diff.patch.as_ref())
                .is_some_and(|patch| patch.hash == hash),
            Some(Of::Text(_)) | None => false,
        })
}

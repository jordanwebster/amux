//! The session driver: one open chat's stream, its RPCs and its model.
//!
//! The driver owns the Subscribe stream and the client handle and feeds
//! everything into [`SessionState`]; nothing else about a chat does I/O. It
//! never holds a cursor and never sees the origin: the local runtime answers
//! from its rows, and CaughtUp and Detached on the stream say whether the
//! chat is current.

use std::collections::{BTreeSet, HashMap};
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use client::{Client, Clock, EventStream, RpcError};
use futures_util::{FutureExt as _, StreamExt as _};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use ui_state::{BlobStatus, Connection, InputId, InputOutcome, Key, Msg, Outcome, SessionState};
use wire::{
    Agent, BlobRef, DumpFile, DumpPart, ErrorCode, FetchRequest, GetBlobRequest, GetRequest,
    HostEntry, Input, PutBlobRequest, ResumeAgentRequest, SendInputRequest, SendInputResponse,
    SessionEvent, SubscribeRequest, send_input_response, session_event, subscribe_request,
};

use crate::trace::{DriverEvent, DriverTrace, Ring, Structure as _, TraceEvent};
use crate::{Backoff, inputs};

/// What became of a send.
#[derive(Clone, Debug, PartialEq)]
pub struct Sent {
    pub id: InputId,
    pub outcome: InputOutcome,
}

/// Why an act on the chat did not take effect.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    #[error("rejected: {0}")]
    Rejected(String),
    /// The connection dropped before the verdict: the chat shows it not
    /// confirmed until it has caught up again.
    #[error("not confirmed: the connection dropped before the agent answered")]
    Uncertain,
}

/// Why an older page did not arrive.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum PageError {
    /// Older history is held only by the agent's host, which cannot be
    /// reached. The chat says so; it is never an empty page.
    #[error("older history is on the agent's host, which cannot be reached")]
    OriginUnreachable,
    #[error(transparent)]
    Rpc(RpcError),
}

/// What changed since the host last asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Every row key an update may have changed, once each.
    pub keys: Vec<Key>,
    /// A Reset's transcript was swapped in: every row id may be new.
    pub reloaded: bool,
    /// Something outside the rows moved.
    pub session: bool,
}

impl Changes {
    fn absorb(&mut self, outcome: &Outcome, seen: &mut BTreeSet<Key>) {
        for key in &outcome.changed {
            if seen.insert(key.clone()) {
                self.keys.push(key.clone());
            }
        }
        self.reloaded |= outcome.reloaded;
        self.session |= outcome.session;
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && !self.reloaded && !self.session
    }
}

struct Model {
    state: SessionState,
    trace: Ring<SessionState, Msg>,
    changes: Changes,
    changed_keys: BTreeSet<Key>,
    /// Attachment bytes fetched during this open. Dropped with the session.
    blobs: HashMap<Vec<u8>, Arc<[u8]>>,
    /// The runtime refused to serve the stream again: the agent is gone.
    ended: Option<RpcError>,
}

pub(crate) struct Inner {
    client: Arc<dyn Client>,
    agent_id: Vec<u8>,
    tail: u32,
    clock: Arc<dyn Clock>,
    model: Mutex<Model>,
    changed: watch::Sender<()>,
    closed: AtomicBool,
}

impl Inner {
    fn model(&self) -> MutexGuard<'_, Model> {
        self.model
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Applies one message; after close, a late result changes nothing.
    fn apply(&self, msg: Msg) -> Outcome {
        if self.closed.load(Ordering::Acquire) {
            return Outcome::default();
        }
        let at_ms = self.clock.now_ms();
        let mut model = self.model();
        let Model {
            state,
            trace,
            changes,
            changed_keys,
            ..
        } = &mut *model;
        trace.record(state, at_ms, TraceEvent::Msg(msg.clone()));
        let outcome = state.update(msg);
        changes.absorb(&outcome, changed_keys);
        drop(model);
        let moved = !outcome.changed.is_empty() || outcome.reloaded || outcome.session;
        if moved {
            self.changed.send_replace(());
        }
        outcome
    }

    fn note(&self, event: DriverEvent) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let at_ms = self.clock.now_ms();
        let mut model = self.model();
        let Model { state, trace, .. } = &mut *model;
        trace.record(state, at_ms, TraceEvent::Driver(event));
    }

    fn subscribe_request(&self) -> SubscribeRequest {
        SubscribeRequest {
            agent_id: self.agent_id.clone(),
            from: Some(subscribe_request::From::Tail(self.tail)),
        }
    }

    /// Applies one stream event, answering an Append whose base is not
    /// held with Get before the next event, so later appends meet it.
    async fn event(&self, event: SessionEvent) -> Outcome {
        let outcome = self.apply(Msg::Event(event));
        if let Some(key) = &outcome.need_get {
            self.note(DriverEvent::Get { key: key.clone() });
            let request = GetRequest {
                agent_id: self.agent_id.clone(),
                key: key.clone(),
            };
            match self.client.get(request).await {
                Ok(item) => {
                    self.apply(Msg::Event(SessionEvent {
                        of: Some(session_event::Of::Item(item)),
                    }));
                }
                Err(error) => self.note(DriverEvent::GetFailed {
                    key: key.clone(),
                    error: error.to_string(),
                }),
            }
        }
        outcome
    }

    async fn send(&self, mut input: Input) -> Sent {
        if input.input_id.is_empty() {
            input.input_id = inputs::input_id();
        }
        let id = input.input_id.clone();
        self.apply(Msg::Send(input.clone()));
        let request = SendInputRequest {
            agent_id: self.agent_id.clone(),
            input: Some(input),
        };
        let outcome = match self.client.send_input(request).await {
            Ok(reply) => InputOutcome::Reply(reply),
            Err(RpcError::Transport(_)) => InputOutcome::Lost,
            // The runtime handed the input on and lost the agent's answer:
            // it may or may not have arrived.
            Err(error) if error.code() == Some(ErrorCode::Aborted) => InputOutcome::Lost,
            // Any other refusal never reached the agent, which is a
            // rejection, never uncertain.
            Err(RpcError::Refused(error)) => InputOutcome::Reply(rejected(error.message)),
        };
        self.apply(Msg::Sent(id.clone(), outcome.clone()));
        Sent { id, outcome }
    }

    async fn fetch_blob(self: Arc<Self>, hash: Vec<u8>) {
        self.note(DriverEvent::Blob { hash: hash.clone() });
        self.apply(Msg::Blob {
            hash: hash.clone(),
            status: BlobStatus::Fetching,
        });
        let request = GetBlobRequest {
            agent_id: self.agent_id.clone(),
            hash: hash.clone(),
        };
        let status = match self.client.get_blob(request).await {
            Ok(blob) => {
                self.model().blobs.insert(hash.clone(), blob.bytes.into());
                BlobStatus::Ready
            }
            Err(error) => BlobStatus::Failed(error.to_string()),
        };
        self.apply(Msg::Blob { hash, status });
    }
}

fn rejected(reason: String) -> SendInputResponse {
    SendInputResponse {
        of: Some(send_input_response::Of::Rejected(wire::Rejected { reason })),
    }
}

/// The session's state while the guard lives. Never hold it across an
/// await: the pump waits for it to apply the next event.
pub struct StateGuard<'a>(MutexGuard<'a, Model>);

impl Deref for StateGuard<'_> {
    type Target = SessionState;

    fn deref(&self) -> &SessionState {
        &self.0.state
    }
}

/// One open chat.
pub struct Session {
    inner: Arc<Inner>,
    pump: JoinHandle<()>,
}

/// How a stream the pump was reading ended.
enum End {
    /// Lagged: the runtime closed it; reopen with a tail at once.
    Lagged,
    /// The stream is gone: reconnect with backoff.
    Closed(Option<RpcError>),
}

impl Session {
    /// Subscribes with a tail of `tail` rows and resolves once the Snapshot
    /// and the rows the runtime already holds are applied, so the first
    /// render is correct: at once for an own agent, and from the replica
    /// for another host's even with that host away. CaughtUp may follow.
    pub async fn open(
        client: Arc<dyn Client>,
        agent: Agent,
        tail: u32,
        clock: impl Clock,
    ) -> Result<Session, RpcError> {
        let state = SessionState::new(agent.clone());
        let (changed, _) = watch::channel(());
        let inner = Arc::new(Inner {
            client,
            agent_id: agent.agent_id,
            tail,
            clock: Arc::new(clock),
            model: Mutex::new(Model {
                trace: Ring::new(state.clone()),
                state,
                changes: Changes::default(),
                changed_keys: BTreeSet::new(),
                blobs: HashMap::new(),
                ended: None,
            }),
            changed,
            closed: AtomicBool::new(false),
        });
        let mut stream = inner.client.subscribe(inner.subscribe_request()).await?;
        inner.note(DriverEvent::Subscribed { tail });
        inner.apply(Msg::Connection(Connection::Live));
        let mut ended = None;
        // The Snapshot leads the opening.
        loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    let snapshot = matches!(event.of, Some(session_event::Of::Snapshot(_)));
                    let lagged = matches!(event.of, Some(session_event::Of::Lagged(_)));
                    inner.event(event).await;
                    if lagged {
                        ended = Some(End::Lagged);
                        break;
                    }
                    if snapshot {
                        break;
                    }
                }
                Some(Err(error)) => {
                    return Err(error);
                }
                None => {
                    return Err(RpcError::Transport(
                        "the stream ended before its snapshot".into(),
                    ));
                }
            }
        }
        // The held rows are already on their way: apply what has arrived,
        // up to the marker. Anything later is the pump's.
        while ended.is_none() {
            let Some(next) = stream.next().now_or_never() else {
                break;
            };
            match next {
                Some(Ok(event)) => {
                    let marker = matches!(
                        event.of,
                        Some(session_event::Of::CaughtUp(_) | session_event::Of::Detached(_))
                    );
                    let lagged = matches!(event.of, Some(session_event::Of::Lagged(_)));
                    inner.event(event).await;
                    if lagged {
                        ended = Some(End::Lagged);
                    } else if marker {
                        break;
                    }
                }
                Some(Err(error)) => ended = Some(End::Closed(Some(error))),
                None => ended = Some(End::Closed(None)),
            }
        }
        let pump = tokio::spawn(pump(inner.clone(), stream, ended));
        Ok(Session { inner, pump })
    }

    pub fn agent_id(&self) -> &[u8] {
        &self.inner.agent_id
    }

    pub fn state(&self) -> StateGuard<'_> {
        StateGuard(self.inner.model())
    }

    /// Level-triggered: marked on every change; the host draws on change
    /// and never on a clock, so changes during a draw collapse into one.
    pub fn changed(&self) -> watch::Receiver<()> {
        self.inner.changed.subscribe()
    }

    /// Everything that changed since the last call, for a host that
    /// reconfigures only the rows an update touched.
    pub fn take_changes(&self) -> Changes {
        let mut model = self.inner.model();
        model.changed_keys.clear();
        std::mem::take(&mut model.changes)
    }

    /// Set when the runtime would no longer serve the chat, as when the
    /// agent was deleted: the host closes it to the fleet.
    pub fn ended(&self) -> Option<RpcError> {
        self.inner.model().ended.clone()
    }

    /// The agent's inventory row, from the fleet: lifecycle and phase.
    pub fn set_entry(&self, agent: Agent) {
        self.inner.apply(Msg::Entry(agent));
    }

    /// The agent's host, from the fleet, for the header.
    pub fn set_host(&self, host: HostEntry) {
        self.inner.apply(Msg::Host(host));
    }

    /// Fetches up to `n` rows older than the oldest held and merges them
    /// under the window; returns how many arrived.
    pub async fn page_older(&self, n: u32) -> Result<usize, PageError> {
        let (before, epoch) = {
            let model = self.inner.model();
            (model.state.oldest_order(), model.state.epoch())
        };
        self.inner.note(DriverEvent::Page { before, limit: n });
        let request = FetchRequest {
            agent_id: self.inner.agent_id.clone(),
            before_order: before,
            limit: n,
        };
        match self.inner.client.fetch(request).await {
            Ok(page) => {
                let count = page.items.len();
                self.inner.apply(Msg::Page {
                    items: page.items,
                    exhausted: page.exhausted,
                    epoch,
                });
                Ok(count)
            }
            Err(error) => {
                self.inner.note(DriverEvent::PageFailed {
                    error: error.to_string(),
                });
                if error.code() == Some(ErrorCode::Unreachable) {
                    Err(PageError::OriginUnreachable)
                } else {
                    Err(PageError::Rpc(error))
                }
            }
        }
    }

    /// Sends an input, under a fresh id when it has none. The optimistic
    /// row exists from now; a dropped connection leaves the input
    /// uncertain, and nothing is ever resent on its own.
    pub async fn send(&self, input: Input) -> Sent {
        self.inner.send(input).await
    }

    /// A prompt in this agent's kind.
    pub async fn send_prompt(&self, text: &str, attachments: Vec<wire::Attachment>) -> Sent {
        match inputs::prompt(self.kind(), text, attachments) {
            Some(input) => self.send(input).await,
            None => Sent {
                id: Vec::new(),
                outcome: InputOutcome::Reply(rejected("unsupported".into())),
            },
        }
    }

    /// The exited composer's one tap: the draft becomes the new
    /// incarnation's first prompt. On failure the draft is the composer's
    /// to keep.
    pub async fn resume_with(&self, mut draft: Input) -> Result<Agent, RpcError> {
        if draft.input_id.is_empty() {
            draft.input_id = inputs::input_id();
        }
        let id = draft.input_id.clone();
        self.inner.apply(Msg::Send(draft.clone()));
        let request = ResumeAgentRequest {
            agent_id: self.inner.agent_id.clone(),
            initial_prompt: Some(draft),
        };
        match self.inner.client.resume_agent(request).await {
            Ok(agent) => {
                self.inner.apply(Msg::Entry(agent.clone()));
                let accepted = SendInputResponse {
                    of: Some(send_input_response::Of::Accepted(wire::Accepted {
                        queued: false,
                    })),
                };
                self.inner
                    .apply(Msg::Sent(id, InputOutcome::Reply(accepted)));
                Ok(agent)
            }
            Err(error) => {
                self.inner.apply(Msg::Discard(id));
                Err(error)
            }
        }
    }

    /// Takes a queued prompt back out of the queue.
    pub async fn withdraw(&self, queued: &[u8]) -> Result<(), InputError> {
        self.act(inputs::withdraw(self.kind(), queued)).await
    }

    /// Steers a queued prompt into the running turn; the entry reads
    /// steered until its reflection lands.
    pub async fn send_now(&self, queued: &[u8]) -> Result<(), InputError> {
        self.act(inputs::send_now(self.kind(), queued)).await
    }

    /// Answers an open ask with the input `ui_view::answer_input` made
    /// from its card and the person's choice; any open ask can be answered.
    pub async fn answer(&self, input: Input) -> Result<(), InputError> {
        self.act(Some(input)).await
    }

    pub async fn interrupt(&self) -> Result<(), InputError> {
        self.act(inputs::interrupt(self.kind())).await
    }

    /// Forgets an input that was not confirmed.
    pub fn discard(&self, id: &[u8]) {
        self.inner.apply(Msg::Discard(id.to_vec()));
    }

    async fn act(&self, input: Option<Input>) -> Result<(), InputError> {
        let Some(input) = input else {
            return Err(InputError::Rejected("unsupported".into()));
        };
        match self.send(input).await.outcome {
            InputOutcome::Reply(SendInputResponse {
                of: Some(send_input_response::Of::Accepted(_)),
            }) => Ok(()),
            InputOutcome::Reply(SendInputResponse {
                of: Some(send_input_response::Of::Rejected(rejected)),
            }) => Err(InputError::Rejected(rejected.reason)),
            InputOutcome::Reply(SendInputResponse { of: None }) | InputOutcome::Lost => {
                Err(InputError::Uncertain)
            }
        }
    }

    fn kind(&self) -> wire::Kind {
        self.inner.model().state.kind()
    }

    /// An attachment's bytes, if fetched. A missing one starts its fetch,
    /// and the rows that show it change when it lands; until then they
    /// draw a placeholder from the reference.
    pub fn blob(&self, hash: &[u8]) -> Option<Arc<[u8]>> {
        let status = {
            let model = self.inner.model();
            if let Some(bytes) = model.blobs.get(hash) {
                return Some(bytes.clone());
            }
            model.state.blob(hash)
        };
        if status == BlobStatus::Missing {
            self.fetch_blob(hash);
        }
        None
    }

    /// Fetches an attachment's bytes again, as after a failure.
    pub fn fetch_blob(&self, hash: &[u8]) {
        if self.inner.closed.load(Ordering::Acquire) {
            return;
        }
        tokio::spawn(self.inner.clone().fetch_blob(hash.to_vec()));
    }

    /// Stores bytes to attach to a prompt.
    pub async fn put_blob(
        &self,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<BlobRef, RpcError> {
        let request = PutBlobRequest {
            agent_id: self.inner.agent_id.clone(),
            name: name.to_owned(),
            mime: mime.to_owned(),
            bytes,
        };
        self.inner.client.put_blob(request).await
    }

    /// The bounded trace: the state its oldest segment began from and every
    /// event since, in this client's order.
    pub fn trace(&self) -> DriverTrace<SessionState, Msg> {
        self.inner.model().trace.trace()
    }

    /// This session's part of a dump bundle: the structure of its state and
    /// its trace, with no content (see `dump`).
    pub fn dump_part(&self) -> DumpPart {
        let (state, trace) = {
            let model = self.inner.model();
            (model.state.structure(), model.trace.trace())
        };
        let dir = format!("client/sessions/{}", hex(&self.inner.agent_id));
        DumpPart {
            dump_id: Vec::new(),
            files: vec![
                DumpFile {
                    name: format!("{dir}/state.txt"),
                    contents: state.into_bytes(),
                },
                DumpFile {
                    name: format!("{dir}/trace.txt"),
                    contents: trace.render().into_bytes(),
                },
            ],
        }
    }

    /// Drops the stream. Nothing is flushed: nothing was kept. Results of
    /// calls still in flight change nothing.
    pub fn close(self) {}
}

impl Drop for Session {
    fn drop(&mut self) {
        self.inner.closed.store(true, Ordering::Release);
        self.pump.abort();
    }
}

impl DriverTrace<SessionState, Msg> {
    /// The state the trace ends in, rebuilt from its start and messages.
    pub fn replay(&self) -> SessionState {
        let mut state = self.start.clone();
        for msg in self.msgs() {
            state.update(msg.clone());
        }
        state
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Reads one stream until it ends.
async fn read(inner: &Inner, stream: &mut EventStream<SessionEvent>, backoff: &mut Backoff) -> End {
    loop {
        match stream.next().await {
            Some(Ok(event)) => {
                let lagged = matches!(event.of, Some(session_event::Of::Lagged(_)));
                let caught_up = matches!(event.of, Some(session_event::Of::CaughtUp(_)));
                inner.event(event).await;
                if lagged {
                    return End::Lagged;
                }
                if caught_up {
                    backoff.reset();
                }
            }
            Some(Err(error)) => return End::Closed(Some(error)),
            None => return End::Closed(None),
        }
    }
}

/// The pump: Lagged reopens with a tail at once; the end of the stream
/// reconnects with backoff on the session's clock and re-tails. Uncertain
/// inputs settle at the next CaughtUp from the queue or the items, and the
/// rest stay for the person; nothing is resent.
async fn pump(inner: Arc<Inner>, stream: EventStream<SessionEvent>, ended: Option<End>) {
    let mut backoff = Backoff::default();
    let mut stream = Some(stream);
    let mut ended = ended;
    loop {
        let end = match ended.take() {
            Some(end) => end,
            None => match stream.as_mut() {
                Some(stream) => read(&inner, stream, &mut backoff).await,
                None => End::Closed(None),
            },
        };
        stream = None;
        let mut wait = match end {
            End::Lagged => {
                inner.note(DriverEvent::Retail);
                false
            }
            End::Closed(error) => {
                inner.note(DriverEvent::StreamEnded {
                    error: error.map(|error| error.to_string()),
                });
                inner.apply(Msg::Connection(Connection::Reconnecting));
                true
            }
        };
        while stream.is_none() {
            if wait {
                let until_ms = inner.clock.now_ms() + backoff.next_ms();
                inner.note(DriverEvent::Backoff { until_ms });
                inner.clock.sleep_until(until_ms).await;
            }
            match inner.client.subscribe(inner.subscribe_request()).await {
                Ok(opened) => {
                    inner.note(DriverEvent::Subscribed { tail: inner.tail });
                    inner.apply(Msg::Connection(Connection::Live));
                    stream = Some(opened);
                }
                Err(RpcError::Transport(error)) => {
                    inner.note(DriverEvent::SubscribeFailed { error });
                    if !wait {
                        inner.apply(Msg::Connection(Connection::Reconnecting));
                    }
                    wait = true;
                }
                Err(error) => {
                    inner.note(DriverEvent::Ended {
                        error: error.to_string(),
                    });
                    inner.model().ended = Some(error);
                    inner.changed.send_replace(());
                    return;
                }
            }
        }
    }
}

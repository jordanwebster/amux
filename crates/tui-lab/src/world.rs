//! The fake runtime: a scenario's hosts and agents held in memory and served
//! through the real client seam. Acts from the terminal client change it the
//! way a daemon would (a prompt is reflected and answered, an answered ask
//! closes and the agent carries on), and the timeline plays beats into it.
//!
//! Everything is authored: no provider runs, nothing is persisted, and
//! a relaunch rebuilds the same world by replaying the scenario and the
//! beats that had already fired.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use client::{Client, Clock as _, EventStream, RpcError, SystemClock};
use prost::Message as _;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::sync::mpsc;
use wire::{
    Agent, AgentParent, AskItem, BlobRef, CreateAgentRequest, DeleteAgentRequest,
    DeleteAgentResponse, Diff, DiffRequest, DumpRequest, DumpResponse, Envelope, ErrorCode,
    FetchRequest, FetchResponse, GetBlobRequest, GetBlobResponse, GetRequest, HostEntry, HostVia,
    Input, InventoryEvent, Item, Kind, Lifecycle, ListRepositoriesRequest,
    ListRepositoriesResponse, Phase, ProjectEntry, PutBlobRequest, QueuedInput, RenameAgentRequest,
    ResolveAgentRequest, ResumeAgentRequest, SendInputRequest, SendInputResponse,
    SendMessageResponse, SessionEvent, Snapshot, StopAgentRequest, SubscribeRequest, ToolCall,
    ToolDecision, ToolState, WorkingOn, inventory_event, send_input_response, session_event,
};

use crate::body::{self, Body, SnapshotFacts};
use crate::scenario::{
    AgentSpec, AskSpec, BashSpec, Beat, BoundarySpec, DecisionOutcomeSpec, Entry, KindSpec,
    OptionSpec, PhaseSpec, Presence, QuestionItem, Scenario, SignInStateSpec, TaskStatus,
    ToolStateSpec, TurnOutcomeSpec, UsageStateSpec, Via,
};

fn now_ms() -> i64 {
    SystemClock.now_ms()
}

type Tx<T> = mpsc::UnboundedSender<Result<T, RpcError>>;

fn stream<T: Send + 'static>() -> (Tx<T>, EventStream<T>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    (tx, Box::pin(stream))
}

fn refused(code: ErrorCode, message: impl Into<String>) -> RpcError {
    RpcError::Refused(wire::Error {
        code: code as i32,
        message: message.into(),
        details: Vec::new(),
    })
}

pub fn kind_of(spec: KindSpec) -> Kind {
    match spec {
        KindSpec::Claude => Kind::ClaudePty,
        KindSpec::ClaudeSdk => Kind::ClaudeSdk,
        KindSpec::Codex => Kind::Codex,
    }
}

/// How long an entry takes, for laying an authored transcript out in the
/// past and for pacing it live.
fn nominal_ms(entry: &Entry) -> i64 {
    match entry {
        Entry::User(_) => 20_000,
        Entry::Say(text) => 2_000 + (text.len() as i64 * 15).min(8_000),
        Entry::Think(_) => 6_000,
        Entry::Bash(BashSpec::Full {
            took: Some(took), ..
        }) => took.ms(),
        Entry::Bash(_) => 4_000,
        Entry::Tool(spec) => spec.took.map_or(1_500, |took| took.ms()),
        Entry::Wait(d) => d.ms(),
        Entry::Turn(_) | Entry::Phase(_) | Entry::WorkingOn(_) | Entry::Tasks(_) => 0,
        Entry::Stream(_) | Entry::Said => 0,
        Entry::Plan(_) => 4_000,
        Entry::Context(_) | Entry::Exit(_) => 0,
        _ => 1_500,
    }
}

/// What the agent does once an ask is answered.
struct PendingAsk {
    key: String,
    item_key: String,
    /// The call the ask gates, re-encoded when it resolves.
    tool: Option<ToolCall>,
    output: String,
    ask_item: Option<AskItem>,
    questions: Vec<QuestionItem>,
    then: Vec<Entry>,
}

struct Sim {
    spec: AgentSpec,
    kind: Kind,
    entry: Agent,
    items: Vec<Item>,
    keys: HashMap<String, usize>,
    revision: u64,
    next_key: u64,
    facts: SnapshotFacts,
    queue: Vec<QueuedInput>,
    subs: Vec<Tx<SessionEvent>>,
    turn_open: Option<i64>,
    turns: u64,
    forced: Option<PhaseSpec>,
    asks: Vec<PendingAsk>,
    /// Scripts waiting to play, oldest first.
    jobs: VecDeque<Vec<Entry>>,
    playing: bool,
    /// Bumped by an interrupt or a stop, so a playing script stops.
    epoch: u64,
    /// The prose item a live script is streaming into.
    streaming: Option<String>,
}

impl Sim {
    fn id(&self) -> &[u8] {
        &self.entry.agent_id
    }

    fn exited(&self) -> bool {
        self.entry.lifecycle == Lifecycle::Exited as i32
    }

    fn phase(&self) -> Phase {
        if self.exited() {
            return Phase::Idle;
        }
        if let Some(forced) = self.forced {
            return match forced {
                PhaseSpec::Starting => Phase::Starting,
                PhaseSpec::Idle => Phase::Idle,
                PhaseSpec::Working => Phase::Working,
                PhaseSpec::NeedsYou => Phase::NeedsYou,
            };
        }
        if !self.facts.claude_asks.is_empty() || !self.facts.codex_asks.is_empty() {
            Phase::NeedsYou
        } else if self.turn_open.is_some() || self.playing {
            Phase::Working
        } else {
            Phase::Idle
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut facts = self.facts.clone();
        facts.running_turn = self.turn_open.is_some();
        Snapshot {
            agent: self.id().to_vec(),
            revision: self.revision,
            queue: self.queue.clone(),
            kind: wire::kind_tag(self.kind).into(),
            body: body::encode_snapshot(self.kind, &facts),
            phase: self.phase() as i32,
            working_on: self.entry.working_on.as_ref().map(|w| w.text.clone()),
            at_ms: now_ms(),
        }
    }

    fn send(&mut self, of: session_event::Of) {
        let event = SessionEvent { of: Some(of) };
        self.subs.retain(|tx| tx.send(Ok(event.clone())).is_ok());
    }

    fn push_snapshot(&mut self) {
        self.revision += 1;
        let snapshot = self.snapshot();
        self.send(session_event::Of::Snapshot(snapshot));
    }

    fn fresh_key(&mut self) -> String {
        self.next_key += 1;
        format!("i{}", self.next_key)
    }

    /// Writes an item: a new key appends at the next order, a known key is
    /// a new revision of that item.
    fn commit(&mut self, key: &str, body: &Body, text: &str, at_ms: i64, input_id: &[u8]) {
        self.revision += 1;
        let item = Item {
            agent: self.id().to_vec(),
            key: key.to_owned(),
            order: 0,
            revision: self.revision,
            producer_version: "lab".into(),
            input_id: input_id.to_vec(),
            text: text.to_owned(),
            attachments: Vec::new(),
            kind: wire::kind_tag(self.kind).into(),
            body: body::encode(self.kind, body),
            at_ms,
        };
        let item = match self.keys.get(key) {
            Some(&index) => {
                let held = &mut self.items[index];
                *held = Item {
                    order: held.order,
                    at_ms: held.at_ms,
                    input_id: if input_id.is_empty() {
                        held.input_id.clone()
                    } else {
                        input_id.to_vec()
                    },
                    ..item
                };
                held.clone()
            }
            None => {
                let item = Item {
                    order: self.items.len() as u64 + 1,
                    ..item
                };
                self.keys.insert(key.to_owned(), self.items.len());
                self.items.push(item.clone());
                item
            }
        };
        self.entry.last_activity_ms = self.entry.last_activity_ms.max(at_ms);
        self.send(session_event::Of::Item(item));
    }

    fn append(&mut self, key: &str, text: &str) {
        let Some(&index) = self.keys.get(key) else {
            return;
        };
        self.revision += 1;
        let revision = self.revision;
        let held = &mut self.items[index];
        let base = held.revision;
        held.revision = revision;
        held.text.push_str(text);
        let append = wire::Append {
            agent: held.agent.clone(),
            key: key.to_owned(),
            base_revision: base,
            revision,
            text: text.to_owned(),
        };
        self.send(session_event::Of::Append(append));
    }

    fn working_on(&mut self, text: Option<String>, at_ms: i64) {
        self.entry.working_on = text.map(|text| WorkingOn {
            text,
            updated_at_ms: at_ms,
        });
    }
}

struct Inner {
    scenario: Scenario,
    local_host: Vec<u8>,
    hosts: Vec<HostEntry>,
    agents: Vec<Sim>,
    inventory: Vec<Tx<InventoryEvent>>,
    blobs: HashMap<Vec<u8>, (BlobRef, Vec<u8>)>,
    /// Beats of the timeline applied so far.
    fired: usize,
    created: u32,
}

impl Inner {
    fn sim(&mut self, id: &[u8]) -> Option<&mut Sim> {
        self.agents.iter_mut().find(|sim| sim.id() == id)
    }

    fn index(&self, id: &[u8]) -> Option<usize> {
        self.agents.iter().position(|sim| sim.id() == id)
    }

    fn broadcast(&mut self, of: inventory_event::Of) {
        let event = InventoryEvent { of: Some(of) };
        self.inventory
            .retain(|tx| tx.send(Ok(event.clone())).is_ok());
    }

    /// Recomputes the agent's phase and tells the inventory and the chat.
    fn touch(&mut self, index: usize) {
        let sim = &mut self.agents[index];
        sim.entry.phase = sim.phase() as i32;
        sim.push_snapshot();
        let entry = sim.entry.clone();
        self.broadcast(inventory_event::Of::Agent(entry));
    }

    fn host_online(&self, host_id: &[u8]) -> bool {
        self.hosts
            .iter()
            .find(|host| host.host_id == host_id)
            .is_none_or(|host| host.presence == wire::Presence::Online as i32)
    }

    fn put_blob(&mut self, name: &str, mime: &str, bytes: Vec<u8>) -> BlobRef {
        let hash = Sha256::digest(&bytes).to_vec();
        let blob = BlobRef {
            hash: hash.clone(),
            name: name.to_owned(),
            mime: mime.to_owned(),
            size: bytes.len() as u64,
        };
        self.blobs.insert(hash, (blob.clone(), bytes));
        blob
    }
}

/// The world the lab's client reads and changes.
pub struct World {
    inner: Mutex<Inner>,
    /// Scripts apply at once, with no pacing: the headless renderer's
    /// world, where a frame must not depend on timing.
    instant: bool,
}

impl World {
    /// Builds the scenario's world with its first `fired` beats already
    /// applied.
    pub fn new(scenario: Scenario, fired: usize, instant: bool) -> Arc<World> {
        let local_host = scenario.local_host.as_bytes().to_vec();
        let mut hosts = vec![HostEntry {
            host_id: local_host.clone(),
            name: scenario.local_host.clone(),
            version: Some("lab".into()),
            via: HostVia::Direct as i32,
            signed_in: Some(true),
            platform: Some("macos".into()),
            generation: 1,
            trust: wire::Trust::Trusted as i32,
            presence: wire::Presence::Online as i32,
            ..HostEntry::default()
        }];
        for host in &scenario.hosts {
            hosts.push(HostEntry {
                host_id: host.id.as_bytes().to_vec(),
                name: host.name.clone().unwrap_or_else(|| host.id.clone()),
                version: Some("lab".into()),
                last_dial_error: host.dial_error.clone(),
                via: match host.via {
                    Via::Direct => HostVia::Direct,
                    Via::Relay => HostVia::Relay,
                    Via::Ssh => HostVia::Ssh,
                } as i32,
                signed_in: host.signed_in.or(Some(true)),
                platform: host.platform.clone(),
                generation: 1,
                trust: if host.candidate {
                    wire::Trust::Candidate
                } else {
                    wire::Trust::Trusted
                } as i32,
                presence: presence(host.presence) as i32,
                revoked: host.revoked.then_some(true),
                ..HostEntry::default()
            });
        }
        let world = Arc::new(World {
            inner: Mutex::new(Inner {
                scenario: scenario.clone(),
                local_host,
                hosts,
                agents: Vec::new(),
                inventory: Vec::new(),
                blobs: HashMap::new(),
                fired: 0,
                created: 0,
            }),
            instant,
        });
        for spec in &scenario.agents {
            world.add_agent(spec.clone(), now_ms());
        }
        for beat in scenario.timeline.iter().take(fired) {
            world.apply_beat(beat, true);
        }
        world.inner.lock().unwrap().fired = fired.min(scenario.timeline.len());
        world
    }

    pub fn fired(&self) -> usize {
        self.inner.lock().unwrap().fired
    }

    pub fn local_host(&self) -> Vec<u8> {
        self.inner.lock().unwrap().local_host.clone()
    }

    /// Plays the rest of the timeline in real time.
    pub fn start_timeline(self: &Arc<Self>) {
        let world = self.clone();
        tokio::spawn(async move {
            loop {
                let beat = {
                    let inner = world.inner.lock().unwrap();
                    inner.scenario.timeline.get(inner.fired).cloned()
                };
                let Some(beat) = beat else {
                    return;
                };
                tokio::time::sleep(Duration::from_millis(beat.after.ms().max(0) as u64)).await;
                world.apply_beat(&beat, false);
                world.inner.lock().unwrap().fired += 1;
            }
        });
    }

    fn apply_beat(self: &Arc<Self>, beat: &Beat, instant: bool) {
        if let Some(spec) = &beat.create {
            self.add_agent((**spec).clone(), now_ms());
        }
        if let Some(id) = &beat.remove {
            self.remove(id.as_bytes());
        }
        if let (Some(host), Some(to)) = (&beat.host, beat.presence) {
            self.set_presence(host.as_bytes(), to);
        }
        if let Some(agent) = &beat.agent
            && !beat.play.is_empty()
        {
            let id = agent.as_bytes().to_vec();
            if instant || self.instant {
                let mut inner = self.inner.lock().unwrap();
                if let Some(index) = inner.index(&id) {
                    let mut at = now_ms();
                    play_now(&mut inner, index, beat.play.clone(), &mut at);
                    inner.touch(index);
                }
            } else {
                self.enqueue(&id, beat.play.clone());
            }
        }
    }

    fn set_presence(&self, host_id: &[u8], to: Presence) {
        let mut inner = self.inner.lock().unwrap();
        let Some(host) = inner.hosts.iter_mut().find(|h| h.host_id == host_id) else {
            return;
        };
        host.presence = presence(to) as i32;
        host.generation += 1;
        let entry = host.clone();
        inner.broadcast(inventory_event::Of::Host(entry));
        for sim in inner
            .agents
            .iter_mut()
            .filter(|s| s.entry.host_id == host_id)
        {
            let revision = sim.revision;
            if to == Presence::Online {
                sim.send(session_event::Of::CaughtUp(wire::CaughtUp { revision }));
            } else {
                sim.send(session_event::Of::Detached(wire::Detached {}));
            }
        }
    }

    /// Adds an agent with its authored transcript laid out in the past.
    fn add_agent(&self, spec: AgentSpec, now: i64) -> Agent {
        let mut inner = self.inner.lock().unwrap();
        let kind = kind_of(spec.kind);
        let host_id = spec
            .host
            .clone()
            .unwrap_or_else(|| inner.scenario.local_host.clone());
        let parent = spec.parent.as_ref().map(|parent| {
            let host_id = inner
                .agents
                .iter()
                .find(|sim| sim.id() == parent.as_bytes())
                .map(|sim| sim.entry.host_id.clone())
                .unwrap_or_else(|| host_id.as_bytes().to_vec());
            AgentParent {
                host_id,
                agent_id: parent.as_bytes().to_vec(),
            }
        });
        let ago = spec.ago.map_or(10 * 60_000, |ago| ago.ms());
        let span: i64 = spec.transcript.iter().map(nominal_ms).sum();
        let start = now - ago - span;
        let entry = Agent {
            agent_id: spec.id.as_bytes().to_vec(),
            host_id: host_id.as_bytes().to_vec(),
            kind: kind as i32,
            name: Some(spec.name.clone().unwrap_or_else(|| spec.id.clone())),
            cwd: spec
                .cwd
                .clone()
                .unwrap_or_else(|| inner.scenario.cwd.clone()),
            parent,
            created_at_ms: start - 5_000,
            lifecycle: Lifecycle::Live as i32,
            exit_cause: None,
            phase: Phase::Idle as i32,
            working_on: None,
            last_activity_ms: start,
            producer_version: "lab".into(),
            incarnation: 1,
        };
        let facts = SnapshotFacts {
            model: spec.model.clone().or_else(|| {
                Some(
                    match kind {
                        Kind::Codex => "gpt-5-codex",
                        _ => "opus",
                    }
                    .into(),
                )
            }),
            effort: spec.effort.clone(),
            mode: spec.mode.clone().or_else(|| {
                Some(
                    match kind {
                        Kind::Codex => "on-request",
                        _ => "default",
                    }
                    .into(),
                )
            }),
            context: spec.context.as_ref().map(|c| wire::ContextMeter {
                known: true,
                used_tokens: c.used,
                window_tokens: Some(c.window.unwrap_or(200_000)),
                breakdown: Vec::new(),
            }),
            usage: spec.usage.as_ref().map(|u| wire::UsageLimits {
                state: match u.state {
                    UsageStateSpec::Ok => wire::UsageState::Ok,
                    UsageStateSpec::Near => wire::UsageState::NearLimit,
                    UsageStateSpec::Blocked => wire::UsageState::Blocked,
                } as i32,
                windows: u
                    .windows
                    .iter()
                    .map(|w| wire::UsageWindow {
                        name: w.name.clone(),
                        used_percent: w.used,
                        resets_at_ms: w.resets_in.map(|d| now + d.ms()),
                    })
                    .collect(),
                credits: None,
            }),
            tasks: (!spec.tasks.is_empty()).then(|| task_list(&spec.tasks)),
            sign_in: spec.sign_in.as_ref().map(|s| wire::SignIn {
                state: match s.state {
                    SignInStateSpec::SignedIn => wire::SignInState::SignedIn,
                    SignInStateSpec::SignedOut => wire::SignInState::SignedOut,
                    SignInStateSpec::Expired => wire::SignInState::Expired,
                    SignInStateSpec::Failed => wire::SignInState::Failed,
                } as i32,
                account: s.account.clone(),
                message: s.message.clone(),
            }),
            background: spec.background,
            servers: (!spec.failed_servers.is_empty()).then(|| wire::ToolServerHealth {
                state: wire::HealthState::Degraded as i32,
                servers: spec
                    .failed_servers
                    .iter()
                    .map(|s| wire::ToolServer {
                        name: s.name.clone(),
                        status: wire::ToolServerStatus::Failed as i32,
                        error: s.error.clone(),
                    })
                    .collect(),
            }),
            ..SnapshotFacts::default()
        };
        let queue = spec
            .queue
            .iter()
            .enumerate()
            .map(|(n, text)| QueuedInput {
                input_id: format!("queued-{}-{n}", spec.id).into_bytes(),
                text: text.clone(),
                ..QueuedInput::default()
            })
            .collect();
        let mut sim = Sim {
            kind,
            entry,
            items: Vec::new(),
            keys: HashMap::new(),
            revision: 0,
            next_key: 0,
            facts,
            queue,
            subs: Vec::new(),
            turn_open: None,
            turns: 0,
            forced: spec.phase,
            asks: Vec::new(),
            jobs: VecDeque::new(),
            playing: false,
            epoch: 0,
            streaming: None,
            spec: spec.clone(),
        };
        sim.working_on(spec.working_on.clone(), now - ago);
        inner.agents.push(sim);
        let index = inner.agents.len() - 1;
        let mut at = start;
        play_now(&mut inner, index, spec.transcript.clone(), &mut at);
        let sim = &mut inner.agents[index];
        // A turn's end clears working-on; the scenario's own words win, so
        // an idle agent can carry what it last said.
        if spec.working_on.is_some() {
            sim.working_on(spec.working_on.clone(), now - ago);
        }
        if let Some(cause) = &spec.exited {
            sim.entry.lifecycle = Lifecycle::Exited as i32;
            sim.entry.exit_cause = Some(cause.clone());
        }
        sim.entry.last_activity_ms = now - ago;
        inner.touch(index);
        inner.agents[index].entry.clone()
    }

    fn remove(&self, id: &[u8]) -> Vec<Agent> {
        let mut inner = self.inner.lock().unwrap();
        let mut doomed = vec![id.to_vec()];
        let mut i = 0;
        while i < doomed.len() {
            let parent = doomed[i].clone();
            for sim in &inner.agents {
                if sim
                    .entry
                    .parent
                    .as_ref()
                    .is_some_and(|p| p.agent_id == parent)
                    && !doomed.contains(&sim.entry.agent_id)
                {
                    doomed.push(sim.entry.agent_id.clone());
                }
            }
            i += 1;
        }
        let mut removed = Vec::new();
        for id in &doomed {
            if let Some(index) = inner.index(id) {
                let sim = inner.agents.remove(index);
                let host_id = sim.entry.host_id.clone();
                removed.push(sim.entry);
                inner.broadcast(inventory_event::Of::AgentRemoved(wire::AgentRemoved {
                    host_id,
                    agent_id: id.clone(),
                    reason: Some("deleted".into()),
                }));
            }
        }
        removed
    }

    /// Queues a script on an agent and starts its player if it is idle.
    fn enqueue(self: &Arc<Self>, id: &[u8], entries: Vec<Entry>) {
        if self.instant {
            let mut inner = self.inner.lock().unwrap();
            if let Some(index) = inner.index(id) {
                let mut at = now_ms();
                play_now(&mut inner, index, entries, &mut at);
                inner.touch(index);
            }
            return;
        }
        let start = {
            let mut inner = self.inner.lock().unwrap();
            let Some(index) = inner.index(id) else {
                return;
            };
            let sim = &mut inner.agents[index];
            sim.jobs.push_back(entries);
            let start = !sim.playing;
            sim.playing = true;
            inner.touch(index);
            start
        };
        if start {
            let world = self.clone();
            let id = id.to_vec();
            tokio::spawn(async move { world.player(id).await });
        }
    }

    async fn player(self: Arc<Self>, id: Vec<u8>) {
        loop {
            let (job, epoch) = {
                let mut inner = self.inner.lock().unwrap();
                let Some(index) = inner.index(&id) else {
                    return;
                };
                let sim = &mut inner.agents[index];
                match sim.jobs.pop_front() {
                    Some(job) => (job, sim.epoch),
                    None => {
                        sim.playing = false;
                        inner.touch(index);
                        return;
                    }
                }
            };
            let mut entries: VecDeque<Entry> = job.into();
            while let Some(entry) = entries.pop_front() {
                if !self.live_entry(&id, epoch, entry, &mut entries).await {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(180)).await;
            }
        }
    }

    /// Plays one entry with its pacing; false when the script must stop,
    /// because it was interrupted or it opened an ask.
    async fn live_entry(
        &self,
        id: &[u8],
        epoch: u64,
        entry: Entry,
        rest: &mut VecDeque<Entry>,
    ) -> bool {
        let current = |world: &World| {
            let inner = world.inner.lock().unwrap();
            inner
                .index(id)
                .is_some_and(|i| inner.agents[i].epoch == epoch)
        };
        match entry {
            Entry::Wait(d) => {
                tokio::time::sleep(Duration::from_millis(d.ms().max(0) as u64)).await;
                current(self)
            }
            Entry::Say(text) => {
                let key = {
                    let mut inner = self.inner.lock().unwrap();
                    let Some(index) = inner.index(id) else {
                        return false;
                    };
                    let sim = &mut inner.agents[index];
                    let key = sim.fresh_key();
                    sim.commit(&key, &Body::Message { complete: false }, "", now_ms(), &[]);
                    sim.streaming = Some(key.clone());
                    key
                };
                let words: Vec<&str> = text.split_inclusive(' ').collect();
                for chunk in words.chunks(3) {
                    tokio::time::sleep(Duration::from_millis(45)).await;
                    let mut inner = self.inner.lock().unwrap();
                    let Some(index) = inner.index(id) else {
                        return false;
                    };
                    let sim = &mut inner.agents[index];
                    if sim.epoch != epoch {
                        return false;
                    }
                    sim.append(&key, &chunk.concat());
                }
                let mut inner = self.inner.lock().unwrap();
                let Some(index) = inner.index(id) else {
                    return false;
                };
                let sim = &mut inner.agents[index];
                sim.streaming = None;
                sim.commit(
                    &key,
                    &Body::Message { complete: true },
                    &text,
                    now_ms(),
                    &[],
                );
                inner.touch(index);
                true
            }
            Entry::Think(text) => {
                let key = {
                    let mut inner = self.inner.lock().unwrap();
                    let Some(index) = inner.index(id) else {
                        return false;
                    };
                    let sim = &mut inner.agents[index];
                    let key = sim.fresh_key();
                    sim.commit(&key, &Body::Thinking { complete: false }, "", now_ms(), &[]);
                    key
                };
                tokio::time::sleep(Duration::from_millis(1_400)).await;
                let mut inner = self.inner.lock().unwrap();
                let Some(index) = inner.index(id) else {
                    return false;
                };
                let sim = &mut inner.agents[index];
                if sim.epoch != epoch {
                    return false;
                }
                sim.commit(
                    &key,
                    &Body::Thinking { complete: true },
                    &text,
                    now_ms(),
                    &[],
                );
                true
            }
            Entry::Plan(text) => self.live_plan(id, epoch, text, rest).await,
            entry if tool_call(&entry).is_some() => {
                let (tool, output) = tool_call(&entry).unwrap();
                let running = tool.state == ToolState::Running as i32;
                let took = match &entry {
                    Entry::Bash(BashSpec::Full {
                        took: Some(took), ..
                    }) => took.ms().min(4_000),
                    Entry::Bash(_) => 1_200,
                    Entry::Edit(_) | Entry::Write(_) => 600,
                    Entry::Subagent(_) => 2_500,
                    _ => 350,
                };
                let key = {
                    let mut inner = self.inner.lock().unwrap();
                    let Some(index) = inner.index(id) else {
                        return false;
                    };
                    let sim = &mut inner.agents[index];
                    let key = sim.fresh_key();
                    let mut started = tool.clone();
                    started.state = ToolState::Running as i32;
                    started.outcome_text.clear();
                    started.ended_at_ms = None;
                    started.exit_code = None;
                    if let Some(sub) = &mut started.subagent {
                        sub.finished = false;
                    }
                    sim.commit(&key, &Body::Tool(started), "", now_ms(), &[]);
                    inner.touch(index);
                    key
                };
                if running {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(took as u64)).await;
                let mut inner = self.inner.lock().unwrap();
                let Some(index) = inner.index(id) else {
                    return false;
                };
                let sim = &mut inner.agents[index];
                if sim.epoch != epoch {
                    return false;
                }
                let mut done = tool;
                done.ended_at_ms = Some(now_ms());
                sim.commit(&key, &Body::Tool(done), &output, now_ms(), &[]);
                true
            }
            entry => {
                let mut inner = self.inner.lock().unwrap();
                let Some(index) = inner.index(id) else {
                    return false;
                };
                if inner.agents[index].epoch != epoch {
                    return false;
                }
                let mut at = now_ms();
                let rest_now: Vec<Entry> = rest.drain(..).collect();
                let mut script = vec![entry];
                script.extend(rest_now);
                // An ask takes the rest of the script as what follows it; any
                // other entry is applied alone and the rest goes back.
                let asks = matches!(script[0], Entry::Ask(_));
                if asks {
                    play_now(&mut inner, index, script, &mut at);
                    inner.touch(index);
                    false
                } else {
                    let first = script.remove(0);
                    rest.extend(script);
                    let ended = matches!(first, Entry::Turn(_) | Entry::Exit(_));
                    play_now(&mut inner, index, vec![first], &mut at);
                    inner.touch(index);
                    if ended {
                        drop(inner);
                        self.next_queued(id);
                    }
                    true
                }
            }
        }
    }

    /// A plan played live. Headless Claude streams its plan file's Write and
    /// Codex its plan message, a few lines at a time; Claude in a terminal
    /// writes the file whole. Claude then asks with ExitPlanMode, taking the
    /// rest of the script as what follows approval.
    async fn live_plan(
        &self,
        id: &[u8],
        epoch: u64,
        text: String,
        rest: &mut VecDeque<Entry>,
    ) -> bool {
        let kind = {
            let inner = self.inner.lock().unwrap();
            match inner.index(id) {
                Some(index) => inner.agents[index].kind,
                None => return false,
            }
        };
        let ask = Entry::Ask(AskSpec::Plan(crate::scenario::PlanSpec {
            plan: text.clone(),
            then: None,
        }));
        if kind == Kind::ClaudePty {
            rest.push_front(ask);
            rest.push_front(Entry::Write(crate::scenario::WriteSpec {
                path: plan_path(&text),
                content: text,
            }));
            return true;
        }
        let path = plan_path(&text);
        let written = |sim: &mut Sim, key: &str, so_far: &str, done: bool| match kind {
            Kind::Codex => {
                let tagged = format!(
                    "<proposed_plan>\n{so_far}{}",
                    if done { "\n</proposed_plan>" } else { "" }
                );
                sim.commit(
                    key,
                    &Body::Message { complete: done },
                    &tagged,
                    now_ms(),
                    &[],
                );
            }
            _ => {
                let state = if done {
                    ToolState::Succeeded
                } else {
                    ToolState::Running
                };
                let mut call = tool(
                    "Write",
                    json!({ "file_path": path, "content": so_far }),
                    state,
                );
                if done {
                    call.ended_at_ms = Some(now_ms());
                }
                sim.commit(key, &Body::Tool(call), "", now_ms(), &[]);
            }
        };
        let key = {
            let mut inner = self.inner.lock().unwrap();
            let Some(index) = inner.index(id) else {
                return false;
            };
            let sim = &mut inner.agents[index];
            let key = sim.fresh_key();
            written(sim, &key, "", false);
            inner.touch(index);
            key
        };
        let mut so_far = String::new();
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        for chunk in lines.chunks(2) {
            tokio::time::sleep(Duration::from_millis(140)).await;
            let mut inner = self.inner.lock().unwrap();
            let Some(index) = inner.index(id) else {
                return false;
            };
            let sim = &mut inner.agents[index];
            if sim.epoch != epoch {
                return false;
            }
            so_far.push_str(&chunk.concat());
            written(sim, &key, &so_far, false);
            inner.touch(index);
        }
        {
            let mut inner = self.inner.lock().unwrap();
            let Some(index) = inner.index(id) else {
                return false;
            };
            let sim = &mut inner.agents[index];
            written(sim, &key, &so_far, true);
            inner.touch(index);
        }
        if kind != Kind::Codex {
            rest.push_front(ask);
        }
        true
    }

    /// After a turn ends, the oldest queued prompt starts the next one.
    fn next_queued(&self, id: &[u8]) {
        let script = {
            let mut inner = self.inner.lock().unwrap();
            let Some(index) = inner.index(id) else {
                return;
            };
            let sim = &mut inner.agents[index];
            if sim.turn_open.is_some() || sim.queue.is_empty() || sim.exited() {
                return;
            }
            let queued = sim.queue.remove(0);
            let script = prompt_script(sim, &queued.text, &queued.input_id);
            inner.touch(index);
            script
        };
        // Prompts are played at the front of the agent's jobs.
        let mut inner = self.inner.lock().unwrap();
        if let Some(sim) = inner.sim(id) {
            sim.jobs.push_front(script);
        }
    }

    fn interrupt(&self, id: &[u8]) {
        let mut inner = self.inner.lock().unwrap();
        let Some(index) = inner.index(id) else {
            return;
        };
        let now = now_ms();
        let sim = &mut inner.agents[index];
        sim.epoch += 1;
        sim.jobs.clear();
        if let Some(key) = sim.streaming.take()
            && let Some(&i) = sim.keys.get(&key)
        {
            let text = sim.items[i].text.clone();
            sim.commit(&key, &Body::Message { complete: true }, &text, now, &[]);
        }
        // Calls still in flight are cancelled with the turn.
        let running: Vec<(String, ToolCall)> = sim
            .items
            .iter()
            .filter_map(|item| running_tool(sim.kind, item).map(|t| (item.key.clone(), t)))
            .collect();
        for (key, mut tool) in running {
            tool.state = ToolState::Cancelled as i32;
            sim.commit(&key, &Body::Tool(tool), "", now, &[]);
        }
        sim.facts.claude_asks.clear();
        sim.facts.codex_asks.clear();
        sim.asks.clear();
        if sim.turn_open.is_some() {
            let mut at = now;
            end_turn(sim, TurnOutcomeSpec::Interrupted, None, None, &mut at);
        }
        inner.touch(index);
    }

    fn answer(self: &Arc<Self>, id: &[u8], ask_key: &str, verdict: Verdict) -> bool {
        let then = {
            let mut inner = self.inner.lock().unwrap();
            let Some(index) = inner.index(id) else {
                return false;
            };
            let sim = &mut inner.agents[index];
            let Some(at) = sim.asks.iter().position(|a| a.key == ask_key) else {
                return false;
            };
            let pending = sim.asks.remove(at);
            sim.facts.claude_asks.retain(|a| a.key != ask_key);
            sim.facts.codex_asks.retain(|a| a.key != ask_key);
            let now = now_ms();
            let allowed = verdict.allowed();
            if let Some(mut tool) = pending.tool.clone() {
                tool.state = if allowed && tool.exit_code.unwrap_or(0) != 0 {
                    ToolState::Failed
                } else if allowed {
                    ToolState::Succeeded
                } else {
                    ToolState::Denied
                } as i32;
                // A denied call never ran, so it has no exit.
                if !allowed {
                    tool.exit_code = None;
                }
                tool.decision = Some(ToolDecision {
                    outcome: if allowed {
                        wire::DecisionOutcome::Allowed
                    } else {
                        wire::DecisionOutcome::Denied
                    } as i32,
                    scope: verdict.scope(),
                    note: verdict.note(),
                    elsewhere: false,
                });
                tool.ended_at_ms = Some(now);
                if tool.name == "AskUserQuestion"
                    && let Verdict::Answered { picks, .. } = &verdict
                {
                    let answers: serde_json::Map<String, Value> = pending
                        .questions
                        .iter()
                        .zip(picks)
                        .map(|(q, pick)| (q.question.clone(), Value::String(pick.join(", "))))
                        .collect();
                    tool.outcome_json = json!({ "answers": answers }).to_string().into_bytes();
                }
                let output = if allowed {
                    pending.output.clone()
                } else {
                    String::new()
                };
                // An approved plan leaves plan mode, as Claude does: for
                // accepting edits, or back to asking.
                if allowed && tool.name == "ExitPlanMode" {
                    sim.facts.mode = Some(if verdict.scope() == "acceptEdits" {
                        "acceptEdits".into()
                    } else {
                        "default".into()
                    });
                }
                sim.commit(&pending.item_key, &Body::Tool(tool), &output, now, &[]);
            }
            if let Some(mut ask) = pending.ask_item.clone() {
                ask.closed = Some(wire::AskClosed {
                    outcome: if allowed {
                        wire::AskOutcome::Answered
                    } else {
                        wire::AskOutcome::Declined
                    } as i32,
                    answers: match &verdict {
                        Verdict::Answered { picks, .. } => picks
                            .iter()
                            .enumerate()
                            .map(|(at, picked)| {
                                let secret = pending.questions.get(at).is_some_and(|q| q.secret);
                                wire::AnsweredQuestion {
                                    picked: if secret { Vec::new() } else { picked.clone() },
                                    other: None,
                                    hidden: secret && !picked.is_empty(),
                                }
                            })
                            .collect(),
                        _ => Vec::new(),
                    },
                    note: verdict.note(),
                    fields: match &verdict {
                        Verdict::Sent { fields } => fields.clone(),
                        _ => Vec::new(),
                    },
                    grant: match &verdict {
                        Verdict::Granted(granted) => Some(granted.clone()),
                        _ => None,
                    },
                });
                sim.commit(&pending.item_key, &Body::Ask(ask), "", now, &[]);
            }
            inner.touch(index);
            // A reply sent instead of answering questions (Claude takes it as
            // the question's refusal, Codex as a note on the answers): the
            // agent goes on from the person's words.
            let note = verdict.note();
            if let Some(words) = ui_view::reply_words(&note) {
                let first: String = words
                    .split_whitespace()
                    .take(12)
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut then = vec![Entry::Say(format!(
                    "Thanks, going with that rather than the options: \"{first}\"."
                ))];
                if pending.then.is_empty() {
                    then.push(Entry::Turn(Default::default()));
                } else {
                    then.extend(pending.then);
                }
                then
            } else if allowed {
                if pending.then.is_empty() {
                    vec![
                        Entry::Say("Thanks — carrying on from there.".into()),
                        Entry::Turn(Default::default()),
                    ]
                } else {
                    pending.then
                }
            } else if let Some(plan) = pending
                .tool
                .as_ref()
                .filter(|tool| tool.name == "ExitPlanMode")
                .and_then(|tool| serde_json::from_slice::<Value>(&tool.input_json).ok())
                .and_then(|input| input.get("plan").and_then(Value::as_str).map(str::to_owned))
            {
                // Sent back, Claude reworks the plan and asks again.
                let note = verdict.note();
                let change = if note.trim().is_empty() {
                    "Tightened the steps after review.".to_owned()
                } else {
                    note.trim().to_owned()
                };
                let base = plan
                    .split("\n\n### Changed after review")
                    .next()
                    .unwrap_or(&plan)
                    .to_owned();
                let mut then = vec![
                    Entry::Read("docs/JOURNAL_AND_STORE.md".into()),
                    Entry::Plan(format!(
                        "{base}\n\n### Changed after review\n\n- {change}\n"
                    )),
                ];
                then.extend(pending.then);
                then
            } else {
                vec![
                    Entry::Say("Understood, I won't do that. What would you like instead?".into()),
                    Entry::Turn(Default::default()),
                ]
            }
        };
        self.enqueue(id, then);
        true
    }

    fn prompt(self: &Arc<Self>, id: &[u8], text: &str, input_id: &[u8]) -> SendInputResponse {
        let queued = {
            let mut inner = self.inner.lock().unwrap();
            let Some(index) = inner.index(id) else {
                return rejected("not found");
            };
            let sim = &mut inner.agents[index];
            if sim.exited() {
                return rejected("exited");
            }
            let busy = sim.turn_open.is_some() || sim.playing;
            if busy && !self.instant {
                sim.queue.push(QueuedInput {
                    input_id: input_id.to_vec(),
                    text: text.to_owned(),
                    ..QueuedInput::default()
                });
                inner.touch(index);
                true
            } else {
                false
            }
        };
        if queued {
            return accepted(true);
        }
        let script = {
            let mut inner = self.inner.lock().unwrap();
            let sim = inner.sim(id).expect("the agent was found above");
            sim.forced = None;
            prompt_script(sim, text, input_id)
        };
        self.enqueue(id, script);
        accepted(false)
    }

    fn input(self: &Arc<Self>, id: &[u8], input: Input) -> SendInputResponse {
        use wire::{claude_pty_input as pty, claude_sdk_input as sdk, codex_input as cx, input};
        let input_id = input.input_id.clone();
        enum Act {
            Prompt(String),
            Answer(wire::AnswerInput),
            Approve(wire::Approve),
            Interrupt,
            Withdraw(Vec<u8>),
            SendNow(Vec<u8>),
            Mode(Option<String>),
            Approval(wire::SetApproval),
            Model(Option<String>),
            Effort(Option<String>),
            Clear,
            Other,
        }
        let act = match input.of {
            Some(input::Of::ClaudePty(i)) => match i.of {
                Some(pty::Of::Prompt(p)) => Act::Prompt(p.text),
                Some(pty::Of::Answer(a)) => Act::Answer(a),
                Some(pty::Of::Interrupt(_)) => Act::Interrupt,
                Some(pty::Of::Withdraw(w)) => Act::Withdraw(w.queued_input_id),
                Some(pty::Of::SendNow(s)) => Act::SendNow(s.queued_input_id),
                Some(pty::Of::Key(k)) if k.key() == wire::KeyName::CyclePermissionMode => {
                    Act::Mode(None)
                }
                Some(pty::Of::Clear(_)) => Act::Clear,
                _ => Act::Other,
            },
            Some(input::Of::ClaudeSdk(i)) => match i.of {
                Some(sdk::Of::Prompt(p)) => Act::Prompt(p.text),
                Some(sdk::Of::Answer(a)) => Act::Answer(a),
                Some(sdk::Of::Interrupt(_)) => Act::Interrupt,
                Some(sdk::Of::Withdraw(w)) => Act::Withdraw(w.queued_input_id),
                Some(sdk::Of::SendNow(s)) => Act::SendNow(s.queued_input_id),
                Some(sdk::Of::Mode(m)) => Act::Mode(Some(m.mode)),
                Some(sdk::Of::Model(m)) => Act::Model(m.model),
                Some(sdk::Of::Effort(e)) => Act::Effort(e.effort),
                Some(sdk::Of::Clear(_)) => Act::Clear,
                None => Act::Other,
            },
            Some(input::Of::Codex(i)) => match i.of {
                Some(cx::Of::Prompt(p)) => Act::Prompt(p.text),
                Some(cx::Of::Answer(a)) => Act::Answer(a),
                Some(cx::Of::Approve(a)) => Act::Approve(a),
                Some(cx::Of::Interrupt(_)) => Act::Interrupt,
                Some(cx::Of::Withdraw(w)) => Act::Withdraw(w.queued_input_id),
                Some(cx::Of::SendNow(s)) => Act::SendNow(s.queued_input_id),
                Some(cx::Of::Approval(a)) => Act::Approval(a),
                Some(cx::Of::Model(m)) => Act::Model(m.model),
                Some(cx::Of::Effort(e)) => Act::Effort(e.effort),
                None => Act::Other,
            },
            _ => Act::Other,
        };
        match act {
            Act::Prompt(text) => self.prompt(id, &text, &input_id),
            Act::Answer(answer) => {
                let verdict = verdict_of(&answer, &self.questions(id, &answer.ask_key));
                if self.answer(id, &answer.ask_key, verdict) {
                    accepted(false)
                } else {
                    rejected("that ask is no longer open")
                }
            }
            Act::Approve(approve) => {
                let verdict = match approve.decision() {
                    wire::Decision::Deny | wire::Decision::Abort => Verdict::Denied {
                        note: String::new(),
                    },
                    wire::Decision::ApproveSession => Verdict::Allowed {
                        scope: "for this session".into(),
                    },
                    _ => Verdict::Allowed {
                        scope: String::new(),
                    },
                };
                if self.answer(id, &approve.request_id, verdict) {
                    accepted(false)
                } else {
                    rejected("that ask is no longer open")
                }
            }
            Act::Interrupt => {
                self.interrupt(id);
                accepted(false)
            }
            Act::Withdraw(queued) => {
                let mut inner = self.inner.lock().unwrap();
                if let Some(index) = inner.index(id) {
                    inner.agents[index].queue.retain(|q| q.input_id != queued);
                    inner.touch(index);
                }
                accepted(false)
            }
            Act::SendNow(queued) => {
                let mut inner = self.inner.lock().unwrap();
                if let Some(index) = inner.index(id) {
                    let sim = &mut inner.agents[index];
                    if let Some(at) = sim.queue.iter().position(|q| q.input_id == queued) {
                        let q = sim.queue.remove(at);
                        let key = sim.fresh_key();
                        sim.commit(&key, &Body::Steer, &q.text, now_ms(), &q.input_id);
                    }
                    inner.touch(index);
                }
                accepted(false)
            }
            Act::Mode(mode) => {
                let mut inner = self.inner.lock().unwrap();
                if let Some(index) = inner.index(id) {
                    let sim = &mut inner.agents[index];
                    let next = mode.unwrap_or_else(|| {
                        const CYCLE: [&str; 3] = ["default", "acceptEdits", "plan"];
                        let now = sim.facts.mode.clone().unwrap_or_default();
                        let at = CYCLE.iter().position(|m| *m == now).map_or(0, |i| i + 1);
                        CYCLE[at % CYCLE.len()].into()
                    });
                    sim.facts.mode = Some(next);
                    inner.touch(index);
                }
                accepted(false)
            }
            Act::Approval(approval) => {
                let mut inner = self.inner.lock().unwrap();
                if let Some(index) = inner.index(id) {
                    let sim = &mut inner.agents[index];
                    sim.facts.mode = Some(approval.approval_policy);
                    sim.facts.sandbox = Some(approval.sandbox);
                    inner.touch(index);
                }
                accepted(false)
            }
            Act::Model(model) => {
                let mut inner = self.inner.lock().unwrap();
                if let Some(index) = inner.index(id) {
                    inner.agents[index].facts.model = model;
                    inner.touch(index);
                }
                accepted(false)
            }
            Act::Effort(effort) => {
                let mut inner = self.inner.lock().unwrap();
                if let Some(index) = inner.index(id) {
                    inner.agents[index].facts.effort = effort;
                    inner.touch(index);
                }
                accepted(false)
            }
            Act::Clear => {
                let mut inner = self.inner.lock().unwrap();
                if let Some(index) = inner.index(id) {
                    let mut at = now_ms();
                    play_now(
                        &mut inner,
                        index,
                        vec![Entry::Boundary(BoundarySpec::Cleared)],
                        &mut at,
                    );
                    inner.touch(index);
                }
                accepted(false)
            }
            Act::Other => rejected("the lab does not fake that input"),
        }
    }

    fn questions(&self, id: &[u8], ask_key: &str) -> Vec<QuestionItem> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .sim(id)
            .and_then(|sim| sim.asks.iter().find(|a| a.key == ask_key))
            .map(|a| a.questions.clone())
            .unwrap_or_default()
    }
}

fn presence(p: Presence) -> wire::Presence {
    match p {
        Presence::Online => wire::Presence::Online,
        Presence::Offline => wire::Presence::Offline,
        Presence::Away => wire::Presence::Away,
    }
}

fn accepted(queued: bool) -> SendInputResponse {
    SendInputResponse {
        of: Some(send_input_response::Of::Accepted(wire::Accepted { queued })),
    }
}

fn rejected(reason: &str) -> SendInputResponse {
    SendInputResponse {
        of: Some(send_input_response::Of::Rejected(wire::Rejected {
            reason: reason.into(),
        })),
    }
}

fn task_list(tasks: &[crate::scenario::TaskSpec]) -> wire::TaskList {
    wire::TaskList {
        known: true,
        entries: tasks
            .iter()
            .enumerate()
            .map(|(n, t)| wire::TaskListEntry {
                id: n.to_string(),
                subject: t.subject.clone(),
                status: match t.status {
                    TaskStatus::Pending => wire::TaskListStatus::Pending,
                    TaskStatus::Active => wire::TaskListStatus::InProgress,
                    TaskStatus::Done => wire::TaskListStatus::Completed,
                } as i32,
                active_form: t.subject.clone(),
            })
            .collect(),
    }
}

/// How a person answered an ask, whatever the kind.
enum Verdict {
    Allowed {
        scope: String,
    },
    Denied {
        note: String,
    },
    Answered {
        picks: Vec<Vec<String>>,
        note: String,
    },
    /// A form sent, with the names of its fields.
    Sent {
        fields: Vec<String>,
    },
    /// Access granted, for the turn or the session.
    Granted(wire::GrantAnswer),
}

impl Verdict {
    fn allowed(&self) -> bool {
        !matches!(self, Verdict::Denied { .. })
    }

    fn scope(&self) -> String {
        match self {
            Verdict::Allowed { scope } => scope.clone(),
            _ => String::new(),
        }
    }

    fn note(&self) -> String {
        match self {
            Verdict::Denied { note } | Verdict::Answered { note, .. } => note.clone(),
            Verdict::Allowed { .. } | Verdict::Sent { .. } | Verdict::Granted(_) => String::new(),
        }
    }
}

fn picks(answer: &wire::QuestionAnswer, questions: &[QuestionItem]) -> Vec<Vec<String>> {
    answer
        .answers
        .iter()
        .enumerate()
        .map(|(n, response)| {
            let options = questions
                .get(n)
                .map(|q| q.options.as_slice())
                .unwrap_or(&[]);
            let mut picked: Vec<String> = response
                .selected
                .iter()
                .filter_map(|&i| options.get(i as usize).map(option_label))
                .collect();
            if let Some(other) = &response.other {
                picked.push(other.clone());
            }
            picked
        })
        .collect()
}

fn verdict_of(answer: &wire::AnswerInput, questions: &[QuestionItem]) -> Verdict {
    use wire::{claude_answer, codex_answer, permission_answer, plan_answer};
    let sent = |content: &[u8]| Verdict::Sent {
        fields: serde_json::from_slice::<serde_json::Map<String, Value>>(content)
            .map(|fields| fields.keys().cloned().collect())
            .unwrap_or_default(),
    };
    let form = |action: wire::FormAction| match action {
        wire::FormAction::Accept => Verdict::Answered {
            picks: Vec::new(),
            note: String::new(),
        },
        _ => Verdict::Denied {
            note: String::new(),
        },
    };
    if answer.kind == "codex" {
        let Ok(decoded) = wire::CodexAnswer::decode(answer.body.as_slice()) else {
            return Verdict::Allowed {
                scope: String::new(),
            };
        };
        return match decoded.of {
            Some(codex_answer::Of::Question(q)) => Verdict::Answered {
                picks: picks(&q, questions),
                note: q.note,
            },
            Some(codex_answer::Of::Form(f)) if f.action() == wire::FormAction::Accept => {
                sent(&f.content_json)
            }
            Some(codex_answer::Of::Form(f)) => form(f.action()),
            Some(codex_answer::Of::Grant(g))
                if g.read.is_empty() && g.write.is_empty() && !g.network =>
            {
                Verdict::Denied {
                    note: String::new(),
                }
            }
            Some(codex_answer::Of::Grant(g)) => Verdict::Granted(g),
            Some(codex_answer::Of::Link(l)) => form(l.action()),
            _ => Verdict::Allowed {
                scope: String::new(),
            },
        };
    }
    let Ok(decoded) = wire::ClaudeAnswer::decode(answer.body.as_slice()) else {
        return Verdict::Allowed {
            scope: String::new(),
        };
    };
    match decoded.of {
        Some(claude_answer::Of::Permission(p)) => match p.of {
            Some(permission_answer::Of::Deny(d)) => Verdict::Denied { note: d.note },
            Some(permission_answer::Of::Allow(a)) => Verdict::Allowed {
                scope: a.scope.map(|_| "always".to_owned()).unwrap_or_default(),
            },
            None => Verdict::Allowed {
                scope: String::new(),
            },
        },
        Some(claude_answer::Of::Question(q)) => Verdict::Answered {
            picks: picks(&q, questions),
            note: q.note,
        },
        Some(claude_answer::Of::Plan(p)) => match p.of {
            Some(plan_answer::Of::SendBack(s)) => Verdict::Denied { note: s.note },
            Some(plan_answer::Of::Approve(a)) if a.auto_accept_edits => Verdict::Allowed {
                scope: "acceptEdits".to_owned(),
            },
            _ => Verdict::Allowed {
                scope: String::new(),
            },
        },
        Some(claude_answer::Of::Form(f)) if f.action() == wire::FormAction::Accept => {
            sent(&f.content_json)
        }
        Some(claude_answer::Of::Form(f)) => form(f.action()),
        Some(claude_answer::Of::Link(l)) => form(l.action()),
        None => Verdict::Allowed {
            scope: String::new(),
        },
    }
}

fn option_label(option: &OptionSpec) -> String {
    match option {
        OptionSpec::Label(label) => label.clone(),
        OptionSpec::Full { label, .. } => label.clone(),
    }
}

/// A prompt's reflection and the agent's scripted reply to it.
fn prompt_script(sim: &mut Sim, text: &str, input_id: &[u8]) -> Vec<Entry> {
    let key = sim.fresh_key();
    let now = now_ms();
    sim.commit(&key, &Body::Prompt, text, now, input_id);
    sim.turn_open = Some(now);
    let short: String = text.chars().take(60).collect();
    sim.working_on(Some(short), now);
    let implement = (text.trim() == ui_view::IMPLEMENT_PLAN)
        .then(|| sim.spec.implement.clone())
        .flatten();
    let reply = implement
        .or_else(|| sim.spec.reply.clone())
        .unwrap_or_else(|| {
            vec![
                Entry::Think("Working out what was asked.".into()),
                Entry::Read("src/lib.rs".into()),
                Entry::Grep(text.split_whitespace().next().unwrap_or("todo").into()),
                Entry::Say(format!(
                    "(lab reply) Here is where a real agent would answer: \"{text}\". \
                 Give this agent a `reply:` script in the scenario to make it say something \
                 specific."
                )),
                Entry::Turn(Default::default()),
            ]
        });
    reply
        .into_iter()
        .map(|entry| match entry {
            Entry::Say(say) => Entry::Say(say.replace("{prompt}", text)),
            other => other,
        })
        .collect()
}

fn running_tool(kind: Kind, item: &Item) -> Option<ToolCall> {
    let body = match kind {
        Kind::ClaudePty => match wire::ClaudePtyItem::decode(item.body.as_slice())
            .ok()?
            .kind?
        {
            wire::claude_pty_item::Kind::Tool(tool) => tool,
            _ => return None,
        },
        Kind::ClaudeSdk => match wire::ClaudeSdkItem::decode(item.body.as_slice())
            .ok()?
            .kind?
        {
            wire::claude_sdk_item::Kind::Tool(tool) => tool,
            _ => return None,
        },
        // Codex work is re-encoded from a Claude-shaped call; a running
        // Codex call keeps running when interrupted, which is close enough.
        _ => return None,
    };
    (body.state == ToolState::Running as i32 || body.state == ToolState::Pending as i32)
        .then_some(body)
}

fn tool(name: &str, input: Value, state: ToolState) -> ToolCall {
    ToolCall {
        name: name.to_owned(),
        input_json: input.to_string().into_bytes(),
        state: state as i32,
        class: body::class_of(name) as i32,
        ..ToolCall::default()
    }
}

/// The tool call an entry makes, with its output text; None for an entry
/// that is not a call.
fn tool_call(entry: &Entry) -> Option<(ToolCall, String)> {
    let done = ToolState::Succeeded;
    Some(match entry {
        Entry::Read(path) => (
            tool("Read", json!({ "file_path": path }), done),
            String::new(),
        ),
        Entry::Grep(pattern) => (
            tool("Grep", json!({ "pattern": pattern }), done),
            String::new(),
        ),
        Entry::Glob(pattern) => (
            tool("Glob", json!({ "pattern": pattern }), done),
            String::new(),
        ),
        Entry::Fetch(url) => (tool("WebFetch", json!({ "url": url }), done), String::new()),
        Entry::Search(query) => (
            tool("WebSearch", json!({ "query": query }), done),
            String::new(),
        ),
        Entry::Bash(BashSpec::Command(command)) => {
            let mut call = tool("Bash", json!({ "command": command }), done);
            call.exit_code = Some(0);
            (call, String::new())
        }
        Entry::Bash(BashSpec::Full {
            command,
            output,
            exit,
            background,
            running,
            ..
        }) => {
            let state = if *running {
                ToolState::Running
            } else if exit.unwrap_or(0) != 0 {
                ToolState::Failed
            } else {
                done
            };
            let mut call = tool("Bash", json!({ "command": command }), state);
            call.exit_code = if *running {
                None
            } else {
                Some(exit.unwrap_or(0))
            };
            call.background = *background;
            call.outcome_text = output.clone();
            (call, output.clone())
        }
        Entry::Edit(edit) => (
            tool(
                "Edit",
                json!({ "file_path": edit.path, "old_string": edit.old, "new_string": edit.new }),
                done,
            ),
            String::new(),
        ),
        Entry::Write(write) => (
            tool(
                "Write",
                json!({ "file_path": write.path, "content": write.content }),
                done,
            ),
            String::new(),
        ),
        Entry::Tool(spec) => {
            let state = match spec.state {
                ToolStateSpec::Pending => ToolState::Pending,
                ToolStateSpec::Running => ToolState::Running,
                ToolStateSpec::Ok => ToolState::Succeeded,
                ToolStateSpec::Failed => ToolState::Failed,
                ToolStateSpec::Denied => ToolState::Denied,
                ToolStateSpec::Cancelled => ToolState::Cancelled,
            };
            let mut call = tool(&spec.name, spec.input.clone(), state);
            call.server = spec.server.clone().unwrap_or_default();
            call.outcome_text = spec.output.clone();
            call.decision = spec.decision.as_ref().map(|d| ToolDecision {
                outcome: match d.outcome {
                    DecisionOutcomeSpec::Allowed => wire::DecisionOutcome::Allowed,
                    DecisionOutcomeSpec::Denied => wire::DecisionOutcome::Denied,
                    DecisionOutcomeSpec::Auto => wire::DecisionOutcome::AutoApproved,
                } as i32,
                scope: d.scope.clone(),
                note: d.note.clone(),
                elsewhere: false,
            });
            (call, spec.output.clone())
        }
        Entry::Subagent(sub) => {
            let state = if sub.running {
                ToolState::Running
            } else {
                done
            };
            let mut call = tool(
                "Agent",
                json!({ "description": sub.description, "prompt": sub.prompt }),
                state,
            );
            call.subagent = Some(wire::SubagentProgress {
                tool_count: sub.tools,
                last_tool: sub.last.clone(),
                finished: !sub.running,
            });
            call.outcome_text = sub.result.clone();
            (call, sub.result.clone())
        }
        _ => return None,
    })
}

fn end_turn(
    sim: &mut Sim,
    outcome: TurnOutcomeSpec,
    took: Option<i64>,
    cost: Option<f64>,
    at: &mut i64,
) {
    let started = took
        .map(|took| *at - took)
        .or(sim.turn_open)
        .unwrap_or(*at - 30_000);
    sim.turns += 1;
    let key = sim.fresh_key();
    let turn = wire::Turn {
        turn_id: sim.turns,
        outcome: match outcome {
            TurnOutcomeSpec::Completed => wire::TurnOutcome::Completed,
            TurnOutcomeSpec::Interrupted => wire::TurnOutcome::Interrupted,
            TurnOutcomeSpec::Failed => wire::TurnOutcome::Failed,
        } as i32,
        started_at_ms: started,
        cost_usd: cost,
    };
    sim.commit(&key, &Body::Turn(turn), "", *at, &[]);
    sim.turn_open = None;
    sim.working_on(None, *at);
}

/// Where Claude keeps a plan: its plans directory, named for the plan.
fn plan_path(plan: &str) -> String {
    let title = plan
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("plan")
        .trim_start_matches('#')
        .trim();
    let slug: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(4)
        .map(str::to_lowercase)
        .collect();
    format!("~/.claude/plans/{}.md", slug.join("-"))
}

/// Applies a script at once, advancing `at` by each entry's nominal time.
/// An ask stops the script: what follows it waits for the answer.
fn play_now(inner: &mut Inner, index: usize, entries: Vec<Entry>, at: &mut i64) {
    let mut entries: VecDeque<Entry> = entries.into();
    while let Some(entry) = entries.pop_front() {
        let local = inner.local_host.clone();
        let sim = &mut inner.agents[index];
        let step = nominal_ms(&entry);
        match entry {
            Entry::User(text) => {
                *at += step;
                let key = sim.fresh_key();
                sim.commit(&key, &Body::Prompt, &text, *at, &[]);
                sim.turn_open = Some(*at);
                sim.forced = None;
                continue;
            }
            Entry::Say(text) => {
                let key = sim.fresh_key();
                sim.commit(&key, &Body::Message { complete: true }, &text, *at, &[]);
            }
            Entry::Think(text) => {
                let key = sim.fresh_key();
                sim.commit(&key, &Body::Thinking { complete: true }, &text, *at, &[]);
            }
            Entry::Turn(turn) => {
                let took = turn.took.map(|d| d.ms());
                end_turn(sim, turn.outcome, took, turn.cost, at);
            }
            Entry::Error(error) => {
                let key = sim.fresh_key();
                let api = wire::ApiError {
                    error_kind: "overloaded".into(),
                    message: error.message,
                    will_retry: error.retry,
                    attempt: 1,
                    max_attempts: 3,
                    retry_at_ms: error.retry.then_some(*at + 10_000),
                };
                sim.commit(&key, &Body::ApiError(api), "", *at, &[]);
            }
            Entry::Message(message) => {
                let key = sim.fresh_key();
                let from = message.from.as_ref().map(|from| wire::Sender {
                    value: Some(wire::sender::Value::Agent(wire::AgentSender {
                        agent_id: from.as_bytes().to_vec(),
                        host_id: local.clone(),
                        name: from.clone(),
                        kind: "claude_sdk".into(),
                    })),
                });
                let body = wire::AgentMessage {
                    envelope_id: key.as_bytes().to_vec(),
                    kind: if message.finished {
                        wire::EnvelopeKind::Finished
                    } else {
                        wire::EnvelopeKind::Message
                    } as i32,
                    from,
                    context: Vec::new(),
                    to: message.to.clone().unwrap_or_default(),
                    send_state: wire::SendState::Sent as i32,
                    rejection: String::new(),
                };
                sim.commit(&key, &Body::AgentMessage(body), &message.text, *at, &[]);
            }
            Entry::Boundary(kind) => {
                let key = sim.fresh_key();
                let boundary = wire::Boundary {
                    kind: match kind {
                        BoundarySpec::Started => wire::BoundaryKind::Started,
                        BoundarySpec::Cleared => wire::BoundaryKind::Cleared,
                        BoundarySpec::Compacted => wire::BoundaryKind::Compacted,
                        BoundarySpec::Resumed => wire::BoundaryKind::Resumed,
                        BoundarySpec::Restarted => wire::BoundaryKind::Restarted,
                    } as i32,
                    ..wire::Boundary::default()
                };
                sim.commit(&key, &Body::Boundary(boundary), "", *at, &[]);
            }
            Entry::Wait(_) => {}
            Entry::Phase(phase) => sim.forced = Some(phase),
            Entry::WorkingOn(text) => sim.working_on(Some(text), *at),
            Entry::Exit(cause) => {
                let key = sim.fresh_key();
                let boundary = wire::Boundary {
                    kind: wire::BoundaryKind::Exited as i32,
                    cause: cause.clone(),
                    ..wire::Boundary::default()
                };
                sim.commit(&key, &Body::Boundary(boundary), "", *at, &[]);
                sim.entry.lifecycle = Lifecycle::Exited as i32;
                sim.entry.exit_cause = Some(cause);
                sim.turn_open = None;
            }
            Entry::Tasks(tasks) => sim.facts.tasks = Some(task_list(&tasks)),
            Entry::Stream(chunk) => {
                let key = match sim.streaming.clone() {
                    Some(key) => key,
                    None => {
                        let key = sim.fresh_key();
                        sim.commit(&key, &Body::Message { complete: false }, "", *at, &[]);
                        sim.streaming = Some(key.clone());
                        key
                    }
                };
                sim.append(&key, &chunk);
            }
            Entry::Said => {
                if let Some(key) = sim.streaming.take()
                    && let Some(&i) = sim.keys.get(&key)
                {
                    let text = sim.items[i].text.clone();
                    sim.commit(&key, &Body::Message { complete: true }, &text, *at, &[]);
                }
            }
            Entry::Context(c) => {
                sim.facts.context = Some(wire::ContextMeter {
                    known: true,
                    used_tokens: c.used,
                    window_tokens: Some(c.window.unwrap_or(200_000)),
                    breakdown: Vec::new(),
                })
            }
            // Codex has no plan approval: its plan is a message, and the
            // decision is the client's to offer.
            Entry::Ask(AskSpec::Plan(plan)) if sim.kind == Kind::Codex => {
                entries.push_front(Entry::Plan(plan.plan));
                continue;
            }
            Entry::Plan(text) if sim.kind == Kind::Codex => {
                let key = sim.fresh_key();
                let tagged = format!("<proposed_plan>\n{text}\n</proposed_plan>");
                sim.commit(&key, &Body::Message { complete: true }, &tagged, *at, &[]);
            }
            Entry::Plan(text) => {
                entries.push_front(Entry::Ask(AskSpec::Plan(crate::scenario::PlanSpec {
                    plan: text.clone(),
                    then: None,
                })));
                entries.push_front(Entry::Write(crate::scenario::WriteSpec {
                    path: plan_path(&text),
                    content: text,
                }));
                continue;
            }
            Entry::Ask(ask) => {
                open_ask(sim, ask, entries.drain(..).collect(), *at);
                return;
            }
            entry => {
                if let Some((mut call, output)) = tool_call(&entry) {
                    let key = sim.fresh_key();
                    if call.state != ToolState::Running as i32 {
                        call.ended_at_ms = Some(*at + step);
                    }
                    sim.commit(&key, &Body::Tool(call), &output, *at, &[]);
                }
            }
        }
        *at += step;
    }
}

/// Opens an ask: the call or ask item it hangs on, and the snapshot's open
/// ask in the kind's own shape.
fn open_ask(sim: &mut Sim, ask: AskSpec, rest: Vec<Entry>, at: i64) {
    let item_key = sim.fresh_key();
    let key = format!("ask-{item_key}");
    let codex = sim.kind == Kind::Codex;
    let questions_of = |items: &[QuestionItem]| wire::QuestionAsk {
        questions: items
            .iter()
            .map(|q| wire::Question {
                header: q.header.clone(),
                question: q.question.clone(),
                multi_select: q.multi,
                options: q
                    .options
                    .iter()
                    .map(|o| match o {
                        OptionSpec::Label(label) => wire::QuestionOption {
                            label: label.clone(),
                            ..Default::default()
                        },
                        OptionSpec::Full {
                            label,
                            description,
                            recommended,
                            preview,
                        } => wire::QuestionOption {
                            label: label.clone(),
                            description: description.clone(),
                            preview: preview.clone(),
                            recommended: *recommended,
                        },
                    })
                    .collect(),
                allow_other: q.other,
                secret: q.secret,
            })
            .collect(),
    };
    let mut pending = PendingAsk {
        key: key.clone(),
        item_key: item_key.clone(),
        tool: None,
        output: String::new(),
        ask_item: None,
        questions: Vec::new(),
        then: Vec::new(),
    };
    let then = |then: Option<Vec<Entry>>| {
        let mut all = then.unwrap_or_default();
        all.extend(rest.clone());
        all
    };
    let mut claude_body = None;
    let mut codex_body = None;
    match ask {
        AskSpec::Permission(p) => {
            let mut call = tool(&p.tool, p.input.clone(), ToolState::Pending);
            call.outcome_text.clear();
            pending.output = p.output.clone();
            if p.tool == "Bash" {
                call.exit_code = Some(p.exit.unwrap_or(0));
            }
            pending.tool = Some(call.clone());
            sim.commit(&item_key, &Body::Tool(call), "", at, &[]);
            let field = |name: &str| {
                p.input
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            claude_body = Some(wire::ask::Body::Permission(wire::PermissionAsk {
                tool_name: p.tool.clone(),
                input_json: p.input.to_string().into_bytes(),
                scopes: p
                    .scopes
                    .iter()
                    .enumerate()
                    .map(|(n, label)| wire::ScopeChoice {
                        index: n as u32,
                        destination: "localSettings".into(),
                        label: label.clone(),
                        ..Default::default()
                    })
                    .collect(),
                reason: p.reason.clone(),
                description: String::new(),
                deny_stops: false,
                deny_can_stop: true,
                server: String::new(),
            }));
            codex_body = Some(match p.tool.as_str() {
                "Bash" => wire::codex_ask::Body::Command(wire::CommandApproval {
                    command: field("command"),
                    cwd: sim.entry.cwd.clone(),
                    reason: p.reason.clone(),
                    allow_prefix: Vec::new(),
                    network_hosts: Vec::new(),
                }),
                "Edit" | "Write" => {
                    let path = field("file_path");
                    let new = if p.tool == "Write" {
                        field("content")
                    } else {
                        field("new_string")
                    };
                    wire::codex_ask::Body::FileChange(wire::FileChangeApproval {
                        reason: p.reason.clone(),
                        grant_root: String::new(),
                        changes: vec![wire::FileChange {
                            patch: body::unified_at(
                                &path,
                                &field("old_string"),
                                &new,
                                p.input.get("line").and_then(Value::as_u64).unwrap_or(1),
                            ),
                            path,
                            kind: if p.tool == "Write" {
                                wire::FileChangeKind::Add
                            } else {
                                wire::FileChangeKind::Update
                            } as i32,
                            move_to: String::new(),
                        }],
                    })
                }
                name => wire::codex_ask::Body::McpTool(wire::McpToolApproval {
                    server: String::new(),
                    tool: name.to_owned(),
                    arguments_json: p.input.to_string().into_bytes(),
                }),
            });
            pending.then = then(p.then);
        }
        AskSpec::Question(q) => {
            let asked = questions_of(&q.questions);
            if codex {
                let item = AskItem {
                    ask: Some(wire::ask_item::Ask::Question(asked.clone())),
                    closed: None,
                };
                sim.commit(&item_key, &Body::Ask(item.clone()), "", at, &[]);
                pending.ask_item = Some(item);
            } else {
                let input = json!({ "questions": q.questions.iter().map(|q| json!({
                    "header": q.header,
                    "question": q.question,
                    "multiSelect": q.multi,
                    "options": q.options.iter().map(|o| match o {
                        OptionSpec::Label(label) => json!({ "label": label }),
                        OptionSpec::Full { label, description, preview, .. } =>
                            json!({ "label": label, "description": description, "preview": preview }),
                    }).collect::<Vec<_>>(),
                })).collect::<Vec<_>>() });
                let call = tool("AskUserQuestion", input, ToolState::Pending);
                pending.tool = Some(call.clone());
                sim.commit(&item_key, &Body::Tool(call), "", at, &[]);
            }
            pending.questions = q.questions.clone();
            claude_body = Some(wire::ask::Body::Question(asked.clone()));
            codex_body = Some(wire::codex_ask::Body::Question(asked));
            pending.then = then(q.then);
        }
        AskSpec::Plan(p) => {
            let call = tool(
                "ExitPlanMode",
                json!({ "plan": p.plan }),
                ToolState::Pending,
            );
            if codex {
                // Codex has no plan approval; it asks the same thing as a
                // question.
                let item = QuestionItem {
                    header: "Plan".into(),
                    question: format!("{}\n\nGo ahead with this plan?", p.plan),
                    multi: false,
                    other: true,
                    secret: false,
                    options: vec![
                        OptionSpec::Label("Go ahead".into()),
                        OptionSpec::Label("Revise it".into()),
                    ],
                };
                let asked = questions_of(std::slice::from_ref(&item));
                let ask_item = AskItem {
                    ask: Some(wire::ask_item::Ask::Question(asked.clone())),
                    closed: None,
                };
                sim.commit(&item_key, &Body::Ask(ask_item.clone()), "", at, &[]);
                pending.ask_item = Some(ask_item);
                pending.questions = vec![item];
                codex_body = Some(wire::codex_ask::Body::Question(asked));
            } else {
                pending.tool = Some(call.clone());
                sim.commit(&item_key, &Body::Tool(call), "", at, &[]);
                claude_body = Some(wire::ask::Body::Plan(wire::PlanAsk {
                    plan: p.plan.clone(),
                    offers_auto_accept: true,
                }));
            }
            pending.then = then(p.then);
        }
        AskSpec::Form(f) => {
            let form = wire::FormAsk {
                server: f.server.clone(),
                message: f.message.clone(),
                schema_json: f.schema.to_string().into_bytes(),
            };
            let item = AskItem {
                ask: Some(wire::ask_item::Ask::Form(form.clone())),
                closed: None,
            };
            sim.commit(&item_key, &Body::Ask(item.clone()), "", at, &[]);
            pending.ask_item = Some(item);
            claude_body = Some(wire::ask::Body::Form(form.clone()));
            codex_body = Some(wire::codex_ask::Body::McpForm(form));
            pending.then = then(f.then);
        }
        AskSpec::Link(l) => {
            let link = wire::LinkAsk {
                server: l.server.clone(),
                message: l.message.clone(),
                url: l.url.clone(),
            };
            let item = AskItem {
                ask: Some(wire::ask_item::Ask::Link(link.clone())),
                closed: None,
            };
            sim.commit(&item_key, &Body::Ask(item.clone()), "", at, &[]);
            pending.ask_item = Some(item);
            claude_body = Some(wire::ask::Body::Link(link.clone()));
            codex_body = Some(wire::codex_ask::Body::McpLink(link));
            pending.then = then(l.then);
        }
        AskSpec::Access(a) => {
            let grant = wire::AccessGrant {
                reason: a.reason.clone(),
                read: a.read.clone(),
                write: a.write.clone(),
                network: a.network,
                network_hosts: a.hosts.clone(),
            };
            let item = AskItem {
                ask: Some(wire::ask_item::Ask::Access(grant.clone())),
                closed: None,
            };
            sim.commit(&item_key, &Body::Ask(item.clone()), "", at, &[]);
            pending.ask_item = Some(item);
            codex_body = Some(wire::codex_ask::Body::Access(grant));
            pending.then = then(a.then);
        }
        AskSpec::Unanswerable(u) => {
            let item = AskItem {
                ask: Some(wire::ask_item::Ask::Unanswerable(wire::UnanswerableAsk {
                    reason: u.reason.clone(),
                })),
                closed: None,
            };
            sim.commit(&item_key, &Body::Ask(item.clone()), "", at, &[]);
            pending.ask_item = Some(item);
            claude_body = Some(wire::ask::Body::Unanswerable(wire::UnanswerableAsk {
                reason: u.reason,
            }));
            pending.then = rest;
        }
    }
    if codex {
        if let Some(body) = codex_body {
            let decisions = match &body {
                wire::codex_ask::Body::Command(_)
                | wire::codex_ask::Body::FileChange(_)
                | wire::codex_ask::Body::McpTool(_) => vec![
                    wire::Decision::Approve as i32,
                    wire::Decision::ApproveSession as i32,
                    wire::Decision::Deny as i32,
                    wire::Decision::Abort as i32,
                ],
                _ => Vec::new(),
            };
            sim.facts.codex_asks.push(wire::CodexAsk {
                key: key.clone(),
                item_key: item_key.clone(),
                body: Some(body),
                decisions,
            });
        }
    } else if let Some(body) = claude_body {
        sim.facts.claude_asks.push(wire::Ask {
            key: key.clone(),
            item_key: item_key.clone(),
            body: Some(body),
            opened_at_ms: at,
        });
    }
    sim.asks.push(pending);
}

/// The client the terminal talks to: the world above behind the real seam.
#[derive(Clone)]
pub struct LabClient(pub Arc<World>);

#[async_trait]
impl Client for LabClient {
    async fn subscribe_inventory(&self) -> Result<EventStream<InventoryEvent>, RpcError> {
        let mut inner = self.0.inner.lock().unwrap();
        let (tx, stream) = stream();
        let event = |of| Ok(InventoryEvent { of: Some(of) });
        for host in &inner.hosts {
            let _ = tx.send(event(inventory_event::Of::Host(host.clone())));
        }
        for sim in &inner.agents {
            let _ = tx.send(event(inventory_event::Of::Agent(sim.entry.clone())));
        }
        let _ = tx.send(event(inventory_event::Of::CaughtUp(wire::CaughtUp {
            revision: 1,
        })));
        inner.inventory.push(tx);
        Ok(stream)
    }

    async fn resolve_agent(&self, request: ResolveAgentRequest) -> Result<Agent, RpcError> {
        let inner = self.0.inner.lock().unwrap();
        inner
            .agents
            .iter()
            .find(|sim| sim.entry.name.as_deref() == Some(request.name.as_str()))
            .map(|sim| sim.entry.clone())
            .ok_or_else(|| refused(ErrorCode::NotFound, "no agent by that name"))
    }

    async fn subscribe(
        &self,
        request: SubscribeRequest,
    ) -> Result<EventStream<SessionEvent>, RpcError> {
        let mut inner = self.0.inner.lock().unwrap();
        let online = {
            let host = inner
                .agents
                .iter()
                .find(|sim| sim.id() == request.agent_id)
                .map(|sim| sim.entry.host_id.clone());
            host.map(|host| inner.host_online(&host))
        };
        let Some(online) = online else {
            return Err(refused(ErrorCode::NotFound, "no such agent"));
        };
        let sim = inner.sim(&request.agent_id).expect("found above");
        let (tx, stream) = stream();
        let event = |of| Ok(SessionEvent { of: Some(of) });
        let _ = tx.send(event(session_event::Of::Snapshot(sim.snapshot())));
        let items: Vec<&Item> = match request.from {
            Some(wire::subscribe_request::From::After(after)) => sim
                .items
                .iter()
                .filter(|item| item.revision > after.revision)
                .collect(),
            Some(wire::subscribe_request::From::Tail(tail)) => {
                let skip = sim.items.len().saturating_sub(tail as usize);
                sim.items.iter().skip(skip).collect()
            }
            None => sim.items.iter().collect(),
        };
        for item in items {
            let _ = tx.send(event(session_event::Of::Item(item.clone())));
        }
        let marker = if online {
            session_event::Of::CaughtUp(wire::CaughtUp {
                revision: sim.revision,
            })
        } else {
            session_event::Of::Detached(wire::Detached {})
        };
        let _ = tx.send(event(marker));
        sim.subs.push(tx);
        Ok(stream)
    }

    async fn fetch(&self, request: FetchRequest) -> Result<FetchResponse, RpcError> {
        let mut inner = self.0.inner.lock().unwrap();
        let sim = inner
            .sim(&request.agent_id)
            .ok_or_else(|| refused(ErrorCode::NotFound, "no such agent"))?;
        let before = request.before_order.unwrap_or(u64::MAX);
        let older: Vec<Item> = sim
            .items
            .iter()
            .filter(|item| item.order < before)
            .cloned()
            .collect();
        let skip = older.len().saturating_sub(request.limit.max(1) as usize);
        let items: Vec<Item> = older.into_iter().skip(skip).collect();
        let exhausted = items.first().is_none_or(|item| item.order <= 1);
        Ok(FetchResponse { items, exhausted })
    }

    async fn get(&self, request: GetRequest) -> Result<Item, RpcError> {
        let mut inner = self.0.inner.lock().unwrap();
        let sim = inner
            .sim(&request.agent_id)
            .ok_or_else(|| refused(ErrorCode::NotFound, "no such agent"))?;
        sim.keys
            .get(&request.key)
            .map(|&i| sim.items[i].clone())
            .ok_or_else(|| refused(ErrorCode::NotFound, "no such item"))
    }

    async fn send_input(&self, request: SendInputRequest) -> Result<SendInputResponse, RpcError> {
        let Some(input) = request.input else {
            return Err(refused(ErrorCode::InvalidArgument, "no input"));
        };
        Ok(self.0.input(&request.agent_id, input))
    }

    async fn create_agent(&self, request: CreateAgentRequest) -> Result<Agent, RpcError> {
        let (id, name, kind) = {
            let mut inner = self.0.inner.lock().unwrap();
            inner.created += 1;
            let id = if request.agent_id.is_empty() {
                format!("new-{}", inner.created)
            } else {
                String::from_utf8(request.agent_id.clone())
                    .unwrap_or_else(|_| format!("new-{}", inner.created))
            };
            let prompt = request.initial_prompt.as_ref().and_then(prompt_text);
            let name = request.name.clone().or_else(|| {
                prompt.map(|p| p.split_whitespace().take(4).collect::<Vec<_>>().join(" "))
            });
            let kind = match request.kind() {
                Kind::ClaudePty => KindSpec::Claude,
                Kind::Codex => KindSpec::Codex,
                _ => KindSpec::ClaudeSdk,
            };
            (id, name, kind)
        };
        let host = request
            .host_id
            .clone()
            .map(|h| String::from_utf8_lossy(&h).into_owned());
        let spec = AgentSpec {
            id: id.clone(),
            name,
            kind,
            host,
            cwd: (!request.cwd.is_empty()).then(|| request.cwd.clone()),
            parent: request
                .parent
                .as_ref()
                .map(|p| String::from_utf8_lossy(&p.agent_id).into_owned()),
            phase: Some(PhaseSpec::Starting),
            exited: None,
            working_on: None,
            ago: Some(crate::scenario::Dur(0)),
            model: None,
            effort: None,
            mode: None,
            context: None,
            usage: None,
            tasks: Vec::new(),
            sign_in: None,
            background: None,
            failed_servers: Vec::new(),
            queue: Vec::new(),
            transcript: Vec::new(),
            diff: None,
            reply: None,
            implement: None,
        };
        let agent = self.0.add_agent(spec, now_ms());
        let world = self.0.clone();
        let prompt = request.initial_prompt.as_ref().and_then(prompt_text);
        let agent_id = agent.agent_id.clone();
        let start = async move {
            if !world.instant {
                tokio::time::sleep(Duration::from_millis(900)).await;
            }
            {
                let mut inner = world.inner.lock().unwrap();
                if let Some(index) = inner.index(&agent_id) {
                    inner.agents[index].forced = None;
                    let mut at = now_ms();
                    play_now(
                        &mut inner,
                        index,
                        vec![Entry::Boundary(BoundarySpec::Started)],
                        &mut at,
                    );
                    inner.touch(index);
                }
            }
            if let Some(text) = prompt {
                let id = format!("create-{}", String::from_utf8_lossy(&agent_id));
                world.prompt(&agent_id, &text, id.as_bytes());
            }
        };
        if self.0.instant {
            start.await;
        } else {
            tokio::spawn(start);
        }
        Ok(agent)
    }

    async fn rename_agent(&self, request: RenameAgentRequest) -> Result<Agent, RpcError> {
        let mut inner = self.0.inner.lock().unwrap();
        let index = inner
            .index(&request.agent_id)
            .ok_or_else(|| refused(ErrorCode::NotFound, "no such agent"))?;
        inner.agents[index].entry.name = Some(request.name);
        inner.touch(index);
        Ok(inner.agents[index].entry.clone())
    }

    async fn stop_agent(&self, request: StopAgentRequest) -> Result<(), RpcError> {
        self.0.interrupt(&request.agent_id);
        let mut inner = self.0.inner.lock().unwrap();
        let index = inner
            .index(&request.agent_id)
            .ok_or_else(|| refused(ErrorCode::NotFound, "no such agent"))?;
        let mut at = now_ms();
        play_now(
            &mut inner,
            index,
            vec![Entry::Exit("stopped".into())],
            &mut at,
        );
        inner.touch(index);
        Ok(())
    }

    async fn resume_agent(&self, request: ResumeAgentRequest) -> Result<Agent, RpcError> {
        let agent = {
            let mut inner = self.0.inner.lock().unwrap();
            let index = inner
                .index(&request.agent_id)
                .ok_or_else(|| refused(ErrorCode::NotFound, "no such agent"))?;
            let sim = &mut inner.agents[index];
            sim.entry.lifecycle = Lifecycle::Live as i32;
            sim.entry.exit_cause = None;
            sim.entry.incarnation += 1;
            sim.forced = None;
            let mut at = now_ms();
            play_now(
                &mut inner,
                index,
                vec![Entry::Boundary(BoundarySpec::Resumed)],
                &mut at,
            );
            inner.touch(index);
            inner.agents[index].entry.clone()
        };
        if let Some(input) = request.initial_prompt
            && let Some(text) = prompt_text(&input)
        {
            self.0.prompt(&request.agent_id, &text, &input.input_id);
        }
        Ok(agent)
    }

    async fn delete_agent(
        &self,
        request: DeleteAgentRequest,
    ) -> Result<DeleteAgentResponse, RpcError> {
        let mut removed = self.0.remove(&request.agent_id);
        if removed.is_empty() {
            return Err(refused(ErrorCode::NotFound, "no such agent"));
        }
        removed.remove(0);
        Ok(DeleteAgentResponse {
            removed_children: removed,
            unreachable_children: Vec::new(),
        })
    }

    async fn send_message(&self, _: Envelope) -> Result<SendMessageResponse, RpcError> {
        Err(refused(
            ErrorCode::Unimplemented,
            "the lab does not route messages",
        ))
    }

    async fn put_blob(&self, request: PutBlobRequest) -> Result<BlobRef, RpcError> {
        let mut inner = self.0.inner.lock().unwrap();
        Ok(inner.put_blob(&request.name, &request.mime, request.bytes))
    }

    async fn get_blob(&self, request: GetBlobRequest) -> Result<GetBlobResponse, RpcError> {
        let inner = self.0.inner.lock().unwrap();
        let (blob, bytes) = inner
            .blobs
            .get(&request.hash)
            .cloned()
            .ok_or_else(|| refused(ErrorCode::NotFound, "no such blob"))?;
        Ok(GetBlobResponse {
            blob: Some(blob),
            bytes,
        })
    }

    async fn diff(&self, request: DiffRequest) -> Result<Diff, RpcError> {
        let mut inner = self.0.inner.lock().unwrap();
        let patch = inner
            .sim(&request.agent_id)
            .ok_or_else(|| refused(ErrorCode::NotFound, "no such agent"))?
            .spec
            .diff
            .clone()
            .unwrap_or_default();
        let blob = inner.put_blob("working-tree.patch", "text/x-diff", patch.into_bytes());
        Ok(Diff {
            patch: Some(blob),
            base: request.base,
            head: "0123abcd".into(),
            merge_base: None,
        })
    }

    async fn list_repositories(
        &self,
        _: ListRepositoriesRequest,
    ) -> Result<ListRepositoriesResponse, RpcError> {
        let inner = self.0.inner.lock().unwrap();
        let entry = |path: &String| ProjectEntry {
            path: path.clone(),
            name: path.rsplit('/').next().unwrap_or(path).to_owned(),
            last_used_unix_ms: None,
        };
        Ok(ListRepositoriesResponse {
            recent: inner
                .scenario
                .repositories
                .iter()
                .take(3)
                .map(entry)
                .collect(),
            repositories: inner.scenario.repositories.iter().map(entry).collect(),
            roots: vec!["~/source".into()],
        })
    }

    async fn dump(&self, _: DumpRequest) -> Result<DumpResponse, RpcError> {
        Err(refused(
            ErrorCode::Unimplemented,
            "the lab has nothing to dump",
        ))
    }
}

fn prompt_text(input: &Input) -> Option<String> {
    use wire::{claude_pty_input as pty, claude_sdk_input as sdk, codex_input as cx, input};
    match input.of.as_ref()? {
        input::Of::ClaudePty(i) => match i.of.as_ref()? {
            pty::Of::Prompt(p) => Some(p.text.clone()),
            _ => None,
        },
        input::Of::ClaudeSdk(i) => match i.of.as_ref()? {
            sdk::Of::Prompt(p) => Some(p.text.clone()),
            _ => None,
        },
        input::Of::Codex(i) => match i.of.as_ref()? {
            cx::Of::Prompt(p) => Some(p.text.clone()),
            _ => None,
        },
        _ => None,
    }
}

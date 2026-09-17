//! The runtime shell: owns an explicit RPC client, executes Effects on tokio tasks,
//! and funnels every stimulus into one ordered Msg stream folded on the
//! caller's thread.
//!
//! The shell's edges are actor-shaped tasks, but they make no semantic
//! decisions: anything that affects which Msgs or Effects exist enters as a
//! Msg and is decided in the pure reducer. Shell-private state manages
//! resources only (sockets, reconnect backoff, buffers).

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use artifacts::{ArtifactMeta, Cache, FetchError, StoreError, SystemClock};
use chrono::{DateTime, Utc};
use client::{Client, ClientError, FrontDoorClient, installation_rpc as rpc};
use futures_util::{FutureExt, Stream, StreamExt};
use model::{
    AgentId, AgentIdentifier, ArtifactId, ArtifactKind, ArtifactRef, ClaudePtyIntent,
    CreateAgentRequest, HostId, ProfileId, ProtocolError, ReplayOutcome, ReplayQuery,
    SendInputRequest, SessionArgs, SessionCloseReason, SessionInput, SessionOutput,
    SubscribeSessionEvent, SubscribeSessionRequest,
};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;
use ui_state::codex::CodexInput;
use ui_state::{
    ChatCommand, ChatStreamMsg, Command, DisconnectReason, DumpReason, Effect, InputPayload, Model,
    Msg, NOT_CONNECTED_ERROR, OpError, OpId, OpOutcome, ProfileGeneration, ReplayFactsDto,
    ServerMsg, StoreMsg, StoreStreamQuery, StreamCloseReason, StreamEntry, StreamMsg,
    StructuredProtocol, update,
};
use uuid::Uuid;

use crate::recorder::{DEFAULT_RECORDER_CAPACITY, Recorder};
use crate::report::{
    FrameCapture, LOG_TAIL_BYTES, ReplayVerdict, ReportDraft, ReportKind, ReportParts,
    ReportWriter, TraceKind, log_tail,
};
use crate::store_worker::StoreWorker;

/// Reducer build identity, stamped into reports.
pub const BUILD: &str = concat!("ui-runtime/", env!("CARGO_PKG_VERSION"));

/// One ordered Msg stream; producers wait when it is full (lossless).
const MSG_CHANNEL_CAPACITY: usize = 1024;

/// Msgs folded per `next()` wakeup before control returns to the caller, so
/// a flooding stream batches to a frame budget and never starves input.
const DRAIN_BUDGET: usize = 256;

const RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_millis(250);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(4);
const SUBSCRIPTION_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum structured entries coalesced into one `Msg::Stream(Batch)`.
///
/// The recorded `Msg` is the batch, so replay is independent of arrival
/// timing. Performance workloads use the same ceiling when reproducing the
/// delivery cadence seen by the interactive runtime.
pub const MAX_STREAM_BATCH: usize = 256;

/// Why a connection attempt failed.
#[derive(Clone, Debug)]
pub struct ConnectFailure {
    pub message: String,
    pub auth_required: bool,
    pub subscription_required: bool,
}

/// Future returned by a [`Connector`].
pub type ConnectFuture = Pin<Box<dyn Future<Output = Result<Client, ConnectFailure>> + Send>>;

/// How the shell (re)establishes the daemon connection. Provided by the
/// embedding client (the CLI knows how to spawn the daemon); called again
/// after every disconnect.
pub type Connector = Box<dyn FnMut() -> ConnectFuture + Send>;

/// Reads the daemon's durable subscription-required state.
pub type SubscriptionStatusProvider = Arc<dyn Fn() -> bool + Send + Sync>;

/// Debug-only frame and trace data supplied by an embedding UI when available.
#[derive(Clone, Debug, Default)]
pub struct ReportExtras {
    pub frame: Option<FrameCapture>,
    pub trace: Option<Vec<u8>>,
    /// Which recorder produced the trace. An embedding that draws a native
    /// view says so, so the bundle's reader knows the trace replays on that
    /// platform rather than in the terminal chrome.
    pub trace_kind: TraceKind,
    pub viewport: Option<(u16, u16)>,
}

pub type ReportExtrasProvider = Arc<dyn Fn() -> ReportExtras + Send + Sync>;

/// See [`RuntimeOptions::msg_tap`].
pub type MsgTap = Box<dyn FnMut(&Msg) + Send>;

/// Opens one verified local artifact with the platform viewer.
///
/// The metadata travels with the content-addressed cache path because the path
/// itself deliberately has no extension. Platform launchers may need the kind
/// to choose a viewer that can identify those bytes.
pub type AttachmentOpener = Arc<dyn Fn(&ArtifactMeta, &Path) -> io::Result<()> + Send + Sync>;

/// Future returned by an attachment transport operation.
pub type AttachmentClientFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ClientError>> + Send + 'a>>;

/// Narrow transport boundary for the compound attachment send.
pub trait AttachmentClient: Send + Sync {
    fn put_artifact<'a>(
        &'a self,
        agent: AgentIdentifier,
        kind: ArtifactKind,
        name: &'a str,
        mime: &'a str,
        bytes: Vec<u8>,
    ) -> AttachmentClientFuture<'a, ArtifactRef>;

    fn send_input(&self, request: SendInputRequest) -> AttachmentClientFuture<'_, ()>;
}

impl AttachmentClient for Client {
    fn put_artifact<'a>(
        &'a self,
        agent: AgentIdentifier,
        kind: ArtifactKind,
        name: &'a str,
        mime: &'a str,
        bytes: Vec<u8>,
    ) -> AttachmentClientFuture<'a, ArtifactRef> {
        Box::pin(Client::put_artifact(self, agent, kind, name, mime, bytes))
    }

    fn send_input(&self, request: SendInputRequest) -> AttachmentClientFuture<'_, ()> {
        Box::pin(Client::send_input(self, request))
    }
}

const DEFAULT_ARTIFACT_CACHE_BOUND: u64 = 256 * 1024 * 1024;

/// One profile as the switcher lists it. The reducer never sees this: a
/// profile the user has not selected is not part of any Model.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ProfileEntry {
    pub id: ProfileId,
    pub label: String,
    pub email: Option<String>,
    pub status: String,
    pub socket: PathBuf,
}

/// The installation's profile listing, read from the front door.
///
/// This is a second connection, deliberately: the front door administers the
/// installation and knows nothing about the selected profile, while a
/// profile's client API knows nothing about its neighbours.
pub struct ProfileDirectory {
    front: FrontDoorClient,
}

impl ProfileDirectory {
    /// Connect to the installation's well-known administration socket.
    pub async fn connect(socket: &Path) -> Result<Self, client::ConnectError> {
        Ok(Self::new(FrontDoorClient::connect_socket(socket).await?))
    }

    pub fn new(front: FrontDoorClient) -> Self {
        Self { front }
    }

    /// Every profile in the installation, in the order the front door reports
    /// them. Profiles that failed to start are listed with the reason, so the
    /// switcher can show a profile that cannot currently be selected rather
    /// than silently omitting an account the user has.
    pub async fn list(&self) -> Result<Vec<ProfileEntry>, ClientError> {
        let response = self
            .front
            .profiles
            .clone()
            .list_profiles(rpc::ListProfilesRequest {})
            .await
            .map_err(|status| ClientError::Unexpected {
                method: "ListProfiles",
                message: status.message().to_string(),
            })?
            .into_inner()
            .profiles;
        response
            .iter()
            .map(|info| {
                Ok(ProfileEntry {
                    // A profile id the front door cannot name is not a profile
                    // anything may be switched to; say so rather than offering
                    // a row that would select nothing.
                    id: ProfileId(info.id.parse().map_err(|error| ClientError::Decode {
                        method: "ListProfiles",
                        message: format!("profile id {:?} is not a UUID: {error}", info.id),
                    })?),
                    label: profile_display_label(info, &response),
                    email: (!info.email.is_empty()).then(|| info.email.clone()),
                    status: profile_status_label(info),
                    socket: PathBuf::from(&info.socket_path),
                })
            })
            .collect()
    }
}

fn profile_display_label(info: &rpc::ProfileInfo, directory: &[rpc::ProfileInfo]) -> String {
    if directory
        .iter()
        .filter(|profile| profile.label == info.label)
        .count()
        > 1
    {
        let mut length = 8.min(info.id.len());
        while length < info.id.len()
            && directory
                .iter()
                .any(|other| other.id != info.id && other.id.starts_with(&info.id[..length]))
        {
            length += 1;
        }
        format!("{} ({})", info.label, &info.id[..length])
    } else {
        info.label.clone()
    }
}

fn profile_status_label(profile: &rpc::ProfileInfo) -> String {
    if !profile.startup_error.is_empty() {
        return format!("unavailable: {}", profile.startup_error);
    }
    if !profile.available {
        return "unavailable".into();
    }
    let intent = rpc::Intent::try_from(profile.intent)
        .map(|value| value.as_str_name())
        .unwrap_or("unknown");
    let observed = rpc::Observed::try_from(profile.observed)
        .map(|value| value.as_str_name())
        .unwrap_or("unknown");
    format!(
        "{} / {}",
        intent.trim_start_matches("INTENT_").to_ascii_lowercase(),
        observed
            .trim_start_matches("OBSERVED_")
            .to_ascii_lowercase()
    )
}

/// Which profile selection a shell task belongs to.
///
/// Switching profiles cannot recall work already in flight: an RPC awaiting a
/// reply, a subscription mid-event, a stream task decoding a batch. Each of
/// them carries the generation it was started under, and the fold drops
/// anything stamped with a retired one, so a result about the account the
/// user just left can never be folded into the account they moved to.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Generation(pub u64);

impl std::fmt::Display for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A shell edge belonging to one selection: what a connection task or a
/// stream task holds when it reports a result.
///
/// Tests use this to model work genuinely in flight for the profile being
/// left; integrations can use it when an operation must outlive a view.
#[derive(Clone)]
pub struct ShellEdge(MsgSink);

/// The Runtime an edge was taken from — and every runtime switched to from
/// it — has been dropped, so there is nothing left to report to.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("the runtime this shell edge belonged to is gone")]
pub struct RuntimeGone;

impl ShellEdge {
    /// Report a result as a task of this selection would.
    pub async fn report(&self, msg: Msg) -> Result<(), RuntimeGone> {
        self.0.send(msg).await.map_err(|()| RuntimeGone)
    }
}

/// What a dropped late result would have told the reducer.
///
/// Coarse on purpose: the question a switch raises is which edge of the shell
/// is still delivering for an account the user has left, not what any one
/// message said.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LateResult {
    Inventory,
    Session,
    Attachment,
    Command,
}

impl LateResult {
    fn of(msg: &Msg) -> Option<Self> {
        match msg {
            Msg::Server(_) => Some(Self::Inventory),
            Msg::Stream { .. } | Msg::ChatStream { .. } => Some(Self::Session),
            Msg::OpResult {
                outcome: OpOutcome::AttachmentOpened { .. } | OpOutcome::DiffFetched { .. },
                ..
            } => Some(Self::Attachment),
            Msg::OpResult { .. }
            | Msg::Command { .. }
            | Msg::Store(_)
            | Msg::FleetDelta(_)
            | Msg::Chat(_)
            | Msg::StoreStartup { .. } => Some(Self::Command),
            // Folded straight from the caller's thread, never through a task.
            Msg::Tick { .. } | Msg::UserAttached { .. } | Msg::UserDetached { .. } => None,
        }
    }
}

/// The one way a shell task reaches the fold. Stamping happens here so no
/// task can forget to do it.
#[derive(Clone)]
pub(crate) struct MsgSink {
    tx: mpsc::Sender<(Generation, Msg)>,
    generation: Generation,
}

impl MsgSink {
    /// Bounded lossless send: the producer waits, never drops. `Err` means
    /// the Runtime is gone.
    async fn send(&self, msg: Msg) -> Result<(), ()> {
        self.tx.send((self.generation, msg)).await.map_err(|_| ())
    }

    pub(crate) fn blocking_send(&self, msg: Msg) -> Result<(), ()> {
        self.tx
            .blocking_send((self.generation, msg))
            .map_err(|_| ())
    }
}

/// The host events an inventory source yields, in the client's own vocabulary.
pub type HostEventStream =
    Pin<Box<dyn Stream<Item = Result<model::HostEvent, ClientError>> + Send>>;
pub type HostEventStreamFuture<'a> =
    Pin<Box<dyn Future<Output = Result<HostEventStream, ClientError>> + Send + 'a>>;

/// Where an embedded profile's host inventory comes from when the runtime is
/// the profile's owner rather than one of its clients.
///
/// A client sees only the hosts its profile trusts. The owner also sees the
/// online cloud pairing candidates, so a phone can offer to pair with a
/// machine it has not paired with yet. The runtime never depends on how the
/// owner reaches its profile; the embedding application supplies this.
pub trait HostInventory: Send + Sync {
    fn subscribe_hosts(&self) -> HostEventStreamFuture<'_>;
}

pub struct RuntimeOptions {
    /// The daemon's own host id (read from the local device identity);
    /// enters the Model via `ServerMsg::Connected`.
    pub local_host_id: Option<HostId>,
    /// Shared client store for this profile. `None` is reserved for
    /// embedders and tests that have not installed persistence.
    pub store_path: Option<PathBuf>,
    /// Owner inventory for an embedded profile, including cloud pairing
    /// candidates. Without it, host presence comes from the trusted-only
    /// client subscription.
    pub host_inventory: Option<Arc<dyn HostInventory>>,
    /// Where report bundles land. `None` disables reporting.
    pub report_dir: Option<PathBuf>,
    /// Log file whose bounded tail is included when it exists.
    pub log_path: Option<PathBuf>,
    /// Source revision embedded in every report header.
    pub git_sha: &'static str,
    /// Optional UI-owned capture hook for automatic reports.
    pub report_extras: Option<ReportExtrasProvider>,
    pub recorder_capacity: usize,
    /// Provider polled while connected so marker transitions enter the reducer.
    pub subscription_status_provider: Option<SubscriptionStatusProvider>,
    /// Called with every folded Msg, in fold order, before [`Runtime::next`]
    /// returns. The diagnostic trace uses it: a recording that reconstructs
    /// the fold order from the outside would have to guess how a drain
    /// batched, and a wrong guess is a replay that diverges for no visible
    /// reason. `None` in a build that records nothing.
    pub msg_tap: Option<MsgTap>,
    /// Flat viewing-host artifact cache root. `None` disables attachment opens.
    pub artifact_cache: Option<PathBuf>,
    /// Maximum bytes retained by the viewing-host artifact cache.
    pub artifact_cache_bound: u64,
    /// Platform opener override. Embedders normally leave this at its default.
    pub attachment_opener: AttachmentOpener,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            local_host_id: None,
            store_path: None,
            host_inventory: None,
            report_dir: None,
            log_path: None,
            git_sha: "unknown",
            report_extras: None,
            recorder_capacity: DEFAULT_RECORDER_CAPACITY,
            subscription_status_provider: None,
            msg_tap: None,
            artifact_cache: None,
            artifact_cache_bound: DEFAULT_ARTIFACT_CACHE_BOUND,
            attachment_opener: Arc::new(open_with_platform_viewer),
        }
    }
}

/// One Runtime per client process, one Model per daemon connection.
/// Renderers access the Model in-process by borrow after [`Runtime::next`] /
/// [`Runtime::drain`].
pub struct Runtime {
    model: Model,
    /// Shared with the process panic hook ([`Runtime::install_panic_report`])
    /// so a panic can snapshot the ring after terminal restore. The fold is
    /// single-threaded — contention is nil; the mutex exists for the hook.
    recorder: Arc<StdMutex<Recorder>>,
    msg_sink: MsgSink,
    msg_rx: mpsc::Receiver<(Generation, Msg)>,
    client: Arc<StdMutex<Option<Client>>>,
    tasks: Vec<JoinHandle<()>>,
    /// Live per-agent stream tasks (shell resource bookkeeping only; the
    /// semantic stream state lives in the Model).
    streams: HashMap<AgentId, JoinHandle<()>>,
    store_streams: HashMap<AgentId, StoreStreamTask>,
    store_worker: Option<StoreWorker>,
    store_first_frame_seen: bool,
    startup_gate: Arc<StartupGate>,
    report_dir: Option<PathBuf>,
    log_path: Option<PathBuf>,
    git_sha: &'static str,
    report_extras: Option<ReportExtrasProvider>,
    msg_tap: Option<MsgTap>,
    artifact_cache: Option<Result<Arc<Cache>, String>>,
    attachment_opener: AttachmentOpener,
    queued_bytes: HashMap<ArtifactId, Arc<[u8]>>,
    /// Results from a selection this runtime has left, dropped before the
    /// reducer. Kept as a tally rather than a log: the set is small and
    /// fixed, so it cannot grow with a profile that keeps talking.
    discarded_late: std::collections::BTreeSet<LateResult>,
    discarded_late_count: usize,
    /// Violation kinds already reported this session: invariant logs and
    /// reports are throttled to once per kind so a persistent incoherence
    /// cannot fill the report directory.
    reported_violations: HashSet<&'static str>,
}

/// One open chat's retained store-backed state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatRetentionReport {
    pub agent: AgentId,
    pub visible_entries: usize,
    pub visible_entry_bytes: usize,
    pub canonical_entries: usize,
    pub canonical_entry_bytes: usize,
    pub pending_commits: usize,
    pub pending_mutations: usize,
    pub pending_mutation_bytes: usize,
}

/// In-process ownership accounting for the shipping client runtime.
///
/// Sizes are serialized payload bytes for model values and exact retained
/// string capacities for recorder messages. They intentionally exclude stack
/// frames and allocator overhead, making deltas stable enough to attribute
/// per-row growth without replacing the process-footprint soak.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeRetentionReport {
    pub chats: Vec<ChatRetentionReport>,
    pub store_queued_ops: usize,
    pub store_queued_bytes: usize,
    pub store_page_cache_bytes: usize,
    pub store_write_cache_bytes: usize,
    pub sqlite_bytes: usize,
    pub reducer_effect_count: usize,
    pub reducer_effect_bytes: usize,
    pub reducer_subscription_count: usize,
    pub reducer_subscription_bytes: usize,
    pub provider_state_count: usize,
    pub provider_state_bytes: usize,
    /// A named subset of `provider_state_bytes`.
    pub ask_count: usize,
    pub ask_bytes: usize,
    pub runtime_subscription_tasks: usize,
    pub runtime_subscription_bytes: usize,
    pub recorder_entries: usize,
    pub recorder_entry_bytes: usize,
    pub recorder_checkpoint_bytes: usize,
}

impl RuntimeRetentionReport {
    pub fn visible_entry_bytes(&self) -> usize {
        self.chats.iter().map(|chat| chat.visible_entry_bytes).sum()
    }

    pub fn canonical_entry_bytes(&self) -> usize {
        self.chats
            .iter()
            .map(|chat| chat.canonical_entry_bytes)
            .sum()
    }

    pub fn pending_mutation_bytes(&self) -> usize {
        self.chats
            .iter()
            .map(|chat| chat.pending_mutation_bytes)
            .sum()
    }

    pub fn recorder_bytes(&self) -> usize {
        self.recorder_entry_bytes
            .saturating_add(self.recorder_checkpoint_bytes)
    }

    pub fn accounted_bytes(&self) -> usize {
        self.visible_entry_bytes()
            .saturating_add(self.canonical_entry_bytes())
            .saturating_add(self.pending_mutation_bytes())
            .saturating_add(self.store_queued_bytes)
            .saturating_add(self.store_page_cache_bytes)
            .saturating_add(self.store_write_cache_bytes)
            .saturating_add(self.sqlite_bytes)
            .saturating_add(self.reducer_effect_bytes)
            .saturating_add(self.reducer_subscription_bytes)
            .saturating_add(self.runtime_subscription_bytes)
            .saturating_add(self.provider_state_bytes)
            .saturating_add(self.recorder_bytes())
    }
}

impl std::fmt::Display for RuntimeRetentionReport {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            output,
            "runtime retention: accounted={} bytes, chats={}, sqlite={} bytes",
            self.accounted_bytes(),
            self.chats.len(),
            self.sqlite_bytes
        )?;
        for chat in &self.chats {
            writeln!(
                output,
                "  chat {}: visible={} entries/{} bytes, canonical={} entries/{} bytes, pending={} commits/{} mutations/{} bytes",
                chat.agent,
                chat.visible_entries,
                chat.visible_entry_bytes,
                chat.canonical_entries,
                chat.canonical_entry_bytes,
                chat.pending_commits,
                chat.pending_mutations,
                chat.pending_mutation_bytes,
            )?;
        }
        writeln!(
            output,
            "  store: queued={} ops/{} bytes, page cache={} bytes, write cache={} bytes",
            self.store_queued_ops,
            self.store_queued_bytes,
            self.store_page_cache_bytes,
            self.store_write_cache_bytes,
        )?;
        writeln!(
            output,
            "  reducer: effects={} items/{} bytes, subscriptions={} items/{} bytes, provider state={} layers/{} bytes",
            self.reducer_effect_count,
            self.reducer_effect_bytes,
            self.reducer_subscription_count,
            self.reducer_subscription_bytes,
            self.provider_state_count,
            self.provider_state_bytes,
        )?;
        writeln!(
            output,
            "  asks: {} items/{} bytes (subset of provider state); runtime subscriptions={} tasks/{} bytes",
            self.ask_count,
            self.ask_bytes,
            self.runtime_subscription_tasks,
            self.runtime_subscription_bytes,
        )?;
        write!(
            output,
            "  recorder: {} entries/{} bytes, checkpoint={} bytes, total={} bytes",
            self.recorder_entries,
            self.recorder_entry_bytes,
            self.recorder_checkpoint_bytes,
            self.recorder_bytes(),
        )
    }
}

struct StoreStreamTask {
    task: JoinHandle<()>,
    paused: watch::Sender<bool>,
}

#[derive(Default)]
struct StartupGate {
    completed: std::sync::atomic::AtomicU8,
    notify: Notify,
}

impl StartupGate {
    const FLEET: u8 = 1;
    const VIEW: u8 = 2;

    fn finish_all(&self) {
        self.completed
            .store(Self::FLEET | Self::VIEW, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn observe(&self, msg: &Msg) {
        let bit = match msg {
            Msg::Store(StoreMsg::FleetLoaded { .. }) => Self::FLEET,
            Msg::Store(StoreMsg::ViewLoaded { .. }) => Self::VIEW,
            Msg::Store(StoreMsg::Failed {
                kind: ui_state::StoreOpKind::FleetLoad,
                ..
            }) => Self::FLEET,
            Msg::Store(StoreMsg::Failed {
                kind: ui_state::StoreOpKind::ViewGet,
                ..
            }) => Self::VIEW,
            Msg::Store(StoreMsg::Unavailable { .. }) => {
                self.finish_all();
                return;
            }
            _ => return,
        };
        let previous = self.completed.fetch_or(bit, Ordering::AcqRel);
        if previous | bit == Self::FLEET | Self::VIEW {
            self.notify.notify_waiters();
        }
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.completed.load(Ordering::Acquire) == Self::FLEET | Self::VIEW {
                return;
            }
            notified.await;
        }
    }
}

impl Runtime {
    /// Start the shell with a connector that dials (and re-dials) the
    /// daemon.
    pub fn start(connector: Connector, options: RuntimeOptions) -> Self {
        let (msg_tx, msg_rx) = mpsc::channel(MSG_CHANNEL_CAPACITY);
        Self::start_on_channel(connector, options, msg_tx, msg_rx, Generation::default())
    }

    /// Rebind the shell to another profile.
    ///
    /// The old runtime is consumed: its tasks are aborted and its generation
    /// retired. Abort is not instantaneous, so the two selections share one
    /// Msg channel and whatever the old profile still delivers arrives
    /// stamped with a generation the fold now refuses. The new runtime binds
    /// the selected profile's socket and starts from an empty Model, because
    /// nothing the previous account showed is true of this one.
    pub fn switch(mut self, entry: &ProfileEntry, options: RuntimeOptions) -> Runtime {
        self.switch_in_place(entry, options);
        self
    }

    /// Rebind the shell to another profile behind a borrow.
    ///
    /// Identical to [`Runtime::switch`], for a shell that holds its runtime
    /// by mutable reference for the whole of a session and has nowhere to
    /// put an owned one. The retired selection is dropped — and its tasks
    /// aborted — as soon as the new one has taken the Msg channel over.
    pub fn switch_in_place(&mut self, entry: &ProfileEntry, options: RuntimeOptions) {
        let socket = entry.socket.clone();
        let connector: Connector = Box::new(move || {
            let socket = socket.clone();
            Box::pin(async move {
                Client::connect_socket(&socket)
                    .await
                    .map_err(|error| ConnectFailure {
                        message: format!("{error}"),
                        auth_required: false,
                        subscription_required: false,
                    })
            })
        });
        self.switch_connector(connector, options);
    }

    /// Rebind the shell to another profile it already holds a client for.
    ///
    /// For an embedder that owns every profile in its own process: there is no
    /// socket to dial, and the installation hands out a client per profile.
    /// The selection changes exactly as it does over a socket — a retired
    /// generation, an empty Model, and every late result from the account
    /// being left refused.
    pub fn switch_in_place_with_client(&mut self, client: Client, options: RuntimeOptions) {
        let connector: Connector = Box::new(move || {
            let client = client.clone();
            Box::pin(async move { Ok(client) })
        });
        self.switch_connector(connector, options);
    }

    fn switch_connector(&mut self, connector: Connector, options: RuntimeOptions) {
        // A panic after the switch must report the profile the user is
        // actually looking at, so the panic-report slot follows the selection
        // — but only when it was this runtime's to begin with. A process that
        // never installed one keeps none.
        let owns_panic_report = self.owns_panic_report();
        let (closed_tx, closed_rx) = mpsc::channel(1);
        drop(closed_tx);
        let msg_rx = std::mem::replace(&mut self.msg_rx, closed_rx);
        let msg_tx = self.msg_sink.tx.clone();
        let generation = Generation(self.msg_sink.generation.0 + 1);
        // The retired selection stops talking first, so the two profiles'
        // connections never overlap; whatever is already in flight arrives
        // stamped with a generation the fold refuses.
        for task in self.tasks.drain(..) {
            task.abort();
        }
        for (_, task) in self.streams.drain() {
            task.abort();
        }
        for (_, stream) in self.store_streams.drain() {
            stream.task.abort();
        }

        let next = Self::start_on_channel(connector, options, msg_tx, msg_rx, generation);
        // Dropping the retired runtime releases its client and caches.
        drop(std::mem::replace(self, next));
        // The retired recorder and report directory are gone; leaving them
        // registered would file the next panic against the profile the shell
        // has left. A new selection with nowhere to write clears the slot
        // rather than keeping the stale one.
        if owns_panic_report {
            *lock_panic_report() = self.panic_report_context();
        }
    }

    /// The generation this runtime folds. Results stamped with any other are
    /// dropped before the reducer.
    pub fn generation(&self) -> Generation {
        self.msg_sink.generation
    }

    /// This runtime's shell edge, as its own tasks hold it.
    pub fn shell_edge(&self) -> ShellEdge {
        ShellEdge(self.msg_sink.clone())
    }

    /// How many results for an earlier selection this runtime has dropped.
    pub fn discarded_late_results(&self) -> usize {
        self.discarded_late_count
    }

    /// Which shell edges those dropped results came from.
    pub fn discarded_late_kinds(&self) -> Vec<LateResult> {
        self.discarded_late.iter().copied().collect()
    }

    fn discard_late(&mut self, msg: &Msg) {
        self.discarded_late_count += 1;
        if let Some(kind) = LateResult::of(msg) {
            self.discarded_late.insert(kind);
        }
        tracing::debug!(
            ?msg,
            "dropping a result from a profile the runtime has left"
        );
    }

    fn start_on_channel(
        connector: Connector,
        options: RuntimeOptions,
        msg_tx: mpsc::Sender<(Generation, Msg)>,
        msg_rx: mpsc::Receiver<(Generation, Msg)>,
        generation: Generation,
    ) -> Self {
        let model = Model::default();
        let recorder = Arc::new(StdMutex::new(Recorder::new(
            options.recorder_capacity,
            &model,
        )));
        let msg_sink = MsgSink {
            tx: msg_tx,
            generation,
        };
        let client = Arc::new(StdMutex::new(None));
        let artifact_cache = options.artifact_cache.map(|root| {
            Cache::open(root, options.artifact_cache_bound, Arc::new(SystemClock))
                .map(Arc::new)
                .map_err(|error| error.to_string())
        });

        let startup_gate = Arc::new(StartupGate::default());
        let profile = ProfileGeneration(generation.0);
        let store_worker = options
            .store_path
            .map(|path| StoreWorker::spawn(path, profile, options.local_host_id, msg_sink.clone()));
        if store_worker.is_none() {
            startup_gate.finish_all();
        }

        let subscription_status_provider = options.subscription_status_provider;
        let connection_gate = startup_gate.clone();
        let connection_task = tokio::spawn(connection_task(
            connector,
            msg_sink.clone(),
            client.clone(),
            options.local_host_id,
            subscription_status_provider.clone(),
            options.host_inventory,
            connection_gate,
        ));

        Self {
            model,
            recorder,
            msg_sink,
            msg_rx,
            client,
            tasks: vec![connection_task],
            streams: HashMap::new(),
            store_streams: HashMap::new(),
            store_worker,
            store_first_frame_seen: false,
            startup_gate,
            report_dir: options.report_dir,
            log_path: options.log_path,
            git_sha: options.git_sha,
            report_extras: options.report_extras,
            msg_tap: options.msg_tap,
            artifact_cache,
            attachment_opener: options.attachment_opener,
            queued_bytes: HashMap::new(),
            discarded_late: std::collections::BTreeSet::new(),
            discarded_late_count: 0,
            reported_violations: HashSet::new(),
        }
    }

    /// Start over an already-established client (tests, embedded servers).
    pub fn start_with_client(client: Client, options: RuntimeOptions) -> Self {
        let connector: Connector = Box::new(move || {
            let client = client.clone();
            Box::pin(async move { Ok(client) })
        });
        Self::start(connector, options)
    }

    /// Live attachment resources for a caller restoring a cancelled queue draft.
    /// Copy them into the local composer before its bounded outcome ages out.
    pub fn queued_attachment_bytes(&self, id: &ArtifactId) -> Option<Arc<[u8]>> {
        self.queued_bytes.get(id).cloned()
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Attribute bytes retained by each long-lived runtime component.
    pub fn retention_report(&self) -> RuntimeRetentionReport {
        let model = self.model.retention();
        let chats = self
            .model
            .chats()
            .map(|(agent, chat)| {
                let retained = chat.retention();
                ChatRetentionReport {
                    agent: *agent,
                    visible_entries: retained.visible_entries,
                    visible_entry_bytes: retained.visible_entry_bytes,
                    canonical_entries: retained.canonical_entries,
                    canonical_entry_bytes: retained.canonical_entry_bytes,
                    pending_commits: retained.pending_commits,
                    pending_mutations: retained.pending_mutations,
                    pending_mutation_bytes: retained.pending_mutation_bytes,
                }
            })
            .collect();
        let store = self
            .store_worker
            .as_ref()
            .map(StoreWorker::retention)
            .unwrap_or_default();
        let recorder = lock_recorder(&self.recorder).retention();
        RuntimeRetentionReport {
            chats,
            store_queued_ops: store.queued_ops,
            store_queued_bytes: store.queued_bytes,
            store_page_cache_bytes: store.page_cache_bytes,
            store_write_cache_bytes: store.write_cache_bytes,
            sqlite_bytes: self
                .store_worker
                .as_ref()
                .map_or(0, |_| sqlite_memory_used()),
            reducer_effect_count: model.effect_state_count,
            reducer_effect_bytes: model.effect_state_bytes,
            reducer_subscription_count: model.subscription_state_count,
            reducer_subscription_bytes: model.subscription_state_bytes,
            provider_state_count: model.provider_state_count,
            provider_state_bytes: model.provider_state_bytes,
            ask_count: model.ask_count,
            ask_bytes: model.ask_bytes,
            runtime_subscription_tasks: self.streams.len() + self.store_streams.len(),
            runtime_subscription_bytes: self
                .streams
                .capacity()
                .saturating_mul(std::mem::size_of::<(AgentId, JoinHandle<()>)>())
                .saturating_add(
                    self.store_streams
                        .capacity()
                        .saturating_mul(std::mem::size_of::<(AgentId, StoreStreamTask)>()),
                ),
            recorder_entries: recorder.entries,
            recorder_entry_bytes: recorder.entry_bytes,
            recorder_checkpoint_bytes: recorder.checkpoint_bytes,
        }
    }

    /// Dispatch a command; the outcome returns as state (a finished op).
    pub fn dispatch(&mut self, command: Command) -> OpId {
        let op = OpId(Uuid::new_v4());
        self.dispatch_with_id(op, command);
        op
    }

    /// Dispatch with an ID already allocated by a foreign-language caller.
    pub fn dispatch_with_id(&mut self, op: OpId, command: Command) {
        self.process(Msg::Tick { now: Utc::now() });
        self.process(Msg::Command { op, command });
    }

    /// Feed observed time for time-dependent display.
    pub fn observe_now(&mut self, now: DateTime<Utc>) {
        self.process(Msg::Tick { now });
    }

    /// Reify a user attach: the subscription policy widens to agents the
    /// user interacts with.
    pub fn note_attached(&mut self, agent: AgentId) {
        self.process(Msg::UserAttached { agent });
    }

    /// Whether this runtime has a store whose remembered fleet is not in the
    /// model yet. A screen that projects the fleet before then would draw an
    /// empty fleet over the rows it already showed from the store.
    pub fn remembered_fleet_pending(&self) -> bool {
        self.store_worker.is_some() && !self.model.remembered_fleet_settled()
    }

    /// Open one structured conversation through its persisted lifecycle.
    pub fn open_chat(&mut self, agent: AgentId) {
        if let Some(worker) = &self.store_worker {
            worker.record_chat_opened(agent);
        }
        self.process(Msg::Chat(ChatCommand::Open { agent }));
    }

    /// Extend the visible store-backed chat window toward older history.
    pub fn page_chat_older(&mut self, agent: AgentId) {
        self.process(Msg::Chat(ChatCommand::PageOlder {
            agent,
            n: ui_state::WINDOW_PAGE_ENTRIES,
        }));
    }

    /// Return an older-history window to the newest persisted rows.
    pub fn follow_chat_tip(&mut self, agent: AgentId) {
        self.process(Msg::Chat(ChatCommand::FollowTip { agent }));
    }

    /// Reify a user detach: a conversation nobody has open no longer widens
    /// the subscription policy, and the stream it asked for is let go.
    pub fn note_detached(&mut self, agent: AgentId) {
        self.process(Msg::UserDetached { agent });
    }

    /// Close a structured conversation after its bounded store flush.
    pub fn close_chat(&mut self, agent: AgentId) {
        let now = Utc::now();
        self.process(Msg::Chat(ChatCommand::Close { agent, now }));
        let tx = self.msg_sink.clone();
        tokio::spawn(async move {
            let delay = ui_state::FLUSH_DEADLINE
                .to_std()
                .unwrap_or_else(|_| Duration::from_secs(5));
            tokio::time::sleep(delay).await;
            let _ = tx
                .send(Msg::Chat(ChatCommand::FlushDeadline {
                    agent,
                    now: now + ui_state::FLUSH_DEADLINE,
                }))
                .await;
        });
    }

    /// Await the next Msg, then fold everything already pending (up to a
    /// frame budget). Returns false when the shell has shut down.
    pub async fn next(&mut self) -> bool {
        if !self.next_message().await {
            return false;
        }
        self.drain();
        // Interactive callers draw before they await the next input. The
        // first completed wait is therefore the store worker's safe signal
        // that launch-critical reads no longer share the first-frame path.
        if !self.store_first_frame_seen {
            self.store_first_frame_seen = true;
            if let Some(worker) = &self.store_worker {
                worker.after_first_frame();
            }
        }
        true
    }

    /// Fold one input so embedders can observe every resolved operation before
    /// bounded outcome retention evicts it. Callers own their batching cadence.
    pub async fn next_message(&mut self) -> bool {
        loop {
            let Some((generation, msg)) = self.msg_rx.recv().await else {
                return false;
            };
            if generation != self.msg_sink.generation {
                self.discard_late(&msg);
                continue;
            }
            self.process(msg);
            return true;
        }
    }

    /// Fold every immediately-available Msg (bounded by the frame budget);
    /// returns true if anything was folded.
    pub fn drain(&mut self) -> bool {
        let mut folded = false;
        // A retired generation spends budget without folding: the frame
        // stays bounded even while a departed profile is still delivering.
        for _ in 0..DRAIN_BUDGET {
            match self.msg_rx.try_recv() {
                Ok((generation, msg)) if generation != self.msg_sink.generation => {
                    self.discard_late(&msg);
                }
                Ok((_, msg)) => {
                    self.process(msg);
                    folded = true;
                }
                Err(_) => break,
            }
        }
        folded
    }

    /// Write an automatic diagnostic report. Local-only; never uploaded.
    pub fn report(&mut self, reason: DumpReason) -> io::Result<PathBuf> {
        let Some(dir) = self.report_dir.clone() else {
            return Err(io::Error::other("no report directory configured"));
        };
        let extras = self
            .report_extras
            .as_ref()
            .map(|provider| provider())
            .unwrap_or_default();
        let (log, log_absent_reason) = capture_log_tail(self.log_path.as_deref());
        let (kind, detail) = report_reason(reason);
        ReportWriter::new(dir, BUILD, self.git_sha).write(
            ReportDraft {
                kind,
                detail,
                note: String::new(),
                marks: Vec::new(),
                viewport: extras.viewport,
                replay: ReplayVerdict::Unchecked,
            },
            ReportParts {
                frame: extras.frame,
                trace: extras.trace,
                trace_kind: extras.trace_kind,
                msgs: Some(self.recorder_snapshot()),
                daemon: None,
                log,
                absent_reason: automatic_absent_reason().to_string(),
                log_absent_reason,
                daemon_absent_reason: None,
            },
        )
    }

    pub fn recorder_snapshot(&self) -> crate::RecorderSnapshot {
        lock_recorder(&self.recorder).snapshot()
    }

    /// Register this Runtime's recorder with the process-global panic-report
    /// slot read by [`write_panic_report`]. Call once after start; a Runtime
    /// without a report directory registers nothing.
    pub fn install_panic_report(&self) {
        let Some(context) = self.panic_report_context() else {
            return;
        };
        *lock_panic_report() = Some(context);
    }

    /// What the panic hook would need to report on this Runtime's behalf, or
    /// None for a Runtime with nowhere to write a report.
    fn panic_report_context(&self) -> Option<PanicReportContext> {
        Some(PanicReportContext {
            recorder: self.recorder.clone(),
            report_dir: self.report_dir.clone()?,
            log_path: self.log_path.clone(),
            git_sha: self.git_sha,
            report_extras: self.report_extras.clone(),
        })
    }

    /// True when the process-global panic-report slot is this Runtime's own.
    fn owns_panic_report(&self) -> bool {
        lock_panic_report()
            .as_ref()
            .is_some_and(|context| Arc::ptr_eq(&context.recorder, &self.recorder))
    }

    fn process(&mut self, msg: Msg) {
        if let Msg::Command {
            command:
                Command::Queue(
                    ui_state::QueueCommand::Hold { draft, .. }
                    | ui_state::QueueCommand::Replace { draft, .. },
                ),
            ..
        } = &msg
        {
            for attachment in &draft.attachments {
                if let Some(bytes) = &attachment.bytes {
                    self.queued_bytes
                        .insert(attachment.id.clone(), bytes.clone());
                }
            }
        }

        lock_recorder(&self.recorder).record(&msg);
        if let Some(tap) = self.msg_tap.as_mut() {
            tap(&msg);
        }
        let startup_result = matches!(
            &msg,
            Msg::Store(StoreMsg::FleetLoaded { .. })
                | Msg::Store(StoreMsg::ViewLoaded { .. })
                | Msg::Store(StoreMsg::Unavailable { .. })
                | Msg::Store(StoreMsg::Failed {
                    kind: ui_state::StoreOpKind::FleetLoad | ui_state::StoreOpKind::ViewGet,
                    ..
                })
        )
        .then(|| msg.clone());
        let paged_agent = match &msg {
            Msg::Store(StoreMsg::Paged { agent, .. }) => Some(*agent),
            _ => None,
        };
        // Shell-side resource bookkeeping keyed on an observed Msg (allowed:
        // the shell manages resources, never decides semantics): a stream
        // task always ends by sending `Closed`, so drop its finished
        // JoinHandle here instead of letting it linger until Drop. The
        // is_finished guard keeps a stale Closed — queued before a newer
        // OpenStream replaced the handle — from discarding the live task.
        if let Msg::Stream {
            agent,
            event: StreamMsg::Closed { .. },
        } = &msg
            && self
                .streams
                .get(agent)
                .is_some_and(|task| task.is_finished())
        {
            self.streams.remove(agent);
        }
        if let Msg::ChatStream {
            agent,
            event: ChatStreamMsg::Closed { .. },
            ..
        } = &msg
            && self
                .store_streams
                .get(agent)
                .is_some_and(|stream| stream.task.is_finished())
        {
            self.store_streams.remove(agent);
        }
        let effects = update(&mut self.model, msg);
        if let Some(agent) = paged_agent
            && let Some(chat) = self.model.chat(agent)
        {
            tracing::debug!(
                target: "amux::store",
                %agent,
                entries = chat.entries.len(),
                encoded_bytes = chat.encoded_window_bytes(),
                max_entries = ui_state::WINDOW_MAX_ENTRIES,
                max_bytes = ui_state::WINDOW_MAX_BYTES,
                "store page installed in bounded chat window"
            );
        }
        self.enforce_invariants();
        for effect in effects {
            self.run_effect(effect);
        }
        // Opening the network is gated on these messages having crossed the
        // recorder and reducer seam, not merely having been queued by the
        // store thread.
        if let Some(msg) = startup_result.as_ref() {
            self.startup_gate.observe(msg);
        }
        self.enforce_invariants();
        // Binary payloads are shell resources, absent from reducer state and reports.
        // Cancellation keeps them through the bounded returned-draft outcome so a
        // caller can resend metadata while restoring its own composer assets.
        if !self.queued_bytes.is_empty() {
            let mut retained = HashSet::new();
            for (_, queue) in self.model.queued_messages() {
                retained.extend(queue.draft.attachments.iter().map(|a| a.id.clone()));
            }
            for finished in self.model.finished_ops() {
                if let OpOutcome::QueueCancelled { draft } = &finished.outcome {
                    retained.extend(draft.attachments.iter().map(|a| a.id.clone()));
                }
            }
            self.queued_bytes.retain(|id, _| retained.contains(id));
        }
        // Shell companion invariant: every live stream task is known to the
        // Model (the inverse does not hold — a Closed stream keeps its Model
        // entry with no task behind it). Checked AFTER the effects loop
        // because a `CloseStream` decided by this very fold removes the
        // Model entry in `update` but only removes the task when the effect
        // executes — between the two, the task map is legitimately ahead.
        #[cfg(debug_assertions)]
        for agent in self.streams.keys() {
            debug_assert!(
                self.model.stream(*agent).is_some(),
                "shell stream task for agent {agent} has no Model stream entry"
            );
        }
    }

    /// Model coherence at the fold seam (`docs/UI.md`, Testing): distinct
    /// from input tripwires, which refuse impossible inputs at the receiving
    /// reducer arm — this checks the folded state itself, in every build.
    /// Every build writes once per violation kind, marks a sticky
    /// renderer warning, and keeps folding. `AMUX_INVARIANT_FATAL=1` is the
    /// sole opt-in to the fatal panic policy used by tests and CI.
    fn enforce_invariants(&mut self) {
        let violations = self.model.check_invariants();
        if violations.is_empty() {
            return;
        }
        self.model.note_invariant_violation();
        lock_recorder(&self.recorder).note_invariant_violation();
        if std::env::var("AMUX_INVARIANT_FATAL").as_deref() == Ok("1") {
            let details: Vec<String> = violations.iter().map(ToString::to_string).collect();
            panic!("model invariants violated: {}", details.join("; "));
        }
        for violation in violations {
            if self.reported_violations.insert(violation.kind()) {
                tracing::error!(%violation, "model invariant violated; writing report");
                if let Err(error) = self.report(DumpReason::Tripwire {
                    detail: format!("invariant: {violation}"),
                }) {
                    tracing::error!(%violation, %error, "failed to write invariant report");
                }
            }
        }
    }

    fn run_effect(&mut self, effect: Effect) {
        match effect {
            Effect::Rpc { op, command } => {
                let client = self.client.lock().expect("client mutex poisoned").clone();
                let tx = self.msg_sink.clone();
                tokio::spawn(async move {
                    let outcome = match client {
                        Some(client) => execute_rpc(&client, command).await,
                        None => OpOutcome::Error {
                            error: OpError::general(NOT_CONNECTED_ERROR),
                        },
                    };
                    let _ = tx.send(Msg::OpResult { op, outcome }).await;
                });
            }
            Effect::SendInput {
                op,
                agent,
                input_id,
                payload,
            } => {
                let client = self.client.lock().expect("client mutex poisoned").clone();
                let tx = self.msg_sink.clone();
                tokio::spawn(async move {
                    let outcome = match client {
                        Some(client) => execute_send_input(&client, agent, input_id, payload).await,
                        None => OpOutcome::Error {
                            error: OpError::general(NOT_CONNECTED_ERROR),
                        },
                    };
                    let _ = tx.send(Msg::OpResult { op, outcome }).await;
                });
            }
            Effect::PutThenSend {
                op,
                agent,
                mut puts,
                input,
                pin,
            } => {
                for attachment in &mut puts {
                    if attachment.bytes.is_none() {
                        attachment.bytes = self.queued_bytes.get(&attachment.id).cloned();
                    }
                }
                let client = self.client.lock().expect("client mutex poisoned").clone();
                let tx = self.msg_sink.clone();
                tokio::spawn(async move {
                    let outcome = match client {
                        Some(client) => {
                            execute_put_then_send(&client, op, agent, puts, input, pin).await
                        }
                        None => OpOutcome::Error {
                            error: OpError::general(NOT_CONNECTED_ERROR),
                        },
                    };
                    let _ = tx.send(Msg::OpResult { op, outcome }).await;
                });
            }
            Effect::PutAttachment {
                op,
                agent,
                attachment,
            } => {
                let client = self.client.lock().expect("client mutex poisoned").clone();
                let tx = self.msg_sink.clone();
                tokio::spawn(async move {
                    let outcome = match client {
                        Some(client) => execute_put(&client, agent, attachment).await,
                        None => OpOutcome::Error {
                            error: OpError::general(NOT_CONNECTED_ERROR),
                        },
                    };
                    let _ = tx.send(Msg::OpResult { op, outcome }).await;
                });
            }
            Effect::FetchDiff { op, agent, id } => {
                let client = self.client.lock().expect("client mutex poisoned").clone();
                let cache = clone_artifact_cache(&self.artifact_cache);
                let tx = self.msg_sink.clone();
                tokio::spawn(async move {
                    let outcome = match (client, cache) {
                        (Some(client), Ok(cache)) => {
                            match fetch_through_cache(&cache, &client, agent, &id).await {
                                Ok((_, bytes)) => match String::from_utf8(bytes) {
                                    Ok(patch) => OpOutcome::DiffFetched { id, patch },
                                    Err(error) => OpOutcome::Error {
                                        error: OpError::DiffUnavailable {
                                            message: format!("review patch is not UTF-8: {error}"),
                                        },
                                    },
                                },
                                Err(error) => OpOutcome::Error { error },
                            }
                        }
                        (None, _) => OpOutcome::Error {
                            error: OpError::general(NOT_CONNECTED_ERROR),
                        },
                        (_, Err(error)) => OpOutcome::Error { error },
                    };
                    let _ = tx.send(Msg::OpResult { op, outcome }).await;
                });
            }
            Effect::OpenExternally { op, agent, id } => {
                let client = self.client.lock().expect("client mutex poisoned").clone();
                let cache = clone_artifact_cache(&self.artifact_cache);
                let opener = self.attachment_opener.clone();
                let tx = self.msg_sink.clone();
                tokio::spawn(async move {
                    let outcome = match (client, cache) {
                        (Some(client), Ok(cache)) => {
                            match fetch_through_cache(&cache, &client, agent, &id).await {
                                Ok((meta, _)) => match cache.path_of(&id) {
                                    Ok(path) => match opener(&meta, &path) {
                                        Ok(()) => OpOutcome::AttachmentOpened { id },
                                        Err(error) => OpOutcome::Error {
                                            error: OpError::general(format!(
                                                "failed to open attachment: {error}"
                                            )),
                                        },
                                    },
                                    Err(error) => OpOutcome::Error {
                                        error: map_store_error(error, None),
                                    },
                                },
                                Err(error) => OpOutcome::Error { error },
                            }
                        }
                        (None, _) => OpOutcome::Error {
                            error: OpError::general(NOT_CONNECTED_ERROR),
                        },
                        (_, Err(error)) => OpOutcome::Error { error },
                    };
                    let _ = tx.send(Msg::OpResult { op, outcome }).await;
                });
            }
            Effect::Diff { op, agent, base } => {
                let client = self.client.lock().expect("client mutex poisoned").clone();
                let tx = self.msg_sink.clone();
                tokio::spawn(async move {
                    let outcome = match client {
                        Some(client) => match client.diff(AgentIdentifier::Id(agent), base).await {
                            Ok(response) => OpOutcome::DiffReady { response },
                            Err(error) => OpOutcome::Error {
                                error: map_client_error(&error, None, &[]),
                            },
                        },
                        None => OpOutcome::Error {
                            error: OpError::general(NOT_CONNECTED_ERROR),
                        },
                    };
                    let _ = tx.send(Msg::OpResult { op, outcome }).await;
                });
            }
            Effect::OpenStream {
                agent,
                protocol,
                tail,
            } => {
                let client = self.client.lock().expect("client mutex poisoned").clone();
                let tx = self.msg_sink.clone();
                if let Some(stale) = self.streams.insert(
                    agent,
                    tokio::spawn(stream_task(client, agent, protocol, tail, tx)),
                ) {
                    stale.abort();
                }
            }
            Effect::OpenStoreStream {
                agent,
                protocol,
                attempt,
                query,
                paused: initially_paused,
            } => {
                let client = self.client.clone();
                let tx = self.msg_sink.clone();
                let (paused, paused_rx) = watch::channel(initially_paused);
                let task = tokio::spawn(store_stream_task(
                    client, agent, protocol, attempt, query, paused_rx, tx,
                ));
                if let Some(stale) = self
                    .store_streams
                    .insert(agent, StoreStreamTask { task, paused })
                {
                    stale.task.abort();
                }
            }
            Effect::PauseStream(agent) => {
                if let Some(stream) = self.store_streams.get(&agent) {
                    let _ = stream.paused.send(true);
                }
            }
            Effect::ResumeStream(agent) => {
                if let Some(stream) = self.store_streams.get(&agent) {
                    let _ = stream.paused.send(false);
                }
            }
            Effect::Store(op) => {
                if let Some(worker) = &self.store_worker {
                    worker.execute(op);
                }
            }
            Effect::RetryStore { after_ms, op } => {
                if let (Some(worker), Effect::Store(op)) = (&self.store_worker, *op) {
                    let worker = worker.handle();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(after_ms)).await;
                        worker.execute(op);
                    });
                }
            }
            Effect::CloseStream { agent } => {
                if let Some(task) = self.streams.remove(&agent) {
                    task.abort();
                }
                if let Some(stream) = self.store_streams.remove(&agent) {
                    stream.task.abort();
                }
            }
            Effect::RequestDump { reason } => {
                if let Err(error) = self.report(reason.clone()) {
                    tracing::warn!(?reason, %error, "failed to write requested report");
                }
            }
        }
    }
}

fn sqlite_memory_used() -> usize {
    // SAFETY: sqlite3_memory_used takes no pointers and is safe to call while
    // SQLite is active when the library is threadsafe. Store qualification
    // refuses libraries without that property before the runtime opens them.
    usize::try_from(unsafe { rusqlite::ffi::sqlite3_memory_used() }).unwrap_or(0)
}

impl Drop for Runtime {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        for task in self.streams.values() {
            task.abort();
        }
        for stream in self.store_streams.values() {
            stream.task.abort();
        }
    }
}

#[derive(Clone)]
struct PanicReportContext {
    recorder: Arc<StdMutex<Recorder>>,
    report_dir: PathBuf,
    log_path: Option<PathBuf>,
    git_sha: &'static str,
    report_extras: Option<ReportExtrasProvider>,
}

/// The context registered for panic reports: written by
/// [`Runtime::install_panic_report`] and read inside the panic hook.
/// Replaceable rather than write-once, because switching profiles builds a
/// new runtime with a new recorder — a panic afterwards must report the
/// profile the user is actually looking at.
static PANIC_REPORT: StdMutex<Option<PanicReportContext>> = StdMutex::new(None);
static PANIC_REPORT_WRITING: AtomicBool = AtomicBool::new(false);

struct PanicReportGuard;

impl Drop for PanicReportGuard {
    fn drop(&mut self) {
        PANIC_REPORT_WRITING.store(false, Ordering::Release);
    }
}

/// Lock the recorder even when poisoned: a panic mid-record must not block
/// the panic hook from reporting. The ring holds pre-serialized lines, so the
/// worst a poisoned lock can cost is the newest entry.
/// Same poison tolerance as [`lock_recorder`], for the same reason: a panic
/// while installing a context must not stop the hook from reporting.
fn lock_panic_report() -> std::sync::MutexGuard<'static, Option<PanicReportContext>> {
    PANIC_REPORT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_recorder(recorder: &StdMutex<Recorder>) -> std::sync::MutexGuard<'_, Recorder> {
    recorder
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn report_reason(reason: DumpReason) -> (ReportKind, Option<String>) {
    match reason {
        DumpReason::Tripwire { detail } => (ReportKind::Tripwire, Some(detail)),
        DumpReason::ChannelOverflow { detail } => (ReportKind::ChannelOverflow, Some(detail)),
        DumpReason::Panic { detail } => (ReportKind::Panic, Some(detail)),
        DumpReason::UserRequested => (
            ReportKind::Bug,
            Some("legacy user-requested capture".to_string()),
        ),
    }
}

fn automatic_absent_reason() -> &'static str {
    if cfg!(debug_assertions) {
        "not captured by this automatic report"
    } else {
        "unavailable in release build"
    }
}

fn capture_log_tail(log_path: Option<&Path>) -> (Option<String>, Option<String>) {
    let Some(path) = log_path else {
        return (None, None);
    };
    match log_tail(path, LOG_TAIL_BYTES) {
        Ok(log) => (log, None),
        Err(error) => (
            None,
            Some(format!(
                "failed to read log tail from {}: {error}",
                path.display()
            )),
        ),
    }
}

/// Best-effort report from the process panic hook, called after terminal
/// restore. Returns quietly on every failure; the process is already dying.
pub fn write_panic_report(detail: &str) {
    if PANIC_REPORT_WRITING.swap(true, Ordering::AcqRel) {
        return;
    }
    let _guard = PanicReportGuard;
    let Some(context) = lock_panic_report().clone() else {
        return;
    };
    let extras = context
        .report_extras
        .as_ref()
        .and_then(|provider| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| provider())).ok()
        })
        .unwrap_or_default();
    let (log, log_absent_reason) = capture_log_tail(context.log_path.as_deref());
    let snapshot = lock_recorder(&context.recorder).snapshot();
    let _ = ReportWriter::new(context.report_dir.clone(), BUILD, context.git_sha).write(
        ReportDraft {
            kind: ReportKind::Panic,
            detail: Some(detail.to_string()),
            note: String::new(),
            marks: Vec::new(),
            viewport: extras.viewport,
            replay: ReplayVerdict::Unchecked,
        },
        ReportParts {
            frame: extras.frame,
            trace: extras.trace,
            trace_kind: extras.trace_kind,
            msgs: Some(snapshot),
            daemon: None,
            log,
            absent_reason: automatic_absent_reason().to_string(),
            log_absent_reason,
            daemon_absent_reason: None,
        },
    );
}

async fn execute_rpc(client: &Client, command: Command) -> OpOutcome {
    match command {
        Command::CreateAgent {
            host,
            name,
            agent_type,
            working_dir,
        } => {
            let request = CreateAgentRequest {
                agent_id: Uuid::new_v4(),
                host_id: host,
                name: Some(name),
                agent_type,
                working_dir,
                terminal_size: None,
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            };
            match client.create_agent(request).await {
                Ok(agent) => OpOutcome::AgentCreated { agent },
                Err(error) => op_error_outcome(&error),
            }
        }
        Command::RenameAgent { agent, name } => match client.rename_agent(agent, name).await {
            Ok(agent) => OpOutcome::AgentRenamed { agent },
            Err(error) => op_error_outcome(&error),
        },
        Command::DeleteAgent { agent } => match client.delete_agent(agent).await {
            Ok(()) => OpOutcome::AgentDeleted,
            Err(error) => op_error_outcome(&error),
        },
        // Input commands never ride Effect::Rpc — the reducer emits
        // Effect::SendInput for them (typed input + seq guard).
        Command::Send { .. }
        | Command::Queue(_)
        | Command::SetModel { .. }
        | Command::SetEffort { .. }
        | Command::SetPreset { .. }
        | Command::ClaudeSdk(_)
        | Command::Claude(_)
        | Command::Codex(_)
        | Command::SendPromptWithAttachments { .. }
        | Command::PutAttachment { .. }
        | Command::FetchDiff { .. }
        | Command::OpenAttachment { .. }
        | Command::RequestDiff { .. } => OpOutcome::Error {
            error: OpError::general("input command routed to the RPC executor"),
        },
    }
}

fn clone_artifact_cache(cache: &Option<Result<Arc<Cache>, String>>) -> Result<Arc<Cache>, OpError> {
    match cache {
        Some(Ok(cache)) => Ok(cache.clone()),
        Some(Err(error)) => Err(OpError::general(format!(
            "failed to open attachment cache: {error}"
        ))),
        None => Err(OpError::general("attachment cache is not configured")),
    }
}

async fn fetch_through_cache(
    cache: &Cache,
    client: &Client,
    agent: AgentId,
    id: &ArtifactId,
) -> Result<(ArtifactMeta, Vec<u8>), OpError> {
    let mut remote_error = None;
    let result = cache
        .get(id, async {
            match client.get_artifact(AgentIdentifier::Id(agent), id).await {
                Ok((artifact, bytes)) => Ok((
                    ArtifactMeta {
                        id: artifact.id,
                        kind: artifact.kind,
                        name: artifact.name,
                        mime: artifact.mime,
                        size: artifact.size,
                        created_at: Utc::now(),
                        pinned_at: None,
                    },
                    bytes,
                )),
                Err(error) => {
                    remote_error = Some(map_client_error(&error, None, &[]));
                    Err(FetchError::new(error.to_string()))
                }
            }
        })
        .await;
    match (result, remote_error) {
        (Ok(value), _) => Ok(value),
        (Err(_), Some(error)) => Err(error),
        (Err(error), None) => Err(map_store_error(error, None)),
    }
}

/// Store one picked draft's bytes and answer with what a token can name.
///
/// The host computes the artifact's identity from the bytes it received; a
/// disagreement with the identity computed here means the bytes did not
/// arrive intact, so the draft is refused rather than named.
pub async fn execute_put<C: AttachmentClient + ?Sized>(
    client: &C,
    agent: AgentId,
    draft: ui_state::DraftAttachment,
) -> OpOutcome {
    let Some(bytes) = draft.bytes.clone() else {
        return OpOutcome::Error {
            error: OpError::general("an attachment can only be stored with its bytes"),
        };
    };
    match client
        .put_artifact(
            AgentIdentifier::Id(agent),
            draft.kind,
            &draft.name,
            &draft.mime,
            bytes.to_vec(),
        )
        .await
    {
        Ok(artifact) if artifact.id == draft.id => OpOutcome::AttachmentStored {
            attachment: ui_state::DraftAttachment {
                bytes: None,
                ..draft
            },
        },
        Ok(_) => OpOutcome::Error {
            error: OpError::ArtifactCorrupt { id: draft.id },
        },
        Err(error) => OpOutcome::Error {
            error: map_client_error(&error, Some(&draft.name), std::slice::from_ref(&draft)),
        },
    }
}

/// Store every live draft, then deliver one native input carrying all pins.
/// No input is sent if any put fails.
pub async fn execute_put_then_send<C: AttachmentClient + ?Sized>(
    client: &C,
    op: OpId,
    agent: AgentId,
    puts: Vec<ui_state::attachments::DraftAttachment>,
    input: InputPayload,
    pin: Vec<ArtifactId>,
) -> OpOutcome {
    for draft in &puts {
        let Some(bytes) = &draft.bytes else {
            continue;
        };
        match client
            .put_artifact(
                AgentIdentifier::Id(agent),
                draft.kind,
                &draft.name,
                &draft.mime,
                bytes.to_vec(),
            )
            .await
        {
            Ok(artifact) if artifact.id == draft.id => {}
            Ok(_) => {
                return OpOutcome::Error {
                    error: OpError::ArtifactCorrupt {
                        id: draft.id.clone(),
                    },
                };
            }
            Err(error) => {
                return OpOutcome::Error {
                    error: map_client_error(&error, Some(&draft.name), &puts),
                };
            }
        }
    }

    let input = match input {
        InputPayload::Claude {
            expected_seq,
            intent,
            ..
        } => SessionInput::ClaudePtyTranscriptV1(model::ClaudePtyTranscriptV1Input {
            expected_seq,
            intent,
        }),
        InputPayload::ClaudeSdk { payload } => SessionInput::ClaudeSdkV1(payload),
        InputPayload::Codex { payload } => SessionInput::CodexSdkV1(payload),
    };
    match client
        .send_input(SendInputRequest {
            agent: AgentIdentifier::Id(agent),
            input_id: op.0.as_bytes().to_vec(),
            input,
            pin: pin.into_iter().map(|id| id.to_string()).collect(),
        })
        .await
    {
        Ok(()) => OpOutcome::InputSent,
        Err(error) => OpOutcome::Error {
            error: map_client_error(&error, None, &puts),
        },
    }
}

fn map_client_error(
    error: &ClientError,
    current_name: Option<&str>,
    puts: &[ui_state::attachments::DraftAttachment],
) -> OpError {
    match error {
        ClientError::Protocol(ProtocolError::AttachmentMissing { id }) => {
            let parsed = id.parse::<ArtifactId>();
            match parsed {
                Ok(id) => {
                    let name = puts
                        .iter()
                        .find(|draft| draft.id == id)
                        .map(|draft| draft.name.clone())
                        .or_else(|| current_name.map(str::to_owned))
                        .unwrap_or_else(|| id.to_string());
                    OpError::AttachmentMissing { id, name }
                }
                Err(_) => OpError::general(error.to_string()),
            }
        }
        ClientError::Protocol(ProtocolError::AttachmentTooLarge { size, max }) => {
            OpError::AttachmentTooLarge {
                name: current_name.unwrap_or("attachment").to_string(),
                size: *size,
                max: *max,
            }
        }
        ClientError::Protocol(ProtocolError::ArtifactCorrupt { id }) => match id.parse() {
            Ok(id) => OpError::ArtifactCorrupt { id },
            Err(_) => OpError::general(error.to_string()),
        },
        ClientError::Protocol(ProtocolError::DiffUnavailable { message }) => {
            OpError::DiffUnavailable {
                message: message.clone(),
            }
        }
        _ => OpError::classified(
            error.to_string(),
            is_auth_error(error),
            is_subscription_error(error),
        ),
    }
}

fn map_store_error(error: StoreError, name: Option<&str>) -> OpError {
    match error {
        StoreError::TooLarge { size, max } => OpError::AttachmentTooLarge {
            name: name.unwrap_or("attachment").to_string(),
            size,
            max,
        },
        StoreError::Missing { id } => OpError::AttachmentMissing {
            name: name.map(str::to_owned).unwrap_or_else(|| id.to_string()),
            id,
        },
        StoreError::Corrupt { id } => OpError::ArtifactCorrupt { id },
        StoreError::Fetch(error) => OpError::general(error.to_string()),
        StoreError::Io(error) => OpError::general(error.to_string()),
    }
}

fn open_with_platform_viewer(meta: &ArtifactMeta, path: &Path) -> io::Result<()> {
    platform_open_command(meta, path)?.spawn()?;
    Ok(())
}

fn platform_open_command(meta: &ArtifactMeta, path: &Path) -> io::Result<std::process::Command> {
    // Only the macOS viewer chooses its application from the artifact kind.
    #[cfg(not(target_os = "macos"))]
    let _ = meta;
    // A platform with no viewer never reaches the line that opens the file.
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let _ = path;
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        // Content-addressed blobs intentionally have no filename extension,
        // so LaunchServices classifies even valid PNG bytes as public.data.
        // Preview identifies every image format accepted by the composer from
        // the bytes themselves once it is selected explicitly.
        if meta.kind == ArtifactKind::Image {
            command.args(["-a", "Preview"]);
        }
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no platform attachment viewer is available",
    ));
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        command.arg(path);
        Ok(command)
    }
}

/// How many times a `retry_stale` input is re-sent with the seq
/// the refusal reported. Mechanical execution policy only: WHETHER a
/// send retries is the reducer's decision, carried on the effect.
const STALE_RETRY_LIMIT: u32 = 3;

/// The stated form of a seq-guard refusal (C5: the resurfaced ask carries
/// the failure stated; the technical detail rides in parentheses).
const STALE_INPUT_ERROR: &str = "input raced the session — it moved on before the keys landed";

/// Send a semantic Claude intent under the transcript sequence guard.
async fn execute_send_input(
    client: &Client,
    agent: AgentId,
    input_id: Vec<u8>,
    payload: InputPayload,
) -> OpOutcome {
    execute_send_input_with_pin(client, agent, input_id, payload, Vec::new()).await
}

async fn execute_send_input_with_pin(
    client: &Client,
    agent: AgentId,
    input_id: Vec<u8>,
    payload: InputPayload,
    pin: Vec<String>,
) -> OpOutcome {
    match payload {
        InputPayload::Claude {
            expected_seq,
            intent,
            retry_stale,
        } => {
            execute_claude_input(
                client,
                agent,
                input_id,
                expected_seq,
                intent,
                retry_stale,
                pin,
            )
            .await
        }
        InputPayload::ClaudeSdk { payload } => {
            match client
                .send_input(SendInputRequest {
                    agent: AgentIdentifier::Id(agent),
                    input_id,
                    input: SessionInput::ClaudeSdkV1(payload),
                    pin,
                })
                .await
            {
                Ok(()) => OpOutcome::InputSent,
                Err(error) => op_error_outcome(&error),
            }
        }
        InputPayload::Codex { payload } => {
            execute_codex_input(client, agent, input_id, payload, pin).await
        }
    }
}

async fn execute_codex_input(
    client: &Client,
    agent: AgentId,
    input_id: Vec<u8>,
    input: CodexInput,
    pin: Vec<String>,
) -> OpOutcome {
    match client
        .send_input(SendInputRequest {
            agent: AgentIdentifier::Id(agent),
            input_id,
            input: SessionInput::CodexSdkV1(input),
            pin,
        })
        .await
    {
        Ok(()) => OpOutcome::InputSent,
        Err(error) => op_error_outcome(&error),
    }
}

async fn execute_claude_input(
    client: &Client,
    agent: AgentId,
    input_id: Vec<u8>,
    expected_seq: u64,
    intent: ClaudePtyIntent,
    retry_stale: bool,
    pin: Vec<String>,
) -> OpOutcome {
    let mut expected_seq = expected_seq;
    let mut attempts = 0;
    loop {
        match client
            .send_input(SendInputRequest {
                agent: AgentIdentifier::Id(agent),
                input_id: input_id.clone(),
                input: SessionInput::ClaudePtyTranscriptV1(model::ClaudePtyTranscriptV1Input {
                    expected_seq,
                    intent: intent.clone(),
                }),
                pin: pin.clone(),
            })
            .await
        {
            Ok(()) => return OpOutcome::InputSent,
            Err(ClientError::Protocol(ProtocolError::SequenceNumberMismatch {
                current_seq,
                ..
            })) if retry_stale && attempts < STALE_RETRY_LIMIT => {
                // Position-independent intents (interrupt) re-send with
                // the seq the source reported; positional ones never take
                // this branch — they fail fast and resurface (C5).
                expected_seq = current_seq;
                attempts += 1;
            }
            Err(error @ ClientError::Protocol(ProtocolError::SequenceNumberMismatch { .. })) => {
                return OpOutcome::Error {
                    error: OpError::general(format!("{STALE_INPUT_ERROR} ({error})")),
                };
            }
            Err(error) => return op_error_outcome(&error),
        }
    }
}

fn op_error_outcome(error: &ClientError) -> OpOutcome {
    OpOutcome::Error {
        error: map_client_error(error, None, &[]),
    }
}

fn is_auth_error(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Protocol(ProtocolError::InvalidCredentials)
    )
}

fn is_subscription_error(error: &ClientError) -> bool {
    matches!(error, ClientError::Protocol(ProtocolError::PaymentRequired))
}

/// Map a client error to the disconnect vocabulary.
/// `ProtocolError::InvalidCredentials` surfaces as authentication-required —
/// the degraded state, never a dead app.
fn disconnect_reason(error: &ClientError) -> DisconnectReason {
    match error {
        ClientError::ServerShutdown(reason) => DisconnectReason::ServerShutdown {
            detail: reason.to_string(),
        },
        error if is_auth_error(error) => DisconnectReason::AuthenticationRequired,
        error if is_subscription_error(error) => DisconnectReason::SubscriptionRequired,
        error => DisconnectReason::TransportError {
            message: error.to_string(),
        },
    }
}

/// Dial, subscribe, pump inventory events into Msgs; on failure report
/// `Disconnected` and retry with backoff. This task manages resources; every
/// semantic decision it forwards as a Msg.
async fn connection_task(
    mut connector: Connector,
    tx: MsgSink,
    shared_client: Arc<StdMutex<Option<Client>>>,
    local_host_id: Option<HostId>,
    subscription_status_provider: Option<SubscriptionStatusProvider>,
    host_inventory: Option<Arc<dyn HostInventory>>,
    startup_gate: Arc<StartupGate>,
) {
    startup_gate.wait().await;
    let mut backoff = RECONNECT_BACKOFF_INITIAL;
    loop {
        let client = match connector().await {
            Ok(client) => client,
            Err(failure) => {
                let reason = if failure.auth_required {
                    DisconnectReason::AuthenticationRequired
                } else if failure.subscription_required {
                    DisconnectReason::SubscriptionRequired
                } else {
                    DisconnectReason::TransportError {
                        message: failure.message,
                    }
                };
                if tx
                    .send(Msg::Server(ServerMsg::Disconnected { reason }))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
                continue;
            }
        };

        *shared_client.lock().expect("client mutex poisoned") = Some(client.clone());
        let session_end = pump_inventory(
            &client,
            &tx,
            local_host_id,
            subscription_status_provider.as_ref(),
            host_inventory.as_ref(),
        )
        .await;
        *shared_client.lock().expect("client mutex poisoned") = None;

        let Some(reason) = session_end else {
            // The Msg channel closed: the Runtime is gone.
            return;
        };
        if tx
            .send(Msg::Server(ServerMsg::Disconnected { reason }))
            .await
            .is_err()
        {
            return;
        }
        backoff = RECONNECT_BACKOFF_INITIAL;
        tokio::time::sleep(backoff).await;
    }
}

/// Subscribe to hosts and agents and forward events until either stream
/// fails. Returns the disconnect reason, or `None` when the Msg channel
/// closed beneath us.
async fn pump_inventory(
    client: &Client,
    tx: &MsgSink,
    local_host_id: Option<HostId>,
    subscription_status_provider: Option<&SubscriptionStatusProvider>,
    host_inventory: Option<&Arc<dyn HostInventory>>,
) -> Option<DisconnectReason> {
    let hosts_stream = match host_inventory {
        Some(inventory) => inventory.subscribe_hosts().await,
        None => client.subscribe_hosts().await.map(|stream| stream.boxed()),
    };
    let mut hosts_stream = match hosts_stream {
        Ok(stream) => stream,
        Err(error) => return Some(disconnect_reason(&error)),
    };
    let mut agents_stream = match client.subscribe_agents().await {
        Ok(stream) => stream,
        Err(error) => return Some(disconnect_reason(&error)),
    };

    if tx
        .send(Msg::Server(ServerMsg::Connected { local_host_id }))
        .await
        .is_err()
    {
        return None;
    }
    let mut subscription_required = subscription_status_provider.map(|provider| provider());
    if let Some(required) = subscription_required
        && tx
            .send(Msg::Server(ServerMsg::CloudSubscriptionStatus { required }))
            .await
            .is_err()
    {
        return None;
    }
    let mut subscription_poll = subscription_status_provider
        .map(|_| tokio::time::interval(SUBSCRIPTION_STATUS_POLL_INTERVAL));
    let mut snapshot_agents = HashMap::new();
    if let Some(poll) = subscription_poll.as_mut() {
        poll.tick().await;
    }

    loop {
        let events: Vec<Msg> = tokio::select! {
            event = hosts_stream.next() => match event {
                Some(Ok(event)) => host_messages(event),
                Some(Err(error)) => return Some(disconnect_reason(&error)),
                None => return Some(DisconnectReason::TransportError {
                    message: "host inventory stream ended".into(),
                }),
            },
            event = agents_stream.recv() => match event {
                Ok(event) => agent_messages(&mut snapshot_agents, event),
                Err(error) => return Some(disconnect_reason(&error)),
            },
            _ = maybe_interval_tick(&mut subscription_poll), if subscription_poll.is_some() => {
                let required = subscription_status_provider.expect("poll requires provider")();
                if subscription_required == Some(required) {
                    continue;
                }
                subscription_required = Some(required);
                vec![Msg::Server(ServerMsg::CloudSubscriptionStatus { required })]
            },
        };
        for event in events {
            if tx.send(event).await.is_err() {
                return None;
            }
        }
    }
}

fn host_messages(event: model::HostEvent) -> Vec<Msg> {
    match event {
        model::HostEvent::HostUpdated { host } => vec![
            Msg::FleetDelta(store::FleetDelta::Host {
                host: host.clone(),
                revision: 0,
            }),
            Msg::FleetDelta(store::FleetDelta::Reachability {
                host_id: host.id,
                online: host.online,
            }),
            Msg::Server(ServerMsg::HostUpserted { host }),
        ],
        model::HostEvent::HostRemoved { id } => vec![
            Msg::FleetDelta(store::FleetDelta::HostRemoved { host_id: id }),
            Msg::Server(ServerMsg::HostRemoved { id }),
        ],
        model::HostEvent::SnapshotComplete => {
            vec![Msg::Server(ServerMsg::HostsSynchronized)]
        }
    }
}

#[cfg(test)]
fn agent_server_msgs(event: model::AgentEvent) -> Vec<ServerMsg> {
    agent_messages(&mut HashMap::new(), event)
        .into_iter()
        .filter_map(|message| match message {
            Msg::Server(message) => Some(message),
            _ => None,
        })
        .collect()
}

fn agent_messages(
    snapshot_agents: &mut HashMap<AgentId, model::Agent>,
    event: model::AgentEvent,
) -> Vec<Msg> {
    match event {
        model::AgentEvent::AgentUp { agent } => {
            snapshot_agents.insert(agent.id, agent.clone());
            let delta = store::FleetDelta::AgentUp {
                revision: agent.inventory_revision,
                agent: agent.clone(),
            };
            vec![
                Msg::FleetDelta(delta),
                Msg::Server(ServerMsg::AgentUpserted { agent }),
            ]
        }
        model::AgentEvent::AgentUpdated { agent } => {
            snapshot_agents.insert(agent.id, agent.clone());
            let delta = store::FleetDelta::AgentUpdated {
                revision: agent.inventory_revision,
                agent: agent.clone(),
            };
            vec![
                Msg::FleetDelta(delta),
                Msg::Server(ServerMsg::AgentUpserted { agent }),
            ]
        }
        model::AgentEvent::AgentDown {
            host_id,
            agent_id,
            inventory_revision,
        } => {
            snapshot_agents.remove(&agent_id);
            vec![
                Msg::FleetDelta(store::FleetDelta::AgentDown {
                    host_id,
                    agent_id,
                    revision: inventory_revision,
                    reason: None,
                }),
                Msg::Server(ServerMsg::AgentRemoved { id: agent_id }),
            ]
        }
        model::AgentEvent::SnapshotComplete {
            host_id,
            through_revision,
        } => {
            let mut agents = snapshot_agents
                .values()
                .filter(|agent| agent.host_id == host_id)
                .cloned()
                .map(|agent| {
                    let revision = agent.inventory_revision;
                    (agent, revision)
                })
                .collect::<Vec<_>>();
            agents.sort_by_key(|(agent, _)| agent.id);
            vec![
                Msg::FleetDelta(store::FleetDelta::Snapshot(store::FleetSnapshot {
                    host_id,
                    through_revision,
                    agents,
                })),
                Msg::Server(ServerMsg::AgentsSynchronized),
            ]
        }
        model::AgentEvent::Summary {
            host_id,
            agent_id,
            envelope,
        } => vec![
            Msg::FleetDelta(store::FleetDelta::Summary {
                host_id,
                agent_id,
                envelope: envelope.clone(),
            }),
            Msg::Server(ServerMsg::AgentSummary {
                agent: agent_id,
                envelope,
            }),
        ],
        model::AgentEvent::Progress {
            host_id,
            agent_id,
            progress,
        } => vec![
            Msg::FleetDelta(store::FleetDelta::Progress {
                host_id,
                agent_id,
                progress: progress.clone(),
            }),
            Msg::Server(ServerMsg::AgentProgress {
                agent: agent_id,
                progress,
            }),
        ],
        model::AgentEvent::HostInventory {
            host_id,
            agents,
            through_revision,
        } => {
            let agent_ids = agents.iter().map(|agent| agent.id).collect();
            let mut messages = agents
                .iter()
                .cloned()
                .map(|agent| Msg::Server(ServerMsg::AgentUpserted { agent }))
                .collect::<Vec<_>>();
            messages.insert(
                0,
                Msg::FleetDelta(store::FleetDelta::Snapshot(store::FleetSnapshot {
                    host_id,
                    through_revision,
                    agents: agents
                        .into_iter()
                        .map(|agent| {
                            let revision = agent.inventory_revision;
                            (agent, revision)
                        })
                        .collect(),
                })),
            );
            messages.push(Msg::Server(ServerMsg::HostInventory { host_id, agent_ids }));
            messages
        }
    }
}

async fn maybe_interval_tick(interval: &mut Option<tokio::time::Interval>) {
    if let Some(interval) = interval {
        interval.tick().await;
    }
}

/// Subscribe an agent's structured stream and forward coalesced batches.
/// Always terminates with a `Closed` Msg (unless the Runtime is gone), so
/// the Model never holds a stream open that no task backs.
async fn stream_task(
    client: Option<Client>,
    agent: AgentId,
    protocol: StructuredProtocol,
    tail: u64,
    tx: MsgSink,
) {
    if let Some(reason) = pump_structured_stream(client, agent, protocol, tail, &tx).await {
        let _ = tx
            .send(Msg::Stream {
                agent,
                event: StreamMsg::Closed { reason },
            })
            .await;
    }
}

async fn store_stream_task(
    shared_client: Arc<StdMutex<Option<Client>>>,
    agent: AgentId,
    protocol: StructuredProtocol,
    attempt: ui_state::StreamAttempt,
    query: StoreStreamQuery,
    paused: watch::Receiver<bool>,
    tx: MsgSink,
) {
    let reason =
        pump_store_stream(shared_client, agent, protocol, attempt, query, paused, &tx).await;
    if let Some(reason) = reason {
        let _ = tx
            .send(Msg::ChatStream {
                agent,
                attempt,
                event: ChatStreamMsg::Closed {
                    at: Utc::now(),
                    reason,
                },
            })
            .await;
    }
}

async fn pump_store_stream(
    shared_client: Arc<StdMutex<Option<Client>>>,
    agent: AgentId,
    protocol: StructuredProtocol,
    attempt: ui_state::StreamAttempt,
    query: StoreStreamQuery,
    mut paused: watch::Receiver<bool>,
    tx: &MsgSink,
) -> Option<StreamCloseReason> {
    let client = loop {
        if let Some(client) = shared_client.lock().expect("client mutex poisoned").clone() {
            break client;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let replay_query = match query {
        StoreStreamQuery::After { after, tail_bound } => ReplayQuery::After { after, tail_bound },
        StoreStreamQuery::TailCount { count, tail_bound } => {
            ReplayQuery::TailCount { count, tail_bound }
        }
    };
    let mut session = match client
        .subscribe_session(SubscribeSessionRequest {
            agent: AgentIdentifier::Id(agent),
            args: structured_stream_args_with_query(protocol, replay_query),
        })
        .await
    {
        Ok(session) => session,
        Err(error) => return Some(stream_close_from_client_error(&error)),
    };

    let mut opened = false;
    let mut batch = Vec::new();
    loop {
        while *paused.borrow() {
            if paused.changed().await.is_err() {
                return None;
            }
        }
        let event = if batch.is_empty() {
            Some(session.recv().await)
        } else if batch.len() >= MAX_STREAM_BATCH {
            None
        } else {
            session.recv().now_or_never()
        };
        match event {
            None => flush_store_stream_batch(tx, agent, attempt, &mut batch).await?,
            Some(Ok(SubscribeSessionEvent::Opened { replay })) => {
                if opened {
                    return Some(StreamCloseReason::InternalError {
                        detail: "session opened more than once".to_owned(),
                    });
                }
                let Some(facts) = replay else {
                    return Some(StreamCloseReason::InternalError {
                        detail: "structured session opened without replay facts".to_owned(),
                    });
                };
                tx.send(Msg::ChatStream {
                    agent,
                    attempt,
                    event: ChatStreamMsg::Opened {
                        facts: ReplayFactsDto::from(facts),
                        at: Utc::now(),
                    },
                })
                .await
                .ok()?;
                opened = true;
            }
            Some(Ok(SubscribeSessionEvent::Output(output))) => {
                if !opened {
                    return Some(StreamCloseReason::InternalError {
                        detail: "structured session emitted output before opening".to_owned(),
                    });
                }
                let row = match (protocol, output) {
                    (
                        StructuredProtocol::ClaudePtyTranscript,
                        SessionOutput::ClaudePtyTranscriptV1(row),
                    )
                    | (StructuredProtocol::ClaudeSdk, SessionOutput::ClaudeSdkV1(row))
                    | (StructuredProtocol::Codex, SessionOutput::CodexSdkV1(row)) => row,
                    _ => {
                        flush_store_stream_batch(tx, agent, attempt, &mut batch).await?;
                        return Some(StreamCloseReason::InternalError {
                            detail: "session emitted output for the wrong protocol".to_owned(),
                        });
                    }
                };
                match stream_entry(row) {
                    Ok(entry) => batch.push(entry),
                    Err(reason) => {
                        flush_store_stream_batch(tx, agent, attempt, &mut batch).await?;
                        return Some(reason);
                    }
                }
            }
            Some(Ok(SubscribeSessionEvent::ReplayComplete)) => {
                flush_store_stream_batch(tx, agent, attempt, &mut batch).await?;
                tx.send(Msg::ChatStream {
                    agent,
                    attempt,
                    event: ChatStreamMsg::ReplayComplete { at: Utc::now() },
                })
                .await
                .ok()?;
            }
            Some(Ok(SubscribeSessionEvent::Closed { reason })) => {
                flush_store_stream_batch(tx, agent, attempt, &mut batch).await?;
                return Some(stream_close_from_session(reason));
            }
            Some(Err(error)) => {
                flush_store_stream_batch(tx, agent, attempt, &mut batch).await?;
                return Some(stream_close_from_client_error(&error));
            }
        }
    }
}

async fn flush_store_stream_batch(
    tx: &MsgSink,
    agent: AgentId,
    attempt: ui_state::StreamAttempt,
    batch: &mut Vec<StreamEntry>,
) -> Option<()> {
    if batch.is_empty() {
        return Some(());
    }
    tx.send(Msg::ChatStream {
        agent,
        attempt,
        event: ChatStreamMsg::Batch {
            at: Utc::now(),
            entries: std::mem::take(batch),
        },
    })
    .await
    .ok()?;
    Some(())
}

/// Batches structured output opportunistically: block for the first entry,
/// then take whatever is already available (bounded), then flush one Batch
/// Msg — coalescing happens BEFORE the recorder sees the Msg. The `Opened`
/// Msg is derived at first flush because truncation is only knowable from
/// the first replayed seq.
async fn pump_structured_stream(
    client: Option<Client>,
    agent: AgentId,
    protocol: StructuredProtocol,
    tail: u64,
    tx: &MsgSink,
) -> Option<StreamCloseReason> {
    let Some(client) = client else {
        return Some(StreamCloseReason::TransportError {
            message: NOT_CONNECTED_ERROR.to_string(),
        });
    };
    let args = structured_stream_args(protocol, tail);
    let mut session = match client
        .subscribe_session(SubscribeSessionRequest {
            agent: AgentIdentifier::Id(agent),
            args,
        })
        .await
    {
        Ok(session) => session,
        Err(error) => return Some(stream_close_from_client_error(&error)),
    };

    let mut opened = false;
    let mut batch: Vec<StreamEntry> = Vec::new();
    loop {
        // Block only when there is nothing to flush; otherwise poll
        // opportunistically and flush on Pending or a full batch.
        let event = if batch.is_empty() {
            Some(session.recv().await)
        } else if batch.len() >= MAX_STREAM_BATCH {
            None
        } else {
            session.recv().now_or_never()
        };
        match event {
            None => {
                flush_stream_batch(tx, agent, &mut batch).await?;
            }
            Some(Ok(SubscribeSessionEvent::Opened { replay })) => {
                if opened {
                    return Some(StreamCloseReason::InternalError {
                        detail: "session opened more than once".to_string(),
                    });
                }
                let Some(facts) = replay else {
                    return Some(StreamCloseReason::InternalError {
                        detail: "structured session opened without replay facts".to_string(),
                    });
                };
                tx.send(Msg::Stream {
                    agent,
                    event: StreamMsg::Opened {
                        truncated: !matches!(facts.outcome, ReplayOutcome::Continuous),
                    },
                })
                .await
                .ok()?;
                opened = true;
            }
            Some(Ok(SubscribeSessionEvent::Output(output))) => {
                if !opened {
                    return Some(StreamCloseReason::InternalError {
                        detail: "structured session emitted output before opening".to_string(),
                    });
                }
                let row = match (protocol, output) {
                    (
                        StructuredProtocol::ClaudePtyTranscript,
                        SessionOutput::ClaudePtyTranscriptV1(row),
                    )
                    | (StructuredProtocol::ClaudeSdk, SessionOutput::ClaudeSdkV1(row))
                    | (StructuredProtocol::Codex, SessionOutput::CodexSdkV1(row)) => row,
                    _ => {
                        flush_stream_batch(tx, agent, &mut batch).await?;
                        return Some(StreamCloseReason::InternalError {
                            detail: "session emitted output for the wrong protocol".to_string(),
                        });
                    }
                };
                match stream_entry(row) {
                    Ok(entry) => batch.push(entry),
                    Err(reason) => {
                        flush_stream_batch(tx, agent, &mut batch).await?;
                        return Some(reason);
                    }
                }
            }
            Some(Ok(SubscribeSessionEvent::ReplayComplete)) => {
                flush_stream_batch(tx, agent, &mut batch).await?;
                tx.send(Msg::Stream {
                    agent,
                    event: StreamMsg::ReplayComplete,
                })
                .await
                .ok()?;
            }
            Some(Ok(SubscribeSessionEvent::Closed { reason })) => {
                flush_stream_batch(tx, agent, &mut batch).await?;
                return Some(stream_close_from_session(reason));
            }
            Some(Err(error)) => {
                flush_stream_batch(tx, agent, &mut batch).await?;
                return Some(stream_close_from_client_error(&error));
            }
        }
    }
}

/// Send the pending batch. Returns `None` when the Runtime is gone.
async fn flush_stream_batch(
    tx: &MsgSink,
    agent: AgentId,
    batch: &mut Vec<StreamEntry>,
) -> Option<()> {
    if batch.is_empty() {
        return Some(());
    }
    let entries = std::mem::take(batch);
    tx.send(Msg::Stream {
        agent,
        event: StreamMsg::Batch {
            at: Utc::now(),
            entries,
        },
    })
    .await
    .ok()?;
    Some(())
}

fn structured_stream_args(protocol: StructuredProtocol, tail: u64) -> SessionArgs {
    let replay_query = Some(ReplayQuery::TailCount {
        count: tail,
        tail_bound: None,
    });
    structured_stream_args_optional(protocol, replay_query)
}

fn structured_stream_args_with_query(
    protocol: StructuredProtocol,
    replay_query: ReplayQuery,
) -> SessionArgs {
    structured_stream_args_optional(protocol, Some(replay_query))
}

fn structured_stream_args_optional(
    protocol: StructuredProtocol,
    replay_query: Option<ReplayQuery>,
) -> SessionArgs {
    match protocol {
        StructuredProtocol::ClaudePtyTranscript => {
            SessionArgs::ClaudePtyTranscriptV1(model::ClaudePtyTranscriptV1Args {
                terminal_size: None,
                replay_query,
            })
        }
        StructuredProtocol::Codex => {
            SessionArgs::CodexSdkV1(model::CodexSdkV1Args { replay_query })
        }
        StructuredProtocol::ClaudeSdk => {
            SessionArgs::ClaudeSdkV1(model::ClaudeSdkV1Args { replay_query })
        }
    }
}

fn stream_entry(row: model::StructuredRow) -> Result<StreamEntry, StreamCloseReason> {
    let published_at =
        DateTime::from_timestamp_millis(row.published_at_unix_ms).ok_or_else(|| {
            StreamCloseReason::InternalError {
                detail: format!(
                    "structured entry {} has invalid publication timestamp {}",
                    row.seq, row.published_at_unix_ms
                ),
            }
        })?;
    let activity_at = row
        .activity_at_unix_ms
        .map(|timestamp| {
            DateTime::from_timestamp_millis(timestamp).ok_or_else(|| {
                StreamCloseReason::InternalError {
                    detail: format!(
                        "structured entry {} has invalid activity timestamp {timestamp}",
                        row.seq
                    ),
                }
            })
        })
        .transpose()?;
    let payload =
        serde_json::from_slice(&row.payload).map_err(|error| StreamCloseReason::InternalError {
            detail: format!("structured entry {} is not JSON: {error}", row.seq),
        })?;
    Ok(StreamEntry {
        seq: row.seq,
        published_at,
        activity_at,
        historical: row.historical,
        payload,
    })
}

fn stream_close_from_session(reason: SessionCloseReason) -> StreamCloseReason {
    match reason {
        SessionCloseReason::AgentDeleted => StreamCloseReason::AgentDeleted,
        SessionCloseReason::AgentExited { exit_code } => {
            StreamCloseReason::AgentExited { exit_code }
        }
        SessionCloseReason::HostUnreachable => StreamCloseReason::HostUnreachable,
        SessionCloseReason::Reset => StreamCloseReason::Reset,
        SessionCloseReason::InternalError { detail } => StreamCloseReason::InternalError { detail },
    }
}

fn stream_close_from_client_error(error: &ClientError) -> StreamCloseReason {
    if is_auth_error(error) {
        StreamCloseReason::AuthenticationRequired
    } else if is_subscription_error(error) {
        StreamCloseReason::SubscriptionRequired
    } else {
        StreamCloseReason::TransportError {
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_sdk_stream_wire_preserves_tail_sequence_and_json() {
        assert_eq!(
            structured_stream_args(StructuredProtocol::ClaudeSdk, 1000),
            SessionArgs::ClaudeSdkV1(model::ClaudeSdkV1Args {
                replay_query: Some(ReplayQuery::TailCount {
                    count: 1000,
                    tail_bound: None,
                }),
            })
        );
        let row = br#"{"type":"amux.claude_sdk.ready","session_id":"s","resumed":false}"#;
        assert_eq!(
            stream_entry(model::StructuredRow {
                seq: 7,
                published_at_unix_ms: 1,
                activity_at_unix_ms: None,
                historical: false,
                payload: row.to_vec(),
            })
            .unwrap(),
            StreamEntry {
                seq: 7,
                published_at: DateTime::from_timestamp_millis(1).unwrap(),
                activity_at: None,
                historical: false,
                payload: serde_json::from_slice(row).unwrap()
            }
        );
        assert!(matches!(stream_entry(model::StructuredRow {
                seq: 7,
                published_at_unix_ms: 1,
                activity_at_unix_ms: None,
                historical: false,
                payload: b"{".to_vec(),
            }),
            Err(StreamCloseReason::InternalError { detail }) if detail.contains("entry 7 is not JSON")));
    }

    #[test]
    fn host_inventory_forwards_agent_bodies_before_authoritative_membership() {
        let host_id = HostId::from_u128(7);
        let first = codex_agent(AgentId::from_u128(1), host_id);
        let second = claude_agent(AgentId::from_u128(2), host_id);

        assert_eq!(
            agent_server_msgs(model::AgentEvent::HostInventory {
                host_id,
                agents: vec![first.clone(), second.clone()],
                through_revision: 3,
            }),
            vec![
                ServerMsg::AgentUpserted { agent: first },
                ServerMsg::AgentUpserted { agent: second },
                ServerMsg::HostInventory {
                    host_id,
                    agent_ids: vec![AgentId::from_u128(1), AgentId::from_u128(2)],
                },
            ]
        );
    }

    #[test]
    fn host_updates_persist_facts_before_reachability_and_model_projection() {
        let host = model::HostEntry {
            id: HostId::from_u128(9),
            name: "remembered-host".to_owned(),
            online: false,
            version: Some("test".to_owned()),
            capabilities: Some(model::Capabilities::default()),
            trust_status: model::HostTrustStatus::Trusted,
            last_dial_error: Some("offline".to_owned()),
            platform: None,
        };
        assert_eq!(
            host_messages(model::HostEvent::HostUpdated { host: host.clone() }),
            vec![
                Msg::FleetDelta(store::FleetDelta::Host {
                    host: host.clone(),
                    revision: 0,
                }),
                Msg::FleetDelta(store::FleetDelta::Reachability {
                    host_id: host.id,
                    online: false,
                }),
                Msg::Server(ServerMsg::HostUpserted { host }),
            ]
        );
    }

    #[test]
    fn local_snapshot_uses_the_agent_run_seen_before_completion() {
        let host = HostId::from_u128(7);
        let kept = claude_agent(AgentId::from_u128(1), host);
        let mut snapshot_agents = HashMap::new();
        let _ = agent_messages(
            &mut snapshot_agents,
            model::AgentEvent::AgentUp {
                agent: kept.clone(),
            },
        );

        assert_eq!(
            agent_messages(
                &mut snapshot_agents,
                model::AgentEvent::SnapshotComplete {
                    host_id: host,
                    through_revision: 3,
                },
            ),
            vec![
                Msg::FleetDelta(store::FleetDelta::Snapshot(store::FleetSnapshot {
                    host_id: host,
                    through_revision: 3,
                    agents: vec![(kept.clone(), kept.inventory_revision)],
                })),
                Msg::Server(ServerMsg::AgentsSynchronized),
            ]
        );
    }

    #[test]
    fn live_summary_and_progress_events_reach_the_reducer() {
        let agent = AgentId::from_u128(1);
        let host = HostId::from_u128(2);
        let envelope = model::SummaryEnvelope {
            through: 8,
            producer_version: 1,
            observed_at: Utc::now(),
            stale: false,
            revision: 12,
            summary: model::Summary {
                attention: model::Attention::Working,
                phase: model::AgentPhase::Running,
                last_activity: None,
                todo: None,
                context: None,
                model: None,
                unknown: vec![model::SummaryField::LastActivity],
            },
        };
        assert_eq!(
            agent_server_msgs(model::AgentEvent::Summary {
                host_id: host,
                agent_id: agent,
                envelope: envelope.clone(),
            }),
            vec![ServerMsg::AgentSummary { agent, envelope }]
        );

        let progress = model::Progress {
            through: 9,
            at: Utc::now(),
            revision: 13,
        };
        assert_eq!(
            agent_server_msgs(model::AgentEvent::Progress {
                host_id: host,
                agent_id: agent,
                progress: progress.clone(),
            }),
            vec![ServerMsg::AgentProgress { agent, progress }]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_opens_extensionless_images_with_preview() {
        let meta = ArtifactMeta {
            id: model::id_of(b"png"),
            kind: ArtifactKind::Image,
            name: "clipboard.png".to_string(),
            mime: "image/png".to_string(),
            size: 3,
            created_at: Utc::now(),
            pinned_at: None,
        };
        let command = platform_open_command(&meta, Path::new("/cache/blobs/digest"))
            .expect("macOS has an attachment opener");

        assert_eq!(command.get_program(), "open");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-a", "Preview", "/cache/blobs/digest"]
        );
    }

    #[test]
    fn payment_required_maps_to_subscription_state_only() {
        let error = ClientError::Protocol(ProtocolError::PaymentRequired);

        assert!(is_subscription_error(&error));
        assert!(!is_auth_error(&error));
        assert_eq!(
            disconnect_reason(&error),
            DisconnectReason::SubscriptionRequired
        );
        assert_eq!(
            stream_close_from_client_error(&error),
            StreamCloseReason::SubscriptionRequired
        );
    }

    fn codex_agent(agent: AgentId, host: HostId) -> model::Agent {
        model::Agent {
            id: agent,
            host_id: host,
            name: Some("projection-test".to_string()),
            command: "codex".to_string(),
            working_dir: PathBuf::from("/work"),
            kind: model::AgentKind::Codex,
            readonly: false,
            args: Vec::new(),
            created_at: DateTime::from_timestamp(1_754_697_600, 0).expect("valid fixture time"),
            parent: None,
            working_on: None,
            summary: None,
            progress: None,
            inventory_revision: 0,
        }
    }

    fn claude_agent(agent: AgentId, host: HostId) -> model::Agent {
        model::Agent {
            id: agent,
            host_id: host,
            name: Some("dispatch-clock-test".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/work"),
            kind: model::AgentKind::Claude {
                driver: model::ClaudeDriver::Pty,
            },
            readonly: false,
            args: Vec::new(),
            created_at: DateTime::from_timestamp(1_754_697_600, 0).expect("valid fixture time"),
            parent: None,
            working_on: None,
            summary: None,
            progress: None,
            inventory_revision: 0,
        }
    }

    fn process_and_assert_coherent(runtime: &mut Runtime, msg: Msg) {
        runtime.process(msg);
        let violations = runtime.model().check_invariants();
        assert!(
            violations.is_empty(),
            "Runtime fold must stay coherent after every Msg: {violations:?}"
        );
    }

    /// A Runtime with no shell tasks: Msgs enter only through the direct
    /// fold surface (`dispatch`/`observe_now`), which is all the panic-report
    /// path needs. No tokio runtime required.
    fn a_runtime(report_dir: PathBuf) -> Runtime {
        a_runtime_with_git_sha(report_dir, "test-sha")
    }

    fn a_runtime_with_git_sha(report_dir: PathBuf, git_sha: &'static str) -> Runtime {
        let log_path = report_dir.join("amux.log");
        std::fs::write(&log_path, "runtime test log\n").expect("write test log");
        let model = Model::default();
        let recorder = Arc::new(StdMutex::new(Recorder::new(
            DEFAULT_RECORDER_CAPACITY,
            &model,
        )));
        let (msg_tx, msg_rx) = mpsc::channel(MSG_CHANNEL_CAPACITY);
        Runtime {
            model,
            recorder,
            msg_sink: MsgSink {
                tx: msg_tx,
                generation: Generation::default(),
            },
            msg_rx,
            client: Arc::new(StdMutex::new(None)),
            tasks: Vec::new(),
            streams: HashMap::new(),
            store_streams: HashMap::new(),
            store_worker: None,
            store_first_frame_seen: false,
            startup_gate: Arc::new(StartupGate::default()),
            report_dir: Some(report_dir),
            log_path: Some(log_path),
            git_sha,
            report_extras: None,
            msg_tap: None,
            artifact_cache: None,
            attachment_opener: Arc::new(open_with_platform_viewer),
            queued_bytes: HashMap::new(),
            discarded_late: std::collections::BTreeSet::new(),
            discarded_late_count: 0,
            reported_violations: HashSet::new(),
        }
    }

    fn test_view_set(
        profile: ProfileGeneration,
        op: u64,
        key: impl Into<String>,
    ) -> ui_state::StoreOp {
        ui_state::StoreOp::ViewSet {
            profile,
            op: store::OpId(op),
            kind: "runtime-test".to_owned(),
            key: key.into(),
            value: "written".to_owned(),
        }
    }

    #[tokio::test]
    async fn a_replacement_store_stream_starts_with_reducer_backpressure() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut runtime = a_runtime(directory.path().to_path_buf());
        let agent = AgentId::from_u128(1);

        runtime.run_effect(Effect::OpenStoreStream {
            agent,
            protocol: StructuredProtocol::ClaudePtyTranscript,
            attempt: ui_state::StreamAttempt(2),
            query: StoreStreamQuery::After {
                after: 10,
                tail_bound: Some(1_000),
            },
            paused: true,
        });

        let stream = runtime
            .store_streams
            .get(&agent)
            .expect("replacement stream task");
        assert!(*stream.paused.borrow());
    }

    #[tokio::test]
    async fn store_startup_is_recorded_before_the_first_connection_attempt() {
        let directory = tempfile::tempdir().expect("tempdir");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let connector_calls = calls.clone();
        let connector: Connector = Box::new(move || {
            connector_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        });
        let seen = Arc::new(StdMutex::new(Vec::<Msg>::new()));
        let tapped = seen.clone();
        let mut runtime = Runtime::start(
            connector,
            RuntimeOptions {
                store_path: Some(directory.path().join("store.sqlite")),
                msg_tap: Some(Box::new(move |msg| {
                    tapped.lock().expect("tap mutex").push(msg.clone());
                })),
                ..RuntimeOptions::default()
            },
        );

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        tokio::task::yield_now().await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the connector must remain gated before store startup is folded"
        );

        tokio::time::timeout(Duration::from_secs(5), runtime.next_message())
            .await
            .expect("store startup message timed out");
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        tokio::time::timeout(Duration::from_secs(5), runtime.next_message())
            .await
            .expect("fleet startup message timed out");
        tokio::task::yield_now().await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the connector must remain gated until the remembered view is folded"
        );

        tokio::time::timeout(Duration::from_secs(5), runtime.next_message())
            .await
            .expect("view startup message timed out");
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let seen = seen.lock().expect("tap mutex");
        assert!(matches!(seen[0], Msg::StoreStartup { .. }));
        assert!(matches!(
            (&seen[1], &seen[2]),
            (
                Msg::Store(StoreMsg::FleetLoaded { .. }),
                Msg::Store(StoreMsg::ViewLoaded { .. })
            )
        ));
        assert!(
            runtime
                .recorder_snapshot()
                .msgs
                .iter()
                .any(|line| line.contains("FleetLoaded"))
        );
        assert!(
            runtime
                .recorder_snapshot()
                .msgs
                .iter()
                .any(|line| line.contains("ViewLoaded"))
        );
    }

    #[tokio::test]
    async fn active_store_polls_external_changes_while_commands_keep_arriving() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("store.sqlite");
        let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tapped = changed.clone();
        let mut runtime = Runtime::start(
            Box::new(|| Box::pin(std::future::pending())),
            RuntimeOptions {
                store_path: Some(path.clone()),
                msg_tap: Some(Box::new(move |msg| {
                    if matches!(msg, Msg::Store(StoreMsg::FleetChanged { .. })) {
                        tapped.store(true, Ordering::Release);
                    }
                })),
                ..RuntimeOptions::default()
            },
        );
        for _ in 0..3 {
            tokio::time::timeout(Duration::from_secs(5), runtime.next_message())
                .await
                .expect("startup message timed out");
        }

        let worker = runtime
            .store_worker
            .as_ref()
            .expect("store worker")
            .handle();
        let producer = tokio::spawn(async move {
            for op in 10_000..10_040 {
                worker.execute(test_view_set(
                    ProfileGeneration(0),
                    op,
                    format!("steady-{op}"),
                ));
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });

        let external = store::Store::open(&path)
            .await
            .expect("second store handle");
        external
            .view_set("poll-test", "changed", "yes")
            .await
            .expect("external write");
        external.close().await;

        tokio::time::timeout(Duration::from_secs(3), async {
            while !changed.load(Ordering::Acquire) {
                assert!(runtime.next_message().await);
            }
        })
        .await
        .expect("data-version change was not reported");

        let polls = runtime
            .store_worker
            .as_ref()
            .expect("store worker")
            .data_version_poll_counter();
        runtime.switch_connector(
            Box::new(|| Box::pin(std::future::pending())),
            RuntimeOptions::default(),
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let retired_at = polls.load(Ordering::Acquire);
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert_eq!(
            polls.load(Ordering::Acquire),
            retired_at,
            "a retired profile must stop polling its store"
        );
        producer.abort();
    }

    #[test]
    fn switching_profiles_does_not_wait_for_a_store_worker_blocked_on_the_ui_channel() {
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let executor = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime");
            executor.block_on(async move {
                let directory = tempfile::tempdir().expect("tempdir");
                let path = directory.path().join("store.sqlite");
                let mut runtime = a_runtime(directory.path().to_path_buf());
                runtime.store_worker = Some(StoreWorker::spawn(
                    path.clone(),
                    ProfileGeneration(0),
                    None,
                    runtime.msg_sink.clone(),
                ));
                for _ in 0..3 {
                    next_store_runtime_message(&mut runtime).await;
                }

                let generation = runtime.generation();
                let mut filled = 0;
                loop {
                    match runtime
                        .msg_sink
                        .tx
                        .try_send((generation, Msg::Tick { now: Utc::now() }))
                    {
                        Ok(()) => filled += 1,
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => break,
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            panic!("runtime message channel closed")
                        }
                    }
                }
                assert_eq!(filled, MSG_CHANNEL_CAPACITY);

                let worker = runtime.store_worker.as_ref().expect("store worker");
                for op in 20_000..20_004 {
                    worker.execute(test_view_set(
                        ProfileGeneration(0),
                        op,
                        format!("queued-{op}"),
                    ));
                }
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let written = rusqlite::Connection::open(&path)
                            .and_then(|connection| {
                                connection.query_row(
                                    "SELECT value FROM view_state WHERE kind='runtime-test' AND key='queued-20000'",
                                    [],
                                    |row| row.get::<_, String>(0),
                                )
                            })
                            .ok();
                        if written.as_deref() == Some("written") {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("store operation did not reach the full UI channel");

                runtime.switch_connector(
                    Box::new(|| Box::pin(std::future::pending())),
                    RuntimeOptions::default(),
                );
                finished_tx.send(()).expect("report completed switch");
            });
        });

        finished_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("profile switch waited for the retired store worker");
        thread.join().expect("switch test thread");
    }

    async fn next_store_runtime_message(runtime: &mut Runtime) {
        assert!(
            tokio::time::timeout(Duration::from_secs(5), runtime.next_message())
                .await
                .expect("store runtime message timed out"),
            "store runtime closed before the expected message"
        );
    }

    async fn wait_for_store_runtime(
        runtime: &mut Runtime,
        mut ready: impl FnMut(&Runtime) -> bool,
    ) {
        for _ in 0..20 {
            if ready(runtime) {
                return;
            }
            next_store_runtime_message(runtime).await;
        }
        panic!("store runtime did not reach the expected state");
    }

    async fn seed_recovery_fleet(path: &Path, agent: AgentId, host: HostId) {
        let store = store::Store::open(path).await.expect("seed store");
        let generations = store
            .generations()
            .for_provider("claude_pty")
            .expect("Claude PTY store generations");
        store
            .apply_fleet(
                generations,
                store::FleetDelta::Host {
                    host: model::HostEntry {
                        id: host,
                        name: "recovery-host".to_owned(),
                        online: true,
                        version: Some("test".to_owned()),
                        capabilities: Some(model::Capabilities::default()),
                        trust_status: model::HostTrustStatus::Trusted,
                        last_dial_error: None,
                        platform: None,
                    },
                    revision: 1,
                },
            )
            .await
            .expect("seed host");
        store
            .apply_fleet(
                generations,
                store::FleetDelta::AgentUp {
                    agent: claude_agent(agent, host),
                    revision: 2,
                },
            )
            .await
            .expect("seed agent");
        store.close().await;
    }

    async fn seed_recovery_chat(path: &Path, agent: AgentId, host: HostId) {
        seed_recovery_fleet(path, agent, host).await;

        let mut runtime = Runtime::start(
            Box::new(|| Box::pin(std::future::pending())),
            RuntimeOptions {
                store_path: Some(path.to_owned()),
                ..RuntimeOptions::default()
            },
        );
        for _ in 0..3 {
            next_store_runtime_message(&mut runtime).await;
        }
        runtime.open_chat(agent);
        wait_for_store_runtime(&mut runtime, |runtime| {
            runtime
                .model()
                .chat(agent)
                .is_some_and(|chat| chat.state == ui_state::ChatState::Painted)
        })
        .await;

        let attempt = runtime
            .model()
            .chat(agent)
            .expect("seed chat")
            .stream_attempt;
        let now = Utc::now();
        runtime.process(Msg::ChatStream {
            agent,
            attempt,
            event: ChatStreamMsg::Opened {
                facts: ReplayFactsDto {
                    retained_from: 1,
                    through: 2,
                    selected_from: 1,
                    reset_at: 0,
                    outcome: ui_state::ReplayOutcomeDto::Continuous,
                },
                at: now,
            },
        });
        runtime.process(Msg::ChatStream {
            agent,
            attempt,
            event: ChatStreamMsg::Batch {
                at: now,
                entries: vec![
                    StreamEntry::observed(
                        1,
                        now,
                        serde_json::json!({"type": "amux.transcript_ready"}),
                    ),
                    StreamEntry::observed(
                        2,
                        now,
                        serde_json::json!({
                            "type": "user",
                            "uuid": "dddddddd-0000-4000-8000-000000000058",
                            "sessionId": "22222222-2222-4222-8222-222222222222",
                            "timestamp": "2026-08-11T22:00:00.000Z",
                            "message": {"role": "user", "content": "remember this"},
                            "origin": {"kind": "human"},
                            "promptSource": "typed"
                        }),
                    ),
                ],
            },
        });
        wait_for_store_runtime(&mut runtime, |runtime| {
            runtime
                .model()
                .chat(agent)
                .is_some_and(|chat| chat.pending_bytes() == 0)
        })
        .await;
        runtime.process(Msg::ChatStream {
            agent,
            attempt,
            event: ChatStreamMsg::ReplayComplete { at: now },
        });
        wait_for_store_runtime(&mut runtime, |runtime| {
            runtime
                .model()
                .chat(agent)
                .is_some_and(|chat| chat.pending_bytes() == 0)
        })
        .await;
        assert!(!runtime.model().chat(agent).expect("seed chat").live_only);
    }

    fn stored_last_opened_at(path: &Path, agent: AgentId) -> Option<i64> {
        rusqlite::Connection::open(path)
            .expect("open store for recency inspection")
            .query_row(
                "SELECT last_opened_at FROM agent WHERE id=?1",
                [agent.to_string()],
                |row| row.get(0),
            )
            .expect("read chat recency")
    }

    #[tokio::test]
    async fn store_maintenance_starts_after_the_first_frame_and_is_hourly() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut runtime = Runtime::start(
            Box::new(|| Box::pin(std::future::pending())),
            RuntimeOptions {
                store_path: Some(directory.path().join("store.sqlite")),
                ..RuntimeOptions::default()
            },
        );
        assert_eq!(
            runtime
                .store_worker
                .as_ref()
                .expect("store worker")
                .maintenance_runs(),
            0
        );

        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert_eq!(
            runtime
                .store_worker
                .as_ref()
                .expect("store worker")
                .maintenance_runs(),
            0,
            "maintenance must not run before the first frame"
        );

        tokio::time::timeout(Duration::from_secs(5), runtime.next())
            .await
            .expect("startup message timed out");
        tokio::time::timeout(Duration::from_secs(3), async {
            while runtime
                .store_worker
                .as_ref()
                .expect("store worker")
                .maintenance_runs()
                == 0
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("maintenance did not run after the first frame");
        assert_eq!(
            runtime
                .store_worker
                .as_ref()
                .expect("store worker")
                .maintenance_runs(),
            1
        );

        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert_eq!(
            runtime
                .store_worker
                .as_ref()
                .expect("store worker")
                .maintenance_runs(),
            1,
            "idle polling must not repeat maintenance inside the hour"
        );
    }

    #[tokio::test]
    async fn user_open_records_recency_but_remembered_startup_does_not_subscribe() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("store.sqlite");
        let agent = AgentId::from_u128(598);
        let host = HostId::from_u128(599);
        seed_recovery_fleet(&path, agent, host).await;
        assert_eq!(stored_last_opened_at(&path, agent), None);

        let options = || RuntimeOptions {
            store_path: Some(path.clone()),
            ..RuntimeOptions::default()
        };
        let mut runtime = Runtime::start(Box::new(|| Box::pin(std::future::pending())), options());
        for _ in 0..3 {
            next_store_runtime_message(&mut runtime).await;
        }
        runtime.open_chat(agent);
        tokio::time::timeout(Duration::from_secs(5), async {
            while stored_last_opened_at(&path, agent).is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("explicit user open did not record recency");
        drop(runtime);

        let connection = rusqlite::Connection::open(&path).expect("open store recency marker");
        connection
            .execute(
                "UPDATE agent SET last_opened_at=7 WHERE id=?1",
                [agent.to_string()],
            )
            .expect("install exact recency marker");
        drop(connection);

        let mut reopened = Runtime::start(Box::new(|| Box::pin(std::future::pending())), options());
        wait_for_store_runtime(&mut reopened, |runtime| {
            runtime.model().remembered_chat() == Some(agent)
                && runtime.model().agent(agent).is_some()
        })
        .await;
        reopened.process(Msg::Server(ServerMsg::Connected {
            local_host_id: Some(host),
        }));
        assert!(reopened.model().chat(agent).is_none());
        assert!(
            reopened.store_streams.is_empty(),
            "connecting with a remembered cursor must not subscribe"
        );
        assert_eq!(
            stored_last_opened_at(&path, agent),
            Some(7),
            "remembered startup must not look like a user open"
        );
        reopened.open_chat(agent);
        wait_for_store_runtime(&mut reopened, |runtime| {
            runtime
                .model()
                .chat(agent)
                .is_some_and(|chat| chat.state == ui_state::ChatState::Painted)
        })
        .await;
        assert!(
            reopened.store_streams.contains_key(&agent),
            "the explicit user open must start the chat stream"
        );
    }

    #[tokio::test]
    async fn a_local_snapshot_removes_an_agent_that_disappeared_between_runs() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("store.sqlite");
        let agent = AgentId::from_u128(608);
        let host = HostId::from_u128(609);
        seed_recovery_fleet(&path, agent, host).await;

        let options = || RuntimeOptions {
            store_path: Some(path.clone()),
            ..RuntimeOptions::default()
        };
        let mut observing =
            Runtime::start(Box::new(|| Box::pin(std::future::pending())), options());
        wait_for_store_runtime(&mut observing, |runtime| {
            runtime.model().agent(agent).is_some()
        })
        .await;

        for message in host_messages(model::HostEvent::HostUpdated {
            host: model::HostEntry {
                id: host,
                name: "remembered-host".to_owned(),
                online: true,
                version: Some("new".to_owned()),
                capabilities: Some(model::Capabilities::default()),
                trust_status: model::HostTrustStatus::Trusted,
                last_dial_error: None,
                platform: None,
            },
        }) {
            observing.process(message);
        }
        let mut snapshot_agents = HashMap::new();
        for message in agent_messages(
            &mut snapshot_agents,
            model::AgentEvent::SnapshotComplete {
                host_id: host,
                through_revision: 3,
            },
        ) {
            observing.process(message);
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (membership, host_name): (i64, String) = rusqlite::Connection::open(&path)
                    .expect("inspect fleet store")
                    .query_row(
                        "SELECT agent.membership,host.name
                         FROM agent JOIN host ON host.id=agent.host_id
                         WHERE agent.id=?1",
                        [agent.to_string()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .expect("stored agent membership");
                if membership == 1 && host_name == "remembered-host" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("snapshot was not persisted");
        drop(observing);

        let mut reopened = Runtime::start(Box::new(|| Box::pin(std::future::pending())), options());
        for _ in 0..3 {
            next_store_runtime_message(&mut reopened).await;
        }
        assert!(
            reopened.model().agent(agent).is_none(),
            "the absent agent must not return as a remembered card"
        );
    }

    #[tokio::test]
    async fn invalidated_real_store_chats_open_a_valid_successor_without_losing_the_old_segment() {
        for (case, damage, expected_boundary) in [
            (
                "tip-version",
                "UPDATE chat_head SET tip_version=0 WHERE agent_id=?1",
                ui_state::Boundary::VersionGap,
            ),
            (
                "undecodable-tip",
                "UPDATE claude_pty_tip SET tip=X'00' WHERE agent_id=?1",
                ui_state::Boundary::Gap,
            ),
        ] {
            let directory = tempfile::tempdir().expect("tempdir");
            let path = directory.path().join(format!("{case}.sqlite"));
            let agent = AgentId::from_u128(if case == "tip-version" { 581 } else { 582 });
            let host = HostId::from_u128(583);
            seed_recovery_chat(&path, agent, host).await;

            let connection = rusqlite::Connection::open(&path).expect("open seeded database");
            assert_eq!(
                connection
                    .execute(damage, [agent.to_string()])
                    .expect("damage stored tip"),
                1
            );
            drop(connection);

            let mut runtime = Runtime::start(
                Box::new(|| Box::pin(std::future::pending())),
                RuntimeOptions {
                    store_path: Some(path),
                    ..RuntimeOptions::default()
                },
            );
            wait_for_store_runtime(&mut runtime, |runtime| {
                runtime.model().remembered_chat() == Some(agent)
                    && runtime.model().agent(agent).is_some()
            })
            .await;
            runtime.open_chat(agent);
            wait_for_store_runtime(&mut runtime, |runtime| {
                runtime.model().chat(agent).is_some_and(|chat| {
                    chat.state == ui_state::ChatState::Painted
                        && chat
                            .boundaries
                            .iter()
                            .any(|boundary| boundary.boundary == expected_boundary)
                })
            })
            .await;

            let before = runtime.model().chat(agent).expect("recovered chat");
            assert!(!before.live_only, "{case} invalidation became live-only");
            assert!(
                !before.entries.is_empty(),
                "{case} discarded the old segment"
            );
            let attempt = before.stream_attempt;
            let now = Utc::now();
            runtime.process(Msg::ChatStream {
                agent,
                attempt,
                event: ChatStreamMsg::Opened {
                    facts: ReplayFactsDto {
                        retained_from: 1,
                        through: 3,
                        selected_from: 3,
                        reset_at: 0,
                        outcome: ui_state::ReplayOutcomeDto::Continuous,
                    },
                    at: now,
                },
            });
            let catching_up = runtime.model().chat(agent).expect("successor chat");
            assert_eq!(catching_up.state, ui_state::ChatState::CatchingUp);
            assert!(!catching_up.live_only);
            assert!(
                !catching_up.entries.is_empty(),
                "{case} did not keep the old segment behind the boundary"
            );

            runtime.process(Msg::ChatStream {
                agent,
                attempt,
                event: ChatStreamMsg::Batch {
                    at: now,
                    entries: vec![StreamEntry::observed(
                        3,
                        now,
                        serde_json::json!({
                            "type": "user",
                            "uuid": "dddddddd-0000-4000-8000-000000000059",
                            "sessionId": "22222222-2222-4222-8222-222222222222",
                            "timestamp": "2026-08-11T22:00:01.000Z",
                            "message": {"role": "user", "content": "after recovery"},
                            "origin": {"kind": "human"},
                            "promptSource": "typed"
                        }),
                    )],
                },
            });
            wait_for_store_runtime(&mut runtime, |runtime| {
                runtime.model().chat(agent).is_some_and(|chat| {
                    chat.state == ui_state::ChatState::CatchingUp && chat.pending_bytes() == 0
                })
            })
            .await;

            runtime.process(Msg::ChatStream {
                agent,
                attempt,
                event: ChatStreamMsg::ReplayComplete { at: now },
            });
            for _ in 0..5 {
                if runtime.model().chat(agent).is_some_and(|chat| {
                    chat.state == ui_state::ChatState::Live && chat.pending_bytes() == 0
                }) {
                    break;
                }
                if tokio::time::timeout(Duration::from_secs(5), runtime.next_message())
                    .await
                    .is_err()
                {
                    panic!(
                        "{case} successor stopped before commit resolution: {:?}",
                        runtime.model().chat(agent)
                    );
                }
            }
            assert!(
                !runtime
                    .model()
                    .chat(agent)
                    .expect("committed successor")
                    .live_only,
                "{case} successor commit was refused"
            );
        }
    }

    #[tokio::test]
    async fn unresolved_durable_quarantine_does_not_make_derived_chats_live_only() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("store.sqlite");
        store::Store::open(&path)
            .await
            .expect("initialize store")
            .close()
            .await;
        let connection = rusqlite::Connection::open(&path).expect("open initialized database");
        connection
            .execute(
                "INSERT INTO quarantine(id,manifest,durable_unresolved) VALUES ('lost','{}',1)",
                [],
            )
            .expect("mark durable state unresolved");
        drop(connection);

        let mut runtime = Runtime::start(
            Box::new(|| Box::pin(std::future::pending())),
            RuntimeOptions {
                store_path: Some(path),
                ..RuntimeOptions::default()
            },
        );
        for _ in 0..3 {
            next_store_runtime_message(&mut runtime).await;
        }
        let agent = AgentId::from_u128(591);
        let host = HostId::from_u128(592);
        runtime.process(Msg::Server(ServerMsg::Connected {
            local_host_id: Some(host),
        }));
        runtime.process(Msg::Server(ServerMsg::AgentUpserted {
            agent: claude_agent(agent, host),
        }));
        runtime.open_chat(agent);
        wait_for_store_runtime(&mut runtime, |runtime| {
            runtime
                .model()
                .chat(agent)
                .is_some_and(|chat| chat.state == ui_state::ChatState::Painted)
        })
        .await;

        let chat = runtime.model().chat(agent).expect("derived chat opens");
        assert!(!chat.live_only);
        assert_eq!(chat.persistence_error, None);
    }

    #[tokio::test]
    async fn unavailable_store_keeps_a_chat_live_only() {
        let directory = tempfile::tempdir().expect("tempdir");
        let obstruction = directory.path().join("not-a-directory");
        std::fs::write(&obstruction, b"file").expect("write obstruction");
        let mut runtime = Runtime::start(
            Box::new(|| Box::pin(std::future::pending())),
            RuntimeOptions {
                store_path: Some(obstruction.join("store.sqlite")),
                ..RuntimeOptions::default()
            },
        );
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(5), runtime.next_message())
                .await
                .expect("unavailable startup timed out");
        }

        let agent = Uuid::from_u128(801);
        let host = Uuid::from_u128(802);
        runtime.process(Msg::Server(ServerMsg::Connected {
            local_host_id: Some(host),
        }));
        runtime.process(Msg::Server(ServerMsg::AgentUpserted {
            agent: claude_agent(agent, host),
        }));
        runtime.open_chat(agent);
        tokio::time::timeout(Duration::from_secs(5), runtime.next_message())
            .await
            .expect("load unavailability timed out");

        let chat = runtime.model().chat(agent).expect("chat remains visible");
        assert!(chat.live_only);
        assert_eq!(chat.state, ui_state::ChatState::Painted);
        assert_eq!(chat.persistence_error, Some(store::StoreError::Io));
    }

    #[tokio::test]
    async fn store_result_from_a_retired_profile_is_discarded() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut runtime = a_runtime(directory.path().to_path_buf());
        let retired = runtime.shell_edge();
        runtime.switch_connector(
            Box::new(|| Box::pin(std::future::pending())),
            RuntimeOptions::default(),
        );
        retired
            .report(Msg::Store(StoreMsg::FleetChanged {
                profile: ProfileGeneration(0),
            }))
            .await
            .expect("shared channel remains live");
        tokio::task::yield_now().await;
        assert!(!runtime.drain(), "late result must not reach the reducer");
        assert_eq!(runtime.discarded_late_results(), 1);
        assert_eq!(runtime.discarded_late_kinds(), vec![LateResult::Command]);
    }

    /// Binary queue resources survive holding and cancellation, while the Model
    /// and its serialized replay agree. The caller takes ownership before the
    /// bounded cancellation outcome ages out of runtime retention.
    #[test]
    fn queue_runtime_restores_attachment_bytes_without_recording_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = a_runtime(dir.path().to_path_buf());
        let agent = Uuid::from_u128(57);
        let host = Uuid::from_u128(58);
        let now = Utc::now();
        for msg in [
            Msg::Server(ServerMsg::Connected {
                local_host_id: Some(host),
            }),
            Msg::Server(ServerMsg::AgentUpserted {
                agent: claude_agent(agent, host),
            }),
            Msg::Server(ServerMsg::HostsSynchronized),
            Msg::Server(ServerMsg::AgentsSynchronized),
            Msg::Stream {
                agent,
                event: StreamMsg::Opened { truncated: false },
            },
            Msg::Stream {
                agent,
                event: StreamMsg::ReplayComplete,
            },
            Msg::Stream {
                agent,
                event: StreamMsg::Batch {
                    at: now,
                    entries: vec![
                        ui_state::StreamEntry::observed(
                            1,
                            now,
                            serde_json::json!({"type":"amux.transcript_ready"}),
                        ),
                        ui_state::StreamEntry::observed(
                            2,
                            now,
                            serde_json::json!({"type":"user", "uuid":"00000000-0000-0000-0000-000000000001", "origin":{"kind":"human"}, "timestamp":now, "message":{"role":"user", "content":"work"}}),
                        ),
                    ],
                },
            },
        ] {
            update(&mut runtime.model, msg);
        }
        let attachment = ui_state::DraftAttachment::from_bytes(
            ArtifactKind::File,
            "notes.txt",
            "text/plain",
            b"private queue bytes".to_vec(),
        );
        let id = attachment.id.clone();
        runtime.dispatch(Command::Queue(ui_state::QueueCommand::Hold {
            agent,
            draft: ui_state::Draft {
                segments: vec![ui_state::DraftSegment::Text {
                    text: "read the attachment".into(),
                }],
                attachments: vec![attachment],
            },
        }));
        assert!(
            runtime.model().queued(agent).unwrap().draft.attachments[0]
                .bytes
                .is_none()
        );
        assert_eq!(
            serde_json::from_value::<Model>(serde_json::to_value(runtime.model()).unwrap())
                .unwrap(),
            *runtime.model()
        );
        let cancel = runtime.dispatch(Command::Queue(ui_state::QueueCommand::Cancel { agent }));
        assert!(matches!(
            runtime.model().finished_op(cancel).unwrap().outcome,
            OpOutcome::QueueCancelled { .. }
        ));
        let restored = runtime
            .queued_attachment_bytes(&id)
            .expect("caller restores the cancelled attachment");
        assert_eq!(&*restored, b"private queue bytes");
        for _ in 0..70 {
            runtime.dispatch(Command::Queue(ui_state::QueueCommand::Cancel { agent }));
        }
        assert!(
            runtime.queued_attachment_bytes(&id).is_none(),
            "unreferenced runtime resources are released"
        );
        assert_eq!(
            &*restored, b"private queue bytes",
            "the restored composer still owns its bytes"
        );
    }

    const INVARIANT_POLICY_CHILD: &str = "AMUX_INVARIANT_POLICY_CHILD";
    const INVARIANT_REPORT_DIR: &str = "AMUX_INVARIANT_REPORT_DIR";
    const INVARIANT_REPORT_GIT_SHA: &str = "AMUX_INVARIANT_REPORT_GIT_SHA";

    fn corrupt_with_orphan_stream(runtime: &mut Runtime) {
        let mut encoded = serde_json::to_value(&runtime.model).expect("serialize model");
        encoded["streams"][Uuid::from_u128(0xdead).to_string()] = serde_json::json!({
            "phase": { "stream_phase": "live" },
            "truncated": false
        });
        runtime.model = serde_json::from_value(encoded).expect("deserialize corrupt model");
    }

    fn report_paths(dir: &std::path::Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .expect("read report directory")
            .map(|entry| entry.expect("read report entry").path())
            .filter(|path| path.is_dir() && path.join("report.json").is_file())
            .collect()
    }

    #[test]
    fn the_msg_tap_sees_every_fold_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seen: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let mut runtime = a_runtime(dir.path().to_path_buf());
        let recorded = seen.clone();
        runtime.msg_tap = Some(Box::new(move |msg: &Msg| {
            recorded
                .lock()
                .expect("tap lock")
                .push(format!("{}", MsgLabel(msg)));
        }));

        // A drain batches an unknown number of Msgs, so the tap is the only
        // honest report of what was folded and in what order.
        runtime.process(Msg::Server(ServerMsg::Disconnected {
            reason: ui_state::DisconnectReason::ApplicationShutdown,
        }));
        runtime.process(Msg::Tick {
            now: DateTime::from_timestamp(1_754_697_600, 0).expect("fixture time"),
        });
        runtime.process(Msg::Server(ServerMsg::Disconnected {
            reason: ui_state::DisconnectReason::ApplicationShutdown,
        }));

        assert_eq!(
            *seen.lock().expect("tap lock"),
            vec!["server", "tick", "server"],
            "the tap sees each fold once, in fold order"
        );
    }

    /// The coarse shape of a Msg — enough to assert on order without
    /// pinning this test to the wording of any one variant.
    struct MsgLabel<'m>(&'m Msg);

    impl std::fmt::Display for MsgLabel<'_> {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(match self.0 {
                Msg::Server(_) => "server",
                Msg::Tick { .. } => "tick",
                _ => "other",
            })
        }
    }

    #[test]
    fn report_degrades_when_log_tail_is_not_utf8() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("amux.log");
        let mut runtime = a_runtime(dir.path().to_path_buf());
        std::fs::write(&log_path, b"valid line\n\xff").expect("write invalid UTF-8 log");
        let read_error = log_tail(&log_path, LOG_TAIL_BYTES)
            .expect_err("invalid UTF-8 must fail")
            .to_string();

        let report = runtime
            .report(DumpReason::Tripwire {
                detail: "invalid log fixture".to_string(),
            })
            .expect("the report still writes");

        assert!(report.join("report.json").is_file());
        assert!(!report.join("log.txt").exists());
        let header = crate::report::read_header(&report).expect("read degraded report header");
        assert_eq!(
            header.parts.log,
            crate::report::PartState::Absent {
                reason: format!(
                    "failed to read log tail from {}: {read_error}",
                    log_path.display()
                ),
            }
        );
        assert_eq!(header.parts.msgs, crate::report::PartState::Present);
    }

    fn run_invariant_policy_child(case: &str, fatal: Option<&str>) -> std::process::Output {
        let mut command =
            std::process::Command::new(std::env::current_exe().expect("current test executable"));
        command
            .arg("--exact")
            .arg("runtime::tests::invariant_policy_child")
            .arg("--nocapture")
            .env(INVARIANT_POLICY_CHILD, case);
        match fatal {
            Some(value) => {
                command.env("AMUX_INVARIANT_FATAL", value);
            }
            None => {
                command.env_remove("AMUX_INVARIANT_FATAL");
            }
        }
        command.output().expect("run invariant policy child")
    }

    /// Process-isolated because the fatal policy is controlled by a
    /// process-global environment variable. The parent tests below select a
    /// case on this otherwise inert helper.
    #[test]
    fn invariant_policy_child() {
        let Ok(case) = std::env::var(INVARIANT_POLICY_CHILD) else {
            return;
        };
        let configured_dir = std::env::var_os(INVARIANT_REPORT_DIR).map(PathBuf::from);
        let temporary_dir = configured_dir
            .is_none()
            .then(|| tempfile::tempdir().expect("tempdir"));
        let report_dir = configured_dir.clone().unwrap_or_else(|| {
            temporary_dir
                .as_ref()
                .expect("temporary report directory")
                .path()
                .to_path_buf()
        });
        std::fs::create_dir_all(&report_dir).expect("create report directory");
        let git_sha = std::env::var(INVARIANT_REPORT_GIT_SHA)
            .map(|sha| {
                assert!(
                    sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "{INVARIANT_REPORT_GIT_SHA} must be a full git sha"
                );
                &*Box::leak(sha.into_boxed_str())
            })
            .unwrap_or("test-sha");
        let mut runtime = a_runtime_with_git_sha(report_dir.clone(), git_sha);
        match case.as_str() {
            "nonfatal" => {
                corrupt_with_orphan_stream(&mut runtime);
                runtime.enforce_invariants();
                assert!(runtime.model().has_invariant_warning());
                let reports = report_paths(&report_dir);
                assert_eq!(reports.len(), 1, "one report for the new violation kind");
                let header =
                    crate::report::read_header(&reports[0]).expect("read invariant report header");
                assert!(matches!(header.kind, crate::report::ReportKind::Tripwire));
                assert!(
                    header
                        .detail
                        .as_deref()
                        .is_some_and(|detail| detail.starts_with("invariant:"))
                );
                assert!(matches!(
                    header.parts.frame,
                    crate::report::PartState::Absent { .. }
                ));
                assert!(matches!(
                    header.parts.trace,
                    crate::report::PartState::Absent { .. }
                ));
                assert_eq!(header.parts.msgs, crate::report::PartState::Present);
                assert!(matches!(
                    header.parts.daemon,
                    crate::report::PartState::Absent { .. }
                ));
                assert_eq!(header.parts.log, crate::report::PartState::Present);
                if configured_dir.is_some() {
                    println!("written report: {}", reports[0].display());
                    print!(
                        "{}",
                        std::fs::read_to_string(reports[0].join("report.json"))
                            .expect("read written report header")
                    );
                }
                let replayed = crate::recorder::replay_msgs(&reports[0].join("msgs.jsonl"))
                    .expect("replay invariant report");
                assert!(
                    replayed.has_invariant_warning(),
                    "replayed invariant report must retain the sticky warning"
                );

                runtime.enforce_invariants();
                assert_eq!(
                    report_paths(&report_dir).len(),
                    1,
                    "persistent corruption stays throttled once per kind"
                );
            }
            "fatal" => {
                corrupt_with_orphan_stream(&mut runtime);
                runtime.enforce_invariants();
            }
            "coherent" => {
                runtime.enforce_invariants();
                assert!(!runtime.model().has_invariant_warning());
                assert!(report_paths(&report_dir).is_empty());
            }
            other => panic!("unknown invariant policy child case: {other}"),
        }
    }

    #[test]
    fn invariant_policy_is_nonfatal_by_default_and_for_other_values() {
        for fatal in [None, Some("0")] {
            let output = run_invariant_policy_child("nonfatal", fatal);
            assert!(
                output.status.success(),
                "non-fatal child failed (AMUX_INVARIANT_FATAL={fatal:?}):\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn invariant_policy_fatal_opt_in_panics_with_details() {
        let output = run_invariant_policy_child("fatal", Some("1"));
        assert!(
            !output.status.success(),
            "fatal child unexpectedly succeeded"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("model invariants violated: stream for"),
            "fatal panic omitted violation details:\n{stderr}"
        );
    }

    #[test]
    fn invariant_policy_leaves_a_coherent_model_unmarked_and_undumped() {
        let output = run_invariant_policy_child("coherent", Some("1"));
        assert!(
            output.status.success(),
            "coherent child failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn dispatch_stamps_fresh_observation_time_before_reducing_the_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut runtime = a_runtime(dir.path().to_path_buf());
        let agent = Uuid::from_u128(57);
        let host = Uuid::from_u128(58);
        for msg in [
            Msg::Server(ServerMsg::Connected {
                local_host_id: Some(host),
            }),
            Msg::Server(ServerMsg::AgentUpserted {
                agent: claude_agent(agent, host),
            }),
            Msg::Server(ServerMsg::HostsSynchronized),
            Msg::Server(ServerMsg::AgentsSynchronized),
            Msg::Stream {
                agent,
                event: StreamMsg::Opened { truncated: false },
            },
            Msg::Stream {
                agent,
                event: StreamMsg::ReplayComplete,
            },
        ] {
            update(&mut runtime.model, msg);
        }
        let old = DateTime::from_timestamp(1_754_697_600, 0).expect("valid fixture time");
        runtime.observe_now(old);

        let before = Utc::now();
        let op = runtime.dispatch(Command::Claude(
            ui_state::claude::ClaudeCommand::SendPrompt {
                agent,
                text: "fresh dispatch".to_string(),
            },
        ));
        let after = Utc::now();

        let observed = runtime.model().now().expect("dispatch observes time");
        assert!(
            observed > old,
            "dispatch replaces the stale observation time"
        );
        assert!(
            (before..=after).contains(&observed),
            "dispatch observation {observed} must be bounded by {before} and {after}"
        );
        let echo = runtime
            .model()
            .claude(agent)
            .expect("Claude layer")
            .pending_echoes()
            .iter()
            .find(|echo| echo.op == op)
            .expect("dispatched prompt echo");
        assert_eq!(
            echo.at,
            Some(observed),
            "the command reducer sees the refreshed dispatch clock"
        );
    }

    /// The panic-report slot is process-global, so the tests that install
    /// into it take turns. Poison-tolerant: a failing test leaves the slot
    /// behind, not the rest of the suite blocked.
    static PANIC_REPORT_TESTS: StdMutex<()> = StdMutex::new(());

    fn panic_report_test_turn() -> std::sync::MutexGuard<'static, ()> {
        PANIC_REPORT_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn a_profile_entry(label: &str, socket: PathBuf) -> ProfileEntry {
        ProfileEntry {
            id: ProfileId(Uuid::from_u128(91)),
            label: label.to_string(),
            email: None,
            status: "ready".to_string(),
            socket,
        }
    }

    /// A panic after a switch belongs to the profile the shell moved to: the
    /// slot the hook reads carries the new selection's report directory and
    /// its recorder, never the retired ones.
    #[tokio::test]
    async fn switching_reregisters_the_panic_report_for_the_new_profile() {
        let _turn = panic_report_test_turn();
        *lock_panic_report() = None;
        let retired_dir = tempfile::tempdir().expect("tempdir");
        let selected_dir = tempfile::tempdir().expect("tempdir");
        let mut runtime = a_runtime(retired_dir.path().to_path_buf());
        let retired_recorder = runtime.recorder.clone();
        runtime.install_panic_report();

        let selected_log = selected_dir.path().join("amux.log");
        std::fs::write(&selected_log, "selected profile log\n").expect("write test log");
        runtime.switch_in_place(
            &a_profile_entry("Work", selected_dir.path().join("work.sock")),
            RuntimeOptions {
                report_dir: Some(selected_dir.path().to_path_buf()),
                log_path: Some(selected_log.clone()),
                ..RuntimeOptions::default()
            },
        );

        let context = lock_panic_report()
            .clone()
            .expect("the switch leaves a panic-report context installed");
        assert_eq!(
            context.report_dir,
            selected_dir.path(),
            "a panic reports into the selected profile's report directory"
        );
        assert_eq!(context.log_path.as_deref(), Some(selected_log.as_path()));
        assert!(
            Arc::ptr_eq(&context.recorder, &runtime.recorder),
            "the panic hook snapshots the new runtime's recorder"
        );
        assert!(
            !Arc::ptr_eq(&context.recorder, &retired_recorder),
            "the retired profile's recorder is no longer what a panic would report"
        );
        *lock_panic_report() = None;
    }

    /// A shell that never installed a panic-report context — an embedding
    /// host with its own hook — does not acquire one by switching.
    #[tokio::test]
    async fn switching_installs_no_panic_report_for_a_shell_that_had_none() {
        let _turn = panic_report_test_turn();
        *lock_panic_report() = None;
        let retired_dir = tempfile::tempdir().expect("tempdir");
        let selected_dir = tempfile::tempdir().expect("tempdir");
        let mut runtime = a_runtime(retired_dir.path().to_path_buf());

        runtime.switch_in_place(
            &a_profile_entry("Work", selected_dir.path().join("work.sock")),
            RuntimeOptions {
                report_dir: Some(selected_dir.path().to_path_buf()),
                ..RuntimeOptions::default()
            },
        );

        assert!(
            lock_panic_report().is_none(),
            "switching profiles does not install a panic hook context the shell never asked for"
        );
    }

    /// The panic hook's report path, exercised WITHOUT panicking: install,
    /// call `write_panic_report`, and the report directory holds a bundle whose
    /// header carries the panic reason.
    #[test]
    fn write_panic_report_writes_a_report_after_install() {
        let _turn = panic_report_test_turn();
        let dir = tempfile::tempdir().expect("tempdir");
        let mut runtime = a_runtime(dir.path().to_path_buf());
        runtime.observe_now(DateTime::from_timestamp(1_754_697_600, 0).expect("valid time"));
        runtime.dispatch(Command::DeleteAgent {
            agent: Uuid::from_u128(7),
        });
        runtime.install_panic_report();

        write_panic_report("test");

        let report = std::fs::read_dir(dir.path())
            .expect("read report dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|path| path.is_dir() && path.join("report.json").is_file())
            .expect("a report exists");
        let header = crate::report::read_header(&report).expect("read panic report");
        assert_eq!(header.kind, crate::report::ReportKind::Panic);
        assert_eq!(header.detail.as_deref(), Some("test"));
        let contents =
            std::fs::read_to_string(report.join("msgs.jsonl")).expect("read recorder snapshot");
        assert!(
            contents.lines().count() > 1,
            "the recorded Msgs ride along in the report"
        );
    }

    #[tokio::test]
    async fn codex_runtime_stays_coherent_from_upsert_through_replay_to_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut runtime = a_runtime(dir.path().to_path_buf());
        let agent = Uuid::from_u128(77);
        let host = Uuid::from_u128(88);

        process_and_assert_coherent(
            &mut runtime,
            Msg::Server(ServerMsg::Connected {
                local_host_id: Some(host),
            }),
        );
        process_and_assert_coherent(
            &mut runtime,
            Msg::Server(ServerMsg::AgentUpserted {
                agent: codex_agent(agent, host),
            }),
        );
        process_and_assert_coherent(
            &mut runtime,
            Msg::Stream {
                agent,
                event: StreamMsg::Opened { truncated: false },
            },
        );
        process_and_assert_coherent(
            &mut runtime,
            Msg::Stream {
                agent,
                event: StreamMsg::Batch {
                    at: DateTime::from_timestamp(1_754_697_601, 0).expect("valid fixture time"),
                    entries: vec![StreamEntry::observed(
                        1,
                        DateTime::from_timestamp(1_754_697_601, 0).expect("valid fixture time"),
                        serde_json::json!({"type":"amux.codex_ready"}),
                    )],
                },
            },
        );
        assert_eq!(
            runtime.model().agent(agent).unwrap().attention,
            ui_state::Attention::Unknown
        );
        assert_eq!(
            ui_state::codex::phase(runtime.model(), agent),
            ui_state::codex::CodexPhase::Replaying
        );
        assert_eq!(
            ui_state::codex::send_gate(runtime.model(), agent),
            ui_state::codex::SendGate::Replaying
        );

        process_and_assert_coherent(
            &mut runtime,
            Msg::Stream {
                agent,
                event: StreamMsg::ReplayComplete,
            },
        );
        assert_eq!(
            runtime.model().agent(agent).unwrap().attention,
            ui_state::Attention::Idle
        );
        assert_eq!(
            ui_state::codex::phase(runtime.model(), agent),
            ui_state::codex::CodexPhase::Idle
        );
        assert_eq!(
            ui_state::codex::send_gate(runtime.model(), agent),
            ui_state::codex::SendGate::Ready
        );
    }

    #[tokio::test]
    async fn resumed_codex_rows_stay_unknown_until_replay_completes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut runtime = a_runtime(dir.path().to_path_buf());
        let agent = Uuid::from_u128(177);
        let host = Uuid::from_u128(188);

        for msg in [
            Msg::Server(ServerMsg::Connected {
                local_host_id: Some(host),
            }),
            Msg::Server(ServerMsg::AgentUpserted {
                agent: codex_agent(agent, host),
            }),
            Msg::Stream {
                agent,
                event: StreamMsg::Opened { truncated: false },
            },
            Msg::Stream {
                agent,
                event: StreamMsg::Batch {
                    at: DateTime::from_timestamp(1_754_697_601, 0).expect("valid fixture time"),
                    entries: vec![
                        StreamEntry::observed(
                            1,
                            DateTime::from_timestamp(1_754_697_601, 0).expect("valid fixture time"),
                            serde_json::json!({"type":"amux.codex_ready"}),
                        ),
                        StreamEntry::observed(
                            2,
                            DateTime::from_timestamp(1_754_697_601, 0).expect("valid fixture time"),
                            serde_json::json!({
                                "type":"turn/started",
                                "turn":{"id":"resumed-turn","status":"inProgress"}
                            }),
                        ),
                        StreamEntry::observed(
                            3,
                            DateTime::from_timestamp(1_754_697_601, 0).expect("valid fixture time"),
                            serde_json::json!({
                                "type":"turn/completed",
                                "turn":{"id":"resumed-turn","status":"completed"}
                            }),
                        ),
                    ],
                },
            },
        ] {
            process_and_assert_coherent(&mut runtime, msg);
        }

        let layer = runtime.model().codex(agent).expect("folded Codex layer");
        assert!(
            layer.entry_count() > 0,
            "resumed replay must carry folded rows"
        );
        assert_eq!(
            layer.attention(),
            ui_state::Attention::NeedsYou {
                why: ui_state::Why::Finished
            }
        );
        assert_eq!(
            runtime.model().agent(agent).unwrap().attention,
            ui_state::Attention::Unknown
        );
        assert_eq!(
            ui_state::codex::send_gate(runtime.model(), agent),
            ui_state::codex::SendGate::Replaying
        );

        process_and_assert_coherent(
            &mut runtime,
            Msg::Stream {
                agent,
                event: StreamMsg::ReplayComplete,
            },
        );
        assert_eq!(
            runtime.model().agent(agent).unwrap().attention,
            ui_state::Attention::NeedsYou {
                why: ui_state::Why::Finished
            }
        );
        assert_eq!(
            ui_state::codex::send_gate(runtime.model(), agent),
            ui_state::codex::SendGate::Ready
        );
    }
}

//! The runtime shell: owns `amux::Client`, executes Effects on tokio tasks,
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
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use amux::{
    AgentId, AgentIdentifier, ArtifactId, ArtifactKind, ArtifactRef, Client, ClientError,
    CreateAgentRequest, HostId, ProtocolError, SendInputRequest, SessionCloseReason,
    SubscribeSessionEvent, SubscribeSessionRequest, claude_io, codex_io,
};
use amux_artifacts::{ArtifactMeta, Cache, FetchError, StoreError, SystemClock};
use chrono::{DateTime, Utc};
use futures_util::FutureExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::codex::CodexInput;
use crate::effect::{DumpReason, Effect, InputPayload};
use crate::model::{Model, StructuredProtocol};
use crate::msg::{
    Command, DisconnectReason, Msg, OpError, OpId, OpOutcome, ServerMsg, StreamCloseReason,
    StreamEntry, StreamMsg,
};
use crate::recorder::{DEFAULT_RECORDER_CAPACITY, Recorder};
use crate::report::{
    FrameCapture, LOG_TAIL_BYTES, ReplayVerdict, ReportDraft, ReportKind, ReportParts,
    ReportWriter, log_tail,
};
use crate::update::{NOT_CONNECTED_ERROR, update};

/// Reducer build identity, stamped into reports.
pub const BUILD: &str = concat!("amux-ui/", env!("CARGO_PKG_VERSION"));

/// One ordered Msg stream; producers wait when it is full (lossless).
const MSG_CHANNEL_CAPACITY: usize = 1024;

/// Msgs folded per `next()` wakeup before control returns to the caller, so
/// a flooding stream batches to a frame budget and never starves input.
const DRAIN_BUDGET: usize = 256;

const RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_millis(250);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(4);
const SUBSCRIPTION_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Structured entries coalesced into one `Msg::Stream(Batch)` — the recorded
/// Msg is the batch, so replay is independent of arrival timing.
const MAX_STREAM_BATCH: usize = 256;

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

pub struct RuntimeOptions {
    /// The daemon's own host id (read from the local device identity);
    /// enters the Model via `ServerMsg::Connected`.
    pub local_host_id: Option<HostId>,
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
    msg_tx: mpsc::Sender<Msg>,
    msg_rx: mpsc::Receiver<Msg>,
    client: Arc<StdMutex<Option<Client>>>,
    tasks: Vec<JoinHandle<()>>,
    /// Live per-agent stream tasks (shell resource bookkeeping only; the
    /// semantic stream state lives in the Model).
    streams: HashMap<AgentId, JoinHandle<()>>,
    report_dir: Option<PathBuf>,
    log_path: Option<PathBuf>,
    git_sha: &'static str,
    report_extras: Option<ReportExtrasProvider>,
    msg_tap: Option<MsgTap>,
    artifact_cache: Option<Result<Arc<Cache>, String>>,
    attachment_opener: AttachmentOpener,
    queued_bytes: HashMap<ArtifactId, Arc<[u8]>>,
    /// Violation kinds already reported this session: invariant logs and
    /// reports are throttled to once per kind so a persistent incoherence
    /// cannot fill the report directory.
    reported_violations: HashSet<&'static str>,
}

impl Runtime {
    /// Start the shell with a connector that dials (and re-dials) the
    /// daemon.
    pub fn start(connector: Connector, options: RuntimeOptions) -> Self {
        let model = Model::default();
        let recorder = Arc::new(StdMutex::new(Recorder::new(
            options.recorder_capacity,
            &model,
        )));
        let (msg_tx, msg_rx) = mpsc::channel(MSG_CHANNEL_CAPACITY);
        let client = Arc::new(StdMutex::new(None));
        let artifact_cache = options.artifact_cache.map(|root| {
            Cache::open(root, options.artifact_cache_bound, Arc::new(SystemClock))
                .map(Arc::new)
                .map_err(|error| error.to_string())
        });

        let subscription_status_provider = options.subscription_status_provider;
        let connection_task = tokio::spawn(connection_task(
            connector,
            msg_tx.clone(),
            client.clone(),
            options.local_host_id,
            subscription_status_provider.clone(),
        ));

        Self {
            model,
            recorder,
            msg_tx,
            msg_rx,
            client,
            tasks: vec![connection_task],
            streams: HashMap::new(),
            report_dir: options.report_dir,
            log_path: options.log_path,
            git_sha: options.git_sha,
            report_extras: options.report_extras,
            msg_tap: options.msg_tap,
            artifact_cache,
            attachment_opener: options.attachment_opener,
            queued_bytes: HashMap::new(),
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

    /// Await the next Msg, then fold everything already pending (up to a
    /// frame budget). Returns false when the shell has shut down.
    pub async fn next(&mut self) -> bool {
        if !self.next_message().await {
            return false;
        }
        self.drain();
        true
    }

    /// Fold one input so embedders can observe every resolved operation before
    /// bounded outcome retention evicts it. Callers own their batching cadence.
    pub async fn next_message(&mut self) -> bool {
        let Some(msg) = self.msg_rx.recv().await else {
            return false;
        };
        self.process(msg);
        true
    }

    /// Fold every immediately-available Msg (bounded by the frame budget);
    /// returns true if anything was folded.
    pub fn drain(&mut self) -> bool {
        let mut folded = false;
        for _ in 0..DRAIN_BUDGET {
            match self.msg_rx.try_recv() {
                Ok(msg) => {
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
        let Some(report_dir) = self.report_dir.clone() else {
            return;
        };
        let _ = PANIC_REPORT.set(PanicReportContext {
            recorder: self.recorder.clone(),
            report_dir,
            log_path: self.log_path.clone(),
            git_sha: self.git_sha,
            report_extras: self.report_extras.clone(),
        });
    }

    fn process(&mut self, msg: Msg) {
        if let Msg::Command {
            command:
                Command::Queue(
                    crate::QueueCommand::Hold { draft, .. }
                    | crate::QueueCommand::Replace { draft, .. },
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
        let effects = update(&mut self.model, msg);
        self.enforce_invariants();
        for effect in effects {
            self.run_effect(effect);
        }
        self.enforce_invariants();
        // Binary payloads are shell resources, absent from reducer state and reports.
        // Cancellation keeps them through the bounded returned-draft outcome so a
        // caller can resend metadata while restoring its own composer assets.
        if !self.queued_bytes.is_empty() {
            let mut retained = HashSet::new();
            for queue in self.model.queues.values() {
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
                let tx = self.msg_tx.clone();
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
                let tx = self.msg_tx.clone();
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
                let tx = self.msg_tx.clone();
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
            Effect::FetchDiff { op, agent, id } => {
                let client = self.client.lock().expect("client mutex poisoned").clone();
                let cache = clone_artifact_cache(&self.artifact_cache);
                let tx = self.msg_tx.clone();
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
                let tx = self.msg_tx.clone();
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
                let tx = self.msg_tx.clone();
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
                let tx = self.msg_tx.clone();
                if let Some(stale) = self.streams.insert(
                    agent,
                    tokio::spawn(stream_task(client, agent, protocol, tail, tx)),
                ) {
                    stale.abort();
                }
            }
            Effect::CloseStream { agent } => {
                if let Some(task) = self.streams.remove(&agent) {
                    task.abort();
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

impl Drop for Runtime {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        for task in self.streams.values() {
            task.abort();
        }
    }
}

struct PanicReportContext {
    recorder: Arc<StdMutex<Recorder>>,
    report_dir: PathBuf,
    log_path: Option<PathBuf>,
    git_sha: &'static str,
    report_extras: Option<ReportExtrasProvider>,
}

/// The context registered for panic reports: set once by
/// [`Runtime::install_panic_report`] and read inside the panic hook.
static PANIC_REPORT: OnceLock<PanicReportContext> = OnceLock::new();
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
    let Some(context) = PANIC_REPORT.get() else {
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
        | Command::Claude(_)
        | Command::Codex(_)
        | Command::SendPromptWithAttachments { .. }
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

/// Store every live draft, then deliver one native input carrying all pins.
/// No input is sent if any put fails.
pub async fn execute_put_then_send<C: AttachmentClient + ?Sized>(
    client: &C,
    op: OpId,
    agent: AgentId,
    puts: Vec<crate::attachments::DraftAttachment>,
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

    let (io_protocol, payload) = match input {
        InputPayload::Claude {
            expected_seq,
            intent,
            ..
        } => (
            crate::claude::PROTOCOL.to_string(),
            claude_io::encode_pty_transcript_v1_input(claude_io::ClaudePtyTranscriptV1Input {
                expected_seq,
                intent,
            })
            .into(),
        ),
        InputPayload::Codex { payload } => (
            crate::codex::PROTOCOL.to_string(),
            codex_io::encode_codex_sdk_v1_input(codex_wire_input(payload)).into(),
        ),
    };
    match client
        .send_input(SendInputRequest {
            agent: AgentIdentifier::Id(agent),
            input_id: op.0.as_bytes().to_vec(),
            io_protocol,
            payload,
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

fn codex_wire_input(input: CodexInput) -> codex_io::CodexSdkV1Input {
    match input {
        CodexInput::Command { name, args } => codex_io::CodexSdkV1Input::Command { name, args },
        CodexInput::SetModel { model } => codex_io::CodexSdkV1Input::SetModel { model },
        CodexInput::SetEffort { effort } => codex_io::CodexSdkV1Input::SetEffort { effort },
        CodexInput::SetPreset { approval, sandbox } => {
            codex_io::CodexSdkV1Input::SetPreset { approval, sandbox }
        }
        CodexInput::UserTurn { input } => codex_io::CodexSdkV1Input::UserTurn { input },
        CodexInput::Steer { turn_id, input } => codex_io::CodexSdkV1Input::Steer { turn_id, input },
        CodexInput::Interrupt { turn_id } => codex_io::CodexSdkV1Input::Interrupt { turn_id },
        CodexInput::ApprovalDecision {
            request_id,
            decision,
        } => codex_io::CodexSdkV1Input::ApprovalDecision {
            request_id,
            decision,
        },
    }
}

fn map_client_error(
    error: &ClientError,
    current_name: Option<&str>,
    puts: &[crate::attachments::DraftAttachment],
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

fn platform_open_command(_meta: &ArtifactMeta, _path: &Path) -> io::Result<std::process::Command> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        // Content-addressed blobs intentionally have no filename extension,
        // so LaunchServices classifies even valid PNG bytes as public.data.
        // Preview identifies every image format accepted by the composer from
        // the bytes themselves once it is selected explicitly.
        if _meta.kind == ArtifactKind::Image {
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
        command.arg(_path);
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
    let input = codex_wire_input(input);
    let payload = codex_io::encode_codex_sdk_v1_input(input);
    match client
        .send_input(SendInputRequest {
            agent: AgentIdentifier::Id(agent),
            input_id,
            io_protocol: crate::codex::PROTOCOL.to_string(),
            payload: payload.into(),
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
    intent: claude_io::Intent,
    retry_stale: bool,
    pin: Vec<String>,
) -> OpOutcome {
    let mut expected_seq = expected_seq;
    let mut attempts = 0;
    loop {
        let payload =
            claude_io::encode_pty_transcript_v1_input(claude_io::ClaudePtyTranscriptV1Input {
                expected_seq,
                intent: intent.clone(),
            });
        match client
            .send_input(SendInputRequest {
                agent: AgentIdentifier::Id(agent),
                input_id: input_id.clone(),
                io_protocol: crate::claude::PROTOCOL.to_string(),
                payload: payload.into(),
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
    tx: mpsc::Sender<Msg>,
    shared_client: Arc<StdMutex<Option<Client>>>,
    local_host_id: Option<HostId>,
    subscription_status_provider: Option<SubscriptionStatusProvider>,
) {
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
                if send_msg(&tx, Msg::Server(ServerMsg::Disconnected { reason }))
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
        )
        .await;
        *shared_client.lock().expect("client mutex poisoned") = None;

        let Some(reason) = session_end else {
            // The Msg channel closed: the Runtime is gone.
            return;
        };
        if send_msg(&tx, Msg::Server(ServerMsg::Disconnected { reason }))
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
    tx: &mpsc::Sender<Msg>,
    local_host_id: Option<HostId>,
    subscription_status_provider: Option<&SubscriptionStatusProvider>,
) -> Option<DisconnectReason> {
    let mut hosts_stream = match client.subscribe_hosts().await {
        Ok(stream) => stream,
        Err(error) => return Some(disconnect_reason(&error)),
    };
    let mut agents_stream = match client.subscribe_agents().await {
        Ok(stream) => stream,
        Err(error) => return Some(disconnect_reason(&error)),
    };

    if send_msg(tx, Msg::Server(ServerMsg::Connected { local_host_id }))
        .await
        .is_err()
    {
        return None;
    }
    let mut subscription_required = subscription_status_provider.map(|provider| provider());
    if let Some(required) = subscription_required
        && send_msg(
            tx,
            Msg::Server(ServerMsg::CloudSubscriptionStatus { required }),
        )
        .await
        .is_err()
    {
        return None;
    }
    let mut subscription_poll = subscription_status_provider
        .map(|_| tokio::time::interval(SUBSCRIPTION_STATUS_POLL_INTERVAL));
    if let Some(poll) = subscription_poll.as_mut() {
        poll.tick().await;
    }

    loop {
        let event = tokio::select! {
            event = hosts_stream.recv() => match event {
                Ok(amux::HostEvent::HostUpdated { host }) => ServerMsg::HostUpserted { host },
                Ok(amux::HostEvent::HostRemoved { id }) => ServerMsg::HostRemoved { id },
                Ok(amux::HostEvent::SnapshotComplete) => ServerMsg::HostsSynchronized,
                Err(error) => return Some(disconnect_reason(&error)),
            },
            event = agents_stream.recv() => match event {
                Ok(amux::AgentEvent::AgentUp { agent })
                | Ok(amux::AgentEvent::AgentUpdated { agent }) => {
                    ServerMsg::AgentUpserted { agent }
                }
                Ok(amux::AgentEvent::AgentDown { agent_id }) => {
                    ServerMsg::AgentRemoved { id: agent_id }
                }
                Ok(amux::AgentEvent::SnapshotComplete) => ServerMsg::AgentsSynchronized,
                Ok(amux::AgentEvent::HostInventory { host_id, agent_ids }) => ServerMsg::HostInventory { host_id, agent_ids },
                Err(error) => return Some(disconnect_reason(&error)),
            },
            _ = maybe_interval_tick(&mut subscription_poll), if subscription_poll.is_some() => {
                let required = subscription_status_provider.expect("poll requires provider")();
                if subscription_required == Some(required) {
                    continue;
                }
                subscription_required = Some(required);
                ServerMsg::CloudSubscriptionStatus { required }
            },
        };
        if send_msg(tx, Msg::Server(event)).await.is_err() {
            return None;
        }
    }
}

async fn maybe_interval_tick(interval: &mut Option<tokio::time::Interval>) {
    if let Some(interval) = interval {
        interval.tick().await;
    }
}

async fn send_msg(tx: &mpsc::Sender<Msg>, msg: Msg) -> Result<(), ()> {
    // Bounded lossless send: the producer waits, never drops.
    tx.send(msg).await.map_err(|_| ())
}

/// Subscribe an agent's structured stream and forward coalesced batches.
/// Always terminates with a `Closed` Msg (unless the Runtime is gone), so
/// the Model never holds a stream open that no task backs.
async fn stream_task(
    client: Option<Client>,
    agent: AgentId,
    protocol: StructuredProtocol,
    tail: u64,
    tx: mpsc::Sender<Msg>,
) {
    if let Some(reason) = pump_structured_stream(client, agent, protocol, tail, &tx).await {
        let _ = send_msg(
            &tx,
            Msg::Stream {
                agent,
                event: StreamMsg::Closed { reason },
            },
        )
        .await;
    }
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
    tx: &mpsc::Sender<Msg>,
) -> Option<StreamCloseReason> {
    let Some(client) = client else {
        return Some(StreamCloseReason::TransportError {
            message: NOT_CONNECTED_ERROR.to_string(),
        });
    };
    let args = match protocol {
        StructuredProtocol::Claude => {
            claude_io::encode_pty_transcript_v1_args(claude_io::ClaudePtyTranscriptV1Args {
                terminal_size: None,
                replay_query: Some(claude_io::ClaudePtyTranscriptV1ReplayQuery::Tail {
                    count: tail,
                }),
            })
        }
        StructuredProtocol::Codex => codex_io::encode_codex_sdk_v1_args(codex_io::CodexSdkV1Args {
            replay_query: Some(codex_io::CodexSdkV1ReplayQuery::Tail { count: tail }),
        }),
        // The subscription policy never opens this protocol, because no
        // layer here folds it. Reaching this arm means the policy changed
        // without the fold: say which protocol is missing rather than
        // subscribing with arguments nobody can read.
        StructuredProtocol::ClaudeSdk => {
            return Some(StreamCloseReason::InternalError {
                detail: format!(
                    "{} has no client-side fold in this build",
                    protocol.as_str()
                ),
            });
        }
    };
    let mut session = match client
        .subscribe_session(SubscribeSessionRequest {
            agent: AgentIdentifier::Id(agent),
            io_protocol: protocol.as_str().to_string(),
            args: args.map(Into::into),
        })
        .await
    {
        Ok(session) => session,
        Err(error) => return Some(stream_close_from_client_error(&error)),
    };

    let mut sent_opened = false;
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
                flush_stream_batch(tx, agent, &mut sent_opened, &mut batch).await?;
            }
            Some(Ok(SubscribeSessionEvent::Opened)) => {}
            Some(Ok(SubscribeSessionEvent::Output { payload })) => {
                match decode_structured_entry(protocol, &payload) {
                    Ok(entry) => batch.push(entry),
                    Err(reason) => {
                        flush_stream_batch(tx, agent, &mut sent_opened, &mut batch).await?;
                        return Some(reason);
                    }
                }
            }
            Some(Ok(SubscribeSessionEvent::ReplayComplete { .. })) => {
                flush_stream_batch(tx, agent, &mut sent_opened, &mut batch).await?;
                send_msg(
                    tx,
                    Msg::Stream {
                        agent,
                        event: StreamMsg::ReplayComplete,
                    },
                )
                .await
                .ok()?;
            }
            Some(Ok(SubscribeSessionEvent::Closed { reason })) => {
                flush_stream_batch(tx, agent, &mut sent_opened, &mut batch).await?;
                return Some(stream_close_from_session(reason));
            }
            Some(Err(error)) => {
                flush_stream_batch(tx, agent, &mut sent_opened, &mut batch).await?;
                return Some(stream_close_from_client_error(&error));
            }
        }
    }
}

/// Send the pending batch (and, first time, the `Opened` Msg carrying the
/// truncation fact: replay beginning past seq 1 means history was bounded
/// at the source). Returns `None` when the Runtime is gone.
async fn flush_stream_batch(
    tx: &mpsc::Sender<Msg>,
    agent: AgentId,
    sent_opened: &mut bool,
    batch: &mut Vec<StreamEntry>,
) -> Option<()> {
    if !*sent_opened {
        let truncated = batch.first().is_some_and(|entry| entry.seq > 1);
        send_msg(
            tx,
            Msg::Stream {
                agent,
                event: StreamMsg::Opened { truncated },
            },
        )
        .await
        .ok()?;
        *sent_opened = true;
    }
    if batch.is_empty() {
        return Some(());
    }
    let entries = std::mem::take(batch);
    send_msg(
        tx,
        Msg::Stream {
            agent,
            event: StreamMsg::Batch {
                at: Utc::now(),
                entries,
            },
        },
    )
    .await
    .ok()?;
    Some(())
}

fn decode_structured_entry(
    protocol: StructuredProtocol,
    payload: &[u8],
) -> Result<StreamEntry, StreamCloseReason> {
    let output = match protocol {
        StructuredProtocol::Claude => claude_io::decode_pty_transcript_v1_output(payload),
        StructuredProtocol::ClaudeSdk => {
            return Err(StreamCloseReason::InternalError {
                detail: format!(
                    "{} has no client-side fold in this build",
                    protocol.as_str()
                ),
            });
        }
        StructuredProtocol::Codex => {
            let output = codex_io::decode_codex_sdk_v1_output(payload).map_err(|error| {
                StreamCloseReason::InternalError {
                    detail: error.to_string(),
                }
            })?;
            let payload = serde_json::from_slice(&output.payload).map_err(|error| {
                StreamCloseReason::InternalError {
                    detail: format!("structured entry {} is not JSON: {error}", output.seq),
                }
            })?;
            return Ok(StreamEntry {
                seq: output.seq,
                payload,
            });
        }
    }
    .map_err(|error| StreamCloseReason::InternalError {
        detail: error.to_string(),
    })?;
    let payload = serde_json::from_slice(&output.payload).map_err(|error| {
        StreamCloseReason::InternalError {
            detail: format!("structured entry {} is not JSON: {error}", output.seq_id),
        }
    })?;
    Ok(StreamEntry {
        seq: output.seq_id,
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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_opens_extensionless_images_with_preview() {
        let meta = ArtifactMeta {
            id: amux_artifacts::id_of(b"png"),
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

    fn codex_agent(agent: AgentId, host: HostId) -> amux::Agent {
        amux::Agent {
            id: agent,
            host_id: host,
            name: Some("projection-test".to_string()),
            command: "codex".to_string(),
            working_dir: PathBuf::from("/work"),
            kind: amux::AgentKind::Codex,
            readonly: false,
            args: Vec::new(),
            created_at: DateTime::from_timestamp(1_754_697_600, 0).expect("valid fixture time"),
            parent: None,
            working_on: None,
        }
    }

    fn claude_agent(agent: AgentId, host: HostId) -> amux::Agent {
        amux::Agent {
            id: agent,
            host_id: host,
            name: Some("dispatch-clock-test".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/work"),
            kind: amux::AgentKind::Claude {
                driver: amux::ClaudeDriver::Pty,
            },
            readonly: false,
            args: Vec::new(),
            created_at: DateTime::from_timestamp(1_754_697_600, 0).expect("valid fixture time"),
            parent: None,
            working_on: None,
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
            msg_tx,
            msg_rx,
            client: Arc::new(StdMutex::new(None)),
            tasks: Vec::new(),
            streams: HashMap::new(),
            report_dir: Some(report_dir),
            log_path: Some(log_path),
            git_sha,
            report_extras: None,
            msg_tap: None,
            artifact_cache: None,
            attachment_opener: Arc::new(open_with_platform_viewer),
            queued_bytes: HashMap::new(),
            reported_violations: HashSet::new(),
        }
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
                        crate::StreamEntry {
                            seq: 1,
                            payload: serde_json::json!({"type":"amux.transcript_ready"}),
                        },
                        crate::StreamEntry {
                            seq: 2,
                            payload: serde_json::json!({"type":"user", "uuid":"00000000-0000-0000-0000-000000000001", "origin":{"kind":"human"}, "timestamp":now, "message":{"role":"user", "content":"work"}}),
                        },
                    ],
                },
            },
        ] {
            update(&mut runtime.model, msg);
        }
        let attachment = crate::DraftAttachment::from_bytes(
            amux::ArtifactKind::File,
            "notes.txt",
            "text/plain",
            b"private queue bytes".to_vec(),
        );
        let id = attachment.id.clone();
        runtime.dispatch(Command::Queue(crate::QueueCommand::Hold {
            agent,
            draft: crate::Draft {
                segments: vec![crate::DraftSegment::Text {
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
        let cancel = runtime.dispatch(Command::Queue(crate::QueueCommand::Cancel { agent }));
        assert!(matches!(
            runtime.model().finished_op(cancel).unwrap().outcome,
            OpOutcome::QueueCancelled { .. }
        ));
        let restored = runtime
            .queued_attachment_bytes(&id)
            .expect("caller restores the cancelled attachment");
        assert_eq!(&*restored, b"private queue bytes");
        for _ in 0..70 {
            runtime.dispatch(Command::Queue(crate::QueueCommand::Cancel { agent }));
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
        runtime.model.streams.insert(
            Uuid::from_u128(0xdead),
            crate::model::StreamState {
                phase: crate::model::StreamPhase::Live,
                truncated: false,
            },
        );
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
            reason: crate::msg::DisconnectReason::ApplicationShutdown,
        }));
        runtime.process(Msg::Tick {
            now: DateTime::from_timestamp(1_754_697_600, 0).expect("fixture time"),
        });
        runtime.process(Msg::Server(ServerMsg::Disconnected {
            reason: crate::msg::DisconnectReason::ApplicationShutdown,
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
        let op = runtime.dispatch(Command::Claude(crate::claude::ClaudeCommand::SendPrompt {
            agent,
            text: "fresh dispatch".to_string(),
        }));
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

    /// The panic hook's report path, exercised WITHOUT panicking: install,
    /// call `write_panic_report`, and the report directory holds a bundle whose
    /// header carries the panic reason.
    #[test]
    fn write_panic_report_writes_a_report_after_install() {
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
                    entries: vec![StreamEntry {
                        seq: 1,
                        payload: serde_json::json!({"type":"amux.codex_ready"}),
                    }],
                },
            },
        );
        assert_eq!(
            runtime.model().agent(agent).unwrap().attention,
            crate::Attention::Unknown
        );
        assert_eq!(
            crate::codex::phase(runtime.model(), agent),
            crate::codex::CodexPhase::Replaying
        );
        assert_eq!(
            crate::codex::send_gate(runtime.model(), agent),
            crate::codex::SendGate::Replaying
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
            crate::Attention::Idle
        );
        assert_eq!(
            crate::codex::phase(runtime.model(), agent),
            crate::codex::CodexPhase::Idle
        );
        assert_eq!(
            crate::codex::send_gate(runtime.model(), agent),
            crate::codex::SendGate::Ready
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
                        StreamEntry {
                            seq: 1,
                            payload: serde_json::json!({"type":"amux.codex_ready"}),
                        },
                        StreamEntry {
                            seq: 2,
                            payload: serde_json::json!({
                                "type":"turn/started",
                                "turn":{"id":"resumed-turn","status":"inProgress"}
                            }),
                        },
                        StreamEntry {
                            seq: 3,
                            payload: serde_json::json!({
                                "type":"turn/completed",
                                "turn":{"id":"resumed-turn","status":"completed"}
                            }),
                        },
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
            crate::Attention::NeedsYou {
                why: crate::Why::Finished
            }
        );
        assert_eq!(
            runtime.model().agent(agent).unwrap().attention,
            crate::Attention::Unknown
        );
        assert_eq!(
            crate::codex::send_gate(runtime.model(), agent),
            crate::codex::SendGate::Replaying
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
            crate::Attention::NeedsYou {
                why: crate::Why::Finished
            }
        );
        assert_eq!(
            crate::codex::send_gate(runtime.model(), agent),
            crate::codex::SendGate::Ready
        );
    }
}

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
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use amux::{
    AgentId, AgentIdentifier, Client, ClientError, CreateAgentRequest, HostId, ProtocolError,
    SendInputRequest, SessionCloseReason, SubscribeSessionEvent, SubscribeSessionRequest,
    claude_io, codex_io,
};
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
    FrameCapture, ReplayVerdict, ReportDraft, ReportKind, ReportParts, ReportWriter, log_tail,
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

    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Dispatch a command; the outcome returns as state (a finished op).
    pub fn dispatch(&mut self, command: Command) -> OpId {
        let op = OpId(Uuid::new_v4());
        self.process(Msg::Tick { now: Utc::now() });
        self.process(Msg::Command { op, command });
        op
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
        let Some(msg) = self.msg_rx.recv().await else {
            return false;
        };
        self.process(msg);
        self.drain();
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
        let log = self
            .log_path
            .as_deref()
            .map(|path| log_tail(path, REPORT_LOG_TAIL_BYTES))
            .transpose()?
            .flatten();
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
        lock_recorder(&self.recorder).record(&msg);
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
                            error: OpError {
                                message: NOT_CONNECTED_ERROR.to_string(),
                                auth_required: false,
                                subscription_required: false,
                            },
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
                            error: OpError {
                                message: NOT_CONNECTED_ERROR.to_string(),
                                auth_required: false,
                                subscription_required: false,
                            },
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

const REPORT_LOG_TAIL_BYTES: usize = 64 * 1024;

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
    let log = context
        .log_path
        .as_deref()
        .and_then(|path| log_tail(path, REPORT_LOG_TAIL_BYTES).ok().flatten());
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
        Command::Claude(_) => OpOutcome::Error {
            error: OpError {
                message: "input command routed to the RPC executor".to_string(),
                auth_required: false,
                subscription_required: false,
            },
        },
        Command::Codex(_) => OpOutcome::Error {
            error: OpError {
                message: "input command routed to the RPC executor".to_string(),
                auth_required: false,
                subscription_required: false,
            },
        },
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
    match payload {
        InputPayload::Claude {
            expected_seq,
            intent,
            retry_stale,
        } => execute_claude_input(client, agent, input_id, expected_seq, intent, retry_stale).await,
        InputPayload::Codex { payload } => {
            execute_codex_input(client, agent, input_id, payload).await
        }
    }
}

async fn execute_codex_input(
    client: &Client,
    agent: AgentId,
    input_id: Vec<u8>,
    input: CodexInput,
) -> OpOutcome {
    let input = match input {
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
    };
    let payload = codex_io::encode_codex_sdk_v1_input(input);
    match client
        .send_input(SendInputRequest {
            agent: AgentIdentifier::Id(agent),
            input_id,
            io_protocol: crate::codex::PROTOCOL.to_string(),
            payload: payload.into(),
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
                    error: OpError {
                        message: format!("{STALE_INPUT_ERROR} ({error})"),
                        auth_required: false,
                        subscription_required: false,
                    },
                };
            }
            Err(error) => return op_error_outcome(&error),
        }
    }
}

fn op_error_outcome(error: &ClientError) -> OpOutcome {
    OpOutcome::Error {
        error: OpError {
            message: error.to_string(),
            auth_required: is_auth_error(error),
            subscription_required: is_subscription_error(error),
        },
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
            git_sha: "test-sha",
            report_extras: None,
            reported_violations: HashSet::new(),
        }
    }

    const INVARIANT_POLICY_CHILD: &str = "AMUX_INVARIANT_POLICY_CHILD";

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
        let dir = tempfile::tempdir().expect("tempdir");
        let mut runtime = a_runtime(dir.path().to_path_buf());
        match case.as_str() {
            "nonfatal" => {
                corrupt_with_orphan_stream(&mut runtime);
                runtime.enforce_invariants();
                assert!(runtime.model().has_invariant_warning());
                let reports = report_paths(dir.path());
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
                let replayed = crate::recorder::replay_msgs(&reports[0].join("msgs.jsonl"))
                    .expect("replay invariant report");
                assert!(
                    replayed.has_invariant_warning(),
                    "replayed invariant report must retain the sticky warning"
                );

                runtime.enforce_invariants();
                assert_eq!(
                    report_paths(dir.path()).len(),
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
                assert!(report_paths(dir.path()).is_empty());
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

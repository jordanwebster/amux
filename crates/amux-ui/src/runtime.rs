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
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use amux::{
    AgentId, AgentIdentifier, Client, ClientError, CreateAgentRequest, HostId, ProtocolError,
    SessionCloseReason, SubscribeSessionEvent, SubscribeSessionRequest, claude_io,
};
use chrono::{DateTime, Utc};
use futures_util::FutureExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::effect::{DumpReason, Effect};
use crate::model::Model;
use crate::msg::{
    Command, DisconnectReason, Msg, OpError, OpId, OpOutcome, ServerMsg, StreamCloseReason,
    StreamEntry, StreamMsg,
};
use crate::recorder::{DEFAULT_RECORDER_CAPACITY, Recorder};
use crate::update::{NOT_CONNECTED_ERROR, update};

/// Reducer build identity, stamped into recorder dumps.
pub const BUILD: &str = concat!("amux-ui/", env!("CARGO_PKG_VERSION"));

/// One ordered Msg stream; producers wait when it is full (lossless).
const MSG_CHANNEL_CAPACITY: usize = 1024;

/// Msgs folded per `next()` wakeup before control returns to the caller, so
/// a flooding stream batches to a frame budget and never starves input.
const DRAIN_BUDGET: usize = 256;

const RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_millis(250);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(4);

/// Structured entries coalesced into one `Msg::Stream(Batch)` — the recorded
/// Msg is the batch, so replay is independent of arrival timing.
const MAX_STREAM_BATCH: usize = 256;

/// Why a connection attempt failed.
#[derive(Clone, Debug)]
pub struct ConnectFailure {
    pub message: String,
    pub auth_required: bool,
}

/// Future returned by a [`Connector`].
pub type ConnectFuture = Pin<Box<dyn Future<Output = Result<Client, ConnectFailure>> + Send>>;

/// How the shell (re)establishes the daemon connection. Provided by the
/// embedding client (the CLI knows how to spawn the daemon); called again
/// after every disconnect.
pub type Connector = Box<dyn FnMut() -> ConnectFuture + Send>;

pub struct RuntimeOptions {
    /// The daemon's own host id (read from the local device identity);
    /// enters the Model via `ServerMsg::Connected`.
    pub local_host_id: Option<HostId>,
    /// Where recorder dumps land. `None` disables dumping.
    pub dump_dir: Option<PathBuf>,
    pub recorder_capacity: usize,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            local_host_id: None,
            dump_dir: None,
            recorder_capacity: DEFAULT_RECORDER_CAPACITY,
        }
    }
}

/// One Runtime per client process, one Model per daemon connection.
/// Renderers access the Model in-process by borrow after [`Runtime::next`] /
/// [`Runtime::drain`].
pub struct Runtime {
    model: Model,
    /// Shared with the process panic hook ([`Runtime::install_panic_dump`])
    /// so a panic can dump the ring after terminal restore. The fold is
    /// single-threaded — contention is nil; the mutex exists for the hook.
    recorder: Arc<StdMutex<Recorder>>,
    msg_tx: mpsc::Sender<Msg>,
    msg_rx: mpsc::Receiver<Msg>,
    client: Arc<StdMutex<Option<Client>>>,
    tasks: Vec<JoinHandle<()>>,
    /// Live per-agent stream tasks (shell resource bookkeeping only; the
    /// semantic stream state lives in the Model).
    streams: HashMap<AgentId, JoinHandle<()>>,
    dump_dir: Option<PathBuf>,
    /// Violation kinds already dumped this session: release-mode invariant
    /// dumps are throttled to once per kind so a persistent incoherence
    /// cannot fill the dump directory.
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

        let connection_task = tokio::spawn(connection_task(
            connector,
            msg_tx.clone(),
            client.clone(),
            options.local_host_id,
        ));

        Self {
            model,
            recorder,
            msg_tx,
            msg_rx,
            client,
            tasks: vec![connection_task],
            streams: HashMap::new(),
            dump_dir: options.dump_dir,
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

    /// Dump the recorder ring for diagnosis. Local-only; never uploaded.
    pub fn dump(&mut self, reason: DumpReason) -> io::Result<PathBuf> {
        let Some(dir) = self.dump_dir.clone() else {
            return Err(io::Error::other("no dump directory configured"));
        };
        lock_recorder(&self.recorder).write_dump(&dir, reason, BUILD, &dump_stamp())
    }

    /// Register this Runtime's recorder with the process-global panic-dump
    /// slot read by [`write_panic_dump`]. Call once after start; a Runtime
    /// without a dump directory registers nothing.
    pub fn install_panic_dump(&self) {
        let Some(dir) = self.dump_dir.clone() else {
            return;
        };
        let _ = PANIC_DUMP.set((self.recorder.clone(), dir, BUILD));
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
    /// Debug builds panic (an incoherent Model is a reducer bug to fix);
    /// release builds dump once per violation kind and keep folding — a
    /// degraded fleet beats a dead client.
    fn enforce_invariants(&mut self) {
        let violations = self.model.check_invariants();
        if violations.is_empty() {
            return;
        }
        if cfg!(debug_assertions) {
            let details: Vec<String> = violations.iter().map(ToString::to_string).collect();
            panic!("model invariants violated: {}", details.join("; "));
        }
        for violation in violations {
            if self.reported_violations.insert(violation.kind()) {
                tracing::warn!(%violation, "model invariant violated; dumping recorder ring");
                if let Err(error) = self.dump(DumpReason::Tripwire {
                    detail: format!("invariant: {violation}"),
                }) {
                    tracing::warn!(%violation, %error, "failed to write invariant dump");
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
                            },
                        },
                    };
                    let _ = tx.send(Msg::OpResult { op, outcome }).await;
                });
            }
            Effect::OpenStream { agent, tail } => {
                let client = self.client.lock().expect("client mutex poisoned").clone();
                let tx = self.msg_tx.clone();
                if let Some(stale) = self
                    .streams
                    .insert(agent, tokio::spawn(stream_task(client, agent, tail, tx)))
                {
                    stale.abort();
                }
            }
            Effect::CloseStream { agent } => {
                if let Some(task) = self.streams.remove(&agent) {
                    task.abort();
                }
            }
            Effect::RequestDump { reason } => {
                if let Err(error) = self.dump(reason.clone()) {
                    tracing::warn!(?reason, %error, "failed to write requested ui dump");
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

/// The recorder registered for panic dumps: set once by
/// [`Runtime::install_panic_dump`], read by [`write_panic_dump`] inside the
/// panic hook.
static PANIC_DUMP: OnceLock<(Arc<StdMutex<Recorder>>, PathBuf, &'static str)> = OnceLock::new();

/// Lock the recorder even when poisoned: a panic mid-record must not block
/// the panic hook from dumping. The ring holds pre-serialized lines, so the
/// worst a poisoned lock can cost is the newest entry.
fn lock_recorder(recorder: &StdMutex<Recorder>) -> std::sync::MutexGuard<'_, Recorder> {
    recorder
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Unique dump-file stamp: zero-padded wall millis (name order is age
/// order, which the pruner relies on) plus pid.
fn dump_stamp() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    format!("{millis:013}-{}", std::process::id())
}

/// Best-effort recorder dump from the process panic hook, called AFTER
/// terminal restore (a dump is worthless if writing it destroys the panic
/// report). Returns quietly on every failure — nothing may panic or block
/// here; the process is already dying.
pub fn write_panic_dump(detail: &str) {
    let Some((recorder, dir, build)) = PANIC_DUMP.get() else {
        return;
    };
    let _ = lock_recorder(recorder).write_dump(
        dir,
        DumpReason::Panic {
            detail: detail.to_string(),
        },
        build,
        &dump_stamp(),
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
    }
}

fn op_error_outcome(error: &ClientError) -> OpOutcome {
    OpOutcome::Error {
        error: OpError {
            message: error.to_string(),
            auth_required: is_auth_error(error),
        },
    }
}

fn is_auth_error(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Protocol(ProtocolError::InvalidCredentials)
    )
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
) {
    let mut backoff = RECONNECT_BACKOFF_INITIAL;
    loop {
        let client = match connector().await {
            Ok(client) => client,
            Err(failure) => {
                let reason = if failure.auth_required {
                    DisconnectReason::AuthenticationRequired
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
        let session_end = pump_inventory(&client, &tx, local_host_id).await;
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
        };
        if send_msg(tx, Msg::Server(event)).await.is_err() {
            return None;
        }
    }
}

async fn send_msg(tx: &mpsc::Sender<Msg>, msg: Msg) -> Result<(), ()> {
    // Bounded lossless send: the producer waits, never drops.
    tx.send(msg).await.map_err(|_| ())
}

/// Subscribe an agent's structured stream and forward coalesced batches.
/// Always terminates with a `Closed` Msg (unless the Runtime is gone), so
/// the Model never holds a stream open that no task backs.
async fn stream_task(client: Option<Client>, agent: AgentId, tail: u64, tx: mpsc::Sender<Msg>) {
    if let Some(reason) = pump_structured_stream(client, agent, tail, &tx).await {
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
    tail: u64,
    tx: &mpsc::Sender<Msg>,
) -> Option<StreamCloseReason> {
    let Some(client) = client else {
        return Some(StreamCloseReason::TransportError {
            message: NOT_CONNECTED_ERROR.to_string(),
        });
    };
    let args = claude_io::encode_pty_transcript_v1_args(claude_io::ClaudePtyTranscriptV1Args {
        terminal_size: None,
        replay_query: Some(claude_io::ClaudePtyTranscriptV1ReplayQuery::Tail { count: tail }),
    });
    let mut session = match client
        .subscribe_session(SubscribeSessionRequest {
            agent: AgentIdentifier::Id(agent),
            io_protocol: claude_io::PTY_TRANSCRIPT_V1.to_string(),
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
                match decode_structured_entry(&payload) {
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

fn decode_structured_entry(payload: &[u8]) -> Result<StreamEntry, StreamCloseReason> {
    let output = claude_io::decode_pty_transcript_v1_output(payload).map_err(|error| {
        StreamCloseReason::InternalError {
            detail: error.to_string(),
        }
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
    } else {
        StreamCloseReason::TransportError {
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Runtime with no shell tasks: Msgs enter only through the direct
    /// fold surface (`dispatch`/`observe_now`), which is all the panic-dump
    /// path needs. No tokio runtime required.
    fn a_runtime(dump_dir: PathBuf) -> Runtime {
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
            dump_dir: Some(dump_dir),
            reported_violations: HashSet::new(),
        }
    }

    /// The panic hook's dump path, exercised WITHOUT panicking: install,
    /// call `write_panic_dump`, and the dump directory holds a bundle whose
    /// header carries the panic reason.
    #[test]
    fn write_panic_dump_writes_a_dump_after_install() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut runtime = a_runtime(dir.path().to_path_buf());
        runtime.observe_now(DateTime::from_timestamp(1_754_697_600, 0).expect("valid time"));
        runtime.dispatch(Command::DeleteAgent {
            agent: Uuid::from_u128(7),
        });
        runtime.install_panic_dump();

        write_panic_dump("test");

        let dump = std::fs::read_dir(dir.path())
            .expect("read dump dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("ui-dump-") && name.ends_with(".jsonl"))
            })
            .expect("a dump file exists");
        let contents = std::fs::read_to_string(&dump).expect("read dump");
        let header = contents.lines().next().expect("header line");
        assert!(
            header.contains("panic"),
            "dump header must carry the panic reason: {header}"
        );
        assert!(
            contents.lines().count() > 1,
            "the recorded Msgs ride along in the dump"
        );
    }
}

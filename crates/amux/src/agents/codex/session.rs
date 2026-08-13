use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use codex_sdk::{
    ApprovalResponse, Codex, CodexConfig, DaemonMode, DynamicToolCallResponse, Error as CodexError,
    InputItem, RequestId, Thread, ThreadConfig, ThreadEvent, TurnEvent, connect_daemon,
    connect_socket, daemon_socket_path, ensure_daemon_with_fallback,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};
use tokio::task::AbortHandle;
use uuid::Uuid;

use super::io::{self, CodexSdkV1Input};
use crate::agents::{
    AGENT_TYPE_CODEX, AgentBackend, CodexInput, CreateAgentRequest, LocalAgentNameSource,
    PtyHandle, StopPolicy, StructuredLogSource, spawn_pty_agent,
};
use crate::suspend::SuspendedAgent;

// Codex streams are delta-heavy and this is their sole elastic/replay buffer.
// 8K rows covers several ordinary turns while staying bounded per agent.
const STRUCTURED_LOG_RETENTION: usize = 8192;
const RECONNECT_BACKOFF: [Duration; 5] = [
    Duration::from_millis(100),
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

/// One lazily initialized, reconnectable Codex app-server connection per host.
pub(crate) struct CodexClient {
    private_socket: PathBuf,
    connection: Mutex<Option<Arc<CodexConnection>>>,
}

struct CodexConnection {
    client: Codex,
    mode: &'static str,
    socket_path: PathBuf,
    _daemon: DaemonMode,
}

impl CodexClient {
    pub(crate) fn new(private_socket: PathBuf) -> Self {
        Self {
            private_socket,
            connection: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn private_socket(&self) -> &Path {
        &self.private_socket
    }

    async fn connection(&self) -> Result<Arc<CodexConnection>> {
        let mut slot = self.connection.lock().await;
        if let Some(connection) = slot.as_ref()
            && !connection.client.is_closed()
        {
            return Ok(connection.clone());
        }
        // A dead shared connection must not remain sticky. Dropping its daemon
        // guard here also lets a supervised daemon be recreated.
        slot.take();

        let codex_home = codex_home()?;
        let daemon = ensure_daemon_with_fallback(&codex_home, &self.private_socket)
            .await
            .context("failed to ensure Codex app-server daemon")?;
        let mode = daemon_mode_name(&daemon);
        let config = CodexConfig {
            client_name: "amux".to_string(),
            client_title: Some("amux".to_string()),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            record_io: capture_dir().map(|dir| dir.join("io.jsonl")),
            ..CodexConfig::default()
        };
        let socket_path = match &daemon {
            DaemonMode::Existing => {
                let home = tokio::fs::canonicalize(&codex_home)
                    .await
                    .context("failed to resolve CODEX_HOME")?;
                daemon_socket_path(&home)
            }
            DaemonMode::Spawned(process) | DaemonMode::Private(process) => {
                process.socket_path().to_path_buf()
            }
            DaemonMode::PrivateExisting(socket_path) => socket_path.clone(),
        };
        let daemon_exit = match &daemon {
            DaemonMode::Spawned(process) | DaemonMode::Private(process) => {
                Some(process.exit_token())
            }
            DaemonMode::Existing | DaemonMode::PrivateExisting(_) => None,
        };
        let client = match &daemon {
            DaemonMode::Existing | DaemonMode::Spawned(_) => {
                connect_daemon(&codex_home, config).await
            }
            DaemonMode::Private(process) => connect_socket(process.socket_path(), config).await,
            DaemonMode::PrivateExisting(socket_path) => connect_socket(socket_path, config).await,
        }
        .context("failed to connect to Codex app-server daemon")?;
        if let Some(exited) = daemon_exit {
            let watched_client = client.clone();
            tokio::spawn(async move {
                exited.cancelled().await;
                watched_client.close().await;
            });
        }

        let connection = Arc::new(CodexConnection {
            client,
            mode,
            socket_path,
            _daemon: daemon,
        });
        *slot = Some(connection.clone());
        Ok(connection)
    }
}

fn codex_home() -> Result<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or_else(|| anyhow!("CODEX_HOME or HOME is required for Codex agents"))
}

fn daemon_mode_name(mode: &DaemonMode) -> &'static str {
    match mode {
        DaemonMode::Existing => "existing",
        DaemonMode::Spawned(_) => "spawned-well-known",
        DaemonMode::Private(_) => "spawned-private",
        DaemonMode::PrivateExisting(_) => "existing-private",
    }
}

fn capture_dir() -> Option<PathBuf> {
    std::env::var_os("AMUX_CODEX_CAPTURE_DIR").map(PathBuf::from)
}

fn codex_log_source() -> StructuredLogSource {
    let Some(dir) = capture_dir() else {
        return StructuredLogSource::new(STRUCTURED_LOG_RETENTION);
    };
    match StructuredLogSource::recording(STRUCTURED_LOG_RETENTION, &dir.join("rows.jsonl")) {
        Ok(source) => source,
        Err(error) => {
            tracing::warn!(%error, path = %dir.display(), "failed to enable Codex row capture");
            StructuredLogSource::new(STRUCTURED_LOG_RETENTION)
        }
    }
}

async fn rename_thread(client: &Codex, agent_id: Uuid, thread_id: &str, name: &str) {
    if let Err(error) = client.rename_thread(thread_id, name).await {
        tracing::warn!(%agent_id, %thread_id, %error, "failed to rename Codex thread");
    }
}

#[derive(Debug, Clone, Copy)]
enum PendingRequestKind {
    Approval,
    ToolCall,
}

enum PendingReply {
    Approval(ApprovalResponse),
    ToolCall { success: bool },
}

#[derive(Clone)]
struct CodexLive {
    client: Codex,
    thread: Thread,
    socket_path: PathBuf,
}

struct CodexAttached {
    thread_id: String,
    daemon_mode: Option<String>,
    live: Option<CodexLive>,
    active_turn_id: Option<String>,
    pending: HashMap<RequestId, PendingRequestKind>,
}

struct CodexPty {
    handle: PtyHandle,
    epoch: u64,
}

struct CodexRuntime {
    desired_name: Option<String>,
    attached: Option<CodexAttached>,
    startup_error: Option<String>,
    ingest_abort: Option<AbortHandle>,
    pty: Option<CodexPty>,
    next_pty_epoch: u64,
}

pub(crate) struct CodexSession {
    agent_id: Uuid,
    name: Option<String>,
    working_dir: PathBuf,
    model: Option<String>,
    approval_policy: Option<String>,
    sandbox_policy: Option<String>,
    resume_thread_id: Option<String>,
    created_at: DateTime<Utc>,
    log_source: StructuredLogSource,
    shared_client: Arc<CodexClient>,
    runtime: Arc<StdMutex<CodexRuntime>>,
    stop_tx: watch::Sender<bool>,
    started: bool,
}

impl CodexSession {
    pub(crate) fn new(req: &CreateAgentRequest, shared_client: Arc<CodexClient>) -> Self {
        let (model, approval_policy, sandbox_policy, resume_thread_id) = match &req.agent_type {
            crate::agents::AgentType::Codex {
                model,
                approval_policy,
                sandbox_policy,
                resume_thread_id,
            } => (
                model.clone(),
                approval_policy.clone(),
                sandbox_policy.clone(),
                resume_thread_id.clone(),
            ),
            _ => unreachable!("CodexSession requires AgentType::Codex"),
        };
        let (stop_tx, _) = watch::channel(false);
        let attached = resume_thread_id.as_ref().map(|thread_id| CodexAttached {
            thread_id: thread_id.clone(),
            daemon_mode: None,
            live: None,
            active_turn_id: None,
            pending: HashMap::new(),
        });
        Self {
            agent_id: req.agent_id,
            name: req.name.clone(),
            working_dir: req.working_dir.clone(),
            model,
            approval_policy,
            sandbox_policy,
            resume_thread_id,
            created_at: Utc::now(),
            log_source: codex_log_source(),
            shared_client,
            runtime: Arc::new(StdMutex::new(CodexRuntime {
                desired_name: req.name.clone(),
                attached,
                startup_error: None,
                ingest_abort: None,
                pty: None,
                next_pty_epoch: 0,
            })),
            stop_tx,
            started: false,
        }
    }

    pub(crate) fn from_suspended(
        req: &CreateAgentRequest,
        shared_client: Arc<CodexClient>,
        daemon_mode: String,
        created_at: DateTime<Utc>,
    ) -> Self {
        let session = Self::new(req, shared_client);
        {
            let mut runtime = session
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if let Some(attached) = runtime.attached.as_mut() {
                attached.daemon_mode = Some(daemon_mode);
            }
        }
        Self {
            created_at,
            ..session
        }
    }

    fn thread_config(&self) -> Result<ThreadConfig> {
        let cwd = self
            .working_dir
            .to_str()
            .ok_or_else(|| anyhow!("Codex cwd must be valid UTF-8"))?
            .to_string();
        let approval_policy = self
            .approval_policy
            .as_ref()
            .map(|value| serde_json::from_value(Value::String(value.clone())))
            .transpose()
            .context("invalid Codex approval_policy")?;
        let sandbox = self
            .sandbox_policy
            .as_ref()
            .map(|value| serde_json::from_value(Value::String(value.clone())))
            .transpose()
            .context("invalid Codex sandbox_policy")?;
        Ok(ThreadConfig {
            cwd: Some(cwd),
            model: self.model.clone(),
            approval_policy,
            sandbox,
            ..ThreadConfig::default()
        })
    }

    fn start_task(&self, stop_rx: watch::Receiver<bool>) -> Result<tokio::task::JoinHandle<()>> {
        let thread_config = self.thread_config()?;
        let shared_client = self.shared_client.clone();
        let runtime = self.runtime.clone();
        let log_source = self.log_source.clone();
        let resume_thread_id = self.resume_thread_id.clone();
        let agent_id = self.agent_id;
        let handle = tokio::spawn(run_ingest_supervisor(
            agent_id,
            shared_client,
            runtime.clone(),
            log_source,
            thread_config,
            resume_thread_id,
            stop_rx,
        ));
        runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .ingest_abort = Some(handle.abort_handle());
        Ok(handle)
    }

    fn schedule_remote_rename(&self, name: String) {
        let target = {
            let runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            runtime.attached.as_ref().and_then(|attached| {
                attached
                    .live
                    .as_ref()
                    .map(|live| (live.client.clone(), attached.thread_id.clone()))
            })
        };
        if let Some((client, thread_id)) = target {
            let agent_id = self.agent_id;
            tokio::spawn(async move {
                rename_thread(&client, agent_id, &thread_id, &name).await;
            });
        }
    }

    fn input_target(&self) -> CodexInputTarget {
        CodexInputTarget {
            runtime: self.runtime.clone(),
            log_source: self.log_source.clone(),
        }
    }

    fn ensure_pty(&self) -> Result<PtyHandle> {
        let (handle, exit_handle, epoch) = {
            let mut runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if let Some(pty) = runtime.pty.as_ref() {
                return Ok(pty.handle.clone());
            }
            let attached = runtime.attached.as_ref().ok_or_else(|| {
                anyhow!("Codex raw session is not ready: thread_id is not available yet")
            })?;
            let live = attached.live.as_ref().ok_or_else(|| {
                anyhow!("Codex raw session is unavailable until reconnect succeeds")
            })?;
            std::os::unix::net::UnixStream::connect(&live.socket_path).with_context(|| {
                format!(
                    "Codex raw TUI app-server socket is unavailable: {}",
                    live.socket_path.display()
                )
            })?;
            let args = vec![
                "resume".to_string(),
                attached.thread_id.clone(),
                "--remote".to_string(),
                format!("unix://{}", live.socket_path.display()),
            ];
            let (handle, exit_handle) = spawn_pty_agent(
                self.agent_id,
                "codex",
                &args,
                &self.working_dir,
                &[],
                &[],
                None,
            )
            .context("failed to spawn Codex raw TUI")?;
            let epoch = runtime.next_pty_epoch;
            runtime.next_pty_epoch = runtime.next_pty_epoch.wrapping_add(1);
            runtime.pty = Some(CodexPty {
                handle: handle.clone(),
                epoch,
            });
            (handle, exit_handle, epoch)
        };

        let runtime = self.runtime.clone();
        tokio::spawn(async move {
            let _ = exit_handle.await;
            let mut state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
            if state.pty.as_ref().is_some_and(|pty| pty.epoch == epoch) {
                state.pty = None;
            }
        });
        Ok(handle)
    }
}

async fn run_ingest_supervisor(
    agent_id: Uuid,
    shared_client: Arc<CodexClient>,
    runtime: Arc<StdMutex<CodexRuntime>>,
    log_source: StructuredLogSource,
    thread_config: ThreadConfig,
    mut thread_id: Option<String>,
    mut stop_rx: watch::Receiver<bool>,
) {
    let mut retry = 0_usize;
    // Capture-rig fault injection: close only this SDK transport once. The
    // daemon and other processes remain untouched.
    let mut capture_drop_connection =
        std::env::var_os("AMUX_CODEX_CAPTURE_DROP_CONNECTION_ONCE").is_some();
    loop {
        if *stop_rx.borrow() {
            break;
        }

        let (connection, thread, mut events) =
            match attach_thread(&shared_client, &thread_config, &mut thread_id).await {
                Ok(attached) => attached,
                Err(error) => {
                    let message = error.to_string();
                    set_runtime_error(&runtime, message.clone());
                    write_reconnect_error(&log_source, &message).await;
                    if wait_for_retry(&mut stop_rx, retry).await {
                        break;
                    }
                    retry = (retry + 1).min(RECONNECT_BACKOFF.len() - 1);
                    continue;
                }
            };
        let id = thread.id().to_string();

        let desired_name = {
            let mut state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
            state.startup_error = None;
            state.attached = Some(CodexAttached {
                thread_id: id.clone(),
                daemon_mode: Some(connection.mode.to_string()),
                live: Some(CodexLive {
                    client: connection.client.clone(),
                    thread: thread.clone(),
                    socket_path: connection.socket_path.clone(),
                }),
                active_turn_id: None,
                pending: HashMap::new(),
            });
            state.desired_name.clone()
        };
        if let Some(name) = desired_name {
            rename_thread(&connection.client, agent_id, &id, &name).await;
        }
        log_source.write(json!({"type": "amux.codex_ready"})).await;
        if capture_drop_connection {
            capture_drop_connection = false;
            connection.client.clone().close().await;
        }
        retry = 0;

        let boundary = loop {
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        break None;
                    }
                }
                next = events.next() => match next {
                    Ok(Some(event)) => ingest_event(&runtime, &log_source, event).await,
                    Ok(None) => break Some("connection_lost"),
                    Err(CodexError::ThreadQueueOverflow(_)) => break Some("queue_overflow"),
                    Err(_) => break Some("event_stream_error"),
                }
            }
        };

        let pending = mark_disconnected(&runtime);
        resolve_pending(&log_source, pending, boundary.unwrap_or("session_stopped")).await;
        let Some(reason) = boundary else {
            break;
        };
        log_source
            .write(json!({"type": "amux.codex_gap", "reason": reason}))
            .await;
    }
}

/// Connect, attach the persistent thread, and take its continuous event stream.
///
/// `thread_id` is both the thread to resume (absent means "start a new one")
/// and the identity to remember: a started thread is recorded before the event
/// stream is taken so a later attempt resumes it instead of orphaning it.
async fn attach_thread(
    shared_client: &CodexClient,
    thread_config: &ThreadConfig,
    thread_id: &mut Option<String>,
) -> Result<(Arc<CodexConnection>, Thread, codex_sdk::ThreadEventStream)> {
    let connection = shared_client.connection().await?;
    let thread = match thread_id.as_deref() {
        Some(thread_id) => {
            connection
                .client
                .resume_thread(thread_id, thread_config.clone())
                .await
        }
        None => connection.client.start_thread(thread_config.clone()).await,
    }?;
    *thread_id = Some(thread.id().to_string());
    let events = thread.events().await?;
    Ok((connection, thread, events))
}

async fn wait_for_retry(stop_rx: &mut watch::Receiver<bool>, retry: usize) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(RECONNECT_BACKOFF[retry]) => false,
        _ = stop_rx.changed() => true,
    }
}

/// Apply `update` to the attached thread state, if a thread is attached.
fn update_attached(runtime: &StdMutex<CodexRuntime>, update: impl FnOnce(&mut CodexAttached)) {
    if let Some(attached) = runtime
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .attached
        .as_mut()
    {
        update(attached);
    }
}

fn set_runtime_error(runtime: &Arc<StdMutex<CodexRuntime>>, message: String) {
    let mut state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
    state.startup_error = Some(message);
    if let Some(attached) = state.attached.as_mut() {
        attached.live = None;
        attached.active_turn_id = None;
    }
}

fn mark_disconnected(runtime: &Arc<StdMutex<CodexRuntime>>) -> Vec<RequestId> {
    let mut state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(attached) = state.attached.as_mut() else {
        return Vec::new();
    };
    attached.live = None;
    attached.active_turn_id = None;
    attached.pending.drain().map(|(id, _)| id).collect()
}

async fn write_reconnect_error(log_source: &StructuredLogSource, message: &str) {
    log_source
        .write(json!({
            "type": "amux.codex_reconnect_error",
            "error": {"message": message},
        }))
        .await;
}

async fn resolve_pending(log_source: &StructuredLogSource, pending: Vec<RequestId>, reason: &str) {
    for request_id in pending {
        write_resolution(log_source, &request_id, reason).await;
    }
}

fn raw_row(event: &ThreadEvent) -> Value {
    let mut row = match &event.params {
        Value::Object(params) => params.clone(),
        params => serde_json::Map::from_iter([("params".to_string(), params.clone())]),
    };
    row.insert("type".to_string(), Value::String(event.method.clone()));
    Value::Object(row)
}

async fn ingest_event(
    runtime: &Arc<StdMutex<CodexRuntime>>,
    log_source: &StructuredLogSource,
    event: ThreadEvent,
) {
    log_source.write(raw_row(&event)).await;
    match &event.event {
        TurnEvent::TurnStarted { turn } => {
            update_attached(runtime, |attached| {
                attached.active_turn_id = Some(turn.id.clone());
            });
        }
        TurnEvent::TurnCompleted { turn } => {
            update_attached(runtime, |attached| {
                if attached.active_turn_id.as_deref() == Some(&turn.id) {
                    attached.active_turn_id = None;
                }
            });
        }
        TurnEvent::ApprovalRequired(request) => {
            let request_id = request.request_id();
            insert_pending(runtime, request_id.clone(), PendingRequestKind::Approval);
            write_approval_ask(log_source, &event, &request_id).await;
        }
        TurnEvent::ToolCallRequired(request) => {
            insert_pending(
                runtime,
                request.request_id.clone(),
                PendingRequestKind::ToolCall,
            );
            write_approval_ask(log_source, &event, &request.request_id).await;
        }
        TurnEvent::ApprovalResolved { request_id } => {
            let removed = runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .attached
                .as_mut()
                .and_then(|attached| attached.pending.remove(request_id));
            if removed.is_some() {
                write_resolution(log_source, request_id, "answered_elsewhere").await;
            }
        }
        _ => {}
    }
}

fn insert_pending(
    runtime: &Arc<StdMutex<CodexRuntime>>,
    request_id: RequestId,
    kind: PendingRequestKind,
) {
    update_attached(runtime, |attached| {
        attached.pending.insert(request_id, kind);
    });
}

async fn write_approval_ask(
    log_source: &StructuredLogSource,
    event: &ThreadEvent,
    request_id: &RequestId,
) {
    log_source
        .write(json!({
            "type": "amux.codex_approval_required",
            "request_id": request_id,
            "availableDecisions": event.params.get("availableDecisions").cloned().unwrap_or(Value::Null),
        }))
        .await;
}

async fn write_resolution(log_source: &StructuredLogSource, request_id: &RequestId, reason: &str) {
    log_source
        .write(json!({
            "type": "amux.codex_approval_resolved",
            "request_id": request_id,
            "reason": reason,
        }))
        .await;
}

struct CodexInputTarget {
    runtime: Arc<StdMutex<CodexRuntime>>,
    log_source: StructuredLogSource,
}

impl CodexInputTarget {
    fn live(&self) -> Result<CodexLive> {
        self.runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .attached
            .as_ref()
            .and_then(|attached| attached.live.clone())
            .ok_or_else(|| anyhow!("Codex thread is read-only until reconnect succeeds"))
    }

    async fn execute(&self, input: CodexSdkV1Input) -> Result<()> {
        match input {
            CodexSdkV1Input::UserTurn { input } => {
                let items: Vec<InputItem> = serde_json::from_slice(&input)
                    .context("Codex user_turn input must be JSON input items")?;
                let live = self.live()?;
                let turn_id = live.thread.start_turn(items).await?;
                update_attached(&self.runtime, |attached| {
                    attached.active_turn_id = Some(turn_id);
                });
                Ok(())
            }
            CodexSdkV1Input::Steer { turn_id, input } => {
                let items: Vec<InputItem> = serde_json::from_slice(&input)
                    .context("Codex steer input must be JSON input items")?;
                let live = self.live()?;
                let active = live.thread.steer(&turn_id, items).await?;
                update_attached(&self.runtime, |attached| {
                    attached.active_turn_id = Some(active);
                });
                Ok(())
            }
            CodexSdkV1Input::Interrupt { turn_id } => {
                let (live, interrupt_turn_id) = {
                    let state = self
                        .runtime
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    let Some(attached) = state.attached.as_ref() else {
                        if turn_id.is_empty() {
                            return Ok(());
                        }
                        return Err(anyhow!("Codex thread is not attached"));
                    };
                    let interrupt_turn_id = if turn_id.is_empty() {
                        let Some(active) = attached.active_turn_id.clone() else {
                            return Ok(());
                        };
                        active
                    } else {
                        turn_id
                    };
                    let live = attached.live.clone().ok_or_else(|| {
                        anyhow!("Codex thread is read-only until reconnect succeeds")
                    })?;
                    (live, interrupt_turn_id)
                };
                live.thread.interrupt(&interrupt_turn_id).await?;
                Ok(())
            }
            CodexSdkV1Input::ApprovalDecision {
                request_id,
                decision,
            } => {
                let request_id: RequestId = serde_json::from_slice(&request_id)
                    .context("approval request_id must be a JSON string or integer")?;
                let (live, reply) = {
                    let mut state = self
                        .runtime
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    let attached = state
                        .attached
                        .as_mut()
                        .ok_or_else(|| anyhow!("unknown or already-resolved request id"))?;
                    let kind = *attached
                        .pending
                        .get(&request_id)
                        .ok_or_else(|| anyhow!("unknown or already-resolved request id"))?;
                    let reply = match kind {
                        PendingRequestKind::Approval => {
                            PendingReply::Approval(approval_response(&decision)?)
                        }
                        PendingRequestKind::ToolCall => {
                            let success =
                                matches!(decision.as_str(), "accept" | "acceptForSession");
                            if !success && !matches!(decision.as_str(), "decline" | "cancel") {
                                return Err(anyhow!("unsupported approval decision `{decision}`"));
                            }
                            PendingReply::ToolCall { success }
                        }
                    };
                    let live = attached.live.clone().ok_or_else(|| {
                        anyhow!("Codex thread is read-only until reconnect succeeds")
                    })?;
                    attached.pending.remove(&request_id);
                    (live, reply)
                };
                let result = match reply {
                    PendingReply::Approval(response) => {
                        live.thread
                            .respond_approval(request_id.clone(), response)
                            .await
                    }
                    PendingReply::ToolCall { success } => {
                        live.thread
                            .respond_tool_call(
                                request_id.clone(),
                                DynamicToolCallResponse {
                                    content_items: Vec::new(),
                                    success,
                                },
                            )
                            .await
                    }
                };
                let reason = if result.is_ok() {
                    "answered"
                } else {
                    "response_failed"
                };
                write_resolution(&self.log_source, &request_id, reason).await;
                result.map_err(Into::into)
            }
        }
    }
}

fn approval_response(decision: &str) -> Result<ApprovalResponse> {
    match decision {
        "accept" => Ok(ApprovalResponse::Accept),
        "acceptForSession" => Ok(ApprovalResponse::AcceptForSession),
        "decline" => Ok(ApprovalResponse::Decline),
        "cancel" => Ok(ApprovalResponse::Cancel),
        other => Err(anyhow!("unsupported approval decision `{other}`")),
    }
}

#[async_trait]
impl CodexInput for CodexInputTarget {
    async fn send(&self, input_id: Vec<u8>, input: CodexSdkV1Input) {
        let result = self.execute(input).await;
        let row = match result {
            Ok(()) => json!({
                "type": "amux.input_result",
                "input_id": input_id,
                "ok": {},
            }),
            Err(error) => json!({
                "type": "amux.input_result",
                "input_id": input_id,
                "error": {"message": error.to_string()},
            }),
        };
        self.log_source.write(row).await;
    }
}

#[async_trait]
impl AgentBackend for CodexSession {
    fn agent_id(&self) -> Uuid {
        self.agent_id
    }

    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn set_local_name(&mut self, name: Option<String>, _source: LocalAgentNameSource) {
        self.name = name.clone();
        self.runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .desired_name = name.clone();
        if let Some(name) = name {
            self.schedule_remote_rename(name);
        }
    }

    fn command(&self) -> &str {
        "codex"
    }

    fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    fn readonly(&self) -> bool {
        false
    }

    fn args(&self) -> &[String] {
        &[]
    }

    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        if self.started {
            return Err(anyhow!("Codex session {} already started", self.agent_id));
        }
        self.started = true;
        self.start_task(self.stop_tx.subscribe())
    }

    async fn stop(&self, _policy: StopPolicy) {
        tracing::info!(agent_id = %self.agent_id, "stopping Codex session");
        let _ = self.stop_tx.send(true);
        if let Some(abort) = self
            .runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .ingest_abort
            .take()
        {
            abort.abort();
        }
        let pty = self
            .runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .pty
            .take()
            .map(|pty| pty.handle);
        if let Some(pty) = pty
            && let Err(error) = pty.terminate().await
        {
            tracing::warn!(agent_id = %self.agent_id, %error, "failed to terminate Codex raw TUI");
        }
        let pending = mark_disconnected(&self.runtime);
        resolve_pending(&self.log_source, pending, "session_stopped").await;
        self.log_source.close().await;
    }

    fn agent_type(&self) -> &'static str {
        AGENT_TYPE_CODEX
    }

    fn io_protocols(&self) -> Vec<String> {
        vec![
            io::CODEX_SDK_V1.to_string(),
            crate::agents::terminal_io::TERMINAL_V1.to_string(),
        ]
    }

    fn log_source(&self) -> Option<StructuredLogSource> {
        Some(self.log_source.clone())
    }

    fn pty_handle(&self) -> Result<Option<PtyHandle>> {
        self.ensure_pty().map(Some)
    }

    fn codex_input(&self) -> Option<Box<dyn CodexInput>> {
        Some(Box::new(self.input_target()))
    }

    fn suspended_state(&self) -> Result<SuspendedAgent> {
        let (thread_id, daemon_mode) = {
            let runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            match runtime.attached.as_ref() {
                Some(attached) => (
                    Some(attached.thread_id.clone()),
                    attached.daemon_mode.clone(),
                ),
                None => (None, None),
            }
        };
        let Some(thread_id) = thread_id else {
            return Err(anyhow!(
                "cannot suspend Codex agent {}: thread_id is not available yet",
                self.agent_id
            ));
        };
        let daemon_mode = daemon_mode.ok_or_else(|| {
            anyhow!(
                "cannot suspend Codex agent {}: daemon mode is not available yet",
                self.agent_id
            )
        })?;
        Ok(SuspendedAgent::Codex {
            agent_id: self.agent_id,
            name: self.name.clone(),
            working_dir: self.working_dir.clone(),
            model: self.model.clone(),
            approval_policy: self.approval_policy.clone(),
            sandbox_policy: self.sandbox_policy.clone(),
            thread_id,
            daemon_mode,
            created_at: self.created_at,
        })
    }

    fn debug_json(&self, _verbose: bool) -> serde_json::Result<Value> {
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        Ok(json!({
            "kind": "codex",
            "thread_id": runtime.attached.as_ref().map(|attached| &attached.thread_id),
            "daemon_mode": runtime.attached.as_ref().and_then(|attached| attached.daemon_mode.as_deref()),
            "startup_error": runtime.startup_error,
            "has_event_ingest": runtime.ingest_abort.is_some(),
            "connected": runtime.attached.as_ref().is_some_and(|attached| attached.live.is_some()),
            "has_pty": runtime.pty.is_some(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentType;
    use replay_support::{
        ReplayAdvance, ReplayOptions, load_script, replay_transport_with_controller,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};

    fn session() -> CodexSession {
        let req = CreateAgentRequest {
            agent_id: Uuid::from_u128(1),
            host_id: None,
            name: Some("named".into()),
            agent_type: AgentType::Codex {
                model: None,
                approval_policy: None,
                sandbox_policy: None,
                resume_thread_id: None,
            },
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: Vec::new(),
        };
        CodexSession::new(
            &req,
            Arc::new(CodexClient::new(PathBuf::from("/tmp/amux-codex.sock"))),
        )
    }

    #[test]
    fn advertises_both_planes_before_pty_exists() {
        let session = session();
        assert!(session.runtime.lock().unwrap().pty.is_none());
        assert_eq!(
            session.io_protocols(),
            [io::CODEX_SDK_V1, crate::agents::terminal_io::TERMINAL_V1]
        );
    }

    #[tokio::test]
    async fn raw_spawn_failure_before_thread_ready_leaves_structured_plane_healthy() {
        let session = session();

        let error = session.pty_handle().err().unwrap();
        assert!(error.to_string().contains("thread_id is not available"));
        assert!(session.runtime.lock().unwrap().pty.is_none());
        assert!(session.log_source().unwrap().subscribe().await.is_some());
    }

    #[test]
    fn suspend_is_nonfatal_and_reports_missing_thread_id() {
        let error = session().suspended_state().unwrap_err();
        assert!(error.to_string().contains("thread_id is not available"));
    }

    #[test]
    fn suspend_records_persistent_codex_identity() {
        let session = session();
        {
            let mut runtime = session.runtime.lock().unwrap();
            runtime.attached = Some(CodexAttached {
                thread_id: "thread-persisted".into(),
                daemon_mode: Some("spawned-private".into()),
                live: None,
                active_turn_id: None,
                pending: HashMap::new(),
            });
        }

        let suspended = session.suspended_state().unwrap();
        assert!(matches!(
            suspended,
            SuspendedAgent::Codex {
                thread_id,
                daemon_mode,
                ..
            } if thread_id == "thread-persisted" && daemon_mode == "spawned-private"
        ));
    }

    #[test]
    fn raw_rows_preserve_method_and_params() {
        let event = ThreadEvent {
            method: "item/agentMessage/delta".into(),
            params: json!({"threadId": "t", "delta": "P"}),
            turn_id: Some("turn".into()),
            event: TurnEvent::Unknown {
                method: "ignored".into(),
                params: Value::Null,
            },
        };
        assert_eq!(
            raw_row(&event),
            json!({"type": "item/agentMessage/delta", "threadId": "t", "delta": "P"})
        );
    }

    #[tokio::test]
    async fn connection_loss_resolves_every_pending_request() {
        let source = StructuredLogSource::new(16);
        let runtime = Arc::new(StdMutex::new(CodexRuntime {
            desired_name: None,
            attached: Some(CodexAttached {
                thread_id: "thread-1".into(),
                daemon_mode: Some("test".into()),
                live: None,
                active_turn_id: Some("turn-1".into()),
                pending: HashMap::from([
                    (RequestId::Integer(1), PendingRequestKind::Approval),
                    (
                        RequestId::String("tool-2".into()),
                        PendingRequestKind::ToolCall,
                    ),
                ]),
            }),
            startup_error: None,
            ingest_abort: None,
            pty: None,
            next_pty_epoch: 0,
        }));
        resolve_pending(&source, mark_disconnected(&runtime), "connection_lost").await;
        let (mut reader, seq) = source.subscribe_with_query(None).await.unwrap();
        assert_eq!(seq, 2);
        let mut ids = Vec::new();
        for _ in 0..2 {
            let row = reader.read().await.unwrap().payload;
            assert_eq!(row["type"], "amux.codex_approval_resolved");
            assert_eq!(row["reason"], "connection_lost");
            ids.push(row["request_id"].clone());
        }
        assert!(ids.contains(&json!(1)));
        assert!(ids.contains(&json!("tool-2")));
    }

    #[tokio::test]
    async fn unknown_approval_id_produces_one_correlated_error_row() {
        let source = StructuredLogSource::new(16);
        let runtime = Arc::new(StdMutex::new(CodexRuntime {
            desired_name: None,
            attached: Some(CodexAttached {
                thread_id: "thread-1".into(),
                daemon_mode: Some("test".into()),
                live: None,
                active_turn_id: None,
                pending: HashMap::new(),
            }),
            startup_error: None,
            ingest_abort: None,
            pty: None,
            next_pty_epoch: 0,
        }));
        CodexInputTarget {
            runtime,
            log_source: source.clone(),
        }
        .send(
            b"input-unknown".to_vec(),
            CodexSdkV1Input::ApprovalDecision {
                request_id: br#""missing""#.to_vec(),
                decision: "accept".into(),
            },
        )
        .await;
        let (mut reader, seq) = source.subscribe_with_query(None).await.unwrap();
        assert_eq!(seq, 1);
        let row = reader.read().await.unwrap().payload;
        assert_eq!(row["type"], "amux.input_result");
        assert_eq!(row["input_id"], json!(b"input-unknown"));
        assert!(
            row["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unknown")
        );
    }

    #[tokio::test]
    async fn explicit_interrupt_is_sent_when_active_turn_is_unknown() {
        let (client_side, server_side) = duplex(16 * 1024);
        let (client_read, client_write) = split(client_side);
        let (server_read, mut server_write) = split(server_side);
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut line = String::new();

            reader.read_line(&mut line).await.unwrap();
            let initialize: Value = serde_json::from_str(line.trim()).unwrap();
            server_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({
                            "id": initialize["id"],
                            "result": {
                                "userAgent": "test",
                                "codexHome": "/tmp",
                                "platformFamily": "unix",
                                "platformOs": "test"
                            }
                        })
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(line.trim()).unwrap()["method"],
                "initialized"
            );

            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let start: Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(start["method"], "thread/start");
            server_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({
                            "id": start["id"],
                            "result": {
                                "thread": {
                                    "id": "thread-1",
                                    "path": null,
                                    "agentNickname": null,
                                    "agentRole": null,
                                    "gitInfo": null,
                                    "name": null
                                },
                                "model": "test",
                                "modelProvider": "openai",
                                "serviceTier": null,
                                "cwd": "/tmp",
                                "approvalPolicy": "on-request",
                                "approvalsReviewer": "user",
                                "sandbox": {"type": "readOnly", "networkAccess": false},
                                "reasoningEffort": null
                            }
                        })
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let interrupt: Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(interrupt["method"], "turn/interrupt");
            assert_eq!(interrupt["params"]["threadId"], "thread-1");
            assert_eq!(interrupt["params"]["turnId"], "daemon-turn");
            server_write
                .write_all(format!("{}\n", json!({"id": interrupt["id"], "result": {}})).as_bytes())
                .await
                .unwrap();
        });

        let client = Codex::from_io(
            BufReader::new(client_read),
            client_write,
            CodexConfig::default(),
        )
        .await
        .unwrap();
        let thread = client.start_thread(ThreadConfig::default()).await.unwrap();
        let source = StructuredLogSource::new(4);
        let runtime = Arc::new(StdMutex::new(CodexRuntime {
            desired_name: None,
            attached: Some(CodexAttached {
                thread_id: "thread-1".into(),
                daemon_mode: Some("test".into()),
                live: Some(CodexLive {
                    client: client.clone(),
                    thread,
                    socket_path: PathBuf::from("/tmp/test-codex.sock"),
                }),
                active_turn_id: None,
                pending: HashMap::new(),
            }),
            startup_error: None,
            ingest_abort: None,
            pty: None,
            next_pty_epoch: 0,
        }));
        CodexInputTarget {
            runtime,
            log_source: source.clone(),
        }
        .send(
            b"explicit-interrupt".to_vec(),
            CodexSdkV1Input::Interrupt {
                turn_id: "daemon-turn".into(),
            },
        )
        .await;

        server.await.unwrap();
        let (mut rows, count) = source.subscribe_with_query(None).await.unwrap();
        assert_eq!(count, 1);
        let row = rows.read().await.unwrap().payload;
        assert_eq!(row["type"], "amux.input_result");
        assert!(row.get("ok").is_some());
        client.close().await;
    }

    async fn next_ingested(
        events: &mut codex_sdk::ThreadEventStream,
        runtime: &Arc<StdMutex<CodexRuntime>>,
        source: &StructuredLogSource,
    ) -> ThreadEvent {
        let event = events.next().await.unwrap().expect("fixture event");
        ingest_event(runtime, source, event.clone()).await;
        event
    }

    fn attach_runtime(runtime: &Arc<StdMutex<CodexRuntime>>, client: &Codex, thread: &Thread) {
        runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .attached = Some(CodexAttached {
            thread_id: thread.id().into(),
            daemon_mode: Some("replay".into()),
            live: Some(CodexLive {
                client: client.clone(),
                thread: thread.clone(),
                socket_path: PathBuf::from("/tmp/test-codex.sock"),
            }),
            active_turn_id: None,
            pending: HashMap::new(),
        });
    }

    fn project_row(row: &Value) -> Value {
        let row_type = row.get("type").cloned().unwrap_or(Value::Null);
        match row_type.as_str() {
            Some("amux.input_result") => json!({
                "type": row_type,
                "input_id": row["input_id"],
                "result": if row.get("ok").is_some() { "ok" } else { "error" },
            }),
            Some("amux.codex_approval_required") => json!({
                "type": row_type,
                "request_id": row["request_id"],
                "availableDecisions": row["availableDecisions"],
            }),
            Some("amux.codex_approval_resolved") => json!({
                "type": row_type,
                "request_id": row["request_id"],
                "reason": row["reason"],
            }),
            _ => json!({"type": row_type}),
        }
    }

    #[tokio::test]
    async fn replayed_wire_drives_backend_rows_inputs_and_resume() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_backend");
        let (reader, writer, controller) = replay_transport_with_controller(
            load_script(fixture.join("io.jsonl")),
            ReplayOptions::default(),
        );
        let driver = tokio::spawn(async move {
            while let ReplayAdvance::Advanced { .. } | ReplayAdvance::BlockedOnWrite =
                controller.advance_one().await
            {
                tokio::task::yield_now().await;
            }
        });
        let client = Codex::from_io(
            reader,
            writer,
            CodexConfig {
                client_name: "amux-test".into(),
                client_title: Some("amux".into()),
                client_version: "0.4.0".into(),
                ..CodexConfig::default()
            },
        )
        .await
        .unwrap();
        let thread = client.start_thread(ThreadConfig::default()).await.unwrap();
        let mut events = thread.events().await.unwrap();
        let source = StructuredLogSource::new(128);
        let runtime = Arc::new(StdMutex::new(CodexRuntime {
            desired_name: None,
            attached: None,
            startup_error: None,
            ingest_abort: None,
            pty: None,
            next_pty_epoch: 0,
        }));
        attach_runtime(&runtime, &client, &thread);
        source.write(json!({"type": "amux.codex_ready"})).await;
        let target = CodexInputTarget {
            runtime: runtime.clone(),
            log_source: source.clone(),
        };

        target
            .send(
                b"turn".to_vec(),
                CodexSdkV1Input::UserTurn {
                    input: serde_json::to_vec(&json!([{
                        "type": "text",
                        "text": "Reply exactly PONG."
                    }]))
                    .unwrap(),
                },
            )
            .await;
        for _ in 0..4 {
            next_ingested(&mut events, &runtime, &source).await;
        }

        for (id, input_id, decision) in [
            ("approval-allow", b"allow".as_slice(), "accept"),
            ("approval-deny", b"deny".as_slice(), "decline"),
            ("file-allow", b"file".as_slice(), "accept"),
        ] {
            let ask = next_ingested(&mut events, &runtime, &source).await;
            assert_eq!(
                ask.params["availableDecisions"],
                json!(["accept", "decline"])
            );
            target
                .send(
                    input_id.to_vec(),
                    CodexSdkV1Input::ApprovalDecision {
                        request_id: serde_json::to_vec(id).unwrap(),
                        decision: decision.into(),
                    },
                )
                .await;
        }

        next_ingested(&mut events, &runtime, &source).await;
        target
            .send(
                b"int".to_vec(),
                CodexSdkV1Input::Interrupt {
                    turn_id: String::new(),
                },
            )
            .await;
        next_ingested(&mut events, &runtime, &source).await;

        let resumed = client
            .resume_thread("thread-capture", ThreadConfig::default())
            .await
            .unwrap();
        let mut resumed_events = resumed.events().await.unwrap();
        attach_runtime(&runtime, &client, &resumed);
        source.write(json!({"type": "amux.codex_ready"})).await;
        next_ingested(&mut resumed_events, &runtime, &source).await;
        driver.await.unwrap();

        let expected: Vec<Value> = std::fs::read_to_string(fixture.join("rows.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let (mut reader, seq) = source.subscribe_with_query(None).await.unwrap();
        let mut actual = Vec::new();
        for _ in 0..seq {
            actual.push(project_row(&reader.read().await.unwrap().payload));
        }
        assert_eq!(actual, expected);
    }
}

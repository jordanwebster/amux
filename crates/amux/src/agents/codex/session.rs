use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use codex_sdk::{
    AccountReadParams, ApprovalResponse, Codex, CodexConfig, DaemonMode, DynamicToolCallResponse,
    Error as CodexError, InputItem, RequestId, Thread, ThreadConfig, ThreadEvent, ThreadItem,
    TurnEvent, connect_daemon, connect_socket, daemon_socket_path, ensure_daemon_with_fallback,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};
use tokio::task::AbortHandle;
use uuid::Uuid;

use super::CODEX_RAW_THREAD_NOT_READY;
use super::io::CodexSdkV1Input;
use crate::agent_tools;
use crate::agents::{
    AgentBackend, AgentDeliveryTarget, AgentKind, AgentParent, CreateAgentRequest, Delivery,
    DeliveryError, DeliveryLiveness, LocalAgentNameSource, McpLaunchRoute, Plane, Protocol,
    PtyHandle, RawPtyTarget, SessionEvent, SpawnInheritance, StopPolicy, StructuredInput,
    StructuredInputEvent, StructuredLogSource, spawn_pty_agent,
};
use crate::envelope::{Envelope, Sender};
use crate::protocol::ProtocolError;
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

    async fn connection(&self) -> Result<Arc<CodexConnection>> {
        let codex_home = codex_home()?;
        self.connection_with_codex_home(&codex_home).await
    }

    async fn connection_with_codex_home(&self, codex_home: &Path) -> Result<Arc<CodexConnection>> {
        let mut slot = self.connection.lock().await;
        if let Some(connection) = slot.as_ref()
            && !connection.client.is_closed()
        {
            return Ok(connection.clone());
        }
        // A dead shared connection must not remain sticky. Dropping its daemon
        // guard here also lets a supervised daemon be recreated.
        slot.take();

        let daemon = ensure_daemon_with_fallback(codex_home, &self.private_socket)
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
        // Only a supervised process can report its own exit; an existing
        // daemon is observed through the transport alone.
        let (socket_path, daemon_exit) = match &daemon {
            DaemonMode::Existing => {
                let home = tokio::fs::canonicalize(codex_home)
                    .await
                    .context("failed to resolve CODEX_HOME")?;
                (daemon_socket_path(&home), None)
            }
            DaemonMode::Spawned(process) | DaemonMode::Private(process) => (
                process.socket_path().to_path_buf(),
                Some(process.exit_token()),
            ),
            DaemonMode::PrivateExisting(socket_path) => (socket_path.clone(), None),
        };
        let client = match &daemon {
            DaemonMode::Existing | DaemonMode::Spawned(_) => {
                connect_daemon(codex_home, config).await
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

    /// Check account readiness through the same cached, fallback-capable
    /// connection that agent sessions use for threads and turns.
    pub(crate) async fn ensure_authenticated(&self) -> Result<()> {
        let codex_home = codex_home()?;
        self.ensure_authenticated_with_codex_home(&codex_home).await
    }

    async fn ensure_authenticated_with_codex_home(&self, codex_home: &Path) -> Result<()> {
        let connection = self.connection_with_codex_home(codex_home).await?;
        let response = connection
            .client
            .read_account(AccountReadParams::default())
            .await
            .context("failed to read the Codex account")?;
        require_account(response.account.is_some(), response.requires_openai_auth)
    }
}

fn codex_home() -> Result<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or_else(|| anyhow!("CODEX_HOME or HOME is required for Codex agents"))
}

fn require_account(has_account: bool, requires_openai_auth: bool) -> Result<()> {
    if has_account || !requires_openai_auth {
        Ok(())
    } else {
        Err(anyhow!(
            "Codex is not authenticated; run `codex login` and try again"
        ))
    }
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

/// Name a Codex thread, reporting failure rather than swallowing it.
///
/// Naming is also how amux materializes a thread. Codex 0.147's
/// `thread/start` creates a live thread and reports its prospective rollout
/// path without materializing that rollout. Operations that need it —
/// `thread/resume`, which the raw TUI and amux reconnect paths use, and
/// `thread/archive` — fail with `no rollout found for thread id` until an
/// unrelated mutation persists it. Upstream exposes no persist call. Naming
/// is the least invasive universally applicable materializer; memory mode,
/// Git metadata, injected history, and feature-gated goals also materialize
/// but carry behavioral or applicability costs. See `docs/CODEX.md`.
async fn set_thread_name(
    client: &Codex,
    agent_id: Uuid,
    thread_id: &str,
    name: &str,
) -> Result<(), CodexError> {
    match client.rename_thread(thread_id, name).await {
        Ok(()) => Ok(()),
        Err(error) => {
            tracing::warn!(%agent_id, %thread_id, %error, "failed to name Codex thread");
            Err(error)
        }
    }
}

/// Thread name for an agent the user has not named, using the same short-id
/// convention as the clients' display fallback. It is a bootstrap label, not
/// an agent name: the agent stays unnamed, and naming it later overwrites this
/// through the serialized name reconciler.
fn bootstrap_thread_name(agent_id: Uuid) -> String {
    format!("amux-{}", &agent_id.simple().to_string()[..8])
}

/// The name every Codex thread is created with.
///
/// Naming materializes a thread, so an unnamed agent must not skip it.
/// An empty name is treated as absent: upstream rejects it outright, which
/// would leave the thread unmaterialized.
fn thread_name_for(desired_name: Option<&str>, agent_id: Uuid) -> String {
    desired_name
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| bootstrap_thread_name(agent_id))
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
    last_agent_messages: HashMap<String, String>,
    pending: HashMap<RequestId, PendingRequestKind>,
    applied_name_generation: Option<u64>,
}

#[derive(Clone)]
struct CodexCompletionSink {
    agent_id: Uuid,
    event_tx: tokio::sync::mpsc::Sender<SessionEvent>,
}

struct CodexIngestOptions {
    thread_config: ThreadConfig,
    thread_id: Option<String>,
    completion_sink: Option<CodexCompletionSink>,
}

struct CodexPty {
    handle: PtyHandle,
    epoch: u64,
    subscribers: usize,
}

/// One live `terminal_v1` attachment to a Codex raw PTY.
///
/// The last lease retires its epoch from the cache synchronously before
/// scheduling process-group termination. That ordering prevents a concurrent
/// reattach from observing a handle that is already being torn down.
pub(crate) struct CodexRawPtyLease {
    handle: PtyHandle,
    epoch: u64,
    agent_id: Uuid,
    runtime: Arc<StdMutex<CodexRuntime>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CodexRawPtyPlan {
    thread_id: String,
    socket_path: PathBuf,
    model: Option<String>,
    approval_policy: Option<String>,
    sandbox_policy: Option<String>,
    working_dir: PathBuf,
}

impl CodexRawPtyPlan {
    fn spawn(self, agent_id: Uuid) -> Result<(PtyHandle, tokio::task::JoinHandle<()>)> {
        std::os::unix::net::UnixStream::connect(&self.socket_path).with_context(|| {
            format!(
                "Codex raw TUI app-server socket is unavailable: {}",
                self.socket_path.display()
            )
        })?;
        let args = raw_tui_args(
            &self.thread_id,
            &self.socket_path,
            self.model.as_deref(),
            self.approval_policy.as_deref(),
            self.sandbox_policy.as_deref(),
        );
        spawn_pty_agent(agent_id, "codex", &args, &self.working_dir, &[], &[], None)
            .context("failed to spawn Codex raw TUI")
    }

    fn is_current(&self, runtime: &CodexRuntime) -> bool {
        runtime.attached.as_ref().is_some_and(|attached| {
            attached.thread_id == self.thread_id
                && attached
                    .live
                    .as_ref()
                    .is_some_and(|live| live.socket_path == self.socket_path)
        })
    }
}

/// Owned Codex raw-PTY endpoint. Acquiring it never needs the host registry;
/// the per-session preparation mutex preserves one-spawn fanout while the
/// blocking connect/open/spawn runs without the Codex runtime mutex.
#[derive(Clone)]
pub(crate) struct CodexRawPtyTarget {
    agent_id: Uuid,
    model: Option<String>,
    approval_policy: Option<String>,
    sandbox_policy: Option<String>,
    working_dir: PathBuf,
    runtime: Arc<StdMutex<CodexRuntime>>,
    preparation: Arc<Mutex<()>>,
    stop_tx: watch::Sender<bool>,
}

impl CodexRawPtyTarget {
    pub(crate) fn active_handle(&self) -> Option<PtyHandle> {
        self.runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .pty
            .as_ref()
            .map(|pty| pty.handle.clone())
    }

    pub(crate) async fn acquire_lease(&self) -> Result<CodexRawPtyLease> {
        let _preparation = self.preparation.lock().await;
        let plan = {
            let mut runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if *self.stop_tx.borrow() {
                return Err(anyhow!("Codex raw session stopped during preparation"));
            }
            if let Some(lease) = cached_raw_pty_lease(self.agent_id, &self.runtime, &mut runtime)? {
                return Ok(lease);
            }
            let attached = runtime
                .attached
                .as_ref()
                .ok_or_else(|| anyhow!("{CODEX_RAW_THREAD_NOT_READY}"))?;
            let live = attached.live.as_ref().ok_or_else(|| {
                anyhow!("Codex raw session is unavailable until reconnect succeeds")
            })?;
            CodexRawPtyPlan {
                thread_id: attached.thread_id.clone(),
                socket_path: live.socket_path.clone(),
                model: self.model.clone(),
                approval_policy: self.approval_policy.clone(),
                sandbox_policy: self.sandbox_policy.clone(),
                working_dir: self.working_dir.clone(),
            }
        };

        // This synchronous section is intentionally outside both the host
        // registry guard and `CodexRuntime` mutex. Keeping it in this future
        // also makes cancellation wait until forkpty has either succeeded or
        // failed, so a detached blocking task cannot leak an unpublished PTY.
        let (handle, exit_handle) = plan.clone().spawn(self.agent_id)?;

        let published = {
            let mut runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if *self.stop_tx.borrow() || runtime.pty.is_some() || !plan.is_current(&runtime) {
                None
            } else {
                let epoch = runtime.next_pty_epoch;
                runtime.next_pty_epoch = runtime.next_pty_epoch.wrapping_add(1);
                runtime.pty = Some(CodexPty {
                    handle: handle.clone(),
                    epoch,
                    subscribers: 1,
                });
                Some(epoch)
            }
        };

        let Some(epoch) = published else {
            retire_unpublished_raw_pty(self.agent_id, handle).await;
            exit_handle.abort();
            return Err(anyhow!(
                "Codex raw session changed or stopped during preparation"
            ));
        };

        let exit_runtime = self.runtime.clone();
        tokio::spawn(async move {
            let _ = exit_handle.await;
            clear_cached_pty_epoch(&exit_runtime, epoch);
        });

        Ok(CodexRawPtyLease {
            handle,
            epoch,
            agent_id: self.agent_id,
            runtime: self.runtime.clone(),
        })
    }
}

impl CodexRawPtyLease {
    pub(crate) fn handle(&self) -> &PtyHandle {
        &self.handle
    }
}

impl Drop for CodexRawPtyLease {
    fn drop(&mut self) {
        let retired = {
            let mut runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let Some(pty) = runtime.pty.as_mut().filter(|pty| pty.epoch == self.epoch) else {
                return;
            };
            if pty.subscribers > 1 {
                pty.subscribers -= 1;
                return;
            }
            runtime.pty.take().map(|pty| pty.handle)
        };

        let Some(handle) = retired else {
            return;
        };
        let agent_id = self.agent_id;
        let epoch = self.epoch;
        if let Err(error) = handle.signal_process_group(pty_host::ProcessGroupSignal::Terminate) {
            tracing::warn!(
                %agent_id,
                epoch,
                %error,
                "failed to signal detached Codex raw TUI process group"
            );
        }
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    if let Err(error) = handle.terminate().await {
                        tracing::warn!(
                            %agent_id,
                            epoch,
                            %error,
                            "failed to terminate detached Codex raw TUI process group"
                        );
                    }
                });
            }
            Err(error) => {
                tracing::error!(
                    %agent_id,
                    epoch,
                    %error,
                    "cannot escalate detached Codex raw TUI termination outside a Tokio runtime"
                );
            }
        }
    }
}

struct CodexRuntime {
    desired_name: Option<String>,
    desired_name_generation: u64,
    name_reconciler_running: bool,
    attached: Option<CodexAttached>,
    resume_daemon_mode: Option<String>,
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
    parent: Option<AgentParent>,
    resume_thread_id: Option<String>,
    created_at: DateTime<Utc>,
    log_source: StructuredLogSource,
    shared_client: Arc<CodexClient>,
    mcp_launch_route: McpLaunchRoute,
    runtime: Arc<StdMutex<CodexRuntime>>,
    raw_pty_preparation: Arc<Mutex<()>>,
    stop_tx: watch::Sender<bool>,
    started: bool,
}

impl CodexSession {
    pub(crate) fn new(
        req: &CreateAgentRequest,
        shared_client: Arc<CodexClient>,
        mcp_launch_route: McpLaunchRoute,
    ) -> Self {
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
        Self {
            agent_id: req.agent_id,
            name: req.name.clone(),
            working_dir: req.working_dir.clone(),
            model,
            approval_policy,
            sandbox_policy,
            parent: req.parent,
            resume_thread_id,
            created_at: Utc::now(),
            log_source: codex_log_source(),
            shared_client,
            mcp_launch_route,
            runtime: Arc::new(StdMutex::new(CodexRuntime {
                desired_name: req.name.clone(),
                desired_name_generation: 0,
                name_reconciler_running: false,
                attached: None,
                resume_daemon_mode: None,
                startup_error: None,
                ingest_abort: None,
                pty: None,
                next_pty_epoch: 0,
            })),
            raw_pty_preparation: Arc::new(Mutex::new(())),
            stop_tx,
            started: false,
        }
    }

    pub(crate) fn from_suspended(
        req: &CreateAgentRequest,
        shared_client: Arc<CodexClient>,
        mcp_launch_route: McpLaunchRoute,
        daemon_mode: Option<String>,
        created_at: DateTime<Utc>,
    ) -> Self {
        let session = Self::new(req, shared_client, mcp_launch_route);
        {
            let mut runtime = session
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            runtime.resume_daemon_mode = daemon_mode;
        }
        Self {
            created_at,
            ..session
        }
    }

    fn thread_config(&self) -> Result<ThreadConfig> {
        self.mcp_launch_route
            .validate()
            .context("managed Codex MCP launch route is no longer valid")?;
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
        let executable = self
            .mcp_launch_route
            .executable()
            .to_str()
            .context("the running amux executable path is not valid UTF-8")?;
        let socket_path = self
            .mcp_launch_route
            .socket_path()
            .to_str()
            .context("the daemon socket path is not valid UTF-8")?;
        let mut environment = serde_json::Map::from_iter([
            (
                "AMUX_AGENT_ID".to_string(),
                Value::String(self.agent_id.to_string()),
            ),
            (
                "AMUX_HOST_ID".to_string(),
                Value::String(self.mcp_launch_route.host_id().to_string()),
            ),
        ]);
        if let Some(config_path) = self.mcp_launch_route.config_path() {
            environment.insert(
                "AMUX_CONFIG".to_string(),
                Value::String(
                    config_path
                        .to_str()
                        .context("the amux config path is not valid UTF-8")?
                        .to_string(),
                ),
            );
        }
        let enabled_tools = agent_tools::definitions()
            .into_iter()
            .map(|tool| Value::String(tool.name.to_string()))
            .collect::<Vec<_>>();
        let config = json!({
            "mcp_servers": {
                "amux": {
                    "command": executable,
                    "args": ["mcp", "agent", "--socket-path", socket_path],
                    "env": environment,
                    "enabled": true,
                    "required": true,
                    "startup_timeout_sec": 10,
                    "tool_timeout_sec": 60,
                    "default_tools_approval_mode": "approve",
                    "enabled_tools": enabled_tools,
                }
            }
        });
        let Value::Object(config) = config else {
            unreachable!("managed Codex MCP config is an object")
        };
        Ok(ThreadConfig {
            cwd: Some(cwd),
            model: self.model.clone(),
            approval_policy,
            sandbox,
            config: Some(config),
            ..ThreadConfig::default()
        })
    }

    fn start_task(
        &self,
        stop_rx: watch::Receiver<bool>,
        event_tx: &tokio::sync::mpsc::Sender<SessionEvent>,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let thread_config = self.thread_config()?;
        let shared_client = self.shared_client.clone();
        let runtime = self.runtime.clone();
        let log_source = self.log_source.clone();
        let resume_thread_id = self.resume_thread_id.clone();
        let agent_id = self.agent_id;
        let completion_sink = self.completion_sink(event_tx);
        let handle = tokio::spawn(run_ingest_supervisor(
            agent_id,
            shared_client,
            runtime.clone(),
            log_source,
            CodexIngestOptions {
                thread_config,
                thread_id: resume_thread_id,
                completion_sink,
            },
            stop_rx,
        ));
        runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .ingest_abort = Some(handle.abort_handle());
        Ok(handle)
    }

    fn completion_sink(
        &self,
        event_tx: &tokio::sync::mpsc::Sender<SessionEvent>,
    ) -> Option<CodexCompletionSink> {
        self.parent.map(|_| CodexCompletionSink {
            agent_id: self.agent_id,
            event_tx: event_tx.clone(),
        })
    }

    fn input_target(&self) -> CodexInputTarget {
        CodexInputTarget {
            runtime: self.runtime.clone(),
            log_source: self.log_source.clone(),
        }
    }

    fn owned_raw_pty_target(&self) -> CodexRawPtyTarget {
        CodexRawPtyTarget {
            agent_id: self.agent_id,
            model: self.model.clone(),
            approval_policy: self.approval_policy.clone(),
            sandbox_policy: self.sandbox_policy.clone(),
            working_dir: self.working_dir.clone(),
            runtime: self.runtime.clone(),
            preparation: self.raw_pty_preparation.clone(),
            stop_tx: self.stop_tx.clone(),
        }
    }
}

fn cached_raw_pty_lease(
    agent_id: Uuid,
    runtime: &Arc<StdMutex<CodexRuntime>>,
    state: &mut CodexRuntime,
) -> Result<Option<CodexRawPtyLease>> {
    let Some(pty) = state.pty.as_ref() else {
        return Ok(None);
    };
    let handle = pty.handle.clone();
    let epoch = pty.epoch;
    let subscribers = pty
        .subscribers
        .checked_add(1)
        .ok_or_else(|| anyhow!("Codex raw PTY subscriber count overflow"))?;
    state.pty.as_mut().expect("PTY still cached").subscribers = subscribers;
    Ok(Some(CodexRawPtyLease {
        handle,
        epoch,
        agent_id,
        runtime: runtime.clone(),
    }))
}

async fn retire_unpublished_raw_pty(agent_id: Uuid, handle: PtyHandle) {
    if let Err(error) = handle.terminate().await {
        tracing::warn!(
            %agent_id,
            %error,
            "failed to terminate unpublished Codex raw TUI process group"
        );
    }
}

#[cfg(test)]
fn acquire_test_raw_pty_lease(
    agent_id: Uuid,
    runtime: &Arc<StdMutex<CodexRuntime>>,
    spawn: impl FnOnce(&CodexRuntime) -> Result<(PtyHandle, tokio::task::JoinHandle<()>)>,
) -> Result<CodexRawPtyLease> {
    let (handle, exit_handle, epoch) = {
        let mut state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(lease) = cached_raw_pty_lease(agent_id, runtime, &mut state)? {
            return Ok(lease);
        }
        let (handle, exit_handle) = spawn(&state)?;
        let epoch = state.next_pty_epoch;
        state.next_pty_epoch = state.next_pty_epoch.wrapping_add(1);
        state.pty = Some(CodexPty {
            handle: handle.clone(),
            epoch,
            subscribers: 1,
        });
        (handle, exit_handle, epoch)
    };

    let exit_runtime = runtime.clone();
    tokio::spawn(async move {
        let _ = exit_handle.await;
        clear_cached_pty_epoch(&exit_runtime, epoch);
    });

    Ok(CodexRawPtyLease {
        handle,
        epoch,
        agent_id,
        runtime: runtime.clone(),
    })
}

fn clear_cached_pty_epoch(runtime: &StdMutex<CodexRuntime>, epoch: u64) {
    let mut state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
    if state.pty.as_ref().is_some_and(|pty| pty.epoch == epoch) {
        state.pty = None;
    }
}

fn raw_tui_args(
    thread_id: &str,
    socket_path: &Path,
    model: Option<&str>,
    approval_policy: Option<&str>,
    sandbox_policy: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "resume".to_string(),
        thread_id.to_string(),
        "--remote".to_string(),
        format!("unix://{}", socket_path.display()),
    ];
    if let Some(model) = model {
        args.extend(["--model".to_string(), model.to_string()]);
    }
    if let Some(approval_policy) = approval_policy {
        args.extend([
            "--ask-for-approval".to_string(),
            approval_policy.to_string(),
        ]);
    }
    if let Some(sandbox_policy) = sandbox_policy {
        args.extend(["--sandbox".to_string(), sandbox_policy.to_string()]);
    }
    args
}

async fn run_ingest_supervisor(
    agent_id: Uuid,
    shared_client: Arc<CodexClient>,
    runtime: Arc<StdMutex<CodexRuntime>>,
    log_source: StructuredLogSource,
    options: CodexIngestOptions,
    mut stop_rx: watch::Receiver<bool>,
) {
    let CodexIngestOptions {
        thread_config,
        mut thread_id,
        completion_sink,
    } = options;
    let mut initial_persisted_resume_pending = thread_id.is_some();
    let mut retry = 0_usize;
    let mut ambiguous_started_thread_id = None;
    // Capture-rig fault injection: close only this SDK transport once. The
    // daemon and other processes remain untouched.
    let mut capture_drop_connection =
        std::env::var_os("AMUX_CODEX_CAPTURE_DROP_CONNECTION_ONCE").is_some();
    loop {
        if *stop_rx.borrow() {
            break;
        }

        let (connection, mut thread, provenance) = match attach_thread(
            &shared_client,
            &thread_config,
            thread_id.as_deref(),
            ambiguous_started_thread_id.as_deref(),
        )
        .await
        {
            Ok(attached) => attached,
            Err(error) => {
                let message = error.to_string();
                let pending = mark_disconnected(&runtime, Some(message.clone()));
                resolve_pending(&log_source, pending, "connection_lost").await;
                write_reconnect_error(&log_source, &message).await;
                if wait_for_retry(&mut stop_rx, retry).await {
                    break;
                }
                retry = (retry + 1).min(RECONNECT_BACKOFF.len() - 1);
                continue;
            }
        };
        ambiguous_started_thread_id = None;
        let applied_name_generation = match provenance {
            AttachmentProvenance::Started => {
                let materialized = materialize_started_thread(
                    &connection.client,
                    agent_id,
                    &thread_config,
                    thread,
                    &runtime,
                    &log_source,
                    &mut stop_rx,
                )
                .await;
                match materialized {
                    MaterializeStartOutcome::Ready(materialized) => {
                        thread = materialized.thread;
                        materialized.applied_name_generation
                    }
                    MaterializeStartOutcome::TransportLost {
                        candidate_thread_id,
                        message,
                    } => {
                        ambiguous_started_thread_id = Some(candidate_thread_id);
                        let pending = mark_disconnected(&runtime, Some(message.clone()));
                        resolve_pending(&log_source, pending, "connection_lost").await;
                        write_reconnect_error(&log_source, &message).await;
                        if wait_for_retry(&mut stop_rx, retry).await {
                            break;
                        }
                        retry = (retry + 1).min(RECONNECT_BACKOFF.len() - 1);
                        continue;
                    }
                    MaterializeStartOutcome::Stopped => break,
                }
            }
            AttachmentProvenance::Resumed => None,
        };
        let id = thread.id().to_string();
        // Only now is a freshly started id durable: either its naming RPC
        // completed, or a successful resume authoritatively proved that an
        // ambiguous response had materialized it.
        thread_id = Some(id.clone());
        let mut events = match thread.events().await {
            Ok(events) => events,
            Err(error) => {
                let message = error.to_string();
                let pending = mark_disconnected(&runtime, Some(message.clone()));
                resolve_pending(&log_source, pending, "connection_lost").await;
                write_reconnect_error(&log_source, &message).await;
                if wait_for_retry(&mut stop_rx, retry).await {
                    break;
                }
                retry = (retry + 1).min(RECONNECT_BACKOFF.len() - 1);
                continue;
            }
        };

        {
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
                last_agent_messages: HashMap::new(),
                pending: HashMap::new(),
                applied_name_generation,
            });
        }
        schedule_name_reconciliation(agent_id, &runtime, stop_rx.clone());
        let resumed =
            take_initial_resumed_marker(&mut initial_persisted_resume_pending, provenance);
        log_source.write(ready_row(resumed)).await;
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
                    Ok(Some(event)) => ingest_event(
                        &runtime,
                        &log_source,
                        completion_sink.as_ref(),
                        event,
                    ).await,
                    Ok(None) => break Some("connection_lost"),
                    Err(CodexError::ThreadQueueOverflow(_)) => break Some("queue_overflow"),
                    Err(_) => break Some("event_stream_error"),
                }
            }
        };

        let pending = mark_disconnected(&runtime, None);
        resolve_pending(&log_source, pending, boundary.unwrap_or("session_stopped")).await;
        let Some(reason) = boundary else {
            break;
        };
        log_source
            .write(json!({"type": "amux.codex_gap", "reason": reason}))
            .await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachmentProvenance {
    Started,
    Resumed,
}

fn take_initial_resumed_marker(
    initial_persisted_resume_pending: &mut bool,
    provenance: AttachmentProvenance,
) -> bool {
    let resumed = *initial_persisted_resume_pending && provenance == AttachmentProvenance::Resumed;
    *initial_persisted_resume_pending = false;
    resumed
}

fn ready_row(resumed: bool) -> Value {
    if resumed {
        json!({"type": "amux.codex_ready", "resumed": true})
    } else {
        json!({"type": "amux.codex_ready"})
    }
}

struct MaterializedStart {
    thread: Thread,
    applied_name_generation: Option<u64>,
}

enum MaterializeStartOutcome {
    Ready(MaterializedStart),
    TransportLost {
        candidate_thread_id: String,
        message: String,
    },
    Stopped,
}

/// Keep a fresh thread private until its rollout exists. A failed naming
/// response is ambiguous, so resume is the authority: success commits this
/// same id; RPC failure leaves the original in-memory thread available for
/// retry, while transport loss returns the candidate id to the reconnecting
/// supervisor.
async fn materialize_started_thread(
    client: &Codex,
    agent_id: Uuid,
    thread_config: &ThreadConfig,
    thread: Thread,
    runtime: &Arc<StdMutex<CodexRuntime>>,
    log_source: &StructuredLogSource,
    stop_rx: &mut watch::Receiver<bool>,
) -> MaterializeStartOutcome {
    let mut retry = 0_usize;
    let mut registration_replaced = false;
    loop {
        let (desired_name, generation) = {
            let state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
            (state.desired_name.clone(), state.desired_name_generation)
        };
        let label = thread_name_for(desired_name.as_deref(), agent_id);
        let naming = tokio::select! {
            result = set_thread_name(client, agent_id, thread.id(), &label) => Some(result),
            changed = stop_rx.changed() => {
                let _ = changed;
                None
            }
        };
        let Some(naming) = naming else {
            return MaterializeStartOutcome::Stopped;
        };
        match naming {
            Ok(()) => {
                if registration_replaced {
                    let resumed = tokio::select! {
                        result = client.resume_thread(thread.id(), thread_config.clone()) => Some(result),
                        changed = stop_rx.changed() => {
                            let _ = changed;
                            None
                        }
                    };
                    let Some(resumed) = resumed else {
                        return MaterializeStartOutcome::Stopped;
                    };
                    match resumed {
                        Ok(thread) => {
                            return MaterializeStartOutcome::Ready(MaterializedStart {
                                thread,
                                applied_name_generation: Some(generation),
                            });
                        }
                        Err(CodexError::TransportClosed) => {
                            return MaterializeStartOutcome::TransportLost {
                                candidate_thread_id: thread.id().to_string(),
                                message: format!(
                                    "Codex transport closed while restoring fresh thread {} after naming",
                                    thread.id()
                                ),
                            };
                        }
                        Err(error) => {
                            let message = format!(
                                "fresh Codex thread was named but its event registration could \
                                 not be restored with thread/resume: {error}"
                            );
                            runtime
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .startup_error = Some(message.clone());
                            write_reconnect_error(log_source, &message).await;
                        }
                    }
                } else {
                    return MaterializeStartOutcome::Ready(MaterializedStart {
                        thread,
                        applied_name_generation: Some(generation),
                    });
                }
            }
            Err(CodexError::TransportClosed) => {
                return MaterializeStartOutcome::TransportLost {
                    candidate_thread_id: thread.id().to_string(),
                    message: format!(
                        "Codex transport closed while materializing fresh thread {}",
                        thread.id()
                    ),
                };
            }
            Err(name_error) => {
                // codex-sdk replaces the thread's event registration before
                // issuing resume, even when that RPC fails. Once attempted,
                // a later naming success must resume again to obtain the live
                // registration that will be published.
                registration_replaced = true;
                let resume = tokio::select! {
                    result = client.resume_thread(thread.id(), thread_config.clone()) => Some(result),
                    changed = stop_rx.changed() => {
                        let _ = changed;
                        None
                    }
                };
                let Some(resume) = resume else {
                    return MaterializeStartOutcome::Stopped;
                };
                match resume {
                    Ok(resumed) => {
                        return MaterializeStartOutcome::Ready(MaterializedStart {
                            thread: resumed,
                            // Resume proves the rollout exists, but not which
                            // desired-name generation won an ambiguous reply.
                            applied_name_generation: None,
                        });
                    }
                    Err(CodexError::TransportClosed) => {
                        return MaterializeStartOutcome::TransportLost {
                            candidate_thread_id: thread.id().to_string(),
                            message: format!(
                                "Codex transport closed while checking materialization of fresh thread {}",
                                thread.id()
                            ),
                        };
                    }
                    Err(resume_error) => {
                        let message = format!(
                            "failed to materialize fresh Codex thread: {name_error}; \
                             authoritative resume check failed: {resume_error}"
                        );
                        runtime
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner())
                            .startup_error = Some(message.clone());
                        write_reconnect_error(log_source, &message).await;
                    }
                }
            }
        }
        if wait_for_retry(stop_rx, retry).await {
            return MaterializeStartOutcome::Stopped;
        }
        retry = (retry + 1).min(RECONNECT_BACKOFF.len() - 1);
    }
}

fn schedule_name_reconciliation(
    agent_id: Uuid,
    runtime: &Arc<StdMutex<CodexRuntime>>,
    stop_rx: watch::Receiver<bool>,
) {
    let should_start = {
        let mut state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
        let pending = state.attached.as_ref().is_some_and(|attached| {
            attached.live.is_some()
                && attached.applied_name_generation != Some(state.desired_name_generation)
        });
        if pending && !state.name_reconciler_running {
            state.name_reconciler_running = true;
            true
        } else {
            false
        }
    };
    if should_start {
        tokio::spawn(reconcile_thread_name(agent_id, runtime.clone(), stop_rx));
    }
}

async fn reconcile_thread_name(
    agent_id: Uuid,
    runtime: Arc<StdMutex<CodexRuntime>>,
    mut stop_rx: watch::Receiver<bool>,
) {
    let mut retry = 0_usize;
    loop {
        if *stop_rx.borrow() {
            break;
        }
        let target = {
            let mut state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
            let generation = state.desired_name_generation;
            let desired_name = state.desired_name.clone();
            let Some(attached) = state.attached.as_ref() else {
                state.name_reconciler_running = false;
                return;
            };
            if attached.applied_name_generation == Some(generation) {
                state.name_reconciler_running = false;
                return;
            }
            let Some(live) = attached.live.as_ref() else {
                state.name_reconciler_running = false;
                return;
            };
            (
                live.client.clone(),
                attached.thread_id.clone(),
                generation,
                thread_name_for(desired_name.as_deref(), agent_id),
            )
        };
        let (client, thread_id, generation, label) = target;
        let result = tokio::select! {
            result = set_thread_name(&client, agent_id, &thread_id, &label) => Some(result),
            changed = stop_rx.changed() => {
                let _ = changed;
                None
            }
        };
        let Some(result) = result else {
            break;
        };
        if result.is_ok() {
            let mut state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
            if let Some(attached) = state.attached.as_mut()
                && attached.thread_id == thread_id
            {
                attached.applied_name_generation = Some(generation);
            }
            retry = 0;
            continue;
        }
        if wait_for_retry(&mut stop_rx, retry).await {
            break;
        }
        retry = (retry + 1).min(RECONNECT_BACKOFF.len() - 1);
    }
    runtime
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .name_reconciler_running = false;
}

/// Connect, attach the persistent thread, and take its continuous event stream.
///
/// The provenance is load-bearing: a resumed thread has an authoritative
/// rollout already, while a started thread must remain private until naming
/// materializes it.
async fn attach_thread(
    shared_client: &CodexClient,
    thread_config: &ThreadConfig,
    thread_id: Option<&str>,
    ambiguous_started_thread_id: Option<&str>,
) -> Result<(Arc<CodexConnection>, Thread, AttachmentProvenance)> {
    let connection = shared_client.connection().await?;
    let (thread, provenance) = match thread_id {
        Some(thread_id) => {
            let thread = connection
                .client
                .resume_thread(thread_id, thread_config.clone())
                .await?;
            (thread, AttachmentProvenance::Resumed)
        }
        None => match ambiguous_started_thread_id {
            Some(candidate_id) => match connection
                .client
                .resume_thread(candidate_id, thread_config.clone())
                .await
            {
                Ok(thread) => (thread, AttachmentProvenance::Resumed),
                Err(error) if is_missing_rollout(&error) => (
                    connection
                        .client
                        .start_thread(thread_config.clone())
                        .await?,
                    AttachmentProvenance::Started,
                ),
                Err(error) => return Err(error.into()),
            },
            None => (
                connection
                    .client
                    .start_thread(thread_config.clone())
                    .await?,
                AttachmentProvenance::Started,
            ),
        },
    };
    Ok((connection, thread, provenance))
}

fn is_missing_rollout(error: &CodexError) -> bool {
    matches!(
        error,
        CodexError::Rpc { message, .. }
            if message.starts_with("no rollout found for thread id")
    )
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

/// Drop the connection-local handles, optionally recording why, and hand back
/// every ask that was pending on them.
///
/// The caller MUST resolve the returned request IDs: this is the only place
/// obligations are released, so dropping them would leave asks that no client
/// can ever see closed.
fn mark_disconnected(
    runtime: &Arc<StdMutex<CodexRuntime>>,
    error: Option<String>,
) -> Vec<RequestId> {
    let mut state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(error) = error {
        state.startup_error = Some(error);
    }
    let Some(attached) = state.attached.as_mut() else {
        return Vec::new();
    };
    attached.live = None;
    attached.active_turn_id = None;
    attached.last_agent_messages.clear();
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
    completion_sink: Option<&CodexCompletionSink>,
    event: ThreadEvent,
) {
    log_source.write(raw_row(&event)).await;
    match &event.event {
        TurnEvent::TurnStarted { turn } => {
            update_attached(runtime, |attached| {
                attached.active_turn_id = Some(turn.id.clone());
            });
        }
        TurnEvent::ItemCompleted(ThreadItem::AgentMessage { text, .. }) => {
            if let Some(turn_id) = event.turn_id.as_ref() {
                update_attached(runtime, |attached| {
                    attached
                        .last_agent_messages
                        .insert(turn_id.clone(), text.clone());
                });
            }
        }
        TurnEvent::TurnCompleted { turn } => {
            let last_message = {
                let mut state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
                state.attached.as_mut().and_then(|attached| {
                    if attached.active_turn_id.as_deref() == Some(&turn.id) {
                        attached.active_turn_id = None;
                    }
                    attached.last_agent_messages.remove(&turn.id)
                })
            };
            if let Some(text) = last_message
                && let Some(sink) = completion_sink
            {
                let _ = sink
                    .event_tx
                    .send(SessionEvent::Completed {
                        agent_id: sink.agent_id,
                        text,
                    })
                    .await;
            }
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

struct CodexDeliveryTarget {
    runtime: Arc<StdMutex<CodexRuntime>>,
    log_source: StructuredLogSource,
}

impl CodexDeliveryTarget {
    fn live_and_active(&self) -> Result<(CodexLive, bool)> {
        let state = self
            .runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let attached = state
            .attached
            .as_ref()
            .ok_or_else(|| anyhow!("Codex thread is not attached"))?;
        let live = attached
            .live
            .clone()
            .ok_or_else(|| anyhow!("Codex thread is read-only until reconnect succeeds"))?;
        Ok((live, attached.active_turn_id.is_some()))
    }

    async fn deliver_envelope(&self, envelope: &Envelope) -> Result<Delivery> {
        let text = crate::envelope::format(envelope);
        let (live, active) = self.live_and_active()?;
        let item = json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": text}],
        });

        let delivery = match live.thread.inject_items(vec![item]).await {
            Ok(()) if active => Delivery::InjectQueued,
            Ok(()) => {
                let turn_id = live.thread.start_empty_turn().await?;
                update_attached(&self.runtime, |attached| {
                    attached.active_turn_id = Some(turn_id);
                });
                Delivery::InjectStarted
            }
            Err(inject_error) => {
                tracing::warn!(
                    %inject_error,
                    envelope_id = %envelope.id,
                    "Codex message injection failed; starting a visible turn"
                );
                let turn_id = live.thread.start_turn(text).await?;
                update_attached(&self.runtime, |attached| {
                    attached.active_turn_id = Some(turn_id);
                });
                Delivery::TurnStarted
            }
        };

        self.log_source
            .write(codex_message_row(envelope, delivery))
            .await;
        Ok(delivery)
    }
}

#[async_trait]
impl AgentDeliveryTarget for CodexDeliveryTarget {
    fn liveness(&self) -> std::result::Result<DeliveryLiveness, DeliveryError> {
        let state = self
            .runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match state.attached.as_ref() {
            Some(attached) if attached.live.is_some() => Ok(DeliveryLiveness::Live),
            Some(_) => Ok(DeliveryLiveness::Pending(
                "Codex thread is read-only until reconnect succeeds".to_string(),
            )),
            None => Ok(DeliveryLiveness::Pending(
                "Codex thread is not attached".to_string(),
            )),
        }
    }

    async fn deliver(&self, envelope: &Envelope) -> std::result::Result<Delivery, DeliveryError> {
        self.deliver_envelope(envelope)
            .await
            .map_err(|error| DeliveryError::Failed(error.to_string()))
    }
}

fn codex_message_row(envelope: &Envelope, delivery: Delivery) -> Value {
    let (from, from_id) = match &envelope.from {
        Sender::Agent(agent) => (
            format!("{}/{}", agent.name, agent.host_id),
            Some(agent.agent_id),
        ),
        Sender::Human => ("human".to_string(), None),
    };
    json!({
        "type": "amux.codex_message",
        "id": envelope.id,
        "kind": envelope.kind,
        "from": from,
        "from_id": from_id,
        "context": envelope.context,
        "text": envelope.text,
        "delivery": delivery.carrier(),
    })
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
impl StructuredInput for CodexInputTarget {
    async fn send(&self, input: StructuredInputEvent) -> std::result::Result<(), ProtocolError> {
        let StructuredInputEvent::Codex { input_id, input } = input else {
            return Err(ProtocolError::InvalidArgument {
                message: "Codex input target received another protocol's input".to_string(),
            });
        };
        self.send(input_id, input).await;
        Ok(())
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
        {
            let mut runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            runtime.desired_name = name;
            runtime.desired_name_generation = runtime.desired_name_generation.wrapping_add(1);
        }
        schedule_name_reconciliation(self.agent_id, &self.runtime, self.stop_tx.subscribe());
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

    fn start(
        &mut self,
        event_tx: &tokio::sync::mpsc::Sender<SessionEvent>,
    ) -> Result<tokio::task::JoinHandle<()>> {
        if self.started {
            return Err(anyhow!("Codex session {} already started", self.agent_id));
        }
        self.started = true;
        self.start_task(self.stop_tx.subscribe(), event_tx)
    }

    async fn stop(&self, _policy: StopPolicy) {
        tracing::info!(agent_id = %self.agent_id, "stopping Codex session");
        self.stop_tx.send_replace(true);
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
        let pending = mark_disconnected(&self.runtime, None);
        resolve_pending(&self.log_source, pending, "session_stopped").await;
        self.log_source.close().await;
    }

    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn plane(&self, protocol: Protocol) -> std::result::Result<Plane, ProtocolError> {
        match protocol {
            Protocol::TerminalV1 => Ok(Plane::Terminal(RawPtyTarget::Codex(
                self.owned_raw_pty_target(),
            ))),
            Protocol::CodexSdkV1 => Ok(Plane::Structured {
                log: self.log_source.clone(),
                input: Box::new(self.input_target()),
            }),
            Protocol::ClaudePtyTranscriptV1 | Protocol::ClaudeSdkV1 | Protocol::TestEchoV1 => {
                Err(ProtocolError::NotExposed {
                    kind: self.kind(),
                    protocol,
                })
            }
        }
    }

    fn spawn_inheritance(&self) -> SpawnInheritance {
        SpawnInheritance {
            codex_approval_policy: self.approval_policy.clone(),
            codex_sandbox_policy: self.sandbox_policy.clone(),
            ..SpawnInheritance::default()
        }
    }

    fn parent(&self) -> Option<AgentParent> {
        self.parent
    }

    fn delivery_target(&self) -> Box<dyn AgentDeliveryTarget> {
        Box::new(CodexDeliveryTarget {
            runtime: self.runtime.clone(),
            log_source: self.log_source.clone(),
        })
    }

    fn suspended_state(&self) -> Result<SuspendedAgent> {
        let (thread_id, daemon_mode) = {
            let runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let thread_id = runtime
                .attached
                .as_ref()
                .map(|attached| attached.thread_id.clone())
                .or_else(|| self.resume_thread_id.clone());
            let Some(thread_id) = thread_id else {
                return Err(anyhow!(
                    "cannot suspend Codex agent {}: thread_id is not available yet",
                    self.agent_id
                ));
            };
            let daemon_mode = runtime
                .attached
                .as_ref()
                .and_then(|attached| attached.daemon_mode.clone())
                .or_else(|| runtime.resume_daemon_mode.clone());
            (thread_id, daemon_mode)
        };
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
            parent: self.parent,
            working_on: None,
        })
    }

    fn debug_json(&self, _verbose: bool) -> serde_json::Result<Value> {
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        Ok(json!({
            "kind": "codex",
            "thread_id": runtime.attached.as_ref().map(|attached| &attached.thread_id).or(self.resume_thread_id.as_ref()),
            "daemon_mode": runtime.attached.as_ref().and_then(|attached| attached.daemon_mode.as_deref()).or(runtime.resume_daemon_mode.as_deref()),
            "startup_error": runtime.startup_error,
            "has_event_ingest": runtime.ingest_abort.is_some(),
            "connected": runtime.attached.as_ref().is_some_and(|attached| attached.live.is_some()),
            "has_pty": runtime.pty.is_some(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use futures_util::{SinkExt, StreamExt};
    use replay_support::{
        IoDirection, ReplayAdvance, ReplayOptions, load_script, replay_transport_with_controller,
    };
    use tokio::io::{
        AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf, duplex, split,
    };
    use tokio::net::UnixListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    use super::*;
    use crate::agents::AgentType;
    use crate::envelope::{AgentSender, EnvelopeKind};

    fn session_request() -> CreateAgentRequest {
        CreateAgentRequest {
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
            parent: None,
            initial_prompt: None,
        }
    }

    fn session() -> CodexSession {
        let req = session_request();
        CodexSession::new(
            &req,
            Arc::new(CodexClient::new(PathBuf::from("/tmp/amux-codex.sock"))),
            crate::agents::mcp_launch_route_for_tests(Uuid::from_u128(10)),
        )
    }

    fn file_backed_session() -> (tempfile::TempDir, CodexSession, Value) {
        let temporary = tempfile::tempdir_in("/tmp").unwrap();
        let config_path = temporary.path().join("amux.yaml");
        std::fs::write(&config_path, "socket_path: daemon.sock\n").unwrap();
        let executable = std::env::current_exe().unwrap();
        let socket_path = temporary.path().join("daemon.sock");
        let host_id = Uuid::from_u128(12);
        let route = McpLaunchRoute::new(
            executable.clone(),
            Some(config_path.clone()),
            socket_path.clone(),
            host_id,
        )
        .unwrap();
        let request = session_request();
        let session = CodexSession::new(
            &request,
            Arc::new(CodexClient::new(temporary.path().join("codex.sock"))),
            route,
        );
        let expected = json!({
            "mcp_servers": {
                "amux": {
                    "command": executable.to_str().unwrap(),
                    "args": ["mcp", "agent", "--socket-path", socket_path.to_str().unwrap()],
                    "env": {
                        "AMUX_AGENT_ID": request.agent_id,
                        "AMUX_HOST_ID": host_id,
                        "AMUX_CONFIG": config_path.to_str().unwrap(),
                    },
                    "enabled": true,
                    "required": true,
                    "startup_timeout_sec": 10,
                    "tool_timeout_sec": 60,
                    "default_tools_approval_mode": "approve",
                    "enabled_tools": ["agents", "send", "spawn", "stop", "status"],
                }
            }
        });
        (temporary, session, expected)
    }

    fn assert_managed_thread_request(request: &Value, method: &str, expected_config: &Value) {
        assert_eq!(request["method"], method);
        assert_eq!(&request["params"]["config"], expected_config);
        assert!(
            request["params"].get("dynamicTools").is_none(),
            "amux-owned Codex threads must not register dynamic tools"
        );
    }

    #[test]
    fn managed_codex_fresh_and_suspended_sessions_keep_the_exact_mcp_route() {
        let request = session_request();
        let route = crate::agents::mcp_launch_route_for_tests(Uuid::from_u128(12));
        let fresh = CodexSession::new(
            &request,
            Arc::new(CodexClient::new(PathBuf::from("/tmp/amux-codex.sock"))),
            route.clone(),
        );
        let suspended = CodexSession::from_suspended(
            &request,
            Arc::new(CodexClient::new(PathBuf::from("/tmp/amux-codex.sock"))),
            route.clone(),
            Some("spawned-private".to_string()),
            Utc::now(),
        );

        assert_eq!(fresh.mcp_launch_route, route);
        assert_eq!(suspended.mcp_launch_route, route);
    }

    #[test]
    fn a2a_codex_thread_config_owns_the_required_definition_derived_mcp_policy() {
        let (_temporary, session, expected) = file_backed_session();
        let config = session.thread_config().unwrap();

        assert_eq!(config.config.as_ref(), expected.as_object());
        assert!(config.dynamic_tools.is_none());
        let names = agent_tools::definitions()
            .into_iter()
            .map(|tool| Value::String(tool.name.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(
            config.config.as_ref().unwrap()["mcp_servers"]["amux"]["enabled_tools"],
            Value::Array(names)
        );
    }

    #[test]
    fn a2a_codex_true_default_mcp_route_omits_only_amux_config() {
        let session = session();
        let config = session.thread_config().unwrap();
        let environment = &config.config.as_ref().unwrap()["mcp_servers"]["amux"]["env"];

        assert!(environment.get("AMUX_CONFIG").is_none());
        assert_eq!(environment["AMUX_AGENT_ID"], session.agent_id.to_string());
        assert_eq!(
            environment["AMUX_HOST_ID"],
            session.mcp_launch_route.host_id().to_string()
        );
    }

    type MockReader = BufReader<ReadHalf<DuplexStream>>;
    type MockWriter = WriteHalf<DuplexStream>;

    #[test]
    fn ready_rows_preserve_the_frozen_method_and_optional_resumed_fact() {
        assert_eq!(ready_row(false), json!({"type": "amux.codex_ready"}));
        assert_eq!(
            ready_row(true),
            json!({"type": "amux.codex_ready", "resumed": true})
        );
    }

    #[test]
    fn initial_persisted_resume_marker_is_one_shot_and_not_inferred() {
        let mut initial_persisted = true;
        assert!(take_initial_resumed_marker(
            &mut initial_persisted,
            AttachmentProvenance::Resumed
        ));
        assert!(!take_initial_resumed_marker(
            &mut initial_persisted,
            AttachmentProvenance::Resumed
        ));

        let mut fresh = false;
        assert!(!take_initial_resumed_marker(
            &mut fresh,
            AttachmentProvenance::Started
        ));
        assert!(!take_initial_resumed_marker(
            &mut fresh,
            AttachmentProvenance::Resumed
        ));

        let mut missing_persisted_thread = true;
        assert!(!take_initial_resumed_marker(
            &mut missing_persisted_thread,
            AttachmentProvenance::Started
        ));
        assert!(!take_initial_resumed_marker(
            &mut missing_persisted_thread,
            AttachmentProvenance::Resumed
        ));
    }

    async fn mock_codex() -> (Codex, MockReader, MockWriter) {
        let (client_side, server_side) = duplex(32 * 1024);
        let (client_read, client_write) = split(client_side);
        let (server_read, mut server_write) = split(server_side);
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let initialize = read_request(&mut reader).await;
            write_response(
                &mut server_write,
                &initialize,
                json!({
                    "userAgent": "test/0.147.0",
                    "codexHome": "/tmp/test-codex-home",
                    "platformFamily": "unix",
                    "platformOs": "test"
                }),
            )
            .await;
            assert_eq!(read_request(&mut reader).await["method"], "initialized");
            (reader, server_write)
        });
        let client = Codex::from_io(
            BufReader::new(client_read),
            client_write,
            CodexConfig::default(),
        )
        .await
        .unwrap();
        let (reader, writer) = server.await.unwrap();
        (client, reader, writer)
    }

    async fn read_request(reader: &mut MockReader) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(!line.is_empty(), "mock Codex transport closed");
        serde_json::from_str(line.trim()).unwrap()
    }

    async fn write_response(writer: &mut MockWriter, request: &Value, result: Value) {
        writer
            .write_all(format!("{}\n", json!({"id": request["id"], "result": result})).as_bytes())
            .await
            .unwrap();
    }

    async fn write_rpc_error(writer: &mut MockWriter, request: &Value, message: &str) {
        writer
            .write_all(
                format!(
                    "{}\n",
                    json!({"id": request["id"], "error": {"code": -32600, "message": message}})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    }

    fn thread_session(thread_id: &str, name: Option<&str>) -> Value {
        json!({
            "thread": {
                "id": thread_id,
                "path": null,
                "agentNickname": null,
                "agentRole": null,
                "gitInfo": null,
                "name": name
            },
            "model": "test",
            "modelProvider": "openai",
            "serviceTier": null,
            "cwd": "/tmp",
            "approvalPolicy": "on-request",
            "approvalsReviewer": "user",
            "sandbox": {"type": "readOnly", "networkAccess": false},
            "reasoningEffort": null
        })
    }

    async fn start_mock_thread(
        client: &Codex,
        reader: &mut MockReader,
        writer: &mut MockWriter,
    ) -> Thread {
        let starting_client = client.clone();
        let start = tokio::spawn(async move {
            starting_client
                .start_thread(ThreadConfig::default())
                .await
                .unwrap()
        });
        let request = read_request(reader).await;
        assert_eq!(request["method"], "thread/start");
        write_response(writer, &request, thread_session("thread-1", None)).await;
        start.await.unwrap()
    }

    fn delivery_envelope() -> Envelope {
        Envelope {
            id: Uuid::from_u128(41),
            context: Some(Uuid::from_u128(42)),
            from: Sender::Agent(AgentSender {
                agent_id: Uuid::from_u128(43),
                host_id: Uuid::from_u128(44),
                name: "sender".into(),
                kind: "claude".into(),
            }),
            to: AgentParent {
                agent_id: Uuid::from_u128(1),
                host_id: Uuid::from_u128(45),
            },
            kind: EnvelopeKind::Message,
            text: "hello from another agent".into(),
        }
    }

    async fn start_delivery_replay(
        fixture: &str,
        injected_text: &str,
    ) -> (Codex, Thread, tokio::task::JoinHandle<()>) {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex_backend")
            .join(fixture);
        let mut script = load_script(fixture);
        for event in script
            .iter_mut()
            .filter(|event| event.direction == IoDirection::Write)
        {
            let mut request: Value = serde_json::from_str(&event.line).expect("request is JSON");
            match request["method"].as_str() {
                Some("initialize") => {
                    request["params"]["clientInfo"]["title"] = Value::Null;
                    event.line = request.to_string();
                }
                Some("thread/inject_items") => {
                    request["params"]["items"][0]["content"][0]["text"] =
                        Value::String(injected_text.to_string());
                    event.line = request.to_string();
                }
                _ => {}
            }
        }
        let (reader, writer, controller) =
            replay_transport_with_controller(script, ReplayOptions::default());
        let driver = tokio::spawn(async move {
            while let ReplayAdvance::Advanced { .. } | ReplayAdvance::BlockedOnWrite =
                controller.advance_one().await
            {
                tokio::task::yield_now().await;
            }
        });
        let client = tokio::time::timeout(
            Duration::from_secs(2),
            Codex::from_io(
                reader,
                writer,
                CodexConfig {
                    client_name: "amux-a2a-capture".into(),
                    client_version: "0.1.0".into(),
                    ..CodexConfig::default()
                },
            ),
        )
        .await
        .expect("initialize replay timed out")
        .expect("initialize replay failed");
        let thread = tokio::time::timeout(
            Duration::from_secs(2),
            client.start_thread(ThreadConfig {
                cwd: Some("[SCRATCH]/project".into()),
                model: Some("gpt-5.6-sol".into()),
                ..ThreadConfig::default()
            }),
        )
        .await
        .expect("thread/start replay timed out")
        .expect("thread/start replay failed");
        (client, thread, driver)
    }

    async fn assert_delivery_row(session: &CodexSession, delivery: Delivery) {
        let (mut rows, count) = session
            .log_source
            .subscribe_with_query(None)
            .await
            .expect("structured log subscription");
        assert_eq!(count, 1);
        assert_eq!(
            rows.read().await.expect("delivery row").payload,
            json!({
                "type": "amux.codex_message",
                "id": Uuid::from_u128(41),
                "kind": "message",
                "from": format!("sender/{}", Uuid::from_u128(44)),
                "from_id": Uuid::from_u128(43),
                "context": Uuid::from_u128(42),
                "text": "hello from another agent",
                "delivery": delivery.carrier(),
            })
        );
    }

    #[tokio::test]
    async fn a2a_codex_dynamic_tool_requests_remain_generic_and_are_never_executed() {
        let (client, mut reader, mut writer) = mock_codex().await;
        let request = session_request();
        let session = CodexSession::new(
            &request,
            Arc::new(CodexClient::new(PathBuf::from("/tmp/amux-codex.sock"))),
            crate::agents::mcp_launch_route_for_tests(Uuid::from_u128(10)),
        );

        let starting_client = client.clone();
        let config = session.thread_config().unwrap();
        let start =
            tokio::spawn(async move { starting_client.start_thread(config).await.unwrap() });
        let request = read_request(&mut reader).await;
        assert_eq!(request["method"], "thread/start");
        assert!(request["params"].get("dynamicTools").is_none());
        assert!(
            request["params"]
                .pointer("/config/mcp_servers/amux")
                .is_some()
        );
        write_response(&mut writer, &request, thread_session("thread-1", None)).await;
        let thread = start.await.unwrap();
        let mut events = thread.events().await.unwrap();
        attach_runtime(&session.runtime, &client, &thread);

        writer
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "method": "item/tool/call",
                        "id": 77,
                        "params": {
                            "threadId": "thread-1",
                            "turnId": "turn-1",
                            "callId": "call-1",
                            "namespace": null,
                            "tool": "send",
                            "arguments": {"to": "peer", "text": "hello"}
                        }
                    })
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let event = events.next().await.unwrap().unwrap();
        ingest_event(&session.runtime, &session.log_source, None, event).await;

        assert!(matches!(
            session
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .attached
                .as_ref()
                .unwrap()
                .pending
                .get(&RequestId::Integer(77)),
            Some(PendingRequestKind::ToolCall)
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), read_request(&mut reader))
                .await
                .is_err(),
            "the retired amux callback path answered the dynamic request"
        );
        client.close().await;
    }

    #[tokio::test]
    async fn a2a_codex_deliver_idle_injects_then_starts_empty_turn() {
        let envelope = delivery_envelope();
        let tagged = crate::envelope::format(&envelope);
        let (client, thread, driver) =
            start_delivery_replay("a2a_inject_idle.io.jsonl", &tagged).await;
        let session = session();
        attach_runtime(&session.runtime, &client, &thread);

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), session.deliver(&envelope))
                .await
                .expect("delivery replay timed out")
                .expect("delivery failed"),
            Delivery::InjectStarted
        );
        assert!(
            session
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .attached
                .as_ref()
                .expect("attached runtime")
                .active_turn_id
                .is_some()
        );
        assert_delivery_row(&session, Delivery::InjectStarted).await;

        tokio::time::timeout(Duration::from_secs(2), driver)
            .await
            .expect("replay driver timed out")
            .expect("replay driver failed");
        client.close().await;
    }

    #[tokio::test]
    async fn a2a_codex_deliver_busy_injects_without_starting_another_turn() {
        let envelope = delivery_envelope();
        let tagged = crate::envelope::format(&envelope);
        let (client, thread, driver) =
            start_delivery_replay("a2a_inject_busy.io.jsonl", &tagged).await;
        let session = session();
        attach_runtime(&session.runtime, &client, &thread);
        let active_turn = thread
            .start_turn("Think carefully for a moment, then reply exactly C13_INITIAL.")
            .await
            .expect("initial turn/start replay failed");
        update_attached(&session.runtime, |attached| {
            attached.active_turn_id = Some(active_turn.clone());
        });

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), session.deliver(&envelope))
                .await
                .expect("delivery replay timed out")
                .expect("delivery failed"),
            Delivery::InjectQueued
        );
        assert_eq!(
            session
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .attached
                .as_ref()
                .expect("attached runtime")
                .active_turn_id
                .as_deref(),
            Some(active_turn.as_str())
        );
        assert_delivery_row(&session, Delivery::InjectQueued).await;

        tokio::time::timeout(Duration::from_secs(2), driver)
            .await
            .expect("replay driver timed out")
            .expect("replay driver failed");
        client.close().await;
    }

    #[tokio::test]
    async fn a2a_codex_deliver_falls_back_to_a_visible_turn() {
        let (client, mut reader, mut writer) = mock_codex().await;
        let thread = start_mock_thread(&client, &mut reader, &mut writer).await;
        let session = session();
        attach_runtime(&session.runtime, &client, &thread);
        let envelope = delivery_envelope();
        let tagged = crate::envelope::format(&envelope);
        let delivery = tokio::spawn({
            let target = session.delivery_target();
            let envelope = envelope.clone();
            async move { target.deliver(&envelope).await }
        });

        let inject = read_request(&mut reader).await;
        assert_eq!(inject["method"], "thread/inject_items");
        assert_eq!(inject["params"]["items"][0]["content"][0]["text"], tagged);
        write_rpc_error(&mut writer, &inject, "injection unavailable").await;
        let fallback = read_request(&mut reader).await;
        assert_eq!(fallback["method"], "turn/start");
        assert_eq!(fallback["params"]["input"][0]["text"], tagged);
        write_response(
            &mut writer,
            &fallback,
            json!({"turn": {"id": "fallback-turn"}}),
        )
        .await;

        assert_eq!(
            delivery.await.expect("delivery task panicked").unwrap(),
            Delivery::TurnStarted
        );
        assert_eq!(
            session
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .attached
                .as_ref()
                .expect("attached runtime")
                .active_turn_id
                .as_deref(),
            Some("fallback-turn")
        );
        assert_delivery_row(&session, Delivery::TurnStarted).await;
        client.close().await;
    }

    #[tokio::test]
    async fn authentication_preflight_uses_the_shared_private_fallback_connection() {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let codex_home = temp.path().join("codex-home");
        tokio::fs::create_dir_all(codex_home.join("app-server-control"))
            .await
            .unwrap();

        // An unrelated listener occupies the well-known path. The backend's
        // supported behavior is to use its private socket instead.
        let well_known = UnixListener::bind(daemon_socket_path(&codex_home)).unwrap();
        let occupied = tokio::spawn(async move {
            let (stream, _) = well_known.accept().await.unwrap();
            drop(stream);
        });

        let private_socket = temp.path().join("private.sock");
        let private = UnixListener::bind(&private_socket).unwrap();
        let app_server = tokio::spawn(async move {
            // The fallback helper probes an existing private listener before
            // the real client performs its initialize handshake.
            let (probe, _) = private.accept().await.unwrap();
            let mut probe = accept_async(probe).await.unwrap();
            let _ = probe.next().await;

            let (stream, _) = private.accept().await.unwrap();
            let mut websocket = accept_async(stream).await.unwrap();
            let Message::Text(initialize) = websocket.next().await.unwrap().unwrap() else {
                panic!("initialize was not a text frame");
            };
            let initialize: Value = serde_json::from_str(&initialize).unwrap();
            websocket
                .send(Message::Text(
                    json!({
                        "id": initialize["id"],
                        "result": {
                            "userAgent": "test/0.147.0",
                            "codexHome": "/tmp/test-codex-home",
                            "platformFamily": "unix",
                            "platformOs": "test"
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();

            let Message::Text(initialized) = websocket.next().await.unwrap().unwrap() else {
                panic!("initialized was not a text frame");
            };
            assert_eq!(
                serde_json::from_str::<Value>(&initialized).unwrap()["method"],
                "initialized"
            );

            let Message::Text(account_read) = websocket.next().await.unwrap().unwrap() else {
                panic!("account/read was not a text frame");
            };
            let account_read: Value = serde_json::from_str(&account_read).unwrap();
            assert_eq!(account_read["method"], "account/read");
            websocket
                .send(Message::Text(
                    json!({
                        "id": account_read["id"],
                        "result": {
                            "account": null,
                            "requiresOpenaiAuth": false
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        });

        let client = CodexClient::new(private_socket.clone());
        client
            .ensure_authenticated_with_codex_home(&codex_home)
            .await
            .unwrap();
        let connection = client.connection.lock().await;
        let connection = connection.as_ref().expect("cached shared connection");
        assert_eq!(connection.mode, "existing-private");
        assert_eq!(
            connection.socket_path,
            private_socket.canonicalize().unwrap()
        );

        occupied.await.unwrap();
        app_server.await.unwrap();
    }

    #[test]
    fn unauthenticated_account_names_the_recovery_command() {
        let error = require_account(false, true).unwrap_err().to_string();
        assert!(error.contains("codex login"));
        assert!(require_account(true, true).is_ok());
        assert!(require_account(false, false).is_ok());
    }

    #[test]
    fn advertises_both_planes_before_pty_exists() {
        let session = session();
        assert!(session.runtime.lock().unwrap().pty.is_none());
        assert!(matches!(
            session.plane(Protocol::TerminalV1),
            Ok(Plane::Terminal(_))
        ));
        assert!(matches!(
            session.plane(Protocol::CodexSdkV1),
            Ok(Plane::Structured { .. })
        ));
    }

    fn test_pty_spawn(aborts: &mut Vec<AbortHandle>) -> (PtyHandle, tokio::task::JoinHandle<()>) {
        let handle = PtyHandle::test_echo();
        let exit = tokio::spawn(std::future::pending::<()>());
        aborts.push(exit.abort_handle());
        (handle, exit)
    }

    #[tokio::test]
    async fn one_of_two_raw_leases_keeps_pty_alive_and_final_drop_retires_it() {
        let session = session();
        let mut aborts = Vec::new();
        let first = acquire_test_raw_pty_lease(session.agent_id, &session.runtime, |_| {
            Ok(test_pty_spawn(&mut aborts))
        })
        .unwrap();
        let second = acquire_test_raw_pty_lease(session.agent_id, &session.runtime, |_| {
            panic!("second subscriber must share the cached PTY")
        })
        .unwrap();
        let mut output = first.handle().subscribe_with_query(None).await.unwrap();

        drop(first);
        {
            let state = session.runtime.lock().unwrap();
            let pty = state.pty.as_ref().expect("one subscriber remains");
            assert_eq!(pty.subscribers, 1);
        }
        second
            .handle()
            .send_input(b"still-live".to_vec())
            .await
            .unwrap();
        assert_eq!(output.read().await.unwrap(), b"still-live");

        drop(second);
        assert!(session.runtime.lock().unwrap().pty.is_none());
        let final_output = tokio::time::timeout(Duration::from_secs(1), output.read())
            .await
            .expect("final detach did not initiate PTY termination");
        assert!(final_output.is_none(), "terminated PTY output stayed open");
        for abort in aborts {
            abort.abort();
        }
    }

    #[tokio::test]
    async fn stale_raw_detach_and_exit_cannot_clear_a_newer_epoch() {
        let session = session();
        let mut aborts = Vec::new();
        let old = acquire_test_raw_pty_lease(session.agent_id, &session.runtime, |_| {
            Ok(test_pty_spawn(&mut aborts))
        })
        .unwrap();
        let old_epoch = old.epoch;
        let new_handle = PtyHandle::test_echo();
        let new_epoch = old_epoch.wrapping_add(1);
        {
            let mut state = session.runtime.lock().unwrap();
            state.pty = Some(CodexPty {
                handle: new_handle.clone(),
                epoch: new_epoch,
                subscribers: 1,
            });
        }
        let new = CodexRawPtyLease {
            handle: new_handle,
            epoch: new_epoch,
            agent_id: session.agent_id,
            runtime: session.runtime.clone(),
        };

        drop(old);
        clear_cached_pty_epoch(&session.runtime, old_epoch);
        assert_eq!(
            session.runtime.lock().unwrap().pty.as_ref().unwrap().epoch,
            new_epoch
        );

        drop(new);
        assert!(session.runtime.lock().unwrap().pty.is_none());
        for abort in aborts {
            abort.abort();
        }
    }

    #[tokio::test]
    async fn raw_reattach_after_final_drop_constructs_a_fresh_pty() {
        let session = session();
        let mut aborts = Vec::new();
        let mut spawns = 0;
        let first = acquire_test_raw_pty_lease(session.agent_id, &session.runtime, |_| {
            spawns += 1;
            Ok(test_pty_spawn(&mut aborts))
        })
        .unwrap();
        let first_epoch = first.epoch;
        drop(first);
        assert!(session.runtime.lock().unwrap().pty.is_none());

        let second = acquire_test_raw_pty_lease(session.agent_id, &session.runtime, |_| {
            spawns += 1;
            Ok(test_pty_spawn(&mut aborts))
        })
        .unwrap();
        assert_eq!(spawns, 2);
        assert_ne!(second.epoch, first_epoch);

        drop(second);
        for abort in aborts {
            abort.abort();
        }
    }

    #[tokio::test]
    async fn raw_spawn_failure_before_thread_ready_leaves_structured_plane_healthy() {
        let session = session();

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            session.owned_raw_pty_target().acquire_lease(),
        )
        .await
        .expect("raw PTY readiness check timed out")
        .err()
        .unwrap();
        assert!(error.to_string().contains("thread_id is not available"));
        assert!(session.runtime.lock().unwrap().pty.is_none());
        assert!(session.log_source.subscribe().await.is_some());
    }

    #[tokio::test]
    async fn raw_target_snapshot_cannot_prepare_after_its_session_stops() {
        let session = session();
        let target = session.owned_raw_pty_target();
        session.stop_tx.send_replace(true);

        let error = tokio::time::timeout(Duration::from_secs(1), target.acquire_lease())
            .await
            .expect("stopped raw target check timed out")
            .err()
            .expect("stopped raw target unexpectedly acquired a lease");
        assert!(error.to_string().contains("stopped during preparation"));
        assert!(session.runtime.lock().unwrap().pty.is_none());
    }

    #[test]
    fn thread_label_policy_uses_agent_name_or_stable_bootstrap_label() {
        let agent_id = Uuid::from_u128(0xfeed_face);
        assert_eq!(thread_name_for(None, agent_id), "amux-00000000");
        assert_eq!(thread_name_for(Some(""), agent_id), "amux-00000000");
        assert_eq!(thread_name_for(Some("morning"), agent_id), "morning");

        // The bootstrap label uses the clients' short-id convention.
        let id = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        assert_eq!(
            bootstrap_thread_name(id),
            format!("amux-{}", &id.simple().to_string()[..8])
        );
    }

    #[tokio::test]
    async fn resumed_thread_stays_attached_when_name_reconciliation_fails() {
        let (client, mut reader, mut writer) = mock_codex().await;
        let thread = start_mock_thread(&client, &mut reader, &mut writer).await;
        let session = session();
        {
            let mut state = session.runtime.lock().unwrap();
            state.desired_name = Some("wanted".into());
            state.desired_name_generation = 1;
            state.attached = Some(CodexAttached {
                thread_id: thread.id().into(),
                daemon_mode: Some("test".into()),
                live: Some(CodexLive {
                    client: client.clone(),
                    thread,
                    socket_path: PathBuf::from("/tmp/missing-test-codex.sock"),
                }),
                active_turn_id: None,
                last_agent_messages: HashMap::new(),
                pending: HashMap::new(),
                applied_name_generation: None,
            });
        }
        schedule_name_reconciliation(
            session.agent_id,
            &session.runtime,
            session.stop_tx.subscribe(),
        );
        let rename = read_request(&mut reader).await;
        assert_eq!(rename["method"], "thread/name/set");
        write_rpc_error(&mut writer, &rename, "injected name failure").await;
        tokio::task::yield_now().await;

        {
            let state = session.runtime.lock().unwrap();
            let attached = state.attached.as_ref().expect("resume stays published");
            assert!(
                attached.live.is_some(),
                "name failure must not detach resume"
            );
        }
        let acquisition = tokio::time::timeout(
            Duration::from_secs(1),
            session.owned_raw_pty_target().acquire_lease(),
        )
        .await
        .expect("raw PTY socket check timed out");
        let error = match acquisition {
            Ok(_) => panic!("missing mock socket must fail after reaching raw spawn path"),
            Err(error) => error.to_string(),
        };
        assert!(!error.contains(CODEX_RAW_THREAD_NOT_READY), "{error}");
        assert!(!error.contains("persist"), "{error}");
        session.stop_tx.send(true).unwrap();
        client.close().await;
    }

    #[tokio::test]
    async fn rename_during_fresh_materialization_is_reconciled_after_publish() {
        let (client, mut reader, mut writer) = mock_codex().await;
        let thread = start_mock_thread(&client, &mut reader, &mut writer).await;
        let runtime = Arc::new(StdMutex::new(CodexRuntime {
            desired_name: Some("bootstrap-snapshot".into()),
            desired_name_generation: 0,
            name_reconciler_running: false,
            attached: None,
            resume_daemon_mode: None,
            startup_error: None,
            ingest_abort: None,
            pty: None,
            next_pty_epoch: 0,
        }));
        let source = StructuredLogSource::new(8);
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let materialize_client = client.clone();
        let materialize_runtime = runtime.clone();
        let materialize_source = source.clone();
        let materialize = tokio::spawn(async move {
            match materialize_started_thread(
                &materialize_client,
                Uuid::from_u128(1),
                &ThreadConfig::default(),
                thread,
                &materialize_runtime,
                &materialize_source,
                &mut stop_rx,
            )
            .await
            {
                MaterializeStartOutcome::Ready(materialized) => materialized,
                _ => panic!("materialization did not complete"),
            }
        });

        let first = read_request(&mut reader).await;
        assert_eq!(first["params"]["name"], "bootstrap-snapshot");
        {
            let mut state = runtime.lock().unwrap();
            state.desired_name = Some("latest".into());
            state.desired_name_generation = 1;
        }
        write_response(&mut writer, &first, json!({})).await;
        let materialized = materialize.await.unwrap();
        {
            let mut state = runtime.lock().unwrap();
            state.attached = Some(CodexAttached {
                thread_id: materialized.thread.id().into(),
                daemon_mode: Some("test".into()),
                live: Some(CodexLive {
                    client: client.clone(),
                    thread: materialized.thread,
                    socket_path: PathBuf::from("/tmp/test.sock"),
                }),
                active_turn_id: None,
                last_agent_messages: HashMap::new(),
                pending: HashMap::new(),
                applied_name_generation: materialized.applied_name_generation,
            });
        }
        schedule_name_reconciliation(Uuid::from_u128(1), &runtime, stop_tx.subscribe());
        let latest = read_request(&mut reader).await;
        assert_eq!(latest["params"]["name"], "latest");
        write_response(&mut writer, &latest, json!({})).await;
        tokio::task::yield_now().await;
        assert_eq!(
            runtime
                .lock()
                .unwrap()
                .attached
                .as_ref()
                .unwrap()
                .applied_name_generation,
            Some(1)
        );
        let _ = stop_tx.send(true);
        client.close().await;
    }

    #[tokio::test]
    async fn rapid_renames_are_serialized_and_clearing_restores_bootstrap_label() {
        let (client, mut reader, mut writer) = mock_codex().await;
        let thread = start_mock_thread(&client, &mut reader, &mut writer).await;
        let runtime = Arc::new(StdMutex::new(CodexRuntime {
            desired_name: Some("older".into()),
            desired_name_generation: 1,
            name_reconciler_running: false,
            attached: Some(CodexAttached {
                thread_id: thread.id().into(),
                daemon_mode: Some("test".into()),
                live: Some(CodexLive {
                    client: client.clone(),
                    thread,
                    socket_path: PathBuf::from("/tmp/test.sock"),
                }),
                active_turn_id: None,
                last_agent_messages: HashMap::new(),
                pending: HashMap::new(),
                applied_name_generation: Some(0),
            }),
            resume_daemon_mode: None,
            startup_error: None,
            ingest_abort: None,
            pty: None,
            next_pty_epoch: 0,
        }));
        let (stop_tx, _) = watch::channel(false);
        let agent_id = Uuid::from_u128(1);
        schedule_name_reconciliation(agent_id, &runtime, stop_tx.subscribe());
        let older = read_request(&mut reader).await;
        assert_eq!(older["params"]["name"], "older");
        {
            let mut state = runtime.lock().unwrap();
            state.desired_name = None;
            state.desired_name_generation = 2;
        }
        schedule_name_reconciliation(agent_id, &runtime, stop_tx.subscribe());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), read_request(&mut reader))
                .await
                .is_err(),
            "a second rename must not be issued before the first completes"
        );
        write_response(&mut writer, &older, json!({})).await;
        let newest = read_request(&mut reader).await;
        assert_eq!(newest["params"]["name"], "amux-00000000");
        write_response(&mut writer, &newest, json!({})).await;
        tokio::task::yield_now().await;
        assert_eq!(
            runtime
                .lock()
                .unwrap()
                .attached
                .as_ref()
                .unwrap()
                .applied_name_generation,
            Some(2)
        );
        let _ = stop_tx.send(true);
        client.close().await;
    }

    #[tokio::test]
    async fn fresh_materialization_failure_keeps_attachment_private_and_retries_same_thread() {
        let (client, mut reader, mut writer) = mock_codex().await;
        let thread = start_mock_thread(&client, &mut reader, &mut writer).await;
        let runtime = Arc::new(StdMutex::new(CodexRuntime {
            desired_name: None,
            desired_name_generation: 0,
            name_reconciler_running: false,
            attached: None,
            resume_daemon_mode: None,
            startup_error: None,
            ingest_abort: None,
            pty: None,
            next_pty_epoch: 0,
        }));
        let source = StructuredLogSource::new(8);
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let materialize_client = client.clone();
        let materialize_runtime = runtime.clone();
        let materialize_source = source.clone();
        let materialize = tokio::spawn(async move {
            match materialize_started_thread(
                &materialize_client,
                Uuid::from_u128(1),
                &ThreadConfig::default(),
                thread,
                &materialize_runtime,
                &materialize_source,
                &mut stop_rx,
            )
            .await
            {
                MaterializeStartOutcome::Ready(materialized) => materialized,
                _ => panic!("materialization did not complete"),
            }
        });
        let first_name = read_request(&mut reader).await;
        assert_eq!(first_name["method"], "thread/name/set");
        write_rpc_error(&mut writer, &first_name, "injected materialization failure").await;
        let resume = read_request(&mut reader).await;
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"]["threadId"], "thread-1");
        write_rpc_error(
            &mut writer,
            &resume,
            "no rollout found for thread id thread-1",
        )
        .await;
        assert!(runtime.lock().unwrap().attached.is_none());

        let retry_name = read_request(&mut reader).await;
        assert_eq!(retry_name["method"], "thread/name/set");
        assert_eq!(retry_name["params"]["threadId"], "thread-1");
        write_response(&mut writer, &retry_name, json!({})).await;
        let registration_resume = read_request(&mut reader).await;
        assert_eq!(registration_resume["method"], "thread/resume");
        write_response(
            &mut writer,
            &registration_resume,
            thread_session("thread-1", Some("amux-00000000")),
        )
        .await;
        let materialized = materialize.await.unwrap();
        assert_eq!(materialized.thread.id(), "thread-1");
        assert!(runtime.lock().unwrap().attached.is_none());
        let _ = stop_tx.send(true);
        client.close().await;
    }

    async fn install_test_connection(shared: &CodexClient, client: Codex, socket_path: &Path) {
        *shared.connection.lock().await = Some(Arc::new(CodexConnection {
            client,
            mode: "test",
            socket_path: socket_path.to_path_buf(),
            _daemon: DaemonMode::PrivateExisting(socket_path.to_path_buf()),
        }));
    }

    #[tokio::test]
    async fn a2a_codex_required_mcp_config_survives_start_resume_and_reconnect() {
        let (_temporary, session, expected_config) = file_backed_session();
        let thread_config = session.thread_config().unwrap();
        let (initial_client, mut initial_reader, mut initial_writer) = mock_codex().await;
        let shared = Arc::new(CodexClient::new(PathBuf::from("/tmp/test-codex.sock")));
        install_test_connection(
            &shared,
            initial_client.clone(),
            Path::new("/tmp/initial.sock"),
        )
        .await;

        let start = tokio::spawn({
            let shared = shared.clone();
            let thread_config = thread_config.clone();
            async move {
                attach_thread(&shared, &thread_config, None, None)
                    .await
                    .unwrap()
            }
        });
        let start_request = read_request(&mut initial_reader).await;
        assert_managed_thread_request(&start_request, "thread/start", &expected_config);
        write_response(
            &mut initial_writer,
            &start_request,
            thread_session("thread-managed", None),
        )
        .await;
        let (_, started, provenance) = start.await.unwrap();
        assert_eq!(started.id(), "thread-managed");
        assert_eq!(provenance, AttachmentProvenance::Started);

        let cold_resume = tokio::spawn({
            let shared = shared.clone();
            let thread_config = thread_config.clone();
            async move {
                attach_thread(&shared, &thread_config, Some("thread-managed"), None)
                    .await
                    .unwrap()
            }
        });
        let resume_request = read_request(&mut initial_reader).await;
        assert_managed_thread_request(&resume_request, "thread/resume", &expected_config);
        assert_eq!(resume_request["params"]["threadId"], "thread-managed");
        write_response(
            &mut initial_writer,
            &resume_request,
            thread_session("thread-managed", Some("named")),
        )
        .await;
        let (_, resumed, provenance) = cold_resume.await.unwrap();
        assert_eq!(resumed.id(), "thread-managed");
        assert_eq!(provenance, AttachmentProvenance::Resumed);

        initial_client.clone().close().await;
        let (reconnected_client, mut reconnected_reader, mut reconnected_writer) =
            mock_codex().await;
        install_test_connection(
            &shared,
            reconnected_client.clone(),
            Path::new("/tmp/reconnected.sock"),
        )
        .await;
        let reconnect = tokio::spawn({
            let shared = shared.clone();
            async move {
                attach_thread(&shared, &thread_config, Some("thread-managed"), None)
                    .await
                    .unwrap()
            }
        });
        let reconnect_request = read_request(&mut reconnected_reader).await;
        assert_managed_thread_request(&reconnect_request, "thread/resume", &expected_config);
        assert_eq!(reconnect_request["params"]["threadId"], "thread-managed");
        write_response(
            &mut reconnected_writer,
            &reconnect_request,
            thread_session("thread-managed", Some("named")),
        )
        .await;
        let (_, reconnected, provenance) = reconnect.await.unwrap();
        assert_eq!(reconnected.id(), "thread-managed");
        assert_eq!(provenance, AttachmentProvenance::Resumed);
        reconnected_client.close().await;
    }

    async fn bootstrap_naming_transport_loss_recovers(candidate_resumes: bool) {
        let (initial_client, mut initial_reader, mut initial_writer) = mock_codex().await;
        let (fresh_client, mut fresh_reader, mut fresh_writer) = mock_codex().await;
        let shared = Arc::new(CodexClient::new(PathBuf::from("/tmp/test-codex.sock")));
        install_test_connection(
            &shared,
            initial_client.clone(),
            Path::new("/tmp/initial.sock"),
        )
        .await;
        let runtime = Arc::new(StdMutex::new(CodexRuntime {
            desired_name: None,
            desired_name_generation: 0,
            name_reconciler_running: false,
            attached: None,
            resume_daemon_mode: None,
            startup_error: None,
            ingest_abort: None,
            pty: None,
            next_pty_epoch: 0,
        }));
        let source = StructuredLogSource::new(16);
        let (stop_tx, stop_rx) = watch::channel(false);
        let supervisor = tokio::spawn(run_ingest_supervisor(
            Uuid::from_u128(1),
            shared.clone(),
            runtime.clone(),
            source,
            CodexIngestOptions {
                thread_config: ThreadConfig::default(),
                thread_id: None,
                completion_sink: None,
            },
            stop_rx,
        ));

        let start = read_request(&mut initial_reader).await;
        assert_eq!(start["method"], "thread/start");
        write_response(
            &mut initial_writer,
            &start,
            thread_session("thread-candidate", None),
        )
        .await;
        let naming = read_request(&mut initial_reader).await;
        assert_eq!(naming["method"], "thread/name/set");
        assert_eq!(naming["params"]["threadId"], "thread-candidate");
        assert!(runtime.lock().unwrap().attached.is_none());

        initial_client.clone().close().await;
        install_test_connection(&shared, fresh_client.clone(), Path::new("/tmp/fresh.sock")).await;

        let resume = tokio::time::timeout(Duration::from_secs(2), read_request(&mut fresh_reader))
            .await
            .expect("supervisor wedged instead of reconnecting");
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"]["threadId"], "thread-candidate");
        assert!(
            runtime.lock().unwrap().attached.is_none(),
            "ambiguous candidate published before authoritative resume"
        );

        let expected_id = if candidate_resumes {
            write_response(
                &mut fresh_writer,
                &resume,
                thread_session("thread-candidate", Some("amux-00000000")),
            )
            .await;
            "thread-candidate"
        } else {
            write_rpc_error(
                &mut fresh_writer,
                &resume,
                "no rollout found for thread id thread-candidate",
            )
            .await;
            let replacement = read_request(&mut fresh_reader).await;
            assert_eq!(replacement["method"], "thread/start");
            write_response(
                &mut fresh_writer,
                &replacement,
                thread_session("thread-replacement", None),
            )
            .await;
            let replacement_name = read_request(&mut fresh_reader).await;
            assert_eq!(replacement_name["method"], "thread/name/set");
            assert_eq!(replacement_name["params"]["threadId"], "thread-replacement");
            assert!(
                runtime.lock().unwrap().attached.is_none(),
                "replacement published before naming materialized it"
            );
            write_response(&mut fresh_writer, &replacement_name, json!({})).await;
            "thread-replacement"
        };

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if runtime
                    .lock()
                    .unwrap()
                    .attached
                    .as_ref()
                    .is_some_and(|attached| attached.thread_id == expected_id)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("agent did not publish the proven thread after reconnect");

        stop_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), supervisor)
            .await
            .expect("supervisor did not stop")
            .unwrap();
        fresh_client.close().await;
    }

    #[tokio::test]
    async fn naming_transport_loss_reconnects_and_publishes_resumable_candidate() {
        bootstrap_naming_transport_loss_recovers(true).await;
    }

    #[tokio::test]
    async fn naming_transport_loss_reconnects_and_replaces_missing_candidate() {
        bootstrap_naming_transport_loss_recovers(false).await;
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
                last_agent_messages: HashMap::new(),
                pending: HashMap::new(),
                applied_name_generation: Some(0),
            });
        }

        let suspended = session.suspended_state().unwrap();
        assert!(matches!(
            suspended,
            SuspendedAgent::Codex {
                thread_id,
                daemon_mode,
                ..
            } if thread_id == "thread-persisted"
                && daemon_mode.as_deref() == Some("spawned-private")
        ));
    }

    #[test]
    fn suspend_records_explicit_resume_thread_before_daemon_attach() {
        let req = CreateAgentRequest {
            agent_id: Uuid::from_u128(2),
            host_id: None,
            name: Some("resume-pending".into()),
            agent_type: AgentType::Codex {
                model: Some("gpt-test".into()),
                approval_policy: Some("never".into()),
                sandbox_policy: Some("read-only".into()),
                resume_thread_id: Some("thread-known".into()),
            },
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        };
        let session = CodexSession::new(
            &req,
            Arc::new(CodexClient::new(PathBuf::from("/tmp/amux-codex.sock"))),
            crate::agents::mcp_launch_route_for_tests(Uuid::from_u128(11)),
        );

        let suspended = session.suspended_state().unwrap();
        assert!(matches!(
            suspended,
            SuspendedAgent::Codex {
                thread_id,
                daemon_mode: None,
                ..
            } if thread_id == "thread-known"
        ));
    }

    #[test]
    fn raw_tui_argv_forwards_all_stored_overrides() {
        assert_eq!(
            raw_tui_args(
                "thread-123",
                Path::new("/tmp/codex.sock"),
                Some("gpt-test"),
                Some("never"),
                Some("read-only"),
            ),
            [
                "resume",
                "thread-123",
                "--remote",
                "unix:///tmp/codex.sock",
                "--model",
                "gpt-test",
                "--ask-for-approval",
                "never",
                "--sandbox",
                "read-only",
            ]
        );
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
            desired_name_generation: 0,
            name_reconciler_running: false,
            attached: Some(CodexAttached {
                thread_id: "thread-1".into(),
                daemon_mode: Some("test".into()),
                live: None,
                active_turn_id: Some("turn-1".into()),
                last_agent_messages: HashMap::new(),
                pending: HashMap::from([
                    (RequestId::Integer(1), PendingRequestKind::Approval),
                    (
                        RequestId::String("tool-2".into()),
                        PendingRequestKind::ToolCall,
                    ),
                ]),
                applied_name_generation: Some(0),
            }),
            resume_daemon_mode: None,
            startup_error: None,
            ingest_abort: None,
            pty: None,
            next_pty_epoch: 0,
        }));
        resolve_pending(
            &source,
            mark_disconnected(&runtime, None),
            "connection_lost",
        )
        .await;
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
            desired_name_generation: 0,
            name_reconciler_running: false,
            attached: Some(CodexAttached {
                thread_id: "thread-1".into(),
                daemon_mode: Some("test".into()),
                live: None,
                active_turn_id: None,
                last_agent_messages: HashMap::new(),
                pending: HashMap::new(),
                applied_name_generation: Some(0),
            }),
            resume_daemon_mode: None,
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
            desired_name_generation: 0,
            name_reconciler_running: false,
            attached: Some(CodexAttached {
                thread_id: "thread-1".into(),
                daemon_mode: Some("test".into()),
                live: Some(CodexLive {
                    client: client.clone(),
                    thread,
                    socket_path: PathBuf::from("/tmp/test-codex.sock"),
                }),
                active_turn_id: None,
                last_agent_messages: HashMap::new(),
                pending: HashMap::new(),
                applied_name_generation: Some(0),
            }),
            resume_daemon_mode: None,
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
        ingest_event(runtime, source, None, event.clone()).await;
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
            last_agent_messages: HashMap::new(),
            pending: HashMap::new(),
            applied_name_generation: Some(0),
        });
    }

    #[tokio::test]
    async fn a2a_codex_completion_replays_last_agent_message() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex_backend/a2a_last_message.io.jsonl");
        let mut script = load_script(fixture);
        let initialize = script
            .iter_mut()
            .find(|event| event.direction == IoDirection::Write)
            .expect("fixture has initialize request");
        let mut initialize_value: Value =
            serde_json::from_str(&initialize.line).expect("initialize request is JSON");
        initialize_value["params"]["clientInfo"]["title"] = Value::Null;
        initialize.line = initialize_value.to_string();
        let (reader, writer, controller) =
            replay_transport_with_controller(script, ReplayOptions::default());
        let driver = tokio::spawn(async move {
            while let ReplayAdvance::Advanced { .. } | ReplayAdvance::BlockedOnWrite =
                controller.advance_one().await
            {
                tokio::task::yield_now().await;
            }
        });
        let client = tokio::time::timeout(
            Duration::from_secs(2),
            Codex::from_io(
                reader,
                writer,
                CodexConfig {
                    client_name: "amux-a2a-capture".into(),
                    client_version: "0.1.0".into(),
                    ..CodexConfig::default()
                },
            ),
        )
        .await
        .expect("initialize replay timed out")
        .expect("initialize replay failed");
        let thread = tokio::time::timeout(
            Duration::from_secs(2),
            client.start_thread(ThreadConfig {
                cwd: Some("[SCRATCH]/project".into()),
                model: Some("gpt-5.6-sol".into()),
                ..ThreadConfig::default()
            }),
        )
        .await
        .expect("thread/start replay timed out")
        .expect("thread/start replay failed");
        let mut events = thread.events().await.expect("thread events");

        let mut session = session();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        assert!(session.completion_sink(&event_tx).is_none());
        session.parent = Some(AgentParent {
            agent_id: Uuid::from_u128(2),
            host_id: Uuid::from_u128(3),
        });
        attach_runtime(&session.runtime, &client, &thread);
        let source = StructuredLogSource::new(64);
        let completion_sink = session
            .completion_sink(&event_tx)
            .expect("parent agent has a completion sink");

        thread
            .start_turn(
                "Send two separate assistant messages in this turn: first exactly C14_FIRST in commentary, then exactly C14_SECOND as the final answer.",
            )
            .await
            .expect("turn/start replay failed");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = events.next().await.expect("thread event").expect("event");
                let turn_completed = matches!(&event.event, TurnEvent::TurnCompleted { .. });
                ingest_event(&session.runtime, &source, Some(&completion_sink), event).await;
                if turn_completed {
                    break;
                }
            }
        })
        .await
        .expect("completion replay timed out");

        assert!(matches!(
            event_rx.recv().await,
            Some(SessionEvent::Completed { agent_id, text })
                if agent_id == session.agent_id && text == "C14_SECOND"
        ));
        assert!(
            event_rx.try_recv().is_err(),
            "completion emitted more than once"
        );
        {
            let state = session.runtime.lock().unwrap();
            let attached = state.attached.as_ref().expect("attached runtime");
            assert!(attached.active_turn_id.is_none());
            assert!(attached.last_agent_messages.is_empty());
        }

        tokio::time::timeout(Duration::from_secs(2), driver)
            .await
            .expect("replay driver timed out")
            .expect("replay driver failed");
        client.close().await;
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
            Some("amux.codex_message") => json!({
                "type": row_type,
                "id": row["id"],
                "kind": row["kind"],
                "from": row["from"],
                "from_id": row["from_id"],
                "context": row["context"],
                "text": row["text"],
                "delivery": row["delivery"],
            }),
            _ => json!({"type": row_type}),
        }
    }

    #[test]
    fn a2a_codex_row_vocab_projects_message_fixture() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex_backend/rows.jsonl");
        let expected = std::fs::read_to_string(fixture)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|row| row["type"] == "amux.codex_message")
            .expect("fixture contains the synthesized message row");

        assert_eq!(
            project_row(&codex_message_row(
                &delivery_envelope(),
                Delivery::InjectQueued
            )),
            expected
        );
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
            desired_name_generation: 0,
            name_reconciler_running: false,
            attached: None,
            resume_daemon_mode: None,
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
        source
            .write(codex_message_row(
                &delivery_envelope(),
                Delivery::InjectQueued,
            ))
            .await;
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

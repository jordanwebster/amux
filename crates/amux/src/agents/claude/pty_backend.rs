use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use claude::hooks::{HookPayload, MessagingCredentials};
use claude::pty::keymap::{BAKED_KEYMAPS, KeymapSources};
use claude::pty::{Control, HookSource, PtyEvent, PtySource, Session, Sources, TranscriptSource};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use uuid::Uuid;

use super::delivery::ClaudeDeliveryTarget;
use super::io as pty_io;
use super::suspend::{ClaudeSuspendRecord, sanitize_resume_args};
use crate::agents::claude::ClaudeVersionCache;
use crate::agents::{
    AgentBackend, AgentDeliveryTarget, AgentKind, AgentParent, AgentRecord, AgentType,
    ClaudeDriver, CreateAgentRequest, HookEnvironment, HookError, HookOutcome,
    LocalAgentNameSource, McpLaunchRoute, Plane, Protocol, PtyHandle, RawPtyTarget, SessionEvent,
    SpawnInheritance, StopPolicy, StructuredInput, StructuredInputEvent, StructuredLogSource,
    TerminalSize,
};
use crate::debug::DebugView;
use crate::protocol::ProtocolError;
use crate::suspend::SuspendedAgent;

const STRUCTURED_LOG_RETENTION: usize = 1000;
const HOOK_DEDUPE_WINDOW: Duration = Duration::from_secs(2);
const MESSAGING_SOCKET_ENV: &str = "CLAUDE_CODE_MESSAGING_SOCKET";
const MESSAGING_TOKEN_ENV: &str = "CLAUDE_CODE_MESSAGING_TOKEN";
const MESSAGING_SOCKET_MIN_VERSION: semver::Version = semver::Version::new(2, 1, 224);

#[derive(Default)]
struct Runtime {
    control: Option<Control>,
    pty: Option<PtyHandle>,
    hook_tx: Option<mpsc::Sender<HookPayload>>,
    messaging: Option<MessagingCredentials>,
    session_id: Option<Uuid>,
    last_hook: Option<(u64, tokio::time::Instant)>,
}

pub(crate) struct ClaudePtyBackend {
    driver: ClaudeDriver,
    agent_id: Uuid,
    name: Option<String>,
    command: String,
    working_dir: PathBuf,
    readonly: bool,
    args: Vec<String>,
    terminal_size: Option<TerminalSize>,
    runtime_dir: PathBuf,
    keymaps: KeymapSources,
    version_cache: ClaudeVersionCache,
    launch_route: Option<McpLaunchRoute>,
    parent: Option<AgentParent>,
    name_source: LocalAgentNameSource,
    created_at: DateTime<Utc>,
    log: StructuredLogSource,
    runtime: Arc<Mutex<Runtime>>,
    delivery_ready: Arc<AtomicBool>,
    injected: Option<Session>,
    started: bool,
    ingest_abort: Option<AbortHandle>,
}

impl ClaudePtyBackend {
    pub(in crate::agents) fn new(
        req: &CreateAgentRequest,
        runtime_dir: PathBuf,
        version_cache: ClaudeVersionCache,
        launch_route: McpLaunchRoute,
        user_keymap_dir: PathBuf,
    ) -> Self {
        let driver = match req.agent_type {
            AgentType::Claude { driver } => driver,
            _ => unreachable!("Claude backend requires a Claude request"),
        };
        debug_assert_eq!(driver, ClaudeDriver::Pty);
        Self {
            driver,
            agent_id: req.agent_id,
            name: req.name.clone(),
            command: "claude".to_string(),
            working_dir: req.working_dir.clone(),
            readonly: false,
            args: req.args.clone(),
            terminal_size: req.terminal_size,
            runtime_dir,
            keymaps: KeymapSources {
                baked: BAKED_KEYMAPS,
                user_dir: Some(user_keymap_dir),
            },
            version_cache,
            launch_route: Some(launch_route),
            parent: req.parent,
            name_source: if req.name.is_some() {
                LocalAgentNameSource::Amux
            } else {
                LocalAgentNameSource::Unset
            },
            created_at: Utc::now(),
            log: StructuredLogSource::new(STRUCTURED_LOG_RETENTION),
            runtime: Arc::new(Mutex::new(Runtime::default())),
            delivery_ready: Arc::new(AtomicBool::new(false)),
            injected: None,
            started: false,
            ingest_abort: None,
        }
    }

    pub(in crate::agents) fn from_suspended(
        req: &CreateAgentRequest,
        name_source: LocalAgentNameSource,
        session_id: Uuid,
        created_at: DateTime<Utc>,
        runtime_dir: PathBuf,
        version_cache: ClaudeVersionCache,
        launch_route: McpLaunchRoute,
        user_keymap_dir: PathBuf,
    ) -> Self {
        let mut backend = Self::new(
            req,
            runtime_dir,
            version_cache,
            launch_route,
            user_keymap_dir,
        );
        backend.args = sanitize_resume_args(backend.args);
        backend.name_source = name_source;
        backend.created_at = created_at;
        backend
            .runtime
            .lock()
            .expect("Claude runtime poisoned")
            .session_id = Some(session_id);
        backend
    }

    #[allow(dead_code)]
    pub(crate) fn with_session(record: AgentRecord, session: Session) -> Self {
        debug_assert_eq!(
            record.kind,
            AgentKind::Claude {
                driver: ClaudeDriver::Pty
            }
        );
        Self {
            driver: ClaudeDriver::Pty,
            agent_id: record.id,
            name: record.name.clone(),
            command: record.command,
            working_dir: record.working_dir,
            readonly: record.readonly,
            args: record.args,
            terminal_size: None,
            runtime_dir: std::env::temp_dir(),
            keymaps: KeymapSources::default(),
            version_cache: ClaudeVersionCache::default(),
            launch_route: None,
            parent: record.parent,
            name_source: if record.name.is_some() {
                LocalAgentNameSource::Amux
            } else {
                LocalAgentNameSource::Unset
            },
            created_at: record.created_at,
            log: StructuredLogSource::new(STRUCTURED_LOG_RETENTION),
            runtime: Arc::new(Mutex::new(Runtime::default())),
            delivery_ready: Arc::new(AtomicBool::new(false)),
            injected: Some(session),
            started: false,
            ingest_abort: None,
        }
    }

    fn new_readonly(agent_id: Uuid, working_dir: PathBuf) -> Self {
        let (session, hook_tx) = external_session();
        let record = AgentRecord {
            id: agent_id,
            host_id: Uuid::nil(),
            name: None,
            command: "claude".to_string(),
            working_dir,
            kind: AgentKind::Claude {
                driver: ClaudeDriver::Pty,
            },
            readonly: true,
            args: Vec::new(),
            created_at: Utc::now(),
            parent: None,
            working_on: None,
        };
        let mut backend = Self::with_session(record, session);
        backend
            .runtime
            .lock()
            .expect("Claude runtime poisoned")
            .hook_tx = Some(hook_tx);
        let (event_tx, _event_rx) = mpsc::channel(1);
        let handle = backend
            .activate_injected(&event_tx)
            .expect("fresh external session activates");
        backend.ingest_abort = Some(handle.abort_handle());
        backend
    }

    #[cfg(any(debug_assertions, test))]
    pub(crate) fn for_protocol_tests(
        req: &CreateAgentRequest,
        runtime_dir: PathBuf,
        version_cache: ClaudeVersionCache,
        launch_route: McpLaunchRoute,
        user_keymap_dir: PathBuf,
    ) -> Self {
        Self::scripted(
            req,
            runtime_dir,
            version_cache,
            launch_route,
            user_keymap_dir,
        )
    }

    #[cfg(feature = "testnet")]
    pub(crate) fn scripted_for_testnet(
        req: &CreateAgentRequest,
        runtime_dir: PathBuf,
        version_cache: ClaudeVersionCache,
        launch_route: McpLaunchRoute,
        user_keymap_dir: PathBuf,
    ) -> Self {
        Self::scripted(
            req,
            runtime_dir,
            version_cache,
            launch_route,
            user_keymap_dir,
        )
    }

    #[cfg(any(debug_assertions, test, feature = "testnet"))]
    pub(super) fn scripted(
        req: &CreateAgentRequest,
        runtime_dir: PathBuf,
        version_cache: ClaudeVersionCache,
        launch_route: McpLaunchRoute,
        user_keymap_dir: PathBuf,
    ) -> Self {
        let mut backend = Self::new(
            req,
            runtime_dir,
            version_cache,
            launch_route,
            user_keymap_dir,
        );
        let (session, hook_tx) = scripted_session(&backend.keymaps);
        backend.injected = Some(session);
        backend
            .runtime
            .lock()
            .expect("Claude runtime poisoned")
            .hook_tx = Some(hook_tx);
        backend.delivery_ready.store(true, Ordering::Release);
        let (event_tx, _event_rx) = mpsc::channel(1);
        let handle = backend
            .activate_injected(&event_tx)
            .expect("scripted session activates");
        backend.ingest_abort = Some(handle.abort_handle());
        backend
    }

    fn activate_injected(
        &mut self,
        event_tx: &mpsc::Sender<SessionEvent>,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let session = self
            .injected
            .take()
            .context("Claude provider session is unavailable")?;
        self.activate(session, event_tx)
    }

    fn activate(
        &mut self,
        session: Session,
        event_tx: &mpsc::Sender<SessionEvent>,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let Session {
            mut events,
            control,
        } = session;
        let pty = (!self.readonly)
            .then(|| PtyHandle::from_claude(control.clone()))
            .flatten();
        {
            let mut runtime = self.runtime.lock().expect("Claude runtime poisoned");
            runtime.control = Some(control);
            runtime.pty = pty;
        }
        let runtime = self.runtime.clone();
        let log = self.log.clone();
        let ready = self.delivery_ready.clone();
        let version_cache = self.version_cache.clone();
        let event_tx = event_tx.clone();
        let agent_id = self.agent_id;
        Ok(tokio::spawn(async move {
            let mut names = NameState::default();
            while let Some(event) = events.recv().await {
                match event {
                    PtyEvent::Ready { .. } | PtyEvent::Ask(_) | PtyEvent::Delivery(_) => {}
                    PtyEvent::Keymap(resolved) => {
                        log.write(keymap_row(resolved)).await;
                    }
                    PtyEvent::InputResult(result) => {
                        log.write(input_result_row(result)).await;
                    }
                    PtyEvent::Relink { reason, .. } => {
                        if !matches!(reason, claude::pty::RelinkReason::Initial) {
                            log.clear().await;
                        }
                    }
                    PtyEvent::Transcript { row, .. } => {
                        let value = row.into_value();
                        version_cache.observe_transcript_row(&value);
                        if value.get("type").and_then(Value::as_str)
                            == Some("amux.transcript_ready")
                        {
                            ready.store(true, Ordering::Release);
                        }
                        if let Some((name, source)) = names.observe(&value) {
                            let _ = event_tx
                                .send(SessionEvent::NameCandidateChanged {
                                    agent_id,
                                    name,
                                    source,
                                })
                                .await;
                        }
                        log.write(value).await;
                    }
                    PtyEvent::Hook(hook) => {
                        ingest_hook(agent_id, &runtime, &log, &ready, &event_tx, hook).await;
                    }
                    PtyEvent::Exited(_) => break,
                }
            }
            log.close().await;
        }))
    }

    fn launch(&self) -> Result<claude::launch::Launch> {
        let route = self
            .launch_route
            .as_ref()
            .context("managed Claude session is missing its MCP launch route")?;
        route
            .validate()
            .context("managed Claude MCP launch route is no longer valid")?;
        let mut args = claude::launch::without_managed_spawn_args(&self.args);
        let user_settings = claude::launch::take_settings_args(&mut args)?;
        args.extend([
            "--name".to_string(),
            self.name
                .clone()
                .unwrap_or_else(|| self.agent_id.to_string()),
        ]);
        if self
            .version_cache
            .current()
            .is_some_and(|version| version.0 >= MESSAGING_SOCKET_MIN_VERSION)
        {
            args.extend([
                "--messaging-socket-path".to_string(),
                self.runtime_dir
                    .join(format!("amux-{}.sock", self.agent_id))
                    .to_string_lossy()
                    .into_owned(),
            ]);
        }
        let executable = route
            .executable()
            .to_str()
            .context("the running amux executable path is not valid UTF-8")?;
        let socket = route
            .socket_path()
            .to_str()
            .context("the daemon socket path is not valid UTF-8")?;
        let mut env = serde_json::Map::from_iter([
            (
                "AMUX_AGENT_ID".to_string(),
                Value::String(self.agent_id.to_string()),
            ),
            (
                "AMUX_HOST_ID".to_string(),
                Value::String(route.host_id().to_string()),
            ),
        ]);
        if let Some(path) = route.config_path() {
            env.insert(
                "AMUX_CONFIG".to_string(),
                Value::String(
                    path.to_str()
                        .context("the amux config path is not valid UTF-8")?
                        .to_string(),
                ),
            );
        }
        let hook_command = vec![
            executable.to_string(),
            "hooks".to_string(),
            "claude".to_string(),
        ];
        let settings = claude::launch::merged_settings(
            claude::launch::load_user_settings(&self.working_dir, &user_settings)?,
            &claude::launch::ManagedSettings {
                hook_command: hook_command.clone(),
                mcp_servers: Vec::new(),
            },
        );
        let mcp_servers = vec![claude::launch::McpServerConfig {
            name: "amux".to_string(),
            config: json!({"command": executable, "args": ["mcp", "agent", "--socket-path", socket], "env": env}),
        }];
        let session_id = self
            .runtime
            .lock()
            .expect("Claude runtime poisoned")
            .session_id
            .unwrap_or(self.agent_id);
        Ok(claude::launch::Launch {
            binary: self.command.clone().into(),
            cwd: self.working_dir.clone(),
            args,
            session_id,
            resume: session_id != self.agent_id,
            settings,
            hook_command,
            mcp_servers,
            env_scrub: claude::launch::CHILD_SESSION_ENV_SCRUB,
        })
    }

    pub(crate) async fn bootstrap_external_hook(
        agent_id: Uuid,
        payload: &[u8],
        env: &HookEnvironment,
    ) -> std::result::Result<Option<Self>, HookError> {
        let hook = parse_hook(payload)?;
        if matches!(
            hook,
            HookPayload::Unknown { .. } | HookPayload::SessionEnd(_)
        ) {
            return Ok(None);
        }
        let mut backend = Self::new_readonly(agent_id, hook.common().cwd.clone());
        backend.sync_messaging(env);
        backend.send_hook(hook).await?;
        Ok(Some(backend))
    }

    fn sync_messaging(&mut self, env: &HookEnvironment) {
        let credentials = env
            .get(MESSAGING_SOCKET_ENV)
            .filter(|v| !v.is_empty())
            .zip(env.get(MESSAGING_TOKEN_ENV).filter(|v| !v.is_empty()))
            .map(|(path, token)| MessagingCredentials {
                socket_path: path.into(),
                token: token.clone(),
            });
        if let Some(credentials) = credentials {
            self.runtime
                .lock()
                .expect("Claude runtime poisoned")
                .messaging = Some(credentials);
        }
    }

    async fn send_hook(&self, hook: HookPayload) -> std::result::Result<(), HookError> {
        let tx = self
            .runtime
            .lock()
            .expect("Claude runtime poisoned")
            .hook_tx
            .clone();
        tx.ok_or_else(|| HookError::InvalidPayload {
            message: "Claude hook source is unavailable".to_string(),
        })?
        .send(hook)
        .await
        .map_err(|_| HookError::InvalidPayload {
            message: "Claude hook source is closed".to_string(),
        })
    }

    pub(super) fn delivery_snapshot(
        &self,
    ) -> (
        bool,
        Option<Control>,
        Option<MessagingCredentials>,
        Arc<AtomicBool>,
    ) {
        let runtime = self.runtime.lock().expect("Claude runtime poisoned");
        (
            self.readonly,
            runtime.control.clone(),
            runtime.messaging.clone(),
            self.delivery_ready.clone(),
        )
    }

    #[cfg(test)]
    pub(crate) fn set_session_id_for_tests(&self, session_id: Uuid) {
        self.runtime
            .lock()
            .expect("Claude runtime poisoned")
            .session_id = Some(session_id);
    }

    fn input_target(&self) -> ClaudeInputTarget {
        ClaudeInputTarget {
            readonly: self.readonly,
            runtime: self.runtime.clone(),
            log: self.log.clone(),
        }
    }

    #[cfg(debug_assertions)]
    pub(crate) async fn current_seq_for_derived_rows(&self) -> u64 {
        self.log.current_seq().await
    }

    #[cfg(debug_assertions)]
    pub(crate) async fn close_log_for_derived_rows(&self) {
        self.log.close().await;
    }
}

#[async_trait]
impl AgentBackend for ClaudePtyBackend {
    fn agent_id(&self) -> Uuid {
        self.agent_id
    }
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    fn set_local_name(&mut self, name: Option<String>, source: LocalAgentNameSource) {
        self.name = name;
        self.name_source = source;
    }
    fn command(&self) -> &str {
        &self.command
    }
    fn working_dir(&self) -> &Path {
        &self.working_dir
    }
    fn readonly(&self) -> bool {
        self.readonly
    }
    fn args(&self) -> &[String] {
        &self.args
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    fn start(
        &mut self,
        event_tx: &mpsc::Sender<SessionEvent>,
    ) -> Result<tokio::task::JoinHandle<()>> {
        if self.started {
            return Err(anyhow!("Claude session {} already started", self.agent_id));
        }
        self.started = true;
        if self.injected.is_some() {
            return self.activate_injected(event_tx);
        }
        let version = self
            .version_cache
            .current()
            .context("Claude version probe did not complete")?;
        let launch = self.launch()?;
        let size = self.terminal_size.unwrap_or_default();
        let session = claude::pty::spawn_with_version(
            &launch,
            &self.keymaps,
            pty_host::PtySize {
                rows: size.rows,
                cols: size.cols,
            },
            version,
        )?;
        self.activate(session, event_tx)
    }

    async fn stop(&self, _policy: StopPolicy) {
        if let Some(abort) = &self.ingest_abort {
            abort.abort();
        }
        let pty = self
            .runtime
            .lock()
            .expect("Claude runtime poisoned")
            .pty
            .clone();
        if let Some(pty) = pty {
            pty.close().await;
        }
        self.log.close().await;
    }

    fn kind(&self) -> AgentKind {
        AgentKind::Claude {
            driver: self.driver,
        }
    }

    fn plane(&self, protocol: Protocol) -> std::result::Result<Plane, ProtocolError> {
        match (self.driver, protocol) {
            (ClaudeDriver::Pty, Protocol::TerminalV1) => self
                .runtime
                .lock()
                .expect("Claude runtime poisoned")
                .pty
                .clone()
                .map(RawPtyTarget::Existing)
                .map(Plane::Terminal)
                .ok_or_else(|| ProtocolError::FailedPrecondition {
                    message: "Claude PTY is not active".to_string(),
                }),
            (ClaudeDriver::Pty, Protocol::ClaudePtyTranscriptV1)
            | (ClaudeDriver::Sdk, Protocol::ClaudeSdkV1) => Ok(Plane::Structured {
                log: self.log.clone(),
                input: Box::new(self.input_target()),
            }),
            (
                ClaudeDriver::Pty,
                Protocol::ClaudeSdkV1 | Protocol::CodexSdkV1 | Protocol::TestEchoV1,
            )
            | (
                ClaudeDriver::Sdk,
                Protocol::TerminalV1
                | Protocol::ClaudePtyTranscriptV1
                | Protocol::CodexSdkV1
                | Protocol::TestEchoV1,
            ) => Err(ProtocolError::NotExposed {
                kind: self.kind(),
                protocol,
            }),
        }
    }

    fn spawn_inheritance(&self) -> SpawnInheritance {
        SpawnInheritance {
            claude_permission_args: crate::agent_tools::claude_permission_args(&self.args),
            ..SpawnInheritance::default()
        }
    }
    fn parent(&self) -> Option<AgentParent> {
        self.parent
    }
    fn delivery_target(&self) -> Box<dyn AgentDeliveryTarget> {
        Box::new(ClaudeDeliveryTarget::new(self))
    }

    async fn handle_hook_payload(
        &mut self,
        payload: &[u8],
        env: &HookEnvironment,
    ) -> std::result::Result<HookOutcome, HookError> {
        let hook = parse_hook(payload)?;
        self.sync_messaging(env);
        let unknown = matches!(hook, HookPayload::Unknown { .. });
        let withdraw = self.readonly && matches!(hook, HookPayload::SessionEnd(_));
        let completion = match &hook {
            HookPayload::Stop {
                last_assistant_message,
                ..
            } => last_assistant_message.clone(),
            _ => None,
        };
        if !unknown {
            self.send_hook(hook).await?;
        }
        Ok(if unknown {
            HookOutcome::Noop
        } else if withdraw {
            HookOutcome::WithdrawSession
        } else if let Some(text) = completion {
            HookOutcome::Completed { text }
        } else {
            HookOutcome::KeepSession
        })
    }

    fn local_name_source(&self) -> Option<LocalAgentNameSource> {
        Some(self.name_source)
    }

    fn suspended_state(&self) -> Result<SuspendedAgent> {
        let session_id = self.runtime.lock().expect("Claude runtime poisoned").session_id
            .ok_or_else(|| anyhow!("cannot suspend claude agent {}: no session_id (SessionStart hook not received)", self.agent_id))?;
        Ok(ClaudeSuspendRecord {
            driver: self.driver,
            agent_id: self.agent_id,
            name: self.name.clone(),
            name_source: self.name_source,
            working_dir: self.working_dir.clone(),
            terminal_size: self.terminal_size,
            created_at: self.created_at,
            args: self.args.clone(),
            session_id,
            parent: self.parent,
        }
        .into())
    }

    fn debug_json(&self, verbose: bool) -> serde_json::Result<Value> {
        serde_json::to_value(DebugView::new(self, verbose))
    }
}

struct ClaudeInputTarget {
    readonly: bool,
    runtime: Arc<Mutex<Runtime>>,
    log: StructuredLogSource,
}

#[async_trait]
impl StructuredInput for ClaudeInputTarget {
    async fn send(&self, input: StructuredInputEvent) -> std::result::Result<(), ProtocolError> {
        let StructuredInputEvent::ClaudePty { client_seq, intent } = input else {
            return Err(ProtocolError::InvalidArgument {
                message: "Claude PTY input target received another protocol's input".to_string(),
            });
        };
        if self.readonly {
            return Err(ProtocolError::ServerError {
                message: "session is readonly".to_string(),
            });
        }
        let current_seq = self.log.current_seq().await;
        if client_seq != current_seq {
            return Err(ProtocolError::SequenceNumberMismatch {
                client_seq,
                current_seq,
            });
        }
        let control = self
            .runtime
            .lock()
            .expect("Claude runtime poisoned")
            .control
            .clone()
            .ok_or_else(|| ProtocolError::ServerError {
                message: "structured input requires an active PTY".to_string(),
            })?;
        control
            .send(provider_intent(intent))
            .await
            .map(|_| ())
            .map_err(input_protocol_error)
    }
}

fn input_protocol_error(error: claude::pty::InputError) -> ProtocolError {
    match error {
        error @ claude::pty::InputError::Pty(_) => ProtocolError::ServerError {
            message: error.to_string(),
        },
        error @ (claude::pty::InputError::UnknownAsk(_)
        | claude::pty::InputError::UnverifiedShape { .. }
        | claude::pty::InputError::UnsafeText { .. }
        | claude::pty::InputError::AnswerMismatchesAsk { .. }
        | claude::pty::InputError::NoKeymap { .. }) => ProtocolError::InvalidArgument {
            message: error.to_string(),
        },
    }
}

fn provider_intent(intent: pty_io::Intent) -> claude::pty::Intent {
    match intent {
        pty_io::Intent::Prompt { text } => claude::pty::Intent::Prompt { text },
        pty_io::Intent::Interrupt => claude::pty::Intent::Interrupt,
        pty_io::Intent::CyclePermissionMode => claude::pty::Intent::CyclePermissionMode,
        pty_io::Intent::Answer { ask_id, answer } => claude::pty::Intent::Answer {
            ask_id: claude::pty::AskId(ask_id),
            answer: provider_answer(answer),
        },
    }
}

fn provider_answer(answer: pty_io::AskAnswer) -> claude::pty::AskAnswer {
    match answer {
        pty_io::AskAnswer::Permission(answer) => claude::pty::AskAnswer::Permission(match answer {
            pty_io::PermissionAnswer::AllowOnce => claude::pty::PermissionAnswer::AllowOnce,
            pty_io::PermissionAnswer::AllowScoped { suggestion } => {
                claude::pty::PermissionAnswer::AllowScoped { suggestion }
            }
            pty_io::PermissionAnswer::Deny { feedback } => {
                claude::pty::PermissionAnswer::Deny { feedback }
            }
        }),
        pty_io::AskAnswer::Plan(answer) => claude::pty::AskAnswer::Plan(match answer {
            pty_io::PlanAnswer::ApproveAuto => claude::pty::PlanAnswer::ApproveAuto,
            pty_io::PlanAnswer::ApproveManual => claude::pty::PlanAnswer::ApproveManual,
            pty_io::PlanAnswer::RequestChanges { feedback } => {
                claude::pty::PlanAnswer::RequestChanges { feedback }
            }
        }),
        pty_io::AskAnswer::Question(response) => {
            claude::pty::AskAnswer::Question(claude::pty::QuestionResponse {
                answers: response
                    .answers
                    .into_iter()
                    .map(|answer| claude::pty::QuestionAnswer {
                        selected: answer.selected,
                        other: answer.other,
                    })
                    .collect(),
            })
        }
    }
}

fn keymap_row(resolved: claude::pty::keymap::Resolved) -> Value {
    json!({
        "type": "amux.claude.keymap",
        "keymap": resolved.keymap,
        "basis": resolved.basis,
        "stability_limits": resolved.stability_limits,
    })
}

fn input_result_row(result: claude::pty::InputResult) -> Value {
    json!({
        "type": "amux.claude.input_result",
        "intent": result.intent,
        "keymap": result.keymap,
        "basis": result.basis,
        "program": result.program,
        "bytes_written": result.bytes_written,
    })
}

async fn ingest_hook(
    agent_id: Uuid,
    runtime: &Arc<Mutex<Runtime>>,
    log: &StructuredLogSource,
    ready: &Arc<AtomicBool>,
    event_tx: &mpsc::Sender<SessionEvent>,
    hook: HookPayload,
) {
    let common = hook.common();
    {
        let mut state = runtime.lock().expect("Claude runtime poisoned");
        state.session_id = Some(common.session_id);
        if let Some(messaging) = &common.messaging {
            state.messaging = Some(messaging.clone());
        }
    }
    let tag = match &hook {
        HookPayload::SessionStart(_) => {
            ready.store(true, Ordering::Release);
            return;
        }
        HookPayload::SessionEnd(_) | HookPayload::Unknown { .. } => return,
        HookPayload::PermissionRequest { .. } => "hook.permission_request",
        HookPayload::Stop {
            last_assistant_message,
            ..
        } => {
            if let Some(text) = last_assistant_message.clone() {
                let _ = event_tx
                    .send(SessionEvent::Completed { agent_id, text })
                    .await;
            }
            "hook.stop"
        }
        HookPayload::Notification { .. } => "hook.notification",
        HookPayload::UserPromptSubmit(_) => "hook.user_prompt_submit",
        HookPayload::PreToolUse { .. } => "hook.pre_tool_use",
        HookPayload::PostToolUse { .. } => "hook.post_tool_use",
    };
    let fingerprint = hook_fingerprint(hook.raw());
    let now = tokio::time::Instant::now();
    let duplicate = {
        let mut state = runtime.lock().expect("Claude runtime poisoned");
        let duplicate = state
            .last_hook
            .is_some_and(|(last, at)| last == fingerprint && now - at <= HOOK_DEDUPE_WINDOW);
        state.last_hook = Some((fingerprint, now));
        duplicate
    };
    if duplicate {
        return;
    }
    let mut value = hook.raw().clone();
    if let Some(object) = value.as_object_mut() {
        object.insert("type".to_string(), json!(tag));
    }
    log.write(value).await;
}

fn hook_fingerprint(value: &Value) -> u64 {
    value
        .to_string()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn parse_hook(payload: &[u8]) -> std::result::Result<HookPayload, HookError> {
    claude::hooks::parse(payload).map_err(|error| HookError::InvalidPayload {
        message: error.to_string(),
    })
}

#[derive(Default)]
struct NameState {
    slug: Option<String>,
    agent: Option<String>,
    emitted: Option<(String, LocalAgentNameSource)>,
}
impl NameState {
    fn observe(&mut self, value: &Value) -> Option<(String, LocalAgentNameSource)> {
        if value.get("type").and_then(Value::as_str) == Some("agent-name") {
            self.agent = value
                .get("agentName")
                .and_then(Value::as_str)
                .map(str::to_string);
        } else if let Some(slug) = value.get("slug").and_then(Value::as_str) {
            self.slug = Some(slug.to_string());
        } else {
            return None;
        }
        let candidate = self
            .agent
            .as_ref()
            .map(|name| (name.clone(), LocalAgentNameSource::ProviderName))
            .or_else(|| {
                self.slug
                    .as_ref()
                    .map(|name| (name.clone(), LocalAgentNameSource::ProviderSlug))
            })?;
        if self.emitted.as_ref() == Some(&candidate) {
            None
        } else {
            self.emitted = Some(candidate.clone());
            Some(candidate)
        }
    }
}

fn external_session() -> (Session, mpsc::Sender<HookPayload>) {
    let (_output_tx, output) = mpsc::channel(1);
    let (hooks, hook_tx) = HookSource::channel(64);
    let session = claude::pty::from_sources(
        Sources {
            pty: PtySource {
                output,
                writer: Box::new(tokio::io::sink()),
                handle: None,
                exit: Box::pin(std::future::pending()),
            },
            hooks,
            transcript: TranscriptSource::live(),
            version: claude::version::ClaudeVersion(semver::Version::new(0, 0, 0)),
            delays: claude::pty::DelaySource::live(),
        },
        &claude::pty::keymap::KeymapSources::default(),
    );
    (session, hook_tx)
}

#[cfg(any(debug_assertions, test, feature = "testnet"))]
fn scripted_session(keymaps: &KeymapSources) -> (Session, mpsc::Sender<HookPayload>) {
    let (output_tx, output) = mpsc::channel(64);
    let (writer, mut echo) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let mut bytes = vec![0; 4096];
        loop {
            match echo.read(&mut bytes).await {
                Ok(0) | Err(_) => break,
                Ok(count)
                    if output_tx
                        .send(bytes[..count].to_vec().into())
                        .await
                        .is_err() =>
                {
                    break;
                }
                Ok(_) => {}
            }
        }
    });
    let (hooks, hook_tx) = HookSource::channel(64);
    let session = claude::pty::from_sources(
        Sources {
            pty: PtySource {
                output,
                writer: Box::new(writer),
                handle: None,
                exit: Box::pin(std::future::pending()),
            },
            hooks,
            transcript: TranscriptSource::live(),
            version: claude::version::ClaudeVersion(semver::Version::new(2, 1, 251)),
            delays: claude::pty::DelaySource::live(),
        },
        keymaps,
    );
    (session, hook_tx)
}

impl Serialize for DebugView<'_, ClaudePtyBackend> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let runtime = self.inner.runtime.lock().expect("Claude runtime poisoned");
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("kind", "claude")?;
        if let Some(session_id) = runtime.session_id {
            map.serialize_entry("session_id", &session_id)?;
        }
        map.serialize_entry("readonly", &self.inner.readonly)?;
        map.serialize_entry("has_pty", &runtime.pty.is_some())?;
        map.serialize_entry("has_messaging_credentials", &runtime.messaging.is_some())?;
        if self.verbose {
            map.serialize_entry("structured_seq", &0u64)?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook_payload(name: &str, session_id: Uuid, path: &str) -> HookPayload {
        let raw = json!({
            "hook_event_name": name,
            "session_id": session_id,
            "transcript_path": path,
            "cwd": "/tmp",
            "tool_name": "Bash",
            "tool_input": {"command": "echo one"},
        });
        claude::hooks::parse(raw.to_string().as_bytes()).unwrap()
    }

    fn injected_backend() -> (
        ClaudePtyBackend,
        mpsc::Sender<HookPayload>,
        mpsc::Sender<(PathBuf, claude::transcript::TranscriptRow)>,
        tokio::task::JoinHandle<()>,
    ) {
        let (_output_tx, output) = mpsc::channel(1);
        let (hooks, hook_tx) = HookSource::channel(8);
        let (transcript, row_tx, _paths) = TranscriptSource::channel(8);
        let session = claude::pty::from_sources(
            Sources {
                pty: PtySource {
                    output,
                    writer: Box::new(tokio::io::sink()),
                    handle: None,
                    exit: Box::pin(std::future::pending()),
                },
                hooks,
                transcript,
                version: claude::version::ClaudeVersion(semver::Version::new(2, 1, 251)),
                delays: claude::pty::DelaySource::replay(replay_support::ReplayClock::new(Some(0))),
            },
            &claude::pty::keymap::KeymapSources::default(),
        );
        let record = AgentRecord {
            id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
            name: None,
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
            kind: AgentKind::Claude {
                driver: ClaudeDriver::Pty,
            },
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
            parent: None,
            working_on: None,
        };
        let mut backend = ClaudePtyBackend::with_session(record, session);
        let (event_tx, _event_rx) = mpsc::channel(8);
        let ingest = backend.start(&event_tx).unwrap();
        (backend, hook_tx, row_tx, ingest)
    }

    async fn wait_for_session_id(backend: &ClaudePtyBackend, expected: Uuid) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if backend
                    .runtime
                    .lock()
                    .expect("Claude runtime poisoned")
                    .session_id
                    == Some(expected)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("hook session id was not ingested");
    }

    #[test]
    fn resume_args_drop_session_selectors() {
        assert_eq!(
            sanitize_resume_args(vec![
                "--model".into(),
                "sonnet".into(),
                "--resume".into(),
                "old".into(),
                "--fork-session".into()
            ]),
            vec!["--model", "sonnet"]
        );
    }

    #[tokio::test]
    async fn injected_session_drives_structured_and_terminal_planes() {
        let req = CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            host_id: None,
            name: None,
            agent_type: AgentType::Claude {
                driver: ClaudeDriver::Pty,
            },
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        };
        let backend = ClaudePtyBackend::scripted(
            &req,
            PathBuf::from("/tmp"),
            ClaudeVersionCache::default(),
            crate::agents::mcp_launch_route_for_tests(Uuid::new_v4()),
            PathBuf::from("/tmp/amux-test-keymaps"),
        );
        assert!(matches!(
            backend.plane(Protocol::TerminalV1),
            Ok(Plane::Terminal(_))
        ));
        assert!(matches!(
            backend.plane(Protocol::ClaudePtyTranscriptV1),
            Ok(Plane::Structured { .. })
        ));
        assert!(matches!(
            backend.plane(Protocol::ClaudeSdkV1),
            Err(ProtocolError::NotExposed { .. })
        ));
    }

    #[tokio::test]
    async fn semantic_input_writes_keymap_and_input_result_rows() {
        let (backend, _hooks, _rows, _ingest) = injected_backend();
        tokio::time::timeout(Duration::from_secs(1), async {
            while backend.log.current_seq().await < 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("initial keymap row was not ingested");

        backend
            .input_target()
            .send(StructuredInputEvent::ClaudePty {
                client_seq: 1,
                intent: pty_io::Intent::Prompt {
                    text: "hello".to_string(),
                },
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while backend.log.current_seq().await < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("input result row was not ingested");

        let (mut replay, seq) = backend.log.subscribe_with_query(None).await.unwrap();
        assert_eq!(seq, 2);
        let keymap = replay.read().await.unwrap().payload;
        assert_eq!(keymap["type"], "amux.claude.keymap");
        assert_eq!(keymap["keymap"]["name"], "claude-2.1");
        assert_eq!(keymap["basis"]["basis"], "in_range");
        let result = replay.read().await.unwrap().payload;
        assert_eq!(result["type"], "amux.claude.input_result");
        assert_eq!(result["intent"]["intent"], "prompt");
        assert_eq!(result["intent"]["text"], "hello");
        assert_eq!(result["program"], "prompt");
        assert!(result["bytes_written"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn managed_session_resolves_the_user_keymap_carried_by_agent_deps() {
        let data_dir = tempfile::tempdir().unwrap();
        let user_dir = crate::keymap_dir(data_dir.path());
        std::fs::create_dir_all(&user_dir).unwrap();
        let user_file = user_dir.join("claude-2.1.toml");
        std::fs::write(
            &user_file,
            BAKED_KEYMAPS[0]
                .1
                .replace("after_paste = 400", "after_paste = 777"),
        )
        .unwrap();

        let deps = crate::agents::AgentDeps::new(
            data_dir.path().join("runtime"),
            data_dir.path().join("codex.sock"),
            crate::agents::mcp_launch_route_for_tests(Uuid::new_v4()),
            user_dir.clone(),
        );
        let sources = KeymapSources {
            baked: BAKED_KEYMAPS,
            user_dir: Some(deps.claude_user_keymap_dir.clone()),
        };
        let expected = claude::pty::keymap::resolve(
            &sources,
            &claude::version::ClaudeVersion(semver::Version::new(2, 1, 251)),
        )
        .unwrap();
        let baked = claude::pty::keymap::resolve(
            &KeymapSources::default(),
            &claude::version::ClaudeVersion(semver::Version::new(2, 1, 251)),
        )
        .unwrap();
        assert_ne!(
            expected.keymap.digest, baked.keymap.digest,
            "the installed delay change must produce a distinct identity"
        );
        let req = CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            host_id: None,
            name: Some("user-keymap".to_string()),
            agent_type: AgentType::Claude {
                driver: ClaudeDriver::Pty,
            },
            working_dir: data_dir.path().to_path_buf(),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        };
        let backend = ClaudePtyBackend::scripted(
            &req,
            deps.runtime_dir,
            deps.claude_version_cache,
            deps.mcp_launch_route,
            deps.claude_user_keymap_dir,
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            while backend.log.current_seq().await < 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("initial user keymap row was not ingested");
        let (mut replay, _) = backend.log.subscribe_with_query(None).await.unwrap();
        let row = replay.read().await.unwrap().payload;

        assert_eq!(row["type"], "amux.claude.keymap");
        assert_eq!(row["keymap"]["digest"], expected.keymap.digest);
        assert_eq!(row["keymap"]["source"]["source"], "user");
        assert_eq!(row["keymap"]["source"]["path"].as_str(), user_file.to_str());
        assert_eq!(row["basis"]["basis"], "in_range");
    }

    #[tokio::test(start_paused = true)]
    async fn duplicate_hook_is_suppressed_inside_window_and_admitted_afterward() {
        let agent_id = Uuid::new_v4();
        let runtime = Arc::new(Mutex::new(Runtime::default()));
        let log = StructuredLogSource::new(8);
        let ready = Arc::new(AtomicBool::new(false));
        let (event_tx, _event_rx) = mpsc::channel(8);
        let hook = hook_payload("PermissionRequest", Uuid::new_v4(), "/tmp/one");

        ingest_hook(agent_id, &runtime, &log, &ready, &event_tx, hook.clone()).await;
        ingest_hook(agent_id, &runtime, &log, &ready, &event_tx, hook.clone()).await;
        assert_eq!(log.current_seq().await, 1);

        tokio::time::advance(HOOK_DEDUPE_WINDOW + Duration::from_millis(1)).await;
        ingest_hook(agent_id, &runtime, &log, &ready, &event_tx, hook).await;
        assert_eq!(log.current_seq().await, 2);
    }

    #[tokio::test]
    async fn external_readonly_backend_refuses_the_terminal_plane() {
        let backend = ClaudePtyBackend::new_readonly(Uuid::new_v4(), PathBuf::from("/tmp"));

        assert!(matches!(
            backend.plane(Protocol::TerminalV1),
            Err(ProtocolError::FailedPrecondition { message })
                if message == "Claude PTY is not active"
        ));
        assert!(matches!(
            backend.plane(Protocol::ClaudePtyTranscriptV1),
            Ok(Plane::Structured { .. })
        ));
    }

    #[tokio::test]
    async fn non_initial_relink_discards_previous_generation_rows() {
        let (backend, hooks, rows, _ingest) = injected_backend();
        let first_session = Uuid::new_v4();
        hooks
            .send(hook_payload(
                "SessionStart",
                first_session,
                "/tmp/transcript-one.jsonl",
            ))
            .await
            .unwrap();
        wait_for_session_id(&backend, first_session).await;

        rows.send((
            PathBuf::from("/tmp/transcript-one.jsonl"),
            claude::transcript::TranscriptRow::parse(json!({
                "type": "assistant",
                "generation": "old",
            })),
        ))
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while backend.log.current_seq().await < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let second_session = Uuid::new_v4();
        hooks
            .send(hook_payload(
                "SessionStart",
                second_session,
                "/tmp/transcript-two.jsonl",
            ))
            .await
            .unwrap();
        wait_for_session_id(&backend, second_session).await;
        rows.send((
            PathBuf::from("/tmp/transcript-two.jsonl"),
            claude::transcript::TranscriptRow::parse(json!({
                "type": "assistant",
                "generation": "new",
            })),
        ))
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while backend.log.current_seq().await < 5 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let (mut replay, seq) = backend.log.subscribe_with_query(None).await.unwrap();
        assert_eq!(seq, 5, "clearing retains the monotonic sequence");
        assert_eq!(
            replay.read().await.unwrap().payload["type"],
            "amux.claude.keymap"
        );
        assert_eq!(replay.read().await.unwrap().payload["generation"], "new");
        assert!(
            tokio::time::timeout(Duration::from_millis(25), replay.read())
                .await
                .is_err(),
            "rows from the prior transcript generation must not replay"
        );
    }
}

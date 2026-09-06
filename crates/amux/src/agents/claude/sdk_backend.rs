//! amux adapter for Claude's canonical stream-JSON provider session.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use claude::sdk::{
    ContentBlock, Control, ElicitationResult, HookOutput, McpServerConfig, McpStdioServerConfig,
    MessageContent, MessageParam, PermissionResult, QueryOptions, Role, SdkEvent, Session,
    SettingsConfig, SyncHookOutput, UserDialogResult, UserMessage,
};
use futures_util::StreamExt;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};
use tokio::task::AbortHandle;
use uuid::Uuid;

use super::sdk_delivery::ClaudeSdkDeliveryTarget;
use super::sdk_facts::SessionFacts;
use super::sdk_io::{ClaudeSdkSynthesized, ClaudeSdkV1Input, ClaudeSdkV1Row};
use super::suspend::{ClaudeSuspendRecord, sanitize_resume_args};
use crate::agents::{
    AgentBackend, AgentDeliveryTarget, AgentKind, AgentParent, AgentRecord, AgentType,
    BackendState, ClaudeDriver, CreateAgentRequest, LocalAgentNameSource, McpLaunchRoute,
    ObligationDebug, Plane, Protocol, SessionDebug, SessionEvent, SpawnInheritance, StopPolicy,
    StructuredInput, StructuredInputEvent, StructuredLogSource,
};
use crate::debug::DebugView;
use crate::protocol::ProtocolError;
use crate::suspend::SuspendedAgent;

const STRUCTURED_LOG_RETENTION: usize = 8192;

#[derive(Clone, Copy)]
enum RequestKind {
    Permission,
    Elicitation,
    Dialog,
}

impl RequestKind {
    const ALL: [Self; 3] = [Self::Permission, Self::Elicitation, Self::Dialog];

    fn name(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Elicitation => "elicitation",
            Self::Dialog => "dialog",
        }
    }

    fn resolution(self, request_id: String, decision: &str) -> ClaudeSdkSynthesized {
        let decision = decision.to_string();
        match self {
            Self::Permission => ClaudeSdkSynthesized::PermissionResolved {
                request_id,
                decision,
            },
            Self::Elicitation => ClaudeSdkSynthesized::ElicitationResolved {
                request_id,
                decision,
            },
            Self::Dialog => ClaudeSdkSynthesized::DialogResolved {
                request_id,
                decision,
            },
        }
    }
}

#[derive(Default)]
struct PendingRequests {
    permissions: HashSet<String>,
    elicitations: HashSet<String>,
    dialogs: HashSet<String>,
}

impl PendingRequests {
    fn ids(&mut self, kind: RequestKind) -> &mut HashSet<String> {
        match kind {
            RequestKind::Permission => &mut self.permissions,
            RequestKind::Elicitation => &mut self.elicitations,
            RequestKind::Dialog => &mut self.dialogs,
        }
    }

    fn drain(&mut self) -> Vec<(RequestKind, String)> {
        RequestKind::ALL
            .into_iter()
            .flat_map(|kind| {
                let mut ids = self.ids(kind).drain().collect::<Vec<_>>();
                ids.sort();
                ids.into_iter().map(move |id| (kind, id))
            })
            .collect()
    }

    fn obligations(&self) -> Vec<ObligationDebug> {
        [
            (RequestKind::Permission, &self.permissions),
            (RequestKind::Elicitation, &self.elicitations),
            (RequestKind::Dialog, &self.dialogs),
        ]
        .into_iter()
        .flat_map(|(kind, ids)| {
            ids.iter().map(move |id| ObligationDebug {
                kind: kind.name().to_string(),
                id: Some(id.clone()),
            })
        })
        .collect()
    }
}

#[derive(Default)]
pub(super) struct Runtime {
    pub(super) control: Option<Control>,
    pub(super) session_id: Option<Uuid>,
    pending: PendingRequests,
    // A provider reply can arrive before its stdin write returns. Publish the
    // accepted prompt and receipt before ingesting that reply.
    prompt_publication: Arc<tokio::sync::Mutex<()>>,
    facts: SessionFacts,
    pub(super) inflight_inputs: usize,
    pub(super) ready: bool,
    pub(super) exited: bool,
}

pub(crate) struct ClaudeSdkBackend {
    agent_id: Uuid,
    name: Option<String>,
    command: String,
    working_dir: PathBuf,
    args: Vec<String>,
    parent: Option<AgentParent>,
    name_source: LocalAgentNameSource,
    created_at: DateTime<Utc>,
    launch_route: Option<McpLaunchRoute>,
    artifact_root: PathBuf,
    runtime: Arc<Mutex<Runtime>>,
    input_done: Arc<Notify>,
    log: StructuredLogSource,
    injected: Option<Session>,
    resumed: bool,
    started: bool,
    ingest_abort: Option<AbortHandle>,
}

impl ClaudeSdkBackend {
    pub(in crate::agents) fn new(req: &CreateAgentRequest, launch_route: McpLaunchRoute) -> Self {
        debug_assert_eq!(
            req.agent_type,
            AgentType::Claude {
                driver: ClaudeDriver::Sdk
            }
        );
        Self {
            agent_id: req.agent_id,
            name: req.name.clone(),
            command: "claude".to_string(),
            working_dir: req.working_dir.clone(),
            args: req.args.clone(),
            parent: req.parent,
            name_source: if req.name.is_some() {
                LocalAgentNameSource::Amux
            } else {
                LocalAgentNameSource::Unset
            },
            created_at: Utc::now(),
            launch_route: Some(launch_route),
            artifact_root: req.working_dir.join(".amux-artifacts"),
            runtime: Arc::new(Mutex::new(Runtime {
                facts: SessionFacts::from_args(&req.args),
                session_id: Some(req.agent_id),
                ..Runtime::default()
            })),
            input_done: Arc::new(Notify::new()),
            log: StructuredLogSource::new(STRUCTURED_LOG_RETENTION),
            injected: None,
            resumed: false,
            started: false,
            ingest_abort: None,
        }
    }

    pub(in crate::agents) fn from_suspended(
        req: &CreateAgentRequest,
        name_source: LocalAgentNameSource,
        session_id: Uuid,
        created_at: DateTime<Utc>,
        launch_route: McpLaunchRoute,
    ) -> Self {
        let mut backend = Self::new(req, launch_route);
        backend.args = sanitize_resume_args(backend.args);
        backend.name_source = name_source;
        backend.created_at = created_at;
        backend.resumed = true;
        backend
            .runtime
            .lock()
            .expect("Claude SDK runtime poisoned")
            .session_id = Some(session_id);
        backend
    }

    #[allow(dead_code)]
    pub(crate) fn with_session(record: AgentRecord, session: Session) -> Self {
        debug_assert_eq!(
            record.kind,
            AgentKind::Claude {
                driver: ClaudeDriver::Sdk
            }
        );
        let session_id = session.control.session_id().parse().ok();
        let artifact_root = record.working_dir.join(".amux-artifacts");
        let facts = SessionFacts::from_args(&record.args);
        Self {
            agent_id: record.id,
            name: record.name.clone(),
            command: record.command,
            working_dir: record.working_dir,
            args: record.args,
            parent: record.parent,
            name_source: if record.name.is_some() {
                LocalAgentNameSource::Amux
            } else {
                LocalAgentNameSource::Unset
            },
            created_at: record.created_at,
            launch_route: None,
            artifact_root,
            runtime: Arc::new(Mutex::new(Runtime {
                session_id,
                facts,
                ..Runtime::default()
            })),
            input_done: Arc::new(Notify::new()),
            log: StructuredLogSource::new(STRUCTURED_LOG_RETENTION),
            injected: Some(session),
            resumed: false,
            started: false,
            ingest_abort: None,
        }
    }

    pub(in crate::agents) fn with_artifact_root(mut self, artifact_root: PathBuf) -> Self {
        self.artifact_root = artifact_root;
        self
    }

    #[cfg(test)]
    pub(in crate::agents) fn permissions_allow_for_tests(&self) -> Result<Value> {
        let Some(SettingsConfig::Inline(settings)) = self.query_options()?.settings else {
            return Err(anyhow!("managed SDK settings must be inline"));
        };
        Ok(settings["permissions"]["allow"].clone())
    }

    fn input_target(&self) -> ClaudeSdkInputTarget {
        ClaudeSdkInputTarget {
            runtime: self.runtime.clone(),
            input_done: self.input_done.clone(),
            log: self.log.clone(),
        }
    }

    pub(super) fn delivery_snapshot(&self) -> (Arc<Mutex<Runtime>>, StructuredLogSource) {
        (self.runtime.clone(), self.log.clone())
    }

    fn query_options(&self) -> Result<QueryOptions> {
        let route = self
            .launch_route
            .as_ref()
            .context("managed Claude SDK session is missing its MCP launch route")?;
        route
            .validate()
            .context("managed Claude MCP launch route is no longer valid")?;

        let mut args = sanitize_resume_args(self.args.clone());
        let settings_sources = claude::launch::take_settings_args(&mut args)?;
        let args = claude::launch::without_managed_spawn_args(&args);
        let mut options = QueryOptions {
            cli_path: Some(self.command.clone().into()),
            cwd: Some(self.working_dir.clone()),
            env: Some(scrubbed_environment()),
            include_partial_messages: true,
            ..QueryOptions::default()
        };

        let session_id = self
            .runtime
            .lock()
            .expect("Claude SDK runtime poisoned")
            .session_id
            .unwrap_or(self.agent_id)
            .to_string();
        if self.resumed {
            options.resume = Some(session_id);
        } else {
            options.session_id = Some(session_id);
        }

        let user_settings =
            claude::launch::load_user_settings(&self.working_dir, &settings_sources)?;
        let settings = claude::launch::merged_settings(
            user_settings,
            &claude::launch::ManagedSettings {
                hook_command: Vec::new(),
                mcp_servers: Vec::new(),
                permissions_allow: vec![crate::agents::artifact_read_rule(&self.artifact_root)],
            },
        )
        .into_value();
        options.settings = Some(SettingsConfig::Inline(settings));

        let executable = route
            .executable()
            .to_str()
            .context("the running amux executable path is not valid UTF-8")?;
        let socket = route
            .socket_path()
            .to_str()
            .context("the daemon socket path is not valid UTF-8")?;
        let mut env = HashMap::from([
            ("AMUX_AGENT_ID".to_string(), self.agent_id.to_string()),
            ("AMUX_HOST_ID".to_string(), route.host_id().to_string()),
        ]);
        let path = route.config_path()?;
        env.insert(
            "AMUX_CONFIG".to_string(),
            path.to_str()
                .context("the amux config path is not valid UTF-8")?
                .to_string(),
        );
        options.mcp_servers.insert(
            "amux".to_string(),
            McpServerConfig::Stdio(McpStdioServerConfig {
                command: executable.to_string(),
                args: vec![
                    "mcp".to_string(),
                    "agent".to_string(),
                    "--socket-path".to_string(),
                    socket.to_string(),
                ],
                env,
                timeout: None,
                always_load: None,
            }),
        );
        options.allowed_tools.push("mcp__amux__*".to_string());
        insert_extra_args(&mut options, &args)?;
        options.extra_args.insert(
            "name".to_string(),
            Some(
                self.name
                    .clone()
                    .unwrap_or_else(|| self.agent_id.to_string()),
            ),
        );
        Ok(options)
    }

    fn start_session_task(
        &mut self,
        event_tx: &mpsc::Sender<SessionEvent>,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let event_tx = event_tx.clone();
        let runtime = self.runtime.clone();
        let log = self.log.clone();
        let input_done = self.input_done.clone();
        let agent_id = self.agent_id;
        let resumed = self.resumed;

        let handle = if let Some(session) = self.injected.take() {
            tokio::spawn(ingest_session(
                agent_id, resumed, session, runtime, input_done, log, event_tx,
            ))
        } else {
            let options = self.query_options()?;
            tokio::spawn(async move {
                match claude::sdk::spawn(options).await {
                    Ok(session) => {
                        ingest_session(
                            agent_id, resumed, session, runtime, input_done, log, event_tx,
                        )
                        .await
                    }
                    Err(error) => {
                        tracing::error!(%agent_id, %error, "failed to spawn Claude SDK session");
                        log.close().await;
                    }
                }
            })
        };
        self.ingest_abort = Some(handle.abort_handle());
        Ok(handle)
    }
}

fn scrubbed_environment() -> HashMap<String, String> {
    std::env::vars()
        .filter(|(name, _)| !claude::launch::CHILD_SESSION_ENV_SCRUB.contains(&name.as_str()))
        .collect()
}

fn insert_extra_args(options: &mut QueryOptions, args: &[String]) -> Result<()> {
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        let Some(long) = argument.strip_prefix("--") else {
            return Err(anyhow!(
                "Claude SDK arguments must use long flags; unsupported argument `{argument}`"
            ));
        };
        let (name, inline_value) = long.split_once('=').map_or((long, None), |(name, value)| {
            (name, Some(value.to_string()))
        });
        if name.is_empty() {
            return Err(anyhow!("Claude SDK argument name cannot be empty"));
        }
        let has_inline_value = inline_value.is_some();
        let value = inline_value.or_else(|| {
            args.get(index + 1)
                .filter(|value| !value.starts_with('-'))
                .cloned()
        });
        if !has_inline_value && value.is_some() {
            index += 1;
        }
        if options.extra_args.insert(name.to_string(), value).is_some() {
            return Err(anyhow!("Claude SDK argument `--{name}` cannot be repeated"));
        }
        index += 1;
    }
    if options.extra_args.contains_key("permission-mode") {
        options.permission_mode = None;
    }
    Ok(())
}

async fn ingest_session(
    agent_id: Uuid,
    resumed: bool,
    session: Session,
    runtime: Arc<Mutex<Runtime>>,
    input_done: Arc<Notify>,
    log: StructuredLogSource,
    event_tx: mpsc::Sender<SessionEvent>,
) {
    let Session {
        mut events,
        control,
    } = session;
    let session_id = control.session_id().to_string();
    {
        let mut state = runtime.lock().expect("Claude SDK runtime poisoned");
        state.session_id = session_id.parse().ok();
        state
            .facts
            .initialize_commands(control.supported_commands().unwrap_or_default());
        state
            .facts
            .initialize_models(control.supported_models().unwrap_or_default());
        state.control = Some(control.clone());
    }
    if resumed {
        write_synthesized(
            &log,
            ClaudeSdkSynthesized::Gap {
                resumed_session_id: session_id.clone(),
            },
        )
        .await;
    }
    write_synthesized(
        &log,
        ClaudeSdkSynthesized::Ready {
            session_id,
            resumed,
        },
    )
    .await;
    write_session_facts(&runtime, &log).await;
    runtime.lock().expect("Claude SDK runtime poisoned").ready = true;

    let prompt_publication = runtime
        .lock()
        .expect("Claude SDK runtime poisoned")
        .prompt_publication
        .clone();
    while let Some(event) = events.next().await {
        let _publication = prompt_publication.lock().await;
        match event {
            Ok(SdkEvent::Message(message)) => {
                let completed = match &message {
                    claude::sdk::Message::Result(claude::sdk::ResultMessage::Success(result)) => {
                        Some(result.result.clone())
                    }
                    _ => None,
                };
                let facts = {
                    let mut state = runtime.lock().expect("Claude SDK runtime poisoned");
                    state.facts.observe(&message).then(|| state.facts.row())
                };
                match serde_json::to_value(message) {
                    Ok(row) => log.write(row).await,
                    Err(error) => {
                        tracing::warn!(%agent_id, %error, "failed to serialize Claude SDK row")
                    }
                }
                if let Some(facts) = facts {
                    write_synthesized(&log, facts).await;
                }
                if let Some(text) = completed {
                    let _ = event_tx
                        .send(SessionEvent::Completed { agent_id, text })
                        .await;
                }
            }
            Ok(SdkEvent::PermissionRequest {
                id,
                tool_name,
                input,
                suggestions,
                blocked_path: _,
            }) => {
                runtime
                    .lock()
                    .expect("Claude SDK runtime poisoned")
                    .pending
                    .permissions
                    .insert(id.clone());
                write_synthesized(
                    &log,
                    ClaudeSdkSynthesized::PermissionRequired {
                        request_id: id,
                        tool_name,
                        input,
                        suggestions: suggestions
                            .into_iter()
                            .map(|suggestion| {
                                serde_json::to_value(suggestion)
                                    .expect("Claude permission suggestions serialize as JSON")
                            })
                            .collect(),
                    },
                )
                .await;
            }
            Ok(SdkEvent::HookCallback { id, input, context }) => {
                log.write(control_request_row(
                    &id,
                    "hook_callback",
                    json!({"input": input, "context": context_row(context)}),
                ))
                .await;
                if let Err(error) = control.answer_hook(id, default_hook_output()).await {
                    tracing::warn!(%agent_id, %error, "failed to answer Claude SDK hook callback");
                }
            }
            Ok(SdkEvent::Elicitation { id, request }) => {
                runtime
                    .lock()
                    .expect("Claude SDK runtime poisoned")
                    .pending
                    .elicitations
                    .insert(id.clone());
                write_synthesized(
                    &log,
                    ClaudeSdkSynthesized::ElicitationRequired {
                        request_id: id,
                        server: Some(request.server_name),
                        message: request.message,
                        schema: request.requested_schema.unwrap_or(Value::Null),
                    },
                )
                .await;
            }
            Ok(SdkEvent::UserDialog { id, request }) => {
                runtime
                    .lock()
                    .expect("Claude SDK runtime poisoned")
                    .pending
                    .dialogs
                    .insert(id.clone());
                write_synthesized(
                    &log,
                    ClaudeSdkSynthesized::DialogRequired {
                        request_id: id,
                        dialog_kind: request.dialog_kind,
                        payload: request.payload,
                    },
                )
                .await;
            }
            Ok(SdkEvent::Exited(_)) => break,
            Err(error) => {
                tracing::warn!(%agent_id, %error, "Claude SDK event stream failed");
                break;
            }
        }
    }

    wait_for_inputs(&runtime, &input_done).await;
    let pending = {
        let mut state = runtime.lock().expect("Claude SDK runtime poisoned");
        state.control = None;
        state.exited = true;
        state.pending.drain()
    };
    for (kind, request_id) in pending {
        write_synthesized(&log, kind.resolution(request_id, "session_exited")).await;
    }
    log.close().await;
}

async fn wait_for_inputs(runtime: &Arc<Mutex<Runtime>>, input_done: &Notify) {
    loop {
        let notified = input_done.notified();
        if runtime
            .lock()
            .expect("Claude SDK runtime poisoned")
            .inflight_inputs
            == 0
        {
            return;
        }
        notified.await;
    }
}

fn context_row(context: claude::sdk::HookCallbackContext) -> Value {
    let mut value = serde_json::Map::from_iter([
        ("request_id".to_string(), Value::String(context.request_id)),
        (
            "tool_use_id".to_string(),
            context.tool_use_id.map_or(Value::Null, Value::String),
        ),
    ]);
    value.extend(context.extensions);
    Value::Object(value)
}

fn control_request_row(request_id: &str, subtype: &str, payload: Value) -> Value {
    let mut request = payload
        .as_object()
        .cloned()
        .unwrap_or_else(|| serde_json::Map::from_iter([("payload".to_string(), payload)]));
    request.insert("subtype".to_string(), Value::String(subtype.to_string()));
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": request,
    })
}

fn default_hook_output() -> HookOutput {
    HookOutput::Sync(SyncHookOutput {
        r#continue: None,
        suppress_output: None,
        stop_reason: None,
        decision: None,
        system_message: None,
        reason: None,
        hook_specific_output: None,
    })
}

async fn write_session_facts(runtime: &Mutex<Runtime>, log: &StructuredLogSource) {
    let row = runtime
        .lock()
        .expect("Claude SDK runtime poisoned")
        .facts
        .row();
    write_synthesized(log, row).await;
}

async fn write_synthesized(log: &StructuredLogSource, row: ClaudeSdkSynthesized) {
    log.write(ClaudeSdkV1Row::Synthesized(row).into_json())
        .await;
}

struct ClaudeSdkInputTarget {
    runtime: Arc<Mutex<Runtime>>,
    input_done: Arc<Notify>,
    log: StructuredLogSource,
}

impl ClaudeSdkInputTarget {
    fn control(&self) -> Result<Control> {
        self.runtime
            .lock()
            .expect("Claude SDK runtime poisoned")
            .control
            .clone()
            .ok_or_else(|| anyhow!("Claude SDK session is not active"))
    }

    async fn execute(&self, input_id: &[u8], input: ClaudeSdkV1Input) -> Result<()> {
        let control = self.control()?;
        match input {
            ClaudeSdkV1Input::Prompt {
                text,
                mut image_blocks,
            } => {
                let message = if image_blocks.is_empty() {
                    UserMessage::text(text)
                } else {
                    let mut blocks = Vec::with_capacity(image_blocks.len() + 1);
                    blocks.push(ContentBlock::Text {
                        text,
                        extensions: Default::default(),
                    });
                    blocks.append(&mut image_blocks);
                    UserMessage::new(
                        MessageParam {
                            role: Role::User,
                            content: MessageContent::Blocks(blocks),
                            extensions: Default::default(),
                        },
                        None,
                    )
                };
                let identity = Uuid::from_slice(input_id)
                    .map(|id| id.to_string())
                    .unwrap_or_else(|_| crate::agents::attachments::hex_bytes(input_id));
                let row = json!({
                    "type": "user",
                    "uuid": identity,
                    "input_id": crate::agents::attachments::hex_bytes(input_id),
                    "session_id": control.session_id(),
                    "parent_tool_use_id": message.parent_tool_use_id,
                    "message": message.message,
                });
                control.prompt(message).await?;
                self.log.write(row).await;
            }
            ClaudeSdkV1Input::SetPermissionMode { mode } => {
                self.runtime
                    .lock()
                    .expect("Claude SDK runtime poisoned")
                    .facts
                    .check_mode(&mode)?;
                let applied = control
                    .set_permission_mode(mode.clone())
                    .await?
                    .unwrap_or(mode);
                self.runtime
                    .lock()
                    .expect("Claude SDK runtime poisoned")
                    .facts
                    .permission_mode = Some(applied.as_str().into());
                write_session_facts(&self.runtime, &self.log).await;
            }
            ClaudeSdkV1Input::SetModel { model } => {
                control.set_model(model.as_deref()).await?;
                {
                    let mut state = self.runtime.lock().expect("Claude SDK runtime poisoned");
                    state.facts.model = model.or_else(|| state.facts.launch_model.clone());
                }
                write_session_facts(&self.runtime, &self.log).await;
            }
            ClaudeSdkV1Input::SetEffort { effort } => {
                control.set_effort(effort.clone()).await?;
                self.runtime
                    .lock()
                    .expect("Claude SDK runtime poisoned")
                    .facts
                    .effort = effort.map(|effort| effort.as_str().to_owned());
                write_session_facts(&self.runtime, &self.log).await;
            }
            ClaudeSdkV1Input::RequestContextBreakdown => {
                let usage = control.get_context_usage().await?;
                write_synthesized(
                    &self.log,
                    ClaudeSdkSynthesized::ContextBreakdown {
                        usage: Box::new(usage),
                    },
                )
                .await;
            }
            ClaudeSdkV1Input::Interrupt => {
                control.interrupt().await?;
            }
            ClaudeSdkV1Input::PermissionDecision {
                request_id,
                decision,
            } => {
                if let PermissionResult::Allow {
                    updated_permissions: Some(updates),
                    ..
                } = &decision
                {
                    for update in updates {
                        if let claude::sdk::PermissionUpdate::SetMode { mode, .. } = update {
                            self.runtime
                                .lock()
                                .expect("Claude SDK runtime poisoned")
                                .facts
                                .check_mode(mode)?;
                        }
                    }
                }
                let decision_name = permission_decision_name(&decision);
                self.take_pending(RequestKind::Permission, &request_id)?;
                let result = control
                    .answer_permission(request_id.clone(), decision)
                    .await;
                self.resolve(RequestKind::Permission, request_id, decision_name, result)
                    .await?;
            }
            ClaudeSdkV1Input::ElicitationDecision { request_id, result } => {
                let decision = match &result {
                    ElicitationResult::Accept { .. } => "accept",
                    ElicitationResult::Decline { .. } => "decline",
                    ElicitationResult::Cancel { .. } => "cancel",
                };
                self.take_pending(RequestKind::Elicitation, &request_id)?;
                let result = control.answer_elicitation(request_id.clone(), result).await;
                self.resolve(RequestKind::Elicitation, request_id, decision, result)
                    .await?;
            }
            ClaudeSdkV1Input::DialogDecision { request_id, result } => {
                let decision = match &result {
                    UserDialogResult::Completed { .. } => "completed",
                    UserDialogResult::Cancelled { .. } => "cancelled",
                };
                self.take_pending(RequestKind::Dialog, &request_id)?;
                let result = control.answer_user_dialog(request_id.clone(), result).await;
                self.resolve(RequestKind::Dialog, request_id, decision, result)
                    .await?;
            }
        }
        Ok(())
    }

    fn take_pending(&self, kind: RequestKind, request_id: &str) -> Result<()> {
        let mut state = self.runtime.lock().expect("Claude SDK runtime poisoned");
        if !state.pending.ids(kind).remove(request_id) {
            return Err(anyhow!(
                "unknown or already-resolved {} request id",
                kind.name()
            ));
        }
        Ok(())
    }

    async fn resolve(
        &self,
        kind: RequestKind,
        request_id: String,
        decision: &str,
        result: std::result::Result<(), claude::sdk::Error>,
    ) -> Result<()> {
        write_synthesized(
            &self.log,
            kind.resolution(
                request_id,
                if result.is_ok() {
                    decision
                } else {
                    "response_failed"
                },
            ),
        )
        .await;
        result?;
        Ok(())
    }

    async fn send(&self, input_id: Vec<u8>, input: ClaudeSdkV1Input) {
        self.runtime
            .lock()
            .expect("Claude SDK runtime poisoned")
            .inflight_inputs += 1;
        let publication = self
            .runtime
            .lock()
            .expect("Claude SDK runtime poisoned")
            .prompt_publication
            .clone();
        let _publication = if matches!(&input, ClaudeSdkV1Input::Prompt { .. }) {
            Some(publication.lock().await)
        } else {
            None
        };
        let outcome = match self.execute(&input_id, input).await {
            Ok(()) => "ok".to_string(),
            Err(error) => error.to_string(),
        };
        write_synthesized(
            &self.log,
            ClaudeSdkSynthesized::InputResult { input_id, outcome },
        )
        .await;
        self.runtime
            .lock()
            .expect("Claude SDK runtime poisoned")
            .inflight_inputs -= 1;
        self.input_done.notify_waiters();
    }
}

fn permission_decision_name(decision: &PermissionResult) -> &'static str {
    match decision {
        PermissionResult::Allow { .. } => "allow",
        PermissionResult::Deny { .. } => "deny",
    }
}

#[async_trait]
impl StructuredInput for ClaudeSdkInputTarget {
    async fn send(&self, input: StructuredInputEvent) -> std::result::Result<(), ProtocolError> {
        let StructuredInputEvent::ClaudeSdk { input_id, input } = input else {
            return Err(ProtocolError::InvalidArgument {
                message: "Claude SDK input target received another protocol's input".to_string(),
            });
        };
        self.send(input_id, input).await;
        Ok(())
    }
}

#[async_trait]
impl AgentBackend for ClaudeSdkBackend {
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
        false
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
            return Err(anyhow!(
                "Claude SDK session {} already started",
                self.agent_id
            ));
        }
        self.started = true;
        self.start_session_task(event_tx)
    }

    async fn stop(&self, _policy: StopPolicy) {
        let control = self
            .runtime
            .lock()
            .expect("Claude SDK runtime poisoned")
            .control
            .clone();
        if let Some(control) = control {
            let _ = control.close().await;
        }
        if let Some(abort) = &self.ingest_abort {
            abort.abort();
        }
        let pending = self
            .runtime
            .lock()
            .expect("Claude SDK runtime poisoned")
            .pending
            .drain();
        for (kind, request_id) in pending {
            write_synthesized(&self.log, kind.resolution(request_id, "session_stopped")).await;
        }
        self.log.close().await;
    }

    fn kind(&self) -> AgentKind {
        AgentKind::Claude {
            driver: ClaudeDriver::Sdk,
        }
    }

    fn plane(&self, protocol: Protocol) -> std::result::Result<Plane, ProtocolError> {
        match protocol {
            Protocol::ClaudeSdkV1 => Ok(Plane::Structured {
                log: self.log.clone(),
                input: Box::new(self.input_target()),
            }),
            Protocol::TerminalV1
            | Protocol::ClaudePtyTranscriptV1
            | Protocol::CodexSdkV1
            | Protocol::TestEchoV1 => Err(ProtocolError::NotExposed {
                kind: self.kind(),
                protocol,
            }),
        }
    }

    fn attachment_log(&self) -> Option<StructuredLogSource> {
        Some(self.log.clone())
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
        Box::new(ClaudeSdkDeliveryTarget::new(self))
    }

    fn local_name_source(&self) -> Option<LocalAgentNameSource> {
        Some(self.name_source)
    }

    fn suspended_state(&self) -> Result<SuspendedAgent> {
        let session_id = self
            .runtime
            .lock()
            .expect("Claude SDK runtime poisoned")
            .session_id
            .ok_or_else(|| {
                anyhow!(
                    "cannot suspend Claude SDK agent {}: session id is unavailable",
                    self.agent_id
                )
            })?;
        Ok(ClaudeSuspendRecord {
            driver: ClaudeDriver::Sdk,
            agent_id: self.agent_id,
            name: self.name.clone(),
            name_source: self.name_source,
            working_dir: self.working_dir.clone(),
            terminal_size: None,
            created_at: self.created_at,
            args: self.args.clone(),
            session_id,
            parent: self.parent,
        }
        .into())
    }

    async fn debug_json(&self, verbose: bool) -> serde_json::Result<Value> {
        let (active, ready, exited, obligations) = {
            let runtime = self.runtime.lock().expect("Claude SDK runtime poisoned");
            (
                runtime.control.is_some(),
                runtime.ready,
                runtime.exited,
                runtime.pending.obligations(),
            )
        };
        let output = self.log.debug_snapshot().await;
        let backend = if exited {
            BackendState::Exited { code: None }
        } else if active || ready {
            BackendState::Running { pid: None }
        } else {
            BackendState::Starting
        };
        let session =
            SessionDebug::new(Some(&output), output.subscriber_count, backend, obligations);
        let mut value = serde_json::to_value(DebugView::new(self, verbose))?;
        value
            .as_object_mut()
            .expect("Claude SDK debug view is an object")
            .insert("session".to_string(), serde_json::to_value(session)?);
        Ok(value)
    }
}

impl Serialize for DebugView<'_, ClaudeSdkBackend> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let runtime = self
            .inner
            .runtime
            .lock()
            .expect("Claude SDK runtime poisoned");
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("kind", "claude/sdk")?;
        if let Some(session_id) = runtime.session_id {
            map.serialize_entry("session_id", &session_id)?;
        }
        map.serialize_entry("pending_permissions", &runtime.pending.permissions.len())?;
        map.serialize_entry("pending_elicitations", &runtime.pending.elicitations.len())?;
        map.serialize_entry("pending_dialogs", &runtime.pending.dialogs.len())?;
        map.serialize_entry("active", &runtime.control.is_some())?;
        map.serialize_entry("exited", &runtime.exited)?;
        let _ = self.verbose;
        map.end()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use claude::sdk::{HookEvent, HookSubscription};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

    use super::*;
    use crate::agents::mcp_launch_route_for_tests;

    /// How long a test waits for a real child process to produce its first
    /// output. These tests share a machine with whatever else is building, so
    /// the deadline is only here to turn a genuine hang into a failure rather
    /// than to measure how fast the process starts.
    const START_DEADLINE: Duration = Duration::from_secs(30);

    /// Wait for a stand-in `claude` binary to record the arguments it was
    /// launched with. The stand-ins write the capture to a neighbouring path
    /// and rename it into place, so a file that exists is a complete one.
    #[cfg(unix)]
    async fn launch_arguments(path: &std::path::Path) -> Vec<String> {
        let deadline = std::time::Instant::now() + START_DEADLINE;
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                return text.lines().map(str::to_string).collect();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Claude never recorded its launch arguments at {}",
                path.display()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub(super) async fn read_json_line(
        reader: &mut BufReader<tokio::io::DuplexStream>,
    ) -> serde_json::Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    pub(super) async fn write_json_line(
        writer: &mut tokio::io::DuplexStream,
        value: serde_json::Value,
    ) {
        writer
            .write_all(value.to_string().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
    }

    pub(super) fn record(id: Uuid) -> AgentRecord {
        AgentRecord {
            id,
            host_id: Uuid::new_v4(),
            name: Some("sdk-test".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
            kind: AgentKind::Claude {
                driver: ClaudeDriver::Sdk,
            },
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
            parent: None,
            working_on: None,
        }
    }

    #[tokio::test]
    async fn debug_json_embeds_sdk_session_state() {
        let req = CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            host_id: None,
            name: None,
            agent_type: AgentType::Claude {
                driver: ClaudeDriver::Sdk,
            },
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        };
        let backend = ClaudeSdkBackend::new(&req, mcp_launch_route_for_tests(Uuid::new_v4()));
        let debug = backend.debug_json(true).await.unwrap();

        assert_eq!(debug["session"]["epoch"], 0);
        assert!(debug["session"]["buffer"].is_object());
        assert_eq!(debug["session"]["backend"]["state"], "starting");
        assert!(debug["session"]["obligations"].is_array());
    }

    #[test]
    fn managed_sdk_launch_pins_mcp_and_preapproves_artifact_reads() {
        let artifact_root = PathBuf::from("/var/amux/agents/test/artifacts");
        let req = CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            host_id: None,
            name: Some("attachments".to_string()),
            agent_type: AgentType::Claude {
                driver: ClaudeDriver::Sdk,
            },
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: vec![
                "--settings".to_string(),
                r#"{"permissions":{"allow":["Read(/user/**)"]}}"#.to_string(),
            ],
            parent: None,
            initial_prompt: None,
        };
        let backend = ClaudeSdkBackend::new(&req, mcp_launch_route_for_tests(Uuid::new_v4()))
            .with_artifact_root(artifact_root.clone());

        let options = backend.query_options().unwrap();
        let McpServerConfig::Stdio(mcp) = &options.mcp_servers["amux"] else {
            panic!("managed MCP server must use stdio");
        };
        let route = backend.launch_route.as_ref().unwrap();
        assert_eq!(
            mcp.env["AMUX_CONFIG"],
            route.config_path().unwrap().to_str().unwrap()
        );
        assert_eq!(
            mcp.args,
            vec![
                "mcp",
                "agent",
                "--socket-path",
                route.socket_path().to_str().unwrap()
            ]
        );
        let Some(SettingsConfig::Inline(settings)) = options.settings else {
            panic!("managed SDK settings must be inline");
        };
        assert_eq!(
            settings["permissions"]["allow"],
            json!([
                "Read(/user/**)",
                crate::agents::artifact_read_rule(&artifact_root)
            ])
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_sdk_launch_settings_stream_and_leave_user_hooks_to_claude() {
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("capture-claude-argv.sh");
        let argv = directory.path().join("argv.txt");
        std::fs::write(
            &cli,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$AMUX_ARGV_CAPTURE.partial\"\nmv \"$AMUX_ARGV_CAPTURE.partial\" \"$AMUX_ARGV_CAPTURE\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();

        let req = CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            host_id: None,
            name: Some("settings-test".to_string()),
            agent_type: AgentType::Claude {
                driver: ClaudeDriver::Sdk,
            },
            working_dir: directory.path().to_path_buf(),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        };
        let mut backend = ClaudeSdkBackend::new(&req, mcp_launch_route_for_tests(Uuid::new_v4()));
        backend.command = cli.to_string_lossy().into_owned();
        let mut options = backend.query_options().unwrap();

        assert!(options.include_partial_messages);
        assert!(options.setting_sources.is_empty());
        assert!(options.hook_subscriptions.is_empty());
        let Some(SettingsConfig::Inline(settings)) = options.settings.as_ref() else {
            panic!("managed SDK settings must be inline");
        };
        assert!(settings.get("hooks").is_none());

        options.env.as_mut().unwrap().insert(
            "AMUX_ARGV_CAPTURE".to_string(),
            argv.to_string_lossy().into_owned(),
        );
        // The stand-in binary exits without answering the handshake, so the
        // spawn never finishes; the launch arguments it recorded are the point.
        let launching = tokio::spawn(claude::sdk::spawn(options));
        let launched = launch_arguments(&argv).await;
        launching.abort();
        let launched = launched.iter().map(String::as_str).collect::<Vec<_>>();
        assert!(launched.contains(&"--include-partial-messages"));
        assert!(!launched.contains(&"--setting-sources"));
        let settings_index = launched
            .iter()
            .position(|argument| *argument == "--settings")
            .expect("managed settings argument");
        let launched_settings: Value =
            serde_json::from_str(launched[settings_index + 1]).expect("inline settings JSON");
        assert!(launched_settings.get("hooks").is_none());
    }

    async fn provider_session(
        transcript_path: String,
    ) -> (Session, tokio::task::JoinHandle<()>, Uuid) {
        let session_id = Uuid::new_v4();
        let (sdk_stdin, server_stdin) = duplex(32 * 1024);
        let (server_stdout, sdk_stdout) = duplex(32 * 1024);
        let server = tokio::spawn(async move {
            let mut stdin = BufReader::new(server_stdin);
            let mut stdout = server_stdout;
            let init = read_json_line(&mut stdin).await;
            let init_id = init["request_id"].as_str().unwrap();
            let hook_id = init["request"]["hooks"]["PreToolUse"][0]["hookCallbackIds"][0]
                .as_str()
                .unwrap()
                .to_string();
            write_json_line(
                &mut stdout,
                json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "success",
                        "request_id": init_id,
                        "response": {
                            "commands": [],
                            "agents": [],
                            "output_style": "default",
                            "available_output_styles": [],
                            "models": [],
                            "account": {}
                        }
                    }
                }),
            )
            .await;

            let prompt = read_json_line(&mut stdin).await;
            assert_eq!(prompt["message"]["content"], "hello from amux");
            write_json_line(
                &mut stdout,
                json!({
                    "type": "prompt_suggestion",
                    "suggestion": "continue",
                    "uuid": Uuid::nil(),
                    "session_id": session_id,
                }),
            )
            .await;
            write_json_line(
                &mut stdout,
                json!({
                    "type": "control_request",
                    "request_id": "permission-1",
                    "request": {
                        "subtype": "can_use_tool",
                        "tool_name": "Bash",
                        "input": {"command": "pwd"},
                        "permission_suggestions": [],
                        "tool_use_id": "tool-1"
                    }
                }),
            )
            .await;
            let permission = read_json_line(&mut stdin).await;
            assert_eq!(permission["response"]["request_id"], "permission-1");
            assert_eq!(permission["response"]["response"]["behavior"], "allow");

            write_json_line(
                &mut stdout,
                json!({
                    "type": "control_request",
                    "request_id": "hook-1",
                    "request": {
                        "subtype": "hook_callback",
                        "callback_id": hook_id,
                        "tool_use_id": "tool-1",
                        "input": {
                            "hook_event_name": "PreToolUse",
                            "session_id": session_id,
                            "transcript_path": transcript_path,
                            "cwd": "/tmp",
                            "tool_name": "Bash",
                            "tool_input": {"command": "pwd"},
                            "tool_use_id": "tool-1"
                        }
                    }
                }),
            )
            .await;
            let hook = read_json_line(&mut stdin).await;
            assert_eq!(hook["response"]["request_id"], "hook-1");
            assert_eq!(hook["response"]["response"], json!({}));

            write_json_line(
                &mut stdout,
                json!({
                    "type": "control_request",
                    "request_id": "elicitation-1",
                    "request": {
                        "subtype": "elicitation",
                        "mcp_server_name": "forms",
                        "message": "Pick one",
                        "requested_schema": {"type": "object", "properties": {"choice": {"type": "string"}}},
                        "future_field": "retained"
                    }
                }),
            )
            .await;
            let elicitation = read_json_line(&mut stdin).await;
            assert_eq!(elicitation["response"]["request_id"], "elicitation-1");
            assert_eq!(
                elicitation["response"]["response"],
                json!({"action": "accept", "content": {"choice": "a"}})
            );

            write_json_line(
                &mut stdout,
                json!({
                    "type": "control_request",
                    "request_id": "dialog-1",
                    "request": {
                        "subtype": "request_user_dialog",
                        "dialog_kind": "Future.Kind/v2",
                        "payload": {"title": "Continue?", "nested": [null, {"values": [1, true]}]},
                        "tool_use_id": "tool-1",
                        "future_field": "retained"
                    }
                }),
            )
            .await;
            let dialog = read_json_line(&mut stdin).await;
            assert_eq!(dialog["response"]["request_id"], "dialog-1");
            assert_eq!(
                dialog["response"]["response"],
                json!({"behavior": "completed", "result": {"confirmed": true}})
            );

            let interrupt = read_json_line(&mut stdin).await;
            let interrupt_id = interrupt["request_id"].as_str().unwrap();
            assert_eq!(interrupt["request"]["subtype"], "interrupt");
            write_json_line(
                &mut stdout,
                json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "success",
                        "request_id": interrupt_id,
                        "response": {}
                    }
                }),
            )
            .await;
        });

        let mut options = QueryOptions {
            session_id: Some(session_id.to_string()),
            ..QueryOptions::default()
        };
        options.hook_subscriptions.push(HookSubscription {
            event: HookEvent::PreToolUse,
            matcher: Some("Bash".to_string()),
        });
        let session = claude::sdk::from_io(BufReader::new(sdk_stdout), sdk_stdin, options)
            .await
            .unwrap();
        (session, server, session_id)
    }

    #[tokio::test]
    async fn claude_sdk_ingests_rows_answers_obligations_and_forwards_input() {
        let directory = tempfile::tempdir().unwrap();
        let transcript_dir = directory.path().join("transcripts");
        let transcript_path = transcript_dir.join("session.jsonl");
        std::fs::create_dir(&transcript_dir).unwrap();
        std::fs::write(&transcript_path, b"must not be opened").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&transcript_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let transcript_path_string = transcript_path.to_string_lossy().into_owned();
        let (session, server, session_id) = provider_session(transcript_path_string.clone()).await;
        let mut backend = ClaudeSdkBackend::with_session(record(session_id), session);
        let (event_tx, _event_rx) = mpsc::channel(8);
        let ingest = backend.start(&event_tx).unwrap();
        let Plane::Structured { log, input } = backend.plane(Protocol::ClaudeSdkV1).unwrap() else {
            panic!("Claude SDK plane must be structured");
        };
        let mut rows = log.subscribe().await.unwrap();
        let ready = tokio::time::timeout(START_DEADLINE, rows.read())
            .await
            .expect("SDK ready row timed out")
            .expect("SDK log closed before ready");
        assert_eq!(ready.payload["type"], "amux.claude_sdk.ready");
        input
            .send(StructuredInputEvent::ClaudeSdk {
                input_id: b"prompt-1".to_vec(),
                input: ClaudeSdkV1Input::Prompt {
                    text: "hello from amux".to_string(),
                    image_blocks: Vec::new(),
                },
            })
            .await
            .unwrap();

        let mut outputs = vec![ready];
        while let Some(output) = tokio::time::timeout(START_DEADLINE, rows.read())
            .await
            .expect("SDK backend row timed out")
        {
            let permission = output.payload["type"] == "amux.claude_sdk.permission_required";
            let elicitation = output.payload["type"] == "amux.claude_sdk.elicitation_required";
            let dialog = output.payload["type"] == "amux.claude_sdk.dialog_required";
            outputs.push(output);
            if permission {
                input
                    .send(StructuredInputEvent::ClaudeSdk {
                        input_id: b"permission-1".to_vec(),
                        input: ClaudeSdkV1Input::PermissionDecision {
                            request_id: "permission-1".to_string(),
                            decision: PermissionResult::Allow {
                                updated_input: Some(json!({"command": "pwd"})),
                                updated_permissions: None,
                                tool_use_id: Some("tool-1".to_string()),
                            },
                        },
                    })
                    .await
                    .unwrap();
            }
            if elicitation || dialog {
                let answer = if elicitation {
                    ClaudeSdkV1Input::ElicitationDecision {
                        request_id: "elicitation-1".into(),
                        result: ElicitationResult::Accept {
                            content: Some(json!({"choice": "a"})),
                            extensions: Default::default(),
                        },
                    }
                } else {
                    ClaudeSdkV1Input::DialogDecision {
                        request_id: "dialog-1".into(),
                        result: UserDialogResult::Completed {
                            result: json!({"confirmed": true}),
                            extensions: Default::default(),
                        },
                    }
                };
                let debug = backend.debug_json(true).await.unwrap();
                assert!(
                    debug["session"]["obligations"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|ask| {
                            ask["kind"] == if elicitation { "elicitation" } else { "dialog" }
                        })
                );
                input
                    .send(StructuredInputEvent::ClaudeSdk {
                        input_id: if elicitation {
                            b"elicitation-answer".to_vec()
                        } else {
                            b"dialog-answer".to_vec()
                        },
                        input: answer.clone(),
                    })
                    .await
                    .unwrap();
                input
                    .send(StructuredInputEvent::ClaudeSdk {
                        input_id: b"duplicate".to_vec(),
                        input: answer,
                    })
                    .await
                    .unwrap();
            }
            if dialog {
                input
                    .send(StructuredInputEvent::ClaudeSdk {
                        input_id: b"interrupt-1".to_vec(),
                        input: ClaudeSdkV1Input::Interrupt,
                    })
                    .await
                    .unwrap();
            }
        }
        ingest.await.unwrap();
        server.await.unwrap();

        assert!(
            outputs
                .iter()
                .enumerate()
                .all(|(index, output)| output.seq == index as u64 + 1)
        );
        assert_eq!(outputs[0].payload["type"], "amux.claude_sdk.ready");
        assert_eq!(outputs[0].payload["session_id"], session_id.to_string());
        assert!(outputs.iter().any(|row| {
            row.payload
                == json!({
                    "type": "prompt_suggestion",
                    "suggestion": "continue",
                    "uuid": Uuid::nil(),
                    "session_id": session_id,
                })
        }));
        assert!(outputs.iter().any(|row| {
            row.payload["type"] == "amux.claude_sdk.permission_required"
                && row.payload["request_id"] == "permission-1"
        }));
        assert!(
            outputs.iter().any(|row| {
                row.payload["type"] == "amux.claude_sdk.permission_resolved"
                    && row.payload["decision"] == "allow"
            }),
            "rows: {outputs:#?}"
        );
        assert!(outputs.iter().any(|row| {
            row.payload["type"] == "control_request"
                && row.payload["request_id"] == "hook-1"
                && row.payload["request"]["input"]["transcript_path"] == transcript_path_string
        }));
        assert!(outputs.iter().any(|row| {
            row.payload
                == json!({
                    "type": "amux.claude_sdk.elicitation_required", "request_id": "elicitation-1",
                    "server": "forms", "message": "Pick one",
                    "schema": {"type": "object", "properties": {"choice": {"type": "string"}}}
                })
        }));
        assert!(outputs.iter().any(|row| {
            row.payload == json!({
                "type": "amux.claude_sdk.dialog_required", "request_id": "dialog-1",
                "dialog_kind": "Future.Kind/v2", "payload": {"title": "Continue?", "nested": [null, {"values": [1, true]}]}
            })
        }));
        assert!(outputs.iter().any(|row| {
            row.payload["type"] == "amux.claude_sdk.input_result"
                && row.payload["input_id"] == json!(b"prompt-1")
                && row.payload["outcome"] == "ok"
        }));
        assert!(outputs.iter().any(|row| {
            row.payload["type"] == "amux.claude_sdk.input_result"
                && row.payload["input_id"] == json!(b"interrupt-1")
                && row.payload["outcome"] == "ok"
        }));
        for (kind, decision) in [("elicitation", "accept"), ("dialog", "completed")] {
            let resolved = outputs
                .iter()
                .position(|row| {
                    row.payload["type"] == format!("amux.claude_sdk.{kind}_resolved")
                        && row.payload["decision"] == decision
                })
                .unwrap();
            let acknowledged = outputs
                .iter()
                .position(|row| {
                    row.payload["input_id"] == json!(format!("{kind}-answer").as_bytes())
                })
                .unwrap();
            assert!(resolved < acknowledged);
            assert!(
                outputs
                    .iter()
                    .any(|row| row.payload["input_id"] == json!(b"duplicate")
                        && row.payload["outcome"]
                            == format!("unknown or already-resolved {kind} request id"))
            );
        }
        assert!(
            backend
                .runtime
                .lock()
                .unwrap()
                .pending
                .obligations()
                .is_empty()
        );
        assert!(
            backend
                .runtime
                .lock()
                .unwrap()
                .pending
                .permissions
                .is_empty()
        );
        if let Some(directory) = std::env::var_os("CLAUDE_SDK_ASK_EVIDENCE") {
            std::fs::create_dir_all(&directory).unwrap();
            let bytes = outputs
                .iter()
                .map(|row| format!("{}\n", serde_json::to_string(&row.payload).unwrap()))
                .collect::<String>();
            std::fs::write(
                PathBuf::from(directory).join("synthesized-elicitation-dialog-answers.rows.jsonl"),
                bytes,
            )
            .unwrap();
        }
        #[cfg(unix)]
        std::fs::set_permissions(&transcript_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            std::fs::read(transcript_path).unwrap(),
            b"must not be opened"
        );
    }

    #[tokio::test]
    async fn claude_sdk_asks_remain_pending_until_answer_or_session_exit() {
        let (sdk_stdin, server_stdin) = duplex(32 * 1024);
        let (server_stdout, sdk_stdout) = duplex(32 * 1024);
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut stdin = BufReader::new(server_stdin);
            let mut stdout = server_stdout;
            let init = read_json_line(&mut stdin).await;
            write_json_line(&mut stdout, json!({
                "type": "control_response", "response": {
                    "subtype": "success", "request_id": init["request_id"],
                    "response": {"commands": [], "agents": [], "output_style": "default", "available_output_styles": [], "models": [], "account": {}}
                }
            })).await;
            for (request_id, request) in [
                (
                    "permission",
                    json!({"subtype": "can_use_tool", "tool_name": "Bash", "input": {}, "permission_suggestions": [], "tool_use_id": "tool-1"}),
                ),
                (
                    "elicitation",
                    json!({"subtype": "elicitation", "mcp_server_name": "forms", "message": "Pick one"}),
                ),
                (
                    "dialog",
                    json!({"subtype": "request_user_dialog", "dialog_kind": "unknown", "payload": {}}),
                ),
            ] {
                write_json_line(&mut stdout, json!({"type": "control_request", "request_id": request_id, "request": request})).await;
            }
            tokio::select! {
                biased;
                reply = read_json_line(&mut stdin) => panic!("asks must not be auto-answered or accept an unknown id: {reply}"),
                _ = exit_rx => {},
            }
        });
        let session_id = Uuid::new_v4();
        let session = claude::sdk::from_io(
            BufReader::new(sdk_stdout),
            sdk_stdin,
            QueryOptions {
                session_id: Some(session_id.to_string()),
                ..QueryOptions::default()
            },
        )
        .await
        .unwrap();
        let mut backend = ClaudeSdkBackend::with_session(record(session_id), session);
        let (event_tx, _event_rx) = mpsc::channel(8);
        let ingest = backend.start(&event_tx).unwrap();
        let Plane::Structured { log, input } = backend.plane(Protocol::ClaudeSdkV1).unwrap() else {
            panic!("structured plane")
        };
        let mut rows = log.subscribe().await.unwrap();
        let mut outputs = Vec::new();
        while outputs
            .last()
            .is_none_or(|row: &Value| row["type"] != "amux.claude_sdk.dialog_required")
        {
            outputs.push(
                tokio::time::timeout(START_DEADLINE, rows.read())
                    .await
                    .unwrap()
                    .unwrap()
                    .payload,
            );
        }
        assert_eq!(
            backend.runtime.lock().unwrap().pending.obligations().len(),
            3
        );
        // A valid id from a different ask kind is still unknown to this input.
        for (kind, answer) in [
            (
                "permission",
                ClaudeSdkV1Input::PermissionDecision {
                    request_id: "elicitation".into(),
                    decision: PermissionResult::Deny {
                        message: "no".into(),
                        interrupt: None,
                        tool_use_id: None,
                    },
                },
            ),
            (
                "elicitation",
                ClaudeSdkV1Input::ElicitationDecision {
                    request_id: "dialog".into(),
                    result: ElicitationResult::Cancel {
                        extensions: Default::default(),
                    },
                },
            ),
            (
                "dialog",
                ClaudeSdkV1Input::DialogDecision {
                    request_id: "permission".into(),
                    result: UserDialogResult::Cancelled {
                        extensions: Default::default(),
                    },
                },
            ),
        ] {
            input
                .send(StructuredInputEvent::ClaudeSdk {
                    input_id: kind.as_bytes().to_vec(),
                    input: answer,
                })
                .await
                .unwrap();
            let row = tokio::time::timeout(START_DEADLINE, rows.read())
                .await
                .unwrap()
                .unwrap()
                .payload;
            assert_eq!(
                row,
                json!({"type": "amux.claude_sdk.input_result", "input_id": kind.as_bytes(), "outcome": format!("unknown or already-resolved {kind} request id")})
            );
            outputs.push(row);
        }
        assert_eq!(
            backend.runtime.lock().unwrap().pending.obligations().len(),
            3
        );
        exit_tx.send(()).unwrap();
        for kind in ["permission", "elicitation", "dialog"] {
            let row = tokio::time::timeout(START_DEADLINE, rows.read())
                .await
                .unwrap()
                .unwrap()
                .payload;
            assert_eq!(
                row,
                json!({"type": format!("amux.claude_sdk.{kind}_resolved"), "request_id": kind, "decision": "session_exited"})
            );
            outputs.push(row);
        }
        assert!(
            tokio::time::timeout(START_DEADLINE, rows.read())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            backend
                .runtime
                .lock()
                .unwrap()
                .pending
                .obligations()
                .is_empty()
        );
        ingest.await.unwrap();
        server.await.unwrap();
        if let Some(directory) = std::env::var_os("CLAUDE_SDK_ASK_EVIDENCE") {
            std::fs::create_dir_all(&directory).unwrap();
            let bytes = outputs
                .iter()
                .map(|row| format!("{}\n", serde_json::to_string(row).unwrap()))
                .collect::<String>();
            std::fs::write(
                PathBuf::from(directory).join("pending-asks-session-exit.rows.jsonl"),
                bytes,
            )
            .unwrap();
        }
    }

    #[test]
    fn plane_exposes_only_claude_sdk_v1() {
        let req = CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            host_id: None,
            name: None,
            agent_type: AgentType::Claude {
                driver: ClaudeDriver::Sdk,
            },
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        };
        let backend = ClaudeSdkBackend::new(&req, mcp_launch_route_for_tests(Uuid::new_v4()));
        assert!(matches!(
            backend.plane(Protocol::ClaudeSdkV1),
            Ok(Plane::Structured { .. })
        ));
        for protocol in [
            Protocol::TerminalV1,
            Protocol::ClaudePtyTranscriptV1,
            Protocol::CodexSdkV1,
            Protocol::TestEchoV1,
        ] {
            assert!(matches!(
                backend.plane(protocol),
                Err(ProtocolError::NotExposed {
                    kind: AgentKind::Claude {
                        driver: ClaudeDriver::Sdk
                    },
                    protocol: refused,
                }) if refused == protocol
            ));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawned_backend_uses_amux_session_id() {
        let directory = tempfile::tempdir().unwrap();
        let argv = directory.path().join("argv.txt");
        let script = directory.path().join("fake-claude");
        let transcript_dir = directory.path().join("transcripts");
        std::fs::create_dir(&transcript_dir).unwrap();
        std::fs::write(transcript_dir.join("session.jsonl"), b"must not be opened").unwrap();
        std::fs::set_permissions(&transcript_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{0}.partial'\nmv '{0}.partial' '{0}'\nprintf '%s\\n' '{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"req_0\",\"response\":{{\"commands\":[],\"agents\":[],\"output_style\":\"default\",\"available_output_styles\":[],\"models\":[],\"account\":{{}}}}}}}}'\nwhile IFS= read -r line; do :; done\n",
                argv.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let agent_id = Uuid::new_v4();
        let req = CreateAgentRequest {
            agent_id,
            host_id: None,
            name: None,
            agent_type: AgentType::Claude {
                driver: ClaudeDriver::Sdk,
            },
            working_dir: directory.path().to_path_buf(),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        };
        let mut backend = ClaudeSdkBackend::new(&req, mcp_launch_route_for_tests(Uuid::new_v4()));
        backend.command = script.to_string_lossy().into_owned();
        let (event_tx, _event_rx) = mpsc::channel(8);
        let ingest = backend.start(&event_tx).unwrap();
        let mut rows = backend.log.subscribe().await.unwrap();
        let ready = tokio::time::timeout(START_DEADLINE, rows.read())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ready.payload["type"], "amux.claude_sdk.ready");
        assert_eq!(ready.payload["session_id"], agent_id.to_string());

        let arguments = launch_arguments(&argv).await;
        let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--session-id", agent_id.to_string().as_str()])
        );
        assert!(arguments.contains(&"--input-format"));
        assert!(arguments.contains(&"stream-json"));

        backend.stop(StopPolicy::Interrupt).await;
        let _ = ingest.await;
        std::fs::set_permissions(&transcript_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            std::fs::read(transcript_dir.join("session.jsonl")).unwrap(),
            b"must not be opened"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sdk_suspend_restart_round_trip_resumes_by_session_id_and_orders_gap() {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("state.yaml");
        let argv = directory.path().join("resume-argv.txt");
        let script = directory.path().join("fake-claude");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{0}.partial'\nmv '{0}.partial' '{0}'\nprintf '%s\\n' '{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"req_0\",\"response\":{{\"commands\":[],\"agents\":[],\"output_style\":\"default\",\"available_output_styles\":[],\"models\":[],\"account\":{{}}}}}}}}'\nwhile IFS= read -r line; do :; done\n",
                argv.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let agent_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let req = CreateAgentRequest {
            agent_id,
            host_id: None,
            name: Some("resumed-sdk".to_string()),
            agent_type: AgentType::Claude {
                driver: ClaudeDriver::Sdk,
            },
            working_dir: directory.path().to_path_buf(),
            terminal_size: None,
            args: vec!["--model".to_string(), "haiku".to_string()],
            parent: None,
            initial_prompt: None,
        };
        let initial = ClaudeSdkBackend::new(&req, mcp_launch_route_for_tests(Uuid::new_v4()));
        initial
            .runtime
            .lock()
            .expect("Claude SDK runtime poisoned")
            .session_id = Some(session_id);
        let suspended = initial.suspended_state().unwrap();
        assert!(matches!(
            &suspended,
            SuspendedAgent::Claude {
                driver: ClaudeDriver::Sdk,
                agent_id: persisted_agent,
                session_id: persisted_session,
                ..
            } if *persisted_agent == agent_id && *persisted_session == session_id
        ));

        crate::suspend::save_suspended(
            &state_path,
            &crate::suspend::SuspendedServerState {
                agents: vec![suspended],
            },
        )
        .unwrap();
        let mut loaded = crate::suspend::load_suspended(&state_path).unwrap().agents;
        let SuspendedAgent::Claude {
            driver,
            agent_id: restored_agent_id,
            name,
            name_source,
            working_dir,
            terminal_size,
            args,
            session_id: restored_session_id,
            created_at: restored_created_at,
            parent,
            working_on: _,
        } = loaded.pop().unwrap()
        else {
            panic!("expected persisted Claude SDK agent");
        };
        assert_eq!(driver, ClaudeDriver::Sdk);
        assert_eq!(restored_agent_id, agent_id);
        assert_eq!(restored_session_id, session_id);

        let resumed_req = CreateAgentRequest {
            agent_id: restored_agent_id,
            host_id: None,
            name,
            agent_type: AgentType::Claude { driver },
            working_dir,
            terminal_size,
            args,
            parent,
            initial_prompt: None,
        };
        let mut resumed = ClaudeSdkBackend::from_suspended(
            &resumed_req,
            name_source.into(),
            restored_session_id,
            restored_created_at,
            mcp_launch_route_for_tests(Uuid::new_v4()),
        );
        resumed.command = script.to_string_lossy().into_owned();
        let mut rows = resumed.log.subscribe().await.unwrap();
        let (event_tx, _event_rx) = mpsc::channel(8);
        let ingest = resumed.start(&event_tx).unwrap();

        let gap = tokio::time::timeout(START_DEADLINE, rows.read())
            .await
            .unwrap()
            .unwrap();
        let ready = tokio::time::timeout(START_DEADLINE, rows.read())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(gap.payload["type"], "amux.claude_sdk.gap");
        assert_eq!(
            gap.payload["resumed_session_id"],
            restored_session_id.to_string()
        );
        assert_eq!(ready.payload["type"], "amux.claude_sdk.ready");
        assert_eq!(ready.payload["session_id"], restored_session_id.to_string());
        assert_eq!(ready.payload["resumed"], true);

        let arguments = launch_arguments(&argv).await;
        let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--resume", restored_session_id.to_string().as_str()])
        );
        assert!(!arguments.contains(&"--session-id"));

        resumed.stop(StopPolicy::Interrupt).await;
        let _ = ingest.await;
    }
}

#[cfg(test)]
#[path = "sdk_session_tests.rs"]
mod session_tests;

#[cfg(test)]
#[path = "sdk_prompt_tests.rs"]
mod prompt_tests;

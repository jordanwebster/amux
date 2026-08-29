use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::pin::Pin;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::process::{Child, ChildStderr};
use tokio::sync::{Mutex, mpsc, watch};

use crate::sdk::abort::{Shutdown, ShutdownReason};
use crate::sdk::control::{HookMatcherConfig, InitializeRequestBody};
use crate::sdk::dispatch::{self, QueryInner, WriteCommand};
use crate::sdk::error::Error;
use crate::sdk::options::{QueryOptions, SkillsConfig, SystemPrompt};
use crate::sdk::process::CliProcess;
use crate::sdk::session::SdkEvent;
use crate::sdk::types::{MessageContent, MessageParam, Role};

/// A user-role input message. The owning [`Session`](crate::sdk::Session) supplies its session ID on
/// the wire, so callers cannot accidentally route a streamed message to a
/// different process.
#[derive(Debug, Clone)]
pub struct UserMessage {
    pub message: MessageParam,
    pub parent_tool_use_id: Option<String>,
}

impl UserMessage {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            message: MessageParam {
                role: Role::User,
                content: MessageContent::Text(text.into()),
                extensions: serde_json::Map::new(),
            },
            parent_tool_use_id: None,
        }
    }

    pub fn new(message: MessageParam, parent_tool_use_id: Option<String>) -> Self {
        Self {
            message,
            parent_tool_use_id,
        }
    }
}

#[derive(Serialize)]
struct WireUserMessage<'a> {
    #[serde(rename = "type")]
    wire_type: &'static str,
    session_id: &'a str,
    message: &'a MessageParam,
    parent_tool_use_id: &'a Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    Exited,
    Closed,
    Aborted,
    Dropped,
    TransportFailed,
}

/// Final process information retained after the message stream ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessExit {
    pub success: bool,
    pub code: Option<i32>,
    pub stderr: String,
    pub termination: Termination,
}

impl ProcessExit {
    fn virtual_success(termination: Termination) -> Self {
        Self {
            success: true,
            code: Some(0),
            stderr: String::new(),
            termination,
        }
    }

    fn from_status(status: ExitStatus, stderr: String, termination: Termination) -> Self {
        Self {
            success: status.success(),
            code: status.code(),
            stderr,
            termination,
        }
    }

    fn status_label(&self) -> String {
        self.code
            .map(|code| format!("code {code}"))
            .unwrap_or_else(|| "terminated by signal".to_owned())
    }
}

#[derive(Default)]
pub(crate) struct QueryRuntimeConfig {
    initialize_request: InitializeRequestBody,
    sdk_mcp_servers: HashMap<String, crate::sdk::mcp::SdkMcpServer>,
    hook_callback_ids: HashSet<String>,
}

impl From<QueryOptions> for QueryRuntimeConfig {
    fn from(options: QueryOptions) -> Self {
        let QueryOptions {
            supported_dialog_kinds,
            per_task_stop_affordance,
            hook_subscriptions,
            agents,
            output_format,
            system_prompt,
            plan_mode_instructions,
            tool_aliases,
            title,
            skills,
            prompt_suggestions,
            agent_progress_summaries,
            forward_subagent_text,
            mcp_servers,
            ..
        } = options;

        let sdk_mcp_servers = mcp_servers
            .into_iter()
            .filter_map(|(name, config)| config.sdk_server().cloned().map(|server| (name, server)))
            .collect::<HashMap<_, _>>();
        let mut sdk_mcp_server_names = sdk_mcp_servers.keys().cloned().collect::<Vec<_>>();
        sdk_mcp_server_names.sort();

        let (system_prompt, append_system_prompt, exclude_dynamic_sections) = match system_prompt {
            Some(SystemPrompt::Custom(prompt)) => (Some(vec![prompt]), None, None),
            Some(SystemPrompt::Blocks(blocks)) => (Some(blocks), None, None),
            Some(SystemPrompt::Preset {
                append,
                exclude_dynamic_sections,
                ..
            }) => (None, append, Some(exclude_dynamic_sections)),
            None => (None, None, None),
        };
        let skills = skills.and_then(|skills| match skills {
            SkillsConfig::All => None,
            SkillsConfig::Selected(skills) => Some(skills),
        });

        let mut hook_callback_ids = HashSet::new();
        let mut wire_hooks = BTreeMap::new();
        // Callback ids are handed out in the order the events are walked, and
        // they travel on the wire. Walking a HashMap would make the initialize
        // request differ between runs configured identically, which is both a
        // surprise to anyone diffing two sessions and a recording that cannot
        // be replayed.
        let mut hook_subscriptions = hook_subscriptions;
        hook_subscriptions.sort_by_key(|subscription| subscription.event.wire_name());
        for (next_callback_id, subscription) in hook_subscriptions.into_iter().enumerate() {
            let callback_id = format!("hook_{next_callback_id}");
            hook_callback_ids.insert(callback_id.clone());
            wire_hooks
                .entry(subscription.event.wire_name().to_owned())
                .or_insert_with(Vec::new)
                .push(HookMatcherConfig {
                    matcher: subscription.matcher,
                    hook_callback_ids: vec![callback_id],
                    timeout: None,
                });
        }

        Self {
            initialize_request: InitializeRequestBody {
                subtype: "initialize",
                sdk_mcp_servers: (!sdk_mcp_server_names.is_empty()).then_some(sdk_mcp_server_names),
                hooks: (!wire_hooks.is_empty()).then_some(wire_hooks),
                json_schema: output_format.map(|format| format.schema),
                system_prompt,
                append_system_prompt,
                plan_mode_instructions,
                tool_aliases: (!tool_aliases.is_empty()).then_some(tool_aliases),
                exclude_dynamic_sections,
                agents: (!agents.is_empty()).then_some(agents.into_iter().collect()),
                title,
                skills,
                prompt_suggestions: prompt_suggestions.then_some(true),
                agent_progress_summaries,
                forward_subagent_text,
                supported_dialog_kinds: (!supported_dialog_kinds.is_empty())
                    .then_some(supported_dialog_kinds),
                per_task_stop_affordance,
            },
            sdk_mcp_servers,
            hook_callback_ids,
        }
    }
}

/// The sole owner of a Claude subprocess and its ordered, fallible output.
pub(crate) struct Query {
    pub(crate) inner: Arc<QueryInner>,
    output_rx: mpsc::Receiver<Result<SdkEvent, Error>>,
    pub(crate) shutdown: Arc<Shutdown>,
    pub(crate) exit_rx: watch::Receiver<Option<ProcessExit>>,
}

/// An initialized query that has not yet received its first prompt.
///
/// A warm query is single-use. Dropping it terminates its owned process; use
/// [`WarmQuery::close`] when the final process status matters.
pub(crate) struct WarmQuery {
    query: Option<Query>,
}

impl fmt::Debug for WarmQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WarmQuery")
            .field(
                "session_id",
                &self
                    .query
                    .as_ref()
                    .map(|query| query.inner.session_id.as_str()),
            )
            .finish_non_exhaustive()
    }
}

impl WarmQuery {
    /// Prepare a warm query around caller-owned stream-JSON I/O.
    ///
    /// This is also the public custom-spawn equivalent: the caller creates and
    /// owns the process or remote transport, while `WarmQuery` owns protocol
    /// initialization, stdin closure, reader cancellation, and message/control
    /// correlation. Its [`ProcessExit`] is virtual; the caller remains
    /// responsible for waiting for or terminating the external process.
    pub async fn from_io(
        options: QueryOptions,
        reader: impl AsyncBufRead + Unpin + Send + 'static,
        writer: impl AsyncWrite + Unpin + Send + 'static,
        initialize_timeout: Duration,
    ) -> Result<Self, Error> {
        options.validate()?;
        let session_id = query_session_id(&options);
        let runtime = QueryRuntimeConfig::from(options);
        let (query, supervisor) = Query::with_io(session_id, reader, writer, runtime);
        spawn_virtual_supervisor(supervisor);
        prepare_warm_query(query, initialize_timeout).await
    }

    pub(crate) fn into_query(mut self) -> Query {
        self.query.take().expect("warm query already consumed")
    }
}

struct SupervisorHandles {
    reader_task: tokio::task::JoinHandle<()>,
    writer_task: tokio::task::JoinHandle<()>,
    shutdown: Arc<Shutdown>,
    stdin_tx: mpsc::UnboundedSender<WriteCommand>,
    output_tx: mpsc::Sender<Result<SdkEvent, Error>>,
    exit_tx: watch::Sender<Option<ProcessExit>>,
}

impl fmt::Debug for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Query")
            .field("session_id", &self.inner.session_id)
            .field("termination", &self.shutdown.reason())
            .finish_non_exhaustive()
    }
}

impl Query {
    pub(crate) async fn warm_from_process(
        options: QueryOptions,
        process: CliProcess,
        initialize_timeout: Duration,
    ) -> Result<WarmQuery, Error> {
        let session_id = query_session_id(&options);
        let runtime = QueryRuntimeConfig::from(options);
        let (child, stdin, stdout, stderr) = process.into_parts();
        let (query, supervisor) = Self::with_io(session_id, stdout, stdin, runtime);
        spawn_process_supervisor(child, stderr, supervisor);
        prepare_warm_query(query, initialize_timeout).await
    }

    fn with_io(
        session_id: String,
        reader: impl AsyncBufRead + Unpin + Send + 'static,
        writer: impl AsyncWrite + Unpin + Send + 'static,
        runtime: QueryRuntimeConfig,
    ) -> (Self, SupervisorHandles) {
        let shutdown = Shutdown::new();
        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel();
        let (output_tx, output_rx) = mpsc::channel(256);
        let (exit_tx, exit_rx) = watch::channel(None);
        let inner = Arc::new(QueryInner {
            session_id,
            stdin_tx,
            pending_controls: Mutex::new(HashMap::new()),
            init_result: std::sync::OnceLock::new(),
            request_counter: AtomicU64::new(0),
            initialize_request: serde_json::to_value(runtime.initialize_request)
                .unwrap_or_else(|_| serde_json::json!({ "subtype": "initialize" })),
            pending_incoming: Mutex::new(HashMap::new()),
            hook_callback_ids: runtime.hook_callback_ids,
            sdk_mcp_servers: std::sync::RwLock::new(runtime.sdk_mcp_servers),
        });
        let reader_task =
            dispatch::spawn_reader_task(reader, output_tx.clone(), inner.clone(), shutdown.clone());
        let writer_task =
            dispatch::spawn_writer_task(writer, stdin_rx, shutdown.clone(), output_tx.clone());
        (
            Self {
                inner: inner.clone(),
                output_rx,
                shutdown: shutdown.clone(),
                exit_rx,
            },
            SupervisorHandles {
                reader_task,
                writer_task,
                shutdown,
                stdin_tx: inner.stdin_tx.clone(),
                output_tx,
                exit_tx,
            },
        )
    }

    async fn initialize(&self) -> Result<(), Error> {
        if self.inner.init_result.get().is_some() {
            return Ok(());
        }
        let response = self
            .inner
            .send_control(self.inner.initialize_request.clone())
            .await?;
        let initialization = serde_json::from_value(response.response)
            .map_err(|error| Error::Control(format!("failed to parse init response: {error}")))?;
        let _ = self.inner.init_result.set(initialization);
        Ok(())
    }
}

async fn prepare_warm_query(
    query: Query,
    initialize_timeout: Duration,
) -> Result<WarmQuery, Error> {
    match tokio::time::timeout(initialize_timeout, query.initialize()).await {
        Ok(Ok(())) => Ok(WarmQuery { query: Some(query) }),
        Ok(Err(error)) => {
            let exit = close_query(query).await;
            if !exit.success {
                Err(Error::ProcessExit {
                    status: exit.status_label(),
                    stderr: exit.stderr,
                })
            } else {
                Err(error)
            }
        }
        Err(_) => {
            let _ = close_query(query).await;
            Err(Error::Control(format!(
                "subprocess initialization did not complete within {} ms",
                initialize_timeout.as_millis()
            )))
        }
    }
}

pub(crate) fn query_session_id(options: &QueryOptions) -> String {
    options
        .session_id
        .clone()
        .or_else(|| {
            (!options.fork_session)
                .then(|| options.resume.clone())
                .flatten()
        })
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

impl Stream for Query {
    type Item = Result<SdkEvent, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.output_rx).poll_recv(cx)
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        self.shutdown.request(ShutdownReason::Dropped);
    }
}

pub(crate) async fn send_user_message(
    inner: &QueryInner,
    message: &UserMessage,
) -> Result<(), Error> {
    let wire = WireUserMessage {
        wire_type: "user",
        session_id: &inner.session_id,
        message: &message.message,
        parent_tool_use_id: &message.parent_tool_use_id,
    };
    let bytes = serde_json::to_vec(&wire)?;
    inner.write(bytes).await
}

pub(crate) async fn wait_for_exit(
    exit_rx: &mut watch::Receiver<Option<ProcessExit>>,
) -> ProcessExit {
    loop {
        if let Some(exit) = exit_rx.borrow().clone() {
            return exit;
        }
        if exit_rx.changed().await.is_err() {
            return ProcessExit::virtual_success(Termination::TransportFailed);
        }
    }
}

async fn close_query(mut query: Query) -> ProcessExit {
    query.output_rx.close();
    query.shutdown.request(ShutdownReason::Closed);
    wait_for_exit(&mut query.exit_rx).await
}

fn termination(reason: ShutdownReason) -> Termination {
    match reason {
        ShutdownReason::Closed => Termination::Closed,
        ShutdownReason::Aborted => Termination::Aborted,
        ShutdownReason::Dropped => Termination::Dropped,
        ShutdownReason::TransportFailed => Termination::TransportFailed,
        ShutdownReason::Running => Termination::Exited,
    }
}

fn spawn_process_supervisor(
    mut child: Child,
    mut stderr: tokio::io::BufReader<ChildStderr>,
    handles: SupervisorHandles,
) {
    tokio::spawn(async move {
        let SupervisorHandles {
            reader_task,
            writer_task,
            shutdown,
            stdin_tx,
            output_tx,
            exit_tx,
        } = handles;
        let stderr_task = tokio::spawn(async move {
            let mut stderr_text = String::new();
            let mut line = String::new();
            let result = loop {
                match tokio::io::AsyncBufReadExt::read_line(&mut stderr, &mut line).await {
                    Ok(0) => break Ok(()),
                    Ok(_) => {
                        stderr_text.push_str(&line);
                        line.clear();
                    }
                    Err(error) => break Err(error),
                }
            };
            (result, stderr_text)
        });
        let token = shutdown.token();
        let status = tokio::select! {
            biased;
            _ = token.cancelled() => {
                let reason = shutdown.reason();
                let _ = stdin_tx.send(WriteCommand::Close);
                if matches!(reason, ShutdownReason::Closed | ShutdownReason::Dropped) {
                    match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                        Ok(status) => status,
                        Err(_) => {
                            let _ = child.kill().await;
                            child.wait().await
                        }
                    }
                } else {
                    let _ = child.kill().await;
                    child.wait().await
                }
            }
            status = child.wait() => status,
        };
        let _ = reader_task.await;
        let _ = stdin_tx.send(WriteCommand::Close);
        let _ = writer_task.await;
        let (stderr_result, stderr) = stderr_task
            .await
            .unwrap_or_else(|error| (Err(std::io::Error::other(error.to_string())), String::new()));
        let reason = shutdown.reason();
        let exit = match status {
            Ok(status) => ProcessExit::from_status(status, stderr, termination(reason)),
            Err(error) => {
                let message = format!("failed waiting for Claude process: {error}");
                let _ = output_tx.send(Err(Error::Process(message))).await;
                ProcessExit {
                    success: false,
                    code: None,
                    stderr,
                    termination: termination(reason),
                }
            }
        };
        if let Err(error) = stderr_result {
            let _ = output_tx
                .send(Err(Error::Stream(format!(
                    "I/O error reading Claude stderr: {error}"
                ))))
                .await;
        }
        match reason {
            ShutdownReason::Running if !exit.success => {
                let _ = output_tx
                    .send(Err(Error::ProcessExit {
                        status: exit.status_label(),
                        stderr: exit.stderr.clone(),
                    }))
                    .await;
            }
            ShutdownReason::Aborted => {
                let _ = output_tx.send(Err(Error::Aborted)).await;
            }
            _ => {}
        }
        let _ = output_tx.send(Ok(SdkEvent::Exited(exit.clone()))).await;
        let _ = exit_tx.send(Some(exit));
    });
}

fn spawn_virtual_supervisor(handles: SupervisorHandles) {
    tokio::spawn(async move {
        let SupervisorHandles {
            reader_task,
            writer_task,
            shutdown,
            stdin_tx,
            output_tx,
            exit_tx,
        } = handles;
        let _ = reader_task.await;
        let _ = stdin_tx.send(WriteCommand::Close);
        let _ = writer_task.await;
        let reason = shutdown.reason();
        if reason == ShutdownReason::Aborted {
            let _ = output_tx.send(Err(Error::Aborted)).await;
        }
        let exit = ProcessExit::virtual_success(termination(reason));
        let _ = output_tx.send(Ok(SdkEvent::Exited(exit.clone()))).await;
        let _ = exit_tx.send(Some(exit));
    });
}

//! One Claude PTY event stream paired with its control handle.

use std::collections::HashMap;
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::hooks::{HookPayload, HookReceiver};
use crate::launch::{Launch, pty_spawn_args};
use crate::transcript::{TranscriptRow, TranscriptTailer};
use crate::version::{ClaudeVersion, VersionError, probe_version};

const CHANNEL_CAPACITY: usize = 256;
const MAX_DELAY_MS: u32 = 5_000;
const SOCKET_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(2);

pub type EventStream = mpsc::Receiver<PtyEvent>;
type ExitFuture = Pin<Box<dyn Future<Output = pty_host::ExitStatus> + Send>>;
type Relink = Arc<dyn Fn(PathBuf) + Send + Sync>;

pub struct Sources {
    pub pty: PtySource,
    pub hooks: HookSource,
    pub transcript: TranscriptSource,
    pub version: ClaudeVersion,
}

pub struct PtySource {
    pub output: mpsc::Receiver<Bytes>,
    pub writer: Box<dyn AsyncWrite + Unpin + Send>,
    pub handle: Option<pty_host::PtyHandle>,
    pub exit: ExitFuture,
}

pub struct HookSource {
    pub payloads: mpsc::Receiver<HookPayload>,
    receiver: Option<HookReceiver>,
}

impl HookSource {
    pub fn channel(capacity: usize) -> (Self, mpsc::Sender<HookPayload>) {
        let (tx, payloads) = mpsc::channel(capacity);
        (
            Self {
                payloads,
                receiver: None,
            },
            tx,
        )
    }

    fn from_receiver(receiver: HookReceiver) -> Self {
        let payloads = receiver.payloads();
        Self {
            payloads,
            receiver: Some(receiver),
        }
    }
}

pub struct TranscriptSource {
    pub rows: mpsc::Receiver<(PathBuf, TranscriptRow)>,
    pub relink: Relink,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TranscriptSource {
    pub fn channel(
        capacity: usize,
    ) -> (
        Self,
        mpsc::Sender<(PathBuf, TranscriptRow)>,
        mpsc::UnboundedReceiver<PathBuf>,
    ) {
        let (row_tx, rows) = mpsc::channel(capacity);
        let (path_tx, path_rx) = mpsc::unbounded_channel();
        let relink = Arc::new(move |path| {
            let _ = path_tx.send(path);
        });
        (
            Self {
                rows,
                relink,
                task: None,
            },
            row_tx,
            path_rx,
        )
    }

    pub fn live() -> Self {
        let (row_tx, rows) = mpsc::channel(CHANNEL_CAPACITY);
        let (path_tx, mut path_rx) = mpsc::unbounded_channel::<PathBuf>();
        let relink = Arc::new(move |path| {
            let _ = path_tx.send(path);
        });
        let task = tokio::spawn(async move {
            let mut current: Option<(PathBuf, TranscriptTailer, mpsc::Receiver<TranscriptRow>)> =
                None;
            loop {
                match current.as_mut() {
                    Some((path, _, source_rows)) => tokio::select! {
                        next = path_rx.recv() => {
                            let Some(next) = next else { break; };
                            if *path != next {
                                let tailer = TranscriptTailer::follow(next.clone());
                                let source_rows = tailer.rows();
                                current = Some((next, tailer, source_rows));
                            }
                        }
                        row = source_rows.recv() => {
                            let Some(row) = row else { current = None; continue; };
                            if row_tx.send((path.clone(), row)).await.is_err() { break; }
                        }
                    },
                    None => {
                        let Some(path) = path_rx.recv().await else {
                            break;
                        };
                        let tailer = TranscriptTailer::follow(path.clone());
                        let source_rows = tailer.rows();
                        current = Some((path, tailer, source_rows));
                    }
                }
            }
        });
        Self {
            rows,
            relink,
            task: Some(task),
        }
    }

    fn recorded(rows: mpsc::Receiver<(PathBuf, TranscriptRow)>) -> Self {
        Self {
            rows,
            relink: Arc::new(|_| {}),
            task: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PtyEvent {
    Ready {
        version: ClaudeVersion,
    },
    Transcript {
        path: PathBuf,
        row: TranscriptRow,
    },
    Hook(HookPayload),
    Ask(AskFacts),
    Relink {
        transcript_path: PathBuf,
        reason: RelinkReason,
    },
    Keymap(super::keymap::Resolved),
    InputResult(InputResult),
    Delivery(DeliveryOutcome),
    Exited(pty_host::ExitStatus),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskFacts {
    pub id: AskId,
    pub kind: AskKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AskId(pub String);

impl std::fmt::Display for AskId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AskKind {
    Permission {
        tool_name: String,
        suggestions: usize,
        is_plan: bool,
    },
    Question {
        questions: Vec<QuestionFact>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionFact {
    pub options: usize,
    pub multi_select: bool,
}

/// Semantic input accepted by a Claude PTY session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case", deny_unknown_fields)]
pub enum Intent {
    Prompt { text: String },
    Interrupt,
    CyclePermissionMode,
    Answer { ask_id: AskId, answer: AskAnswer },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case", deny_unknown_fields)]
pub enum AskAnswer {
    Permission(PermissionAnswer),
    Plan(PlanAnswer),
    Question(QuestionResponse),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "permission", rename_all = "snake_case", deny_unknown_fields)]
pub enum PermissionAnswer {
    AllowOnce,
    AllowScoped { suggestion: usize },
    Deny { feedback: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "plan", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanAnswer {
    ApproveAuto,
    ApproveManual,
    RequestChanges { feedback: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionResponse {
    pub answers: Vec<QuestionAnswer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionAnswer {
    pub selected: Vec<usize>,
    pub other: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelinkReason {
    Initial,
    Compact,
    Clear,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub enum PtyInput {
    Bytes(Vec<u8>),
    Delay(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputResult {
    pub intent: Intent,
    pub keymap: super::keymap::KeymapId,
    pub basis: super::keymap::Basis,
    pub program: super::keymap::ProgramName,
    pub bytes_written: usize,
}

#[derive(Debug, Clone)]
pub enum Carrier {
    Pty,
    Socket {
        path: PathBuf,
        token: String,
        confirmation: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Pty,
    Socket,
    PtyFallback { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("unknown Claude ask '{0}'")]
    UnknownAsk(AskId),
    #[error("unverified keymap shape for {program:?}: {reason}")]
    UnverifiedShape {
        program: super::keymap::ProgramName,
        reason: String,
    },
    #[error("unsafe PTY input text: {reason}")]
    UnsafeText { reason: String },
    #[error("answer does not fit the ask: {detail}")]
    AnswerMismatchesAsk { detail: String },
    #[error("no keymap for Claude {version} can answer {program:?}")]
    NoKeymap {
        version: ClaudeVersion,
        program: super::keymap::ProgramName,
    },
    #[error("PTY input failed: {0}")]
    Pty(#[from] pty_host::PtyError),
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error("Claude delivery failed: {0}")]
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error(transparent)]
    Version(#[from] VersionError),
    #[error("could not bind Claude hook receiver: {0}")]
    Hook(#[source] std::io::Error),
    #[error(transparent)]
    Pty(#[from] pty_host::PtyError),
    #[error("recording omitted the `{0}` transport")]
    MissingTransport(&'static str),
}

pub struct Session {
    pub events: EventStream,
    pub control: Control,
}

struct ActiveKeymap {
    resolved: super::keymap::Resolved,
    keymap: super::keymap::Keymap,
}

struct SemanticState {
    version: ClaudeVersion,
    active: Option<ActiveKeymap>,
    asks: HashMap<AskId, AskKind>,
}

#[derive(Clone)]
pub struct Control {
    writer: Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    terminal_output: Arc<Mutex<Option<mpsc::Receiver<Bytes>>>>,
    terminal: Option<pty_host::PtyHandle>,
    events: mpsc::Sender<PtyEvent>,
    semantic: Arc<Mutex<SemanticState>>,
    send_lock: Arc<tokio::sync::Mutex<()>>,
    confirmations: broadcast::Sender<Value>,
    exit: watch::Receiver<Option<pty_host::ExitStatus>>,
}

impl Control {
    pub async fn send(&self, intent: Intent) -> Result<InputResult, InputError> {
        let _send = self.send_lock.lock().await;
        let (ask, resolved, keymap, program) = {
            let semantic = self.semantic.lock().expect("semantic state mutex poisoned");
            let ask = match &intent {
                Intent::Answer { ask_id, .. } => Some(
                    semantic
                        .asks
                        .get(ask_id)
                        .cloned()
                        .ok_or_else(|| InputError::UnknownAsk(ask_id.clone()))?,
                ),
                _ => None,
            };
            let program = super::keymap::program_for(&intent, ask.as_ref())?;
            let active = semantic
                .active
                .as_ref()
                .ok_or_else(|| InputError::NoKeymap {
                    version: semantic.version.clone(),
                    program,
                })?;
            (ask, active.resolved.clone(), active.keymap.clone(), program)
        };
        let prompt = match &intent {
            Intent::Prompt { text } => Some(text.as_str()),
            _ => None,
        };
        let answer = match &intent {
            Intent::Answer { answer, .. } => Some(answer),
            _ => None,
        };
        let steps = super::keymap::encode(
            &keymap,
            &resolved,
            program,
            &super::keymap::Environment {
                ask: ask.as_ref(),
                answer,
                prompt,
            },
        )?;
        let bytes_written = self.write_key_steps(&steps).await?;
        if let Intent::Answer { ask_id, .. } = &intent {
            self.semantic
                .lock()
                .expect("semantic state mutex poisoned")
                .asks
                .remove(ask_id);
        }
        let result = InputResult {
            intent,
            keymap: resolved.keymap,
            basis: resolved.basis,
            program,
            bytes_written,
        };
        let _ = self
            .events
            .send(PtyEvent::InputResult(result.clone()))
            .await;
        Ok(result)
    }

    /// Writes daemon-owned terminal or delivery bytes without representing
    /// them as semantic user input.
    pub async fn send_program(&self, program: Vec<PtyInput>) -> Result<usize, InputError> {
        let mut writer = self.writer.lock().await;
        let mut bytes_written = 0;
        for step in &program {
            match step {
                PtyInput::Bytes(bytes) => {
                    writer
                        .write_all(bytes)
                        .await
                        .map_err(pty_host::PtyError::Io)?;
                    writer.flush().await.map_err(pty_host::PtyError::Io)?;
                    bytes_written += bytes.len();
                }
                PtyInput::Delay(ms) => {
                    tokio::time::sleep(Duration::from_millis(u64::from((*ms).min(MAX_DELAY_MS))))
                        .await
                }
            }
        }
        Ok(bytes_written)
    }

    async fn write_key_steps(&self, steps: &[super::keymap::KeyStep]) -> Result<usize, InputError> {
        let mut writer = self.writer.lock().await;
        let mut bytes_written = 0;
        for step in steps {
            match step {
                super::keymap::KeyStep::Write(bytes) => {
                    writer
                        .write_all(bytes)
                        .await
                        .map_err(pty_host::PtyError::Io)?;
                    writer.flush().await.map_err(pty_host::PtyError::Io)?;
                    bytes_written += bytes.len();
                }
                super::keymap::KeyStep::Delay(delay) => {
                    tokio::time::sleep(
                        (*delay).min(Duration::from_millis(u64::from(MAX_DELAY_MS))),
                    )
                    .await;
                }
            }
        }
        Ok(bytes_written)
    }

    pub fn resize(&self, size: pty_host::PtySize) -> Result<(), pty_host::PtyError> {
        self.terminal
            .as_ref()
            .ok_or_else(unavailable_pty_error)?
            .resize(size)
    }

    pub async fn deliver(
        &self,
        text: &str,
        carrier: Carrier,
    ) -> Result<DeliveryOutcome, DeliveryError> {
        let outcome = match carrier {
            Carrier::Pty => {
                self.send_program(paste_program(text))
                    .await
                    .map_err(|e| DeliveryError::Failed(e.to_string()))?;
                DeliveryOutcome::Pty
            }
            Carrier::Socket {
                path,
                token,
                confirmation,
            } => {
                match self
                    .deliver_socket(text, &path, &token, &confirmation)
                    .await
                {
                    Ok(()) => DeliveryOutcome::Socket,
                    Err(reason) => {
                        self.send_program(paste_program(text))
                            .await
                            .map_err(|e| DeliveryError::Failed(e.to_string()))?;
                        DeliveryOutcome::PtyFallback { reason }
                    }
                }
            }
        };
        let _ = self.events.send(PtyEvent::Delivery(outcome.clone())).await;
        Ok(outcome)
    }

    async fn deliver_socket(
        &self,
        text: &str,
        path: &Path,
        token: &str,
        confirmation: &str,
    ) -> Result<(), String> {
        let mut rows = self.confirmations.subscribe();
        let mut socket = crate::messaging::MessagingSocket::connect(path, token)
            .await
            .map_err(|e| e.to_string())?;
        socket.send(text).await.map_err(|e| e.to_string())?;
        tokio::time::timeout(SOCKET_CONFIRMATION_TIMEOUT, async {
            loop {
                let row = rows.recv().await.map_err(|e| e.to_string())?;
                if row_confirms_delivery(&row, confirmation) {
                    return Ok(());
                }
            }
        })
        .await
        .map_err(|_| "transcript did not confirm socket delivery".to_string())?
    }

    pub async fn stop(mut self, policy: pty_host::Terminate) -> pty_host::ExitStatus {
        if let Some(status) = self.exit.borrow().clone() {
            return status;
        }
        if let Some(handle) = &self.terminal {
            match policy {
                pty_host::Terminate::Kill => {
                    let _ = handle.signal_process_group(pty_host::ProcessGroupSignal::Kill);
                }
                pty_host::Terminate::Graceful { grace } => {
                    let _ = handle.signal_process_group(pty_host::ProcessGroupSignal::Terminate);
                    if let Ok(status) = tokio::time::timeout(grace, wait_exit(&mut self.exit)).await
                    {
                        return status;
                    }
                    let _ = handle.signal_process_group(pty_host::ProcessGroupSignal::Kill);
                }
            }
        }
        wait_exit(&mut self.exit).await
    }

    pub fn terminal(&self) -> Option<pty_host::PtyHandle> {
        self.terminal.clone()
    }

    pub fn terminal_output(&self) -> Option<mpsc::Receiver<Bytes>> {
        self.terminal_output
            .lock()
            .expect("terminal output mutex poisoned")
            .take()
    }
}

pub async fn spawn(
    launch: &Launch,
    keymaps: &super::keymap::KeymapSources,
    size: pty_host::PtySize,
) -> Result<Session, SpawnError> {
    let version = probe_version(&launch.binary).await?;
    spawn_with_version(launch, keymaps, size, version)
}

/// Spawn a live session when the host has already completed its shared
/// version probe.
pub fn spawn_with_version(
    launch: &Launch,
    keymaps: &super::keymap::KeymapSources,
    size: pty_host::PtySize,
    version: ClaudeVersion,
) -> Result<Session, SpawnError> {
    let hook_dir = std::env::temp_dir()
        .join("amux-claude-hooks")
        .join(launch.session_id.to_string());
    let receiver = HookReceiver::bind_sync(&hook_dir).map_err(SpawnError::Hook)?;
    let hook_path = receiver.path.clone();
    let process = pty_host::spawn(pty_host::PtySpawn {
        command: launch.binary.clone(),
        args: pty_spawn_args(launch),
        cwd: launch.cwd.clone(),
        env: vec![(
            OsString::from("CLAUDE_HOOK_SOCKET"),
            hook_path.into_os_string(),
        )],
        env_remove: launch.env_scrub.iter().map(OsString::from).collect(),
        size,
    })?;
    let output = process.handle.output();
    let handle = process.handle.clone();
    let writer = writer_for_handle(handle.clone());
    let mut exit = process.exit;
    Ok(from_sources(
        Sources {
            pty: PtySource {
                output,
                writer,
                handle: Some(handle),
                exit: Box::pin(async move { exit.wait().await }),
            },
            hooks: HookSource::from_receiver(receiver),
            transcript: TranscriptSource::live(),
            version,
        },
        keymaps,
    ))
}

pub fn from_sources(sources: Sources, keymaps: &super::keymap::KeymapSources) -> Session {
    let Sources {
        pty,
        hooks,
        transcript,
        version,
    } = sources;
    let PtySource {
        output,
        writer,
        handle,
        exit,
    } = pty;
    let HookSource {
        mut payloads,
        receiver,
    } = hooks;
    let TranscriptSource {
        mut rows,
        relink,
        task,
    } = transcript;
    let (event_tx, events) = mpsc::channel(CHANNEL_CAPACITY);
    let (confirmation_tx, _) = broadcast::channel(CHANNEL_CAPACITY);
    let (exit_tx, exit_rx) = watch::channel(None);
    let initial = super::keymap::resolve_session(keymaps, &version).ok();
    let initial_event = initial.as_ref().map(|(resolved, _)| resolved.clone());
    let semantic = Arc::new(Mutex::new(SemanticState {
        version: version.clone(),
        active: initial.map(|(resolved, keymap)| ActiveKeymap { resolved, keymap }),
        asks: HashMap::new(),
    }));

    event_tx
        .try_send(PtyEvent::Ready {
            version: version.clone(),
        })
        .expect("new PTY event channel has capacity for readiness");
    if let Some(resolved) = initial_event {
        event_tx
            .try_send(PtyEvent::Keymap(resolved))
            .expect("new PTY event channel has capacity for its keymap");
    }

    let tx = event_tx.clone();
    let semantic_for_hooks = semantic.clone();
    let keymaps = keymaps.clone();
    tokio::spawn(async move {
        let _receiver = receiver;
        let mut current_path = None;
        while let Some(hook) = payloads.recv().await {
            let path = hook.common().transcript_path.clone();
            if current_path.as_ref() != Some(&path) {
                let reason = relink_reason(&hook, current_path.is_none());
                relink(path.clone());
                current_path = Some(path.clone());
                if tx
                    .send(PtyEvent::Relink {
                        transcript_path: path,
                        reason,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
                let resolved = refresh_keymap(&semantic_for_hooks, &keymaps);
                if let Some(resolved) = resolved
                    && tx.send(PtyEvent::Keymap(resolved)).await.is_err()
                {
                    break;
                }
            }
            let ask = ask_from_hook(&hook);
            if tx.send(PtyEvent::Hook(hook)).await.is_err() {
                break;
            }
            if let Some(ask) = ask {
                semantic_for_hooks
                    .lock()
                    .expect("semantic state mutex poisoned")
                    .asks
                    .insert(ask.id.clone(), ask.kind.clone());
                if tx.send(PtyEvent::Ask(ask)).await.is_err() {
                    break;
                }
            }
        }
    });

    let tx = event_tx.clone();
    let confirmations = confirmation_tx.clone();
    let semantic_for_rows = semantic.clone();
    tokio::spawn(async move {
        let _task = task;
        while let Some((path, row)) = rows.recv().await {
            let _ = confirmations.send(row.as_value().clone());
            let ask = ask_from_transcript(&row);
            if tx.send(PtyEvent::Transcript { path, row }).await.is_err() {
                break;
            }
            if let Some(ask) = ask {
                semantic_for_rows
                    .lock()
                    .expect("semantic state mutex poisoned")
                    .asks
                    .insert(ask.id.clone(), ask.kind.clone());
                if tx.send(PtyEvent::Ask(ask)).await.is_err() {
                    break;
                }
            }
        }
    });

    let tx = event_tx.clone();
    tokio::spawn(async move {
        let status = exit.await;
        exit_tx.send_replace(Some(status.clone()));
        let _ = tx.send(PtyEvent::Exited(status)).await;
    });

    Session {
        events,
        control: Control {
            writer: Arc::new(tokio::sync::Mutex::new(writer)),
            terminal_output: Arc::new(Mutex::new(Some(output))),
            terminal: handle,
            events: event_tx,
            semantic,
            send_lock: Arc::new(tokio::sync::Mutex::new(())),
            confirmations: confirmation_tx,
            exit: exit_rx,
        },
    }
}

pub fn from_recording(
    replay: &mut replay_support::StrictReplay,
    manifest: &replay_support::Manifest,
    keymaps: &super::keymap::KeymapSources,
) -> Result<Session, SpawnError> {
    let pty = replay
        .transports
        .remove("pty")
        .ok_or(SpawnError::MissingTransport("pty"))?;
    let hooks = replay
        .transports
        .remove("hook")
        .ok_or(SpawnError::MissingTransport("hook"))?;
    let transcript = replay
        .transports
        .remove("transcript")
        .ok_or(SpawnError::MissingTransport("transcript"))?;
    let (output_tx, output) = mpsc::channel(CHANNEL_CAPACITY);
    let (exit_tx, exit_rx) = oneshot::channel();
    tokio::spawn(async move {
        pump_bytes(pty.reader, output_tx).await;
        let _ = exit_tx.send(pty_host::ExitStatus::with_exit_code(0));
    });
    let (hook_source, hook_tx) = HookSource::channel(CHANNEL_CAPACITY);
    tokio::spawn(async move {
        pump_hooks(hooks.reader, hook_tx).await;
    });
    let (row_tx, rows) = mpsc::channel(CHANNEL_CAPACITY);
    let fallback = PathBuf::from(format!("recording/{}.jsonl", manifest.spec));
    tokio::spawn(async move {
        pump_transcript(transcript.reader, row_tx, fallback).await;
    });
    Ok(from_sources(
        Sources {
            pty: PtySource {
                output,
                writer: pty.writer,
                handle: None,
                exit: Box::pin(async move {
                    exit_rx
                        .await
                        .unwrap_or_else(|_| pty_host::ExitStatus::with_signal("replay closed"))
                }),
            },
            hooks: hook_source,
            transcript: TranscriptSource::recorded(rows),
            version: ClaudeVersion(manifest.recorded.version.clone()),
        },
        keymaps,
    ))
}

fn refresh_keymap(
    semantic: &Mutex<SemanticState>,
    keymaps: &super::keymap::KeymapSources,
) -> Option<super::keymap::Resolved> {
    let mut semantic = semantic.lock().expect("semantic state mutex poisoned");
    let active = super::keymap::resolve_session(keymaps, &semantic.version).ok();
    let event = active.as_ref().map(|(resolved, _)| resolved.clone());
    semantic.active = active.map(|(resolved, keymap)| ActiveKeymap { resolved, keymap });
    event
}

pub fn paste_program(text: &str) -> Vec<PtyInput> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text: String = text
        .chars()
        .filter_map(|c| match c {
            '\n' => Some('\n'),
            '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let mut paste = b"\x1b[200~".to_vec();
    paste.extend_from_slice(text.as_bytes());
    paste.extend_from_slice(b"\x1b[201~");
    vec![
        PtyInput::Bytes(paste),
        PtyInput::Delay(400),
        PtyInput::Bytes(b"\r".to_vec()),
    ]
}

fn writer_for_handle(handle: pty_host::PtyHandle) -> Box<dyn AsyncWrite + Unpin + Send> {
    let (client, mut bridge) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let mut buffer = [0; 4096];
        loop {
            match bridge.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) if handle.write(&buffer[..read]).await.is_err() => break,
                Ok(_) => {}
            }
        }
    });
    Box::new(client)
}

async fn wait_exit(
    exit: &mut watch::Receiver<Option<pty_host::ExitStatus>>,
) -> pty_host::ExitStatus {
    loop {
        if let Some(status) = exit.borrow().clone() {
            return status;
        }
        if exit.changed().await.is_err() {
            return pty_host::ExitStatus::with_signal("exit monitor closed");
        }
    }
}

fn unavailable_pty_error() -> pty_host::PtyError {
    pty_host::PtyError::Io(std::io::Error::new(
        std::io::ErrorKind::NotConnected,
        "session has no live PTY handle",
    ))
}

fn relink_reason(hook: &HookPayload, initial: bool) -> RelinkReason {
    if initial {
        return RelinkReason::Initial;
    }
    match hook.raw().get("source").and_then(Value::as_str) {
        Some("compact") => RelinkReason::Compact,
        Some("clear") => RelinkReason::Clear,
        Some(other) => RelinkReason::Other(other.to_string()),
        None => RelinkReason::Other("hook transcript path changed".to_string()),
    }
}

fn ask_from_hook(hook: &HookPayload) -> Option<AskFacts> {
    let HookPayload::PermissionRequest {
        common,
        tool_name,
        suggestions,
        ..
    } = hook
    else {
        return None;
    };
    let id = hook
        .raw()
        .get("tool_use_id")
        .or_else(|| hook.raw().get("prompt_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("permission:{}:{tool_name}", common.session_id));
    Some(AskFacts {
        id: AskId(id),
        kind: AskKind::Permission {
            tool_name: tool_name.clone(),
            suggestions: suggestions.len(),
            is_plan: tool_name == "ExitPlanMode",
        },
    })
}

fn ask_from_transcript(row: &TranscriptRow) -> Option<AskFacts> {
    for block in row.as_value().pointer("/message/content")?.as_array()? {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let id = block.get("id").and_then(Value::as_str)?.to_string();
        match block.get("name").and_then(Value::as_str)? {
            "ExitPlanMode" => {
                return Some(AskFacts {
                    id: AskId(id),
                    kind: AskKind::Permission {
                        tool_name: "ExitPlanMode".to_string(),
                        suggestions: 0,
                        is_plan: true,
                    },
                });
            }
            "AskUserQuestion" => {
                let questions = block
                    .pointer("/input/questions")
                    .and_then(Value::as_array)?
                    .iter()
                    .map(|q| QuestionFact {
                        options: q
                            .get("options")
                            .and_then(Value::as_array)
                            .map_or(0, Vec::len),
                        multi_select: q
                            .get("multiSelect")
                            .or_else(|| q.get("multi_select"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                    .collect();
                return Some(AskFacts {
                    id: AskId(id),
                    kind: AskKind::Question { questions },
                });
            }
            _ => {}
        }
    }
    None
}

fn row_confirms_delivery(row: &Value, confirmation: &str) -> bool {
    let enqueued = row.get("type").and_then(Value::as_str) == Some("queue-operation")
        && row.get("operation").and_then(Value::as_str) == Some("enqueue")
        && row
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|content| content.contains(confirmation));
    let peer_user = row.get("type").and_then(Value::as_str) == Some("user")
        && row.pointer("/origin/kind").and_then(Value::as_str) == Some("peer")
        && row
            .pointer("/message/content")
            .and_then(Value::as_str)
            .is_some_and(|content| content.contains(confirmation));
    let queued_command = row.get("type").and_then(Value::as_str) == Some("attachment")
        && row.pointer("/attachment/type").and_then(Value::as_str) == Some("queued_command")
        && row
            .pointer("/attachment/prompt")
            .and_then(Value::as_str)
            .is_some_and(|prompt| prompt.contains(confirmation));
    enqueued || peer_user || queued_command
}

async fn pump_bytes(
    mut reader: Box<dyn tokio::io::AsyncBufRead + Unpin + Send>,
    tx: mpsc::Sender<Bytes>,
) {
    let mut buffer = [0; 4096];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(n) if tx.send(Bytes::copy_from_slice(&buffer[..n])).await.is_err() => break,
            Ok(_) => {}
        }
    }
}

async fn pump_hooks(
    mut reader: Box<dyn tokio::io::AsyncBufRead + Unpin + Send>,
    tx: mpsc::Sender<HookPayload>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if let Ok(payload) = crate::hooks::parse(line.trim().as_bytes())
                    && tx.send(payload).await.is_err()
                {
                    break;
                }
            }
        }
    }
}

async fn pump_transcript(
    mut reader: Box<dyn tokio::io::AsyncBufRead + Unpin + Send>,
    tx: mpsc::Sender<(PathBuf, TranscriptRow)>,
    fallback: PathBuf,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                let path = value
                    .get("path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| fallback.clone());
                let row = value.get("row").cloned().unwrap_or(value);
                if tx.send((path, TranscriptRow::parse(row))).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HookCommon;

    type TestBundle = (
        Sources,
        mpsc::Sender<HookPayload>,
        mpsc::Sender<(PathBuf, TranscriptRow)>,
        mpsc::UnboundedReceiver<PathBuf>,
        tokio::io::DuplexStream,
        oneshot::Sender<pty_host::ExitStatus>,
    );

    fn source_bundle() -> TestBundle {
        let (session_writer, peer_writer) = tokio::io::duplex(4096);
        let (_output_tx, output) = mpsc::channel(4);
        let (hooks, hook_tx) = HookSource::channel(16);
        let (transcript, row_tx, paths) = TranscriptSource::channel(16);
        let (exit_tx, exit_rx) = oneshot::channel();
        (
            Sources {
                pty: PtySource {
                    output,
                    writer: Box::new(session_writer),
                    handle: None,
                    exit: Box::pin(async move {
                        exit_rx.await.unwrap_or_else(|_| {
                            pty_host::ExitStatus::with_signal("test source closed")
                        })
                    }),
                },
                hooks,
                transcript,
                version: "2.1.251".parse().unwrap(),
            },
            hook_tx,
            row_tx,
            paths,
            peer_writer,
            exit_tx,
        )
    }

    fn session_start(path: &str, source: &str) -> HookPayload {
        let raw = serde_json::json!({
            "hook_event_name":"SessionStart",
            "session_id":"00000000-0000-0000-0000-000000000001",
            "transcript_path":path,
            "cwd":"/tmp",
            "source":source,
        });
        HookPayload::SessionStart(HookCommon {
            session_id: uuid::Uuid::from_u128(1),
            transcript_path: PathBuf::from(path),
            cwd: PathBuf::from("/tmp"),
            permission_mode: None,
            messaging: None,
            raw,
        })
    }

    async fn next(events: &mut EventStream) -> PtyEvent {
        tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event stream remains open")
    }

    fn from_test_sources(sources: Sources) -> Session {
        from_sources(sources, &super::super::keymap::KeymapSources::default())
    }

    #[tokio::test]
    async fn sources_emit_ready_compact_clear_relinks_and_exit() {
        let (sources, hooks, _rows, mut paths, _writer, exit) = source_bundle();
        let mut session = from_test_sources(sources);
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Ready { .. }
        ));
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Keymap(super::super::keymap::Resolved {
                basis: super::super::keymap::Basis::InRange,
                ..
            })
        ));

        hooks
            .send(session_start("/tmp/one", "startup"))
            .await
            .unwrap();
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Relink {
                reason: RelinkReason::Initial,
                ..
            }
        ));
        assert_eq!(paths.recv().await.unwrap(), PathBuf::from("/tmp/one"));
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Keymap(_)
        ));
        assert!(matches!(next(&mut session.events).await, PtyEvent::Hook(_)));

        hooks
            .send(session_start("/tmp/two", "compact"))
            .await
            .unwrap();
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Relink {
                reason: RelinkReason::Compact,
                ..
            }
        ));
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Keymap(_)
        ));
        assert!(matches!(next(&mut session.events).await, PtyEvent::Hook(_)));
        hooks
            .send(session_start("/tmp/three", "clear"))
            .await
            .unwrap();
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Relink {
                reason: RelinkReason::Clear,
                ..
            }
        ));
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Keymap(_)
        ));

        exit.send(pty_host::ExitStatus::with_exit_code(7)).unwrap();
        loop {
            if let PtyEvent::Exited(status) = next(&mut session.events).await {
                assert_eq!(status.exit_code(), 7);
                break;
            }
        }
    }

    #[tokio::test]
    async fn hooks_and_transcript_tool_uses_derive_ask_facts() {
        let (sources, hooks, rows, _paths, _writer, _exit) = source_bundle();
        let mut session = from_test_sources(sources);
        let _ = next(&mut session.events).await;
        let _ = next(&mut session.events).await;
        let raw = serde_json::json!({
            "hook_event_name":"PermissionRequest",
            "session_id":"00000000-0000-0000-0000-000000000001",
            "transcript_path":"/tmp/one",
            "cwd":"/tmp",
            "tool_name":"Bash",
            "tool_use_id":"permission-1",
            "tool_input":{},
            "permission_suggestions":[{"type":"addDirectories","directories":["/tmp"],"destination":"session"}],
        });
        hooks
            .send(crate::hooks::parse(raw.to_string().as_bytes()).unwrap())
            .await
            .unwrap();
        loop {
            if let PtyEvent::Ask(ask) = next(&mut session.events).await {
                assert_eq!(ask.id, AskId("permission-1".to_owned()));
                assert!(matches!(
                    ask.kind,
                    AskKind::Permission {
                        suggestions: 1,
                        is_plan: false,
                        ..
                    }
                ));
                break;
            }
        }

        let row = TranscriptRow::parse(serde_json::json!({
            "type":"assistant",
            "message":{"content":[{"type":"tool_use","id":"ask-1","name":"AskUserQuestion","input":{"questions":[{"options":[{},{}],"multiSelect":true}]}}]}
        }));
        rows.send((PathBuf::from("/tmp/one"), row)).await.unwrap();
        loop {
            if let PtyEvent::Ask(ask) = next(&mut session.events).await {
                assert_eq!(ask.id, AskId("ask-1".to_string()));
                assert_eq!(
                    ask.kind,
                    AskKind::Question {
                        questions: vec![QuestionFact {
                            options: 2,
                            multi_select: true
                        }],
                    }
                );
                break;
            }
        }
    }

    #[tokio::test]
    async fn control_sends_semantic_intent_and_paste_delivery() {
        let (sources, _hooks, _rows, _paths, mut peer, _exit) = source_bundle();
        let mut session = from_test_sources(sources);
        let _ = next(&mut session.events).await;
        let initial = next(&mut session.events).await;
        let PtyEvent::Keymap(resolved) = initial else {
            panic!("session did not emit its keymap");
        };
        let intent = Intent::Prompt {
            text: "hello".to_owned(),
        };
        let result = session.control.send(intent.clone()).await.unwrap();
        assert_eq!(result.intent, intent);
        assert_eq!(result.keymap, resolved.keymap);
        assert_eq!(result.basis, resolved.basis);
        assert_eq!(result.program, super::super::keymap::ProgramName::Prompt);
        assert_eq!(result.bytes_written, b"\x1b[200~hello\x1b[201~\r".len());
        let mut bytes = vec![0; result.bytes_written];
        peer.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"\x1b[200~hello\x1b[201~\r");
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::InputResult(event) if event == result
        ));

        assert_eq!(
            session
                .control
                .deliver("message", Carrier::Pty)
                .await
                .unwrap(),
            DeliveryOutcome::Pty
        );
        let mut pasted = vec![0; b"\x1b[200~message\x1b[201~\r".len()];
        peer.read_exact(&mut pasted).await.unwrap();
        assert_eq!(pasted, b"\x1b[200~message\x1b[201~\r");
    }

    #[tokio::test]
    async fn answer_intents_use_and_consume_ask_ids() {
        let (sources, _hooks, rows, _paths, _peer, _exit) = source_bundle();
        let mut session = from_test_sources(sources);
        let _ = next(&mut session.events).await;
        let _ = next(&mut session.events).await;
        rows.send((
            PathBuf::from("/tmp/one"),
            TranscriptRow::parse(serde_json::json!({
                "type":"assistant",
                "message":{"content":[{
                    "type":"tool_use",
                    "id":"question-1",
                    "name":"AskUserQuestion",
                    "input":{"questions":[{"options":[{},{}],"multiSelect":false}]}
                }]}
            })),
        ))
        .await
        .unwrap();
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Transcript { .. }
        ));
        assert!(matches!(next(&mut session.events).await, PtyEvent::Ask(_)));

        let intent = Intent::Answer {
            ask_id: AskId("question-1".to_owned()),
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: vec![1],
                    other: None,
                }],
            }),
        };
        let result = session.control.send(intent.clone()).await.unwrap();
        assert_eq!(result.intent, intent);
        assert_eq!(
            result.program,
            super::super::keymap::ProgramName::QuestionForm
        );
        assert!(matches!(
            session.control.send(result.intent).await,
            Err(InputError::UnknownAsk(AskId(ref id))) if id == "question-1"
        ));
    }

    #[tokio::test]
    async fn semantic_input_errors_remain_typed() {
        let (sources, _hooks, rows, _paths, _peer, _exit) = source_bundle();
        let mut session = from_test_sources(sources);
        let _ = next(&mut session.events).await;
        let _ = next(&mut session.events).await;

        assert!(matches!(
            session
                .control
                .send(Intent::Prompt {
                    text: "bad\u{1b}".to_owned()
                })
                .await,
            Err(InputError::UnsafeText { .. })
        ));
        assert!(matches!(
            session
                .control
                .send(Intent::Answer {
                    ask_id: AskId("missing".to_owned()),
                    answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
                })
                .await,
            Err(InputError::UnknownAsk(AskId(ref id))) if id == "missing"
        ));

        rows.send((
            PathBuf::from("/tmp/one"),
            TranscriptRow::parse(serde_json::json!({
                "type":"assistant",
                "message":{"content":[{
                    "type":"tool_use",
                    "id":"plan-1",
                    "name":"ExitPlanMode",
                    "input":{}
                }]}
            })),
        ))
        .await
        .unwrap();
        let _ = next(&mut session.events).await;
        let _ = next(&mut session.events).await;
        assert!(matches!(
            session
                .control
                .send(Intent::Answer {
                    ask_id: AskId("plan-1".to_owned()),
                    answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
                })
                .await,
            Err(InputError::AnswerMismatchesAsk { .. })
        ));

        let (sources, hooks, _rows, _paths, _peer, _exit) = source_bundle();
        let mut session = from_test_sources(sources);
        let _ = next(&mut session.events).await;
        let _ = next(&mut session.events).await;
        let raw = serde_json::json!({
            "hook_event_name":"PermissionRequest",
            "session_id":"00000000-0000-0000-0000-000000000001",
            "transcript_path":"/tmp/one",
            "cwd":"/tmp",
            "tool_name":"Bash",
            "tool_use_id":"permission-7",
            "tool_input":{},
            "permission_suggestions": vec![serde_json::json!({
                "type":"addDirectories",
                "directories":["/tmp"],
                "destination":"session"
            }); 7],
        });
        hooks
            .send(crate::hooks::parse(raw.to_string().as_bytes()).unwrap())
            .await
            .unwrap();
        loop {
            if matches!(next(&mut session.events).await, PtyEvent::Ask(_)) {
                break;
            }
        }
        assert!(matches!(
            session
                .control
                .send(Intent::Answer {
                    ask_id: AskId("permission-7".to_owned()),
                    answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
                })
                .await,
            Err(InputError::UnverifiedShape { .. })
        ));

        let (sources, _hooks, _rows, _paths, _peer, _exit) = source_bundle();
        let session = from_sources(
            sources,
            &super::super::keymap::KeymapSources {
                baked: &[],
                user_dir: None,
            },
        );
        assert!(matches!(
            session
                .control
                .send(Intent::Prompt {
                    text: "hello".to_owned()
                })
                .await,
            Err(InputError::NoKeymap {
                program: super::super::keymap::ProgramName::Prompt,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn relink_reloads_keymap_sources_and_emits_the_new_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude.toml");
        let original = super::super::keymap::BAKED_KEYMAPS[0].1;
        std::fs::write(&path, original).unwrap();
        let keymaps = super::super::keymap::KeymapSources {
            baked: super::super::keymap::BAKED_KEYMAPS,
            user_dir: Some(dir.path().to_path_buf()),
        };
        let (sources, hooks, _rows, _paths, _peer, _exit) = source_bundle();
        let mut session = from_sources(sources, &keymaps);
        let _ = next(&mut session.events).await;
        let PtyEvent::Keymap(initial) = next(&mut session.events).await else {
            panic!("session did not emit its initial keymap");
        };
        assert!(matches!(
            initial.keymap.source,
            super::super::keymap::KeymapSource::User(_)
        ));

        std::fs::write(
            &path,
            original.replace("after_paste = 400", "after_paste = 401"),
        )
        .unwrap();
        hooks
            .send(session_start("/tmp/relinked", "compact"))
            .await
            .unwrap();
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Relink { .. }
        ));
        let PtyEvent::Keymap(relinked) = next(&mut session.events).await else {
            panic!("relink did not emit a keymap");
        };
        assert_ne!(relinked.keymap.digest, initial.keymap.digest);
        assert_eq!(relinked.basis, initial.basis);
    }

    #[test]
    fn paste_program_normalizes_controls_without_exposing_terminal_input() {
        assert_eq!(
            paste_program("tab\there\r\nreturn\r escape\x1b[201~rest\0"),
            vec![
                PtyInput::Bytes(b"\x1b[200~tab here\nreturn\n escape[201~rest\x1b[201~".to_vec()),
                PtyInput::Delay(400),
                PtyInput::Bytes(b"\r".to_vec()),
            ]
        );
    }

    #[test]
    fn socket_confirmation_requires_a_row_attributable_to_the_envelope() {
        let confirmation = uuid::Uuid::new_v4().to_string();
        assert!(row_confirms_delivery(
            &serde_json::json!({
                "type": "queue-operation",
                "operation": "enqueue",
                "content": format!("<cross-session-message>[amux id={confirmation}]"),
            }),
            &confirmation,
        ));
        assert!(row_confirms_delivery(
            &serde_json::json!({
                "type": "user",
                "origin": {"kind": "peer"},
                "message": {"content": format!("native {confirmation}")},
            }),
            &confirmation,
        ));
        assert!(row_confirms_delivery(
            &serde_json::json!({
                "type": "attachment",
                "attachment": {
                    "type": "queued_command",
                    "prompt": format!("queued {confirmation}"),
                },
            }),
            &confirmation,
        ));
        assert!(!row_confirms_delivery(
            &serde_json::json!({
                "type": "queue-operation",
                "operation": "enqueue",
                "content": format!("[amux id={}]", uuid::Uuid::new_v4()),
            }),
            &confirmation,
        ));
        assert!(!row_confirms_delivery(
            &serde_json::json!({"type": "queue-operation", "operation": "dequeue"}),
            &confirmation,
        ));
        assert!(!row_confirms_delivery(
            &serde_json::json!({
                "type": "user",
                "origin": {"kind": "human"},
                "message": {"content": confirmation},
            }),
            &confirmation,
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_delivery_confirms_by_transcript_and_falls_back_to_paste() {
        use tokio::net::UnixListener;

        let dir = tempfile::Builder::new()
            .prefix("cp")
            .tempdir_in("/tmp")
            .unwrap();
        let socket_path = dir.path().join("message.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let (sources, _hooks, rows, _paths, mut peer, _exit) = source_bundle();
        let session = from_test_sources(sources);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut lines = tokio::io::BufReader::new(stream).lines();
            let _ = lines.next_line().await.unwrap();
            let _ = lines.next_line().await.unwrap();
            rows.send((
                PathBuf::from("/tmp/one"),
                TranscriptRow::parse(
                    serde_json::json!({"type":"queue-operation","operation":"enqueue","content":"delivery-1"}),
                ),
            ))
            .await
            .unwrap();
        });
        assert_eq!(
            session
                .control
                .deliver(
                    "socket message",
                    Carrier::Socket {
                        path: socket_path,
                        token: "secret".to_string(),
                        confirmation: "delivery-1".to_string(),
                    }
                )
                .await
                .unwrap(),
            DeliveryOutcome::Socket
        );
        server.await.unwrap();

        let outcome = session
            .control
            .deliver(
                "fallback",
                Carrier::Socket {
                    path: dir.path().join("missing.sock"),
                    token: "secret".to_string(),
                    confirmation: "delivery-2".to_string(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(outcome, DeliveryOutcome::PtyFallback { .. }));
        let mut pasted = vec![0; b"\x1b[200~fallback\x1b[201~\r".len()];
        peer.read_exact(&mut pasted).await.unwrap();
        assert_eq!(pasted, b"\x1b[200~fallback\x1b[201~\r");
    }
}

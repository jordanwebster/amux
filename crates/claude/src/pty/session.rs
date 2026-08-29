//! One Claude PTY event stream paired with its control handle.

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

    fn live() -> Self {
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
    InputResult(InputResult),
    Delivery(DeliveryOutcome),
    Exited(pty_host::ExitStatus),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskFacts {
    pub id: AskId,
    pub kind: AskKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AskId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionFact {
    pub options: usize,
    pub multi_select: bool,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputResult {
    pub bytes_written: usize,
    pub steps: usize,
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
    #[error("PTY input failed: {0}")]
    Io(#[from] std::io::Error),
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

#[derive(Clone)]
pub struct Control {
    writer: Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    terminal_output: Arc<Mutex<Option<mpsc::Receiver<Bytes>>>>,
    terminal: Option<pty_host::PtyHandle>,
    events: mpsc::Sender<PtyEvent>,
    confirmations: broadcast::Sender<Value>,
    exit: watch::Receiver<Option<pty_host::ExitStatus>>,
}

impl Control {
    pub async fn send(&self, program: Vec<PtyInput>) -> Result<InputResult, InputError> {
        let mut writer = self.writer.lock().await;
        let mut bytes_written = 0;
        for step in &program {
            match step {
                PtyInput::Bytes(bytes) => {
                    writer.write_all(bytes).await?;
                    writer.flush().await?;
                    bytes_written += bytes.len();
                }
                PtyInput::Delay(ms) => {
                    tokio::time::sleep(Duration::from_millis(u64::from((*ms).min(MAX_DELAY_MS))))
                        .await
                }
            }
        }
        let result = InputResult {
            bytes_written,
            steps: program.len(),
        };
        let _ = self
            .events
            .send(PtyEvent::InputResult(result.clone()))
            .await;
        Ok(result)
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
                self.send(paste_program(text))
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
                        self.send(paste_program(text))
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

pub async fn spawn(launch: &Launch, size: pty_host::PtySize) -> Result<Session, SpawnError> {
    let version = probe_version(&launch.binary).await?;
    let hook_dir = std::env::temp_dir()
        .join("amux-claude-hooks")
        .join(launch.session_id.to_string());
    let receiver = HookReceiver::bind(&hook_dir)
        .await
        .map_err(SpawnError::Hook)?;
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
    Ok(from_sources(Sources {
        pty: PtySource {
            output,
            writer,
            handle: Some(handle),
            exit: Box::pin(async move { exit.wait().await }),
        },
        hooks: HookSource::from_receiver(receiver),
        transcript: TranscriptSource::live(),
        version,
    }))
}

pub fn from_sources(sources: Sources) -> Session {
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

    event_tx
        .try_send(PtyEvent::Ready { version })
        .expect("new PTY event channel has capacity for readiness");

    let tx = event_tx.clone();
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
            }
            let ask = ask_from_hook(&hook);
            if tx.send(PtyEvent::Hook(hook)).await.is_err() {
                break;
            }
            if let Some(ask) = ask
                && tx.send(PtyEvent::Ask(ask)).await.is_err()
            {
                break;
            }
        }
    });

    let tx = event_tx.clone();
    let confirmations = confirmation_tx.clone();
    tokio::spawn(async move {
        let _task = task;
        while let Some((path, row)) = rows.recv().await {
            let _ = confirmations.send(row.as_value().clone());
            let ask = ask_from_transcript(&row);
            if tx.send(PtyEvent::Transcript { path, row }).await.is_err() {
                break;
            }
            if let Some(ask) = ask
                && tx.send(PtyEvent::Ask(ask)).await.is_err()
            {
                break;
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
            confirmations: confirmation_tx,
            exit: exit_rx,
        },
    }
}

pub fn from_recording(
    replay: &mut replay_support::StrictReplay,
    manifest: &replay_support::Manifest,
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
    Ok(from_sources(Sources {
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
    }))
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
            raw,
        })
    }

    async fn next(events: &mut EventStream) -> PtyEvent {
        tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event stream remains open")
    }

    #[tokio::test]
    async fn sources_emit_ready_compact_clear_relinks_and_exit() {
        let (sources, hooks, _rows, mut paths, _writer, exit) = source_bundle();
        let mut session = from_sources(sources);
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Ready { .. }
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
        let mut session = from_sources(sources);
        let _ = next(&mut session.events).await;
        let raw = serde_json::json!({
            "hook_event_name":"PermissionRequest",
            "session_id":"00000000-0000-0000-0000-000000000001",
            "transcript_path":"/tmp/one",
            "cwd":"/tmp",
            "tool_name":"Bash",
            "tool_input":{},
            "permission_suggestions":[{"type":"addDirectories","directories":["/tmp"],"destination":"session"}],
        });
        hooks
            .send(crate::hooks::parse(raw.to_string().as_bytes()).unwrap())
            .await
            .unwrap();
        loop {
            if let PtyEvent::Ask(ask) = next(&mut session.events).await {
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
    async fn control_sends_fixed_program_and_paste_delivery() {
        let (sources, _hooks, _rows, _paths, mut peer, _exit) = source_bundle();
        let session = from_sources(sources);
        let result = session
            .control
            .send(vec![PtyInput::Bytes(b"hello".to_vec())])
            .await
            .unwrap();
        assert_eq!(
            result,
            InputResult {
                bytes_written: 5,
                steps: 1
            }
        );
        let mut bytes = [0; 5];
        peer.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"hello");

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
        let session = from_sources(sources);
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

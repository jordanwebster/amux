//! Host-side Claude scripts, played through live JSONL tailing and PTY hooks.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use claude::pty::{AskId, HookSource, PtyEvent, PtySource, Session, Sources, TranscriptSource};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;
use uuid::Uuid;

use crate::claude_io::{AskAnswer, Intent, PermissionAnswer, PlanAnswer};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub efforts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reaction {
    pub on: Trigger,
    pub play: Vec<Step>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Trigger {
    AnyPrompt,
    PromptContains(String),
    Command { name: String },
    Answer(AskKindMatch),
    Interrupt,
    Any,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AskKindMatch {
    Permission,
    Question,
    Plan,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Step {
    Rows {
        jsonl: Vec<Value>,
    },
    Markdown {
        text: String,
    },
    /// A turn beginning with somebody asking for something.
    ///
    /// The same rows and hook a real prompt writes, so a script can play a
    /// whole turn — the ask, the work, the rule that closes it — without a
    /// client being there to type one. A turn nobody opened cannot end, and
    /// the row that says a turn ended is one a reader is shown.
    Prompt {
        text: String,
    },
    Tool {
        name: String,
        input: Value,
        output: Option<String>,
        denied: bool,
        /// The tool ran and came back an error. A refusal and a failure are
        /// different rows to a reader, and only a denial carries the typed
        /// denial kind, so a script says which of the two it means.
        #[serde(default)]
        failed: bool,
        /// The `toolUseResult` sidecar Claude writes beside a tool result.
        ///
        /// The text a tool returns is what a reader sees; the sidecar is what
        /// the fold reads to say a file changed by so many lines. A step that
        /// only carries text can never become a file-change row, so a script
        /// proving one says what the sidecar held.
        #[serde(default)]
        result: Option<Value>,
    },
    Ask(ScriptAsk),
    Todo {
        items: Vec<(String, TodoState)>,
    },
    ChildStarted {
        name: String,
    },
    ChildFinished {
        name: String,
    },
    AgentMessage {
        from: String,
        text: String,
        /// What the carrier said the envelope was: a message, a sender that
        /// finished its turn, a sender whose session ended. Unstated is an
        /// ordinary message.
        #[serde(default)]
        kind: Option<String>,
    },
    /// The person cut a turn, or the tool it had asked to run, short.
    Interrupted {
        tool_use: bool,
    },
    Working {
        secs: f32,
    },
    EndTurn,
    Compaction,
    ApiError {
        message: String,
    },
    Exit {
        code: i32,
    },
    Unknown {
        raw: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ScriptAsk {
    Permission {
        tool: String,
        invocation: Value,
        scoped_directories: Vec<String>,
    },
    Question {
        questions: Vec<QuestionSpec>,
    },
    Plan {
        markdown: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionSpec {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoState {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObservedInput {
    pub seq: u64,
    pub intent: String,
    pub text: Option<String>,
    pub ask_id: Option<String>,
    pub answer: Option<Value>,
    pub pins: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError {
    #[error("no pending ask")]
    NoPendingAsk,
    #[error("unknown ask {0}")]
    UnknownAsk(AskId),
    #[error("no remaining reaction matches the input")]
    Exhausted,
    #[error("script session is closed")]
    Closed,
    #[error("script playback failed: {0}")]
    Playback(String),
}

pub struct Engine {
    script: Script,
    cursor: usize,
    pending_ask: Option<(AskId, AskKindMatch)>,
    observed: Vec<ObservedInput>,
    running: bool,
    queued: VecDeque<Intent>,
    failure: Option<ScriptError>,
}

impl Engine {
    pub fn new(script: Script) -> Self {
        Self {
            script,
            cursor: 0,
            pending_ask: None,
            observed: Vec::new(),
            running: false,
            queued: VecDeque::new(),
            failure: None,
        }
    }

    pub fn observed(&self) -> &[ObservedInput] {
        &self.observed
    }

    /// Record arrival before validation, including refused answers and deferred prompts.
    pub fn feed(
        &mut self,
        input: Intent,
        pins: Vec<crate::ArtifactId>,
    ) -> Result<Vec<Step>, ScriptError> {
        let value = serde_json::to_value(&input).expect("Intent serializes");
        self.observed.push(ObservedInput {
            seq: self.observed.len() as u64 + 1,
            intent: value["intent"].as_str().unwrap().to_owned(),
            text: value["text"].as_str().map(str::to_owned),
            ask_id: value["ask_id"].as_str().map(str::to_owned),
            answer: value.get("answer").cloned(),
            pins: pins.into_iter().map(|id| id.to_string()).collect(),
        });
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if matches!(input, Intent::Prompt { .. }) && self.running {
            self.queued.push_back(input);
            return Ok(Vec::new());
        }
        self.react(&input)
    }

    fn react(&mut self, input: &Intent) -> Result<Vec<Step>, ScriptError> {
        if let Intent::Answer { ask_id, .. } = input
            && !self
                .pending_ask
                .as_ref()
                .is_some_and(|(id, _)| id.0 == *ask_id)
        {
            return Err(ScriptError::UnknownAsk(AskId(ask_id.clone())));
        }
        let offset = self.script.reactions[self.cursor..]
            .iter()
            .position(|reaction| reaction.on.matches(input))
            .ok_or(ScriptError::Exhausted)?;
        self.cursor += offset + 1;
        if matches!(input, Intent::Prompt { .. }) {
            self.running = true;
        }
        if matches!(input, Intent::Answer { .. }) {
            self.pending_ask = None;
        }
        Ok(self.script.reactions[self.cursor - 1].play.clone())
    }

    /// Called only when the player reaches EndTurn, never when a reaction is selected.
    fn end_turn(&mut self) -> bool {
        let running = std::mem::take(&mut self.running);
        self.pending_ask = None;
        running
    }

    fn next_prompt(&mut self) -> Result<Option<Playback>, ScriptError> {
        if self.running {
            return Ok(None);
        }
        self.queued
            .pop_front()
            .map(|input| {
                let steps = self.react(&input)?;
                Ok(Playback {
                    input: Some(input),
                    steps,
                    reply: None,
                })
            })
            .transpose()
    }
}

impl Trigger {
    fn matches(&self, input: &Intent) -> bool {
        match (self, input) {
            (Self::Any, _)
            | (Self::AnyPrompt, Intent::Prompt { .. })
            | (Self::Interrupt, Intent::Interrupt) => true,
            (Self::PromptContains(needle), Intent::Prompt { text }) => text.contains(needle),
            (Self::Command { name }, Intent::Prompt { text }) => {
                text.split_whitespace()
                    .next()
                    .and_then(|word| word.strip_prefix('/'))
                    == Some(name.trim_start_matches('/'))
            }
            (Self::Answer(kind), Intent::Answer { answer, .. }) => {
                *kind
                    == match answer {
                        AskAnswer::Permission(_) => AskKindMatch::Permission,
                        AskAnswer::Question(_) => AskKindMatch::Question,
                        AskAnswer::Plan(_) => AskKindMatch::Plan,
                    }
            }
            _ => false,
        }
    }
}

struct Playback {
    input: Option<Intent>,
    steps: Vec<Step>,
    reply: Option<oneshot::Sender<Result<(), ScriptError>>>,
}

/// Owns playback and its temporary transcript. Dropping the last handle closes the session.
#[derive(Clone)]
pub struct Provider(Arc<ProviderInner>);

struct ProviderInner {
    engine: Arc<Mutex<Engine>>,
    tx: mpsc::UnboundedSender<Playback>,
    tasks: Vec<AbortHandle>,
}

impl Drop for ProviderInner {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Provider {
    /// The daemon calls this after decoding and accepting structured input.
    pub fn feed(&self, input: Intent, pins: Vec<crate::ArtifactId>) -> Result<(), ScriptError> {
        let mut engine = self.0.engine.lock().unwrap();
        let deferred = engine.running && matches!(input, Intent::Prompt { .. });
        let steps = engine.feed(input.clone(), pins)?;
        if !deferred {
            self.0
                .tx
                .send(Playback {
                    input: Some(input),
                    steps,
                    reply: None,
                })
                .map_err(|_| ScriptError::Closed)?;
        }
        Ok(())
    }

    pub fn observed(&self) -> Vec<ObservedInput> {
        self.0.engine.lock().unwrap().observed.clone()
    }

    pub fn error(&self) -> Option<ScriptError> {
        self.0.engine.lock().unwrap().failure.clone()
    }

    /// Control operations settle after their rows and hooks reach the real session stream.
    pub async fn play(&self, steps: Vec<Step>) -> Result<(), ScriptError> {
        if let Some(error) = self.error() {
            return Err(error);
        }
        let (tx, rx) = oneshot::channel();
        self.0
            .tx
            .send(Playback {
                input: None,
                steps,
                reply: Some(tx),
            })
            .map_err(|_| ScriptError::Closed)?;
        rx.await.map_err(|_| ScriptError::Closed)?
    }
}

#[derive(Clone, Default)]
struct Progress {
    rows: u64,
    hooks: u64,
    asks: u64,
    exited: bool,
}

/// A process-free PTY with real provider parsing, semantic asks and live file tailing.
pub async fn session(script: Script) -> Result<(Session, Provider), ScriptError> {
    let root = tempfile::tempdir().map_err(playback_error)?;
    let session_id = Uuid::new_v4();
    let path = root.path().join(format!("{session_id}.jsonl"));
    let file = tokio::fs::File::create(&path)
        .await
        .map_err(playback_error)?;
    let (output_tx, output) = mpsc::channel(64);
    let (writer, mut echo) = tokio::io::duplex(64 * 1024);
    let echo_task = tokio::spawn(async move {
        let mut bytes = [0; 4096];
        while let Ok(count) = echo.read(&mut bytes).await {
            if count == 0
                || output_tx
                    .send(bytes[..count].to_vec().into())
                    .await
                    .is_err()
            {
                break;
            }
        }
    });
    let (hooks, hook_tx) = HookSource::channel(64);
    let (exit_tx, exit_rx) = oneshot::channel();
    let Session {
        mut events,
        control,
    } = claude::pty::from_sources(
        Sources {
            pty: PtySource {
                output,
                writer: Box::new(writer),
                handle: None,
                exit: Box::pin(async move {
                    exit_rx
                        .await
                        .unwrap_or_else(|_| pty_host::ExitStatus::with_signal("script closed"))
                }),
            },
            hooks,
            transcript: TranscriptSource::live(),
            version: claude::version::ClaudeVersion(semver::Version::new(2, 1, 251)),
            delays: claude::pty::DelaySource::live(),
        },
        &claude::pty::keymap::KeymapSources::default(),
    );
    let (tx, public_events) = mpsc::channel(256);
    let (progress_tx, progress) = watch::channel(Progress::default());
    // Observe the actual parser output. File flush alone cannot order independent source tasks.
    let forward = tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                _ = tx.closed() => break,
                event = events.recv() => match event { Some(event) => event, None => break },
            };
            let exited = matches!(event, PtyEvent::Exited(_));
            progress_tx.send_modify(|progress| match &event {
                PtyEvent::Transcript { row, .. }
                    if row.as_value()["type"] != "amux.transcript_ready" =>
                {
                    progress.rows += 1
                }
                PtyEvent::Hook(_) => progress.hooks += 1,
                PtyEvent::Ask(_) => progress.asks += 1,
                PtyEvent::Exited(_) => progress.exited = true,
                _ => {}
            });
            if tx.send(event).await.is_err() || exited {
                break;
            }
        }
    });
    let engine = Arc::new(Mutex::new(Engine::new(script)));
    let (tx, mut rx) = mpsc::unbounded_channel::<Playback>();
    let mut player = Player {
        _root: root,
        path,
        file,
        hooks: hook_tx,
        exit: Some(exit_tx),
        progress,
        engine: engine.clone(),
        serial: 0,
        session_id,
        duration_ms: 0,
        message_count: 0,
        turn_open: false,
        turn_ended: false,
    };
    let worker = tokio::spawn(async move {
        let result = async {
            player
                .hook("SessionStart", json!({"source":"startup"}))
                .await?;
            while let Some(mut work) = rx.recv().await {
                loop {
                    let result = player.play(&work).await;
                    if let Some(reply) = work.reply.take() {
                        let _ = reply.send(result.clone());
                    }
                    result?;
                    if player.exit.is_none() {
                        return Ok(());
                    }
                    let next = {
                        let mut engine = player.engine.lock().unwrap();
                        if player.turn_ended {
                            engine.end_turn();
                        }
                        engine.next_prompt()?
                    };
                    match next {
                        Some(next) => work = next,
                        None => break,
                    }
                }
            }
            Ok::<(), ScriptError>(())
        }
        .await;
        player.engine.lock().unwrap().failure = Some(result.err().unwrap_or(ScriptError::Closed));
    });
    Ok((
        Session {
            events: public_events,
            control,
        },
        Provider(Arc::new(ProviderInner {
            engine,
            tx,
            tasks: vec![
                worker.abort_handle(),
                forward.abort_handle(),
                echo_task.abort_handle(),
            ],
        })),
    ))
}

struct Player {
    _root: tempfile::TempDir,
    path: PathBuf,
    file: tokio::fs::File,
    hooks: mpsc::Sender<claude::hooks::HookPayload>,
    exit: Option<oneshot::Sender<pty_host::ExitStatus>>,
    progress: watch::Receiver<Progress>,
    engine: Arc<Mutex<Engine>>,
    serial: u64,
    session_id: Uuid,
    duration_ms: u64,
    message_count: u64,
    turn_open: bool,
    turn_ended: bool,
}

impl Player {
    fn id(&mut self) -> String {
        self.serial += 1;
        Uuid::from_u128(u128::from(self.serial)).to_string()
    }

    async fn wait(&mut self, ready: impl Fn(&Progress) -> bool) -> Result<(), ScriptError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if ready(&self.progress.borrow_and_update()) {
                    return Ok(());
                }
                self.progress
                    .changed()
                    .await
                    .map_err(|_| ScriptError::Closed)?;
            }
        })
        .await
        .map_err(|_| ScriptError::Playback("provider ingestion timed out".into()))?
    }

    async fn rows(&mut self, rows: Vec<Value>) -> Result<(), ScriptError> {
        let target = self.progress.borrow().rows + rows.len() as u64;
        for row in rows {
            let mut bytes = serde_json::to_vec(&row).map_err(playback_error)?;
            bytes.push(b'\n');
            self.file.write_all(&bytes).await.map_err(playback_error)?;
            if matches!(row["type"].as_str(), Some("user" | "assistant")) {
                self.message_count += 1;
            }
        }
        self.file.flush().await.map_err(playback_error)?;
        self.wait(|progress| progress.rows >= target).await
    }

    fn row(&mut self, kind: &str, fields: Value) -> Value {
        let mut row = json!({"type":kind, "uuid": self.id(), "sessionId":self.session_id,
            "timestamp":"2026-01-01T00:00:00.000Z"});
        row.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        row
    }

    async fn message(&mut self, role: &str, content: Value) -> Result<(), ScriptError> {
        let id = self.id();
        let row = self.row(
            role,
            json!({"message":{"id":id, "role":role, "content":content}}),
        );
        self.rows(vec![row]).await
    }

    async fn hook(&mut self, name: &str, fields: Value) -> Result<(), ScriptError> {
        let mut raw = json!({"hook_event_name":name, "session_id":self.session_id,
            "transcript_path":self.path, "cwd":self._root.path(), "permission_mode":"default"});
        raw.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let hook = claude::hooks::parse(&serde_json::to_vec(&raw).map_err(playback_error)?)
            .map_err(playback_error)?;
        let target = self.progress.borrow().hooks + 1;
        let asks = self.progress.borrow().asks + u64::from(name == "PermissionRequest");
        self.hooks
            .send(hook)
            .await
            .map_err(|_| ScriptError::Closed)?;
        self.wait(|progress| progress.hooks >= target && progress.asks >= asks)
            .await
    }

    async fn tool(
        &mut self,
        name: &str,
        input: &Value,
        output: Option<&str>,
        denied: bool,
        failed: bool,
        result: Option<Value>,
    ) -> Result<(), ScriptError> {
        let id = self.id();
        self.message(
            "assistant",
            json!([{"type":"tool_use", "id":id, "name":name, "input":input}]),
        )
        .await?;
        self.hook(
            "PreToolUse",
            json!({"tool_name":name,"tool_input":input,"tool_use_id":id}),
        )
        .await?;
        if output.is_some() || denied || failed {
            let content = if denied {
                "The user doesn't want to proceed with this tool use."
            } else {
                output.unwrap_or("the tool returned an error")
            };
            let mut row = self.row("user", json!({"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":content,"is_error":denied || failed}]}}));
            // Only a denial carries the typed denial kind. A failure without
            // one is what tells the fold the two apart.
            if denied {
                row["toolDenialKind"] = json!("user_rejected");
            }
            if let Some(sidecar) = result {
                row["toolUseResult"] = sidecar;
            }
            self.rows(vec![row]).await?;
            self.hook(
                if denied || failed {
                    "PostToolUseFailure"
                } else {
                    "PostToolUse"
                },
                json!({"tool_name":name,"tool_response":content,"tool_use_id":id}),
            )
            .await?;
        }
        Ok(())
    }

    async fn ask(&mut self, ask: &ScriptAsk) -> Result<(), ScriptError> {
        let id = self.id();
        let (kind, tool, input, directories) = match ask {
            ScriptAsk::Permission {
                tool,
                invocation,
                scoped_directories,
            } => (
                AskKindMatch::Permission,
                tool.as_str(),
                invocation.clone(),
                scoped_directories.clone(),
            ),
            ScriptAsk::Question { questions } => (
                AskKindMatch::Question,
                "AskUserQuestion",
                json!({"questions":questions}),
                vec![],
            ),
            ScriptAsk::Plan { markdown } => (
                AskKindMatch::Plan,
                "ExitPlanMode",
                json!({"plan":markdown}),
                vec![],
            ),
        };
        self.engine.lock().unwrap().pending_ask = Some((AskId(id.clone()), kind));
        let suggestions: Vec<Value> = directories.into_iter().map(|directory|
            json!({"type":"addDirectories","directories":[directory],"destination":"session"})).collect();
        // The hook first and the transcript row after it, which is the order a
        // real Claude produces them in. A reader pairs the two by the tool the
        // ask is about, and it only looks forwards: an ask announced after its
        // own transcript row is never paired with it, and an unpaired ask is
        // one nothing can ever close — so the session could be asked exactly
        // one question and every answer after that was refused as queued
        // behind a menu that had not gone away.
        self.hook(
            "PermissionRequest",
            json!({"tool_use_id":id,"tool_name":tool,
            "tool_input":input,"permission_suggestions":suggestions}),
        )
        .await?;
        self.message(
            "assistant",
            json!([{"type":"tool_use","id":id,"name":tool,"input":input}]),
        )
        .await
    }

    /// The result a real Claude writes once an ask has been answered.
    ///
    /// A permission or a plan review is a tool the agent asked to use, and
    /// answering it finishes that tool: the transcript carries the result
    /// under the same `tool_use_id`, and that correlation is what takes the
    /// ask out of every reader's queue. Without it a scripted session could
    /// be asked exactly one question — the second answer is refused as queued
    /// behind a menu that never closed.
    async fn answered(&mut self, id: &str, answer: &AskAnswer) -> Result<(), ScriptError> {
        let (content, refused) = match answer {
            AskAnswer::Permission(PermissionAnswer::Deny { feedback }) => (
                feedback.clone().unwrap_or_else(|| {
                    "The user doesn't want to proceed with this tool use.".to_string()
                }),
                true,
            ),
            AskAnswer::Permission(_) => ("Permission granted.".to_string(), false),
            AskAnswer::Plan(PlanAnswer::RequestChanges { feedback }) => (feedback.clone(), true),
            AskAnswer::Plan(_) => ("The user has approved your plan.".to_string(), false),
            AskAnswer::Question(response) => (
                serde_json::to_string(response).map_err(playback_error)?,
                false,
            ),
        };
        let row = self.row(
            "user",
            json!({"message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":id,"content":content,"is_error":refused}]}}),
        );
        self.rows(vec![row]).await
    }

    async fn play(&mut self, work: &Playback) -> Result<(), ScriptError> {
        self.turn_ended = false;
        if let Some(Intent::Answer { ask_id, answer }) = &work.input {
            let (ask_id, answer) = (ask_id.clone(), answer.clone());
            self.answered(&ask_id, &answer).await?;
        }
        if let Some(Intent::Prompt { text }) = &work.input {
            self.turn_open = true;
            self.duration_ms = 0;
            self.hook("UserPromptSubmit", json!({"prompt":text}))
                .await?;
            let row = self.row(
                "user",
                json!({
                    "origin":{"kind":"human"}, "promptSource":"typed",
                    "message":{"role":"user","content":text},
                }),
            );
            self.rows(vec![row]).await?;
        }
        for step in &work.steps {
            match step {
                Step::Rows { jsonl } => self.rows(jsonl.clone()).await?,
                Step::Unknown { raw } => self.rows(vec![raw.clone()]).await?,
                Step::Prompt { text } => {
                    self.turn_open = true;
                    self.duration_ms = 0;
                    self.hook("UserPromptSubmit", json!({"prompt":text}))
                        .await?;
                    let row = self.row(
                        "user",
                        json!({
                            "origin":{"kind":"human"}, "promptSource":"typed",
                            "message":{"role":"user","content":text},
                        }),
                    );
                    self.rows(vec![row]).await?
                }
                Step::Markdown { text } => {
                    self.message("assistant", json!([{"type":"text","text":text}]))
                        .await?
                }
                Step::Tool {
                    name,
                    input,
                    output,
                    denied,
                    failed,
                    result,
                } => {
                    self.tool(
                        name,
                        input,
                        output.as_deref(),
                        *denied,
                        *failed,
                        result.clone(),
                    )
                    .await?
                }
                Step::Ask(ask) => self.ask(ask).await?,
                Step::Todo { items } => {
                    let todos: Vec<_> = items.iter().map(|(text, state)| json!({"content":text,"activeForm":text,"status":state})).collect();
                    self.tool(
                        "TodoWrite",
                        &json!({"todos":todos}),
                        Some("Todos updated"),
                        false,
                        false,
                        None,
                    )
                    .await?;
                }
                Step::ChildStarted { name } => {
                    self.tool("Agent", &json!({"description":name,"subagent_type":"general-purpose","run_in_background":true}), Some(&format!("agentId: {name}")), false, false, None).await?;
                    self.hook(
                        "SubagentStart",
                        json!({"agent_id":name,"agent_type":"general-purpose"}),
                    )
                    .await?;
                }
                Step::ChildFinished { name } => {
                    let row = self.row("user", json!({"origin":{"kind":"task-notification"},"message":{"role":"user","content":format!("Agent {name} completed")}}));
                    self.rows(vec![row]).await?;
                    self.hook("SubagentStop", json!({"agent_id":name})).await?;
                }
                Step::AgentMessage { from, text, kind } => {
                    let escape = |s: &str| {
                        s.replace('&', "&amp;")
                            .replace('<', "&lt;")
                            .replace('>', "&gt;")
                            .replace('"', "&quot;")
                    };
                    self.message(
                        "user",
                        json!(format!(
                            "<amux from=\"{}\" kind=\"{}\">\n{}\n</amux>",
                            escape(from),
                            escape(kind.as_deref().unwrap_or("message")),
                            escape(text)
                        )),
                    )
                    .await?;
                }
                Step::Interrupted { tool_use } => {
                    // The canonical markers claude writes when somebody cuts
                    // a turn short; the fold reads these strings and nothing
                    // else, so a script that means an interruption says them.
                    let text = if *tool_use {
                        "[Request interrupted by user for tool use]"
                    } else {
                        "[Request interrupted by user]"
                    };
                    self.message("user", json!([{"type":"text","text":text}]))
                        .await?
                }
                Step::Working { secs } => {
                    let duration = Duration::try_from_secs_f32(*secs).map_err(playback_error)?;
                    self.duration_ms = self
                        .duration_ms
                        .saturating_add(duration.as_millis().try_into().unwrap_or(u64::MAX));
                    tokio::time::sleep(duration).await;
                }
                Step::EndTurn => {
                    let ended = std::mem::take(&mut self.turn_open);
                    if ended {
                        self.turn_ended = true;
                        // Publish the stop pre-signal before the authoritative final row,
                        // so observing turn completion does not race a trailing Stop hook.
                        self.hook("Stop", json!({})).await?;
                        let row = self.row("system", json!({"subtype":"turn_duration","durationMs":self.duration_ms,"messageCount":self.message_count}));
                        self.rows(vec![row]).await?;
                    }
                }
                Step::Compaction => {
                    let row = self.row(
                        "system",
                        json!({"subtype":"compact_boundary","compactMetadata":{"trigger":"auto"}}),
                    );
                    self.rows(vec![row]).await?;
                    self.hook("SessionStart", json!({"source":"compact"}))
                        .await?;
                }
                Step::ApiError { message } => {
                    let row = self.row("assistant", json!({"isApiErrorMessage":true,"message":{"role":"assistant","content":[{"type":"text","text":message}]}}));
                    self.rows(vec![row]).await?;
                }
                Step::Exit { code } => {
                    self.hook("SessionEnd", json!({"reason":"other"})).await?;
                    self.exit
                        .take()
                        .ok_or(ScriptError::Closed)?
                        .send(pty_host::ExitStatus::with_exit_code(*code as u32))
                        .map_err(|_| ScriptError::Closed)?;
                    self.wait(|progress| progress.exited).await?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

fn playback_error(error: impl std::fmt::Display) -> ScriptError {
    ScriptError::Playback(error.to_string())
}

#[cfg(test)]
#[path = "script/tests.rs"]
mod testnet_script;

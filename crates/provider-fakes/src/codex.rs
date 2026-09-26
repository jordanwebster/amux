//! `fake-codex`: `codex app-server` speaking JSON-RPC over stdio.
//!
//! Modelled on codex-cli 0.157.0: the host initializes, starts or resumes a
//! thread and starts turns; a turn reports its items as started, streamed
//! and completed notifications and ends with `turn/completed`. Asks are
//! server requests (command, file-change and access approvals, questions,
//! tool-server forms and links) that block the turn until the host
//! answers. `thread/inject_items` is acknowledged at once and reports
//! nothing: written while a turn runs, the turn drains it and goes on;
//! written to an idle thread, it waits for the next `turn/start`.
//! `turn/steer` adds the host's input to the running turn as a user
//! message; `turn/interrupt` ends the turn as interrupted.

use std::collections::{BTreeMap, VecDeque};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader, Stdout};
use tokio::sync::mpsc;

use crate::lines::Out;
use crate::script::{Ask, Question, Script, Step, Tool};
use crate::{DRIFT_EXIT, Mode};

/// The asks Codex can raise.
pub const RAISES: &[&str] = &["permission", "question", "form", "link", "grant"];

/// The Codex version the fake reports: the newest the corpus shows.
pub const VERSION: &str = "0.157.0";

pub fn main() -> i32 {
    let mode = match crate::mode_from_env() {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("fake-codex: {error}");
            return DRIFT_EXIT;
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    runtime.block_on(async move {
        match mode {
            Mode::Playback(process) => {
                match crate::lines::play(
                    &process,
                    BufReader::new(tokio::io::stdin()),
                    tokio::io::stdout(),
                )
                .await
                {
                    Ok(code) => code.unwrap_or(0),
                    Err(error) => {
                        eprintln!("fake-codex: {error}");
                        DRIFT_EXIT
                    }
                }
            }
            Mode::Script(script) => {
                if let Err(error) = script.check("Codex", RAISES) {
                    eprintln!("fake-codex: {error}");
                    return DRIFT_EXIT;
                }
                Engine::new(script).run().await
            }
        }
    })
}

struct Engine {
    out: Out<Stdout>,
    input: mpsc::UnboundedReceiver<Value>,
    eof: bool,
    steps: VecDeque<Step>,
    model: String,
    cwd: String,
    thread: Option<String>,
    /// The running turn's id.
    turn: Option<String>,
    /// `turn/start` requests waiting to run, with their input.
    starts: VecDeque<(Value, Value)>,
    interrupted: bool,
    /// Responses to our server requests, by id.
    answers: BTreeMap<String, Value>,
    next_request: u64,
    next_item: u64,
    last_message: Option<Value>,
}

impl Engine {
    fn new(script: Script) -> Self {
        let (tx, input) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match serde_json::from_str::<Value>(&line) {
                    Ok(value) => {
                        if tx.send(value).is_err() {
                            break;
                        }
                    }
                    Err(error) => eprintln!("fake-codex: unreadable input {line}: {error}"),
                }
            }
        });
        Self {
            out: Out::new(tokio::io::stdout()),
            input,
            eof: false,
            steps: script.steps.into(),
            model: script.model.unwrap_or_else(|| "gpt-fake".into()),
            cwd: std::env::current_dir()
                .map(|dir| dir.display().to_string())
                .unwrap_or_default(),
            thread: None,
            turn: None,
            starts: VecDeque::new(),
            interrupted: false,
            answers: BTreeMap::new(),
            next_request: 0,
            next_item: 0,
            last_message: None,
        }
    }

    async fn run(mut self) -> i32 {
        loop {
            if let Some((id, params)) = self.starts.pop_front() {
                if let Some(code) = self.run_turn(id, params).await {
                    return code;
                }
                continue;
            }
            if self.eof {
                return 0;
            }
            match self.input.recv().await {
                Some(frame) => self.handle(frame).await,
                None => self.eof = true,
            }
        }
    }

    async fn send(&mut self, frame: Value) {
        if self.out.send(&frame).await.is_err() {
            std::process::exit(0);
        }
    }

    async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "method": method, "params": params, "emittedAtMs": now_ms() }))
            .await;
    }

    async fn respond(&mut self, id: &Value, result: Value) {
        self.send(json!({ "id": id, "result": result })).await;
    }

    async fn refuse(&mut self, id: &Value, code: i64, message: &str) {
        self.send(json!({ "id": id, "error": { "code": code, "message": message } }))
            .await;
    }

    async fn drain(&mut self) {
        loop {
            match self.input.try_recv() {
                Ok(frame) => self.handle(frame).await,
                Err(mpsc::error::TryRecvError::Empty) => return,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.eof = true;
                    return;
                }
            }
        }
    }

    async fn pump(&mut self) {
        if self.eof {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            return;
        }
        tokio::select! {
            frame = self.input.recv() => match frame {
                Some(frame) => self.handle(frame).await,
                None => self.eof = true,
            },
            () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
    }

    fn thread_id(&self) -> String {
        self.thread.clone().unwrap_or_default()
    }

    async fn handle(&mut self, frame: Value) {
        let Some(method) = frame["method"].as_str().map(str::to_owned) else {
            // A response to one of our requests.
            if let Some(id) = frame.get("id") {
                self.answers.insert(key(id), frame.clone());
            }
            return;
        };
        let Some(id) = frame.get("id").cloned() else {
            // `initialized` and other notifications need nothing.
            return;
        };
        let params = &frame["params"];
        match method.as_str() {
            "initialize" => {
                let result = json!({
                    "codexHome": format!("{}/.codex", self.cwd),
                    "platformFamily": "unix",
                    "platformOs": std::env::consts::OS,
                    "userAgent": format!("fake-codex/{VERSION}"),
                });
                self.respond(&id, result).await;
            }
            "thread/start" | "thread/resume" => {
                if let Some(model) = params["model"].as_str() {
                    self.model = model.to_owned();
                }
                if let Some(cwd) = params["cwd"].as_str() {
                    self.cwd = cwd.to_owned();
                }
                let thread = params["threadId"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                self.thread = Some(thread.clone());
                let result = json!({
                    "activePermissionProfile": null,
                    "approvalPolicy": params["approvalPolicy"].as_str().unwrap_or("on-request"),
                    "approvalsReviewer": "user",
                    "cwd": self.cwd,
                    "disabledPluginIds": [],
                    "instructionSources": [],
                    "model": self.model,
                    "modelProvider": "openai",
                    "multiAgentMode": "explicitRequestOnly",
                    "reasoningEffort": null,
                    "runtimeWorkspaceRoots": [self.cwd],
                    "sandbox": {
                        "excludeSlashTmp": false,
                        "excludeTmpdirEnvVar": false,
                        "networkAccess": false,
                        "type": "workspaceWrite",
                        "writableRoots": [],
                    },
                    "serviceTier": null,
                    "thread": self.thread_value(),
                });
                self.respond(&id, result).await;
                if method == "thread/start" {
                    let thread = self.thread_value();
                    self.notify("thread/started", json!({ "thread": thread }))
                        .await;
                }
            }
            "turn/start" => {
                if self.turn.is_some() {
                    self.refuse(&id, -32600, "a turn is already running").await;
                } else {
                    self.starts.push_back((id, params.clone()));
                }
            }
            "turn/steer" => {
                let expected = params["expectedTurnId"].as_str();
                match self.turn.clone() {
                    Some(turn) if expected.is_none_or(|expected| expected == turn) => {
                        self.respond(&id, json!({ "turnId": turn })).await;
                        self.user_message(&turn, &params["input"]).await;
                    }
                    _ => self.refuse(&id, -32600, "no active turn to steer").await,
                }
            }
            "turn/interrupt" => {
                if self.turn.is_some() {
                    self.interrupted = true;
                }
                self.respond(&id, json!({})).await;
            }
            // Drained by a running turn, held for the next by an idle one;
            // either way nothing is reported.
            "thread/inject_items" => self.respond(&id, json!({})).await,
            "thread/compact/start" => {
                self.respond(&id, json!({})).await;
                self.compact().await;
            }
            "thread/name/set" => self.respond(&id, json!({})).await,
            "account/read" => {
                self.respond(
                    &id,
                    json!({ "account": { "type": "chatgpt", "planType": "pro" }, "requiresOpenaiAuth": true }),
                )
                .await
            }
            other => {
                self.refuse(&id, -32601, &format!("method not found: {other}"))
                    .await
            }
        }
    }

    fn thread_value(&self) -> Value {
        let now = now_ms() / 1000;
        let thread = self.thread_id();
        json!({
            "agentNickname": null,
            "agentRole": null,
            "canAcceptDirectInput": true,
            "cliVersion": VERSION,
            "createdAt": now,
            "cwd": self.cwd,
            "daybreakEnabled": null,
            "environments": [{
                "cwd": self.cwd,
                "environmentId": "local",
                "runtimeWorkspaceRoots": [self.cwd],
            }],
            "ephemeral": false,
            "extra": null,
            "forkedFromId": null,
            "gitInfo": null,
            "historyMode": "paginated",
            "id": thread,
            "model": self.model,
            "modelProvider": "openai",
            "name": null,
            "originator": "fake-codex",
            "parentThreadId": null,
            "path": format!("{}/.codex/sessions/{thread}.jsonl", self.cwd),
            "preview": "",
            "projectId": null,
            "reasoningEffort": null,
            "recencyAt": now,
            "section": null,
            "sectionEnteredAt": null,
            "sessionId": thread,
            "source": "vscode",
            "status": { "type": "idle" },
            "threadSource": null,
            "turns": [],
            "updatedAt": now,
        })
    }

    fn turn_value(&self, turn: &str, status: &str, items: Vec<Value>) -> Value {
        let done = status != "inProgress";
        json!({
            "completedAt": if done { json!(now_ms() / 1000) } else { Value::Null },
            "durationMs": if done { json!(1) } else { Value::Null },
            "error": null,
            "id": turn,
            "items": items,
            "itemsView": if done && status == "completed" { "summary" } else { "notLoaded" },
            "startedAt": now_ms() / 1000,
            "status": status,
        })
    }

    fn item_id(&mut self, prefix: &str) -> String {
        self.next_item += 1;
        format!("{prefix}{:024}", self.next_item)
    }

    async fn item(&mut self, turn: &str, phase: &str, item: &Value) {
        let thread = self.thread_id();
        let mut params = json!({ "item": item, "threadId": thread, "turnId": turn });
        if phase == "item/started" {
            params["startedAtMs"] = json!(now_ms());
        } else {
            params["completedAtMs"] = json!(now_ms());
        }
        self.notify(phase, params).await;
    }

    async fn user_message(&mut self, turn: &str, input: &Value) {
        let content: Vec<Value> = input
            .as_array()
            .into_iter()
            .flatten()
            .map(|part| {
                let mut part = part.clone();
                if part.get("text_elements").is_none() && part["type"] == "text" {
                    part["text_elements"] = json!([]);
                }
                part
            })
            .collect();
        if content.is_empty() {
            return;
        }
        let item = json!({
            "clientId": null,
            "content": content,
            "id": self.item_id("user_"),
            "type": "userMessage",
        });
        self.item(turn, "item/started", &item).await;
        self.item(turn, "item/completed", &item).await;
    }

    async fn status(&mut self, active: Option<&[&str]>) {
        let status = match active {
            Some(flags) => json!({ "type": "active", "activeFlags": flags }),
            None => json!({ "type": "idle" }),
        };
        let thread = self.thread_id();
        self.notify(
            "thread/status/changed",
            json!({ "status": status, "threadId": thread }),
        )
        .await;
    }

    async fn compact(&mut self) {
        let turn = uuid::Uuid::new_v4().to_string();
        let thread = self.thread_id();
        self.status(Some(&[])).await;
        let started = self.turn_value(&turn, "inProgress", vec![]);
        self.notify(
            "turn/started",
            json!({ "threadId": thread, "turn": started }),
        )
        .await;
        let item = json!({ "id": self.item_id("compact_"), "type": "contextCompaction" });
        self.item(&turn, "item/started", &item).await;
        self.item(&turn, "item/completed", &item).await;
        self.status(None).await;
        let mut done = self.turn_value(&turn, "completed", vec![]);
        done["itemsView"] = json!("notLoaded");
        self.notify(
            "turn/completed",
            json!({ "threadId": thread, "turn": done }),
        )
        .await;
    }

    /// Run a turn. `Some(code)` ends the process.
    async fn run_turn(&mut self, id: Value, params: Value) -> Option<i32> {
        let turn = uuid::Uuid::new_v4().to_string();
        let thread = self.thread_id();
        self.turn = Some(turn.clone());
        self.interrupted = false;
        self.last_message = None;
        let started = self.turn_value(&turn, "inProgress", vec![]);
        let mut response = started.clone();
        response["startedAt"] = Value::Null;
        self.respond(&id, json!({ "turn": response })).await;
        self.status(Some(&[])).await;
        self.notify(
            "turn/started",
            json!({ "threadId": thread, "turn": started }),
        )
        .await;
        self.user_message(&turn, &params["input"]).await;
        let exit = loop {
            self.drain().await;
            if self.interrupted {
                break None;
            }
            let Some(step) = self.steps.pop_front() else {
                break None;
            };
            match step {
                Step::Text { chunks } => self.text(&turn, &chunks).await,
                Step::Thinking { text } => self.reasoning(&turn, &text).await,
                Step::Tool(tool) => self.tool(&turn, &tool, None).await,
                Step::Ask(ask) => self.ask(&turn, ask).await,
                Step::WaitFor { path } => {
                    while !path.exists() && !self.interrupted {
                        self.pump().await;
                    }
                }
                Step::Pause { ms } => {
                    let until = tokio::time::Instant::now() + std::time::Duration::from_millis(ms);
                    while tokio::time::Instant::now() < until && !self.interrupted {
                        self.pump().await;
                    }
                }
                Step::TurnEnd => break None,
                Step::Exit { code } => break Some(code),
            }
        };
        if exit.is_some() {
            return exit;
        }
        let status = if self.interrupted {
            while let Some(step) = self.steps.pop_front() {
                if step == Step::TurnEnd {
                    break;
                }
            }
            "interrupted"
        } else {
            let usage = token_usage();
            self.notify(
                "thread/tokenUsage/updated",
                json!({ "threadId": thread, "turnId": turn, "tokenUsage": usage }),
            )
            .await;
            "completed"
        };
        self.status(None).await;
        let items = if status == "completed" {
            self.last_message.clone().into_iter().collect()
        } else {
            vec![]
        };
        let done = self.turn_value(&turn, status, items);
        self.notify(
            "turn/completed",
            json!({ "threadId": thread, "turn": done }),
        )
        .await;
        self.turn = None;
        None
    }

    /// Reasoning with its summary streamed as one part.
    async fn reasoning(&mut self, turn: &str, text: &str) {
        let id = self.item_id("rs_");
        let thread = self.thread_id();
        let mut item = json!({ "content": [], "id": id, "summary": [], "type": "reasoning" });
        self.item(turn, "item/started", &item).await;
        let at = json!({ "itemId": id, "summaryIndex": 0, "threadId": thread, "turnId": turn });
        self.notify("item/reasoning/summaryPartAdded", at.clone())
            .await;
        let mut delta = at;
        delta["delta"] = json!(text);
        self.notify("item/reasoning/summaryTextDelta", delta).await;
        item["summary"] = json!([text]);
        self.item(turn, "item/completed", &item).await;
    }

    async fn text(&mut self, turn: &str, chunks: &[String]) {
        let id = self.item_id("msg_");
        let thread = self.thread_id();
        let mut item = json!({
            "delivery": null,
            "id": id,
            "memoryCitation": null,
            "phase": "final_answer",
            "questions": null,
            "text": "",
            "type": "agentMessage",
        });
        self.item(turn, "item/started", &item).await;
        for chunk in chunks {
            self.notify(
                "item/agentMessage/delta",
                json!({ "delta": chunk, "itemId": id, "threadId": thread, "turnId": turn }),
            )
            .await;
        }
        item["text"] = json!(chunks.concat());
        self.item(turn, "item/completed", &item).await;
        self.last_message = Some(item);
    }

    /// Send a server request and wait for its answer, or the turn's end.
    async fn request(&mut self, method: &str, params: Value) -> Option<Value> {
        let id = self.next_request;
        self.next_request += 1;
        self.send(json!({ "id": id, "method": method, "params": params }))
            .await;
        let answer = loop {
            if let Some(answer) = self.answers.remove(&id.to_string()) {
                break Some(answer);
            }
            if self.interrupted || self.eof {
                break None;
            }
            self.pump().await;
        };
        let thread = self.thread_id();
        self.notify(
            "serverRequest/resolved",
            json!({ "requestId": id, "threadId": thread }),
        )
        .await;
        answer
    }

    /// A call: a command, or a file change when named `apply_patch`. With
    /// `approval` the call waits for the host's decision first.
    async fn tool(&mut self, turn: &str, tool: &Tool, approval: Option<()>) {
        let thread = self.thread_id();
        if tool.name.as_deref() == Some("apply_patch") {
            let id = self.item_id("patch_");
            let changes = tool.input.clone().unwrap_or_else(|| {
                json!([{
                    "diff": "added\n",
                    "kind": { "type": "add" },
                    "path": format!("{}/new.txt", self.cwd),
                }])
            });
            let mut item = json!({
                "changes": changes,
                "id": id,
                "status": "inProgress",
                "type": "fileChange",
            });
            self.item(turn, "item/started", &item).await;
            let mut declined = false;
            if approval.is_some() {
                let answer = self
                    .request(
                        "item/fileChange/requestApproval",
                        json!({
                            "grantRoot": null,
                            "itemId": id,
                            "reason": null,
                            "startedAtMs": now_ms(),
                            "threadId": thread,
                            "turnId": turn,
                        }),
                    )
                    .await;
                let Some(answer) = answer else { return };
                declined = !accepted(&answer);
            }
            item["status"] = json!(if declined {
                "declined"
            } else if tool.outcome.error {
                "failed"
            } else {
                "completed"
            });
            self.item(turn, "item/completed", &item).await;
            return;
        }
        let id = self.item_id("exec-");
        let command = tool
            .input
            .as_ref()
            .and_then(|input| input["command"].as_str().map(str::to_owned))
            .unwrap_or_else(|| {
                if tool.is_exploration() {
                    "cat README.md".into()
                } else {
                    "true".into()
                }
            });
        let action = if tool.is_exploration() {
            json!({ "command": command, "name": "README.md", "path": format!("{}/README.md", self.cwd), "type": "read" })
        } else {
            json!({ "command": command, "type": "unknown" })
        };
        let mut item = json!({
            "aggregatedOutput": null,
            "command": format!("/bin/zsh -lc '{command}'"),
            "commandActions": [action],
            "cwd": self.cwd,
            "durationMs": null,
            "exitCode": null,
            "id": id,
            "pluginId": null,
            "processId": null,
            "scriptPath": null,
            "source": "agent",
            "status": "inProgress",
            "type": "commandExecution",
        });
        self.item(turn, "item/started", &item).await;
        if approval.is_some() {
            self.status(Some(&["waitingOnApproval"])).await;
            let answer = self
                .request(
                    "item/commandExecution/requestApproval",
                    json!({
                        "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"],
                        "command": item["command"],
                        "commandActions": item["commandActions"],
                        "cwd": self.cwd,
                        "environmentId": "local",
                        "itemId": id,
                        "kind": "command",
                        "proposedExecpolicyAmendment": command.split_whitespace().collect::<Vec<_>>(),
                        "reason": "The script asks before running this.",
                        "startedAtMs": now_ms(),
                        "threadId": thread,
                        "turnId": turn,
                    }),
                )
                .await;
            let Some(answer) = answer else { return };
            self.status(Some(&[])).await;
            if !accepted(&answer) {
                item["status"] = json!("declined");
                self.item(turn, "item/completed", &item).await;
                return;
            }
        }
        let output = tool.outcome.output.clone();
        if !output.is_empty() {
            self.notify(
                "item/commandExecution/outputDelta",
                json!({ "delta": output, "itemId": id, "threadId": thread, "turnId": turn }),
            )
            .await;
        }
        item["aggregatedOutput"] = json!(output);
        item["durationMs"] = json!(0);
        item["exitCode"] = json!(if tool.outcome.error { 1 } else { 0 });
        item["status"] = json!(if tool.outcome.error {
            "failed"
        } else {
            "completed"
        });
        self.item(turn, "item/completed", &item).await;
    }

    async fn ask(&mut self, turn: &str, ask: Ask) {
        let thread = self.thread_id();
        match ask {
            Ask::Permission(tool) => self.tool(turn, &tool, Some(())).await,
            Ask::Question { questions } => {
                let item = self.item_id("call_");
                let _ = self
                    .request(
                        "item/tool/requestUserInput",
                        json!({
                            "autoResolutionMs": null,
                            "isBlocking": true,
                            "itemId": item,
                            "questions": questions.iter().enumerate().map(|(index, q)| question(index, q)).collect::<Vec<_>>(),
                            "threadId": thread,
                            "turnId": turn,
                        }),
                    )
                    .await;
            }
            Ask::Grant { reason, paths } => {
                let item = self.item_id("exec-");
                let permissions = json!({
                    "fileSystem": if paths.is_empty() { Value::Null } else { json!({ "write": paths }) },
                    "network": if paths.is_empty() { json!({ "enabled": true }) } else { Value::Null },
                });
                let _ = self
                    .request(
                        "item/permissions/requestApproval",
                        json!({
                            "cwd": self.cwd,
                            "environmentId": "local",
                            "itemId": item,
                            "permissions": permissions,
                            "reason": reason,
                            "startedAtMs": now_ms(),
                            "threadId": thread,
                            "turnId": turn,
                        }),
                    )
                    .await;
            }
            Ask::Form {
                server,
                message,
                schema,
            } => {
                let params = json!({
                    "_meta": null,
                    "message": message,
                    "mode": "form",
                    "requestedSchema": schema,
                    "serverName": server,
                    "threadId": thread,
                    "turnId": turn,
                });
                self.elicit(turn, &server, params).await;
            }
            Ask::Link {
                server,
                message,
                url,
            } => {
                let params = json!({
                    "_meta": null,
                    "elicitationId": self.item_id("elicit_"),
                    "message": message,
                    "mode": "url",
                    "serverName": server,
                    "threadId": thread,
                    "turnId": turn,
                    "url": url,
                });
                self.elicit(turn, &server, params).await;
            }
            Ask::Plan { .. } => unreachable!("refused when the script loaded"),
        }
    }

    /// A tool-server call that asks the host through its server.
    async fn elicit(&mut self, turn: &str, server: &str, params: Value) {
        let mut item = json!({
            "appContext": null,
            "arguments": {},
            "durationMs": null,
            "error": null,
            "id": self.item_id("exec-"),
            "mcpAppUi": null,
            "pluginId": null,
            "readOnlyHint": null,
            "result": null,
            "server": server,
            "status": "inProgress",
            "tool": "ask",
            "type": "mcpToolCall",
        });
        self.item(turn, "item/started", &item).await;
        let answer = self.request("mcpServer/elicitation/request", params).await;
        let Some(answer) = answer else { return };
        let action = answer["result"]["action"].as_str().unwrap_or("cancel");
        let mut said = format!("elicitation {action}");
        if let Some(content) = answer["result"].get("content") {
            said.push(' ');
            said.push_str(&content.to_string());
        }
        item["durationMs"] = json!(0);
        item["result"] = json!({
            "_meta": null,
            "content": [{ "text": said, "type": "text" }],
            "structuredContent": null,
        });
        item["status"] = json!("completed");
        self.item(turn, "item/completed", &item).await;
    }
}

fn key(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn accepted(answer: &Value) -> bool {
    matches!(
        answer["result"]["decision"].as_str(),
        Some("accept" | "acceptForSession")
    ) || answer["result"]["decision"].is_object()
}

fn question(index: usize, question: &Question) -> Value {
    json!({
        "header": question.header,
        "id": format!("q{index}"),
        "isOther": false,
        "isSecret": false,
        "options": question.options.iter().map(|label| json!({
            "description": label,
            "label": label,
        })).collect::<Vec<_>>(),
        "question": question.question,
    })
}

fn token_usage() -> Value {
    let tally = json!({
        "cachedInputTokens": 0,
        "inputTokens": 1,
        "outputTokens": 1,
        "reasoningOutputTokens": 0,
        "totalTokens": 2,
    });
    json!({ "last": tally, "total": tally, "modelContextWindow": 258400 })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

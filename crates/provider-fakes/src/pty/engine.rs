//! The scripted terminal session.
//!
//! Keys arrive on the terminal as the claude-2.1 keymap types them: a
//! prompt is a bracketed paste then Enter; Escape interrupts; Shift+Tab
//! cycles the permission mode; an open permission or plan menu takes the
//! entry's digit; a question form takes digits, arrows, Space, Tab, typed
//! text and Enter. What Claude would show, the fake reports the way Claude
//! does to a host that reads no screen: rows appended to the session
//! transcript and payloads handed to the hook commands `--settings` names
//! (only the hook events the corpus records). A prompt submitted while a
//! turn runs is queued and folded into that turn at its next tool
//! boundary; Ctrl+X Ctrl+S sends it now, cutting the turn short. A message
//! written to the messaging socket runs like a prompt from a peer.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{Args, append_row, raw_mode, run_hooks};
use crate::claude::{Ids, timestamp, uuid};
use crate::playback::{claude_config_dir, transcript_path};
use crate::script::{Ask, Question, Script, Step, Tool};
use crate::sdk::{claude_tool, sidecar, turn_usage};
use crate::{DRIFT_EXIT, ToolClass};

/// The asks terminal Claude raises.
pub const RAISES: &[&str] = &["permission", "question", "plan"];

/// What the terminal delivered, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    Paste(String),
    Char(char),
    Enter,
    Escape,
    Tab,
    Space,
    Down,
    Up,
    ShiftTab,
    SendNow,
}

enum In {
    Key(Key),
    /// A message from the messaging socket.
    Peer(String),
    Closed,
}

pub async fn run(script: Script, args: Args) -> i32 {
    if let Err(error) = script.check("terminal Claude", RAISES) {
        eprintln!("fake-claude-pty: {error}");
        return DRIFT_EXIT;
    }
    raw_mode();
    let (tx, rx) = mpsc::unbounded_channel();
    spawn_keys(tx.clone());
    spawn_signals(tx.clone());
    let mut messaging = None;
    if let Some(path) = &args.messaging_socket {
        match serve_messaging(path.clone(), tx) {
            Ok(credentials) => messaging = Some(credentials),
            Err(error) => {
                eprintln!(
                    "fake-claude-pty: messaging socket {}: {error}",
                    path.display()
                );
                return DRIFT_EXIT;
            }
        }
    }
    Engine::new(script, args, rx, messaging).run().await
}

/// A terminal hang-up or termination ends the session the way closing it
/// does: the running step finishes, SessionEnd runs, the process exits 0.
fn spawn_signals(tx: mpsc::UnboundedSender<In>) {
    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut hup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
        ) else {
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = hup.recv() => {}
        }
        let _ = tx.send(In::Closed);
    });
    #[cfg(not(unix))]
    let _ = tx;
}

fn spawn_keys(tx: mpsc::UnboundedSender<In>) {
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut pending: Vec<u8> = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(In::Closed);
                    return;
                }
                Ok(read) => read,
            };
            pending.extend_from_slice(&buffer[..read]);
            for key in decode(&mut pending) {
                if tx.send(In::Key(key)).is_err() {
                    return;
                }
            }
        }
    });
}

const PASTE_BEGIN: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Decode whole keys from the front of `bytes`, leaving an unfinished tail.
fn decode(bytes: &mut Vec<u8>) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        if rest.starts_with(PASTE_BEGIN) {
            let body = &rest[PASTE_BEGIN.len()..];
            let Some(end) = body.windows(PASTE_END.len()).position(|w| w == PASTE_END) else {
                break;
            };
            keys.push(Key::Paste(
                String::from_utf8_lossy(&body[..end]).into_owned(),
            ));
            at += PASTE_BEGIN.len() + end + PASTE_END.len();
            continue;
        }
        let (key, used) = match rest {
            [0x1b, b'[', b'B', ..] => (Some(Key::Down), 3),
            [0x1b, b'[', b'A', ..] => (Some(Key::Up), 3),
            [0x1b, b'[', b'Z', ..] => (Some(Key::ShiftTab), 3),
            // A partial sequence waits for the rest.
            [0x1b, b'[', ..] if rest.len() < 3 || rest.starts_with(b"\x1b[2") => {
                if rest.len() < PASTE_BEGIN.len() {
                    break;
                }
                (None, 1)
            }
            [0x1b, ..] => (Some(Key::Escape), 1),
            [b'\r', ..] | [b'\n', ..] => (Some(Key::Enter), 1),
            [b'\t', ..] => (Some(Key::Tab), 1),
            [b' ', ..] => (Some(Key::Space), 1),
            [0x18, 0x13, ..] => (Some(Key::SendNow), 2),
            [0x18] => break,
            _ => {
                let text = String::from_utf8_lossy(rest);
                match text.chars().next() {
                    Some(c) if !c.is_control() => (Some(Key::Char(c)), c.len_utf8()),
                    _ => (None, 1),
                }
            }
        };
        if let Some(key) = key {
            keys.push(key);
        }
        at += used;
    }
    bytes.drain(..at);
    keys
}

#[derive(Clone)]
struct Messaging {
    socket: PathBuf,
    token: String,
}

/// Bind the messaging socket and hand each message to the engine.
fn serve_messaging(path: PathBuf, tx: mpsc::UnboundedSender<In>) -> std::io::Result<Messaging> {
    #[cfg(unix)]
    {
        use std::io::BufRead;
        use std::os::unix::fs::PermissionsExt;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let expected = token.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut lines = std::io::BufReader::new(stream).lines();
                let auth: Option<Value> = lines
                    .next()
                    .and_then(Result::ok)
                    .and_then(|line| serde_json::from_str(&line).ok());
                if auth.as_ref().and_then(|auth| auth["token"].as_str()) != Some(expected.as_str())
                {
                    continue;
                }
                for line in lines.map_while(Result::ok) {
                    let Ok(message) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    let text = message["message"]["content"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    if tx.send(In::Peer(text)).is_err() {
                        return;
                    }
                }
            }
        });
        Ok(Messaging {
            socket: path,
            token,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (path, tx);
        Err(std::io::Error::other("messaging sockets need Unix"))
    }
}

/// A prompt waiting for a turn, or to fold into the running one.
struct Queued {
    text: String,
    peer: bool,
}

/// Why a turn stopped before its steps did.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cut {
    Interrupted,
    Denied,
    SentNow,
}

struct Engine {
    input: mpsc::UnboundedReceiver<In>,
    closed: bool,
    steps: VecDeque<Step>,
    args: Args,
    messaging: Option<Messaging>,
    session: String,
    model: String,
    mode: String,
    cwd: String,
    transcript: PathBuf,
    ids: Ids,
    composer: String,
    queue: VecDeque<Queued>,
    busy: bool,
    cut: Option<Cut>,
    /// The last row written, for `parentUuid`.
    parent: Option<String>,
    prompt_id: String,
    last_text: String,
}

impl Engine {
    fn new(
        script: Script,
        args: Args,
        input: mpsc::UnboundedReceiver<In>,
        messaging: Option<Messaging>,
    ) -> Self {
        let session = args
            .resume
            .clone()
            .or_else(|| args.session_id.clone())
            .unwrap_or_else(uuid);
        let cwd = std::env::current_dir().unwrap_or_default();
        let transcript = transcript_path(&claude_config_dir(), &cwd, &session);
        Self {
            input,
            closed: false,
            steps: script.steps.into(),
            model: args
                .model
                .clone()
                .or(script.model)
                .unwrap_or_else(|| "claude-fake-1".into()),
            mode: args
                .permission_mode
                .clone()
                .unwrap_or_else(|| "default".into()),
            args,
            messaging,
            session,
            cwd: cwd.display().to_string(),
            transcript,
            ids: Ids::default(),
            composer: String::new(),
            queue: VecDeque::new(),
            busy: false,
            cut: None,
            parent: None,
            prompt_id: uuid(),
            last_text: String::new(),
        }
    }

    async fn run(mut self) -> i32 {
        let source = if self.args.resume.is_some() {
            "resume"
        } else {
            "startup"
        };
        self.hook(json!({
            "hook_event_name": "SessionStart",
            "model": self.model,
            "source": source,
        }));
        self.row(json!({
            "type": "permission-mode",
            "permissionMode": self.mode,
            "sessionId": self.session,
        }));
        self.screen("Claude Code (scripted)");
        loop {
            if !self.busy
                && let Some(next) = self.queue.pop_front()
            {
                if let Some(code) = self.turn(next).await {
                    return code;
                }
                continue;
            }
            if self.closed {
                self.hook(json!({ "hook_event_name": "SessionEnd", "reason": "other" }));
                return 0;
            }
            self.wait().await;
        }
    }

    /// Wait for the next input and act on it.
    async fn wait(&mut self) {
        match self.input.recv().await {
            Some(input) => self.handle(input),
            None => self.closed = true,
        }
    }

    /// Wait for input or a moment, whichever is first.
    async fn pump(&mut self) {
        tokio::select! {
            input = self.input.recv() => match input {
                Some(input) => self.handle(input),
                None => self.closed = true,
            },
            () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
    }

    fn drain(&mut self) {
        while let Ok(input) = self.input.try_recv() {
            self.handle(input);
        }
    }

    /// Composer keys: typing, submitting, interrupting, sending now.
    fn handle(&mut self, input: In) {
        match input {
            In::Closed => self.closed = true,
            In::Peer(text) => self.submit(Queued { text, peer: true }),
            In::Key(key) => match key {
                Key::Paste(text) => self.composer.push_str(&text),
                Key::Char(c) => self.composer.push(c),
                Key::Space => self.composer.push(' '),
                Key::Enter => {
                    let text = std::mem::take(&mut self.composer);
                    if !text.trim().is_empty() {
                        self.submit(Queued { text, peer: false });
                    }
                }
                Key::Escape => {
                    if self.busy {
                        self.cut = Some(Cut::Interrupted);
                    } else {
                        self.composer.clear();
                    }
                }
                Key::ShiftTab => {
                    self.mode = next_mode(&self.mode).to_owned();
                    let row = json!({
                        "type": "permission-mode",
                        "permissionMode": self.mode,
                        "sessionId": self.session,
                    });
                    self.row(row);
                }
                Key::SendNow => {
                    if self.busy && !self.queue.is_empty() {
                        self.cut = Some(Cut::SentNow);
                    }
                }
                Key::Tab | Key::Down | Key::Up => {}
            },
        }
    }

    fn submit(&mut self, queued: Queued) {
        if self.busy {
            let row = json!({
                "type": "queue-operation",
                "operation": "enqueue",
                "content": queued.text,
                "sessionId": self.session,
                "timestamp": timestamp(),
            });
            self.row(row);
        }
        self.queue.push_back(queued);
    }

    fn screen(&self, text: &str) {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(text.replace('\n', "\r\n").as_bytes());
        let _ = stdout.write_all(b"\r\n");
        let _ = stdout.flush();
    }

    /// Append a row. Conversation rows carry the envelope Claude puts on
    /// them and chain through `parentUuid`.
    fn row(&mut self, row: Value) {
        let bytes = serde_json::to_vec(&row).expect("rows serialise");
        if let Err(error) = append_row(&self.transcript, &bytes) {
            eprintln!("fake-claude-pty: transcript: {error}");
        }
        if let Some(uuid) = row["uuid"].as_str() {
            self.parent = Some(uuid.to_owned());
        }
    }

    fn envelope(&self, mut row: Value) -> Value {
        let id = uuid();
        for (key, value) in [
            ("cwd", json!(self.cwd)),
            ("entrypoint", json!("cli")),
            ("gitBranch", json!("HEAD")),
            ("isSidechain", json!(false)),
            ("parentUuid", json!(self.parent)),
            ("sessionId", json!(self.session)),
            ("timestamp", json!(timestamp())),
            ("userType", json!("external")),
            ("uuid", json!(id)),
            ("version", json!(crate::claude::VERSION)),
        ] {
            row[key] = value;
        }
        row
    }

    fn hook(&self, mut payload: Value) {
        payload["cwd"] = json!(self.cwd);
        payload["session_id"] = json!(self.session);
        payload["transcript_path"] = json!(self.transcript.display().to_string());
        let event = payload["hook_event_name"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let event = event.as_str();
        if !matches!(event, "SessionStart" | "SessionEnd") {
            payload["permission_mode"] = json!(self.mode);
            payload["prompt_id"] = json!(self.prompt_id);
        }
        if matches!(event, "SessionEnd") {
            payload["prompt_id"] = json!(self.prompt_id);
        }
        if matches!(
            event,
            "SessionStart" | "PreToolUse" | "PostToolUse" | "PermissionRequest" | "Stop"
        ) {
            payload["scratchpad_dir"] = json!(format!("{}/.scratchpad", self.cwd));
        }
        let mut env = Vec::new();
        if let Some(messaging) = &self.messaging {
            env.push((
                "CLAUDE_CODE_MESSAGING_SOCKET".to_owned(),
                messaging.socket.display().to_string(),
            ));
            env.push((
                "CLAUDE_CODE_MESSAGING_TOKEN".to_owned(),
                messaging.token.clone(),
            ));
        }
        let bytes = serde_json::to_vec(&payload).expect("payloads serialise");
        if let Err(error) = run_hooks(&self.args, &bytes, &env) {
            eprintln!("fake-claude-pty: hook: {error}");
        }
    }

    fn user_row(&mut self, queued: &Queued) {
        let origin = if queued.peer {
            // No terminal recording shows a socket message yet; headless
            // Claude marks its reflection this way.
            json!({ "kind": "peer" })
        } else {
            json!({ "kind": "human" })
        };
        let row = self.envelope(json!({
            "type": "user",
            "message": { "role": "user", "content": queued.text },
            "origin": origin,
            "permissionMode": self.mode,
            "promptId": self.prompt_id,
            "promptSource": if queued.peer { "peer" } else { "typed" },
            "turnOrigin": if queued.peer { "peer" } else { "human" },
        }));
        self.row(row);
    }

    async fn turn(&mut self, first: Queued) -> Option<i32> {
        self.busy = true;
        self.cut = None;
        self.prompt_id = uuid();
        self.last_text.clear();
        self.screen(&format!("> {}", first.text));
        self.user_row(&first);
        let request = self.ids.next("req_fake");
        let message = self.ids.next("msg_fake");
        let exit = loop {
            self.drain();
            if self.cut.is_some() {
                break None;
            }
            let Some(step) = self.steps.pop_front() else {
                break None;
            };
            match step {
                Step::Text { chunks } => {
                    let text = chunks.concat();
                    self.screen(&text);
                    self.assistant(
                        &request,
                        &message,
                        json!({ "type": "text", "text": text }),
                        "end_turn",
                    );
                    self.last_text = text;
                }
                Step::Thinking { text } => {
                    let block = json!({ "type": "thinking", "thinking": text, "signature": "" });
                    self.assistant(&request, &message, block, "tool_use");
                }
                Step::Tool(tool) => {
                    let (id, name, input) = self.tool_use(&request, &message, &tool);
                    self.finish_tool(&id, &name, &input, &tool, Ok(tool.outcome.output.clone()));
                    self.fold();
                }
                Step::Ask(ask) => {
                    self.ask(&request, &message, ask).await;
                    if self.cut.is_none() {
                        self.fold();
                    }
                }
                Step::WaitFor { path } => {
                    while !path.exists() && self.cut.is_none() && !self.closed {
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
        if let Some(cut) = self.cut {
            while let Some(step) = self.steps.pop_front() {
                if step == Step::TurnEnd {
                    break;
                }
            }
            if cut != Cut::Denied {
                let row = self.envelope(json!({
                    "type": "user",
                    "message": { "role": "user", "content": [{ "type": "text", "text": "[Request interrupted by user]" }] },
                    "promptId": self.prompt_id,
                    "session_id": self.session,
                }));
                self.row(row);
            }
        }
        self.hook(json!({
            "hook_event_name": "Stop",
            "background_tasks": [],
            "last_assistant_message": self.last_text,
            "session_crons": [],
            "stop_hook_active": false,
        }));
        self.busy = false;
        None
    }

    fn assistant(&mut self, request: &str, message: &str, block: Value, stop: &str) {
        self.assistant_with(request, message, block, stop, Value::Null);
    }

    /// An assistant row; a tool call's row also carries its input by id.
    fn assistant_with(
        &mut self,
        request: &str,
        message: &str,
        block: Value,
        stop: &str,
        wire_inputs: Value,
    ) {
        let mut row = self.envelope(json!({
            "type": "assistant",
            "apiBlockIndex": 0,
            "message": {
                "container": null,
                "content": [block],
                "context_management": null,
                "diagnostics": null,
                "id": message,
                "input_transformations": [],
                "model": self.model,
                "role": "assistant",
                "stop_details": null,
                "stop_reason": stop,
                "stop_sequence": null,
                "type": "message",
                "usage": turn_usage(),
            },
            "perTurnEffort": null,
            "requestId": request,
            "session_id": self.session,
        }));
        if !wire_inputs.is_null() {
            row["wireToolInputs"] = wire_inputs;
        }
        self.row(row);
    }

    /// Announce a call: its PreToolUse hook, then its row.
    fn tool_use(&mut self, request: &str, message: &str, tool: &Tool) -> (String, String, Value) {
        let (name, input) = claude_tool(tool, &self.cwd);
        let id = self.ids.next("toolu_fake");
        self.hook(json!({
            "hook_event_name": "PreToolUse",
            "tool_input": input,
            "tool_name": name,
            "tool_use_id": id,
        }));
        let block = json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
            "caller": { "type": "direct" },
        });
        self.assistant_with(
            request,
            message,
            block,
            "tool_use",
            json!({ id.clone(): input.clone() }),
        );
        self.screen(&format!("● {name}"));
        (id, name, input)
    }

    /// The call's result row, and its PostToolUse hook when it ran.
    fn finish_tool(
        &mut self,
        id: &str,
        name: &str,
        input: &Value,
        tool: &Tool,
        outcome: Result<String, String>,
    ) {
        let (content, is_error, result) = match outcome {
            Ok(output) if tool.outcome.error => {
                (output.clone(), true, json!(format!("Error: {output}")))
            }
            Ok(output) => {
                let result = sidecar(name, input, &output);
                (output, false, result)
            }
            Err(refusal) => (refusal.clone(), true, json!(format!("Error: {refusal}"))),
        };
        self.result_row(id, json!(content), is_error, result.clone());
        if !is_error {
            self.hook(json!({
                "hook_event_name": "PostToolUse",
                "duration_ms": 1,
                "tool_input": input,
                "tool_name": name,
                "tool_response": result,
                "tool_use_id": id,
            }));
        }
    }

    fn result_row(&mut self, id: &str, content: Value, is_error: bool, result: Value) {
        let source = self.parent.clone();
        let row = self.envelope(json!({
            "type": "user",
            "message": { "role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": id,
                "content": content,
                "is_error": is_error,
            }] },
            "promptId": self.prompt_id,
            "session_id": self.session,
            "sourceToolAssistantUUID": source,
            "toolUseResult": result,
        }));
        self.row(row);
    }

    /// Fold prompts queued since the last boundary into the running turn.
    fn fold(&mut self) {
        self.drain();
        while let Some(queued) = self.queue.pop_front() {
            let row = self.envelope(json!({
                "type": "attachment",
                "attachment": {
                    "type": "queued_command",
                    "commandMode": "prompt",
                    "prompt": queued.text,
                    "source_uuid": uuid(),
                    "timestamp": timestamp(),
                },
                "rendered": [{ "content": format!("<system-reminder>\nThe user sent a new message while you were working:\n{}\n</system-reminder>", queued.text) }],
                "session_id": self.session,
            }));
            self.row(row);
        }
    }

    /// Wait for a menu or form's keys, or for the turn to be cut.
    async fn key(&mut self) -> Option<Key> {
        loop {
            if self.closed {
                return None;
            }
            match self.input.recv().await {
                Some(In::Key(Key::Escape)) => {
                    self.cut = Some(Cut::Interrupted);
                    return None;
                }
                Some(In::Key(key)) => return Some(key),
                Some(other) => self.handle(other),
                None => {
                    self.closed = true;
                    return None;
                }
            }
        }
    }

    /// Typed text up to Enter.
    async fn line(&mut self) -> Option<String> {
        let mut text = String::new();
        loop {
            match self.key().await? {
                Key::Enter => return Some(text),
                Key::Char(c) => text.push(c),
                Key::Space => text.push(' '),
                Key::Paste(pasted) => text.push_str(&pasted),
                _ => {}
            }
        }
    }

    async fn ask(&mut self, request: &str, message: &str, ask: Ask) {
        match ask {
            Ask::Permission(tool) => {
                let (id, name, input) = self.tool_use(request, message, &tool);
                self.hook(json!({
                    "hook_event_name": "PermissionRequest",
                    "permission_suggestions": [{
                        "type": "addDirectories",
                        "destination": "session",
                        "directories": [self.cwd],
                    }],
                    "tool_input": input,
                    "tool_name": name,
                }));
                self.screen("Do you want to proceed?\n1. Yes\n2. Yes, and don't ask again\n3. No");
                let choice = loop {
                    match self.key().await {
                        None => return self.abandon(&id, &name, &input, &tool),
                        Some(Key::Char(c @ '1'..='3')) => break c,
                        Some(_) => {}
                    }
                };
                if choice == '3' {
                    let refusal = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.".to_owned();
                    self.finish_tool(&id, &name, &input, &tool, Err(refusal));
                    // Terminal Claude ends the turn on a deny.
                    self.cut = Some(Cut::Denied);
                    let row = self.envelope(json!({
                        "type": "user",
                        "message": { "role": "user", "content": [{ "type": "text", "text": "[Request interrupted by user for tool use]" }] },
                        "promptId": self.prompt_id,
                        "session_id": self.session,
                    }));
                    self.row(row);
                    return;
                }
                self.finish_tool(&id, &name, &input, &tool, Ok(tool.outcome.output.clone()));
            }
            Ask::Plan { markdown } => {
                let tool = named("ExitPlanMode", json!({ "plan": markdown }));
                let (id, name, input) = self.tool_use(request, message, &tool);
                self.hook(json!({
                    "hook_event_name": "PermissionRequest",
                    "permission_suggestions": [],
                    "tool_input": input,
                    "tool_name": name,
                }));
                self.screen(&format!("{markdown}\n1. Yes, auto-accept edits\n2. Yes, manually approve edits\n3. No, keep planning"));
                let choice = loop {
                    match self.key().await {
                        None => return self.abandon(&id, &name, &input, &tool),
                        Some(Key::Char(c @ '1'..='3')) => break c,
                        Some(_) => {}
                    }
                };
                if choice == '3' {
                    let Some(feedback) = self.line().await else {
                        return self.abandon(&id, &name, &input, &tool);
                    };
                    let refusal = format!(
                        "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said:\n{feedback}"
                    );
                    self.finish_tool(&id, &name, &input, &tool, Err(refusal));
                    return;
                }
                self.mode = if choice == '1' {
                    "acceptEdits"
                } else {
                    "default"
                }
                .to_owned();
                let content = format!(
                    "User has approved your plan. You can now start coding.\n\n## Approved Plan:\n{markdown}"
                );
                self.result_row(
                    &id,
                    json!(content),
                    false,
                    json!({ "plan": markdown, "isAgent": false }),
                );
            }
            Ask::Question { questions } => {
                let input =
                    json!({ "questions": questions.iter().map(question).collect::<Vec<_>>() });
                let tool = named("AskUserQuestion", input);
                let (id, name, input) = self.tool_use(request, message, &tool);
                self.hook(json!({
                    "hook_event_name": "PermissionRequest",
                    "permission_suggestions": [],
                    "tool_input": input,
                    "tool_name": name,
                }));
                let Some(answers) = self.form(&questions).await else {
                    return self.abandon(&id, &name, &input, &tool);
                };
                let said = questions
                    .iter()
                    .zip(&answers)
                    .map(|(q, answer)| format!("\"{}\"=\"{answer}\"", q.question))
                    .collect::<Vec<_>>()
                    .join(", ");
                let content = format!(
                    "User has answered your questions: {said}. You can now continue with the user's answers in mind."
                );
                let answers: serde_json::Map<String, Value> = questions
                    .iter()
                    .zip(answers)
                    .map(|(q, answer)| (q.question.clone(), json!(answer)))
                    .collect();
                let result = json!({ "questions": input["questions"], "answers": answers });
                self.result_row(&id, json!(content), false, result);
            }
            Ask::Form { .. } | Ask::Link { .. } | Ask::Grant { .. } => {
                unreachable!("refused when the script loaded")
            }
        }
    }

    /// The call a cut-short ask leaves: rejected, the turn over.
    fn abandon(&mut self, id: &str, name: &str, input: &Value, tool: &Tool) {
        let refusal = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.".to_owned();
        self.finish_tool(id, name, input, tool, Err(refusal));
    }

    /// Fill in a question form from the keys the claude-2.1 keymap types,
    /// returning one answer per question (labels joined by ", ").
    async fn form(&mut self, questions: &[Question]) -> Option<Vec<String>> {
        let mut answers = Vec::new();
        for q in questions {
            let other_row = q.options.len() + 1;
            if q.multi_select {
                let mut cursor = 1;
                let mut picked = vec![false; q.options.len()];
                let mut other: Option<String> = None;
                let mut other_on = false;
                loop {
                    match self.key().await? {
                        Key::Down => cursor = (cursor + 1).min(other_row),
                        Key::Up => cursor = cursor.saturating_sub(1).max(1),
                        Key::Space if cursor == other_row => other_on = !other_on,
                        Key::Space => picked[cursor - 1] = !picked[cursor - 1],
                        Key::Enter if cursor == other_row => other = Some(self.line().await?),
                        Key::Tab => break,
                        _ => {}
                    }
                }
                let mut labels: Vec<String> = q
                    .options
                    .iter()
                    .zip(&picked)
                    .filter(|(_, on)| **on)
                    .map(|(label, _)| label.clone())
                    .collect();
                if other_on && let Some(other) = other {
                    labels.push(other);
                }
                answers.push(labels.join(", "));
            } else {
                let digit = loop {
                    if let Key::Char(c) = self.key().await?
                        && let Some(digit) = c.to_digit(10)
                    {
                        break digit as usize;
                    }
                };
                if digit == other_row {
                    answers.push(self.line().await?);
                } else if (1..other_row).contains(&digit) {
                    answers.push(q.options[digit - 1].clone());
                } else {
                    answers.push(String::new());
                }
            }
        }
        let single = questions.len() == 1 && !questions[0].multi_select;
        let enters = match questions.last() {
            _ if single => 0,
            Some(last) if last.multi_select => 2,
            _ => 1,
        };
        for _ in 0..enters {
            while self.key().await? != Key::Enter {}
        }
        Some(answers)
    }
}

fn named(name: &str, input: Value) -> Tool {
    Tool {
        name: Some(name.to_owned()),
        class: ToolClass::Consequential,
        input: Some(input),
        outcome: Default::default(),
    }
}

fn question(question: &Question) -> Value {
    json!({
        "question": question.question,
        "header": question.header,
        "multiSelect": question.multi_select,
        "options": question.options.iter().map(|label| json!({
            "label": label,
            "description": label,
        })).collect::<Vec<_>>(),
    })
}

fn next_mode(mode: &str) -> &'static str {
    match mode {
        "default" => "acceptEdits",
        "acceptEdits" => "plan",
        _ => "default",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_decode_as_the_keymap_types_them() {
        let mut bytes = b"\x1b[200~hi there\x1b[201~\r1\x1b\x1b[B \t\x1b[Z\x18\x13x".to_vec();
        assert_eq!(
            decode(&mut bytes),
            [
                Key::Paste("hi there".into()),
                Key::Enter,
                Key::Char('1'),
                Key::Escape,
                Key::Down,
                Key::Space,
                Key::Tab,
                Key::ShiftTab,
                Key::SendNow,
                Key::Char('x'),
            ]
        );
        assert!(bytes.is_empty());
        let mut partial = b"\x1b[200~unfinished".to_vec();
        assert!(decode(&mut partial).is_empty());
        assert_eq!(partial, b"\x1b[200~unfinished");
    }
}

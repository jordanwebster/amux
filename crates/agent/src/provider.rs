//! The provider child: how it is started for each kind, how its output
//! becomes facts, and how the interpreter's effects reach it.
//!
//! Headless Claude is a plain child speaking stream-JSON on stdin and
//! stdout. Claude in a terminal runs on a pseudo-terminal whose raw bytes
//! go to the terminal log; its facts are hook payloads (on
//! private/hooks.sock) and its transcript rows. Every child runs in its own
//! process group so a kill reaches whatever it started.
//!
//! The child's exit is what ends the agent, never the end of its output: a
//! straggler the provider left behind can hold a terminal or a pipe open
//! forever, so after the exit the output still buffered is collected for a
//! moment and the reader is then abandoned, not joined.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use interpret::{Channel, Effect, Fact};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{mpsc, oneshot};
use wire::{AgentSpec, Attachment};

use crate::dir;
use crate::terminal::{self, Keys};

/// How long output still in flight is collected after the child exits.
const TRAILING_OUTPUT: Duration = Duration::from_millis(300);
/// The request ids of the agent's own Codex handshake; the interpreter's
/// requests are numbered amux-N.
const CODEX_INITIALIZE: &str = "agent-initialize";
const CODEX_THREAD: &str = "agent-thread";
/// How often a followed transcript is read for new rows.
const TRANSCRIPT_POLL: Duration = Duration::from_millis(25);
/// The hook events terminal Claude reports to the agent, when the spec's
/// hook policy names none.
const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PermissionRequest",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "Notification",
];

/// What the provider did.
#[derive(Debug)]
pub enum ProviderEvent {
    Fact(Fact),
    /// Raw terminal bytes, for the terminal log.
    Output(Vec<u8>),
    /// The child exited, with its code when it had one.
    Exited(Option<i32>),
    /// Terminal Claude's messaging socket and the token it takes, as its
    /// hooks report them.
    Messaging(claude::hooks::MessagingCredentials),
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("starting {command}: {source}")]
    Spawn { command: String, source: io::Error },
    #[error("agents of kind {0:?} cannot be hosted yet")]
    Unhosted(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// A plain child's stdin, shared with the handshake that writes first.
type Stdin = Arc<tokio::sync::Mutex<Option<ChildStdin>>>;

enum Input {
    Stdin(Stdin),
    /// Keystrokes for the typing task.
    Terminal(mpsc::UnboundedSender<Vec<claude::pty::keymap::KeyStep>>),
    Closed,
}

/// The running provider child.
pub struct Provider {
    input: Input,
    /// The child's pid, which is also its process group.
    #[cfg_attr(not(unix), allow(dead_code))]
    pid: Option<u32>,
    /// Kills a plain child where there are no process groups.
    #[cfg_attr(unix, allow(dead_code))]
    kill: Option<oneshot::Sender<()>>,
    terminal: Option<pty_host::PtyHandle>,
    /// Terminal Claude's keymap, when one covers its version.
    keys: Option<Keys>,
    messaging: Option<claude::hooks::MessagingCredentials>,
    session: String,
    dir: PathBuf,
    events: mpsc::Sender<ProviderEvent>,
    followers: Vec<tokio::task::JoinHandle<()>>,
}

impl Provider {
    pub async fn spawn(
        spec: &AgentSpec,
        dir: &Path,
        events: mpsc::Sender<ProviderEvent>,
    ) -> Result<Self, ProviderError> {
        match spec.kind.as_str() {
            "claude_sdk" => Self::spawn_sdk(spec, dir, events).await,
            "claude_pty" => Self::spawn_terminal(spec, dir, events).await,
            "codex" => Self::spawn_codex(spec, dir, events),
            other => Err(ProviderError::Unhosted(other.to_owned())),
        }
    }

    /// Headless Claude: `claude -p` over stream-JSON, told to echo each
    /// user message so its reflection carries the uuid it was sent with.
    async fn spawn_sdk(
        spec: &AgentSpec,
        dir: &Path,
        events: mpsc::Sender<ProviderEvent>,
    ) -> Result<Self, ProviderError> {
        let (session, resume) = provider_session(spec, dir)?;
        let (args, settings) = claude_launch(spec, false)?;
        let mut command = tokio::process::Command::new(&spec.provider_command);
        command
            .args(&args)
            .args([
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-prompt-tool",
                "stdio",
                "--replay-user-messages",
                if resume { "--resume" } else { "--session-id" },
                &session,
                "--settings",
                &settings,
            ])
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(provider_log(dir)?)
            .kill_on_drop(false);
        environment(&mut command, spec);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|source| ProviderError::Spawn {
            command: spec.provider_command.clone(),
            source,
        })?;
        let pid = child.id();
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");

        let lines = events.clone();
        let reader = tokio::spawn(async move {
            let mut stdout = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = stdout.next_line().await {
                let fact = Fact {
                    channel: Channel::Stream,
                    payload: line.into_bytes(),
                };
                if lines.send(ProviderEvent::Fact(fact)).await.is_err() {
                    break;
                }
            }
        });
        let (kill, killed) = oneshot::channel();
        let exited = events.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status,
                _ = killed => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            let _ = tokio::time::timeout(TRAILING_OUTPUT, reader).await;
            let code = status.ok().and_then(|status| status.code());
            let _ = exited.send(ProviderEvent::Exited(code)).await;
        });

        let mut provider = Self {
            input: Input::Stdin(Arc::new(tokio::sync::Mutex::new(Some(stdin)))),
            pid,
            kill: Some(kill),
            terminal: None,
            keys: None,
            messaging: None,
            session,
            dir: dir.to_owned(),
            events,
            followers: Vec::new(),
        };
        // Claude reports nothing until it is asked; its answer is what says
        // it takes input.
        provider
            .write_line(br#"{"type":"control_request","request_id":"agent-initialize","request":{"subtype":"initialize"}}"#)
            .await?;
        Ok(provider)
    }

    /// Codex: one app server per agent over stdio. The agent does the
    /// handshake (initialize, initialized, then thread/start or, for a later
    /// incarnation, thread/resume); every line the server writes is a fact,
    /// and the interpreter writes everything after the handshake.
    fn spawn_codex(
        spec: &AgentSpec,
        dir: &Path,
        events: mpsc::Sender<ProviderEvent>,
    ) -> Result<Self, ProviderError> {
        let session_path = dir.join(dir::PRIVATE).join(dir::PROVIDER_SESSION);
        let thread = std::fs::read_to_string(&session_path)
            .ok()
            .map(|thread| thread.trim().to_owned())
            .filter(|thread| !thread.is_empty() && spec.incarnation > 1);
        let mut command = tokio::process::Command::new(&spec.provider_command);
        command
            .args(&spec.provider_args)
            .args(["app-server", "--listen", "stdio://"])
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(provider_log(dir)?)
            .kill_on_drop(false);
        environment(&mut command, spec);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|source| ProviderError::Spawn {
            command: spec.provider_command.clone(),
            source,
        })?;
        let pid = child.id();
        let stdin: Stdin = Arc::new(tokio::sync::Mutex::new(child.stdin.take()));
        let stdout = child.stdout.take().expect("stdout is piped");

        let (initialized, on_initialized) = oneshot::channel();
        let lines = events.clone();
        let reader = tokio::spawn(async move {
            let mut initialized = Some(initialized);
            let mut stdout = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = stdout.next_line().await {
                if let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) {
                    match message["id"].as_str() {
                        Some(CODEX_INITIALIZE) => {
                            if let Some(initialized) = initialized.take() {
                                let _ = initialized.send(());
                            }
                        }
                        // The thread the server made or resumed is the
                        // session the next incarnation resumes.
                        Some(CODEX_THREAD) => {
                            if let Some(thread) = message["result"]["thread"]["id"].as_str() {
                                let _ = std::fs::write(&session_path, thread);
                            }
                        }
                        _ => {}
                    }
                }
                let fact = Fact {
                    channel: Channel::Rpc,
                    payload: line.into_bytes(),
                };
                if lines.send(ProviderEvent::Fact(fact)).await.is_err() {
                    break;
                }
            }
        });
        let (kill, killed) = oneshot::channel();
        let exited = events.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status,
                _ = killed => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            let _ = tokio::time::timeout(TRAILING_OUTPUT, reader).await;
            let code = status.ok().and_then(|status| status.code());
            let _ = exited.send(ProviderEvent::Exited(code)).await;
        });

        let handshake = stdin.clone();
        let cwd = spec.cwd.clone();
        let model = spec.config.as_ref().and_then(|config| config.model.clone());
        tokio::spawn(async move {
            let initialize = serde_json::json!({
                "id": CODEX_INITIALIZE,
                "method": "initialize",
                "params": {
                    "clientInfo": { "name": "amux", "title": null, "version": crate::VERSION },
                    "capabilities": { "experimentalApi": true },
                },
            });
            if write_line(&handshake, initialize.to_string().as_bytes())
                .await
                .is_err()
                || on_initialized.await.is_err()
            {
                return;
            }
            let mut params = serde_json::json!({ "cwd": cwd });
            if let Some(model) = model {
                params["model"] = serde_json::json!(model);
            }
            let method = match thread {
                Some(thread) => {
                    params["threadId"] = serde_json::json!(thread);
                    "thread/resume"
                }
                None => "thread/start",
            };
            let start =
                serde_json::json!({ "id": CODEX_THREAD, "method": method, "params": params });
            let _ = write_line(&handshake, br#"{"method":"initialized"}"#).await;
            let _ = write_line(&handshake, start.to_string().as_bytes()).await;
        });

        Ok(Self {
            input: Input::Stdin(stdin),
            pid,
            kill: Some(kill),
            terminal: None,
            keys: None,
            messaging: None,
            session: String::new(),
            dir: dir.to_owned(),
            events,
            followers: Vec::new(),
        })
    }

    /// Claude in a terminal. The agent reads its version and resolves the
    /// keymap for it first, and reports both as the launch fact before
    /// anything Claude says. Its hooks report to private/hooks.sock; its
    /// messaging socket is private/messaging.sock.
    async fn spawn_terminal(
        spec: &AgentSpec,
        dir: &Path,
        events: mpsc::Sender<ProviderEvent>,
    ) -> Result<Self, ProviderError> {
        let (session, resume) = provider_session(spec, dir)?;
        let keys = Keys::resolve(Path::new(&spec.provider_command)).await;
        let launch = interpret::claude_pty::launch_fact(
            keys.as_ref().map_or("", |keys| keys.version.as_str()),
            keys.as_ref().map_or("", Keys::name),
        );
        let _ = events
            .send(ProviderEvent::Fact(Fact {
                channel: Channel::Agent,
                payload: launch,
            }))
            .await;
        let private = dir.join(dir::PRIVATE);
        let (mut args, settings) = claude_launch(spec, true)?;
        args.extend([
            if resume { "--resume" } else { "--session-id" }.to_owned(),
            session.clone(),
            "--settings".to_owned(),
            settings,
            "--messaging-socket-path".to_owned(),
            socket_address(&private.join(dir::MESSAGING_SOCK))?
                .display()
                .to_string(),
        ]);
        let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = spec
            .config
            .iter()
            .flat_map(|config| config.env.iter())
            .chain(spec.provider_env.iter())
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        env.push((
            claude::hooks::HOOK_SOCKET_ENV.into(),
            socket_address(&private.join(dir::HOOKS_SOCK))?.into(),
        ));
        let process = pty_host::spawn(pty_host::PtySpawn {
            command: PathBuf::from(&spec.provider_command),
            args,
            cwd: PathBuf::from(&spec.cwd),
            env,
            env_remove: claude::launch::CHILD_SESSION_ENV_SCRUB
                .iter()
                .map(Into::into)
                .collect(),
            size: pty_host::PtySize::default(),
        })
        .map_err(|error| ProviderError::Spawn {
            command: spec.provider_command.clone(),
            source: io::Error::other(error),
        })?;
        let handle = process.handle.clone();
        let mut output = handle.output();
        let mut exit = process.exit;
        let forward = events.clone();
        tokio::spawn(async move {
            let mut ready = Ready::default();
            let status = loop {
                tokio::select! {
                    bytes = output.recv() => match bytes {
                        Some(bytes) => {
                            let _ = forward.send(ProviderEvent::Output(bytes.to_vec())).await;
                            if ready.watch(&bytes) {
                                let fact = Fact {
                                    channel: Channel::Agent,
                                    payload: interpret::claude_pty::ready_fact(),
                                };
                                let _ = forward.send(ProviderEvent::Fact(fact)).await;
                            }
                        }
                        None => break exit.wait().await,
                    },
                    status = exit.wait() => break status,
                }
            };
            // The reader ends only when every holder of the terminal has
            // closed it; take what is already there and leave it.
            while let Ok(Some(bytes)) = tokio::time::timeout(TRAILING_OUTPUT, output.recv()).await {
                let _ = forward.send(ProviderEvent::Output(bytes.to_vec())).await;
            }
            let code = if status.signal().is_some() {
                None
            } else {
                Some(status.exit_code() as i32)
            };
            let _ = forward.send(ProviderEvent::Exited(code)).await;
        });
        Ok(Self {
            input: Input::Terminal(terminal::typist(handle.clone())),
            pid: Some(handle.pid()),
            kill: None,
            terminal: Some(handle),
            keys,
            messaging: None,
            session,
            dir: dir.to_owned(),
            events,
            followers: Vec::new(),
        })
    }

    /// Carries out an effect aimed at the provider.
    pub async fn perform(&mut self, effect: Effect) -> io::Result<()> {
        match effect {
            Effect::ProviderWrite(bytes) => self.write_line(&bytes).await,
            Effect::UserMessage {
                uuid,
                text,
                attachments,
            } => self.user_message(&uuid, &text, &attachments).await,
            Effect::Inject {
                envelope,
                via: interpret::Carrier::Stdin,
            } => {
                let uuid = interpret::claude_sdk::client_uuid(&envelope.id);
                self.user_message(&uuid, &envelope.text, &[]).await
            }
            Effect::CodexTurnInput {
                request,
                attachments,
            } => {
                let request = codex_turn_input(&request, &attachments, &self.dir.join(dir::BLOBS))?;
                self.write_line(&request).await
            }
            Effect::FollowTranscript { path } => {
                self.follow(PathBuf::from(path));
                Ok(())
            }
            Effect::Terminal(input) => {
                let text = match &input {
                    interpret::claude_pty::TerminalInput::Prompt { text, attachments } => {
                        self.with_attachments(text, attachments)
                    }
                    _ => String::new(),
                };
                let keys = self.keys.as_ref().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "no keymap covers this Claude, so nothing can be typed",
                    )
                })?;
                let steps = keys.steps(&input, &text)?;
                self.type_keys(steps)
            }
            Effect::Inject {
                envelope,
                via: interpret::Carrier::MessagingSocket,
            } => self.message_terminal(&envelope.text).await,
            other => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("this agent cannot carry out {other:?} yet"),
            )),
        }
    }

    /// The text a provider reads for a prompt: its attachments written in
    /// as elements naming their blobs.
    fn with_attachments(&self, text: &str, attachments: &[Attachment]) -> String {
        if attachments.is_empty() {
            return text.to_owned();
        }
        attachments::format(
            &attachments::Positioned {
                text: text.to_owned(),
                attachments: attachments.to_vec(),
            },
            &self.dir.join(dir::BLOBS),
        )
    }

    fn type_keys(&self, steps: Vec<claude::pty::keymap::KeyStep>) -> io::Result<()> {
        match &self.input {
            Input::Terminal(keys) => keys.send(steps).map_err(|_| closed()),
            _ => Err(closed()),
        }
    }

    pub fn set_messaging(&mut self, messaging: claude::hooks::MessagingCredentials) {
        self.messaging = Some(messaging);
    }

    /// An agent message for terminal Claude: onto its messaging socket,
    /// which queues it the way Claude queues a peer's message; pasted into
    /// the terminal when the socket is not known yet or refuses it.
    async fn message_terminal(&mut self, text: &str) -> io::Result<()> {
        if let Some(messaging) = &self.messaging {
            let sent = async {
                let mut socket = claude::messaging::MessagingSocket::connect(
                    &messaging.socket_path,
                    &messaging.token,
                )
                .await?;
                socket.send(text).await
            }
            .await;
            match sent {
                Ok(_) => return Ok(()),
                Err(error) => {
                    eprintln!("amux agent: the messaging socket failed ({error}); pasting")
                }
            }
        }
        let keys = self.keys.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "no messaging socket and no keymap to paste an agent message with",
            )
        })?;
        let steps = keys.prompt(&terminal::pasted_message(text))?;
        self.type_keys(steps)
    }

    async fn user_message(
        &mut self,
        uuid: &str,
        text: &str,
        attachments: &[Attachment],
    ) -> io::Result<()> {
        let content = self.with_attachments(text, attachments);
        let line = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": content },
            "parent_tool_use_id": null,
            "session_id": self.session,
            "uuid": uuid,
        });
        self.write_line(line.to_string().as_bytes()).await
    }

    async fn write_line(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &mut self.input {
            Input::Stdin(stdin) => write_line(stdin, bytes).await,
            Input::Terminal(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "terminal Claude takes keystrokes, not lines",
            )),
            Input::Closed => Err(closed()),
        }
    }

    /// Reads a transcript's rows as they are written. A transcript an
    /// earlier incarnation followed is read on from where it stopped, so a
    /// resumed session's old rows are not read twice; any other from its
    /// start. The position is kept in private/ after every read.
    fn follow(&mut self, path: PathBuf) {
        let events = self.events.clone();
        let cursor_path = self.dir.join(dir::PRIVATE).join(dir::TRANSCRIPT_CURSOR);
        self.followers.push(tokio::spawn(async move {
            let mut offset = saved_cursor(&cursor_path, &path);
            loop {
                if let Ok(bytes) = tokio::fs::read(&path).await
                    && bytes.len() > offset
                {
                    // Only whole rows: a row still being written waits for
                    // the next read.
                    let Some(end) = bytes[offset..].iter().rposition(|byte| *byte == b'\n') else {
                        tokio::time::sleep(TRANSCRIPT_POLL).await;
                        continue;
                    };
                    let whole = &bytes[offset..offset + end + 1];
                    for row in whole.split(|byte| *byte == b'\n') {
                        if row.is_empty() {
                            continue;
                        }
                        let fact = Fact {
                            channel: Channel::Transcript,
                            payload: row.to_vec(),
                        };
                        if events.send(ProviderEvent::Fact(fact)).await.is_err() {
                            return;
                        }
                    }
                    offset += end + 1;
                    let _ = std::fs::write(&cursor_path, format!("{offset}\n{}", path.display()));
                }
                tokio::time::sleep(TRANSCRIPT_POLL).await;
            }
        }));
    }

    /// Asks the provider to finish: end of input for a plain child, a
    /// terminate signal for a terminal one.
    pub fn close(&mut self) {
        match std::mem::replace(&mut self.input, Input::Closed) {
            Input::Stdin(stdin) => {
                tokio::spawn(async move { stdin.lock().await.take() });
            }
            Input::Terminal(_) => {
                if let Some(terminal) = &self.terminal {
                    let _ = terminal.signal_process_group(pty_host::ProcessGroupSignal::Terminate);
                }
            }
            Input::Closed => {}
        }
    }

    /// The process group, now.
    pub fn kill(&mut self) {
        if let Some(terminal) = &self.terminal {
            let _ = terminal.signal_process_group(pty_host::ProcessGroupSignal::Kill);
            return;
        }
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // SAFETY: killpg only sends a signal.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        if let Some(kill) = self.kill.take() {
            let _ = kill.send(());
        }
    }

    /// Resizes the terminal, for a provider that has one.
    #[allow(dead_code)]
    pub fn resize(&self, rows: u16, cols: u16) {
        if let Some(terminal) = &self.terminal {
            let _ = terminal.resize(pty_host::PtySize { rows, cols });
        }
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        for follower in &self.followers {
            follower.abort();
        }
    }
}

/// A Codex turn request with its attachments appended to `params.input`:
/// an image as a local image by its blob's path, anything else as the
/// element text the model reads.
fn codex_turn_input(
    request: &[u8],
    attachments: &[Attachment],
    blobs: &Path,
) -> io::Result<Vec<u8>> {
    let mut request: serde_json::Value = serde_json::from_slice(request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let Some(input) = request["params"]["input"].as_array_mut() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a Codex turn request without params.input",
        ));
    };
    for attachment in attachments {
        input.push(match &attachment.of {
            Some(wire::attachment::Of::Image(blob)) => serde_json::json!({
                "type": "localImage",
                "path": blobs.join(interpret::to_hex(&blob.hash)),
            }),
            _ => serde_json::json!({
                "type": "text",
                "text": attachments::element(attachment, None),
                "text_elements": [],
            }),
        });
    }
    serde_json::to_vec(&request).map_err(io::Error::other)
}

/// Watches terminal output for Claude turning on bracketed paste, which it
/// does when its input goes live: the first moment a prompt can be typed.
#[derive(Default)]
struct Ready {
    seen: bool,
    /// The end of the output so far, for a sequence split across reads.
    tail: Vec<u8>,
}

impl Ready {
    const SEQUENCE: &[u8] = b"\x1b[?2004h";

    /// True the first time the sequence appears.
    fn watch(&mut self, bytes: &[u8]) -> bool {
        if self.seen {
            return false;
        }
        self.tail.extend_from_slice(bytes);
        if self
            .tail
            .windows(Self::SEQUENCE.len())
            .any(|window| window == Self::SEQUENCE)
        {
            self.seen = true;
            self.tail = Vec::new();
            return true;
        }
        let keep = self.tail.len().saturating_sub(Self::SEQUENCE.len() - 1);
        self.tail.drain(..keep);
        false
    }
}

/// Where an earlier follower of `path` stopped; the start for any other
/// file, or one that is shorter now.
fn saved_cursor(cursor: &Path, path: &Path) -> usize {
    let Ok(saved) = std::fs::read_to_string(cursor) else {
        return 0;
    };
    let Some((offset, saved_path)) = saved.split_once('\n') else {
        return 0;
    };
    let length = std::fs::metadata(path).map_or(0, |metadata| metadata.len() as usize);
    match offset.parse::<usize>() {
        Ok(offset) if Path::new(saved_path) == path && offset <= length => offset,
        _ => 0,
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the provider's input is closed")
}

/// Claude's launch arguments from the spec, less any `--settings`, and the
/// one settings value the agent passes instead: the user's settings with
/// the agent's merged over them. Every Claude accepts messages from other
/// sessions at once, so a bypass-permissions agent does not hold an agent
/// message behind an approval nobody is there to give; terminal Claude
/// also reports its hook events to the agent through the hook binary at the
/// install path.
fn claude_launch(spec: &AgentSpec, hooks: bool) -> io::Result<(Vec<String>, String)> {
    let mut args = spec.provider_args.clone();
    let sources = claude::launch::take_settings_args(&mut args).map_err(io::Error::other)?;
    let user = claude::launch::load_user_settings(Path::new(&spec.cwd), &sources)
        .map_err(io::Error::other)?;
    let config = spec.config.clone().unwrap_or_default();
    let managed = claude::launch::ManagedSettings {
        hook_command: if hooks && !config.install_path.is_empty() {
            vec![config.install_path, "hooks".into(), "claude".into()]
        } else {
            Vec::new()
        },
        hook_events: if config.hooks.is_empty() {
            HOOK_EVENTS
                .iter()
                .map(|event| (*event).to_owned())
                .collect()
        } else {
            config.hooks
        },
        accept_cross_session: true,
        ..Default::default()
    };
    let settings = claude::launch::merged_settings(user, &managed).into_value();
    Ok((args, settings.to_string()))
}

/// The path a Unix socket in the agent directory is bound and dialled at,
/// for a child that dials it with no knowledge of the short links long
/// paths need.
fn socket_address(path: &Path) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        crate::local_socket::unix_address(path)
    }
    #[cfg(not(unix))]
    {
        Ok(path.to_owned())
    }
}

async fn write_line(stdin: &Stdin, bytes: &[u8]) -> io::Result<()> {
    let mut stdin = stdin.lock().await;
    let Some(stdin) = stdin.as_mut() else {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "the provider's input is closed",
        ));
    };
    stdin.write_all(bytes).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await
}

/// The provider's own session id for this agent, and whether this
/// incarnation continues it. The first incarnation makes one up; later ones
/// resume it.
fn provider_session(spec: &AgentSpec, dir: &Path) -> io::Result<(String, bool)> {
    let path = dir.join(dir::PRIVATE).join(dir::PROVIDER_SESSION);
    match std::fs::read_to_string(&path) {
        Ok(session) if !session.trim().is_empty() => {
            Ok((session.trim().to_owned(), spec.incarnation > 1))
        }
        Ok(_) => new_session(&path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => new_session(&path),
        Err(error) => Err(error),
    }
}

fn new_session(path: &Path) -> io::Result<(String, bool)> {
    let session = uuid::Uuid::new_v4().to_string();
    std::fs::write(path, &session)?;
    Ok((session, false))
}

fn provider_log(dir: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(dir::PRIVATE).join(dir::PROVIDER_LOG))
}

/// The child's environment: ours without the variables a parent Claude
/// session sets, plus the spec's additions.
fn environment(command: &mut tokio::process::Command, spec: &AgentSpec) {
    for key in claude::launch::CHILD_SESSION_ENV_SCRUB {
        command.env_remove(key);
    }
    if let Some(config) = &spec.config {
        command.envs(&config.env);
    }
    command.envs(&spec.provider_env);
}

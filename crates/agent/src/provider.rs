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
/// How long terminal Claude's output must stay quiet after its input goes
/// live before it counts as ready, so a first screen that is its
/// folder-trust dialog is seen before anything is typed into it.
const FIRST_SCREEN_QUIET: Duration = Duration::from_millis(100);
/// The longest the first screen is waited for once input is live.
const FIRST_SCREEN_WAIT: Duration = Duration::from_millis(500);
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
    followers: Vec<Follower>,
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
        let (args, settings) = claude_launch(spec, dir, false)?;
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
            .args(codex_args(spec, dir))
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
        let (session, mut resume) = provider_session(spec, dir)?;
        // Claude writes a session's transcript once the session begins; one
        // that ended before (at its folder-trust dialog, say) has none, and
        // Claude refuses to resume it. It starts under the same id instead.
        if resume && !session_began(spec, &session) {
            eprintln!(
                "amux agent: Claude's session {session} never began; starting it rather than resuming"
            );
            resume = false;
        }
        let keys = Keys::resolve(Path::new(&spec.provider_command)).await;
        let launch = interpret::claude_pty::launch_fact(
            keys.as_ref().map_or("", |keys| keys.version.as_str()),
            keys.as_ref().map_or("", Keys::name),
            &keys
                .as_ref()
                .map(Keys::permission_menus)
                .unwrap_or_default(),
        );
        let _ = events
            .send(ProviderEvent::Fact(Fact {
                channel: Channel::Agent,
                payload: launch,
            }))
            .await;
        let private = dir.join(dir::PRIVATE);
        let (mut args, settings) = claude_launch(spec, dir, true)?;
        args.extend([
            if resume { "--resume" } else { "--session-id" }.to_owned(),
            session.clone(),
            "--settings".to_owned(),
            settings,
            "--messaging-socket-path".to_owned(),
            messaging_address(&private.join(dir::MESSAGING_SOCK))?
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
            let agent_fact = |payload| {
                ProviderEvent::Fact(Fact {
                    channel: Channel::Agent,
                    payload,
                })
            };
            let mut ready = crate::ready::InputLive::default();
            // Only a first screen is the trust dialog; later output that
            // happens to read like it is not.
            let mut trust = Some(crate::ready::TrustDialog::default());
            // Input is live: when the first screen has settled (quiet, or
            // waited for long enough), Claude is ready.
            let mut settling: Option<(tokio::time::Instant, tokio::time::Instant)> = None;
            let status = loop {
                let settled = settling.map(|(quiet, cap)| quiet.min(cap));
                tokio::select! {
                    bytes = output.recv() => match bytes {
                        Some(bytes) => {
                            let _ = forward.send(ProviderEvent::Output(bytes.to_vec())).await;
                            if trust.as_mut().is_some_and(|trust| trust.watch(&bytes)) {
                                trust = None;
                                let payload = interpret::claude_pty::trust_dialog_fact();
                                let _ = forward.send(agent_fact(payload)).await;
                            }
                            let now = tokio::time::Instant::now();
                            if ready.watch(&bytes) {
                                settling = Some((now + FIRST_SCREEN_QUIET, now + FIRST_SCREEN_WAIT));
                            } else if let Some((quiet, _)) = &mut settling {
                                *quiet = now + FIRST_SCREEN_QUIET;
                            }
                        }
                        None => break exit.wait().await,
                    },
                    () = tokio::time::sleep_until(settled.unwrap_or_else(tokio::time::Instant::now)),
                        if settled.is_some() =>
                    {
                        settling = None;
                        trust = None;
                        let payload = interpret::claude_pty::ready_fact();
                        let _ = forward.send(agent_fact(payload)).await;
                    }
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

    /// A headless user message's content: the text with its elements, or,
    /// when a person attached images, content blocks with each image's bytes
    /// as a native image block right after its element.
    fn content(&self, text: &str, attachments: &[Attachment]) -> serde_json::Value {
        use attachments::Piece;
        use wire::attachment::Of;

        let images = attachments
            .iter()
            .any(|attachment| matches!(attachment.of, Some(Of::Image(_))));
        if !images {
            return self.with_attachments(text, attachments).into();
        }
        let positioned = attachments::Positioned {
            text: text.to_owned(),
            attachments: attachments.to_vec(),
        };
        let mut blocks = Vec::new();
        let mut prose = String::new();
        for piece in attachments::pieces(&positioned, &self.dir.join(dir::BLOBS)) {
            match piece {
                Piece::Text(text) => prose.push_str(text),
                Piece::Element {
                    element,
                    attachment,
                    path,
                } => {
                    prose.push_str(&element);
                    let Some(Of::Image(blob)) = &attachment.of else {
                        continue;
                    };
                    // The element names the file, so a model can still
                    // read an image whose bytes cannot be inlined.
                    let Some(bytes) = path.and_then(|path| std::fs::read(path).ok()) else {
                        continue;
                    };
                    blocks.push(
                        serde_json::json!({ "type": "text", "text": std::mem::take(&mut prose) }),
                    );
                    blocks.push(serde_json::json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": blob.mime,
                            "data": base64(&bytes),
                        },
                    }));
                }
            }
        }
        if !prose.is_empty() {
            blocks.push(serde_json::json!({ "type": "text", "text": prose }));
        }
        blocks.into()
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
        let content = self.content(text, attachments);
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
    /// start. The position is kept in private/ after every row.
    fn follow(&mut self, path: PathBuf) {
        let cursor = Cursor {
            file: self.dir.join(dir::PRIVATE).join(dir::TRANSCRIPT_CURSOR),
            offset: Arc::new(std::sync::Mutex::new(saved_cursor(
                &self.dir.join(dir::PRIVATE).join(dir::TRANSCRIPT_CURSOR),
                &path,
            ))),
            path,
        };
        let events = self.events.clone();
        let reading = cursor.clone();
        let task = tokio::spawn(async move {
            loop {
                for row in reading.new_rows() {
                    let length = row.len() + 1;
                    if row.is_empty() {
                        reading.advance(length);
                        continue;
                    }
                    let fact = Fact {
                        channel: Channel::Transcript,
                        payload: row,
                    };
                    // Sending is where an abort lands; the row counts as read
                    // only once it is sent.
                    if events.send(ProviderEvent::Fact(fact)).await.is_err() {
                        return;
                    }
                    reading.advance(length);
                }
                tokio::time::sleep(TRANSCRIPT_POLL).await;
            }
        });
        self.followers.push(Follower { cursor, task });
    }

    /// Stops following and returns the rows the followers had not read:
    /// the provider has exited, so its transcripts are complete, and rows
    /// it wrote just before exiting must not be lost to the poll interval.
    pub async fn finish_transcripts(&mut self) -> Vec<Fact> {
        let mut rows = Vec::new();
        for follower in std::mem::take(&mut self.followers) {
            follower.task.abort();
            let _ = follower.task.await;
            for row in follower.cursor.new_rows() {
                follower.cursor.advance(row.len() + 1);
                if row.is_empty() {
                    continue;
                }
                rows.push(Fact {
                    channel: Channel::Transcript,
                    payload: row,
                });
            }
        }
        rows
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

    /// Bytes an attached terminal typed, in order with the keystrokes the
    /// interpreter asked for.
    pub fn type_raw(&self, bytes: Vec<u8>) {
        let _ = self.type_keys(vec![claude::pty::keymap::KeyStep::Write(bytes)]);
    }

    /// Resizes the terminal, for a provider that has one.
    pub fn resize(&self, rows: u16, cols: u16) {
        if let Some(terminal) = &self.terminal {
            let _ = terminal.resize(pty_host::PtySize { rows, cols });
        }
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        for follower in &self.followers {
            follower.task.abort();
        }
    }
}

/// A Codex turn request with its attachments appended to `params.input`:
/// an image as a local image by its blob's path, anything else as the
/// element text the model reads, naming the file's path so the model can
/// open it.
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
                "text": attachments::element(
                    attachment,
                    attachments::blob_path(attachment, blobs).as_deref(),
                ),
                "text_elements": [],
            }),
        });
    }
    serde_json::to_vec(&request).map_err(io::Error::other)
}

/// A followed transcript and the task reading it.
struct Follower {
    cursor: Cursor,
    task: tokio::task::JoinHandle<()>,
}

/// How far a transcript has been read, kept in private/ as it moves.
#[derive(Clone)]
struct Cursor {
    path: PathBuf,
    file: PathBuf,
    offset: Arc<std::sync::Mutex<usize>>,
}

impl Cursor {
    /// The whole rows written past the cursor; a row still being written
    /// waits for the next read.
    fn new_rows(&self) -> Vec<Vec<u8>> {
        let offset = *self.offset.lock().expect("cursor lock");
        let Ok(bytes) = std::fs::read(&self.path) else {
            return Vec::new();
        };
        let Some(unread) = bytes.get(offset..) else {
            return Vec::new();
        };
        let Some(end) = unread.iter().rposition(|byte| *byte == b'\n') else {
            return Vec::new();
        };
        unread[..end]
            .split(|byte| *byte == b'\n')
            .map(<[u8]>::to_vec)
            .collect()
    }

    /// Moves past one row and its newline; an empty row is skipped the
    /// same way, and never sent.
    fn advance(&self, length: usize) {
        let mut offset = self.offset.lock().expect("cursor lock");
        *offset += length;
        let _ = std::fs::write(&self.file, format!("{offset}\n{}", self.path.display()));
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

/// Claude's launch arguments from the spec, less any `--settings`, plus
/// amux's tool server, and the one settings value the agent passes
/// instead: the user's settings with the agent's merged over them. Every
/// Claude accepts messages from other sessions at once, so a
/// bypass-permissions agent does not hold an agent message behind an
/// approval nobody is there to give, and runs amux's own tools without
/// asking; terminal Claude also reports its hook events to the agent
/// through the hook binary at the install path.
fn claude_launch(spec: &AgentSpec, dir: &Path, hooks: bool) -> io::Result<(Vec<String>, String)> {
    let mut args = spec.provider_args.clone();
    let tools = tool_server(spec, dir);
    if let Some((command, server_args)) = &tools {
        args.extend([
            "--mcp-config".to_owned(),
            serde_json::json!({
                "mcpServers": {
                    interpret::AMUX_TOOL_SERVER: { "command": command, "args": server_args },
                },
            })
            .to_string(),
        ]);
    }
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
        permissions_allow: tools
            .iter()
            .map(|_| format!("mcp__{}__*", interpret::AMUX_TOOL_SERVER))
            .collect(),
        ..Default::default()
    };
    let settings = claude::launch::merged_settings(user, &managed).into_value();
    Ok((args, settings.to_string()))
}

/// Codex's global arguments: the spec's, then amux's tool server as
/// configuration overrides in Codex's TOML (a JSON string or array of
/// strings is the same TOML value).
fn codex_args(spec: &AgentSpec, dir: &Path) -> Vec<String> {
    let mut args = spec.provider_args.clone();
    if let Some((server, server_args)) = tool_server(spec, dir) {
        let key = format!("mcp_servers.{}", interpret::AMUX_TOOL_SERVER);
        args.extend([
            "--config".to_owned(),
            format!("{key}.command={}", serde_json::json!(server)),
            "--config".to_owned(),
            format!("{key}.args={}", serde_json::json!(server_args)),
        ]);
    }
    args
}

/// A Codex terminal view on the agent's thread: `codex resume <thread>`
/// launched as the agent's own app server is, on a terminal of its own.
pub fn codex_view(
    spec: &AgentSpec,
    dir: &Path,
    thread: &str,
    size: pty_host::PtySize,
) -> pty_host::PtySpawn {
    let mut args = codex_args(spec, dir);
    args.extend(["resume".to_owned(), thread.to_owned()]);
    pty_host::PtySpawn {
        command: PathBuf::from(&spec.provider_command),
        args,
        cwd: PathBuf::from(&spec.cwd),
        env: spec
            .config
            .iter()
            .flat_map(|config| config.env.iter())
            .chain(spec.provider_env.iter())
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
        env_remove: claude::launch::CHILD_SESSION_ENV_SCRUB
            .iter()
            .map(Into::into)
            .collect(),
        size,
    }
}

/// The Codex thread this agent runs, once its app server has made or
/// resumed one.
pub fn codex_thread(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(dir::PRIVATE).join(dir::PROVIDER_SESSION))
        .ok()
        .map(|thread| thread.trim().to_owned())
        .filter(|thread| !thread.is_empty())
}

/// amux's tool server as the harness launches it: the install path's
/// `mcp` subcommand on this agent's directory, which is how it finds the
/// socket that identifies it. None without an install path.
fn tool_server(spec: &AgentSpec, dir: &Path) -> Option<(String, Vec<String>)> {
    let install_path = spec.config.as_ref()?.install_path.clone();
    (!install_path.is_empty()).then(|| {
        (
            install_path,
            vec!["mcp".to_owned(), dir.display().to_string()],
        )
    })
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

/// Where terminal Claude binds its messaging socket. Claude binds it itself
/// and refuses a directory reached through a symbolic link, so a path too
/// long for a socket moves to a real private directory rather than behind
/// the link the agent's own sockets use.
fn messaging_address(path: &Path) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        crate::local_socket::unix_private_address(path)
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

/// Whether Claude has a transcript for this session: resuming needs one.
/// Claude keeps them under CLAUDE_CONFIG_DIR, as the spec or this process
/// sets it, else ~/.claude.
fn session_began(spec: &AgentSpec, session: &str) -> bool {
    const CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";
    let configured = spec
        .provider_env
        .get(CONFIG_DIR)
        .or_else(|| {
            spec.config
                .as_ref()
                .and_then(|config| config.env.get(CONFIG_DIR))
        })
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(CONFIG_DIR).map(PathBuf::from));
    let Some(config) = configured
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
    else {
        return true;
    };
    claude::history::find_session_file(&config, Path::new(&spec.cwd), session).is_some()
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

/// Standard base64 with padding, as Claude's image blocks carry bytes.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (index, byte)| {
            word | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(ALPHABET[(word >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// A file reaches Codex as its element naming where the file is, so the
    /// model can open it; an image goes as a local image by the same path.
    #[test]
    fn codex_attachments_name_their_blob_paths() {
        let blob = |hash: u8, name: &str, mime: &str| wire::BlobRef {
            hash: vec![hash; 32],
            name: name.into(),
            mime: mime.into(),
            size: 3,
        };
        let attachments = [
            wire::Attachment {
                of: Some(wire::attachment::Of::File(blob(
                    0xcd,
                    "report.pdf",
                    "application/pdf",
                ))),
            },
            wire::Attachment {
                of: Some(wire::attachment::Of::Image(blob(
                    0xab,
                    "chart.png",
                    "image/png",
                ))),
            },
        ];
        let blobs = Path::new("/agents/a/blobs");
        let request =
            br#"{"method":"turn/start","params":{"input":[{"type":"text","text":"read these"}]}}"#;
        let written = super::codex_turn_input(request, &attachments, blobs).unwrap();
        let written: serde_json::Value = serde_json::from_slice(&written).unwrap();
        let input = written["params"]["input"].as_array().unwrap();
        let file = blobs.join("cd".repeat(32));
        assert_eq!(
            input[1]["text"],
            attachments::element(&attachments[0], Some(&file))
        );
        assert!(
            input[1]["text"]
                .as_str()
                .unwrap()
                .contains(&format!("path=\"{}\"", file.display()))
        );
        assert_eq!(input[2]["type"], "localImage");
        assert_eq!(
            input[2]["path"],
            blobs.join("ab".repeat(32)).display().to_string()
        );
    }

    #[test]
    fn base64_pads_as_the_standard_does() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"foob"), "Zm9vYg==");
    }
}

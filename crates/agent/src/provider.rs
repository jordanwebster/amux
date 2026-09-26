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
use std::time::Duration;

use interpret::{Channel, Effect, Fact};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{mpsc, oneshot};
use wire::{AgentSpec, Attachment};

use crate::dir;

/// How long output still in flight is collected after the child exits.
const TRAILING_OUTPUT: Duration = Duration::from_millis(300);
/// How often a followed transcript is read for new rows.
const TRANSCRIPT_POLL: Duration = Duration::from_millis(25);

/// What the provider did.
#[derive(Debug)]
pub enum ProviderEvent {
    Fact(Fact),
    /// Raw terminal bytes, for the terminal log.
    Output(Vec<u8>),
    /// The child exited, with its code when it had one.
    Exited(Option<i32>),
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

enum Input {
    Stdin(ChildStdin),
    Terminal(pty_host::PtyHandle),
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
            "claude_pty" => Self::spawn_terminal(spec, dir, events),
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
        let mut command = tokio::process::Command::new(&spec.provider_command);
        command
            .args(&spec.provider_args)
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
            input: Input::Stdin(stdin),
            pid,
            kill: Some(kill),
            terminal: None,
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

    /// Claude in a terminal. Its hooks report to private/hooks.sock.
    fn spawn_terminal(
        spec: &AgentSpec,
        dir: &Path,
        events: mpsc::Sender<ProviderEvent>,
    ) -> Result<Self, ProviderError> {
        let (session, resume) = provider_session(spec, dir)?;
        let mut args = spec.provider_args.clone();
        args.extend([
            if resume { "--resume" } else { "--session-id" }.to_owned(),
            session.clone(),
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
            dir.join(dir::PRIVATE).join(dir::HOOKS_SOCK).into(),
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
            let status = loop {
                tokio::select! {
                    bytes = output.recv() => match bytes {
                        Some(bytes) => {
                            let _ = forward.send(ProviderEvent::Output(bytes.to_vec())).await;
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
            input: Input::Terminal(handle.clone()),
            pid: Some(handle.pid()),
            kill: None,
            terminal: Some(handle),
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
            Effect::FollowTranscript { path } => {
                self.follow(PathBuf::from(path));
                Ok(())
            }
            other => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("this agent cannot carry out {other:?} yet"),
            )),
        }
    }

    async fn user_message(
        &mut self,
        uuid: &str,
        text: &str,
        attachments: &[Attachment],
    ) -> io::Result<()> {
        let content = if attachments.is_empty() {
            text.to_owned()
        } else {
            attachments::format(
                &attachments::Positioned {
                    text: text.to_owned(),
                    attachments: attachments.to_vec(),
                },
                &self.dir.join(dir::BLOBS),
            )
        };
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
            Input::Stdin(stdin) => {
                stdin.write_all(bytes).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await
            }
            Input::Terminal(terminal) => terminal.write(bytes).await.map_err(io::Error::other),
            Input::Closed => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the provider's input is closed",
            )),
        }
    }

    /// Reads a transcript's rows as they are written, from its start.
    fn follow(&mut self, path: PathBuf) {
        let events = self.events.clone();
        self.followers.push(tokio::spawn(async move {
            let mut offset = 0usize;
            let mut partial = Vec::new();
            loop {
                if let Ok(bytes) = tokio::fs::read(&path).await
                    && bytes.len() > offset
                {
                    partial.extend_from_slice(&bytes[offset..]);
                    offset = bytes.len();
                    while let Some(end) = partial.iter().position(|byte| *byte == b'\n') {
                        let line: Vec<u8> = partial.drain(..=end).collect();
                        let row = line[..line.len() - 1].to_vec();
                        if row.is_empty() {
                            continue;
                        }
                        let fact = Fact {
                            channel: Channel::Transcript,
                            payload: row,
                        };
                        if events.send(ProviderEvent::Fact(fact)).await.is_err() {
                            return;
                        }
                    }
                }
                tokio::time::sleep(TRANSCRIPT_POLL).await;
            }
        }));
    }

    /// Asks the provider to finish: end of input for a plain child, a
    /// terminate signal for a terminal one.
    pub fn close(&mut self) {
        match std::mem::replace(&mut self.input, Input::Closed) {
            Input::Stdin(stdin) => drop(stdin),
            Input::Terminal(terminal) => {
                let _ = terminal.signal_process_group(pty_host::ProcessGroupSignal::Terminate);
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

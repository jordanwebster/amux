//! Capture-harness infrastructure: an isolated real amux daemon driving a
//! real `claude`, with the structured transcript stream recorded to disk.
//!
//! Isolation contract: everything the daemon touches lives under a scratch
//! root — its own config, data dir (identity/trust), state file, and socket.
//! The user's live daemon, config, and trust store are never contacted. The
//! spawned claude still uses the user's `~/.claude` credentials and writes
//! its transcript under `~/.claude/projects/<scratch-slug>/` (read-only
//! observation of what the scratch claude writes).
//!
//! The daemon environment is deliberately *poisoned* with the Claude Code
//! child-session marker set by default (see [`DaemonEnv`]): every capture run
//! doubles as a live regression test of the spawn-seam scrub — if the scrub
//! regressed, claude would suppress transcript persistence and the run would
//! observe zero rows.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use amux::claude_io::{
    ClaudePtyTranscriptV1Action, ClaudePtyTranscriptV1Input, PTY_TRANSCRIPT_V1, RAW_V1,
    decode_pty_transcript_v1_output, encode_pty_transcript_v1_input,
};
use amux::{
    AgentType, Client, Config, CreateAgentRequest, ProtocolError, SendInputRequest,
    SubscribeSessionEvent, SubscribeSessionRequest, TerminalSize,
};
use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::Mutex;
use uuid::Uuid;

/// One received transcript-stream row.
#[derive(Clone)]
pub struct Row {
    pub seq: u64,
    pub raw: String,
    pub json: serde_json::Value,
}

impl Row {
    pub fn row_type(&self) -> &str {
        self.json.get("type").and_then(|t| t.as_str()).unwrap_or("")
    }

    /// True if this is an `assistant` row carrying a `tool_use` block for the
    /// named tool.
    pub fn is_tool_use(&self, tool_name: &str) -> bool {
        if self.row_type() != "assistant" {
            return false;
        }
        self.json
            .pointer("/message/content")
            .and_then(|c| c.as_array())
            .is_some_and(|blocks| {
                blocks.iter().any(|b| {
                    b.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                        && b.get("name").and_then(|n| n.as_str()) == Some(tool_name)
                })
            })
    }

    /// The `tool_use.id` of the first `tool_use` block for `tool_name` in this
    /// assistant row, if any.
    pub fn tool_use_id(&self, tool_name: &str) -> Option<String> {
        if self.row_type() != "assistant" {
            return None;
        }
        self.json
            .pointer("/message/content")?
            .as_array()?
            .iter()
            .find(|b| {
                b.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                    && b.get("name").and_then(|n| n.as_str()) == Some(tool_name)
            })
            .and_then(|b| b.get("id").and_then(|i| i.as_str()).map(str::to_string))
    }

    /// True if this is a `user` row carrying a `tool_result` block whose
    /// `tool_use_id` matches `id`.
    pub fn is_tool_result_for(&self, id: &str) -> bool {
        if self.row_type() != "user" {
            return false;
        }
        self.json
            .pointer("/message/content")
            .and_then(|c| c.as_array())
            .is_some_and(|blocks| {
                blocks.iter().any(|b| {
                    b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                        && b.get("tool_use_id").and_then(|t| t.as_str()) == Some(id)
                })
            })
    }

    /// True if this is a `user` row carrying a `tool_result` block (any tool).
    pub fn is_tool_result(&self) -> bool {
        if self.row_type() != "user" {
            return false;
        }
        self.json
            .pointer("/message/content")
            .and_then(|c| c.as_array())
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
            })
    }
}

pub struct Scratch {
    pub root: PathBuf,
    pub projects: PathBuf,
    pub out: PathBuf,
}

impl Scratch {
    pub fn create(out: PathBuf) -> Result<Self> {
        let root = std::env::temp_dir().join(format!("amux-capture-{}", std::process::id()));
        let socket = root.join("sock/amux.sock");
        let socket_len = socket.as_os_str().len();
        if socket_len > 90 {
            bail!(
                "scratch socket path too long for a unix socket ({socket_len} bytes): {}",
                socket.display()
            );
        }
        for dir in ["config/amux", "data", "state", "sock", "tmp", "projects"] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        std::fs::create_dir_all(&out)?;
        std::fs::write(
            root.join("config/amux/config.yaml"),
            format!(
                "host_name: capture\nsocket_path: {}\nstate_path: {}\nenable_cloud_mode: false\nprevent_idle_sleep: false\n",
                socket.display(),
                root.join("state/state.yaml").display(),
            ),
        )?;
        Ok(Self {
            projects: root.join("projects"),
            out,
            root,
        })
    }

    fn socket_path(&self) -> PathBuf {
        self.root.join("sock/amux.sock")
    }

    /// A fresh git-initialized project dir seeded with `config.txt`.
    pub fn project_dir(&self, scenario: &str) -> Result<PathBuf> {
        let dir = self.projects.join(scenario);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("config.txt"), "VALUE=1\n")?;
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        };
        git(&["init", "-q"])?;
        git(&["add", "-A"])?;
        git(&[
            "-c",
            "user.email=capture@amux.test",
            "-c",
            "user.name=capture",
            "commit",
            "-qm",
            "seed",
        ])?;
        Ok(dir)
    }
}

/// The environment handed to the scratch daemon: a curated allowlist of the
/// parent environment plus scratch XDG overrides — never a blind inherit.
pub struct DaemonEnv {
    pub poisoned: bool,
}

impl DaemonEnv {
    fn apply(&self, cmd: &mut Command, scratch: &Scratch, target_debug: &Path) {
        cmd.env_clear();
        for key in ["HOME", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL"] {
            if let Ok(value) = std::env::var(key) {
                cmd.env(key, value);
            }
        }
        cmd.env("TERM", "xterm-256color");
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
        cmd.env("PATH", format!("{}:{path}", target_debug.display()));
        cmd.env("XDG_CONFIG_HOME", scratch.root.join("config"));
        cmd.env("XDG_DATA_HOME", scratch.root.join("data"));
        cmd.env("XDG_STATE_HOME", scratch.root.join("state"));
        cmd.env("TMPDIR", scratch.root.join("tmp"));
        if self.poisoned {
            // The exact marker set `ps eww` shows on a daemon whose ancestry
            // includes a Claude session (the transcript-persistence bug).
            cmd.env("CLAUDECODE", "1");
            cmd.env("CLAUDE_CODE_CHILD_SESSION", "1");
            cmd.env("CLAUDE_CODE_SESSION_ID", Uuid::new_v4().to_string());
            cmd.env("CLAUDE_PID", "99999");
            cmd.env("CLAUDE_EFFORT", "high");
            cmd.env("AI_AGENT", "capture-poison-probe");
            cmd.env("CLAUDE_CODE_ENTRYPOINT", "cli");
        }
    }
}

/// RAII guard around a spawned daemon child: kills + waits on drop unless
/// [`Self::disarm`] hands the child off. Holding this across the startup
/// connect loop means a startup-timeout bail cannot orphan the daemon.
struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn disarm(mut self) -> Child {
        self.0.take().expect("child taken twice")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The scratch daemon process; killed on drop.
pub struct ScratchDaemon {
    child: Child,
    pub client: Client,
}

impl Drop for ScratchDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub async fn start_daemon(scratch: &Scratch, env: &DaemonEnv) -> Result<ScratchDaemon> {
    let target_debug = target_debug_dir()?;
    let amux = target_debug.join("amux");
    if !amux.exists() {
        bail!(
            "amux binary missing at {} — run `cargo build -p amux-cli` first",
            amux.display()
        );
    }

    let mut cmd = Command::new(&amux);
    cmd.args(["server", "start", "--foreground"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(
            scratch.out.join("daemon.log"),
        )?))
        .stderr(Stdio::from(std::fs::File::create(
            scratch.out.join("daemon.err"),
        )?));
    env.apply(&mut cmd, scratch, &target_debug);
    // Hold the child in a RAII guard *before* the connect loop, so a startup-
    // timeout bail kills the daemon instead of orphaning it.
    let guard = ChildGuard(Some(cmd.spawn().context("spawning scratch daemon")?));

    let config = Config {
        socket_path: scratch.socket_path(),
        state_path: scratch.root.join("state/state.yaml"),
        ..Config::default()
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    let client = loop {
        match amux::Server::builder()
            .config(config.clone())
            .daemon()
            .open()
            .await
        {
            Ok(client) => break client,
            Err(error) => {
                if Instant::now() > deadline {
                    // `guard` drops here → child killed + waited.
                    return Err(anyhow!("scratch daemon did not come up: {error}"));
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    Ok(ScratchDaemon {
        child: guard.disarm(),
        client,
    })
}

fn target_debug_dir() -> Result<PathBuf> {
    // tests/capture is compiled into target/debug/deps; the workspace target
    // dir is two levels up from the manifest dir's sibling — resolve from the
    // current exe, which lives in <target>/debug/deps/.
    let exe = std::env::current_exe().context("current_exe")?;
    let deps = exe
        .parent()
        .ok_or_else(|| anyhow!("exe has no parent dir"))?;
    let debug = deps
        .parent()
        .ok_or_else(|| anyhow!("deps has no parent dir"))?;
    Ok(debug.to_path_buf())
}

/// A live capture session over one claude agent: the transcript subscription
/// recorded row by row, plus the raw PTY byte stream for menu debugging.
pub struct CaptureSession {
    pub agent_name: String,
    pub rows: Arc<Mutex<Vec<Row>>>,
    /// Lossy accumulated PTY screen bytes — used only to detect and answer
    /// claude's *startup* dialogs (workspace trust), never to interpret
    /// conversation state; the transcript rows are the truth for that.
    pub raw_screen: Arc<Mutex<String>>,
    pub keys_log: Vec<serde_json::Value>,
    client: Client,
    raw_task: tokio::task::JoinHandle<()>,
    rows_task: tokio::task::JoinHandle<()>,
}

impl CaptureSession {
    pub async fn open(
        daemon: &ScratchDaemon,
        scratch: &Scratch,
        scenario: &str,
        working_dir: PathBuf,
        extra_args: &[&str],
        model: &str,
    ) -> Result<Self> {
        let agent_name = format!("cap-{scenario}");
        let mut args = vec!["--model".to_string(), model.to_string()];
        args.extend(extra_args.iter().map(|s| s.to_string()));
        daemon
            .client
            .create_agent(CreateAgentRequest {
                agent_id: Uuid::new_v4(),
                host_id: None,
                name: Some(agent_name.clone()),
                agent_type: AgentType::Claude,
                working_dir,
                terminal_size: Some(TerminalSize {
                    rows: 45,
                    cols: 140,
                }),
                args,
            })
            .await
            .context("create claude agent")?;

        // Raw PTY subscription: debugging eyes on the menus (H.8 coexistence).
        let raw_stream = daemon
            .client
            .subscribe_session(SubscribeSessionRequest {
                agent: agent_name.as_str().into(),
                io_protocol: RAW_V1.to_string(),
                args: None,
            })
            .await
            .context("subscribe raw")?;
        let raw_path = scratch.out.join(format!("{scenario}.raw.log"));
        let raw_screen: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let raw_screen_clone = raw_screen.clone();
        let raw_task = tokio::spawn(async move {
            let mut stream = raw_stream;
            let Ok(mut file) = std::fs::File::create(&raw_path) else {
                return;
            };
            while let Ok(event) = stream.recv().await {
                match event {
                    SubscribeSessionEvent::Output { payload } => {
                        let _ = file.write_all(&payload);
                        let _ = file.flush();
                        let mut screen = raw_screen_clone.lock().await;
                        screen.push_str(&String::from_utf8_lossy(&payload));
                        // Bounded: startup-dialog detection needs a window,
                        // not the whole scrollback.
                        if screen.len() > 256 * 1024 {
                            let cut = screen.len() - 128 * 1024;
                            screen.drain(..cut);
                        }
                    }
                    SubscribeSessionEvent::Closed { .. } => break,
                    _ => {}
                }
            }
        });

        let transcript_stream = daemon
            .client
            .subscribe_session(SubscribeSessionRequest {
                agent: agent_name.as_str().into(),
                io_protocol: PTY_TRANSCRIPT_V1.to_string(),
                args: None,
            })
            .await
            .context("subscribe transcript")?;
        let rows: Arc<Mutex<Vec<Row>>> = Arc::new(Mutex::new(Vec::new()));
        let rows_clone = rows.clone();
        let rows_path = scratch.out.join(format!("{scenario}.rows.jsonl"));
        let rows_task = tokio::spawn(async move {
            let mut stream = transcript_stream;
            let Ok(mut file) = std::fs::File::create(&rows_path) else {
                return;
            };
            while let Ok(event) = stream.recv().await {
                match event {
                    SubscribeSessionEvent::Output { payload } => {
                        let Ok(output) = decode_pty_transcript_v1_output(&payload) else {
                            continue;
                        };
                        let raw = String::from_utf8_lossy(&output.payload).to_string();
                        let json: serde_json::Value =
                            serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
                        let _ = writeln!(file, "{raw}");
                        let _ = file.flush();
                        rows_clone.lock().await.push(Row {
                            seq: output.seq_id,
                            raw,
                            json,
                        });
                    }
                    SubscribeSessionEvent::Closed { .. } => break,
                    _ => {}
                }
            }
        });

        Ok(Self {
            agent_name,
            rows,
            raw_screen,
            keys_log: Vec::new(),
            client: daemon.client.clone(),
            raw_task,
            rows_task,
        })
    }

    /// A snapshot copy of the recorded rows so far — for structural assertions
    /// on what the capture actually contains.
    pub async fn snapshot(&self) -> Vec<Row> {
        self.rows.lock().await.clone()
    }

    /// Prepare the session to accept its first prompt: answer claude's
    /// startup dialogs and wait until the composer is up.
    ///
    /// Important sequencing fact (verified): Claude Code creates the
    /// transcript **file** lazily on the first user turn, not at SessionStart
    /// — the SessionStart hook reports the intended `transcript_path` before
    /// the file exists, and amux's tailer only emits `amux.transcript_ready`
    /// once the file appears. So the harness must send the first prompt
    /// *before* it can observe `transcript_ready`; gating the first prompt on
    /// readiness deadlocks.
    ///
    /// Currently handled: the fresh-directory workspace-trust prompt ("Quick
    /// safety check" — Enter confirms the preselected "Yes, I trust this
    /// folder").
    pub async fn prepare_for_first_prompt(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut trust_answered = false;
        loop {
            // The TUI positions words with cursor-move escapes, so multi-word
            // matches never fire; single words survive intact.
            let screen = self.raw_screen.lock().await.clone();
            let seen_trust_prompt = screen.contains("safety") && screen.contains("folder");
            // The composer footer hint ("... for agents") marks a live prompt.
            let composer_up = screen.contains("for agents");
            if !trust_answered && seen_trust_prompt && !composer_up {
                tokio::time::sleep(Duration::from_millis(800)).await;
                self.send_keys(
                    "workspace trust dialog: Enter (preselected 'Yes, I trust this folder')",
                    vec![ClaudePtyTranscriptV1Action::Write(b"\r".to_vec())],
                )
                .await?;
                trust_answered = true;
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            if composer_up {
                // Let the composer settle before typing.
                tokio::time::sleep(Duration::from_millis(800)).await;
                return Ok(());
            }
            if Instant::now() > deadline {
                bail!("timed out after {timeout:?} waiting for the claude composer to come up");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Wait (bounded) for `amux.transcript_ready` — the replay→live boundary.
    /// Sent only after the first turn has begun (see `prepare_for_first_prompt`).
    pub async fn wait_for_transcript_ready(&self, timeout: Duration) -> Result<usize> {
        self.wait_for_row(0, timeout, "amux.transcript_ready", |row| {
            row.row_type() == "amux.transcript_ready"
        })
        .await
    }

    /// Wait (bounded) until some row at/after `from_index` satisfies `pred`.
    /// Returns the index *after* the matching row.
    pub async fn wait_for_row(
        &self,
        from_index: usize,
        timeout: Duration,
        what: &str,
        pred: impl Fn(&Row) -> bool,
    ) -> Result<usize> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let rows = self.rows.lock().await;
                for (index, row) in rows.iter().enumerate().skip(from_index) {
                    if pred(row) {
                        return Ok(index + 1);
                    }
                }
            }
            if Instant::now() > deadline {
                bail!("timed out after {timeout:?} waiting for {what}");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Wait for a turn-end signal (amux `hook.stop` or `system/turn_duration`).
    pub async fn wait_for_turn_end(&self, from_index: usize, timeout: Duration) -> Result<usize> {
        self.wait_for_row(from_index, timeout, "turn end", |row| {
            row.row_type() == "hook.stop"
                || (row.row_type() == "system"
                    && row.json.get("subtype").and_then(|s| s.as_str()) == Some("turn_duration"))
        })
        .await
    }

    /// Inject keystrokes under the seq guard, with server-side pacing.
    /// `note` records the intent in the keys log that lands in the meta file.
    pub async fn send_keys(
        &mut self,
        note: &str,
        actions: Vec<ClaudePtyTranscriptV1Action>,
    ) -> Result<()> {
        let printable: Vec<String> = actions
            .iter()
            .map(|action| match action {
                ClaudePtyTranscriptV1Action::Write(bytes) => {
                    format!("write {:?}", String::from_utf8_lossy(bytes))
                }
                ClaudePtyTranscriptV1Action::DelayMs(ms) => format!("delay {ms}ms"),
            })
            .collect();
        for attempt in 0..5 {
            let expected_seq = self
                .rows
                .lock()
                .await
                .last()
                .map(|row| row.seq)
                .unwrap_or(0);
            let payload = encode_pty_transcript_v1_input(ClaudePtyTranscriptV1Input {
                expected_seq,
                actions: actions.clone(),
            });
            let result = self
                .client
                .send_input(SendInputRequest {
                    agent: self.agent_name.as_str().into(),
                    io_protocol: PTY_TRANSCRIPT_V1.to_string(),
                    payload: payload.into(),
                })
                .await;
            match result {
                Ok(()) => {
                    self.keys_log.push(serde_json::json!({
                        "note": note,
                        "actions": printable,
                        "seq_retries": attempt,
                    }));
                    return Ok(());
                }
                Err(amux::ClientError::Protocol(ProtocolError::SequenceNumberMismatch {
                    ..
                })) => {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }
                Err(error) => return Err(anyhow!("send_input ({note}): {error}")),
            }
        }
        bail!("send_input ({note}): seq mismatch persisted after 5 retries")
    }

    /// Type a prompt and submit it with CR.
    pub async fn send_prompt(&mut self, prompt: &str) -> Result<()> {
        self.send_keys(
            &format!("prompt: {prompt}"),
            vec![
                ClaudePtyTranscriptV1Action::Write(prompt.as_bytes().to_vec()),
                ClaudePtyTranscriptV1Action::DelayMs(300),
                ClaudePtyTranscriptV1Action::Write(b"\r".to_vec()),
            ],
        )
        .await
    }

    /// Close the capture: delete the agent and stop the recorder tasks.
    pub async fn close(self) -> Result<Vec<serde_json::Value>> {
        let _ = self.client.delete_agent(self.agent_name.as_str()).await;
        // Give the recorders a moment to observe the close, then stop them.
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.raw_task.abort();
        self.rows_task.abort();
        Ok(self.keys_log)
    }
}

/// `claude --version` output, for provenance stamps.
pub fn claude_version() -> String {
    Command::new("claude")
        .arg("--version")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

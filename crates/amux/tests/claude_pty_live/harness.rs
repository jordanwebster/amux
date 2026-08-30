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

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use amux::claude_io::{
    ClaudePtyTranscriptV1Input, Intent, PTY_TRANSCRIPT_V1, decode_pty_transcript_v1_output,
    encode_pty_transcript_v1_input,
};
use amux::terminal_io::TERMINAL_V1;
use amux::{
    AgentType, Client, Config, CreateAgentRequest, ProtocolError, SendInputRequest,
    SubscribeSessionEvent, SubscribeSessionRequest, TerminalSize,
};
use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::Mutex;
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use super::depfile::assert_binary_is_current;

#[derive(Default)]
pub(super) struct RecorderState {
    live_tasks: AtomicUsize,
}

impl RecorderState {
    pub(super) fn enter(self: &Arc<Self>) -> LiveRecorder {
        self.live_tasks.fetch_add(1, Ordering::SeqCst);
        LiveRecorder(self.clone())
    }

    pub(super) fn live_tasks(&self) -> usize {
        self.live_tasks.load(Ordering::SeqCst)
    }

    pub(super) async fn wait_stopped(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.live_tasks() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("recorder tasks did not stop after cancellation")
    }
}

pub(super) struct LiveRecorder(Arc<RecorderState>);

impl Drop for LiveRecorder {
    fn drop(&mut self) {
        self.0.live_tasks.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) struct RecorderTasks {
    raw: Option<JoinHandle<()>>,
    rows: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub enum PtyTestAction {
    Write(Vec<u8>),
    DelayMs(u64),
}

impl RecorderTasks {
    pub(super) fn new(raw: JoinHandle<()>, rows: JoinHandle<()>) -> Self {
        Self {
            raw: Some(raw),
            rows: Some(rows),
        }
    }

    fn abort(&self) {
        if let Some(task) = &self.raw {
            task.abort();
        }
        if let Some(task) = &self.rows {
            task.abort();
        }
    }

    async fn stop(&mut self) {
        self.abort();
        if let Some(task) = self.raw.take() {
            let _ = task.await;
        }
        if let Some(task) = self.rows.take() {
            let _ = task.await;
        }
    }
}

impl Drop for RecorderTasks {
    fn drop(&mut self) {
        self.abort();
    }
}

#[derive(Clone, Default)]
struct ActiveSessionRegistry {
    inner: Arc<std::sync::Mutex<Option<ActiveSession>>>,
}

struct ActiveSession {
    agent_name: String,
    raw: Option<AbortHandle>,
    rows: Option<AbortHandle>,
    recorder_state: Arc<RecorderState>,
}

impl ActiveSessionRegistry {
    fn register(&self, agent_name: String, recorder_state: Arc<RecorderState>) {
        let mut active = self.inner.lock().expect("active session registry poisoned");
        assert!(active.is_none(), "capture scenarios must run sequentially");
        *active = Some(ActiveSession {
            agent_name,
            raw: None,
            rows: None,
            recorder_state,
        });
    }

    fn attach_raw(&self, agent_name: &str, task: AbortHandle) {
        let mut active = self.inner.lock().expect("active session registry poisoned");
        let active = active.as_mut().expect("capture session was not registered");
        assert_eq!(active.agent_name, agent_name);
        active.raw = Some(task);
    }

    fn attach_rows(&self, agent_name: &str, task: AbortHandle) {
        let mut active = self.inner.lock().expect("active session registry poisoned");
        let active = active.as_mut().expect("capture session was not registered");
        assert_eq!(active.agent_name, agent_name);
        active.rows = Some(task);
    }

    fn disarm(&self, agent_name: &str) {
        let mut active = self.inner.lock().expect("active session registry poisoned");
        if active
            .as_ref()
            .is_some_and(|active| active.agent_name == agent_name)
        {
            active.take();
        }
    }

    async fn cancel(&self, client: &Client) -> Result<()> {
        let active = self
            .inner
            .lock()
            .expect("active session registry poisoned")
            .take();
        let Some(active) = active else {
            return Ok(());
        };
        if let Some(task) = active.raw {
            task.abort();
        }
        if let Some(task) = active.rows {
            task.abort();
        }
        let recorder_result = active.recorder_state.wait_stopped().await;
        let delete_result = client
            .delete_agent(active.agent_name.as_str())
            .await
            .context("delete canceled capture agent");
        recorder_result?;
        delete_result?;
        Ok(())
    }
}

/// One received transcript-stream row.
#[derive(Clone)]
pub struct Row {
    pub seq: u64,
    pub json: serde_json::Value,
}

impl Row {
    pub fn row_type(&self) -> &str {
        self.json.get("type").and_then(|t| t.as_str()).unwrap_or("")
    }

    fn blocks(&self) -> &[serde_json::Value] {
        crate::structure::message_blocks(&self.json)
    }

    /// True if this is an `assistant` row carrying a `tool_use` block for the
    /// named tool.
    pub fn is_tool_use(&self, tool_name: &str) -> bool {
        self.row_type() == "assistant"
            && self.blocks().iter().any(|b| {
                b.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                    && b.get("name").and_then(|n| n.as_str()) == Some(tool_name)
            })
    }

    /// The `tool_use.id` of the first `tool_use` block for `tool_name` in this
    /// assistant row, if any.
    pub fn tool_use_id(&self, tool_name: &str) -> Option<String> {
        crate::structure::tool_use_id(&self.json, tool_name).map(str::to_string)
    }

    /// True if this is a `user` row carrying a `tool_result` block whose
    /// `tool_use_id` matches `id`.
    pub fn is_tool_result_for(&self, id: &str) -> bool {
        self.row_type() == "user"
            && self.blocks().iter().any(|b| {
                b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                    && b.get("tool_use_id").and_then(|t| t.as_str()) == Some(id)
            })
    }

    /// True if this is a `user` row carrying a `tool_result` block (any tool).
    pub fn is_tool_result(&self) -> bool {
        self.row_type() == "user"
            && self
                .blocks()
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
    }

    /// True for the transcript's turn-end authority row
    /// (`system`/`turn_duration`).
    pub fn is_turn_duration(&self) -> bool {
        self.row_type() == "system"
            && self.json.get("subtype").and_then(|s| s.as_str()) == Some("turn_duration")
    }

    /// True if this is a parsed permission-request hook for `tool_name`.
    pub fn is_permission_request_for(&self, tool_name: &str) -> bool {
        if tool_name == "ExitPlanMode" {
            return crate::structure::is_exit_plan_request(&self.json);
        }
        self.row_type() == "hook.permission_request"
            && self.json.get("tool_name").and_then(|name| name.as_str()) == Some(tool_name)
    }

    pub fn plan_resolution(&self) -> Option<(&str, crate::structure::PlanOutcome)> {
        crate::structure::plan_resolution(&self.json)
    }

    /// True if this row structurally carries AskUserQuestion answers.
    pub fn has_question_answers(&self) -> bool {
        self.json
            .pointer("/toolUseResult/answers")
            .is_some_and(serde_json::Value::is_object)
    }
}

pub struct Scratch {
    pub root: PathBuf,
    pub projects: PathBuf,
    pub out: PathBuf,
    active_session: ActiveSessionRegistry,
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
            active_session: ActiveSessionRegistry::default(),
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
        // The capture project owns an observable synchronous hook beside the
        // managed launch's asynchronous hook. Both exercise the same daemon
        // seam while this one also records the raw event and messaging env.
        let registration = || {
            serde_json::json!([{
                "hooks": [{
                    "type": "command",
                    "command": "./.claude/amux-capture-hook.sh"
                }]
            }])
        };
        let hooks = serde_json::json!({
            "SessionStart": registration(),
            "SessionEnd": registration(),
            "PermissionRequest": registration(),
            "Stop": registration(),
            "Notification": registration()
        });
        let claude_dir = dir.join(".claude");
        std::fs::create_dir_all(&claude_dir)?;
        std::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_vec_pretty(&serde_json::json!({ "hooks": hooks }))?,
        )?;
        let hook_script = claude_dir.join("amux-capture-hook.sh");
        std::fs::write(
            &hook_script,
            "#!/bin/sh\n\
             env | grep '^CLAUDE_CODE_MESSAGING_' > .claude/messaging-env || true\n\
             cat > .claude/last-hook.json\n\
             exec amux hooks claude < .claude/last-hook.json\n",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&hook_script, std::fs::Permissions::from_mode(0o700))?;
        }
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

    /// Finish cleanup for a scenario future that returned or was canceled
    /// without consuming [`CaptureSession::close`]. Recorder tasks are fully
    /// stopped before the caller reads their output during finalization.
    pub async fn cancel_active_session(&self, client: &Client) -> Result<()> {
        self.active_session.cancel(client).await
    }
}

/// Claude auto-update kill switches, present in every capture environment.
///
/// INCIDENT (Phase 2 hardening): during the Phase 0 captures, claude's
/// auto-updater ran inside the scratch env, downloaded a version into the
/// scratch `XDG_DATA_HOME` (`…/data/claude/versions/2.1.228`) and
/// REPOINTED the owner's real `~/.local/bin/claude` launcher symlink at
/// that temp dir. The launcher path is derived from `homedir()` in the
/// 2.1.228 binary — `join(homedir(), ".local/bin/claude")`, not
/// overridable by any env — and the harness must keep the real HOME
/// (keychain auth requires it, Phase 0). So the only safe stance is to
/// prevent the installer from ever running: `DISABLE_AUTOUPDATER` turns
/// off background auto-updates, `DISABLE_UPDATES` is the hard lock
/// (`claude update` itself refuses under it), and
/// `DISABLE_INSTALLATION_CHECKS` disables the install-repair path — all
/// three verified against the 2.1.228 binary's env registry.
const CLAUDE_UPDATE_GUARDS: &[(&str, &str)] = &[
    ("DISABLE_AUTOUPDATER", "1"),
    ("DISABLE_UPDATES", "1"),
    ("DISABLE_INSTALLATION_CHECKS", "1"),
];

/// The environment handed to the scratch daemon: a curated allowlist of the
/// parent environment plus scratch XDG overrides — never a blind inherit.
/// Spawned claudes inherit this environment, so everything here reaches
/// them.
pub struct DaemonEnv {
    pub poisoned: bool,
}

impl DaemonEnv {
    /// The full environment as a map — pure, so the guard assertion below
    /// checks exactly what the daemon (and every claude under it) receives.
    fn env_map(&self, scratch: &Scratch, target_debug: &Path) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        for key in ["HOME", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL"] {
            if let Ok(value) = std::env::var(key) {
                env.insert(key.to_string(), value);
            }
        }
        env.insert("TERM".into(), "xterm-256color".into());
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
        env.insert("PATH".into(), format!("{}:{path}", target_debug.display()));
        env.insert(
            "XDG_CONFIG_HOME".into(),
            scratch.root.join("config").display().to_string(),
        );
        env.insert(
            "XDG_DATA_HOME".into(),
            scratch.root.join("data").display().to_string(),
        );
        env.insert(
            "XDG_STATE_HOME".into(),
            scratch.root.join("state").display().to_string(),
        );
        env.insert(
            "TMPDIR".into(),
            scratch.root.join("tmp").display().to_string(),
        );
        for (key, value) in CLAUDE_UPDATE_GUARDS {
            env.insert((*key).into(), (*value).into());
        }
        if self.poisoned {
            // The exact marker set `ps eww` shows on a daemon whose ancestry
            // includes a Claude session (the transcript-persistence bug).
            env.insert("CLAUDECODE".into(), "1".into());
            env.insert("CLAUDE_CODE_CHILD_SESSION".into(), "1".into());
            env.insert("CLAUDE_CODE_SESSION_ID".into(), Uuid::new_v4().to_string());
            env.insert("CLAUDE_PID".into(), "99999".into());
            env.insert("CLAUDE_EFFORT".into(), "high".into());
            env.insert("AI_AGENT".into(), "capture-poison-probe".into());
            env.insert("CLAUDE_CODE_ENTRYPOINT".into(), "cli".into());
        }
        env
    }

    fn apply(&self, cmd: &mut Command, scratch: &Scratch, target_debug: &Path) {
        let env = self.env_map(scratch, target_debug);
        // The incident guard, asserted on every capture run (this binary
        // has no #[test] harness — the next scheduled capture validates
        // live): the spawned claude must never be able to run its
        // installer against the owner's real launcher symlink.
        for (key, value) in CLAUDE_UPDATE_GUARDS {
            assert_eq!(
                env.get(*key).map(String::as_str),
                Some(*value),
                "capture env must carry the auto-update guard {key}"
            );
        }
        cmd.env_clear();
        for (key, value) in env {
            cmd.env(key, value);
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

impl ScratchDaemon {
    /// Deliver a hook through the exact CLI seam Claude registrations use.
    /// `managed_agent` sets the child process's AMUX_AGENT_ID; `None` models
    /// an external Claude process and lets the hook's session_id bootstrap a
    /// readonly agent. The parent process environment is never mutated.
    pub fn deliver_hook(
        &self,
        scratch: &Scratch,
        managed_agent: Option<Uuid>,
        payload: &serde_json::Value,
    ) -> Result<()> {
        let amux = target_debug_dir()?.join("amux");
        let mut command = Command::new(amux);
        command
            .args(["hooks", "claude"])
            .env(
                "XDG_CONFIG_HOME",
                scratch.root.join("config").display().to_string(),
            )
            .env_remove("AMUX_AGENT_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(agent) = managed_agent {
            command.env("AMUX_AGENT_ID", agent.to_string());
        }
        let mut child = command.spawn().context("spawn amux hooks claude")?;
        child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("hook child had no stdin"))?
            .write_all(serde_json::to_string(payload)?.as_bytes())?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!(
                "amux hooks claude exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }
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
    assert_binary_is_current(&amux)?;

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
    // This test binary lives in <target>/debug/deps/; the amux binary lands
    // one level up, in <target>/debug/.
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
    agent_id: Uuid,
    agent_name: String,
    rows: Arc<Mutex<Vec<Row>>>,
    /// Lossy accumulated PTY screen bytes — used only to detect and answer
    /// claude's *startup* dialogs (workspace trust), never to interpret
    /// conversation state; the transcript rows are the truth for that.
    raw_screen: Arc<Mutex<String>>,
    raw_closed: Arc<AtomicBool>,
    rows_closed: Arc<AtomicBool>,
    keys_log: Vec<serde_json::Value>,
    client: Client,
    recorder_state: Arc<RecorderState>,
    recorder_tasks: RecorderTasks,
    active_session: ActiveSessionRegistry,
}

impl CaptureSession {
    pub async fn open(
        daemon: &ScratchDaemon,
        scratch: &Scratch,
        scenario: &str,
        working_dir: PathBuf,
        extra_args: &[String],
        model: &str,
    ) -> Result<Self> {
        let agent_name = format!("cap-{scenario}");
        let mut args = vec!["--model".to_string(), model.to_string()];
        args.extend(extra_args.iter().cloned());
        let agent_id = Uuid::new_v4();
        let agent = daemon
            .client
            .create_agent(CreateAgentRequest {
                agent_id,
                host_id: None,
                name: Some(agent_name.clone()),
                agent_type: AgentType::Claude {
                    driver: amux::ClaudeDriver::Pty,
                },
                working_dir,
                terminal_size: Some(TerminalSize {
                    rows: 45,
                    cols: 140,
                }),
                args,
                parent: None,
                initial_prompt: None,
            })
            .await
            .context("create claude agent")?;
        debug_assert_eq!(agent.id, agent_id);
        let recorder_state = Arc::new(RecorderState::default());
        scratch
            .active_session
            .register(agent_name.clone(), recorder_state.clone());

        // Raw PTY subscription: debugging eyes on the menus (H.8 coexistence).
        let raw_stream = daemon
            .client
            .subscribe_session(SubscribeSessionRequest {
                agent: agent_name.as_str().into(),
                io_protocol: TERMINAL_V1.to_string(),
                args: None,
            })
            .await
            .context("subscribe raw")?;
        let raw_path = scratch.out.join(format!("{scenario}.raw.log"));
        let raw_screen: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let raw_screen_clone = raw_screen.clone();
        let raw_closed = Arc::new(AtomicBool::new(false));
        let raw_closed_task = raw_closed.clone();
        let raw_live = recorder_state.enter();
        let raw_task = tokio::spawn(async move {
            let _live = raw_live;
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
            raw_closed_task.store(true, Ordering::SeqCst);
        });
        scratch
            .active_session
            .attach_raw(&agent_name, raw_task.abort_handle());

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
        let rows_closed = Arc::new(AtomicBool::new(false));
        let rows_closed_task = rows_closed.clone();
        let rows_path = scratch.out.join(format!("{scenario}.rows.jsonl"));
        let rows_live = recorder_state.enter();
        let rows_task = tokio::spawn(async move {
            let _live = rows_live;
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
                            json,
                        });
                    }
                    SubscribeSessionEvent::Closed { .. } => break,
                    _ => {}
                }
            }
            rows_closed_task.store(true, Ordering::SeqCst);
        });
        scratch
            .active_session
            .attach_rows(&agent_name, rows_task.abort_handle());

        Ok(Self {
            agent_id,
            agent_name,
            rows,
            raw_screen,
            raw_closed,
            rows_closed,
            keys_log: Vec::new(),
            client: daemon.client.clone(),
            recorder_state,
            recorder_tasks: RecorderTasks::new(raw_task, rows_task),
            active_session: scratch.active_session.clone(),
        })
    }

    /// A snapshot copy of the recorded rows so far — for structural assertions
    /// on what the capture actually contains.
    pub async fn snapshot(&self) -> Vec<Row> {
        self.rows.lock().await.clone()
    }

    pub fn agent_id(&self) -> Uuid {
        self.agent_id
    }

    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    pub async fn current_seq(&self) -> u64 {
        self.rows.lock().await.last().map_or(0, |row| row.seq)
    }

    pub async fn raw_len(&self) -> usize {
        self.raw_screen.lock().await.len()
    }

    /// Whether the live terminal byte stream contained `needle`. Capture
    /// scenarios use this only for terminal-title/listing probes; transcript
    /// rows remain the authority for conversation assertions.
    pub async fn raw_contains(&self, needle: &str) -> bool {
        self.raw_screen.lock().await.contains(needle)
    }

    pub fn streams_open(&self) -> bool {
        !self.raw_closed.load(Ordering::SeqCst) && !self.rows_closed.load(Ordering::SeqCst)
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
        let mut imports_answered = false;
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
                    vec![PtyTestAction::Write(b"\r".to_vec())],
                )
                .await?;
                trust_answered = true;
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            // Claude 2.1.240 follows the workspace-trust prompt with this
            // import confirmation when the project has an AGENTS.md outside
            // the scenario directory. The capture project is created under
            // this checked-out tree, so accepting this known local file is
            // part of reaching the otherwise identical first prompt.
            let seen_import_prompt = screen.contains("external") && screen.contains("imports");
            if !imports_answered && seen_import_prompt && !composer_up {
                tokio::time::sleep(Duration::from_millis(800)).await;
                self.send_keys(
                    "external instructions dialog: Enter (allow checked-out AGENTS.md)",
                    vec![PtyTestAction::Write(b"\r".to_vec())],
                )
                .await?;
                imports_answered = true;
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
            row.row_type() == "hook.stop" || row.is_turn_duration()
        })
        .await
    }

    /// Drive the raw terminal for process-level scenarios that explicitly
    /// exercise terminal fanout, socket fallback, or raw attach.
    pub async fn send_keys(&mut self, note: &str, actions: Vec<PtyTestAction>) -> Result<()> {
        let printable: Vec<String> = actions
            .iter()
            .map(|action| match action {
                PtyTestAction::Write(bytes) => {
                    format!("write {:?}", String::from_utf8_lossy(bytes))
                }
                PtyTestAction::DelayMs(ms) => format!("delay {ms}ms"),
            })
            .collect();
        for action in actions {
            match action {
                PtyTestAction::Write(bytes) => self.send_raw(&bytes).await?,
                PtyTestAction::DelayMs(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
            }
        }
        self.keys_log.push(serde_json::json!({
            "note": note,
            "raw_actions": printable,
        }));
        Ok(())
    }

    pub async fn send_intent(&mut self, note: &str, intent: Intent) -> Result<()> {
        let input_id = Uuid::new_v4().as_bytes().to_vec();
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
                intent: intent.clone(),
            });
            let result = self
                .client
                .send_input(SendInputRequest {
                    agent: self.agent_name.as_str().into(),
                    input_id: input_id.clone(),
                    io_protocol: PTY_TRANSCRIPT_V1.to_string(),
                    payload: payload.into(),
                })
                .await;
            match result {
                Ok(()) => {
                    self.keys_log.push(serde_json::json!({
                        "note": note,
                        "intent": intent,
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

    /// Raw-PTY input for H.8. The structured recorder remains subscribed;
    /// the resulting transcript row proves raw input did not disturb chat.
    pub async fn send_raw(&self, payload: &[u8]) -> Result<()> {
        self.client
            .send_input(SendInputRequest {
                agent: self.agent_name.as_str().into(),
                input_id: Uuid::new_v4().as_bytes().to_vec(),
                io_protocol: TERMINAL_V1.to_string(),
                payload: payload.to_vec().into(),
            })
            .await
            .context("send raw input")
    }

    /// Type a prompt and submit it with CR.
    pub async fn send_prompt(&mut self, prompt: &str) -> Result<()> {
        self.send_intent(
            &format!("prompt: {prompt}"),
            Intent::Prompt {
                text: prompt.to_owned(),
            },
        )
        .await
    }

    /// Close the capture: delete the agent, stop the recorder tasks, and hand
    /// back the keystroke log (a JSON array) for the scenario's meta notes.
    pub async fn close(mut self) -> Result<serde_json::Value> {
        self.client
            .delete_agent(self.agent_name.as_str())
            .await
            .context("delete capture agent")?;
        // Give the recorders a moment to observe the close, then stop them.
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.recorder_tasks.stop().await;
        self.recorder_state.wait_stopped().await?;
        self.active_session.disarm(&self.agent_name);
        Ok(serde_json::Value::Array(std::mem::take(&mut self.keys_log)))
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

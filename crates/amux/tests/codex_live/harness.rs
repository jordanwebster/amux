//! Isolated daemon, structured-row, raw-terminal, and process helpers.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use amux::codex_io::{CODEX_SDK_V1, CodexSdkV1Input, decode_codex_sdk_v1_output};
use amux::terminal_io::{TERMINAL_V1, TerminalV1Args, encode_terminal_v1_args};
use amux::{
    AgentIdentifier, AgentType, Client, Config, CreateAgentRequest, SendInputRequest,
    SubscribeSessionEvent, SubscribeSessionRequest, TerminalSize,
};
use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use super::depfile::assert_binary_is_current;
use super::structure::{self, Matcher, Row};

pub const READY_TIMEOUT: Duration = Duration::from_secs(90);
pub const TURN_TIMEOUT: Duration = Duration::from_secs(240);
pub const RAW_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Scratch {
    _temp: TempDir,
    pub root: PathBuf,
    pub project: PathBuf,
    pub out: PathBuf,
    pub config: Config,
    pub config_path: PathBuf,
    pub codex_home: PathBuf,
}

impl Scratch {
    pub fn create(out: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&out)?;
        let temp = tempfile::Builder::new()
            .prefix("amux-codex-capture-")
            .tempdir_in("/tmp")
            .context("create Codex live scratch directory")?;
        let root = temp
            .path()
            .canonicalize()
            .context("canonicalize Codex live scratch directory")?;
        for dir in [
            "config",
            "data",
            "state",
            "sock",
            "tmp",
            "project",
            "codex-home",
        ] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        std::fs::set_permissions(root.join("sock"), std::fs::Permissions::from_mode(0o700))?;

        let codex_home = root.join("codex-home");
        seed_codex_auth(&codex_home)?;
        let config = Config {
            host_name: "codex-capture".into(),
            socket_path: root.join("sock/amux.sock"),
            state_path: root.join("state/state.yaml"),
            enable_cloud_mode: Some(false),
            prevent_idle_sleep: Some(false),
            ..Config::default()
        };
        let config_path = root.join("config/amux.yaml");
        std::fs::write(&config_path, serde_yaml::to_string(&config)?)?;
        let project = root.join("project");
        let project_key = serde_json::to_string(&project.display().to_string())?;
        std::fs::write(
            codex_home.join("config.toml"),
            format!("[projects.{project_key}]\ntrust_level = \"trusted\"\n"),
        )?;

        Ok(Self {
            _temp: temp,
            root,
            project,
            out,
            config,
            config_path,
            codex_home,
        })
    }

    pub fn app_server_socket(&self) -> PathBuf {
        self.codex_home
            .join("app-server-control/app-server-control.sock")
    }
}

fn seed_codex_auth(destination: &Path) -> Result<()> {
    let source_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or_else(|| anyhow!("CODEX_HOME or HOME is required for a real-Codex run"))?;
    let source = source_home.join("auth.json");
    if !source.exists() {
        bail!(
            "Codex authentication is unavailable at {}; run `codex login` first",
            source.display()
        );
    }
    std::fs::copy(&source, destination.join("auth.json"))
        .with_context(|| format!("copy isolated Codex auth from {}", source.display()))?;
    // Codex 0.147 refuses a pristine CODEX_HOME before starting app-server,
    // even when auth.json is valid. Seed only its non-secret local setup
    // sentinels; do not copy config.toml, plugins, MCP configuration, history,
    // or the owner's installation identifier into the capture environment.
    std::fs::write(destination.join(".personality_migration"), "v1\n")?;
    std::fs::write(destination.join(".sandbox_migration"), "v1\n")?;
    std::fs::write(
        destination.join("installation_id"),
        format!("{}\n", Uuid::new_v4()),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(
            destination.join("auth.json"),
            std::fs::Permissions::from_mode(0o600),
        )?;
    }
    Ok(())
}

pub struct ScratchDaemon {
    child: Child,
    pub client: Client,
}

impl ScratchDaemon {
    async fn wait_for_exit(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.child.kill()?;
                let _ = self.child.wait();
                bail!("scratch amux daemon did not exit within 20s");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Drop for ScratchDaemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub struct Harness {
    pub scratch: Scratch,
    daemon: Option<ScratchDaemon>,
}

impl Harness {
    pub async fn start(out: PathBuf) -> Result<Self> {
        let scratch = Scratch::create(out)?;
        let daemon = start_daemon(&scratch).await?;
        Ok(Self {
            scratch,
            daemon: Some(daemon),
        })
    }

    pub fn client(&self) -> &Client {
        &self.daemon.as_ref().expect("daemon is running").client
    }

    pub async fn stop_for_suspend(&mut self) -> Result<()> {
        let mut daemon = self.daemon.take().expect("daemon is running");
        daemon.wait_for_exit().await
    }

    pub async fn restart(&mut self) -> Result<()> {
        if self.daemon.is_some() {
            bail!("cannot restart a running scratch daemon");
        }
        self.daemon = Some(start_daemon(&self.scratch).await?);
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        let Some(mut daemon) = self.daemon.take() else {
            kill_capture_app_server(&self.scratch).ok();
            return Ok(());
        };
        let _ = daemon.client.shutdown().await;
        let wait = daemon.wait_for_exit().await;
        kill_capture_app_server(&self.scratch).ok();
        wait
    }

    /// `scenario: None` creates an agent with no name — the product default
    /// for `amux new codex`, and the case a hardcoded `Some(..)` here hid:
    /// naming a thread is what persists it, so an unnamed agent exercised a
    /// materially different path that nothing covered.
    pub async fn create_agent(
        &self,
        scenario: Option<&str>,
        model: &str,
        working_dir: &Path,
    ) -> Result<Uuid> {
        let agent_id = Uuid::new_v4();
        let agent = self
            .client()
            .create_agent(CreateAgentRequest {
                agent_id,
                host_id: None,
                name: scenario.map(|scenario| format!("codex-c-{scenario}")),
                agent_type: AgentType::Codex {
                    model: Some(model.to_string()),
                    approval_policy: Some("on-request".into()),
                    sandbox_policy: Some("read-only".into()),
                    resume_thread_id: None,
                },
                working_dir: working_dir.to_path_buf(),
                terminal_size: Some(TerminalSize {
                    rows: 45,
                    cols: 140,
                }),
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            })
            .await
            .context("create real Codex agent")?;
        if agent.id != agent_id {
            bail!("create returned the wrong agent id");
        }
        Ok(agent_id)
    }
}

async fn start_daemon(scratch: &Scratch) -> Result<ScratchDaemon> {
    let amux = target_debug_dir()?.join("amux");
    assert_binary_is_current(&amux)?;
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(scratch.out.join("daemon.log"))?;
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(scratch.out.join("daemon.err"))?;
    let mut command = Command::new(amux);
    command
        .args([
            "--config",
            scratch
                .config_path
                .to_str()
                .context("non-UTF-8 config path")?,
            "server",
            "start",
            "--foreground",
        ])
        .env("CODEX_HOME", &scratch.codex_home)
        .env("AMUX_CONFIG", &scratch.config_path)
        .env("AMUX_LIVE_OUT", &scratch.out)
        .env("AMUX_LOG", scratch.root.join("amux.log"))
        .env("XDG_CONFIG_HOME", scratch.root.join("config"))
        .env("XDG_DATA_HOME", scratch.root.join("data"))
        .env("XDG_STATE_HOME", scratch.root.join("state"))
        .env("TMPDIR", scratch.root.join("tmp"))
        .env_remove("AMUX_CODEX_CAPTURE_DROP_CONNECTION_ONCE")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let mut child = command.spawn().context("spawn scratch amux daemon")?;

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match amux::Server::builder()
            .config(scratch.config.clone())
            .daemon()
            .open()
            .await
        {
            Ok(client) => return Ok(ScratchDaemon { child, client }),
            Err(error) if Instant::now() < deadline => {
                if let Some(status) = child.try_wait()? {
                    bail!("scratch amux daemon exited during startup: {status} ({error})");
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("scratch amux daemon did not come up: {error}");
            }
        }
    }
}

/// The suite drives the prebuilt `target/debug/amux`, which `cargo test -p amux
/// --test codex_live` does not rebuild. A scenario run against a stale
/// binary reports on code that is not in the tree — it silently passes changes
/// it never executed, and silently "passes" reverts too. The header says to
/// build the CLI first; this makes it a control rather than an instruction.
fn target_debug_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    exe.parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("test binary is not under target/debug/deps"))
}

pub struct StructuredCapture {
    agent: Uuid,
    client: Client,
    stream: amux::SessionStream,
    rows: Vec<Row>,
    observed: File,
}

impl StructuredCapture {
    pub async fn open(harness: &Harness, agent: Uuid) -> Result<Self> {
        let stream = harness
            .client()
            .subscribe_session(SubscribeSessionRequest {
                agent: AgentIdentifier::Id(agent),
                io_protocol: CODEX_SDK_V1.into(),
                args: None,
            })
            .await
            .context("subscribe Codex structured plane")?;
        let observed = OpenOptions::new()
            .create(true)
            .append(true)
            .open(harness.scratch.out.join("observed.rows.jsonl"))?;
        Ok(Self {
            agent,
            client: harness.client().clone(),
            stream,
            rows: Vec::new(),
            observed,
        })
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub async fn wait(
        &mut self,
        from: usize,
        timeout: Duration,
        what: &str,
        matcher: Matcher,
    ) -> Result<(usize, Row)> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(index) = structure::find_match(&self.rows, from, &matcher) {
                return Ok((index + 1, self.rows[index].clone()));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("timed out after {timeout:?} waiting for {what}");
            }
            let event = tokio::time::timeout(remaining, self.stream.recv())
                .await
                .with_context(|| format!("timed out waiting for {what}"))??;
            match event {
                SubscribeSessionEvent::Output { payload } => {
                    let output = decode_codex_sdk_v1_output(&payload)?;
                    let row = Row::parse(output.seq, &output.payload)?;
                    if let Some(previous) = self.rows.last()
                        && previous.seq >= row.seq
                    {
                        bail!(
                            "Codex structured sequence did not advance: previous={} next={}",
                            previous.seq,
                            row.seq
                        );
                    }
                    writeln!(self.observed, "{}", serde_json::to_string(&row.json)?)?;
                    self.observed.flush()?;
                    self.rows.push(row);
                }
                SubscribeSessionEvent::Closed { reason } => {
                    bail!("Codex structured stream closed while waiting for {what}: {reason:?}")
                }
                _ => {}
            }
        }
    }

    pub async fn wait_ready(&mut self) -> Result<usize> {
        self.wait(
            0,
            READY_TIMEOUT,
            "amux.codex_ready",
            Matcher::Type("amux.codex_ready"),
        )
        .await
        .map(|(index, _)| index)
    }

    pub async fn send(&self, input: CodexSdkV1Input) -> Result<Vec<u8>> {
        let input_id = Uuid::new_v4().as_bytes().to_vec();
        self.client
            .send_input(SendInputRequest {
                agent: AgentIdentifier::Id(self.agent),
                input_id: input_id.clone(),
                io_protocol: CODEX_SDK_V1.into(),
                payload: Bytes::from(amux::codex_io::encode_codex_sdk_v1_input(input)),
            })
            .await?;
        Ok(input_id)
    }

    pub async fn send_prompt(&self, prompt: &str) -> Result<Vec<u8>> {
        self.send(CodexSdkV1Input::UserTurn {
            input: serde_json::to_vec(&json!([{ "type": "text", "text": prompt }]))?,
        })
        .await
    }
}

pub async fn subscribe_raw(harness: &Harness, agent: Uuid) -> Result<amux::SessionStream> {
    harness
        .client()
        .subscribe_session(SubscribeSessionRequest {
            agent: AgentIdentifier::Id(agent),
            io_protocol: TERMINAL_V1.into(),
            args: encode_terminal_v1_args(TerminalV1Args {
                terminal_size: Some(TerminalSize {
                    rows: 45,
                    cols: 140,
                }),
                replay_query: None,
            })
            .map(Bytes::from),
        })
        .await
        .context("subscribe real Codex terminal")
}

pub async fn raw_until(
    stream: &mut amux::SessionStream,
    timeout: Duration,
    needle: &[u8],
) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    loop {
        if bytes.windows(needle.len()).any(|window| window == needle) {
            return Ok(bytes);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!(
                "timed out after {timeout:?} waiting for raw bytes {:?}",
                String::from_utf8_lossy(needle)
            );
        }
        let event = match tokio::time::timeout(remaining, stream.recv()).await {
            Ok(event) => event?,
            Err(_) => {
                let tail = &bytes[bytes.len().saturating_sub(240)..];
                bail!(
                    "timed out after {timeout:?} waiting for raw bytes {:?}; collected {} bytes, tail={:?}",
                    String::from_utf8_lossy(needle),
                    bytes.len(),
                    String::from_utf8_lossy(tail)
                );
            }
        };
        match event {
            SubscribeSessionEvent::Output { payload } => bytes.extend_from_slice(&payload),
            SubscribeSessionEvent::Closed { reason } => {
                let tail = &bytes[bytes.len().saturating_sub(240)..];
                bail!(
                    "raw terminal closed while waiting for output: {reason:?}; collected {} bytes, tail={:?}",
                    bytes.len(),
                    String::from_utf8_lossy(tail)
                )
            }
            _ => {}
        }
    }
}

pub async fn drain_raw(stream: &mut amux::SessionStream) -> Result<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(bytes);
        }
        match tokio::time::timeout(Duration::from_millis(250).min(remaining), stream.recv()).await {
            Err(_) => return Ok(bytes),
            Ok(Ok(SubscribeSessionEvent::Output { payload })) => bytes.extend_from_slice(&payload),
            Ok(Ok(SubscribeSessionEvent::Closed { reason })) => {
                bail!("raw terminal closed while draining: {reason:?}")
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(error.into()),
        }
    }
}

pub fn app_server_process_group(scratch: &Scratch) -> Result<i32> {
    let socket = scratch.app_server_socket().display().to_string();
    let output = Command::new("ps")
        .args(["-axo", "pid=,pgid=,command="])
        .output()
        .context("list processes for Codex daemon recovery")?;
    if !output.status.success() {
        bail!("ps failed while locating the Codex app-server process group");
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let mut groups = Vec::new();
    for line in listing.lines().filter(|line| {
        line.contains("app-server") && line.contains(&socket) && !line.contains("ps -axo")
    }) {
        let mut fields = line.split_whitespace();
        let _pid: i32 = fields
            .next()
            .context("app-server ps row missing pid")?
            .parse()?;
        let pgid: i32 = fields
            .next()
            .context("app-server ps row missing pgid")?
            .parse()?;
        if !groups.contains(&pgid) {
            groups.push(pgid);
        }
    }
    match groups.as_slice() {
        [pgid] if *pgid > 1 => Ok(*pgid),
        [] => bail!("no app-server process group found for isolated socket {socket}"),
        _ => bail!("multiple app-server process groups found for {socket}: {groups:?}"),
    }
}

pub fn terminate_process_group(pgid: i32) -> Result<()> {
    killpg(Pid::from_raw(pgid), Signal::SIGTERM)
        .with_context(|| format!("kill real Codex app-server process group {pgid}"))
}

fn kill_capture_app_server(scratch: &Scratch) -> Result<()> {
    match app_server_process_group(scratch) {
        Ok(pgid) => terminate_process_group(pgid),
        Err(_) => Ok(()),
    }
}

pub fn codex_version() -> String {
    Command::new("codex")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .find(|part| {
                    part.matches('.').count() == 2
                        && part
                            .chars()
                            .all(|character| character.is_ascii_digit() || character == '.')
                })
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".into())
}

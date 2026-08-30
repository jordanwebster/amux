//! Maintained, opt-in real-Claude SDK backend suite.
//!
//! With no scenario argument this target prints usage and exits successfully
//! without creating a process or making a network request. Build the daemon
//! first, then run every live invocation under an outer timeout:
//!
//! ```text
//! cargo build -p amux-cli
//! AMUX_LIVE_OUT=.autopilot/evidence timeout 1500 \
//!   cargo test -p amux --test claude_sdk_live -- all
//! ```
//!
//! `AMUX_CLAUDE_LIVE_MODEL` defaults to `haiku`. The suite uses the operator's
//! existing Claude login while isolating amux state, XDG state, the project,
//! and the daemon socket under a temporary directory. Claude auto-update is
//! disabled for the live child process.

#[cfg(unix)]
mod claude_sdk_live {
    pub mod args;
}

#[cfg(not(unix))]
fn main() {
    println!("claude_sdk_live: real-Claude scenarios are only available on Unix");
}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    use std::collections::BTreeMap;
    use std::fs::{File, OpenOptions};
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use amux::claude_sdk_io::{
        CLAUDE_SDK_V1, ClaudeSdkV1Input, decode_claude_sdk_v1_output, encode_claude_sdk_v1_input,
    };
    use amux::{
        AgentIdentifier, AgentType, ClaudeDriver, Client, Config, CreateAgentRequest,
        SendInputRequest, SendMessageRequest, SubscribeSessionEvent, SubscribeSessionRequest,
    };
    use anyhow::{Context, Result, anyhow, bail};
    use bytes::Bytes;
    use claude::sdk::PermissionResult;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use uuid::Uuid;

    use claude_sdk_live::args;

    const READY_TIMEOUT: Duration = Duration::from_secs(90);
    const TURN_TIMEOUT: Duration = Duration::from_secs(240);
    const SCENARIO_TIMEOUT: Duration = Duration::from_secs(720);
    const SCENARIOS: &[&str] = &["sdk_driver"];
    const REQUIRED_VERSION: &str = "2.1.251";
    const ROWS_ARTIFACT: &str = "sdk-driver-live.rows.jsonl";
    const TRANSCRIPT_ARTIFACT: &str = "sdk-driver-live.txt";
    const UPDATE_GUARDS: &[(&str, &str)] = &[
        ("DISABLE_AUTOUPDATER", "1"),
        ("DISABLE_UPDATES", "1"),
        ("DISABLE_INSTALLATION_CHECKS", "1"),
    ];

    #[derive(Clone, Debug)]
    struct Row {
        seq: u64,
        json: Value,
    }

    impl Row {
        fn row_type(&self) -> Option<&str> {
            self.json.get("type").and_then(Value::as_str)
        }

        fn assistant_text(&self) -> Option<String> {
            (self.row_type() == Some("assistant"))
                .then(|| self.json.pointer("/message/content")?.as_array())
                .flatten()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|block| {
                            (block.get("type").and_then(Value::as_str) == Some("text"))
                                .then(|| block.get("text").and_then(Value::as_str))
                                .flatten()
                        })
                        .collect::<Vec<_>>()
                        .join("")
                })
                .filter(|text| !text.is_empty())
        }

        fn has_tool_result(&self) -> bool {
            self.row_type() == Some("user")
                && self
                    .json
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .is_some_and(|blocks| {
                        blocks.iter().any(|block| {
                            block.get("type").and_then(Value::as_str) == Some("tool_result")
                        })
                    })
        }
    }

    struct Scratch {
        _temp: TempDir,
        root: PathBuf,
        project: PathBuf,
        config: Config,
        config_path: PathBuf,
    }

    impl Scratch {
        fn create(out: PathBuf) -> Result<Self> {
            std::fs::create_dir_all(&out)?;
            let temp = tempfile::Builder::new()
                .prefix("amux-claude-sdk-live-")
                .tempdir_in("/tmp")
                .context("create Claude SDK live scratch directory")?;
            let root = temp
                .path()
                .canonicalize()
                .context("canonicalize Claude SDK live scratch directory")?;
            for dir in ["config", "data", "state", "sock", "tmp", "project"] {
                std::fs::create_dir_all(root.join(dir))?;
            }
            std::fs::set_permissions(root.join("sock"), std::fs::Permissions::from_mode(0o700))?;
            let config = Config {
                host_name: "claude-sdk-live".into(),
                socket_path: root.join("sock/amux.sock"),
                state_path: root.join("state/state.yaml"),
                enable_cloud_mode: Some(false),
                prevent_idle_sleep: Some(false),
                ..Config::default()
            };
            let config_path = root.join("config/amux.yaml");
            std::fs::write(&config_path, serde_yaml::to_string(&config)?)?;
            let project = root.join("project");
            std::fs::write(
                project.join("README.md"),
                "isolated Claude SDK live project\n",
            )?;

            Ok(Self {
                _temp: temp,
                root,
                project,
                config,
                config_path,
            })
        }
    }

    struct ScratchDaemon {
        child: Child,
        client: Client,
    }

    impl ScratchDaemon {
        async fn wait_for_exit(&mut self) -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if self.child.try_wait()?.is_some() {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    self.child.kill()?;
                    let _ = self.child.wait();
                    bail!("scratch amux daemon did not exit within 30 seconds");
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

    struct Harness {
        scratch: Scratch,
        daemon: Option<ScratchDaemon>,
    }

    impl Harness {
        async fn start(out: PathBuf) -> Result<Self> {
            let scratch = Scratch::create(out)?;
            let daemon = start_daemon(&scratch).await?;
            Ok(Self {
                scratch,
                daemon: Some(daemon),
            })
        }

        fn client(&self) -> &Client {
            &self.daemon.as_ref().expect("daemon is running").client
        }

        async fn stop_for_suspend(&mut self) -> Result<()> {
            let mut daemon = self.daemon.take().expect("daemon is running");
            daemon.wait_for_exit().await
        }

        async fn restart(&mut self) -> Result<()> {
            if self.daemon.is_some() {
                bail!("cannot restart a running scratch daemon");
            }
            self.daemon = Some(start_daemon(&self.scratch).await?);
            Ok(())
        }

        async fn shutdown(&mut self) -> Result<()> {
            let Some(mut daemon) = self.daemon.take() else {
                return Ok(());
            };
            let _ = daemon.client.shutdown().await;
            daemon.wait_for_exit().await
        }

        async fn create_sdk_agent(&self, model: &str) -> Result<Uuid> {
            let agent_id = Uuid::new_v4();
            let agent = self
                .client()
                .create_agent(CreateAgentRequest {
                    agent_id,
                    host_id: None,
                    name: Some("claude-sdk-live".to_string()),
                    agent_type: AgentType::Claude {
                        driver: ClaudeDriver::Sdk,
                    },
                    working_dir: self.scratch.project.clone(),
                    terminal_size: None,
                    args: vec![
                        "--model".to_string(),
                        model.to_string(),
                        "--setting-sources".to_string(),
                        String::new(),
                        "--strict-mcp-config".to_string(),
                    ],
                    parent: None,
                    initial_prompt: None,
                })
                .await
                .context("create real Claude SDK agent")?;
            if agent.id != agent_id {
                bail!("create returned the wrong Claude SDK agent id");
            }
            Ok(agent_id)
        }

        async fn create_sender(&self) -> Result<Uuid> {
            let agent_id = Uuid::new_v4();
            let agent = self
                .client()
                .create_agent(CreateAgentRequest {
                    agent_id,
                    host_id: None,
                    name: Some("sdk-live-sender".to_string()),
                    agent_type: AgentType::TestAgent {
                        command: "cat".to_string(),
                    },
                    working_dir: self.scratch.project.clone(),
                    terminal_size: None,
                    args: Vec::new(),
                    parent: None,
                    initial_prompt: None,
                })
                .await
                .context("create second agent for SDK delivery")?;
            if agent.id != agent_id {
                bail!("create returned the wrong sender agent id");
            }
            Ok(agent_id)
        }
    }

    struct StructuredCapture {
        agent: Uuid,
        client: Client,
        stream: amux::SessionStream,
        rows: Vec<Row>,
    }

    impl StructuredCapture {
        async fn open(harness: &Harness, agent: Uuid) -> Result<Self> {
            let stream = harness
                .client()
                .subscribe_session(SubscribeSessionRequest {
                    agent: AgentIdentifier::Id(agent),
                    io_protocol: CLAUDE_SDK_V1.to_string(),
                    args: None,
                })
                .await
                .context("subscribe Claude SDK structured plane")?;
            Ok(Self {
                agent,
                client: harness.client().clone(),
                stream,
                rows: Vec::new(),
            })
        }

        async fn wait<F>(
            &mut self,
            from: usize,
            timeout: Duration,
            what: &str,
            predicate: F,
        ) -> Result<(usize, Row)>
        where
            F: Fn(&Row) -> bool,
        {
            let deadline = Instant::now() + timeout;
            loop {
                if let Some((index, row)) = self
                    .rows
                    .iter()
                    .enumerate()
                    .skip(from)
                    .find(|(_, row)| predicate(row))
                {
                    return Ok((index + 1, row.clone()));
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
                        let output = decode_claude_sdk_v1_output(&payload)?;
                        if let Some(previous) = self.rows.last()
                            && previous.seq >= output.seq_id
                        {
                            bail!(
                                "Claude SDK sequence did not advance: previous={} next={}",
                                previous.seq,
                                output.seq_id
                            );
                        }
                        let json = serde_json::from_slice(&output.payload)
                            .context("parse Claude SDK structured row")?;
                        self.rows.push(Row {
                            seq: output.seq_id,
                            json,
                        });
                    }
                    SubscribeSessionEvent::Closed { reason } => {
                        bail!("Claude SDK stream closed while waiting for {what}: {reason:?}")
                    }
                    _ => {}
                }
            }
        }

        async fn send(&self, label: &str, input: ClaudeSdkV1Input) -> Result<Vec<u8>> {
            let input_id = label.as_bytes().to_vec();
            self.client
                .send_input(SendInputRequest {
                    agent: AgentIdentifier::Id(self.agent),
                    input_id: input_id.clone(),
                    io_protocol: CLAUDE_SDK_V1.to_string(),
                    payload: Bytes::from(encode_claude_sdk_v1_input(input)?),
                })
                .await?;
            Ok(input_id)
        }

        async fn send_prompt(&self, label: &str, text: &str) -> Result<Vec<u8>> {
            self.send(
                label,
                ClaudeSdkV1Input::Prompt {
                    text: text.to_string(),
                },
            )
            .await
        }

        async fn drain_idle(&mut self) -> Result<()> {
            loop {
                match tokio::time::timeout(Duration::from_millis(400), self.stream.recv()).await {
                    Err(_) => return Ok(()),
                    Ok(Ok(SubscribeSessionEvent::Output { payload })) => {
                        let output = decode_claude_sdk_v1_output(&payload)?;
                        let json = serde_json::from_slice(&output.payload)
                            .context("parse trailing Claude SDK row")?;
                        self.rows.push(Row {
                            seq: output.seq_id,
                            json,
                        });
                    }
                    Ok(Ok(SubscribeSessionEvent::Closed { .. })) => return Ok(()),
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => return Err(error.into()),
                }
            }
        }

        fn into_rows(self) -> Vec<Row> {
            self.rows
        }
    }

    fn input_id_value(input_id: &[u8]) -> Value {
        Value::Array(input_id.iter().copied().map(Value::from).collect())
    }

    fn input_ok(row: &Row, input_id: &[u8]) -> bool {
        row.row_type() == Some("amux.claude_sdk.input_result")
            && row.json.get("input_id") == Some(&input_id_value(input_id))
            && row.json.get("outcome").and_then(Value::as_str) == Some("ok")
    }

    fn result_contains(row: &Row, expected: &str) -> bool {
        row.row_type() == Some("result")
            && row.json.get("subtype").and_then(Value::as_str) == Some("success")
            && row
                .json
                .get("result")
                .and_then(Value::as_str)
                .is_some_and(|result| result.contains(expected))
    }

    async fn prompt_to_result(
        capture: &mut StructuredCapture,
        from: usize,
        label: &str,
        prompt: &str,
        expected: &str,
    ) -> Result<usize> {
        let input_id = capture.send_prompt(label, prompt).await?;
        let (cursor, _) = capture
            .wait(from, READY_TIMEOUT, "successful prompt input", |row| {
                input_ok(row, &input_id)
            })
            .await?;
        capture
            .wait(cursor, TURN_TIMEOUT, "successful Claude result", |row| {
                result_contains(row, expected)
            })
            .await
            .map(|(cursor, _)| cursor)
    }

    async fn run_sdk_driver(harness: &mut Harness, model: &str) -> Result<(Vec<Row>, Value)> {
        let memory_token = "SDK_RESTART_MEMORY_7F3A";
        let agent = harness.create_sdk_agent(model).await?;
        let mut capture = StructuredCapture::open(harness, agent).await?;
        let (mut cursor, ready) = capture
            .wait(0, READY_TIMEOUT, "initial SDK ready row", |row| {
                row.row_type() == Some("amux.claude_sdk.ready")
            })
            .await?;
        let session_id = ready
            .json
            .get("session_id")
            .and_then(Value::as_str)
            .context("initial ready row has no session id")?
            .to_string();
        if ready.json.get("resumed").and_then(Value::as_bool) != Some(false) {
            bail!("initial SDK ready row did not carry resumed=false");
        }

        cursor = prompt_to_result(
            &mut capture,
            cursor,
            "prompt",
            &format!(
                "Remember the token {memory_token} for this session. Reply with exactly SDK_PROMPT_OK and nothing else."
            ),
            "SDK_PROMPT_OK",
        )
        .await?;

        let tool_path = harness.scratch.project.join("sdk-tool-ran.txt");
        let permission_prompt = format!(
            "Use the Write tool to create exactly {} with content SDK_TOOL_RAN. Do not use Bash. After the tool succeeds, reply with exactly SDK_TOOL_DONE and nothing else.",
            tool_path.display()
        );
        let permission_input = capture
            .send_prompt("permission-prompt", &permission_prompt)
            .await?;
        let (permission_cursor, permission) = capture
            .wait(cursor, TURN_TIMEOUT, "Write permission request", |row| {
                row.row_type() == Some("amux.claude_sdk.permission_required")
                    && row.json.get("tool_name").and_then(Value::as_str) == Some("Write")
            })
            .await?;
        let request_id = permission
            .json
            .get("request_id")
            .and_then(Value::as_str)
            .context("permission row has no request id")?
            .to_string();
        let updated_input = permission
            .json
            .get("input")
            .cloned()
            .context("permission row has no tool input")?;
        let permission_decision = capture
            .send(
                "permission-allow",
                ClaudeSdkV1Input::PermissionDecision {
                    request_id: request_id.clone(),
                    decision: PermissionResult::Allow {
                        updated_input: Some(updated_input),
                        updated_permissions: None,
                        tool_use_id: None,
                    },
                },
            )
            .await?;
        let (cursor_after_resolution, _) = capture
            .wait(
                permission_cursor,
                READY_TIMEOUT,
                "permission resolution row",
                |row| {
                    row.row_type() == Some("amux.claude_sdk.permission_resolved")
                        && row.json.get("request_id").and_then(Value::as_str)
                            == Some(request_id.as_str())
                        && row.json.get("decision").and_then(Value::as_str) == Some("allow")
                },
            )
            .await?;
        let (cursor_after_permission_input, _) = capture
            .wait(
                permission_cursor,
                READY_TIMEOUT,
                "permission input result",
                |row| input_ok(row, &permission_decision),
            )
            .await?;
        let permission_done_from = cursor_after_resolution.max(cursor_after_permission_input);
        let (tool_result_cursor, _) = capture
            .wait(
                permission_done_from,
                TURN_TIMEOUT,
                "provider tool result after permission",
                Row::has_tool_result,
            )
            .await?;
        let (next, _) = capture
            .wait(
                tool_result_cursor,
                TURN_TIMEOUT,
                "permission scenario completion",
                |row| result_contains(row, "SDK_TOOL_DONE"),
            )
            .await?;
        cursor = next;
        let tool_contents = std::fs::read_to_string(&tool_path)
            .with_context(|| format!("read tool output at {}", tool_path.display()))?;
        if tool_contents.trim() != "SDK_TOOL_RAN" {
            bail!("Write tool produced unexpected content: {tool_contents:?}");
        }
        if !capture
            .rows
            .iter()
            .any(|row| input_ok(row, &permission_input))
        {
            bail!("permission prompt input did not receive an ok result");
        }

        let interrupt_prompt = capture
            .send_prompt(
                "interrupt-prompt",
                "Count from 1 to 5000, one number per line, with no omissions.",
            )
            .await?;
        let (interrupt_from, _) = capture
            .wait(cursor, READY_TIMEOUT, "interrupt prompt accepted", |row| {
                input_ok(row, &interrupt_prompt)
            })
            .await?;
        let (_, _) = capture
            .wait(
                interrupt_from,
                TURN_TIMEOUT,
                "assistant output before interrupt",
                |row| row.row_type() == Some("assistant"),
            )
            .await?;
        let interrupt_input = capture
            .send("interrupt", ClaudeSdkV1Input::Interrupt)
            .await?;
        capture
            .wait(
                interrupt_from,
                READY_TIMEOUT,
                "interrupt input result",
                |row| input_ok(row, &interrupt_input),
            )
            .await?;
        let (next, _) = capture
            .wait(
                interrupt_from,
                TURN_TIMEOUT,
                "provider interrupted result",
                |row| {
                    row.row_type() == Some("result")
                        && row.json.get("is_error").and_then(Value::as_bool) == Some(true)
                },
            )
            .await?;
        cursor = next;

        let sender = harness.create_sender().await?;
        harness
            .client()
            .send_message(SendMessageRequest {
                to: AgentIdentifier::Id(agent),
                text: "Reply with exactly SDK_A2A_ACTED and nothing else.".to_string(),
                context: Some(Uuid::new_v4()),
                from_agent_id: Some(sender),
            })
            .await
            .context("send message from second agent to Claude SDK agent")?;
        let (message_cursor, message_row) = capture
            .wait(
                cursor,
                READY_TIMEOUT,
                "recipient-owned SDK message row",
                |row| row.row_type() == Some("amux.claude_sdk.message"),
            )
            .await?;
        if message_row.json.get("delivery").and_then(Value::as_str) != Some("stream") {
            bail!("SDK message row did not name the stream carrier");
        }
        let (assistant_cursor, _) = capture
            .wait(
                message_cursor,
                TURN_TIMEOUT,
                "next assistant text acting on the delivered message",
                |row| {
                    row.assistant_text()
                        .is_some_and(|text| text.contains("SDK_A2A_ACTED"))
                },
            )
            .await?;
        let (next, _) = capture
            .wait(
                assistant_cursor,
                TURN_TIMEOUT,
                "A2A turn completion",
                |row| result_contains(row, "SDK_A2A_ACTED"),
            )
            .await?;
        cursor = next;
        harness.client().delete_agent(sender).await?;

        capture.drain_idle().await?;
        let mut all_rows = capture.into_rows();
        let summary = harness.client().suspend().await?;
        if summary.suspended_count != 1 {
            bail!(
                "expected exactly the Claude SDK agent to suspend, got {}",
                summary.suspended_count
            );
        }
        harness.stop_for_suspend().await?;
        harness.restart().await?;
        let resume = harness.client().resume().await?;
        if resume.resumed_count != 1 || resume.failed_count != 0 {
            bail!("resume summary was not 1/0: {resume:?}");
        }

        let mut resumed = StructuredCapture::open(harness, agent).await?;
        let (gap_cursor, gap) = resumed
            .wait(0, READY_TIMEOUT, "SDK resume gap row", |row| {
                row.row_type() == Some("amux.claude_sdk.gap")
            })
            .await?;
        if gap.json.get("resumed_session_id").and_then(Value::as_str) != Some(session_id.as_str()) {
            bail!("resume gap did not name the original session id");
        }
        let (resumed_cursor, ready) = resumed
            .wait(gap_cursor, READY_TIMEOUT, "resumed SDK ready row", |row| {
                row.row_type() == Some("amux.claude_sdk.ready")
            })
            .await?;
        if ready.json.get("session_id").and_then(Value::as_str) != Some(session_id.as_str())
            || ready.json.get("resumed").and_then(Value::as_bool) != Some(true)
        {
            bail!("resumed ready row did not preserve session id with resumed=true");
        }
        let _ = prompt_to_result(
            &mut resumed,
            resumed_cursor,
            "resume-memory",
            "What exact SDK_RESTART_MEMORY token did I ask you to remember? Reply with just that token.",
            memory_token,
        )
        .await?;
        resumed.drain_idle().await?;
        all_rows.extend(resumed.into_rows());

        let listed = harness.client().list_agents().await?;
        if !listed.iter().any(|entry| entry.id == agent) {
            bail!("resumed inventory did not retain Claude SDK agent {agent}");
        }
        harness.client().delete_agent(agent).await?;
        let row_count = all_rows.len();

        Ok((
            all_rows,
            json!({
                "agent_id": agent,
                "session_id": session_id,
                "assertions": {
                    "prompt_answered": true,
                    "permission_answered_through_input": true,
                    "tool_ran": tool_contents.trim(),
                    "interrupt_observed": true,
                    "second_agent_message_recorded": true,
                    "next_assistant_text": "SDK_A2A_ACTED",
                    "daemon_restarted": true,
                    "resume_preserved_session_id": true,
                    "resume_memory": memory_token,
                    "row_count": row_count,
                    "final_cursor_before_restart": cursor,
                }
            }),
        ))
    }

    async fn start_daemon(scratch: &Scratch) -> Result<ScratchDaemon> {
        let target_debug = target_debug_dir()?;
        let amux = target_debug.join("amux");
        if !amux.exists() {
            bail!(
                "amux binary missing at {}; run `cargo build -p amux-cli` first",
                amux.display()
            );
        }
        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(scratch.root.join("daemon.log"))?;
        let stderr = OpenOptions::new()
            .create(true)
            .append(true)
            .open(scratch.root.join("daemon.err"))?;
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
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        apply_daemon_environment(&mut command, scratch, &target_debug);
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

    fn apply_daemon_environment(command: &mut Command, scratch: &Scratch, target_debug: &Path) {
        let mut environment = BTreeMap::new();
        for key in ["HOME", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL"] {
            if let Ok(value) = std::env::var(key) {
                environment.insert(key.to_string(), value);
            }
        }
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
        environment.insert(
            "PATH".to_string(),
            format!("{}:{path}", target_debug.display()),
        );
        environment.insert("TERM".to_string(), "xterm-256color".to_string());
        environment.insert(
            "AMUX_CONFIG".to_string(),
            scratch.config_path.display().to_string(),
        );
        environment.insert(
            "XDG_CONFIG_HOME".to_string(),
            scratch.root.join("config").display().to_string(),
        );
        environment.insert(
            "XDG_DATA_HOME".to_string(),
            scratch.root.join("data").display().to_string(),
        );
        environment.insert(
            "XDG_STATE_HOME".to_string(),
            scratch.root.join("state").display().to_string(),
        );
        environment.insert(
            "TMPDIR".to_string(),
            scratch.root.join("tmp").display().to_string(),
        );
        for (name, value) in UPDATE_GUARDS {
            environment.insert((*name).to_string(), (*value).to_string());
        }
        command.env_clear().envs(environment);
    }

    fn target_debug_dir() -> Result<PathBuf> {
        let executable = std::env::current_exe().context("current_exe")?;
        executable
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| anyhow!("test binary is not under target/debug/deps"))
    }

    fn workspace_path(path: impl AsRef<Path>) -> PathBuf {
        let path = path.as_ref();
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .expect("amux package lives below the workspace root")
                .join(path)
        }
    }

    fn report(transcript: &mut File, line: impl AsRef<str>) -> Result<()> {
        println!("{}", line.as_ref());
        writeln!(transcript, "{}", line.as_ref())?;
        transcript.flush()?;
        Ok(())
    }

    fn redact_text(input: &str, scratch: &Path) -> String {
        let mut output = input.replace(&scratch.display().to_string(), "[SCRATCH]");
        if let Ok(home) = std::env::var("HOME") {
            output = output.replace(&home, "[HOME]");
        }
        output
    }

    fn redact_value(value: &mut Value, scratch: &Path) {
        match value {
            Value::String(text) => *text = redact_text(text, scratch),
            Value::Array(values) => {
                for value in values {
                    redact_value(value, scratch);
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    if key == "messaging_socket_path" && value.is_string() {
                        *value = Value::String("[CLAUDE_SOCKET]".to_string());
                    } else {
                        redact_value(value, scratch);
                    }
                }
            }
            _ => {}
        }
    }

    fn write_rows(path: &Path, rows: &[Row], scratch: &Path) -> Result<()> {
        let mut file = File::create(path)?;
        for row in rows {
            let mut value = row.json.clone();
            redact_value(&mut value, scratch);
            writeln!(file, "{}", serde_json::to_string(&value)?)?;
        }
        file.flush()?;
        let contents = std::fs::read_to_string(path)?;
        let scratch_text = scratch.display().to_string();
        let home = std::env::var("HOME").unwrap_or_default();
        let mut violations = Vec::new();
        if contents.contains(&scratch_text) {
            violations.push("scratch path");
        }
        if !home.is_empty() && contents.contains(&home) {
            violations.push("home path");
        }
        for marker in ["sk-ant-", "oauth_token", "OAUTH_TOKEN", "Bearer "] {
            if contents.contains(marker) {
                violations.push(marker);
            }
        }
        if !violations.is_empty() {
            bail!("Claude SDK live redaction failed: {violations:?}");
        }
        Ok(())
    }

    async fn run(selected: Vec<&str>) -> Result<()> {
        let out = std::env::var_os("AMUX_LIVE_OUT")
            .map(workspace_path)
            .unwrap_or_else(|| workspace_path("target/claude-sdk-live"));
        std::fs::create_dir_all(&out)?;
        let rows_path = out.join(ROWS_ARTIFACT);
        let transcript_path = out.join(TRANSCRIPT_ARTIFACT);
        if rows_path.exists() {
            std::fs::remove_file(&rows_path)?;
        }
        let model = std::env::var("AMUX_CLAUDE_LIVE_MODEL").unwrap_or_else(|_| "haiku".into());
        if !matches!(model.as_str(), "haiku" | "sonnet") {
            bail!("AMUX_CLAUDE_LIVE_MODEL must be `haiku` or `sonnet`, got `{model}`");
        }
        let version = claude::version::probe_version(Path::new("claude"))
            .await?
            .to_string();
        let header = format!("provider=claude version={version} model={model}");
        println!("{header}");
        let mut transcript = File::create(&transcript_path)?;
        writeln!(transcript, "{header}")?;
        transcript.flush()?;
        if version != REQUIRED_VERSION {
            bail!("Claude SDK live suite requires Claude Code {REQUIRED_VERSION}, found {version}");
        }

        for scenario in selected {
            report(
                &mut transcript,
                format!("=== {scenario}: daemon SDK boundary ==="),
            )?;
            let started = Instant::now();
            let mut harness = Harness::start(out.clone()).await?;
            let result =
                tokio::time::timeout(SCENARIO_TIMEOUT, run_sdk_driver(&mut harness, &model))
                    .await
                    .map_err(|_| anyhow!("scenario timeout after {SCENARIO_TIMEOUT:?}"))
                    .and_then(|result| result);
            let cleanup = harness.shutdown().await;
            let result = match (result, cleanup) {
                (Ok(capture), Ok(())) => Ok(capture),
                (Err(error), Ok(())) => Err(error),
                (Ok(_), Err(cleanup_error)) => Err(anyhow!(
                    "scenario passed but cleanup failed: {cleanup_error:#}"
                )),
                (Err(error), Err(cleanup_error)) => {
                    Err(anyhow!("{error:#}; cleanup also failed: {cleanup_error:#}"))
                }
            };
            match result {
                Ok((rows, notes)) => {
                    write_rows(&rows_path, &rows, &harness.scratch.root)?;
                    report(&mut transcript, format!("assertions={notes}"))?;
                    report(
                        &mut transcript,
                        format!(
                            "=== {scenario} PASS ({:.1}s); rows={} ===",
                            started.elapsed().as_secs_f64(),
                            rows_path
                                .strip_prefix(workspace_path(""))
                                .unwrap_or(&rows_path)
                                .display()
                        ),
                    )?;
                }
                Err(error) => {
                    report(
                        &mut transcript,
                        format!("=== {scenario} FAIL: {error:#} ==="),
                    )?;
                    return Err(error);
                }
            }
        }
        report(
            &mut transcript,
            "claude_sdk_live: all selected backend scenarios passed",
        )?;
        Ok(())
    }

    let names: Vec<String> = std::env::args().skip(1).collect();
    let selected = args::select(&names, SCENARIOS).map_err(anyhow::Error::msg)?;
    if selected.is_empty() {
        println!("{}", args::USAGE);
        return Ok(());
    }
    let selected = selected.into_iter().map(|index| SCENARIOS[index]).collect();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(selected))
}

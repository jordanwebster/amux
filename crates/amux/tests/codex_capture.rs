//! Maintained, opt-in real-Codex end-to-end suite (the C suite).
//!
//! With no scenario argument this target exits successfully without creating a
//! process or making a network request. Build the CLI first, then run every
//! invocation under an outer timeout:
//!
//! ```text
//! cargo build -p amux-cli
//! AMUX_CODEX_CAPTURE_DIR=target/codex-capture timeout 600 \
//!   cargo test -p amux --test codex_capture -- c1_pong
//! ```
//!
//! The suite drives the prebuilt `target/debug/amux`, which this target does
//! not rebuild; the harness refuses to start against a binary older than the
//! prerequisites in Cargo's depfile rather than reporting on code it never ran.
//!
//! `c-all` selects C.1-C.14. Each scenario has one row in [`SCENARIOS`], so
//! its id, requirement, timeout, and runner stay together. Captures land in a
//! scenario-named child of `AMUX_CODEX_CAPTURE_DIR` (or a timestamped default)
//! and include backend rows, SDK IO, observed subscription rows, raw bytes
//! where applicable, redacted copies, and version-stamped metadata.

#[cfg(unix)]
mod codex_capture {
    pub mod depfile;
    pub mod harness;
    pub mod redact;
    pub mod structure;
}

#[cfg(not(unix))]
fn main() {
    println!("codex_capture: real-Codex scenarios are only available on Unix");
}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    use std::collections::HashMap;
    use std::fs::{File, OpenOptions};
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    use amux::codex_io::CodexSdkV1Input;
    use anyhow::{Context, Result, anyhow, bail};
    use codex_capture::{harness, redact, structure};
    use codex_sdk::notification::TurnEvent;
    use codex_sdk::{CodexConfig, DynamicToolCallResponse, ThreadConfig, connect};
    use harness::{
        Harness, RAW_TIMEOUT, READY_TIMEOUT, StructuredCapture, TURN_TIMEOUT,
        app_server_process_group, drain_raw, raw_until, subscribe_raw, terminate_process_group,
    };
    use serde_json::{Value, json};
    use structure::Matcher;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Scenario {
        Pong,
        ApprovalAllow,
        ApprovalDeny,
        Interrupt,
        SuspendResume,
        DaemonRecovery,
        RawCoexistence,
        RawFanout,
        RawUnnamed,
        UnnamedReconnect,
        DynamicTools,
        InjectIdle,
        InjectBusy,
        LastMessage,
    }

    #[derive(Clone, Copy)]
    struct ScenarioSpec {
        id: &'static str,
        requirement: &'static str,
        timeout: Duration,
        runner: Scenario,
    }

    const fn scenario(
        id: &'static str,
        requirement: &'static str,
        seconds: u64,
        runner: Scenario,
    ) -> ScenarioSpec {
        ScenarioSpec {
            id,
            requirement,
            timeout: Duration::from_secs(seconds),
            runner,
        }
    }

    const SCENARIOS: &[ScenarioSpec] = &[
        scenario("c1_pong", "C.1 create + pong", 300, Scenario::Pong),
        scenario(
            "c2_approval_allow",
            "C.2 approval allow + world assertion",
            360,
            Scenario::ApprovalAllow,
        ),
        scenario(
            "c3_approval_deny",
            "C.3 approval deny + world assertion",
            360,
            Scenario::ApprovalDeny,
        ),
        scenario(
            "c4_interrupt",
            "C.4 interrupt + reuse",
            360,
            Scenario::Interrupt,
        ),
        scenario(
            "c5_suspend_resume",
            "C.5 suspend/resume across server restart",
            420,
            Scenario::SuspendResume,
        ),
        scenario(
            "c6_daemon_recovery",
            "C.6 real app-server process-group recovery",
            420,
            Scenario::DaemonRecovery,
        ),
        scenario(
            "c7_raw_coexistence",
            "C.7 raw + structured coexistence",
            420,
            Scenario::RawCoexistence,
        ),
        scenario(
            "c8_raw_fanout",
            "C.8 two-terminal fanout",
            420,
            Scenario::RawFanout,
        ),
        scenario(
            "c9_raw_unnamed",
            "C.9 raw attach on an unnamed agent",
            300,
            Scenario::RawUnnamed,
        ),
        scenario(
            "c10_unnamed_reconnect",
            "C.10 zero-turn unnamed suspend/resume",
            300,
            Scenario::UnnamedReconnect,
        ),
        scenario(
            "c11_dynamic_tools",
            "C.11 dynamic tools under experimental API",
            360,
            Scenario::DynamicTools,
        ),
        scenario(
            "c12_inject_idle",
            "C.12 inject items on an idle thread then start an empty turn",
            360,
            Scenario::InjectIdle,
        ),
        scenario(
            "c13_inject_busy",
            "C.13 inject items during a running turn",
            360,
            Scenario::InjectBusy,
        ),
        scenario(
            "c14_last_message",
            "C.14 two assistant messages before turn completion",
            360,
            Scenario::LastMessage,
        ),
    ];

    fn validate_scenarios() -> Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        for (index, spec) in SCENARIOS.iter().enumerate() {
            if !ids.insert(spec.id) {
                bail!("duplicate C-suite scenario id: {}", spec.id);
            }
            let expected = format!("C.{}", index + 1);
            if !spec.requirement.starts_with(&expected) || spec.timeout.is_zero() {
                bail!("incomplete scenario grammar row for {}", spec.id);
            }
        }
        Ok(())
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

    async fn prompt_to_completion(
        capture: &mut StructuredCapture,
        from: usize,
        prompt: &str,
        expected_text: &str,
    ) -> Result<(usize, structure::Row)> {
        let input_id = capture.send_prompt(prompt).await?;
        let (cursor, _) = capture
            .wait(
                from,
                READY_TIMEOUT,
                "successful input result",
                Matcher::InputOk(input_id),
            )
            .await?;
        let (cursor, _) = capture
            .wait(
                cursor,
                TURN_TIMEOUT,
                "completed agent text",
                Matcher::AgentTextContains(expected_text.into()),
            )
            .await?;
        capture
            .wait(
                cursor,
                TURN_TIMEOUT,
                "completed turn",
                Matcher::TurnCompleted("completed"),
            )
            .await
    }

    async fn open(
        harness: &Harness,
        scenario: &str,
        model: &str,
    ) -> Result<(uuid::Uuid, StructuredCapture, usize)> {
        open_named(harness, Some(scenario), model).await
    }

    async fn open_named(
        harness: &Harness,
        scenario: Option<&str>,
        model: &str,
    ) -> Result<(uuid::Uuid, StructuredCapture, usize)> {
        let agent = harness.create_agent(scenario, model).await?;
        let mut capture = StructuredCapture::open(harness, agent).await?;
        let cursor = capture.wait_ready().await?;
        Ok((agent, capture, cursor))
    }

    /// Minimal direct app-server client used only by substrate probes for
    /// experimental RPCs that have not earned a production SDK surface yet.
    struct DirectAppServer {
        child: tokio::process::Child,
        stdin: tokio::process::ChildStdin,
        stdout: BufReader<tokio::process::ChildStdout>,
        io: File,
        started: Instant,
        next_id: u64,
        seen: Vec<Value>,
    }

    impl DirectAppServer {
        async fn open(harness: &Harness) -> Result<Self> {
            let mut command = tokio::process::Command::new("codex");
            command
                .args(["app-server", "--listen", "stdio://"])
                .current_dir(&harness.scratch.project)
                .env("CODEX_HOME", &harness.scratch.codex_home)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null());
            let mut child = command
                .spawn()
                .context("spawn direct Codex app-server for inject-items capture")?;
            let stdin = child.stdin.take().context("take direct app-server stdin")?;
            let stdout = child
                .stdout
                .take()
                .context("take direct app-server stdout")?;
            let io = OpenOptions::new()
                .create(true)
                .append(true)
                .open(harness.scratch.out.join("io.jsonl"))?;
            let mut server = Self {
                child,
                stdin,
                stdout: BufReader::new(stdout),
                io,
                started: Instant::now(),
                next_id: 1,
                seen: Vec::new(),
            };
            server
                .request(
                    "initialize",
                    json!({
                        "clientInfo": {"name": "amux-a2a-capture", "version": "0.1.0"},
                        "capabilities": {"experimentalApi": true}
                    }),
                )
                .await
                .context("initialize direct app-server capture")?;
            server.notify("initialized", None).await?;
            Ok(server)
        }

        fn record(&mut self, direction: &str, line: &str) -> Result<()> {
            writeln!(
                self.io,
                "{}",
                json!({
                    "us": self.started.elapsed().as_micros() as u64,
                    "dir": direction,
                    "line": line,
                })
            )?;
            self.io.flush()?;
            Ok(())
        }

        async fn send(&mut self, value: Value) -> Result<()> {
            let line = serde_json::to_string(&value)?;
            self.record("stdin", &line)?;
            self.stdin.write_all(line.as_bytes()).await?;
            self.stdin.write_all(b"\n").await?;
            self.stdin.flush().await?;
            Ok(())
        }

        async fn next(&mut self) -> Result<Value> {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).await?;
            if read == 0 {
                bail!("direct Codex app-server closed stdout")
            }
            let line = line.trim();
            self.record("stdout", line)?;
            let value: Value =
                serde_json::from_str(line).context("parse direct app-server JSON-RPC line")?;
            self.seen.push(value.clone());
            Ok(value)
        }

        async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
            let id = self.next_id;
            self.next_id += 1;
            self.send(json!({"id": id, "method": method, "params": params}))
                .await?;
            loop {
                let message = self.next().await?;
                if message.get("id") == Some(&Value::from(id)) {
                    if let Some(error) = message.get("error") {
                        bail!("{method} failed: {error}");
                    }
                    return message
                        .get("result")
                        .cloned()
                        .context("direct app-server response omitted result");
                }
            }
        }

        async fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
            let mut value = json!({"method": method});
            if let Some(params) = params {
                value["params"] = params;
            }
            self.send(value).await
        }

        async fn wait_for(
            &mut self,
            timeout: Duration,
            what: &str,
            predicate: impl Fn(&Value) -> bool,
        ) -> Result<Value> {
            let deadline = Instant::now() + timeout;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    bail!("timed out waiting for {what}");
                }
                let message = tokio::time::timeout(remaining, self.next())
                    .await
                    .with_context(|| format!("timed out waiting for {what}"))??;
                if predicate(&message) {
                    return Ok(message);
                }
            }
        }

        async fn close(&mut self) {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
    }

    async fn direct_thread(
        server: &mut DirectAppServer,
        harness: &Harness,
        model: &str,
    ) -> Result<String> {
        let started = server
            .request(
                "thread/start",
                json!({"cwd": harness.scratch.project, "model": model}),
            )
            .await?;
        started
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("thread/start response omitted thread id")
    }

    fn injected_item(text: &str) -> Value {
        json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": text}]
        })
    }

    fn is_turn_started(message: &Value, thread_id: &str) -> bool {
        message.get("method").and_then(Value::as_str) == Some("turn/started")
            && message.pointer("/params/threadId").and_then(Value::as_str) == Some(thread_id)
    }

    fn is_completed_turn(message: &Value, thread_id: &str, turn_id: &str) -> bool {
        message.get("method").and_then(Value::as_str) == Some("turn/completed")
            && message.pointer("/params/threadId").and_then(Value::as_str) == Some(thread_id)
            && message.pointer("/params/turn/id").and_then(Value::as_str) == Some(turn_id)
            && message
                .pointer("/params/turn/status")
                .and_then(Value::as_str)
                == Some("completed")
    }

    fn completed_agent_messages<'a>(server: &'a DirectAppServer, turn_id: &str) -> Vec<&'a Value> {
        server
            .seen
            .iter()
            .filter(|message| {
                message.get("method").and_then(Value::as_str) == Some("item/completed")
                    && message.pointer("/params/turnId").and_then(Value::as_str) == Some(turn_id)
                    && message.pointer("/params/item/type").and_then(Value::as_str)
                        == Some("agentMessage")
            })
            .collect()
    }

    async fn inject_idle(harness: &mut Harness, model: &str) -> Result<Value> {
        let mut server = DirectAppServer::open(harness).await?;
        let result = async {
            let thread_id = direct_thread(&mut server, harness, model).await?;
            server
                .request(
                    "thread/inject_items",
                    json!({
                        "threadId": thread_id,
                        "items": [injected_item("C12_INJECT_IDLE")]
                    }),
                )
                .await
                .context("inject an item into an idle thread")?;
            let started = server
                .request("turn/start", json!({"threadId": thread_id, "input": []}))
                .await
                .context("start an empty-input turn after idle injection")?;
            let turn_id = started
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .context("empty-input turn/start omitted turn id")?
                .to_owned();
            server
                .wait_for(
                    TURN_TIMEOUT,
                    "empty-input injected turn completion",
                    |message| is_completed_turn(message, &thread_id, &turn_id),
                )
                .await?;
            let messages = completed_agent_messages(&server, &turn_id);
            if !messages.iter().any(|message| {
                message.pointer("/params/item/text").and_then(Value::as_str)
                    .is_some_and(|text| text.contains("C12_INJECT_IDLE"))
            }) {
                bail!("empty-input turn did not expose the injected item to the model");
            }
            Ok(json!({
                "assertions": {"inject_accepted": true, "empty_turn_completed": true, "injected_item_seen": true},
                "thread_id": thread_id,
                "turn_id": turn_id,
            }))
        }
        .await;
        server.close().await;
        result
    }

    async fn inject_busy(harness: &mut Harness, model: &str) -> Result<Value> {
        let mut server = DirectAppServer::open(harness).await?;
        let result = async {
            let thread_id = direct_thread(&mut server, harness, model).await?;
            let started = server
                .request(
                    "turn/start",
                    json!({
                        "threadId": thread_id,
                        "input": [{"type": "text", "text": "Think carefully for a moment, then reply exactly C13_INITIAL."}]
                    }),
                )
                .await?;
            let turn_id = started
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .context("busy turn/start omitted turn id")?
                .to_owned();
            server
                .wait_for(TURN_TIMEOUT, "busy turn start", |message| {
                    is_turn_started(message, &thread_id)
                })
                .await?;
            server
                .request(
                    "thread/inject_items",
                    json!({
                        "threadId": thread_id,
                        "items": [injected_item("C13_INJECT_BUSY")]
                    }),
                )
                .await
                .context("inject an item into a running turn")?;
            server
                .wait_for(TURN_TIMEOUT, "busy injected turn completion", |message| {
                    is_completed_turn(message, &thread_id, &turn_id)
                })
                .await?;
            let messages = completed_agent_messages(&server, &turn_id);
            let texts: Vec<_> = messages
                .iter()
                .filter_map(|message| message.pointer("/params/item/text").and_then(Value::as_str))
                .collect();
            if texts != ["C13_INITIAL", "C13_INJECT_BUSY"] {
                bail!("busy injected turn did not queue the injected message: {texts:?}");
            }
            Ok(json!({
                "assertions": {"inject_accepted": true, "busy_turn_completed": true, "injected_item_queued": true},
                "thread_id": thread_id,
                "turn_id": turn_id,
            }))
        }
        .await;
        server.close().await;
        result
    }

    async fn last_message(harness: &mut Harness, model: &str) -> Result<Value> {
        let mut server = DirectAppServer::open(harness).await?;
        let result = async {
            let thread_id = direct_thread(&mut server, harness, model).await?;
            let started = server
                .request(
                    "turn/start",
                    json!({
                        "threadId": thread_id,
                        "input": [{"type": "text", "text": "Send two separate assistant messages in this turn: first exactly C14_FIRST in commentary, then exactly C14_SECOND as the final answer."}]
                    }),
                )
                .await?;
            let turn_id = started
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .context("two-message turn/start omitted turn id")?
                .to_owned();
            let completed = server
                .wait_for(TURN_TIMEOUT, "two-message turn completion", |message| {
                    is_completed_turn(message, &thread_id, &turn_id)
                })
                .await?;
            let messages = completed_agent_messages(&server, &turn_id);
            let texts: Vec<_> = messages
                .iter()
                .filter_map(|message| message.pointer("/params/item/text").and_then(Value::as_str))
                .collect();
            if texts != ["C14_FIRST", "C14_SECOND"] {
                bail!("two-message turn did not preserve assistant-message order: {texts:?}");
            }
            if messages[0]
                .pointer("/params/item/phase")
                .and_then(Value::as_str)
                != Some("commentary")
                || messages[1]
                    .pointer("/params/item/phase")
                    .and_then(Value::as_str)
                    != Some("final_answer")
            {
                bail!("two-message turn did not preserve commentary then final phases");
            }
            Ok(json!({
                "assertions": {"turn_completed": true, "two_messages_before_completion": true, "last_message_is_final": true},
                "thread_id": thread_id,
                "turn_id": turn_id,
                "completion": completed,
            }))
        }
        .await;
        server.close().await;
        result
    }

    async fn pong(harness: &mut Harness, model: &str) -> Result<Value> {
        let (agent, mut capture, cursor) = open(harness, "c1-pong", model).await?;
        let (_, completed) = prompt_to_completion(
            &mut capture,
            cursor,
            "Reply with exactly C1_PONG and nothing else.",
            "C1_PONG",
        )
        .await?;
        let thread = completed
            .thread_id()
            .context("turn/completed missing threadId")?;
        harness.client().delete_agent(agent).await?;
        Ok(json!({
            "assertions": {"ready": true, "agent_text": "C1_PONG", "turn_status": "completed"},
            "thread_id": thread,
        }))
    }

    async fn dynamic_tools(harness: &mut Harness, model: &str) -> Result<Value> {
        let mut env = HashMap::new();
        env.insert(
            "CODEX_HOME".to_string(),
            harness.scratch.codex_home.display().to_string(),
        );
        let mut extra = HashMap::new();
        extra.insert(
            "dynamicTools".to_string(),
            json!([{
                "name": "send",
                "description": "Send a short message to another agent.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "to": {"type": "string"},
                        "text": {"type": "string"}
                    },
                    "required": ["to", "text"]
                }
            }]),
        );
        let codex = connect(CodexConfig {
            cwd: Some(harness.scratch.project.clone()),
            env: Some(env),
            record_io: Some(harness.scratch.out.join("io.jsonl")),
            ..CodexConfig::default()
        })
        .await
        .context("connect direct Codex app-server for dynamic-tools capture")?;
        let thread = codex
            .start_thread(ThreadConfig {
                model: Some(model.to_string()),
                cwd: Some(harness.scratch.project.display().to_string()),
                extra,
                ..ThreadConfig::default()
            })
            .await
            .context("thread/start with dynamicTools")?;
        let mut events = thread
            .events()
            .await
            .context("take dynamic-tools event stream")?;
        let turn_id = thread
            .start_turn("Call the send tool exactly once with to=probe and text=C11_SENT. Do not use any other tool.")
            .await
            .context("start dynamic-tools turn")?;
        let deadline = Instant::now() + TURN_TIMEOUT;
        let mut call = None;
        let mut completed = false;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = tokio::time::timeout(remaining, events.next())
                .await
                .context("wait for dynamic-tools event")??
                .context("dynamic-tools event stream closed")?;
            if let TurnEvent::ToolCallRequired(request) = event.event {
                if request.tool == "send" {
                    call = Some(request.arguments.clone());
                    thread
                        .respond_tool_call(
                            request.request_id,
                            DynamicToolCallResponse {
                                content_items: vec![json!({"type": "inputText", "text": "sent"})],
                                success: true,
                            },
                        )
                        .await
                        .context("respond to dynamic send tool call")?;
                }
            }
            if event.method == "turn/completed" && event.turn_id.as_deref() == Some(&turn_id) {
                completed = true;
                break;
            }
        }
        codex.close().await;
        let call = call.context("model did not request the dynamic send tool")?;
        if !completed {
            bail!("dynamic-tools turn did not complete after the tool response");
        }
        Ok(json!({
            "assertions": {"dynamic_send_called": true, "turn_completed": true},
            "arguments": call,
        }))
    }

    async fn approval(harness: &mut Harness, model: &str, allow: bool) -> Result<Value> {
        let scenario = if allow { "c2-allow" } else { "c3-deny" };
        let proof = harness.scratch.project.join(if allow {
            "c2-allowed.txt"
        } else {
            "c3-denied.txt"
        });
        let (agent, mut capture, cursor) = open(harness, scenario, model).await?;
        let prompt = format!(
            "Run this exact shell command and no substitute: /usr/bin/touch {}. Then say DONE.",
            proof.display()
        );
        let input_id = capture.send_prompt(&prompt).await?;
        let (cursor, _) = capture
            .wait(
                cursor,
                READY_TIMEOUT,
                "approval-turn input result",
                Matcher::InputOk(input_id),
            )
            .await?;
        let (cursor, ask) = capture
            .wait(
                cursor,
                TURN_TIMEOUT,
                "Codex approval request",
                Matcher::ApprovalRequired,
            )
            .await?;
        let request_id = ask
            .json
            .get("request_id")
            .cloned()
            .context("approval row missing request_id")?;
        let decision = if allow { "accept" } else { "decline" };
        let answer_id = capture.answer(&request_id, decision).await?;
        let (cursor, _) = capture
            .wait(
                cursor,
                READY_TIMEOUT,
                "approval resolution",
                Matcher::ApprovalResolved(request_id.clone()),
            )
            .await?;
        let (cursor, _) = capture
            .wait(
                cursor,
                READY_TIMEOUT,
                "approval answer input result",
                Matcher::InputOk(answer_id),
            )
            .await?;
        let command_status = if allow { "completed" } else { "declined" };
        let (cursor, _) = capture
            .wait(
                cursor,
                TURN_TIMEOUT,
                "resolved command execution",
                Matcher::CommandCompleted(command_status),
            )
            .await?;
        capture
            .wait(
                cursor,
                TURN_TIMEOUT,
                "turn completion after approval",
                Matcher::TurnCompleted("completed"),
            )
            .await?;

        let exists = proof.exists();
        if exists != allow {
            bail!(
                "world assertion failed after {decision}: {} exists={exists}, expected {allow}",
                proof.display()
            );
        }
        harness.client().delete_agent(agent).await?;
        Ok(json!({
            "decision": decision,
            "request_id": request_id,
            "assertions": {
                "approval_resolved": "answered",
                "command_status": command_status,
                "world_file_exists": exists,
            }
        }))
    }

    async fn interrupt(harness: &mut Harness, model: &str) -> Result<Value> {
        let (agent, mut capture, cursor) = open(harness, "c4-interrupt", model).await?;
        let input_id = capture
            .send_prompt("Count slowly from 1 to 10000, one number per line, without using tools.")
            .await?;
        let (cursor, _) = capture
            .wait(
                cursor,
                READY_TIMEOUT,
                "long-turn input result",
                Matcher::InputOk(input_id),
            )
            .await?;
        let (cursor, started) = capture
            .wait(cursor, READY_TIMEOUT, "active turn", Matcher::TurnStarted)
            .await?;
        let turn_id = started
            .turn_id()
            .context("turn/started missing turn id")?
            .to_string();
        tokio::time::sleep(Duration::from_millis(250)).await;
        let interrupt_id = capture
            .send(CodexSdkV1Input::Interrupt {
                turn_id: turn_id.clone(),
            })
            .await?;
        let (cursor, _) = capture
            .wait(
                cursor,
                READY_TIMEOUT,
                "interrupt input result",
                Matcher::InputOk(interrupt_id),
            )
            .await?;
        let (cursor, _) = capture
            .wait(
                cursor,
                TURN_TIMEOUT,
                "interrupted turn",
                Matcher::TurnCompleted("interrupted"),
            )
            .await?;
        prompt_to_completion(
            &mut capture,
            cursor,
            "Reply with exactly C4_AFTER_INTERRUPT and nothing else.",
            "C4_AFTER_INTERRUPT",
        )
        .await?;
        harness.client().delete_agent(agent).await?;
        Ok(json!({
            "turn_id": turn_id,
            "assertions": {"first_status": "interrupted", "second_status": "completed", "session_reused": true}
        }))
    }

    async fn suspend_resume(harness: &mut Harness, model: &str) -> Result<Value> {
        let token = "C5_CONTEXT_7F3A";
        let (agent, mut capture, cursor) = open(harness, "c5-suspend-resume", model).await?;
        let (_, first_completed) = prompt_to_completion(
            &mut capture,
            cursor,
            &format!("Remember the token {token}. Reply with exactly C5_STORED."),
            "C5_STORED",
        )
        .await?;
        let thread_before = first_completed
            .thread_id()
            .context("first completion missing threadId")?
            .to_string();
        drop(capture);

        let summary = harness.client().suspend().await?;
        if summary.suspended_count != 1 {
            bail!(
                "expected one suspended agent, got {}",
                summary.suspended_count
            );
        }
        harness.stop_for_suspend().await?;
        harness.restart().await?;
        let resume = harness.client().resume().await?;
        if resume.resumed_count != 1 || resume.failed_count != 0 {
            bail!("resume summary was not 1/0: {resume:?}");
        }
        let listed = harness.client().list_agents().await?;
        if !listed.iter().any(|entry| entry.id == agent) {
            bail!("resumed inventory did not retain agent {agent}");
        }

        let mut resumed = StructuredCapture::open(harness, agent).await?;
        let (cursor, ready) = resumed
            .wait(
                0,
                READY_TIMEOUT,
                "resumed amux.codex_ready",
                Matcher::Type("amux.codex_ready"),
            )
            .await?;
        if ready.json.get("resumed").and_then(Value::as_bool) != Some(true) {
            bail!("post-restart ready row did not carry resumed=true");
        }
        let rows_before_history_prompt = resumed.rows().len();
        let (_, second_completed) = prompt_to_completion(
            &mut resumed,
            cursor,
            "What exact token did I ask you to remember before the server restart? Reply with just that token.",
            token,
        )
        .await?;
        let thread_after = second_completed
            .thread_id()
            .context("resumed completion missing threadId")?;
        if thread_after != thread_before {
            bail!("thread identity changed across restart: {thread_before} -> {thread_after}");
        }
        harness.client().delete_agent(agent).await?;
        Ok(json!({
            "thread_id": thread_before,
            "observed": {"structured_rows_before_history_prompt": rows_before_history_prompt, "prior_feed_replayed": false},
            "assertions": {"resumed_ready": true, "history_token": token, "same_agent": true, "same_thread": true}
        }))
    }

    async fn daemon_recovery(harness: &mut Harness, model: &str) -> Result<Value> {
        let (agent, mut capture, cursor) = open(harness, "c6-daemon-recovery", model).await?;
        let pgid = app_server_process_group(&harness.scratch)?;
        terminate_process_group(pgid)?;
        let (cursor, _) = capture
            .wait(
                cursor,
                READY_TIMEOUT,
                "connection_lost gap",
                Matcher::GapReason("connection_lost"),
            )
            .await?;
        let (cursor, _) = capture
            .wait(
                cursor,
                READY_TIMEOUT,
                "fresh ready after real daemon death",
                Matcher::Type("amux.codex_ready"),
            )
            .await?;
        prompt_to_completion(
            &mut capture,
            cursor,
            "Reply with exactly C6_RECOVERED and nothing else.",
            "C6_RECOVERED",
        )
        .await?;
        harness.client().delete_agent(agent).await?;
        Ok(json!({
            "killed_process_group": pgid,
            "fault_injection_env_used": false,
            "assertions": {"gap_reason": "connection_lost", "fresh_ready": true, "post_recovery_turn": "completed"}
        }))
    }

    async fn raw_coexistence(harness: &mut Harness, model: &str) -> Result<Value> {
        let (agent, mut capture, cursor) = open(harness, "c7-raw-coexistence", model).await?;
        let mut raw = subscribe_raw(harness, agent).await?;
        let mut raw_bytes = raw_until(&mut raw, RAW_TIMEOUT, b"\x1b").await?;
        let (_, completed) = prompt_to_completion(
            &mut capture,
            cursor,
            "Reply with exactly C7_BOTH_PLANES and nothing else.",
            "C7_BOTH_PLANES",
        )
        .await?;
        raw_bytes.extend(raw_until(&mut raw, RAW_TIMEOUT, b"C7_BOTH_PLANES").await?);
        std::fs::write(harness.scratch.out.join("raw.log"), &raw_bytes)?;
        harness.client().delete_agent(agent).await?;
        Ok(json!({
            "raw_bytes": raw_bytes.len(),
            "thread_id": completed.thread_id(),
            "assertions": {"raw_ansi_screen": true, "structured_completed": true, "response_visible_raw": true}
        }))
    }

    fn raw_pty_process_group(scratch: &Path) -> Result<i32> {
        let scratch = scratch
            .canonicalize()
            .with_context(|| format!("canonicalize capture scratch root {}", scratch.display()))?;
        let scratch = scratch.display().to_string();
        let output = Command::new("ps")
            .args(["-axo", "pid=,pgid=,command="])
            .output()
            .context("list processes for Codex raw PTY")?;
        if !output.status.success() {
            bail!("ps failed while locating the Codex raw PTY process group");
        }
        let mut groups = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                line.contains("codex resume")
                    && line.contains("--remote")
                    && line.contains("unix://")
                    && line.contains(&scratch)
                    && !line.contains("ps -axo")
            })
        {
            let mut fields = line.split_whitespace();
            let _pid: i32 = fields
                .next()
                .context("raw PTY ps row missing pid")?
                .parse()?;
            let pgid: i32 = fields
                .next()
                .context("raw PTY ps row missing pgid")?
                .parse()?;
            if !groups.contains(&pgid) {
                groups.push(pgid);
            }
        }
        match groups.as_slice() {
            [pgid] if *pgid > 1 => Ok(*pgid),
            [] => bail!("no raw Codex PTY process group found for isolated capture"),
            _ => bail!("multiple raw Codex PTY process groups found: {groups:?}"),
        }
    }

    async fn wait_for_process_group_exit(pgid: i32, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let output = Command::new("ps")
                .args(["-axo", "pgid="])
                .output()
                .context("list process groups while waiting for raw PTY teardown")?;
            if !output.status.success() {
                bail!("ps failed while waiting for raw PTY teardown");
            }
            let alive = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.trim().parse::<i32>().ok())
                .any(|candidate| candidate == pgid);
            if !alive {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("raw Codex PTY process group {pgid} survived final detach");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The product default: `amux new codex` with no `--name`. Raw attach goes
    /// through `codex resume`, which refuses a thread that was never
    /// persisted, and naming is what persists one — so an unnamed agent must
    /// still reach a live raw screen. No turn is taken; this costs no quota.
    async fn raw_unnamed(harness: &mut Harness, model: &str) -> Result<Value> {
        let (agent, mut capture, _) = open_named(harness, None, model).await?;
        let mut first_raw = subscribe_raw(harness, agent).await?;
        let first_raw_bytes = raw_until(&mut first_raw, RAW_TIMEOUT, b"\x1b").await?;
        let first_pgid = raw_pty_process_group(&harness.scratch.root)?;
        drop(first_raw);
        wait_for_process_group_exit(first_pgid, Duration::from_secs(10)).await?;

        let mut second_raw = subscribe_raw(harness, agent).await?;
        let second_raw_bytes = raw_until(&mut second_raw, RAW_TIMEOUT, b"\x1b").await?;
        std::fs::write(
            harness.scratch.out.join("raw.log"),
            [first_raw_bytes.as_slice(), second_raw_bytes.as_slice()].concat(),
        )?;
        let structured_rows_before_probe = capture.rows().len();
        let structured_probe_window = Duration::from_secs(1);
        // This type is never emitted. `wait` therefore keeps polling the live
        // stream until the outer bound, but still returns early on close/error.
        let (structured_subscription_held_through_reattach, structured_probe_result) =
            match tokio::time::timeout(
                structured_probe_window,
                capture.wait(
                    structured_rows_before_probe,
                    READY_TIMEOUT,
                    "post-raw-reattach structured stream probe",
                    Matcher::Type("amux.c9_structured_stream_probe"),
                ),
            )
            .await
            {
                Err(_) => (true, "open_for_full_probe_window"),
                Ok(Ok(_)) => (true, "structured_row_observed"),
                Ok(Err(error)) => {
                    return Err(error).context(
                        "existing Codex structured subscription failed after raw reattach",
                    );
                }
            };
        let structured_rows_after_probe = capture.rows().len();
        harness.client().delete_agent(agent).await?;
        Ok(json!({
            "raw_bytes": first_raw_bytes.len() + second_raw_bytes.len(),
            "agent_named": false,
            "turns_sent": 0,
            "zero_model_turns": true,
            "assertions": {
                "raw_ansi_screen_without_agent_name": true,
                "final_detach_tore_down_process_group": true,
                "reattach_reached_second_raw_ansi_screen": true,
                "structured_subscription_held_through_reattach": structured_subscription_held_through_reattach
            },
            "observed": {
                "first_raw_bytes": first_raw_bytes.len(),
                "second_raw_bytes": second_raw_bytes.len(),
                "structured_probe": {
                    "window_ms": structured_probe_window.as_millis(),
                    "result": structured_probe_result,
                    "rows_before": structured_rows_before_probe,
                    "rows_after": structured_rows_after_probe
                }
            }
        }))
    }

    /// Eager materialization is also required by amux's structured reconnect
    /// contract: its reconnect path issues `thread/resume`, even when the
    /// original in-memory attachment worked and no turn ever created history.
    async fn unnamed_reconnect(harness: &mut Harness, model: &str) -> Result<Value> {
        let (agent, capture, _) = open_named(harness, None, model).await?;
        drop(capture);
        let summary = harness.client().suspend().await?;
        if summary.suspended_count != 1 {
            bail!(
                "expected one zero-turn unnamed agent to suspend, got {}",
                summary.suspended_count
            );
        }
        harness.stop_for_suspend().await?;
        harness.restart().await?;
        let resume = harness.client().resume().await?;
        if resume.resumed_count != 1 || resume.failed_count != 0 {
            bail!("unnamed zero-turn resume summary was not 1/0: {resume:?}");
        }
        let mut reconnected = StructuredCapture::open(harness, agent).await?;
        reconnected.wait_ready().await?;
        harness.client().delete_agent(agent).await?;
        Ok(json!({
            "agent_named": false,
            "turns_sent": 0,
            "assertions": {
                "suspended": true,
                "resumed": true,
                "structured_reconnected": true,
                "same_agent": true
            }
        }))
    }

    async fn raw_fanout(harness: &mut Harness, model: &str) -> Result<Value> {
        let (agent, mut capture, cursor) = open(harness, "c8-raw-fanout", model).await?;
        let mut first = subscribe_raw(harness, agent).await?;
        let first_prefix = raw_until(&mut first, RAW_TIMEOUT, b"\x1b").await?;
        let mut second = subscribe_raw(harness, agent).await?;
        let (first_drain, second_drain) =
            tokio::join!(drain_raw(&mut first), drain_raw(&mut second));
        let mut first_prelude = first_prefix;
        first_prelude.extend(first_drain?);
        let second_prelude = second_drain?;

        let prompt = "Reply with exactly C8_IDENTICAL_FANOUT and nothing else.";
        let input_id = capture.send_prompt(prompt).await?;
        let (first_live, second_live) = tokio::join!(
            raw_until(&mut first, RAW_TIMEOUT, b"C8_IDENTICAL_FANOUT"),
            raw_until(&mut second, RAW_TIMEOUT, b"C8_IDENTICAL_FANOUT")
        );
        let first_live = first_live?;
        let second_live = second_live?;
        std::fs::write(
            harness.scratch.out.join("raw-1.log"),
            [first_prelude.as_slice(), first_live.as_slice()].concat(),
        )?;
        std::fs::write(
            harness.scratch.out.join("raw-2.log"),
            [second_prelude.as_slice(), second_live.as_slice()].concat(),
        )?;
        if first_live != second_live {
            bail!(
                "two terminal_v1 subscribers received different live bytes: {} vs {}",
                first_live.len(),
                second_live.len()
            );
        }
        let (cursor, _) = capture
            .wait(
                cursor,
                READY_TIMEOUT,
                "fanout prompt input result",
                Matcher::InputOk(input_id),
            )
            .await?;
        capture
            .wait(
                cursor,
                TURN_TIMEOUT,
                "fanout turn completion",
                Matcher::TurnCompleted("completed"),
            )
            .await?;
        harness.client().delete_agent(agent).await?;
        Ok(json!({
            "live_bytes_each": first_live.len(),
            "assertions": {"two_subscribers": true, "identical_live_bytes": true, "structured_completed": true}
        }))
    }

    async fn run_scenario(harness: &mut Harness, runner: Scenario, model: &str) -> Result<Value> {
        match runner {
            Scenario::Pong => pong(harness, model).await,
            Scenario::ApprovalAllow => approval(harness, model, true).await,
            Scenario::ApprovalDeny => approval(harness, model, false).await,
            Scenario::Interrupt => interrupt(harness, model).await,
            Scenario::SuspendResume => suspend_resume(harness, model).await,
            Scenario::DaemonRecovery => daemon_recovery(harness, model).await,
            Scenario::RawCoexistence => raw_coexistence(harness, model).await,
            Scenario::RawFanout => raw_fanout(harness, model).await,
            Scenario::RawUnnamed => raw_unnamed(harness, model).await,
            Scenario::UnnamedReconnect => unnamed_reconnect(harness, model).await,
            Scenario::DynamicTools => dynamic_tools(harness, model).await,
            Scenario::InjectIdle => inject_idle(harness, model).await,
            Scenario::InjectBusy => inject_busy(harness, model).await,
            Scenario::LastMessage => last_message(harness, model).await,
        }
    }

    fn finalize(
        out: &Path,
        scratch: &Path,
        spec: &ScenarioSpec,
        model: &str,
        version: &str,
        notes: Value,
        failure: Option<&str>,
    ) -> Result<()> {
        for (source, destination) in [
            ("rows.jsonl", "redacted.rows.jsonl"),
            ("io.jsonl", "redacted.io.jsonl"),
            ("observed.rows.jsonl", "redacted.observed.rows.jsonl"),
        ] {
            let source = out.join(source);
            if source.exists() {
                let redacted = redact::redact_jsonl(&std::fs::read_to_string(&source)?, scratch)
                    .with_context(|| format!("redact {}", source.display()))?;
                std::fs::write(out.join(destination), redacted)?;
            }
        }
        let meta = json!({
            "scenario": spec.id,
            "requirement": spec.requirement,
            "captured_at": chrono::Utc::now().to_rfc3339(),
            "codex_version": version,
            "model": model,
            "timeout_seconds": spec.timeout.as_secs(),
            "harness": format!("timeout 600 cargo test -p amux --test codex_capture -- {}", spec.id),
            "synthetic_prompts": !matches!(spec.runner, Scenario::RawUnnamed | Scenario::UnnamedReconnect),
            "isolated_codex_home": true,
            "notes": notes,
            "failure": failure,
        });
        let meta = redact::redact_json(&serde_json::to_string(&meta)?, scratch)?;
        std::fs::write(out.join("meta.json"), meta)?;
        Ok(())
    }

    async fn run(selected: Vec<&ScenarioSpec>) -> Result<()> {
        let base = std::env::var_os("AMUX_CODEX_CAPTURE_DIR")
            .map(workspace_path)
            .unwrap_or_else(|| {
                workspace_path("target/codex-capture").join(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("clock is after the Unix epoch")
                        .as_secs()
                        .to_string(),
                )
            });
        let model =
            std::env::var("AMUX_CODEX_CAPTURE_MODEL").unwrap_or_else(|_| "gpt-5.6-sol".into());
        let version = harness::codex_version();
        let mut failures = Vec::new();

        for spec in selected {
            let out = base.join(spec.id);
            std::fs::create_dir_all(&out)?;
            for entry in std::fs::read_dir(&out)? {
                let path = entry?.path();
                if path.is_file() {
                    std::fs::remove_file(path)?;
                }
            }
            println!(
                "=== {} ({}, model={model}, timeout={}s) ===",
                spec.id,
                spec.requirement,
                spec.timeout.as_secs()
            );
            println!("capture={}", out.display());
            let started = std::time::Instant::now();
            let mut harness = match Harness::start(out.clone()).await {
                Ok(harness) => harness,
                Err(error) => {
                    let message = format!("harness startup assertion: {error:#}");
                    println!(
                        "=== {} FAIL: {message}; capture={} ===",
                        spec.id,
                        out.display()
                    );
                    failures.push((spec.id, message));
                    continue;
                }
            };
            let scratch = harness.scratch.root.clone();
            let result = tokio::time::timeout(
                spec.timeout,
                run_scenario(&mut harness, spec.runner, &model),
            )
            .await
            .map_err(|_| anyhow!("scenario timeout after {:?}", spec.timeout))
            .and_then(|result| result);
            let cleanup = harness.shutdown().await;
            let result = match (result, cleanup) {
                (Ok(notes), Ok(())) => Ok(notes),
                (Err(error), Ok(())) => Err(error),
                (Ok(_), Err(cleanup_error)) => Err(anyhow!(
                    "scenario passed but cleanup failed: {cleanup_error:#}"
                )),
                (Err(error), Err(cleanup_error)) => {
                    Err(anyhow!("{error:#}; cleanup also failed: {cleanup_error:#}"))
                }
            };

            match result {
                Ok(notes) => {
                    finalize(&out, &scratch, spec, &model, &version, notes, None)?;
                    println!(
                        "=== {} PASS ({:.1}s); capture={} ===",
                        spec.id,
                        started.elapsed().as_secs_f64(),
                        out.display()
                    );
                }
                Err(error) => {
                    let message = format!("{} structural/world assertion: {error:#}", spec.id);
                    finalize(
                        &out,
                        &scratch,
                        spec,
                        &model,
                        &version,
                        json!({}),
                        Some(&message),
                    )?;
                    println!(
                        "=== {} FAIL: {message}; capture={} ===",
                        spec.id,
                        out.display()
                    );
                    failures.push((spec.id, message));
                }
            }
        }
        if failures.is_empty() {
            println!("codex_capture: all selected C-suite scenarios passed");
            Ok(())
        } else {
            bail!(
                "{} C-suite scenario(s) failed: {failures:?}",
                failures.len()
            )
        }
    }

    validate_scenarios()?;
    let names: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    if names.is_empty() {
        println!(
            "codex_capture: no scenarios named; skipping (opt-in real-Codex C suite — pass `c-all` or scenario ids)"
        );
        return Ok(());
    }
    if names
        .iter()
        .all(|name| name.starts_with("a2a_fixture_") || name == "a2a_version_gate_parser")
    {
        return Ok(());
    }
    let selected: Vec<&ScenarioSpec> = if names.iter().any(|name| name == "c-all") {
        SCENARIOS.iter().collect()
    } else {
        names
            .iter()
            .map(|name| {
                SCENARIOS
                    .iter()
                    .find(|spec| spec.id == name)
                    .ok_or_else(|| {
                        let known: Vec<_> = SCENARIOS.iter().map(|spec| spec.id).collect();
                        anyhow!("unknown Codex capture scenario `{name}`; known: {known:?}")
                    })
            })
            .collect::<Result<_>>()?
    };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(selected))
}

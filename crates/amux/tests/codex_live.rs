//! Maintained, opt-in real-Codex backend suite.
//!
//! With no scenario argument this target prints usage and exits successfully
//! without creating a process or making a network request. Build the CLI first,
//! then run every invocation under an outer timeout:
//!
//! ```text
//! AMUX_LIVE_OUT=target/codex-live wt run codex-live -- all
//! ```
//!
//! The suite drives the prebuilt `target/debug/amux`, which this target does
//! not rebuild; the harness refuses to start against a binary older than the
//! prerequisites in Cargo's depfile rather than reporting on code it never ran.
//!
//! `all` selects every retained process-level scenario; individual names may
//! be listed instead. Each scenario has one row in [`SCENARIOS`], so its id,
//! requirement, timeout, and runner stay together. Captures land in a
//! scenario-named child of `AMUX_LIVE_OUT` (or a timestamped default)
//! and include backend rows, provider IO, observed subscription rows, raw bytes
//! where applicable, redacted copies, and version-stamped metadata.

#[cfg(unix)]
#[allow(dead_code)]
#[path = "support/live_installation.rs"]
mod live_installation;

#[cfg(unix)]
mod codex_live {
    pub mod args;
    pub mod depfile;
    pub mod harness;
    pub mod redact;
    pub mod structure;
}

#[cfg(not(unix))]
fn main() {
    println!("codex_live: real-Codex scenarios are only available on Unix");
}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    use std::fs::File;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, anyhow, bail};
    use codex_live::{args, harness, redact, structure};
    use harness::{
        Harness, RAW_TIMEOUT, READY_TIMEOUT, StructuredCapture, TURN_TIMEOUT,
        app_server_process_group, drain_raw, raw_until, subscribe_raw, terminate_process_group,
    };
    use serde_json::{Value, json};
    use structure::Matcher;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Scenario {
        SuspendResume,
        DaemonRecovery,
        RawCoexistence,
        RawFanout,
        RawUnnamed,
        RawNamed,
        UnnamedReconnect,
        Roundtrip,
        AttachTool,
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
        scenario(
            "suspend_resume",
            "suspend and resume across a server restart",
            420,
            Scenario::SuspendResume,
        ),
        scenario(
            "daemon_recovery",
            "app-server process-group recovery",
            420,
            Scenario::DaemonRecovery,
        ),
        scenario(
            "raw_coexistence",
            "raw and structured planes live at once",
            420,
            Scenario::RawCoexistence,
        ),
        scenario(
            "raw_fanout",
            "two-terminal byte fanout",
            420,
            Scenario::RawFanout,
        ),
        scenario(
            "raw_unnamed",
            "raw attach, lease teardown and reattach on an unnamed agent",
            300,
            Scenario::RawUnnamed,
        ),
        scenario(
            "raw_named",
            "zero-turn raw attach and reattach on a named agent",
            300,
            Scenario::RawNamed,
        ),
        scenario(
            "unnamed_reconnect",
            "zero-turn persistence and resume across server restart",
            300,
            Scenario::UnnamedReconnect,
        ),
        scenario(
            "cross_kind_completion",
            "agent MCP tools and cross-kind child completion",
            600,
            Scenario::Roundtrip,
        ),
        scenario(
            "attach_tool",
            "agent attaches an amux-shot image",
            420,
            Scenario::AttachTool,
        ),
    ];

    fn validate_scenarios() -> Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        for spec in SCENARIOS {
            if !ids.insert(spec.id) {
                bail!("duplicate Codex live scenario id: {}", spec.id);
            }
            if spec.requirement.is_empty() || spec.timeout.is_zero() {
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

    fn display_path(path: &Path) -> String {
        path.strip_prefix(workspace_path(""))
            .unwrap_or(path)
            .display()
            .to_string()
    }

    fn report(transcript: &mut File, line: String) -> Result<()> {
        println!("{line}");
        writeln!(transcript, "{line}")?;
        transcript.flush()?;
        Ok(())
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
        open_named_in(harness, scenario, model, &harness.scratch.project).await
    }

    async fn open_named_in(
        harness: &Harness,
        scenario: Option<&str>,
        model: &str,
        working_dir: &Path,
    ) -> Result<(uuid::Uuid, StructuredCapture, usize)> {
        let agent = harness.create_agent(scenario, model, working_dir).await?;
        let mut capture = StructuredCapture::open(harness, agent).await?;
        let cursor = capture.wait_ready().await?;
        Ok((agent, capture, cursor))
    }

    async fn attach_tool(harness: &mut Harness, model: &str) -> Result<Value> {
        use amux::{AgentIdentifier, ArtifactKind, ArtifactRef};
        use amux_ui::attachments::{Mention, MentionKind, format_mention};

        const NAME: &str = "agent-attach-codex.png";
        let shot = harness.scratch.project.join(NAME);
        let agent = harness
            .create_agent_with_policy(
                Some("attach-tool"),
                model,
                &harness.scratch.project,
                "never",
                "workspace-write",
            )
            .await?;
        let mut capture = StructuredCapture::open(harness, agent).await?;
        let cursor = capture.wait_ready().await?;
        let prompt = format!(
            "Run exactly this command with the shell tool: amux-shot render chat-attachment-blocks --out {}\n\
             Then call the amux attach tool exactly once with path set to {} and name set to {NAME}. \
             Finally reply with exactly the text returned by attach, with no code fence or other text.",
            shot.display(),
            shot.display(),
        );
        let input_id = capture.send_prompt(&prompt).await?;
        let (cursor, _) = capture
            .wait(
                cursor,
                READY_TIMEOUT,
                "attach-tool prompt accepted",
                Matcher::InputOk(input_id),
            )
            .await?;
        let (refs_cursor, refs_row) = capture
            .wait(
                cursor,
                TURN_TIMEOUT,
                "attachment refs row",
                Matcher::Type("amux.attachments"),
            )
            .await?;
        if !refs_row.json.get("input_id").is_some_and(Value::is_null) {
            bail!("agent attachment refs row did not carry a null input_id");
        }
        let refs = serde_json::from_value::<Vec<ArtifactRef>>(
            refs_row
                .json
                .get("refs")
                .cloned()
                .context("attachment row has no refs")?,
        )?;
        let [artifact] = refs.as_slice() else {
            bail!(
                "agent attachment row carried {} refs, expected one",
                refs.len()
            );
        };
        if artifact.name != NAME {
            bail!(
                "attachment refs row named {}, expected {NAME}",
                artifact.name
            );
        }
        let mention = format_mention(&Mention {
            kind: MentionKind::Image {
                id: artifact.id.clone(),
            },
            name: artifact.name.clone(),
            size: Some(artifact.size),
            path: None,
        });
        let (reply_cursor, _) = capture
            .wait(
                refs_cursor,
                TURN_TIMEOUT,
                "completed reply containing the attachment mention",
                Matcher::AgentTextContains(mention.clone()),
            )
            .await?;
        capture
            .wait(
                reply_cursor,
                TURN_TIMEOUT,
                "attach-tool turn completion",
                Matcher::TurnCompleted("completed"),
            )
            .await?;
        let tool_use = capture.rows().iter().any(|row| {
            row.json.pointer("/item/type").and_then(Value::as_str) == Some("mcpToolCall")
                && row.json.pointer("/item/server").and_then(Value::as_str) == Some("amux")
                && row.json.pointer("/item/tool").and_then(Value::as_str) == Some("attach")
                && row.json.pointer("/item/status").and_then(Value::as_str) == Some("completed")
        });
        if !tool_use {
            bail!("Codex reply was not preceded by a completed amux attach tool call");
        }
        let render_tool_use = capture.rows().iter().any(|row| {
            row.json.pointer("/item/type").and_then(Value::as_str) == Some("commandExecution")
                && row
                    .json
                    .pointer("/item/command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| {
                        command.contains("amux-shot render chat-attachment-blocks")
                    })
                && row.json.pointer("/item/status").and_then(Value::as_str) == Some("completed")
                && row.json.pointer("/item/exitCode").and_then(Value::as_i64) == Some(0)
        });
        if !render_tool_use {
            bail!("Codex did not successfully render the screenshot with amux-shot");
        }

        let source = std::fs::read(&shot)
            .with_context(|| format!("read rendered screenshot {}", shot.display()))?;
        let (stored, stored_bytes) = harness
            .client()
            .get_artifact(AgentIdentifier::Id(agent), &artifact.id)
            .await
            .context("fetch attached artifact from the daemon")?;
        if stored != *artifact || stored_bytes != source {
            bail!("stored attachment did not match the refs row and rendered PNG");
        }
        if artifact.kind != ArtifactKind::Image
            || artifact.mime != "image/png"
            || !source.starts_with(b"\x89PNG\r\n\x1a\n")
        {
            bail!(
                "attached artifact was not the rendered PNG: kind={:?} mime={}",
                artifact.kind,
                artifact.mime
            );
        }

        let owner_root = harness
            .scratch
            .config
            .data_dir
            .join("agents")
            .join(agent.to_string())
            .join("artifacts");
        let digest = artifact
            .id
            .as_str()
            .strip_prefix("sha256:")
            .expect("artifact ids are canonical");
        let blob = owner_root.join("blobs").join(digest);
        let index_json: Value = serde_json::from_slice(
            &std::fs::read(owner_root.join("index.json")).context("read artifact owner index")?,
        )?;
        let pinned = index_json
            .get("artifacts")
            .and_then(|artifacts| artifacts.get(artifact.id.as_str()))
            .and_then(|meta| meta.get("pinned_at"))
            .is_some_and(|pinned_at| !pinned_at.is_null());
        if std::fs::read(&blob).ok().as_deref() != Some(source.as_slice()) || !pinned {
            bail!("artifact owner did not persist and pin the rendered PNG");
        }

        harness.client().delete_agent(agent).await?;
        Ok(json!({
            "agent_id": agent,
            "artifact": artifact,
            "mention": mention,
            "world": {
                "rendered_png": shot,
                "owner_blob": blob,
            },
            "assertions": {
                "amux_shot_render": true,
                "attach_tool_use": true,
                "refs_row": true,
                "null_input_id": true,
                "reply_contains_exact_mention": true,
                "stored_bytes_match_render": true,
                "owner_blob_pinned": true,
            }
        }))
    }

    async fn roundtrip(harness: &mut Harness, model: &str) -> Result<Value> {
        let child_marker = "A2A_C15_CHILD_DONE";
        let parent_marker = "A2A_C15_PARENT_RECEIVED";
        let trusted_working_dir = workspace_path(".")
            .canonicalize()
            .context("canonicalize trusted workspace for cross-kind child")?;
        let (parent, mut capture, cursor) =
            open_named_in(harness, Some("c15-roundtrip"), model, &trusted_working_dir).await?;
        let prompt = format!(
            "Call the spawn tool exactly once with kind=claude and prompt=\"Reply with exactly {child_marker} and nothing else.\" After its completion arrives, reply with exactly {parent_marker} and nothing else."
        );
        let input_id = capture.send_prompt(&prompt).await?;
        let (cursor, _) = capture
            .wait(
                cursor,
                READY_TIMEOUT,
                "roundtrip prompt accepted",
                Matcher::InputOk(input_id),
            )
            .await?;
        let (cursor, completion) = capture
            .wait(
                cursor,
                Duration::from_secs(480),
                "Claude child completion delivered to Codex parent",
                Matcher::MessageContains {
                    kind: "completed",
                    text: child_marker.to_string(),
                },
            )
            .await?;
        debug_assert_eq!(
            completion.message().map(|(kind, _)| kind),
            Some("completed")
        );
        capture
            .wait(
                cursor,
                TURN_TIMEOUT,
                "Codex parent acknowledges child completion",
                Matcher::AgentTextContains(parent_marker.to_string()),
            )
            .await?;

        let child = harness
            .client()
            .list_agents()
            .await?
            .into_iter()
            .find(|agent| agent.parent.is_some_and(|edge| edge.agent_id == parent))
            .context("spawned Claude child missing from family inventory")?;
        if child.kind
            != (amux::AgentKind::Claude {
                driver: amux::ClaudeDriver::Pty,
            })
        {
            bail!("spawned child was {}, expected claude/pty", child.kind);
        }
        if child.working_dir != trusted_working_dir {
            bail!(
                "spawned child working directory was {}, expected {}",
                child.working_dir.display(),
                trusted_working_dir.display()
            );
        }
        let child_id = child.id;
        harness.client().delete_agent(parent).await?;
        Ok(json!({
            "parent_id": parent,
            "child_id": child_id,
            "assertions": {
                "spawn_tool": true,
                "cross_kind_child": "claude",
                "completion_delivered": child_marker,
                "parent_acknowledged": parent_marker,
                "child_inherited_trusted_working_dir": child.working_dir == trusted_working_dir,
            }
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

        let suspended_count = crate::live_installation::suspend(&harness.scratch.root).await?;
        if suspended_count != 1 {
            bail!("expected one suspended agent, got {}", suspended_count);
        }
        harness.stop_for_suspend().await?;
        harness.restart().await?;
        let resume = crate::live_installation::resume(&harness.scratch.root).await?;
        if resume.0 != 1 || resume.1 != 0 {
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
        let mut raw_bytes = raw_until(&mut raw, RAW_TIMEOUT, b"\x1b")
            .await
            .context("wait for the initial raw terminal screen")?;
        let (completed, raw_response) = tokio::join!(
            prompt_to_completion(
                &mut capture,
                cursor,
                "Reply with exactly C7_BOTH_PLANES and nothing else.",
                "C7_BOTH_PLANES",
            ),
            raw_until(&mut raw, RAW_TIMEOUT, b"C7_BOTH_PLANES")
        );
        let (_, completed) =
            completed.context("complete a structured turn while the raw terminal is subscribed")?;
        raw_bytes
            .extend(raw_response.context("wait for the structured response on the raw terminal")?);
        std::fs::write(harness.scratch.out.join("raw.log"), &raw_bytes)?;
        harness
            .client()
            .delete_agent(agent)
            .await
            .context("delete the two-plane Codex agent")?;
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

    fn recorded_thread_id(harness: &Harness) -> Result<String> {
        let io = std::fs::read_to_string(harness.scratch.out.join("io.jsonl"))?;
        for line in io.lines().rev() {
            let record: Value = serde_json::from_str(line)?;
            let message: Value =
                serde_json::from_str(record["line"].as_str().context("provider IO has no line")?)?;
            if let Some(id) = message.pointer("/result/thread/id").and_then(Value::as_str) {
                return Ok(id.to_string());
            }
        }
        bail!("provider IO has no successful thread response")
    }

    /// Startup draws ANSI before resume has succeeded. A local /status command
    /// must display the expected session identity to prove the TUI is usable.
    /// It does not send a prompt or invoke the model.
    async fn raw_status(
        harness: &Harness,
        agent: uuid::Uuid,
        model: &str,
        thread_id: &str,
    ) -> Result<(amux::SessionStream, Vec<u8>)> {
        let mut raw = subscribe_raw(harness, agent).await?;
        let mut bytes = raw_until(&mut raw, RAW_TIMEOUT, model.as_bytes()).await?;
        harness
            .client()
            .send_input(amux::SendInputRequest {
                agent: amux::AgentIdentifier::Id(agent),
                input_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                io_protocol: amux::terminal_io::TERMINAL_V1.into(),
                payload: bytes::Bytes::from_static(b"/status"),
                pin: Vec::new(),
            })
            .await?;
        // Wait for the composer to echo the command before submitting it;
        // a command and Enter in one write can be classified as a paste.
        bytes.extend(raw_until(&mut raw, RAW_TIMEOUT, b"/status").await?);
        harness
            .client()
            .send_input(amux::SendInputRequest {
                agent: amux::AgentIdentifier::Id(agent),
                input_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                io_protocol: amux::terminal_io::TERMINAL_V1.into(),
                payload: bytes::Bytes::from_static(b"\r"),
                pin: Vec::new(),
            })
            .await?;
        bytes.extend(raw_until(&mut raw, RAW_TIMEOUT, thread_id.as_bytes()).await?);
        Ok((raw, bytes))
    }

    async fn raw_reattach(harness: &mut Harness, model: &str, name: Option<&str>) -> Result<Value> {
        let (agent, mut capture, _) = open_named(harness, name, model).await?;
        let thread_id = recorded_thread_id(harness)?;
        let (first_raw, first_raw_bytes) = raw_status(harness, agent, model, &thread_id).await?;
        let first_pgid = raw_pty_process_group(&harness.scratch.root)?;
        drop(first_raw);
        wait_for_process_group_exit(first_pgid, Duration::from_secs(10)).await?;

        let (_second_raw, second_raw_bytes) = raw_status(harness, agent, model, &thread_id).await?;
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
                    Matcher::Type("amux.codex_live_structured_stream_probe"),
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
            "agent_named": name.is_some(),
            "thread_id": thread_id,
            "turns_sent": 0,
            "zero_model_turns": true,
            "assertions": {
                "raw_status_identified_thread": true,
                "final_detach_tore_down_process_group": true,
                "reattach_status_identified_same_thread": true,
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
        let thread_id = recorded_thread_id(harness)?;
        let server_pgid = app_server_process_group(&harness.scratch)?;
        drop(capture);
        let suspended_count = crate::live_installation::suspend(&harness.scratch.root).await?;
        if suspended_count != 1 {
            bail!(
                "expected one zero-turn unnamed agent to suspend, got {}",
                suspended_count
            );
        }
        harness.stop_for_suspend().await?;
        wait_for_process_group_exit(server_pgid, Duration::from_secs(10)).await?;
        harness.restart().await?;
        let resume = crate::live_installation::resume(&harness.scratch.root).await?;
        if resume.0 != 1 || resume.1 != 0 {
            bail!("unnamed zero-turn resume summary was not 1/0: {resume:?}");
        }
        let mut reconnected = StructuredCapture::open(harness, agent).await?;
        reconnected.wait_ready().await?;
        if recorded_thread_id(harness)? != thread_id {
            bail!("zero-turn restart changed the Codex thread identity");
        }
        let (_raw, raw_bytes) = raw_status(harness, agent, model, &thread_id).await?;
        std::fs::write(harness.scratch.out.join("raw.log"), raw_bytes)?;
        harness.client().delete_agent(agent).await?;
        Ok(json!({
            "agent_named": false,
            "thread_id": thread_id,
            "turns_sent": 0,
            "assertions": {
                "suspended": true,
                "resumed": true,
                "structured_reconnected": true,
                "same_agent": true,
                "same_thread": true,
                "app_server_restarted": true,
                "raw_status_identified_thread": true
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
        let raw_pgid = raw_pty_process_group(&harness.scratch.root)?;
        drop(first);
        drop(second);
        wait_for_process_group_exit(raw_pgid, Duration::from_secs(10)).await?;
        harness.client().delete_agent(agent).await?;
        Ok(json!({
            "live_bytes_each": first_live.len(),
            "assertions": {"two_subscribers": true, "identical_live_bytes": true, "structured_completed": true, "final_detach_tore_down_process_group": true}
        }))
    }

    async fn run_scenario(harness: &mut Harness, runner: Scenario, model: &str) -> Result<Value> {
        match runner {
            Scenario::SuspendResume => suspend_resume(harness, model).await,
            Scenario::DaemonRecovery => daemon_recovery(harness, model).await,
            Scenario::RawCoexistence => raw_coexistence(harness, model).await,
            Scenario::RawFanout => raw_fanout(harness, model).await,
            Scenario::RawUnnamed => raw_reattach(harness, model, None).await,
            Scenario::RawNamed => raw_reattach(harness, model, Some("raw-named")).await,
            Scenario::UnnamedReconnect => unnamed_reconnect(harness, model).await,
            Scenario::Roundtrip => roundtrip(harness, model).await,
            Scenario::AttachTool => attach_tool(harness, model).await,
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
        let amux_log = scratch.join("amux.log");
        if amux_log.exists() {
            let redacted = redact::redact_log(&std::fs::read_to_string(&amux_log)?, scratch)
                .with_context(|| format!("redact {}", amux_log.display()))?;
            std::fs::write(out.join("amux.log"), redacted)?;
        }
        let meta = json!({
            "scenario": spec.id,
            "requirement": spec.requirement,
            "captured_at": chrono::Utc::now().to_rfc3339(),
            "codex_version": version,
            "model": model,
            "timeout_seconds": spec.timeout.as_secs(),
            "harness": format!("wt run codex-live -- {}", spec.id),
            "synthetic_prompts": !matches!(spec.runner, Scenario::RawUnnamed | Scenario::RawNamed | Scenario::UnnamedReconnect),
            "isolated_codex_home": true,
            "notes": notes,
            "failure": failure,
        });
        let meta = redact::redact_json(&serde_json::to_string(&meta)?, scratch)?;
        std::fs::write(out.join("meta.json"), meta)?;
        Ok(())
    }

    async fn run(selected: Vec<&ScenarioSpec>) -> Result<()> {
        let base = std::env::var_os("AMUX_LIVE_OUT")
            .map(workspace_path)
            .unwrap_or_else(|| {
                workspace_path("target/codex-live").join(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("clock is after the Unix epoch")
                        .as_secs()
                        .to_string(),
                )
            });
        let model =
            std::env::var("AMUX_CODEX_LIVE_MODEL").unwrap_or_else(|_| "gpt-5.6-luna".into());
        let version = harness::codex_version();
        std::fs::create_dir_all(&base)?;
        let mut transcript = File::create(base.join("codex-live.txt"))?;
        report(
            &mut transcript,
            format!("codex_live: version={version} model={model}"),
        )?;
        if version != "0.153.4" {
            bail!("Codex live suite requires codex-cli 0.153.4, found {version}");
        }
        let mut failures = Vec::new();

        for spec in selected {
            let out = base.join(spec.id);
            let shown_out = display_path(&out);
            std::fs::create_dir_all(&out)?;
            for entry in std::fs::read_dir(&out)? {
                let path = entry?.path();
                if path.is_file() {
                    std::fs::remove_file(path)?;
                }
            }
            report(
                &mut transcript,
                format!(
                    "=== {} ({}, model={model}, timeout={}s) ===",
                    spec.id,
                    spec.requirement,
                    spec.timeout.as_secs()
                ),
            )?;
            report(&mut transcript, format!("capture={shown_out}"))?;
            let started = std::time::Instant::now();
            let mut harness = match Harness::start(out.clone()).await {
                Ok(harness) => harness,
                Err(error) => {
                    let message = format!("harness startup assertion: {error:#}");
                    report(
                        &mut transcript,
                        format!("=== {} FAIL: {message}; capture={} ===", spec.id, shown_out),
                    )?;
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
                    report(
                        &mut transcript,
                        format!(
                            "=== {} PASS ({:.1}s); capture={} ===",
                            spec.id,
                            started.elapsed().as_secs_f64(),
                            shown_out
                        ),
                    )?;
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
                    report(
                        &mut transcript,
                        format!("=== {} FAIL: {message}; capture={} ===", spec.id, shown_out),
                    )?;
                    failures.push((spec.id, message));
                }
            }
        }
        if failures.is_empty() {
            report(
                &mut transcript,
                "codex_live: all selected backend scenarios passed".into(),
            )?;
            Ok(())
        } else {
            bail!(
                "{} Codex live scenario(s) failed: {failures:?}",
                failures.len()
            )
        }
    }

    validate_scenarios()?;
    let names: Vec<String> = std::env::args().skip(1).collect();
    let known = SCENARIOS.iter().map(|spec| spec.id).collect::<Vec<_>>();
    let selected = args::select(&names, &known).map_err(anyhow::Error::msg)?;
    if selected.is_empty() {
        println!("{}", args::USAGE);
        return Ok(());
    }
    let selected = selected
        .into_iter()
        .map(|index| &SCENARIOS[index])
        .collect();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(selected))
}

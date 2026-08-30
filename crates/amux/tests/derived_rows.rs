#![cfg(unix)]

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use amux::claude_sdk_io::ClaudeSdkV1Input;
use amux::codex_io::CodexSdkV1Input;
use amux::derived_rows_test_support::{ClaudeSdkBackendHarness, CodexBackendHarness};
use anyhow::{Context as _, Result, bail};
use claude::sdk::{PermissionResult, QueryOptions};
use codex::{
    ApprovalPolicy, Codex, CodexConfig, DynamicToolCallResponse, FunctionDynamicToolSpec,
    ListThreadsParams, SandboxMode, Thread, ThreadConfig,
};
use replay_support::{
    IoDirection, Recording, ReplayAdvance, ReplayController, ReplayOptions, ReplayTransport,
    StrictReplay, load_recording, strict_replay,
};
use serde_json::{Value, json};

const UPDATE_FLAG: &str = "UPDATE_DERIVED_ROWS";
const MODEL: &str = codex::specs::CAPTURE_MODEL;
const CLAUDE_SDK_DERIVATIONS: &[(&str, &str)] = &[
    ("text_turn", "session/text_turn"),
    ("permission_callback", "tools/permission_callback"),
    ("interrupted", "results/interrupted"),
    ("resumed", "history/resumed"),
    ("multi_turn", "session/multi_turn"),
];

struct Runtime {
    codex: Codex,
    thread: Thread,
    backend: CodexBackendHarness,
    controller: ReplayController,
    replay_driver: tokio::task::JoinHandle<()>,
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_backend")
}

fn claude_sdk_fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-sdk")
}

fn claude_sdk_recordings_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../claude/fixtures/sdk")
}

fn thread_config(spec: &str) -> ThreadConfig {
    let mut config = ThreadConfig {
        model: Some(MODEL.to_string()),
        cwd: Some("<MACHINE_PATH>".to_string()),
        approval_policy: Some(ApprovalPolicy::OnRequest),
        sandbox: Some(SandboxMode::WorkspaceWrite),
        ..ThreadConfig::default()
    };
    if matches!(spec, "approval_allow" | "approval_deny") {
        config.sandbox = Some(SandboxMode::ReadOnly);
    }
    if spec == "dynamic_tools" {
        config.dynamic_tools = Some(vec![FunctionDynamicToolSpec {
            name: "send".to_string(),
            description: "Send a short message to another agent.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {"to": {"type": "string"}, "text": {"type": "string"}},
                "required": ["to", "text"]
            }),
            defer_loading: None,
        }]);
    }
    config
}

async fn open_runtime(spec: &str, recording_dir: &Path) -> Result<Runtime> {
    let recording = load_recording(recording_dir)
        .with_context(|| format!("load Codex recording {}", recording_dir.display()))?;
    let mut replay = strict_replay(&recording, ReplayOptions::default());
    let transport = if replay.transports.len() == 1 {
        replay
            .transports
            .pop_first()
            .map(|(_, transport)| transport)
    } else {
        replay.transports.remove("<default>")
    }
    .context("recording has no default app-server transport")?;
    let controller = replay.controller.clone();
    let driver_controller = controller.clone();
    let replay_driver = tokio::spawn(async move {
        while let ReplayAdvance::Advanced { .. } | ReplayAdvance::BlockedOnWrite =
            driver_controller.advance_one().await
        {
            tokio::task::yield_now().await;
        }
    });
    let codex = Codex::from_io(
        transport.reader,
        transport.writer,
        CodexConfig {
            model: Some(MODEL.to_string()),
            client_name: "amux-codex-spec".to_string(),
            ..CodexConfig::default()
        },
    )
    .await
    .context("initialize Codex over strict replay")?;
    let thread = codex
        .start_thread(thread_config(spec))
        .await
        .with_context(|| format!("start replayed thread for {spec}"))?;
    let session = codex::open(thread.clone())
        .await
        .with_context(|| format!("open replayed session for {spec}"))?;
    let backend = CodexBackendHarness::with_session(session).await?;
    Ok(Runtime {
        codex,
        thread,
        backend,
        controller,
        replay_driver,
    })
}

fn text_input(text: &str) -> Vec<u8> {
    serde_json::to_vec(&json!([{"type": "text", "text": text}])).expect("fixed Codex input is JSON")
}

async fn send_turn(runtime: &Runtime, id: &str, text: &str) -> Result<()> {
    runtime
        .backend
        .send(
            id.as_bytes(),
            CodexSdkV1Input::UserTurn {
                input: text_input(text),
            },
        )
        .await
}

fn injected_item(text: &str) -> Value {
    json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": text}]
    })
}

async fn drive(runtime: &mut Runtime, spec: &str) -> Result<()> {
    match spec {
        "initialize_and_start" => {
            runtime
                .backend
                .wait_for_type("mcpServer/startupStatus/updated")
                .await?;
        }
        "turn_round_trip" => {
            send_turn(
                runtime,
                "turn",
                "Reply with exactly CODEX_SPEC_PONG and nothing else.",
            )
            .await?;
            runtime.backend.wait_for_type("turn/completed").await?;
        }
        "approval_allow" | "approval_deny" => {
            send_turn(
                runtime,
                "approval-turn",
                "Run this exact shell command and no substitute: /usr/bin/touch <MACHINE_PATH> Then say DONE.",
            )
            .await?;
            let ask = runtime
                .backend
                .wait_for_type("amux.codex_approval_required")
                .await?;
            let request_id = serde_json::to_vec(&ask["request_id"])?;
            let allow = spec == "approval_allow";
            runtime
                .backend
                .send(
                    if allow { b"allow" } else { b"deny" },
                    CodexSdkV1Input::ApprovalDecision {
                        request_id,
                        decision: if allow { "accept" } else { "decline" }.to_string(),
                    },
                )
                .await?;
            runtime.backend.wait_for_type("turn/completed").await?;
        }
        "interrupt" => {
            send_turn(
                runtime,
                "interrupt-turn",
                "Count slowly from one to one hundred, one number per line.",
            )
            .await?;
            runtime.backend.wait_for_type("turn/started").await?;
            runtime
                .backend
                .send(
                    b"interrupt",
                    CodexSdkV1Input::Interrupt {
                        turn_id: String::new(),
                    },
                )
                .await?;
            runtime.backend.wait_for_type("turn/completed").await?;
        }
        "thread_list_and_resume" => {
            send_turn(
                runtime,
                "resume-turn",
                "Reply with exactly CODEX_SPEC_RESUME and nothing else.",
            )
            .await?;
            runtime.backend.wait_for_type("turn/completed").await?;
            let id = runtime.thread.id().to_string();
            runtime
                .codex
                .rename_thread(&id, "codex-spec-resume")
                .await?;
            let listed = runtime
                .codex
                .list_threads(ListThreadsParams::default())
                .await?;
            if !listed.data.iter().any(|item| item.id == id) {
                bail!("thread/list omitted the replayed thread");
            }
            runtime
                .codex
                .resume_thread(&id, thread_config(spec))
                .await?;
            runtime.backend.wait_for_ingest_exit().await?;
        }
        "dynamic_tools" => {
            send_turn(
                runtime,
                "dynamic-turn",
                "Call the send tool exactly once with to=probe and text=CODEX_SPEC_SENT. Do not use any other tool.",
            )
            .await?;
            let ask = runtime
                .backend
                .wait_for_type("amux.codex_approval_required")
                .await?;
            let request_id = serde_json::from_value(ask["request_id"].clone())?;
            runtime
                .thread
                .respond_tool_call(
                    request_id,
                    DynamicToolCallResponse {
                        content_items: vec![json!({"type": "inputText", "text": "sent"})],
                        success: true,
                    },
                )
                .await?;
            runtime.backend.wait_for_type("turn/completed").await?;
        }
        "inject_idle" => {
            runtime
                .thread
                .inject_items(vec![injected_item(
                    "Reply with exactly CODEX_SPEC_INJECT_IDLE and nothing else.",
                )])
                .await?;
            runtime.thread.start_empty_turn().await?;
            runtime.backend.wait_for_type("turn/completed").await?;
        }
        "inject_busy" => {
            send_turn(
                runtime,
                "busy-turn",
                "Think briefly, then reply exactly CODEX_SPEC_INITIAL.",
            )
            .await?;
            runtime.backend.wait_for_type("turn/started").await?;
            runtime
                .thread
                .inject_items(vec![injected_item(
                    "Reply with exactly CODEX_SPEC_INJECT_BUSY and nothing else.",
                )])
                .await?;
            runtime.backend.wait_for_type("turn/completed").await?;
        }
        "two_assistant_messages" => {
            send_turn(
                runtime,
                "two-messages-turn",
                "Send two separate assistant messages in this turn: first exactly CODEX_SPEC_FIRST in commentary, then exactly CODEX_SPEC_SECOND as the final answer.",
            )
            .await?;
            runtime.backend.wait_for_type("turn/completed").await?;
        }
        other => bail!("no Codex backend derivation driver for {other}"),
    }
    Ok(())
}

fn encode_rows(rows: &[Value]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

async fn derive(spec: &str, recording_dir: &Path) -> Result<Vec<u8>> {
    let mut runtime = open_runtime(spec, recording_dir).await?;
    drive(&mut runtime, spec).await?;
    tokio::time::timeout(Duration::from_secs(5), &mut runtime.replay_driver)
        .await
        .with_context(|| format!("strict replay driver did not exhaust for {spec}"))??;
    runtime
        .controller
        .finish()
        .with_context(|| format!("strict replay accounting failed for {spec}"))?;
    let rows = runtime.backend.finish().await?;
    runtime.codex.close().await;
    encode_rows(&rows)
}

#[tokio::test]
async fn codex_recordings_derive_backend_rows_byte_for_byte() -> Result<()> {
    let update = std::env::var_os(UPDATE_FLAG).is_some();
    let output_root = fixtures_root();
    let expected_names = codex::specs::registry()
        .iter()
        .map(|entry| format!("{}.rows.jsonl", entry.name))
        .collect::<BTreeSet<_>>();
    for entry in codex::specs::registry() {
        let recording_dir = codex::specs::fixtures_root().join(entry.recording);
        let actual = derive(entry.name, &recording_dir)
            .await
            .with_context(|| format!("derive {}", entry.name))?;
        let expected_path = output_root.join(format!("{}.rows.jsonl", entry.name));
        if update {
            std::fs::write(&expected_path, &actual)
                .with_context(|| format!("write {}", expected_path.display()))?;
        } else {
            let expected = std::fs::read(&expected_path).with_context(|| {
                format!(
                    "read {} (set {UPDATE_FLAG}=1 to generate)",
                    expected_path.display()
                )
            })?;
            assert_eq!(actual, expected, "derived rows changed for {}", entry.name);
        }
    }
    let actual_names = std::fs::read_dir(&output_root)?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            name.ends_with(".rows.jsonl").then_some(name)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_names, expected_names,
        "Codex backend row fixtures must map one-to-one to the crate registry"
    );
    Ok(())
}

fn claude_transport_order(recording: &Recording) -> Vec<String> {
    let mut seen = BTreeSet::new();
    recording
        .io
        .iter()
        .filter_map(|event| event.transport_id.clone())
        .filter(|transport| seen.insert(transport.clone()))
        .collect()
}

fn claude_prompts(recording: &Recording) -> Result<Vec<String>> {
    recording
        .io
        .iter()
        .filter(|event| event.direction == IoDirection::Write)
        .filter_map(|event| {
            let value = match serde_json::from_str::<Value>(&event.line) {
                Ok(value) => value,
                Err(error) => return Some(Err(error.into())),
            };
            if value.get("type").and_then(Value::as_str) != Some("user") {
                return None;
            }
            Some(
                value
                    .pointer("/message/content")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .context("recorded Claude user message has no text content"),
            )
        })
        .collect()
}

async fn open_claude_harness(
    transport: ReplayTransport,
    session_id: &str,
    resumed: bool,
) -> Result<ClaudeSdkBackendHarness> {
    let mut options = QueryOptions::default();
    if resumed {
        options.resume = Some(session_id.to_string());
    } else {
        options.session_id = Some(session_id.to_string());
    }
    let session = claude::sdk::from_io(transport.reader, transport.writer, options)
        .await
        .context("initialize Claude SDK over strict replay")?;
    ClaudeSdkBackendHarness::with_session(session).await
}

async fn send_claude_prompt(
    harness: &ClaudeSdkBackendHarness,
    input_id: &str,
    prompt: &str,
) -> Result<()> {
    harness
        .send(
            input_id.as_bytes(),
            ClaudeSdkV1Input::Prompt {
                text: prompt.to_string(),
            },
        )
        .await
}

async fn derive_claude_sdk(recording_name: &str, recording_dir: &Path) -> Result<Vec<u8>> {
    let recording = load_recording(recording_dir)
        .with_context(|| format!("load Claude SDK recording {}", recording_dir.display()))?;
    let transport_order = claude_transport_order(&recording);
    if transport_order.len() != recording.manifest.session_ids.len() {
        bail!(
            "Claude recording {recording_name} has {} transports but {} session ids",
            transport_order.len(),
            recording.manifest.session_ids.len()
        );
    }
    let prompts = claude_prompts(&recording)?;
    let expected_prompts = if matches!(recording_name, "resumed" | "multi_turn") {
        2
    } else {
        1
    };
    if prompts.len() != expected_prompts {
        bail!(
            "Claude recording {recording_name} has {} prompts, expected {expected_prompts}",
            prompts.len()
        );
    }

    let StrictReplay {
        mut transports,
        controller,
        clock,
    } = strict_replay(&recording, ReplayOptions::default());
    drop(clock);
    let mut ordered_transports = transport_order
        .into_iter()
        .map(|transport_id| {
            transports
                .remove(&transport_id)
                .with_context(|| format!("recording omits transport {transport_id}"))
        })
        .collect::<Result<VecDeque<_>>>()?;
    if !transports.is_empty() {
        bail!("Claude recording {recording_name} has undeclared transports");
    }
    let driver_controller = controller.clone();
    let mut replay_driver = tokio::spawn(async move {
        while let ReplayAdvance::Advanced { .. } | ReplayAdvance::BlockedOnWrite =
            driver_controller.advance_one().await
        {
            tokio::task::yield_now().await;
        }
    });

    let first_transport = ordered_transports
        .pop_front()
        .context("Claude recording has no first transport")?;
    let mut first =
        open_claude_harness(first_transport, &recording.manifest.session_ids[0], false).await?;
    send_claude_prompt(&first, "prompt-1", &prompts[0]).await?;

    let mut harnesses = Vec::new();
    match recording_name {
        "text_turn" => {
            first.wait_for_type("result").await?;
            harnesses.push(first);
        }
        "permission_callback" => {
            let request = first
                .wait_for_type("amux.claude_sdk.permission_required")
                .await?;
            let request_id = request["request_id"]
                .as_str()
                .context("permission row has no request id")?
                .to_string();
            first
                .send(
                    b"permission-allow",
                    ClaudeSdkV1Input::PermissionDecision {
                        request_id,
                        decision: PermissionResult::Allow {
                            updated_input: Some(request["input"].clone()),
                            updated_permissions: None,
                            tool_use_id: None,
                        },
                    },
                )
                .await?;
            first.wait_for_type("result").await?;
            harnesses.push(first);
        }
        "interrupted" => {
            first.wait_for_type("assistant").await?;
            first
                .send(b"interrupt", ClaudeSdkV1Input::Interrupt)
                .await?;
            first.wait_for_type("result").await?;
            harnesses.push(first);
        }
        "multi_turn" => {
            first.wait_for_type("result").await?;
            send_claude_prompt(&first, "prompt-2", &prompts[1]).await?;
            first.wait_for_type("result").await?;
            harnesses.push(first);
        }
        "resumed" => {
            first.wait_for_type("result").await?;
            let second_transport = ordered_transports
                .pop_front()
                .context("resumed Claude recording has no second transport")?;
            let mut second =
                open_claude_harness(second_transport, &recording.manifest.session_ids[1], true)
                    .await?;
            send_claude_prompt(&second, "prompt-2", &prompts[1]).await?;
            second.wait_for_type("result").await?;
            harnesses.push(first);
            harnesses.push(second);
        }
        other => bail!("no Claude SDK backend derivation driver for {other}"),
    }
    if !ordered_transports.is_empty() {
        bail!("Claude recording {recording_name} left transports unopened");
    }

    tokio::time::timeout(Duration::from_secs(5), &mut replay_driver)
        .await
        .with_context(|| {
            format!("strict Claude replay driver did not exhaust for {recording_name}")
        })??;
    controller
        .finish()
        .with_context(|| format!("strict Claude replay accounting failed for {recording_name}"))?;
    drop(controller);

    let mut rows = Vec::new();
    for harness in harnesses {
        rows.extend(harness.finish().await?);
    }
    encode_rows(&rows)
}

#[tokio::test]
async fn claude_sdk_recordings_derive_backend_rows_byte_for_byte() -> Result<()> {
    let update = std::env::var_os(UPDATE_FLAG).is_some();
    let output_root = claude_sdk_fixtures_root();
    if update {
        std::fs::create_dir_all(&output_root)
            .with_context(|| format!("create {}", output_root.display()))?;
    }
    let expected_names = CLAUDE_SDK_DERIVATIONS
        .iter()
        .map(|(recording, _)| format!("{recording}.rows.jsonl"))
        .collect::<BTreeSet<_>>();
    for (recording, spec) in CLAUDE_SDK_DERIVATIONS {
        let recording_dir = claude_sdk_recordings_root().join(recording);
        let loaded = load_recording(&recording_dir)
            .with_context(|| format!("load Claude SDK recording {recording}"))?;
        if loaded.manifest.spec != *spec {
            bail!(
                "Claude recording {recording} claims {}, expected {spec}",
                loaded.manifest.spec
            );
        }
        let actual = derive_claude_sdk(recording, &recording_dir)
            .await
            .with_context(|| format!("derive Claude SDK {recording}"))?;
        let expected_path = output_root.join(format!("{recording}.rows.jsonl"));
        if update {
            std::fs::write(&expected_path, &actual)
                .with_context(|| format!("write {}", expected_path.display()))?;
        } else {
            let expected = std::fs::read(&expected_path).with_context(|| {
                format!(
                    "read {} (set {UPDATE_FLAG}=1 to generate)",
                    expected_path.display()
                )
            })?;
            assert_eq!(
                actual, expected,
                "derived Claude SDK rows changed for {recording}"
            );
        }
    }
    let actual_names = std::fs::read_dir(&output_root)?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            name.ends_with(".rows.jsonl").then_some(name)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_names, expected_names,
        "Claude SDK row fixtures must map one-to-one to the derivation registry"
    );
    Ok(())
}

//! Opt-in real-Codex capture rig.
//!
//! Run one scenario per process so the shared SDK connection and output files
//! describe exactly one capture:
//!
//! ```text
//! AMUX_CODEX_CAPTURE_DIR=target/codex-capture/pong \
//!   timeout 600 cargo test -p amux --test codex_capture -- pong
//! ```
//!
//! Scenarios: `pong`, `command_allow`, `command_deny`, `file_change`,
//! `interrupt`, `resume_history`, and `reconnect`. The rig records SDK wire traffic to
//! `io.jsonl`, backend structured rows to `rows.jsonl`, and provenance to
//! `meta.json`. Prompts and scratch paths are synthetic.

#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use amux::codex_io::{
    CODEX_SDK_V1, CodexSdkV1Input, decode_codex_sdk_v1_output, encode_codex_sdk_v1_input,
};
#[cfg(unix)]
use amux::{
    AgentIdentifier, AgentType, Config, CreateAgentRequest, SendInputRequest, Server,
    SubscribeSessionEvent, SubscribeSessionRequest,
};
#[cfg(unix)]
use anyhow::{Context, Result, anyhow, bail};
#[cfg(unix)]
use bytes::Bytes;
#[cfg(unix)]
use serde_json::{Value, json};
#[cfg(unix)]
use uuid::Uuid;

#[cfg(not(unix))]
fn main() {
    println!("codex capture is only available on Unix");
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<()> {
    let Some(scenario) = std::env::args().nth(1) else {
        println!("codex capture: no scenario selected; skipping");
        return Ok(());
    };
    let out = std::env::var_os("AMUX_CODEX_CAPTURE_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("AMUX_CODEX_CAPTURE_DIR is required"))?;
    std::fs::create_dir_all(&out)?;
    for name in ["io.jsonl", "rows.jsonl", "meta.json"] {
        let path = out.join(name);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }

    let scratch = tempfile::tempdir()?;
    let config = Config {
        state_path: scratch.path().join("state.yaml"),
        socket_path: scratch.path().join("amux.sock"),
        enable_cloud_mode: Some(false),
        prevent_idle_sleep: Some(false),
        ..Config::default()
    };
    let client = Server::builder().config(config).embedded().open().await?;
    let model = std::env::var("AMUX_CODEX_CAPTURE_MODEL").ok();
    let name = format!("codex-cap-{scenario}");
    let agent_id = create_agent(&client, &name, scratch.path(), model.clone(), None).await?;
    let mut stream = client
        .subscribe_session(SubscribeSessionRequest {
            agent: AgentIdentifier::Id(agent_id),
            io_protocol: CODEX_SDK_V1.into(),
            args: None,
        })
        .await?;
    wait_for_type(&mut stream, "amux.codex_ready").await?;

    let prompt = match scenario.as_str() {
        "pong" | "resume_history" | "reconnect" => "Reply exactly PONG.",
        "command_allow" | "command_deny" => {
            "Run the command `touch codex-capture-command.txt`, then say DONE."
        }
        "file_change" => {
            "Create codex-capture-file.txt containing exactly CAPTURE using the file editing tool."
        }
        "interrupt" => "Count upward slowly from 1 to 1000, one number per line.",
        other => bail!("unknown codex capture scenario `{other}`"),
    };
    send(
        &client,
        agent_id,
        CodexSdkV1Input::UserTurn {
            input: serde_json::to_vec(&json!([{"type": "text", "text": prompt}]))?,
        },
    )
    .await?;

    match scenario.as_str() {
        "reconnect" => {
            wait_for_type(&mut stream, "amux.codex_gap").await?;
            wait_for_type(&mut stream, "amux.codex_ready").await?;
            send(
                &client,
                agent_id,
                CodexSdkV1Input::UserTurn {
                    input: serde_json::to_vec(&json!([{
                        "type": "text",
                        "text": "Reply exactly RECONNECTED."
                    }]))?,
                },
            )
            .await?;
            wait_for_type(&mut stream, "turn/completed").await?;
        }
        "command_allow" => {
            let ask = wait_for_type(&mut stream, "amux.codex_approval_required").await?;
            answer(&client, agent_id, &ask, "accept").await?;
            wait_for_type(&mut stream, "turn/completed").await?;
        }
        "file_change" => loop {
            let row = wait_for_either(
                &mut stream,
                "amux.codex_approval_required",
                "turn/completed",
            )
            .await?;
            if row.get("type").and_then(Value::as_str) == Some("turn/completed") {
                break;
            }
            answer(&client, agent_id, &row, "accept").await?;
        },
        "command_deny" => {
            let ask = wait_for_type(&mut stream, "amux.codex_approval_required").await?;
            answer(&client, agent_id, &ask, "decline").await?;
            wait_for_type(&mut stream, "turn/completed").await?;
        }
        "interrupt" => {
            wait_for_type(&mut stream, "turn/started").await?;
            send(
                &client,
                agent_id,
                CodexSdkV1Input::Interrupt {
                    turn_id: String::new(),
                },
            )
            .await?;
            wait_for_type(&mut stream, "turn/completed").await?;
        }
        "resume_history" => {
            let completed = wait_for_type(&mut stream, "turn/completed").await?;
            let thread_id = completed
                .get("threadId")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("turn/completed missing threadId"))?
                .to_string();
            drop(stream);
            client.delete_agent(agent_id).await?;
            let resumed_name = format!("{name}-resumed");
            let resumed = create_agent(
                &client,
                &resumed_name,
                scratch.path(),
                model.clone(),
                Some(thread_id),
            )
            .await?;
            let mut resumed_stream = client
                .subscribe_session(SubscribeSessionRequest {
                    agent: AgentIdentifier::Id(resumed),
                    io_protocol: CODEX_SDK_V1.into(),
                    args: None,
                })
                .await?;
            wait_for_type(&mut resumed_stream, "amux.codex_ready").await?;
            send(
                &client,
                resumed,
                CodexSdkV1Input::UserTurn {
                    input: serde_json::to_vec(&json!([{
                        "type": "text",
                        "text": "Reply exactly HISTORY_OK if the previous user asked for PONG."
                    }]))?,
                },
            )
            .await?;
            wait_for_type(&mut resumed_stream, "turn/completed").await?;
            client.delete_agent(resumed).await?;
        }
        _ => {
            wait_for_type(&mut stream, "turn/completed").await?;
        }
    }

    if scenario != "resume_history" {
        client.delete_agent(agent_id).await?;
    }
    let codex_version = std::process::Command::new("codex")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    std::fs::write(
        out.join("meta.json"),
        serde_json::to_vec_pretty(&json!({
            "scenario": scenario,
            "captured_at": chrono::Utc::now().to_rfc3339(),
            "codex_version": codex_version,
            "model": model.unwrap_or_else(|| "default".into()),
            "synthetic_prompts": true,
            "redacted": false,
        }))?,
    )?;
    client.shutdown().await?;
    println!("codex capture written to {}", out.display());
    Ok(())
}

#[cfg(unix)]
async fn create_agent(
    client: &amux::Client,
    name: &str,
    cwd: &Path,
    model: Option<String>,
    resume_thread_id: Option<String>,
) -> Result<Uuid> {
    let agent_id = Uuid::new_v4();
    client
        .create_agent(CreateAgentRequest {
            agent_id,
            host_id: None,
            name: Some(name.into()),
            agent_type: AgentType::Codex {
                model,
                approval_policy: Some("on-request".into()),
                sandbox_policy: Some("read-only".into()),
                resume_thread_id,
            },
            working_dir: cwd.to_path_buf(),
            terminal_size: None,
            args: Vec::new(),
        })
        .await
        .context("create Codex agent")?;
    Ok(agent_id)
}

#[cfg(unix)]
async fn send(client: &amux::Client, agent_id: Uuid, input: CodexSdkV1Input) -> Result<()> {
    client
        .send_input(SendInputRequest {
            agent: AgentIdentifier::Id(agent_id),
            io_protocol: CODEX_SDK_V1.into(),
            payload: Bytes::from(encode_codex_sdk_v1_input(input)),
        })
        .await?;
    Ok(())
}

#[cfg(unix)]
async fn answer(client: &amux::Client, agent_id: Uuid, ask: &Value, decision: &str) -> Result<()> {
    send(
        client,
        agent_id,
        CodexSdkV1Input::ApprovalDecision {
            request_id: serde_json::to_vec(&ask["request_id"])?,
            decision: decision.into(),
        },
    )
    .await
}

#[cfg(unix)]
async fn wait_for_type(stream: &mut amux::SessionStream, expected: &str) -> Result<Value> {
    wait_for_either(stream, expected, expected).await
}

#[cfg(unix)]
async fn wait_for_either(
    stream: &mut amux::SessionStream,
    first: &str,
    second: &str,
) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            match stream.recv().await? {
                SubscribeSessionEvent::Output { payload } => {
                    let output = decode_codex_sdk_v1_output(&payload)?;
                    let row: Value = serde_json::from_slice(&output.payload)?;
                    let row_type = row.get("type").and_then(Value::as_str);
                    if row_type == Some(first) || row_type == Some(second) {
                        return Ok(row);
                    }
                }
                SubscribeSessionEvent::Closed { reason } => {
                    return Err(anyhow!("Codex stream closed: {reason:?}"));
                }
                _ => {}
            }
        }
    })
    .await
    .context("timed out waiting for Codex row")?
}

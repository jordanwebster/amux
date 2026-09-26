//! fake-claude-sdk as a host sees it: every frame it composes has a recorded
//! shape, and it echoes, queues, folds and cancels the way Claude does.

use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use provider_fakes::shape::{Classifier, Corpus};
use provider_fakes::{Kind, Script};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

const DEADLINE: Duration = Duration::from_secs(20);

fn corpus() -> &'static Corpus {
    static CORPUS: OnceLock<Corpus> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../claude-specs/fixtures/sdk");
        Corpus::load(Kind::ClaudeSdk, &[&root]).unwrap()
    })
}

struct Host {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Lines<BufReader<ChildStdout>>,
    classifier: Classifier,
    frames: Vec<Value>,
    /// Holds the script until the fake exits.
    _dir: tempfile::TempDir,
}

impl Host {
    async fn start(script: Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let script: Script = serde_json::from_value(script).unwrap();
        let path = dir.path().join("script.json");
        std::fs::write(&path, serde_json::to_vec(&script).unwrap()).unwrap();
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_fake-claude-sdk"))
            .args([
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--session-id",
                "5e55e55e-0000-4000-8000-000000000001",
                "--replay-user-messages",
                "--include-partial-messages",
                "--messaging-socket-path",
                "/tmp/fake-claude-sdk.sock",
            ])
            .env(provider_fakes::SCRIPT_ENV, &path)
            .current_dir(dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut host = Self {
            child,
            stdin,
            stdout,
            classifier: Classifier::default(),
            frames: Vec::new(),
            _dir: dir,
        };
        host.send(json!({"type":"control_request","request_id":"req_0","request":{"subtype":"initialize"}}))
            .await;
        host.until(|frame| frame["type"] == "control_response")
            .await;
        host
    }

    async fn send(&mut self, frame: Value) {
        self.classifier.host(Kind::ClaudeSdk, &frame);
        let mut line = serde_json::to_vec(&frame).unwrap();
        line.push(b'\n');
        self.stdin.as_mut().unwrap().write_all(&line).await.unwrap();
    }

    async fn prompt(&mut self, uuid: &str, text: &str, priority: Option<&str>) {
        let mut frame = json!({
            "type": "user",
            "uuid": uuid,
            "session_id": "",
            "parent_tool_use_id": null,
            "message": { "role": "user", "content": text },
        });
        if let Some(priority) = priority {
            frame["priority"] = json!(priority);
        }
        self.send(frame).await;
    }

    async fn next(&mut self) -> Value {
        let line = tokio::time::timeout(DEADLINE, self.stdout.next_line())
            .await
            .expect("the fake wrote nothing in time")
            .unwrap()
            .expect("the fake closed stdout");
        let frame: Value = serde_json::from_str(&line).unwrap();
        let group = self.classifier.provider(Kind::ClaudeSdk, &frame);
        if let Err(drift) = corpus().check(Kind::ClaudeSdk, &group, &frame) {
            // A cancel answer is shown by the probe of 2.1.282, not by any
            // recording yet.
            if group != "control_response/cancel_async_message/success" {
                panic!("{drift}");
            }
        }
        self.frames.push(frame.clone());
        frame
    }

    /// Read until a frame matches, returning it.
    async fn until(&mut self, matches: impl Fn(&Value) -> bool) -> Value {
        loop {
            let frame = self.next().await;
            if matches(&frame) {
                return frame;
            }
        }
    }

    /// A compact trace of the frames read so far, for order assertions.
    fn trace(&self) -> Vec<String> {
        self.frames.iter().map(describe).collect()
    }

    async fn close(mut self) -> i32 {
        drop(self.stdin.take());
        while let Ok(Ok(Some(line))) = tokio::time::timeout(DEADLINE, self.stdout.next_line()).await
        {
            let frame: Value = serde_json::from_str(&line).unwrap();
            self.frames.push(frame);
        }
        tokio::time::timeout(DEADLINE, self.child.wait())
            .await
            .unwrap()
            .unwrap()
            .code()
            .unwrap_or(-1)
    }
}

fn describe(frame: &Value) -> String {
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    match text(&frame["type"]).as_str() {
        "command_lifecycle" => format!(
            "lifecycle {} {}",
            &text(&frame["command_uuid"])[..8],
            text(&frame["state"])
        ),
        "user" if frame.get("isReplay").is_some() => {
            format!("replay {}", &text(&frame["uuid"])[..8])
        }
        "user" => format!("user {}", text(&frame["message"]["content"][0]["type"])),
        "assistant" => format!(
            "assistant {}",
            text(&frame["message"]["content"][0]["type"])
        ),
        "result" => format!("result {}", text(&frame["subtype"])),
        "system" => format!("system {}", text(&frame["subtype"])),
        "stream_event" => format!("stream {}", text(&frame["event"]["type"])),
        "control_request" => format!("request {}", text(&frame["request"]["subtype"])),
        "control_response" => "response".into(),
        other => other.to_owned(),
    }
}

const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const C: &str = "cccccccc-0000-4000-8000-000000000003";

#[tokio::test]
async fn an_idle_prompt_is_echoed_by_its_uuid_and_closed_by_its_lifecycle() {
    let mut host = Host::start(json!({"steps": [
        {"text": {"chunks": ["Hel", "lo"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Say hello", None).await;
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["result"], "Hello");
    host.until(|frame| frame["type"] == "command_lifecycle" && frame["state"] == "completed")
        .await;
    let replay = host
        .frames
        .iter()
        .find(|frame| frame["isReplay"] == true)
        .unwrap();
    assert_eq!(replay["uuid"], A);
    assert_eq!(replay["message"]["content"], "Say hello");
    assert_eq!(
        host.trace(),
        [
            "response",
            "lifecycle aaaaaaaa queued",
            "lifecycle aaaaaaaa started",
            "system init",
            "replay aaaaaaaa",
            "stream message_start",
            "stream content_block_start",
            "stream content_block_delta",
            "stream content_block_delta",
            "assistant text",
            "stream content_block_stop",
            "result success",
            "lifecycle aaaaaaaa completed",
        ]
    );
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn a_default_message_mid_turn_folds_at_the_next_tool_result_and_later_waits() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("gate");
    let mut host = Host::start(json!({"steps": [
        {"wait_for": {"path": gate}},
        {"tool": {"class": "consequential", "outcome": {"output": "ran"}}},
        {"text": {"chunks": ["done"]}},
        "turn_end",
        {"text": {"chunks": ["later"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Run it", None).await;
    host.until(|frame| frame["isReplay"] == true).await;
    host.prompt(B, "Also this", None).await;
    host.prompt(C, "Afterwards", Some("later")).await;
    host.until(|frame| frame["state"] == "queued" && frame["command_uuid"] == C)
        .await;
    std::fs::write(&gate, "").unwrap();
    host.until(|frame| frame["type"] == "result").await;
    host.until(|frame| frame["state"] == "completed" && frame["command_uuid"] == B)
        .await;
    host.until(|frame| frame["type"] == "result").await;
    host.until(|frame| frame["state"] == "completed" && frame["command_uuid"] == C)
        .await;
    let trace = host.trace();
    let after_init = trace
        .iter()
        .position(|line| line == "replay aaaaaaaa")
        .unwrap();
    assert_eq!(
        trace[after_init..],
        [
            "replay aaaaaaaa",
            "lifecycle bbbbbbbb queued",
            "lifecycle cccccccc queued",
            "assistant tool_use",
            "user tool_result",
            "replay bbbbbbbb",
            "lifecycle bbbbbbbb started",
            "stream message_start",
            "stream content_block_start",
            "stream content_block_delta",
            "assistant text",
            "stream content_block_stop",
            "result success",
            "lifecycle aaaaaaaa completed",
            "lifecycle bbbbbbbb completed",
            "lifecycle cccccccc started",
            "system init",
            "replay cccccccc",
            "stream message_start",
            "stream content_block_start",
            "stream content_block_delta",
            "assistant text",
            "stream content_block_stop",
            "result success",
            "lifecycle cccccccc completed",
        ]
    );
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn cancel_withdraws_only_a_message_still_queued() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("gate");
    let mut host = Host::start(json!({"steps": [
        {"wait_for": {"path": gate}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Wait", None).await;
    host.until(|frame| frame["isReplay"] == true).await;
    host.prompt(B, "Queued", None).await;
    host.until(|frame| frame["state"] == "queued" && frame["command_uuid"] == B)
        .await;
    for (id, target) in [("req_1", B), ("req_2", A)] {
        host.send(json!({"type":"control_request","request_id":id,
            "request":{"subtype":"cancel_async_message","message_uuid":target}}))
            .await;
    }
    let first = host
        .until(|frame| frame["type"] == "control_response")
        .await;
    assert_eq!(first["response"]["response"]["cancelled"], true);
    let second = host
        .until(|frame| frame["type"] == "control_response")
        .await;
    assert_eq!(second["response"]["response"]["cancelled"], false);
    assert!(
        host.trace()
            .contains(&"lifecycle bbbbbbbb cancelled".to_owned())
    );
    std::fs::write(&gate, "").unwrap();
    host.until(|frame| frame["type"] == "result").await;
    host.until(|frame| frame["state"] == "completed").await;
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn interrupt_and_now_cut_the_running_turn_short() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("never");
    let mut host = Host::start(json!({"steps": [
        {"wait_for": {"path": gate}},
        {"text": {"chunks": ["never said"]}},
        "turn_end",
        {"wait_for": {"path": gate}},
        "turn_end",
        {"text": {"chunks": ["preempted"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Wait", None).await;
    host.until(|frame| frame["isReplay"] == true).await;
    host.send(
        json!({"type":"control_request","request_id":"req_1","request":{"subtype":"interrupt"}}),
    )
    .await;
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["subtype"], "error_during_execution");
    host.prompt(B, "Wait again", None).await;
    host.until(|frame| frame["isReplay"] == true && frame["uuid"] == B)
        .await;
    host.prompt(C, "Now instead", Some("now")).await;
    let cut = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(cut["subtype"], "error_during_execution");
    let next = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(next["result"], "preempted");
    assert!(!host.trace().iter().any(|line| line.contains("never")));
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn every_ask_blocks_until_answered_and_resolves_the_call() {
    let mut host = Host::start(json!({"steps": [
        {"thinking": {"text": "Consider"}},
        {"tool": {"class": "exploration", "outcome": {"output": "read"}}},
        {"tool": {"class": "consequential", "input": {"command": "false"},
                  "outcome": {"output": "Exit code 1", "error": true}}},
        {"ask": {"permission": {"class": "consequential", "input": {"command": "rm x"},
                                "outcome": {"output": "removed"}}}},
        {"ask": {"permission": {"class": "consequential", "outcome": {"output": "denied?"}}}},
        {"ask": {"question": {"questions": [{"question": "Which?", "header": "Pick",
                                             "options": ["Red", "Blue"]}]}}},
        {"ask": {"plan": {"markdown": "1. Do it"}}},
        {"ask": {"form": {"server": "amux", "message": "Confirm",
                          "schema": {"type": "object", "properties": {"ok": {"type": "string"}}}}}},
        {"text": {"chunks": ["all done"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Ask everything", None).await;
    let answers = [
        json!({"behavior": "allow", "updatedInput": {"command": "rm x"}}),
        json!({"behavior": "deny", "message": "No thanks"}),
        json!({"behavior": "allow", "updatedInput": {"questions": [], "answers": {"Which?": "Blue"}}}),
        json!({"behavior": "allow", "updatedInput": {"plan": "1. Do it"}}),
        json!({"action": "accept", "content": {"ok": "yes"}}),
    ];
    for answer in answers {
        let request = host.until(|frame| frame["type"] == "control_request").await;
        host.send(json!({"type":"control_response","response":{
            "subtype":"success","request_id":request["request_id"],"response":answer}}))
            .await;
    }
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["result"], "all done");
    let results: Vec<(bool, String)> = host
        .frames
        .iter()
        .filter(|frame| frame["message"]["content"][0]["type"] == "tool_result")
        .map(|frame| {
            let block = &frame["message"]["content"][0];
            (block["is_error"] == true, block["content"].to_string())
        })
        .collect();
    assert_eq!(results.len(), 7, "{results:?}");
    assert!(!results[0].0 && results[0].1.contains("read"));
    assert!(results[1].0);
    assert!(!results[2].0 && results[2].1.contains("removed"));
    assert!(results[3].0 && results[3].1.contains("No thanks"));
    assert!(results[4].1.contains("\\\"Which?\\\"=\\\"Blue\\\""));
    assert!(results[5].1.contains("approved your plan"));
    assert!(results[6].1.contains("elicitation accept"));
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn exit_ends_the_process_with_its_code_mid_turn() {
    let mut host = Host::start(json!({"steps": [
        {"text": {"chunks": ["bye"]}},
        {"exit": {"code": 3}},
    ]}))
    .await;
    host.prompt(A, "Leave", None).await;
    host.until(|frame| frame["type"] == "assistant").await;
    assert_eq!(host.close().await, 3);
}

#[tokio::test]
async fn a_script_asking_for_an_ask_headless_claude_cannot_raise_fails_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.json");
    std::fs::write(
        &path,
        json!({"steps": [{"ask": {"link": {"server": "s", "message": "m", "url": "https://x"}}}]})
            .to_string(),
    )
    .unwrap();
    let status = tokio::process::Command::new(env!("CARGO_BIN_EXE_fake-claude-sdk"))
        .env(provider_fakes::SCRIPT_ENV, &path)
        .stdin(Stdio::null())
        .status()
        .await
        .unwrap();
    assert_eq!(status.code(), Some(provider_fakes::DRIFT_EXIT));
}

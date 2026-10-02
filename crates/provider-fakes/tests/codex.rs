//! fake-codex as a host sees it: every frame it composes has a recorded
//! shape, injected items are acknowledged and drained, steering and
//! interrupting act on the running turn, and every ask blocks the turn.

mod support;

use provider_fakes::Kind;
use serde_json::{Value, json};
use support::Host as Line;

/// Steering is not in any recording yet; the host's interpreter reads only
/// whether the request succeeded. The offered lists are pinned from a
/// read-only probe of codex-cli 0.157.0 in the interpreter's fixtures.
const EXEMPT: &[&str] = &[
    "result/turn/steer",
    "error/turn/steer",
    "result/model/list",
    "result/skills/list",
];

struct Host {
    line: Line,
    next_id: u64,
    thread: String,
}

impl Host {
    async fn start(script: Value) -> Self {
        let line = Line::spawn(
            Kind::Codex,
            env!("CARGO_BIN_EXE_fake-codex"),
            &["app-server", "--listen", "stdio://"],
            script,
            EXEMPT,
        )
        .await;
        let mut host = Host {
            line,
            next_id: 0,
            thread: String::new(),
        };
        host.call(
            "initialize",
            json!({"capabilities": {"experimentalApi": true},
                   "clientInfo": {"name": "amux", "title": null, "version": "0.1.0"}}),
        )
        .await;
        host.line.send(json!({"method": "initialized"})).await;
        let started = host
            .call(
                "thread/start",
                json!({"approvalPolicy": "on-request", "cwd": "/tmp", "model": "gpt-fake",
                       "sandbox": "workspace-write"}),
            )
            .await;
        host.thread = started["result"]["thread"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        host.line
            .until(|frame| frame["method"] == "thread/started")
            .await;
        host
    }

    /// Send a request and return its response.
    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.request(method, params).await;
        self.response(&id).await
    }

    async fn request(&mut self, method: &str, params: Value) -> String {
        self.next_id += 1;
        let id = format!("amux-{}", self.next_id);
        self.line
            .send(json!({"id": id, "method": method, "params": params}))
            .await;
        id
    }

    async fn response(&mut self, id: &str) -> Value {
        self.line
            .until(|frame| frame["id"] == id && frame.get("method").is_none())
            .await
    }

    async fn turn(&mut self, text: &str) -> String {
        let input = if text.is_empty() {
            json!([])
        } else {
            json!([{"type": "text", "text": text}])
        };
        let thread = self.thread.clone();
        let started = self
            .call("turn/start", json!({"threadId": thread, "input": input}))
            .await;
        started["result"]["turn"]["id"].as_str().unwrap().to_owned()
    }

    async fn completed(&mut self) -> Value {
        self.line
            .until(|frame| frame["method"] == "turn/completed")
            .await
    }

    async fn inject(&mut self, text: &str) -> Value {
        let thread = self.thread.clone();
        self.call(
            "thread/inject_items",
            json!({"threadId": thread, "items": [{"type": "message", "role": "user",
                   "content": [{"type": "input_text", "text": text}]}]}),
        )
        .await
    }
}

fn items(frames: &[Value], kind: &str) -> Vec<Value> {
    frames
        .iter()
        .filter(|frame| frame["method"] == "item/completed")
        .map(|frame| frame["params"]["item"].clone())
        .filter(|item| item["type"] == kind)
        .collect()
}

#[tokio::test]
async fn a_turn_streams_its_message_and_completes_with_it() {
    let mut host = Host::start(json!({"steps": [
        {"thinking": {"text": "Consider"}},
        {"tool": {"class": "exploration", "outcome": {"output": "read"}}},
        {"tool": {"class": "consequential", "input": {"command": "false"},
                  "outcome": {"output": "failed", "error": true}}},
        {"tool": {"name": "apply_patch", "class": "consequential"}},
        {"text": {"chunks": ["Do", "ne"]}},
        "turn_end",
    ]}))
    .await;
    host.turn("Go").await;
    let done = host.completed().await;
    assert_eq!(done["params"]["turn"]["status"], "completed");
    assert_eq!(done["params"]["turn"]["items"][0]["text"], "Done");
    let commands = items(&host.line.frames, "commandExecution");
    assert_eq!(commands[0]["commandActions"][0]["type"], "read");
    assert_eq!(commands[1]["exitCode"], 1);
    assert_eq!(commands[1]["status"], "failed");
    assert_eq!(
        items(&host.line.frames, "fileChange")[0]["status"],
        "completed"
    );
    assert_eq!(items(&host.line.frames, "userMessage").len(), 1);
    assert_eq!(host.line.close().await, 0);
}

#[tokio::test]
async fn an_item_injected_mid_turn_is_acknowledged_and_drained_by_that_turn() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("gate");
    let mut host = Host::start(json!({"steps": [
        {"wait_for": {"path": gate}},
        {"text": {"chunks": ["first"]}},
        {"text": {"chunks": ["answering the injected item"]}},
        "turn_end",
        {"text": {"chunks": ["idle inject answered"]}},
        "turn_end",
    ]}))
    .await;
    host.turn("Wait").await;
    let ack = host.inject("From another agent").await;
    assert_eq!(ack["result"], json!({}));
    std::fs::write(&gate, "").unwrap();
    let done = host.completed().await;
    assert_eq!(
        done["params"]["turn"]["items"][0]["text"],
        "answering the injected item"
    );
    // Nothing reports the injected item itself.
    assert!(
        !host
            .line
            .frames
            .iter()
            .any(|frame| frame.to_string().contains("From another agent"))
    );
    // Injected into an idle thread, it waits for an empty turn.
    host.inject("While idle").await;
    host.turn("").await;
    let done = host.completed().await;
    assert_eq!(
        done["params"]["turn"]["items"][0]["text"],
        "idle inject answered"
    );
    assert_eq!(items(&host.line.frames, "userMessage").len(), 1);
    assert_eq!(host.line.close().await, 0);
}

#[tokio::test]
async fn steer_adds_to_the_running_turn_and_interrupt_ends_it() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("never");
    let mut host = Host::start(json!({"steps": [
        {"wait_for": {"path": gate}},
        {"text": {"chunks": ["never said"]}},
        "turn_end",
    ]}))
    .await;
    let turn = host.turn("Wait").await;
    let thread = host.thread.clone();
    let steered = host
        .call(
            "turn/steer",
            json!({"threadId": thread, "expectedTurnId": turn,
                   "input": [{"type": "text", "text": "And this", "text_elements": []}]}),
        )
        .await;
    assert_eq!(steered["result"]["turnId"], turn);
    let reflected = host
        .line
        .until(|frame| frame["method"] == "item/completed")
        .await;
    assert_eq!(
        reflected["params"]["item"]["content"][0]["text"],
        "And this"
    );
    let interrupted = host
        .call(
            "turn/interrupt",
            json!({"threadId": thread, "turnId": turn}),
        )
        .await;
    assert_eq!(interrupted["result"], json!({}));
    let done = host.completed().await;
    assert_eq!(done["params"]["turn"]["status"], "interrupted");
    let refused = host
        .call(
            "turn/steer",
            json!({"threadId": thread, "expectedTurnId": turn, "input": []}),
        )
        .await;
    assert!(refused.get("error").is_some());
    assert_eq!(host.line.close().await, 0);
}

#[tokio::test]
async fn every_ask_blocks_the_turn_until_the_host_answers() {
    let mut host = Host::start(json!({"steps": [
        {"ask": {"permission": {"class": "consequential", "input": {"command": "touch x"},
                                "outcome": {"output": "ok"}}}},
        {"ask": {"permission": {"class": "consequential", "input": {"command": "rm x"}}}},
        {"ask": {"permission": {"name": "apply_patch", "class": "consequential"}}},
        {"ask": {"question": {"questions": [{"question": "Which?", "header": "Pick",
                                             "options": ["Red", "Blue"]}]}}},
        {"ask": {"grant": {"reason": "Needs the network", "paths": []}}},
        {"ask": {"form": {"server": "spec", "message": "Confirm",
                          "schema": {"type": "object", "properties": {}}}}},
        {"ask": {"link": {"server": "spec", "message": "Sign in", "url": "https://example.com"}}},
        {"text": {"chunks": ["all asked"]}},
        "turn_end",
    ]}))
    .await;
    host.turn("Ask").await;
    let answers = [
        json!({"decision": "accept"}),
        json!({"decision": "decline"}),
        json!({"decision": "acceptForSession"}),
        json!({"answers": {"q0": {"answers": ["Blue"]}}}),
        json!({"permissions": {"fileSystem": null, "network": {"enabled": true}}, "scope": "turn"}),
        // A server's ask comes after Codex's own ask to allow the call.
        json!({"action": "accept", "content": {}}),
        json!({"action": "accept", "content": {}}),
        json!({"action": "accept", "content": {}}),
        json!({"action": "cancel"}),
    ];
    let mut methods = Vec::new();
    for answer in answers {
        let request = host
            .line
            .until(|frame| frame.get("method").is_some() && frame.get("id").is_some())
            .await;
        methods.push(request["method"].as_str().unwrap().to_owned());
        host.line
            .send(json!({"id": request["id"], "result": answer}))
            .await;
        host.line
            .until(|frame| frame["method"] == "serverRequest/resolved")
            .await;
    }
    assert_eq!(
        methods,
        [
            "item/commandExecution/requestApproval",
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
            "item/tool/requestUserInput",
            "item/permissions/requestApproval",
            "mcpServer/elicitation/request",
            "mcpServer/elicitation/request",
            "mcpServer/elicitation/request",
            "mcpServer/elicitation/request",
        ]
    );
    let done = host.completed().await;
    assert_eq!(done["params"]["turn"]["items"][0]["text"], "all asked");
    let commands = items(&host.line.frames, "commandExecution");
    assert_eq!(commands[0]["status"], "completed");
    assert_eq!(commands[1]["status"], "declined");
    let calls = items(&host.line.frames, "mcpToolCall");
    assert!(
        calls[1]["result"]
            .to_string()
            .contains("elicitation cancel")
    );
    assert_eq!(host.line.close().await, 0);
}

#[tokio::test]
async fn compaction_runs_as_its_own_turn_and_exit_ends_the_process() {
    let mut host = Host::start(json!({"steps": [
        {"text": {"chunks": ["bye"]}},
        {"exit": {"code": 4}},
    ]}))
    .await;
    let thread = host.thread.clone();
    host.call("thread/compact/start", json!({"threadId": thread}))
        .await;
    host.completed().await;
    assert_eq!(items(&host.line.frames, "contextCompaction").len(), 1);
    host.turn("Leave").await;
    host.line
        .until(|frame| frame["method"] == "item/completed")
        .await;
    assert_eq!(
        host.line
            .exited_within(std::time::Duration::from_secs(1))
            .await,
        4
    );
}

#[tokio::test]
async fn model_and_skill_lists_answer_from_the_script() {
    let mut host = Host::start(json!({"steps": []})).await;
    let models = host.call("model/list", json!({})).await;
    assert_eq!(
        models["result"],
        json!({"data": [{
            "id": "gpt-fake", "model": "gpt-fake", "displayName": "gpt-fake",
            "description": "The scripted model", "hidden": false,
            "supportedReasoningEfforts": [
                {"reasoningEffort": "low", "description": ""},
                {"reasoningEffort": "medium", "description": ""},
                {"reasoningEffort": "high", "description": ""},
            ],
            "defaultReasoningEffort": "medium", "isDefault": true,
        }], "nextCursor": null})
    );
    let skills = host.call("skills/list", json!({})).await;
    assert_eq!(skills["result"]["data"][0]["skills"], json!([]));

    let mut host = Host::start(json!({
        "steps": [],
        "models": [{"value": "gpt-a", "display_name": "GPT A", "efforts": ["low"]},
                   {"value": "gpt-b"}],
        "commands": [{"name": "review", "description": "Review the diff"}],
    }))
    .await;
    let models = host.call("model/list", json!({})).await;
    let data = models["result"]["data"].as_array().unwrap();
    assert_eq!(data.len(), 2);
    assert_eq!(data[0]["displayName"], "GPT A");
    assert_eq!(data[1]["supportedReasoningEfforts"], json!([]));
    let skills = host.call("skills/list", json!({})).await;
    let skill = &skills["result"]["data"][0]["skills"][0];
    assert_eq!(skill["name"], "review");
    assert_eq!(skill["description"], "Review the diff");
    assert_eq!(skill["scope"], "user");
}

#[tokio::test]
async fn account_and_session_facts_play_in_recorded_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let added = dir.path().join("docs/new.md");
    let mut host = Host::start(json!({
        "context_tokens": 182000,
        "edit_files": true,
        "chunk_ms": 30,
        "steps": [
            {"usage": {"status": "allowed_warning", "windows": [
                {"name": "five_hour", "used_percent": 86.0, "resets_in_s": 3600},
                {"name": "seven_day", "used_percent": 40.0, "resets_in_s": 86400},
            ]}},
            {"text": {"chunks": ["Wri", "ting"]}},
            {"tool": {"name": "apply_patch", "class": "consequential", "input": [
                {"diff": "fresh\n", "kind": {"type": "add"}, "path": added},
            ]}},
            {"ask": {"question": {"questions": [
                {"question": "The token?", "header": "Token", "options": [], "other": true, "secret": true},
            ]}}},
            "turn_end",
            {"auth_failed": {"message": "unexpected status 401 Unauthorized: Missing bearer or basic authentication in header"}},
        ],
    }))
    .await;
    host.turn("Go").await;
    let limits = host
        .line
        .until(|frame| frame["method"] == "account/rateLimits/updated")
        .await;
    assert_eq!(limits["params"]["rateLimits"]["primary"]["usedPercent"], 86);
    let ask = host
        .line
        .until(|frame| frame["method"] == "item/tool/requestUserInput")
        .await;
    let question = &ask["params"]["questions"][0];
    assert_eq!(question["isSecret"], true);
    assert_eq!(question["isOther"], true);
    assert_eq!(std::fs::read_to_string(&added).unwrap(), "fresh\n");
    host.line
        .send(json!({"id": ask["id"], "result": {"answers": {"q0": {"answers": ["s3cret"]}}}}))
        .await;
    let usage = host
        .line
        .until(|frame| frame["method"] == "thread/tokenUsage/updated")
        .await;
    assert_eq!(usage["params"]["tokenUsage"]["last"]["inputTokens"], 182000);
    host.completed().await;
    host.turn("Again").await;
    let failed = host.completed().await;
    assert_eq!(failed["params"]["turn"]["status"], "failed");
    assert_eq!(host.line.close().await, 0);
}

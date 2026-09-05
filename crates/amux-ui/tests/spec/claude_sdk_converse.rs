//! Recorded conversations drive the client from sending through work to input readiness.
//!
//! Streaming, tool use and interruption come from three independent provider
//! recordings. Keeping their sessions separate avoids inventing a continuous
//! live conversation. Client commands are inserted at the recorded input rows;
//! the daemon rows themselves are replayed unchanged.

use amux_ui::claude_sdk::{
    self, ClaudeSdkCommand, ClaudeSdkInput, FeedEntryKind, Finality, SdkPhase, SendGate,
};
use amux_ui::{Command, Effect, InputPayload, Model, Msg, OpId, update};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::harness::*;

const AGENT: &str = "sdk-conversation";

fn rows(name: &str) -> Vec<Value> {
    let raw = match name {
        "streaming" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-sdk/streamed_turn.rows.jsonl")
        }
        "tools" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-sdk/max_turns.rows.jsonl")
        }
        "interrupt" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-sdk/interrupted.rows.jsonl")
        }
        _ => panic!("unknown conversation"),
    };
    raw.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn sequence(name: &str) -> Vec<Msg> {
    let mut msgs = claude_sdk_base(AGENT);
    for (i, row) in rows(name).into_iter().enumerate() {
        if row["type"] == "user" && row["input_id"].is_string() {
            msgs.push(command(
                OpId(Uuid::parse_str(row["uuid"].as_str().unwrap()).unwrap()),
                Command::ClaudeSdk(ClaudeSdkCommand::SendPrompt {
                    agent: agent_id(AGENT),
                    text: row["message"]["content"].as_str().unwrap().into(),
                }),
            ));
        }
        if row["type"] == "amux.claude_sdk.input_result"
            && row["input_id"] == json!(Uuid::from_u128(256).as_bytes())
        {
            msgs.push(command(
                OpId(Uuid::from_u128(256)),
                Command::ClaudeSdk(ClaudeSdkCommand::Interrupt {
                    agent: agent_id(AGENT),
                }),
            ));
        }
        msgs.push(batch(AGENT, i as i64, vec![row]));
    }
    msgs
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    ["streaming", "tools", "interrupt"]
        .into_iter()
        .map(|name| (name, sequence(name)))
        .collect()
}

#[test]
fn recorded_conversations_send_stream_use_tools_interrupt_and_return_to_input_readiness() {
    for (name, msgs) in sequences() {
        crate::wire_free::assert_differential_sequence(name, msgs.clone());
        let mut model = Model::default();
        let mut saw_prompt = false;
        let mut saw_streaming = false;
        let mut saw_tool_result = false;
        let mut saw_interrupt = false;
        let mut captures = Vec::new();
        for msg in msgs.clone() {
            let effects = update(&mut model, msg);
            for effect in &effects {
                if let Effect::SendInput {
                    input_id,
                    payload: InputPayload::ClaudeSdk { payload },
                    ..
                } = effect
                {
                    match payload {
                        ClaudeSdkInput::Prompt { text, .. } => {
                            let layer = claude_sdk_layer(&model, AGENT);
                            assert_eq!(input_id, Uuid::from_u128(1).as_bytes());
                            assert_eq!(layer.pending_echo().unwrap().text, *text);
                            assert_eq!(
                                claude_sdk::send_gate(&model, agent_id(AGENT)),
                                SendGate::InputInFlight
                            );
                            captures.push(capture(&model, "sending", &effects));
                        }
                        ClaudeSdkInput::Interrupt => {
                            assert_eq!(name, "interrupt");
                            assert_eq!(
                                claude_sdk::phase(&model, agent_id(AGENT)),
                                SdkPhase::Working
                            );
                            saw_interrupt = true;
                            captures.push(capture(&model, "interrupt-sent", &effects));
                        }
                        other => panic!("unexpected conversation input: {other:?}"),
                    }
                }
            }
            let Some(layer) = model.claude_sdk(agent_id(AGENT)) else {
                continue;
            };
            if !saw_prompt
                && layer
                    .entries()
                    .any(|entry| matches!(&entry.kind, FeedEntryKind::Prompt(_)))
            {
                saw_prompt = true;
                assert!(layer.pending_echo().is_none());
                assert!(layer.in_flight_input().is_none());
                assert_eq!(
                    claude_sdk::phase(&model, agent_id(AGENT)),
                    SdkPhase::Working
                );
                captures.push(capture(&model, "accepted-prompt", &effects));
            }
            if !saw_streaming && layer.entries().any(|entry| matches!(&entry.kind,
                FeedEntryKind::Message(message) if !message.text.is_empty() && message.finality == Finality::Streaming)) {
                assert!(saw_prompt, "the human's prompt is visible before streaming starts");
                saw_streaming = true;
                captures.push(capture(&model, "streaming-reply", &effects));
            }
            if !saw_tool_result
                && layer.entries().any(|entry| {
                    matches!(&entry.kind,
                FeedEntryKind::Tool(tool) if tool.result.is_some())
                })
            {
                assert!(saw_prompt);
                saw_tool_result = true;
                captures.push(capture(&model, "tool-result", &effects));
            }
        }
        assert!(saw_prompt);
        assert_eq!(saw_streaming, name == "streaming");
        assert_eq!(saw_tool_result, name == "tools");
        assert_eq!(saw_interrupt, name == "interrupt");
        let layer = claude_sdk_layer(&model, AGENT);
        assert!(layer.pending_echo().is_none());
        assert!(layer.in_flight_input().is_none());
        assert_eq!(
            claude_sdk::send_gate(&model, agent_id(AGENT)),
            SendGate::Ready
        );
        assert_eq!(
            claude_sdk::phase(&model, agent_id(AGENT)),
            match name {
                "streaming" => SdkPhase::Finished,
                "tools" => SdkPhase::Errored,
                "interrupt" => SdkPhase::Interrupted,
                _ => unreachable!(),
            }
        );
        if name == "streaming" {
            assert!(layer.entries().any(|entry| matches!(&entry.kind,
                FeedEntryKind::Message(message) if message.text == "SEVEN" && message.finality == Finality::Complete)));
        }
        captures.push(capture(&model, "ready-for-next-prompt", &[]));
        if let Some(path) = std::env::var_os("CLAUDE_SDK_CONVERSE_EVIDENCE") {
            let path = std::path::PathBuf::from(path);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join(format!("{name}-states.json")),
                serde_json::to_string_pretty(&captures).unwrap(),
            )
            .unwrap();
            std::fs::write(
                path.join(format!("{name}-msgs.jsonl")),
                msgs.iter()
                    .map(|msg| format!("{}\n", serde_json::to_string(msg).unwrap()))
                    .collect::<String>(),
            )
            .unwrap();
        }
    }
}

fn capture(model: &Model, state: &str, effects: &[Effect]) -> Value {
    let layer = claude_sdk_layer(model, AGENT);
    json!({"state":state, "phase":claude_sdk::phase(model, agent_id(AGENT)),
        "send_gate":claude_sdk::send_gate(model, agent_id(AGENT)),
        "entries":layer.entries().collect::<Vec<_>>(), "pending_echo":layer.pending_echo(), "effects":effects})
}

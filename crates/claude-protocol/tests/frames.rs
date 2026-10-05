//! The initialize exchange and the tolerance rules, on recorded and made-up
//! frames.

use std::path::Path;

use claude_protocol::stream::control::{ControlOutcome, InitializeRequestBody};
use claude_protocol::stream::{
    self, ContentBlock, ControlRequest, ControlRequestBody, ControlResponse, Extensions,
    InitializationResult, Input, Message, MessageContent, MessageParam, Output, PermissionMode,
    Role, UserInput,
};
use serde_json::Value;

/// The lines of one recording that contain every needle, in order.
fn recorded(spec: &str, stdout: bool, needles: &[&str]) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../claude-specs/fixtures/sdk")
        .join(spec)
        .join("io.jsonl");
    std::fs::read_to_string(&path)
        .expect("recording")
        .lines()
        .map(|record| serde_json::from_str::<Value>(record).expect("record"))
        .filter(|record| (record["dir"] == "stdout") == stdout)
        .map(|record| record["line"].as_str().expect("line").to_owned())
        .find(|line| needles.iter().all(|needle| line.contains(needle)))
        .unwrap_or_else(|| panic!("{spec}: no line with {needles:?}"))
}

#[test]
fn the_initialize_exchange_reads_both_ways() {
    let sent = recorded("controls", false, &[r#""subtype":"initialize""#]);
    let Input::ControlRequest(request) = stream::strict_input(sent.as_bytes()).expect("known")
    else {
        panic!("not a control request");
    };
    assert!(matches!(request.request, ControlRequestBody::Initialize(_)));
    let answered = recorded(
        "controls",
        true,
        &[
            &format!(r#""request_id":"{}""#, request.request_id),
            "commands",
        ],
    );
    let Output::ControlResponse(response) = stream::strict(answered.as_bytes()).expect("known")
    else {
        panic!("not a control response");
    };
    assert_eq!(response.response.subtype, ControlOutcome::Success);
    let result: InitializationResult = response.result().expect("a payload").expect("decodes");
    assert!(!result.commands.is_empty());
}

#[test]
fn what_amux_writes_encodes_with_sorted_keys() {
    let initialize = Input::ControlRequest(ControlRequest::new(
        "req_0",
        ControlRequestBody::Initialize(InitializeRequestBody::default()),
    ));
    assert_eq!(
        stream::encode(&initialize),
        br#"{"request":{"subtype":"initialize"},"request_id":"req_0","type":"control_request"}"#
    );

    let prompt = Input::User(UserInput {
        message: MessageParam {
            role: Role::User,
            content: MessageContent::Text("hi".into()),
            extensions: Extensions::new(),
        },
        parent_tool_use_id: None,
        session_id: "s".into(),
        uuid: None,
        priority: None,
        extensions: Extensions::new(),
    });
    assert_eq!(
        stream::encode(&prompt),
        br#"{"message":{"content":"hi","role":"user"},"parent_tool_use_id":null,"session_id":"s","type":"user"}"#
    );

    let mode = Input::ControlRequest(ControlRequest::new(
        "req_1",
        ControlRequestBody::SetPermissionMode(stream::control::SetPermissionModeRequest {
            mode: PermissionMode::AcceptEdits,
            extensions: Extensions::new(),
        }),
    ));
    assert_eq!(
        stream::encode(&mode),
        br#"{"request":{"mode":"acceptEdits","subtype":"set_permission_mode"},"request_id":"req_1","type":"control_request"}"#
    );

    let refusal = Input::ControlResponse(ControlResponse::error("req_9", "no"));
    assert_eq!(
        stream::encode(&refusal),
        br#"{"response":{"error":"no","request_id":"req_9","subtype":"error"},"type":"control_response"}"#
    );
}

#[test]
fn unknown_fields_and_blocks_are_kept_and_refused_strictly() {
    let line = br#"{"type":"assistant","uuid":"u","session_id":"s","parent_tool_use_id":null,"mood":"calm","message":{"id":"m","type":"message","role":"assistant","model":"x","usage":{"input_tokens":1,"output_tokens":1},"content":[{"type":"hologram","shape":"cube"},{"type":"text","text":"hi"}]}}"#;
    let Output::Message(Message::Assistant(assistant)) = stream::decode(line).expect("decodes")
    else {
        panic!("not an assistant message");
    };
    assert_eq!(assistant.extensions["mood"], "calm");
    assert!(matches!(
        assistant.message.content[0],
        ContentBlock::Unknown(_)
    ));
    let again: Value =
        serde_json::from_slice(&stream::encode_output(&stream::decode(line).unwrap())).unwrap();
    assert_eq!(again, serde_json::from_slice::<Value>(line).unwrap());

    let drift = stream::strict(line).expect_err("refused");
    assert!(drift.to_string().contains("hologram"), "{drift}");
}

#[test]
fn a_line_that_is_not_an_object_is_the_only_decode_error() {
    assert!(stream::decode(b"[1]").is_err());
    assert!(stream::decode(b"nope").is_err());
    assert!(matches!(
        stream::decode(br#"{"no":"type"}"#),
        Ok(Output::Unknown(_))
    ));
}

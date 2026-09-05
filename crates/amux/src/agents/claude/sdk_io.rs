//! Claude SDK protocol payloads and structured row vocabulary.
//!
//! Provider messages retain their stream-JSON value unchanged. Rows authored
//! only for this protocol occupy the `amux.claude_sdk.*` namespace; the shared
//! `amux.attachments` row is also synthesized here. Both are a closed enum so
//! additions require a protocol change and a frozen-shape test.

use prost::Message as ProstMessage;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::ArtifactRef;
use crate::protocol::{ProtocolError, wire};

pub const CLAUDE_SDK_V1: &str = "claude_sdk_v1";
const SYNTHESIZED_PREFIX: &str = "amux.claude_sdk.";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClaudeSdkV1Args {
    pub replay_query: Option<ClaudeSdkV1ReplayQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeSdkV1ReplayQuery {
    /// Last structured sequence observed by the client. Replay resumes after it.
    Since {
        seq_id: u64,
    },
    Tail {
        count: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSdkV1Output {
    pub seq_id: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum ClaudeSdkV1Input {
    Prompt {
        text: String,
        /// Blocks added after decoding by daemon-side attachment materialisation.
        image_blocks: Vec<claude::sdk::ContentBlock>,
    },
    Interrupt,
    PermissionDecision {
        request_id: String,
        decision: claude::sdk::PermissionResult,
    },
    ElicitationDecision {
        request_id: String,
        result: claude::sdk::ElicitationResult,
    },
    DialogDecision {
        request_id: String,
        result: claude::sdk::UserDialogResult,
    },
}

/// Rows amux may add to Claude's otherwise verbatim stream-JSON output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ClaudeSdkSynthesized {
    #[serde(rename = "amux.claude_sdk.ready")]
    Ready { session_id: String, resumed: bool },
    #[serde(rename = "amux.claude_sdk.gap")]
    Gap { resumed_session_id: String },
    #[serde(rename = "amux.claude_sdk.permission_required")]
    PermissionRequired {
        request_id: String,
        tool_name: String,
        input: Value,
        suggestions: Vec<Value>,
    },
    #[serde(rename = "amux.claude_sdk.permission_resolved")]
    PermissionResolved {
        request_id: String,
        decision: String,
    },
    #[serde(rename = "amux.claude_sdk.elicitation_required")]
    ElicitationRequired {
        request_id: String,
        server: Option<String>,
        message: String,
        schema: Value,
    },
    #[serde(rename = "amux.claude_sdk.elicitation_resolved")]
    ElicitationResolved {
        request_id: String,
        decision: String,
    },
    #[serde(rename = "amux.claude_sdk.dialog_required")]
    DialogRequired {
        request_id: String,
        dialog_kind: String,
        payload: Value,
    },
    #[serde(rename = "amux.claude_sdk.dialog_resolved")]
    DialogResolved {
        request_id: String,
        decision: String,
    },
    #[serde(rename = "amux.claude_sdk.input_result")]
    InputResult { input_id: Vec<u8>, outcome: String },
    #[serde(rename = "amux.claude_sdk.message")]
    Message { envelope: Value, delivery: String },
    #[serde(rename = "amux.attachments")]
    Attachments {
        input_id: Option<String>,
        refs: Vec<ArtifactRef>,
    },
}

/// One row on `claude_sdk_v1`.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeSdkV1Row {
    Synthesized(ClaudeSdkSynthesized),
    Verbatim(Value),
}

impl ClaudeSdkV1Row {
    pub fn into_json(self) -> Value {
        match self {
            Self::Synthesized(row) => serde_json::to_value(row)
                .expect("ClaudeSdkSynthesized contains only JSON-serializable fields"),
            Self::Verbatim(value) => value,
        }
    }

    pub fn from_json(value: Value) -> Result<Self, ProtocolError> {
        let synthesized = value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|row_type| {
                row_type.starts_with(SYNTHESIZED_PREFIX) || row_type == "amux.attachments"
            });
        if !synthesized {
            return Ok(Self::Verbatim(value));
        }

        serde_json::from_value(value)
            .map(Self::Synthesized)
            .map_err(|error| ProtocolError::InvalidArgument {
                message: format!("invalid `{CLAUDE_SDK_V1}` synthesized row: {error}"),
            })
    }
}

pub(crate) fn decode_claude_sdk_v1_args(
    args: Option<&[u8]>,
) -> Result<ClaudeSdkV1Args, ProtocolError> {
    let args = match args {
        Some(args) => {
            wire::ClaudeSdkV1Args::decode(args).map_err(|error| invalid_args(error.to_string()))?
        }
        None => wire::ClaudeSdkV1Args::default(),
    };
    let replay_query = args
        .replay_query
        .map(|query| {
            let query = query
                .query
                .ok_or_else(|| invalid_args("replay_query missing query"))?;
            Ok(match query {
                wire::claude_sdk_v1_replay_query::Query::Since(seq_id) => {
                    ClaudeSdkV1ReplayQuery::Since { seq_id }
                }
                wire::claude_sdk_v1_replay_query::Query::TailCount(count) => {
                    ClaudeSdkV1ReplayQuery::Tail { count }
                }
            })
        })
        .transpose()?;
    Ok(ClaudeSdkV1Args { replay_query })
}

pub fn encode_claude_sdk_v1_args(args: ClaudeSdkV1Args) -> Option<Vec<u8>> {
    args.replay_query.map(|query| {
        wire::ClaudeSdkV1Args {
            replay_query: Some(wire::ClaudeSdkV1ReplayQuery {
                query: Some(match query {
                    ClaudeSdkV1ReplayQuery::Since { seq_id } => {
                        wire::claude_sdk_v1_replay_query::Query::Since(seq_id)
                    }
                    ClaudeSdkV1ReplayQuery::Tail { count } => {
                        wire::claude_sdk_v1_replay_query::Query::TailCount(count)
                    }
                }),
            }),
        }
        .encode_to_vec()
    })
}

pub(crate) fn decode_claude_sdk_v1_input(
    payload: &[u8],
) -> Result<ClaudeSdkV1Input, ProtocolError> {
    let input = wire::ClaudeSdkV1Input::decode(payload).map_err(|error| {
        invalid_input(format!(
            "payload must be ClaudeSdkV1Input protobuf: {error}"
        ))
    })?;
    match input
        .input
        .ok_or_else(|| invalid_input("payload missing input"))?
    {
        wire::claude_sdk_v1_input::Input::Prompt(prompt) => Ok(ClaudeSdkV1Input::Prompt {
            text: prompt.text,
            image_blocks: Vec::new(),
        }),
        wire::claude_sdk_v1_input::Input::Interrupt(_) => Ok(ClaudeSdkV1Input::Interrupt),
        wire::claude_sdk_v1_input::Input::ElicitationDecision(decision) => {
            Ok(ClaudeSdkV1Input::ElicitationDecision {
                request_id: decision.request_id,
                result: decode_json(&decision.result_json, "elicitation result_json")?,
            })
        }
        wire::claude_sdk_v1_input::Input::DialogDecision(decision) => {
            Ok(ClaudeSdkV1Input::DialogDecision {
                request_id: decision.request_id,
                result: decode_json(&decision.result_json, "dialog result_json")?,
            })
        }
        wire::claude_sdk_v1_input::Input::PermissionDecision(permission) => {
            let decision = permission
                .decision
                .ok_or_else(|| invalid_input("permission decision missing decision"))?;
            let decision = match decision {
                wire::claude_sdk_permission_decision::Decision::Allow(allow) => {
                    let updated_input = allow
                        .updated_input_json
                        .map(|json| decode_json(&json, "updated_input_json"))
                        .transpose()?;
                    let updated_permissions = if allow.updated_permissions_json.is_empty() {
                        None
                    } else {
                        Some(
                            allow
                                .updated_permissions_json
                                .iter()
                                .enumerate()
                                .map(|(index, json)| {
                                    decode_json(json, &format!("updated_permissions_json[{index}]"))
                                })
                                .collect::<Result<Vec<_>, _>>()?,
                        )
                    };
                    claude::sdk::PermissionResult::Allow {
                        updated_input,
                        updated_permissions,
                        tool_use_id: allow.tool_use_id,
                    }
                }
                wire::claude_sdk_permission_decision::Decision::Deny(deny) => {
                    claude::sdk::PermissionResult::Deny {
                        message: deny.message,
                        interrupt: deny.interrupt,
                        tool_use_id: deny.tool_use_id,
                    }
                }
            };
            Ok(ClaudeSdkV1Input::PermissionDecision {
                request_id: permission.request_id,
                decision,
            })
        }
    }
}

pub fn encode_claude_sdk_v1_input(input: ClaudeSdkV1Input) -> Result<Vec<u8>, ProtocolError> {
    let input = match input {
        ClaudeSdkV1Input::Prompt { text, .. } => {
            wire::claude_sdk_v1_input::Input::Prompt(wire::ClaudeSdkPrompt { text })
        }
        ClaudeSdkV1Input::Interrupt => {
            wire::claude_sdk_v1_input::Input::Interrupt(wire::ClaudeSdkInterrupt {})
        }
        ClaudeSdkV1Input::ElicitationDecision { request_id, result } => {
            wire::claude_sdk_v1_input::Input::ElicitationDecision(
                wire::ClaudeSdkElicitationDecision {
                    request_id,
                    result_json: encode_json(&result, "elicitation result")?,
                },
            )
        }
        ClaudeSdkV1Input::DialogDecision { request_id, result } => {
            wire::claude_sdk_v1_input::Input::DialogDecision(wire::ClaudeSdkDialogDecision {
                request_id,
                result_json: encode_json(&result, "dialog result")?,
            })
        }
        ClaudeSdkV1Input::PermissionDecision {
            request_id,
            decision,
        } => {
            let decision = match decision {
                claude::sdk::PermissionResult::Allow {
                    updated_input,
                    updated_permissions,
                    tool_use_id,
                } => wire::claude_sdk_permission_decision::Decision::Allow(
                    wire::ClaudeSdkPermissionAllow {
                        updated_input_json: updated_input
                            .map(|value| encode_json(&value, "updated_input"))
                            .transpose()?,
                        updated_permissions_json: updated_permissions
                            .unwrap_or_default()
                            .iter()
                            .enumerate()
                            .map(|(index, update)| {
                                encode_json(update, &format!("updated_permissions[{index}]"))
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                        tool_use_id,
                    },
                ),
                claude::sdk::PermissionResult::Deny {
                    message,
                    interrupt,
                    tool_use_id,
                } => wire::claude_sdk_permission_decision::Decision::Deny(
                    wire::ClaudeSdkPermissionDeny {
                        message,
                        interrupt,
                        tool_use_id,
                    },
                ),
            };
            wire::claude_sdk_v1_input::Input::PermissionDecision(
                wire::ClaudeSdkPermissionDecision {
                    request_id,
                    decision: Some(decision),
                },
            )
        }
    };
    Ok(wire::ClaudeSdkV1Input { input: Some(input) }.encode_to_vec())
}

pub(crate) fn encode_claude_sdk_v1_output(output: ClaudeSdkV1Output) -> Vec<u8> {
    wire::ClaudeSdkV1Output {
        seq_id: output.seq_id,
        payload: output.payload,
    }
    .encode_to_vec()
}

pub fn decode_claude_sdk_v1_output(payload: &[u8]) -> Result<ClaudeSdkV1Output, ProtocolError> {
    wire::ClaudeSdkV1Output::decode(payload)
        .map(|output| ClaudeSdkV1Output {
            seq_id: output.seq_id,
            payload: output.payload,
        })
        .map_err(|error| ProtocolError::InvalidArgument {
            message: format!(
                "`{CLAUDE_SDK_V1}` output payload must be ClaudeSdkV1Output protobuf: {error}"
            ),
        })
}

fn decode_json<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    field: &str,
) -> Result<T, ProtocolError> {
    serde_json::from_slice(bytes)
        .map_err(|error| invalid_input(format!("{field} must be JSON: {error}")))
}

fn encode_json<T: Serialize>(value: &T, field: &str) -> Result<Vec<u8>, ProtocolError> {
    serde_json::to_vec(value).map_err(|error| ProtocolError::InvalidArgument {
        message: format!("`{CLAUDE_SDK_V1}` {field} could not be encoded as JSON: {error}"),
    })
}

fn invalid_args(message: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::InvalidArgument {
        message: format!("`{CLAUDE_SDK_V1}` SubscribeSession args must be protobuf: {message}"),
    }
}

fn invalid_input(message: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::InvalidArgument {
        message: format!("`{CLAUDE_SDK_V1}` input {message}"),
    }
}

#[cfg(test)]
mod tests {
    use amux_artifacts::{ArtifactKind, id_of};
    use serde_json::json;

    use super::*;

    #[test]
    fn args_and_output_roundtrip() {
        for args in [
            ClaudeSdkV1Args {
                replay_query: Some(ClaudeSdkV1ReplayQuery::Since { seq_id: 41 }),
            },
            ClaudeSdkV1Args {
                replay_query: Some(ClaudeSdkV1ReplayQuery::Tail { count: 8 }),
            },
        ] {
            let encoded = encode_claude_sdk_v1_args(args.clone()).unwrap();
            assert_eq!(decode_claude_sdk_v1_args(Some(&encoded)).unwrap(), args);
        }
        assert_eq!(
            decode_claude_sdk_v1_args(None).unwrap(),
            ClaudeSdkV1Args::default()
        );
        assert_eq!(encode_claude_sdk_v1_args(ClaudeSdkV1Args::default()), None);

        let output = ClaudeSdkV1Output {
            seq_id: 42,
            payload: br#"{"type":"assistant"}"#.to_vec(),
        };
        let encoded = encode_claude_sdk_v1_output(output.clone());
        assert_eq!(decode_claude_sdk_v1_output(&encoded).unwrap(), output);
    }

    #[test]
    fn input_roundtrip_preserves_every_variant() {
        let inputs = [
            ClaudeSdkV1Input::Prompt {
                text: "answer precisely".to_string(),
                image_blocks: Vec::new(),
            },
            ClaudeSdkV1Input::Interrupt,
            ClaudeSdkV1Input::PermissionDecision {
                request_id: "allow-1".to_string(),
                decision: claude::sdk::PermissionResult::Allow {
                    updated_input: Some(json!({"command": "pwd"})),
                    updated_permissions: Some(vec![claude::sdk::PermissionUpdate::SetMode {
                        mode: claude::sdk::PermissionMode::AcceptEdits,
                        destination: claude::sdk::PermissionUpdateDestination::Session,
                    }]),
                    tool_use_id: Some("tool-1".to_string()),
                },
            },
            ClaudeSdkV1Input::PermissionDecision {
                request_id: "deny-1".to_string(),
                decision: claude::sdk::PermissionResult::Deny {
                    message: "not this command".to_string(),
                    interrupt: Some(true),
                    tool_use_id: Some("tool-2".to_string()),
                },
            },
        ];

        for input in inputs {
            let expected = input_as_json(&input);
            let encoded = encode_claude_sdk_v1_input(input).unwrap();
            let decoded = decode_claude_sdk_v1_input(&encoded).unwrap();
            assert_eq!(input_as_json(&decoded), expected);
        }
    }

    #[test]
    fn claude_sdk_ask_inputs_preserve_provider_results_and_freeze_wire_shape() {
        for (dialog, result) in [
            (
                false,
                json!({"action": "accept", "content": {"choice": "a"}, "future": [1, null]}),
            ),
            (false, json!({"action": "accept"})),
            (false, json!({"action": "decline"})),
            (false, json!({"action": "cancel"})),
            (
                true,
                json!({"behavior": "completed", "result": [null, {"ok": true}], "future": 42}),
            ),
            (true, json!({"behavior": "completed", "result": null})),
            (true, json!({"behavior": "cancelled"})),
        ] {
            let input = if dialog {
                ClaudeSdkV1Input::DialogDecision {
                    request_id: "r".into(),
                    result: serde_json::from_value(result.clone()).unwrap(),
                }
            } else {
                ClaudeSdkV1Input::ElicitationDecision {
                    request_id: "r".into(),
                    result: serde_json::from_value(result.clone()).unwrap(),
                }
            };
            let expected = input_as_json(&input);
            let encoded = encode_claude_sdk_v1_input(input).unwrap();
            let decoded = decode_claude_sdk_v1_input(&encoded).unwrap();
            assert_eq!(input_as_json(&decoded), expected);

            // Freeze the oneof tags and the nested request-id/result field numbers.
            let json_bytes = match decoded {
                ClaudeSdkV1Input::DialogDecision { result, .. } => {
                    serde_json::to_vec(&result).unwrap()
                }
                ClaudeSdkV1Input::ElicitationDecision { result, .. } => {
                    serde_json::to_vec(&result).unwrap()
                }
                _ => unreachable!(),
            };
            assert_eq!(
                serde_json::from_slice::<Value>(&json_bytes).unwrap(),
                result
            );
            assert!(json_bytes.len() + 5 < 128);
            let mut expected_wire = vec![
                if dialog { 114 } else { 106 },
                (json_bytes.len() + 5) as u8,
                10,
                1,
                b'r',
                18,
                json_bytes.len() as u8,
            ];
            expected_wire.extend(json_bytes);
            assert_eq!(encoded, expected_wire);
        }
    }

    #[test]
    fn claude_sdk_malformed_ask_results_are_rejected() {
        for result_json in [
            b"".to_vec(),
            b"not-json".to_vec(),
            br#"{"action":"unknown"}"#.to_vec(),
            b"null".to_vec(),
        ] {
            for input in [
                wire::claude_sdk_v1_input::Input::ElicitationDecision(
                    wire::ClaudeSdkElicitationDecision {
                        request_id: "r".into(),
                        result_json: result_json.clone(),
                    },
                ),
                wire::claude_sdk_v1_input::Input::DialogDecision(wire::ClaudeSdkDialogDecision {
                    request_id: "r".into(),
                    result_json: result_json.clone(),
                }),
            ] {
                let encoded = wire::ClaudeSdkV1Input { input: Some(input) }.encode_to_vec();
                assert!(
                    decode_claude_sdk_v1_input(&encoded)
                        .unwrap_err()
                        .to_string()
                        .contains("result_json must be JSON")
                );
            }
        }
    }

    #[test]
    fn malformed_nested_permission_json_is_rejected() {
        let encoded = wire::ClaudeSdkV1Input {
            input: Some(wire::claude_sdk_v1_input::Input::PermissionDecision(
                wire::ClaudeSdkPermissionDecision {
                    request_id: "request-1".to_string(),
                    decision: Some(wire::claude_sdk_permission_decision::Decision::Allow(
                        wire::ClaudeSdkPermissionAllow {
                            updated_input_json: Some(b"not-json".to_vec()),
                            updated_permissions_json: Vec::new(),
                            tool_use_id: None,
                        },
                    )),
                },
            )),
        }
        .encode_to_vec();

        let error = decode_claude_sdk_v1_input(&encoded).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("updated_input_json must be JSON")
        );
    }

    #[test]
    fn claude_sdk_synthesized_row_json_shape_is_frozen() {
        let cases = [
            (
                ClaudeSdkSynthesized::Ready {
                    session_id: "session-1".to_string(),
                    resumed: false,
                },
                json!({"type": "amux.claude_sdk.ready", "session_id": "session-1", "resumed": false}),
            ),
            (
                ClaudeSdkSynthesized::Gap {
                    resumed_session_id: "session-1".to_string(),
                },
                json!({"type": "amux.claude_sdk.gap", "resumed_session_id": "session-1"}),
            ),
            (
                ClaudeSdkSynthesized::PermissionRequired {
                    request_id: "permission-1".to_string(),
                    tool_name: "Bash".to_string(),
                    input: json!({"command": "pwd"}),
                    suggestions: vec![json!({"type": "setMode", "mode": "acceptEdits"})],
                },
                json!({"type": "amux.claude_sdk.permission_required", "request_id": "permission-1", "tool_name": "Bash", "input": {"command": "pwd"}, "suggestions": [{"type": "setMode", "mode": "acceptEdits"}]}),
            ),
            (
                ClaudeSdkSynthesized::PermissionResolved {
                    request_id: "permission-1".to_string(),
                    decision: "allow".to_string(),
                },
                json!({"type": "amux.claude_sdk.permission_resolved", "request_id": "permission-1", "decision": "allow"}),
            ),
            (
                ClaudeSdkSynthesized::ElicitationRequired {
                    request_id: "e".into(),
                    server: Some("forms".into()),
                    message: "Pick one".into(),
                    schema: json!({"type": "object"}),
                },
                json!({"type": "amux.claude_sdk.elicitation_required", "request_id": "e", "server": "forms", "message": "Pick one", "schema": {"type": "object"}}),
            ),
            (
                ClaudeSdkSynthesized::ElicitationRequired {
                    request_id: "e".into(),
                    server: None,
                    message: "Pick one".into(),
                    schema: Value::Null,
                },
                json!({"type": "amux.claude_sdk.elicitation_required", "request_id": "e", "server": null, "message": "Pick one", "schema": null}),
            ),
            (
                ClaudeSdkSynthesized::ElicitationResolved {
                    request_id: "e".into(),
                    decision: "accept".into(),
                },
                json!({"type": "amux.claude_sdk.elicitation_resolved", "request_id": "e", "decision": "accept"}),
            ),
            (
                ClaudeSdkSynthesized::DialogRequired {
                    request_id: "d".into(),
                    dialog_kind: "Future.Kind".into(),
                    payload: json!([null, {"x": [1, 2]}]),
                },
                json!({"type": "amux.claude_sdk.dialog_required", "request_id": "d", "dialog_kind": "Future.Kind", "payload": [null, {"x": [1, 2]}]}),
            ),
            (
                ClaudeSdkSynthesized::DialogResolved {
                    request_id: "d".into(),
                    decision: "completed".into(),
                },
                json!({"type": "amux.claude_sdk.dialog_resolved", "request_id": "d", "decision": "completed"}),
            ),
            (
                ClaudeSdkSynthesized::InputResult {
                    input_id: b"input-1".to_vec(),
                    outcome: "ok".to_string(),
                },
                json!({"type": "amux.claude_sdk.input_result", "input_id": [105, 110, 112, 117, 116, 45, 49], "outcome": "ok"}),
            ),
            (
                ClaudeSdkSynthesized::Message {
                    envelope: json!({"from": "sender", "text": "hello"}),
                    delivery: "stream".to_string(),
                },
                json!({"type": "amux.claude_sdk.message", "envelope": {"from": "sender", "text": "hello"}, "delivery": "stream"}),
            ),
            (
                ClaudeSdkSynthesized::Attachments {
                    input_id: Some("00af10".to_string()),
                    refs: vec![ArtifactRef {
                        id: id_of(b"image"),
                        kind: ArtifactKind::Image,
                        name: "screen.png".to_string(),
                        mime: "image/png".to_string(),
                        size: 5,
                    }],
                },
                json!({
                    "type": "amux.attachments",
                    "input_id": "00af10",
                    "refs": [{
                        "id": id_of(b"image"),
                        "kind": "image",
                        "name": "screen.png",
                        "mime": "image/png",
                        "size": 5
                    }]
                }),
            ),
        ];

        for (row, expected) in cases {
            let value = ClaudeSdkV1Row::Synthesized(row.clone()).into_json();
            assert_eq!(value, expected);
            let mut unknown_field = value.clone();
            unknown_field["unexpected"] = json!(true);
            assert!(ClaudeSdkV1Row::from_json(unknown_field).is_err());
            assert_eq!(
                ClaudeSdkV1Row::from_json(value).unwrap(),
                ClaudeSdkV1Row::Synthesized(row)
            );
        }
    }

    #[test]
    fn claude_sdk_synthesized_namespace_is_closed() {
        for invalid in [
            json!({"type": "amux.claude_sdk.unknown"}),
            json!({"type": "amux.claude_sdk.ready", "session_id": "session-1", "resumed": false, "unexpected": true}),
        ] {
            let error = ClaudeSdkV1Row::from_json(invalid).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("invalid `claude_sdk_v1` synthesized row")
            );
        }
    }

    #[test]
    fn verbatim_stream_json_passes_through_untouched() {
        let upstream = json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "hello"},
                    {"type": "future_block", "nested": {"new": [1, 2, 3]}}
                ],
                "future_field": {"kept": true}
            },
            "unknown_top_level": "kept too"
        });
        let row = ClaudeSdkV1Row::from_json(upstream.clone()).unwrap();
        assert_eq!(row, ClaudeSdkV1Row::Verbatim(upstream.clone()));
        assert_eq!(row.into_json(), upstream);
    }

    fn input_as_json(input: &ClaudeSdkV1Input) -> Value {
        match input {
            ClaudeSdkV1Input::Prompt { text, .. } => json!({"prompt": text}),
            ClaudeSdkV1Input::Interrupt => json!({"interrupt": {}}),
            ClaudeSdkV1Input::ElicitationDecision { request_id, result } => json!({
                "elicitation_decision": {"request_id": request_id, "result": result}
            }),
            ClaudeSdkV1Input::DialogDecision { request_id, result } => json!({
                "dialog_decision": {"request_id": request_id, "result": result}
            }),
            ClaudeSdkV1Input::PermissionDecision {
                request_id,
                decision,
            } => json!({
                "permission_decision": {
                    "request_id": request_id,
                    "decision": serde_json::to_value(decision).unwrap()
                }
            }),
        }
    }
}

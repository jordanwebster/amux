//! Claude SDK protocol payloads and structured row vocabulary.
//!
//! Provider messages retain their stream-JSON value unchanged. Rows authored
//! only for this protocol occupy the `amux.claude_sdk.*` namespace; the shared
//! `amux.attachments` row is also synthesized here. Both are a closed enum so
//! additions require a protocol change and a frozen-shape test.

#[cfg(test)]
use model::ProtocolError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::ArtifactRef;

#[cfg(test)]
pub const CLAUDE_SDK_V1: &str = "claude_sdk_v1";
#[cfg(test)]
const SYNTHESIZED_PREFIX: &str = "amux.claude_sdk.";

#[derive(Debug, Clone)]
pub enum ClaudeSdkV1Input {
    Prompt {
        text: String,
        /// Blocks added after decoding by daemon-side attachment materialisation.
        image_blocks: Vec<claude::sdk::ContentBlock>,
    },
    Interrupt,
    SetPermissionMode {
        mode: claude::sdk::PermissionMode,
    },
    SetModel {
        model: Option<String>,
    },
    RequestContextBreakdown,
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
    #[serde(rename = "amux.claude_sdk.session_facts")]
    SessionFacts {
        model: Option<String>,
        permission_mode: Option<String>,
        context: Option<ContextMeter>,
        mcp_servers: Vec<McpServerFact>,
    },
    #[serde(rename = "amux.claude_sdk.context_breakdown")]
    ContextBreakdown {
        usage: Box<claude::sdk::init::ContextUsage>,
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

/// Latest observed context size, with a passively observed window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextMeter {
    pub used_tokens: u64,
    pub window_tokens: Option<u64>,
    pub source: ContextMeterSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMeterSource {
    AssistantUsage,
    ResultUsage,
    AssistantContextUsage,
    CompactBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerFact {
    pub name: String,
    pub status: String,
}

/// One row on `claude_sdk_v1`.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeSdkV1Row {
    Synthesized(ClaudeSdkSynthesized),
    #[cfg(test)]
    Verbatim(Value),
}

impl ClaudeSdkV1Row {
    pub fn into_json(self) -> Value {
        match self {
            Self::Synthesized(row) => serde_json::to_value(row)
                .expect("ClaudeSdkSynthesized contains only JSON-serializable fields"),
            #[cfg(test)]
            Self::Verbatim(value) => value,
        }
    }

    #[cfg(test)]
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

#[cfg(test)]
mod tests {
    use model::{ArtifactKind, id_of};
    use serde_json::json;

    use super::*;

    #[test]
    fn claude_sdk_synthesized_row_json_shape_is_frozen() {
        let cases = [
            (
                ClaudeSdkSynthesized::SessionFacts {
                    model: Some("model".into()),
                    permission_mode: Some("plan".into()),
                    context: Some(ContextMeter {
                        used_tokens: 42,
                        window_tokens: Some(200000),
                        source: ContextMeterSource::AssistantUsage,
                    }),
                    mcp_servers: vec![McpServerFact {
                        name: "tools".into(),
                        status: "connected".into(),
                    }],
                },
                json!({"type": "amux.claude_sdk.session_facts", "model": "model", "permission_mode": "plan",
                    "context": {"used_tokens": 42, "window_tokens": 200000, "source": "assistant_usage"},
                    "mcp_servers": [{"name": "tools", "status": "connected"}]}),
            ),
            (
                ClaudeSdkSynthesized::SessionFacts {
                    model: None,
                    permission_mode: None,
                    context: None,
                    mcp_servers: vec![],
                },
                json!({"type": "amux.claude_sdk.session_facts", "model": null, "permission_mode": null, "context": null, "mcp_servers": []}),
            ),
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
}

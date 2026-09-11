use prost::Message;
use serde_json::Value;

use crate::{self as wire, DecodeError, EncodeError};

pub fn encode_claude_pty_input(expected_seq: u64, intent: model::ClaudePtyIntent) -> Vec<u8> {
    use model::ClaudePtyIntent;
    use wire::claude_pty_transcript_v1_input::Intent;
    let intent = match intent {
        ClaudePtyIntent::Prompt { text } => Intent::Prompt(wire::ClaudePrompt { text }),
        ClaudePtyIntent::Interrupt => Intent::Interrupt(wire::ClaudeInterrupt {}),
        ClaudePtyIntent::CyclePermissionMode => {
            Intent::CyclePermissionMode(wire::ClaudeCyclePermissionMode {})
        }
        ClaudePtyIntent::Answer { ask_id, answer } => Intent::Answer(wire::ClaudeAnswer {
            ask_id,
            answer: Some(answer_to_wire(answer)),
        }),
    };
    wire::ClaudePtyTranscriptV1Input {
        expected_seq,
        intent: Some(intent),
    }
    .encode_to_vec()
}

fn answer_to_wire(answer: model::AskAnswer) -> wire::claude_answer::Answer {
    use model::{AskAnswer, PermissionAnswer, PlanAnswer};
    use wire::claude_answer::Answer;
    match answer {
        AskAnswer::Permission(answer) => Answer::Permission(wire::ClaudePermissionAnswer {
            decision: Some(match answer {
                PermissionAnswer::AllowOnce => wire::claude_permission_answer::Decision::AllowOnce(
                    wire::ClaudePermissionAllowOnce {},
                ),
                PermissionAnswer::AllowScoped { suggestion } => {
                    wire::claude_permission_answer::Decision::AllowScoped(
                        wire::ClaudePermissionAllowScoped {
                            suggestion: u64::try_from(suggestion).unwrap_or(u64::MAX),
                        },
                    )
                }
                PermissionAnswer::Deny { feedback } => {
                    wire::claude_permission_answer::Decision::Deny(wire::ClaudePermissionDeny {
                        feedback,
                    })
                }
            }),
        }),
        AskAnswer::Plan(answer) => Answer::Plan(wire::ClaudePlanAnswer {
            decision: Some(match answer {
                PlanAnswer::ApproveAuto => {
                    wire::claude_plan_answer::Decision::ApproveAuto(wire::ClaudePlanApproveAuto {})
                }
                PlanAnswer::ApproveManual => wire::claude_plan_answer::Decision::ApproveManual(
                    wire::ClaudePlanApproveManual {},
                ),
                PlanAnswer::RequestChanges { feedback } => {
                    wire::claude_plan_answer::Decision::RequestChanges(
                        wire::ClaudePlanRequestChanges { feedback },
                    )
                }
            }),
        }),
        AskAnswer::Question(response) => Answer::Question(wire::ClaudeQuestionResponse {
            answers: response
                .answers
                .into_iter()
                .map(|answer| wire::ClaudeQuestionAnswer {
                    selected: answer
                        .selected
                        .into_iter()
                        .map(|index| u64::try_from(index).unwrap_or(u64::MAX))
                        .collect(),
                    other: answer.other,
                })
                .collect(),
        }),
    }
}

pub fn encode_claude_pty_args(args: model::ClaudePtyTranscriptV1Args) -> Option<Vec<u8>> {
    if args.terminal_size.is_none() && args.replay_query.is_none() {
        return None;
    }
    Some(
        wire::ClaudePtyTranscriptV1Args {
            terminal_size: args.terminal_size.map(|size| wire::TerminalSize {
                rows: u32::from(size.rows),
                cols: u32::from(size.cols),
            }),
            replay_query: args
                .replay_query
                .map(|query| wire::ClaudePtyTranscriptV1ReplayQuery {
                    query: Some(match query {
                        model::ClaudePtyTranscriptV1ReplayQuery::Since { seq_id } => {
                            wire::claude_pty_transcript_v1_replay_query::Query::Since(seq_id)
                        }
                        model::ClaudePtyTranscriptV1ReplayQuery::Tail { count } => {
                            wire::claude_pty_transcript_v1_replay_query::Query::TailCount(count)
                        }
                    }),
                }),
        }
        .encode_to_vec(),
    )
}

pub fn decode_claude_pty_output(
    payload: &[u8],
) -> Result<model::ClaudePtyTranscriptV1Output, DecodeError> {
    let output = wire::ClaudePtyTranscriptV1Output::decode(payload)
        .map_err(|error| DecodeError::Invalid(format!("invalid Claude PTY output: {error}")))?;
    Ok(model::ClaudePtyTranscriptV1Output {
        seq_id: output.seq_id,
        payload: output.payload,
    })
}

pub fn encode_claude_sdk_args(args: model::ClaudeSdkV1Args) -> Option<Vec<u8>> {
    args.replay_query.map(|query| {
        wire::ClaudeSdkV1Args {
            replay_query: Some(wire::ClaudeSdkV1ReplayQuery {
                query: Some(match query {
                    model::ClaudeSdkV1ReplayQuery::Since { seq_id } => {
                        wire::claude_sdk_v1_replay_query::Query::Since(seq_id)
                    }
                    model::ClaudeSdkV1ReplayQuery::Tail { count } => {
                        wire::claude_sdk_v1_replay_query::Query::TailCount(count)
                    }
                }),
            }),
        }
        .encode_to_vec()
    })
}

pub fn encode_claude_sdk_input(input: model::ClaudeSdkInput) -> Result<Vec<u8>, EncodeError> {
    use model::ClaudeSdkInput;
    use wire::claude_sdk_v1_input::Input;
    let input = match input {
        ClaudeSdkInput::Prompt { text } => Input::Prompt(wire::ClaudeSdkPrompt { text }),
        ClaudeSdkInput::Interrupt => Input::Interrupt(wire::ClaudeSdkInterrupt {}),
        ClaudeSdkInput::SetPermissionMode { mode } => {
            Input::SetPermissionMode(wire::ClaudeSdkSetPermissionMode { mode })
        }
        ClaudeSdkInput::SetModel { model } => Input::SetModel(wire::ClaudeSdkSetModel { model }),
        ClaudeSdkInput::RequestContextBreakdown => {
            Input::RequestContextBreakdown(wire::ClaudeSdkRequestContextBreakdown {})
        }
        ClaudeSdkInput::ElicitationDecision { request_id, result } => {
            Input::ElicitationDecision(wire::ClaudeSdkElicitationDecision {
                request_id,
                result_json: json_bytes(&result, "elicitation result")?,
            })
        }
        ClaudeSdkInput::DialogDecision { request_id, result } => {
            Input::DialogDecision(wire::ClaudeSdkDialogDecision {
                request_id,
                result_json: json_bytes(&result, "dialog result")?,
            })
        }
        ClaudeSdkInput::PermissionDecision {
            request_id,
            decision,
        } => Input::PermissionDecision(permission_decision(request_id, decision)?),
    };
    Ok(wire::ClaudeSdkV1Input { input: Some(input) }.encode_to_vec())
}

fn permission_decision(
    request_id: String,
    value: Value,
) -> Result<wire::ClaudeSdkPermissionDecision, EncodeError> {
    let behavior = value
        .get("behavior")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            EncodeError::Invalid("Claude SDK permission decision requires behavior".into())
        })?;
    let decision = match behavior {
        "allow" => {
            let updated_input_json = value
                .get("updatedInput")
                .filter(|value| !value.is_null())
                .map(|value| json_bytes(value, "updatedInput"))
                .transpose()?;
            let updated_permissions_json = value
                .get("updatedPermissions")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .enumerate()
                .map(|(index, value)| json_bytes(value, &format!("updatedPermissions[{index}]")))
                .collect::<Result<Vec<_>, _>>()?;
            wire::claude_sdk_permission_decision::Decision::Allow(wire::ClaudeSdkPermissionAllow {
                updated_input_json,
                updated_permissions_json,
                tool_use_id: value
                    .get("toolUseID")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        }
        "deny" => {
            wire::claude_sdk_permission_decision::Decision::Deny(wire::ClaudeSdkPermissionDeny {
                message: value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("User denied permission")
                    .to_owned(),
                interrupt: value.get("interrupt").and_then(Value::as_bool),
                tool_use_id: value
                    .get("toolUseID")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        }
        other => {
            return Err(EncodeError::Invalid(format!(
                "unsupported Claude SDK permission behavior {other:?}"
            )));
        }
    };
    Ok(wire::ClaudeSdkPermissionDecision {
        request_id,
        decision: Some(decision),
    })
}

pub fn decode_claude_sdk_output(payload: &[u8]) -> Result<model::ClaudeSdkV1Output, DecodeError> {
    let output = wire::ClaudeSdkV1Output::decode(payload)
        .map_err(|error| DecodeError::Invalid(format!("invalid Claude SDK output: {error}")))?;
    Ok(model::ClaudeSdkV1Output {
        seq_id: output.seq_id,
        payload: output.payload,
    })
}

pub fn encode_codex_sdk_args(args: model::CodexSdkV1Args) -> Option<Vec<u8>> {
    args.replay_query.map(|query| {
        wire::CodexSdkV1Args {
            replay_query: Some(wire::CodexSdkV1ReplayQuery {
                query: Some(match query {
                    model::CodexSdkV1ReplayQuery::Since { seq } => {
                        wire::codex_sdk_v1_replay_query::Query::Since(seq)
                    }
                    model::CodexSdkV1ReplayQuery::Tail { count } => {
                        wire::codex_sdk_v1_replay_query::Query::TailCount(count)
                    }
                }),
            }),
        }
        .encode_to_vec()
    })
}

pub fn encode_codex_sdk_input(input: model::CodexSdkInput) -> Vec<u8> {
    use wire::codex_sdk_v1_input::Input;
    wire::CodexSdkV1Input {
        input: Some(match input {
            model::CodexSdkInput::UserTurn { input } => {
                Input::UserTurn(wire::CodexSdkV1UserTurn { input })
            }
            model::CodexSdkInput::Steer { turn_id, input } => {
                Input::Steer(wire::CodexSdkV1Steer { turn_id, input })
            }
            model::CodexSdkInput::Interrupt { turn_id } => {
                Input::Interrupt(wire::CodexSdkV1Interrupt { turn_id })
            }
            model::CodexSdkInput::ApprovalDecision {
                request_id,
                decision,
            } => Input::ApprovalDecision(wire::CodexSdkV1ApprovalDecision {
                request_id,
                decision,
            }),
        }),
    }
    .encode_to_vec()
}

pub fn decode_codex_sdk_output(payload: &[u8]) -> Result<model::CodexSdkV1Output, DecodeError> {
    let output = wire::CodexSdkV1Output::decode(payload)
        .map_err(|error| DecodeError::Invalid(format!("invalid Codex SDK output: {error}")))?;
    Ok(model::CodexSdkV1Output {
        seq: output.seq,
        payload: output.payload,
    })
}

fn json_bytes(value: &Value, field: &str) -> Result<Vec<u8>, EncodeError> {
    serde_json::to_vec(value)
        .map_err(|error| EncodeError::Invalid(format!("invalid {field}: {error}")))
}

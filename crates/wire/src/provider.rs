use prost::Message;
use serde_json::Value;

use crate::{self as wire, DecodeError, EncodeError};

pub fn decode_terminal_args(payload: Option<&[u8]>) -> Result<model::TerminalV1Args, DecodeError> {
    let args = match payload {
        Some(payload) => wire::TerminalV1Args::decode(payload)
            .map_err(|error| DecodeError::Invalid(format!("invalid terminal args: {error}")))?,
        None => wire::TerminalV1Args::default(),
    };
    Ok(model::TerminalV1Args {
        terminal_size: args
            .terminal_size
            .map(terminal_size_from_wire)
            .transpose()?,
        replay_query: args
            .replay_query
            .map(|query| {
                match query.query.ok_or_else(|| {
                    DecodeError::Invalid("terminal replay query is missing".into())
                })? {
                    wire::terminal_v1_replay_query::Query::TailBytes(count) => {
                        Ok::<_, DecodeError>(model::TerminalV1ReplayQuery::TailBytes { count })
                    }
                }
            })
            .transpose()?,
    })
}

pub fn decode_terminal_control(payload: &[u8]) -> Result<model::TerminalV1Control, DecodeError> {
    let control = wire::SessionControl::decode(payload)
        .map_err(|error| DecodeError::Invalid(format!("invalid terminal control: {error}")))?
        .control
        .ok_or_else(|| DecodeError::Invalid("terminal control is missing".into()))?;
    match control {
        wire::session_control::Control::Resize(size) => Ok(model::TerminalV1Control::Resize(
            terminal_size_from_wire(size)?,
        )),
    }
}

pub fn decode_claude_pty_args(
    payload: Option<&[u8]>,
) -> Result<model::ClaudePtyTranscriptV1Args, DecodeError> {
    let args = match payload {
        Some(payload) => wire::ClaudePtyTranscriptV1Args::decode(payload)
            .map_err(|error| DecodeError::Invalid(format!("invalid Claude PTY args: {error}")))?,
        None => wire::ClaudePtyTranscriptV1Args::default(),
    };
    Ok(model::ClaudePtyTranscriptV1Args {
        terminal_size: args
            .terminal_size
            .map(terminal_size_from_wire)
            .transpose()?,
        replay_query: args
            .replay_query
            .map(|query| {
                match query.query.ok_or_else(|| {
                    DecodeError::Invalid("Claude PTY replay query is missing".into())
                })? {
                    wire::claude_pty_transcript_v1_replay_query::Query::Since(seq_id) => {
                        Ok::<_, DecodeError>(model::ClaudePtyTranscriptV1ReplayQuery::Since {
                            seq_id,
                        })
                    }
                    wire::claude_pty_transcript_v1_replay_query::Query::TailCount(count) => {
                        Ok::<_, DecodeError>(model::ClaudePtyTranscriptV1ReplayQuery::Tail {
                            count,
                        })
                    }
                }
            })
            .transpose()?,
    })
}

pub fn decode_claude_pty_input(
    payload: &[u8],
) -> Result<model::ClaudePtyTranscriptV1Input, DecodeError> {
    let input = wire::ClaudePtyTranscriptV1Input::decode(payload)
        .map_err(|error| DecodeError::Invalid(format!("invalid Claude PTY input: {error}")))?;
    let intent = input
        .intent
        .ok_or_else(|| DecodeError::Invalid("Claude PTY intent is missing".into()))?;
    use wire::claude_pty_transcript_v1_input::Intent;
    let intent = match intent {
        Intent::Prompt(prompt) => model::ClaudePtyIntent::Prompt { text: prompt.text },
        Intent::Interrupt(_) => model::ClaudePtyIntent::Interrupt,
        Intent::CyclePermissionMode(_) => model::ClaudePtyIntent::CyclePermissionMode,
        Intent::Answer(answer) => model::ClaudePtyIntent::Answer {
            ask_id: answer.ask_id,
            answer: answer_from_wire(
                answer
                    .answer
                    .ok_or_else(|| DecodeError::Invalid("Claude answer is missing".into()))?,
            )?,
        },
    };
    Ok(model::ClaudePtyTranscriptV1Input {
        expected_seq: input.expected_seq,
        intent,
    })
}

pub fn decode_claude_sdk_args(
    payload: Option<&[u8]>,
) -> Result<model::ClaudeSdkV1Args, DecodeError> {
    let args = match payload {
        Some(payload) => wire::ClaudeSdkV1Args::decode(payload)
            .map_err(|error| DecodeError::Invalid(format!("invalid Claude SDK args: {error}")))?,
        None => wire::ClaudeSdkV1Args::default(),
    };
    Ok(model::ClaudeSdkV1Args {
        replay_query: args
            .replay_query
            .map(|query| {
                match query.query.ok_or_else(|| {
                    DecodeError::Invalid("Claude SDK replay query is missing".into())
                })? {
                    wire::claude_sdk_v1_replay_query::Query::Since(seq_id) => {
                        Ok::<_, DecodeError>(model::ClaudeSdkV1ReplayQuery::Since { seq_id })
                    }
                    wire::claude_sdk_v1_replay_query::Query::TailCount(count) => {
                        Ok::<_, DecodeError>(model::ClaudeSdkV1ReplayQuery::Tail { count })
                    }
                }
            })
            .transpose()?,
    })
}

pub fn decode_claude_sdk_input(payload: &[u8]) -> Result<model::ClaudeSdkInput, DecodeError> {
    let input = wire::ClaudeSdkV1Input::decode(payload)
        .map_err(|error| DecodeError::Invalid(format!("invalid Claude SDK input: {error}")))?
        .input
        .ok_or_else(|| DecodeError::Invalid("Claude SDK input is missing".into()))?;
    use wire::claude_sdk_v1_input::Input;
    Ok(match input {
        Input::Prompt(value) => model::ClaudeSdkInput::Prompt { text: value.text },
        Input::Interrupt(_) => model::ClaudeSdkInput::Interrupt,
        Input::SetPermissionMode(value) => {
            model::ClaudeSdkInput::SetPermissionMode { mode: value.mode }
        }
        Input::SetModel(value) => model::ClaudeSdkInput::SetModel { model: value.model },
        Input::SetEffort(value) => model::ClaudeSdkInput::SetEffort {
            effort: value.effort,
        },
        Input::RequestContextBreakdown(_) => model::ClaudeSdkInput::RequestContextBreakdown,
        Input::ElicitationDecision(value) => model::ClaudeSdkInput::ElicitationDecision {
            request_id: value.request_id,
            result: json_value(&value.result_json, "elicitation result")?,
        },
        Input::DialogDecision(value) => model::ClaudeSdkInput::DialogDecision {
            request_id: value.request_id,
            result: json_value(&value.result_json, "dialog result")?,
        },
        Input::PermissionDecision(value) => {
            model::ClaudeSdkInput::PermissionDecision {
                request_id: value.request_id,
                decision: permission_decision_value(value.decision.ok_or_else(|| {
                    DecodeError::Invalid("permission decision is missing".into())
                })?)?,
            }
        }
    })
}

pub fn decode_codex_sdk_args(payload: Option<&[u8]>) -> Result<model::CodexSdkV1Args, DecodeError> {
    let args = match payload {
        Some(payload) => wire::CodexSdkV1Args::decode(payload)
            .map_err(|error| DecodeError::Invalid(format!("invalid Codex SDK args: {error}")))?,
        None => wire::CodexSdkV1Args::default(),
    };
    Ok(model::CodexSdkV1Args {
        replay_query: args
            .replay_query
            .map(|query| {
                match query
                    .query
                    .ok_or_else(|| DecodeError::Invalid("Codex replay query is missing".into()))?
                {
                    wire::codex_sdk_v1_replay_query::Query::Since(seq) => {
                        Ok::<_, DecodeError>(model::CodexSdkV1ReplayQuery::Since { seq })
                    }
                    wire::codex_sdk_v1_replay_query::Query::TailCount(count) => {
                        Ok::<_, DecodeError>(model::CodexSdkV1ReplayQuery::Tail { count })
                    }
                }
            })
            .transpose()?,
    })
}

pub fn decode_codex_sdk_input(payload: &[u8]) -> Result<model::CodexSdkInput, DecodeError> {
    let input = wire::CodexSdkV1Input::decode(payload)
        .map_err(|error| DecodeError::Invalid(format!("invalid Codex SDK input: {error}")))?
        .input
        .ok_or_else(|| DecodeError::Invalid("Codex SDK input is missing".into()))?;
    use wire::codex_sdk_v1_input::Input;
    let invalid = |field: &str, error: serde_json::Error| {
        DecodeError::Invalid(format!("invalid Codex preset {field}: {error}"))
    };
    Ok(match input {
        Input::Command(value) => model::CodexSdkInput::Command {
            name: value.name,
            args: value.args,
        },
        Input::SetModel(value) => model::CodexSdkInput::SetModel { model: value.model },
        Input::SetEffort(value) => model::CodexSdkInput::SetEffort {
            effort: value.effort,
        },
        Input::SetPreset(value) => model::CodexSdkInput::SetPreset {
            approval: serde_json::from_value(Value::String(value.approval))
                .map_err(|error| invalid("approval", error))?,
            sandbox: serde_json::from_value(Value::String(value.sandbox))
                .map_err(|error| invalid("sandbox", error))?,
        },
        Input::UserTurn(value) => model::CodexSdkInput::UserTurn { input: value.input },
        Input::Steer(value) => model::CodexSdkInput::Steer {
            turn_id: value.turn_id,
            input: value.input,
        },
        Input::Interrupt(value) => model::CodexSdkInput::Interrupt {
            turn_id: value.turn_id,
        },
        Input::ApprovalDecision(value) => model::CodexSdkInput::ApprovalDecision {
            request_id: value.request_id,
            decision: value.decision,
        },
    })
}

pub fn encode_provider_output(
    protocol: model::Protocol,
    sequence: Option<u64>,
    payload: Vec<u8>,
) -> Result<Vec<u8>, EncodeError> {
    let sequence = || {
        sequence
            .ok_or_else(|| EncodeError::Invalid(format!("{protocol} output requires a sequence")))
    };
    Ok(match protocol {
        model::Protocol::TerminalV1 | model::Protocol::TestEchoV1 => payload,
        model::Protocol::ClaudePtyTranscriptV1 => wire::ClaudePtyTranscriptV1Output {
            seq_id: sequence()?,
            payload,
        }
        .encode_to_vec(),
        model::Protocol::ClaudeSdkV1 => wire::ClaudeSdkV1Output {
            seq_id: sequence()?,
            payload,
        }
        .encode_to_vec(),
        model::Protocol::CodexSdkV1 => wire::CodexSdkV1Output {
            seq: sequence()?,
            payload,
        }
        .encode_to_vec(),
    })
}

pub fn encode_provider_cursor(
    protocol: model::Protocol,
    sequence: Option<u64>,
) -> Result<Option<Vec<u8>>, EncodeError> {
    Ok(match (protocol, sequence) {
        (_, None) => None,
        (
            model::Protocol::ClaudePtyTranscriptV1
            | model::Protocol::ClaudeSdkV1
            | model::Protocol::CodexSdkV1,
            Some(seq_id),
        ) => Some(wire::ClaudePtyTranscriptV1Cursor { seq_id }.encode_to_vec()),
        (protocol, Some(_)) => {
            return Err(EncodeError::Invalid(format!(
                "{protocol} does not support a cursor"
            )));
        }
    })
}

pub fn decode_provider_cursor(
    protocol: model::Protocol,
    payload: &[u8],
) -> Result<Option<u64>, DecodeError> {
    match protocol {
        model::Protocol::ClaudePtyTranscriptV1
        | model::Protocol::ClaudeSdkV1
        | model::Protocol::CodexSdkV1 => wire::ClaudePtyTranscriptV1Cursor::decode(payload)
            .map(|cursor| Some(cursor.seq_id))
            .map_err(|error| DecodeError::Invalid(format!("invalid {protocol} cursor: {error}"))),
        model::Protocol::TerminalV1 | model::Protocol::TestEchoV1 => Ok(None),
    }
}

fn terminal_size_from_wire(size: wire::TerminalSize) -> Result<model::TerminalSize, DecodeError> {
    Ok(model::TerminalSize {
        rows: size.rows.try_into().map_err(|_| {
            DecodeError::Invalid(format!("terminal rows out of range: {}", size.rows))
        })?,
        cols: size.cols.try_into().map_err(|_| {
            DecodeError::Invalid(format!("terminal columns out of range: {}", size.cols))
        })?,
    })
}

fn terminal_size_to_wire(size: model::TerminalSize) -> wire::TerminalSize {
    wire::TerminalSize {
        rows: size.rows.into(),
        cols: size.cols.into(),
    }
}

fn answer_from_wire(answer: wire::claude_answer::Answer) -> Result<model::AskAnswer, DecodeError> {
    use wire::claude_answer::Answer;
    Ok(match answer {
        Answer::Permission(value) => {
            let decision = value.decision.ok_or_else(|| {
                DecodeError::Invalid("Claude permission decision is missing".into())
            })?;
            use wire::claude_permission_answer::Decision;
            model::AskAnswer::Permission(match decision {
                Decision::AllowOnce(_) => model::PermissionAnswer::AllowOnce,
                Decision::AllowScoped(value) => model::PermissionAnswer::AllowScoped {
                    suggestion: value.suggestion.try_into().map_err(|_| {
                        DecodeError::Invalid("Claude suggestion index is out of range".into())
                    })?,
                },
                Decision::Deny(value) => model::PermissionAnswer::Deny {
                    feedback: value.feedback,
                },
            })
        }
        Answer::Plan(value) => {
            let decision = value
                .decision
                .ok_or_else(|| DecodeError::Invalid("Claude plan decision is missing".into()))?;
            use wire::claude_plan_answer::Decision;
            model::AskAnswer::Plan(match decision {
                Decision::ApproveAuto(_) => model::PlanAnswer::ApproveAuto,
                Decision::ApproveManual(_) => model::PlanAnswer::ApproveManual,
                Decision::RequestChanges(value) => model::PlanAnswer::RequestChanges {
                    feedback: value.feedback,
                },
            })
        }
        Answer::Question(value) => model::AskAnswer::Question(model::QuestionResponse {
            answers: value
                .answers
                .into_iter()
                .map(|answer| {
                    Ok(model::QuestionAnswer {
                        selected: answer
                            .selected
                            .into_iter()
                            .map(|index| {
                                index.try_into().map_err(|_| {
                                    DecodeError::Invalid(
                                        "Claude answer index is out of range".into(),
                                    )
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                        other: answer.other,
                    })
                })
                .collect::<Result<Vec<_>, DecodeError>>()?,
        }),
    })
}

fn json_value(bytes: &[u8], field: &str) -> Result<Value, DecodeError> {
    serde_json::from_slice(bytes)
        .map_err(|error| DecodeError::Invalid(format!("invalid {field} JSON: {error}")))
}

fn permission_decision_value(
    decision: wire::claude_sdk_permission_decision::Decision,
) -> Result<Value, DecodeError> {
    use wire::claude_sdk_permission_decision::Decision;
    Ok(match decision {
        Decision::Allow(value) => serde_json::json!({
            "behavior": "allow",
            "updatedInput": value.updated_input_json.as_deref().map(|bytes| json_value(bytes, "updatedInput")).transpose()?,
            "updatedPermissions": value.updated_permissions_json.iter().enumerate().map(|(index, bytes)| json_value(bytes, &format!("updatedPermissions[{index}]"))).collect::<Result<Vec<_>, _>>()?,
            "toolUseID": value.tool_use_id,
        }),
        Decision::Deny(value) => serde_json::json!({
            "behavior": "deny",
            "message": value.message,
            "interrupt": value.interrupt,
            "toolUseID": value.tool_use_id,
        }),
    })
}

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
        ClaudeSdkInput::SetEffort { effort } => {
            Input::SetEffort(wire::ClaudeSdkSetEffort { effort })
        }
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
    // A preset choice's wire name is its serde name, so the two never drift.
    let policy = |value: serde_json::Result<Value>| match value {
        Ok(Value::String(name)) => name,
        _ => unreachable!("Codex preset policies serialize as strings"),
    };
    wire::CodexSdkV1Input {
        input: Some(match input {
            model::CodexSdkInput::Command { name, args } => {
                Input::Command(wire::CodexSdkV1Command { name, args })
            }
            model::CodexSdkInput::SetModel { model } => {
                Input::SetModel(wire::CodexSdkV1SetModel { model })
            }
            model::CodexSdkInput::SetEffort { effort } => {
                Input::SetEffort(wire::CodexSdkV1SetEffort { effort })
            }
            model::CodexSdkInput::SetPreset { approval, sandbox } => {
                Input::SetPreset(wire::CodexSdkV1SetPreset {
                    approval: policy(serde_json::to_value(&approval)),
                    sandbox: policy(serde_json::to_value(&sandbox)),
                })
            }
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
pub fn encode_terminal_args(args: model::TerminalV1Args) -> Vec<u8> {
    wire::TerminalV1Args {
        terminal_size: args.terminal_size.map(terminal_size_to_wire),
        replay_query: args.replay_query.map(|query| wire::TerminalV1ReplayQuery {
            query: Some(match query {
                model::TerminalV1ReplayQuery::TailBytes { count } => {
                    wire::terminal_v1_replay_query::Query::TailBytes(count)
                }
            }),
        }),
    }
    .encode_to_vec()
}

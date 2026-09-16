//! Conversions between the typed session values in `model` and the protobuf
//! session messages. Each oneof is translated exactly once, here; nothing
//! above this crate handles session bytes.

use serde_json::Value;

use crate::{self as wire, DecodeError, EncodeError};

pub fn session_args_from_wire(
    protocol: wire::subscribe_session_request::Protocol,
) -> Result<model::SessionArgs, DecodeError> {
    use wire::subscribe_session_request::Protocol;
    Ok(match protocol {
        Protocol::TerminalV1(args) => model::SessionArgs::TerminalV1(model::TerminalV1Args {
            terminal_size: args
                .terminal_size
                .map(terminal_size_from_wire)
                .transpose()?,
            replay_query: args
                .replay_query
                .map(terminal_replay_from_wire)
                .transpose()?,
        }),
        Protocol::ClaudePtyTranscriptV1(args) => {
            model::SessionArgs::ClaudePtyTranscriptV1(model::ClaudePtyTranscriptV1Args {
                terminal_size: args
                    .terminal_size
                    .map(terminal_size_from_wire)
                    .transpose()?,
                replay_query: args.replay_query.map(replay_query_from_wire).transpose()?,
            })
        }
        Protocol::ClaudeSdkV1(args) => model::SessionArgs::ClaudeSdkV1(model::ClaudeSdkV1Args {
            replay_query: args.replay_query.map(replay_query_from_wire).transpose()?,
        }),
        Protocol::CodexSdkV1(args) => model::SessionArgs::CodexSdkV1(model::CodexSdkV1Args {
            replay_query: args.replay_query.map(replay_query_from_wire).transpose()?,
        }),
        Protocol::TestEchoV1(_) => model::SessionArgs::TestEchoV1,
    })
}

pub fn session_args_to_wire(
    args: &model::SessionArgs,
) -> wire::subscribe_session_request::Protocol {
    use wire::subscribe_session_request::Protocol;
    match args {
        model::SessionArgs::TerminalV1(args) => Protocol::TerminalV1(wire::TerminalV1Args {
            terminal_size: args.terminal_size.map(terminal_size_to_wire),
            replay_query: args.replay_query.as_ref().map(terminal_replay_to_wire),
        }),
        model::SessionArgs::ClaudePtyTranscriptV1(args) => {
            Protocol::ClaudePtyTranscriptV1(wire::ClaudePtyTranscriptV1Args {
                terminal_size: args.terminal_size.map(terminal_size_to_wire),
                replay_query: args.replay_query.as_ref().map(replay_query_to_wire),
            })
        }
        model::SessionArgs::ClaudeSdkV1(args) => Protocol::ClaudeSdkV1(wire::ClaudeSdkV1Args {
            replay_query: args.replay_query.as_ref().map(replay_query_to_wire),
        }),
        model::SessionArgs::CodexSdkV1(args) => Protocol::CodexSdkV1(wire::CodexSdkV1Args {
            replay_query: args.replay_query.as_ref().map(replay_query_to_wire),
        }),
        model::SessionArgs::TestEchoV1 => Protocol::TestEchoV1(wire::TestEchoV1Args {}),
    }
}

pub fn session_args_from_client_wire(
    protocol: wire::client_subscribe_session_request::Protocol,
) -> Result<model::SessionArgs, DecodeError> {
    use wire::client_subscribe_session_request::Protocol;
    session_args_from_wire(match protocol {
        Protocol::TerminalV1(value) => wire::subscribe_session_request::Protocol::TerminalV1(value),
        Protocol::ClaudePtyTranscriptV1(value) => {
            wire::subscribe_session_request::Protocol::ClaudePtyTranscriptV1(value)
        }
        Protocol::ClaudeSdkV1(value) => {
            wire::subscribe_session_request::Protocol::ClaudeSdkV1(value)
        }
        Protocol::CodexSdkV1(value) => wire::subscribe_session_request::Protocol::CodexSdkV1(value),
        Protocol::TestEchoV1(value) => wire::subscribe_session_request::Protocol::TestEchoV1(value),
    })
}

pub fn session_args_to_client_wire(
    args: &model::SessionArgs,
) -> wire::client_subscribe_session_request::Protocol {
    use wire::subscribe_session_request::Protocol;
    match session_args_to_wire(args) {
        Protocol::TerminalV1(value) => {
            wire::client_subscribe_session_request::Protocol::TerminalV1(value)
        }
        Protocol::ClaudePtyTranscriptV1(value) => {
            wire::client_subscribe_session_request::Protocol::ClaudePtyTranscriptV1(value)
        }
        Protocol::ClaudeSdkV1(value) => {
            wire::client_subscribe_session_request::Protocol::ClaudeSdkV1(value)
        }
        Protocol::CodexSdkV1(value) => {
            wire::client_subscribe_session_request::Protocol::CodexSdkV1(value)
        }
        Protocol::TestEchoV1(value) => {
            wire::client_subscribe_session_request::Protocol::TestEchoV1(value)
        }
    }
}

pub fn session_input_from_wire(
    event: wire::send_input_request::Event,
) -> Result<model::SessionInput, DecodeError> {
    use wire::send_input_request::Event;
    Ok(match event {
        Event::TerminalV1(input) => model::SessionInput::TerminalV1 {
            payload: input.payload,
        },
        Event::ClaudePtyTranscriptV1(input) => {
            model::SessionInput::ClaudePtyTranscriptV1(claude_pty_input_from_wire(input)?)
        }
        Event::ClaudeSdkV1(input) => {
            model::SessionInput::ClaudeSdkV1(claude_sdk_input_from_wire(input)?)
        }
        Event::CodexSdkV1(input) => model::SessionInput::CodexSdkV1(codex_input_from_wire(input)?),
        Event::Control(control) => {
            let control = control
                .control
                .ok_or_else(|| invalid("SessionControl missing control"))?;
            match control {
                wire::session_control::Control::Resize(size) => model::SessionInput::Control(
                    model::SessionControl::Resize(terminal_size_from_wire(size)?),
                ),
            }
        }
        Event::TestEchoV1(input) => model::SessionInput::TestEchoV1 {
            payload: input.payload,
        },
    })
}

/// Fails only when a provider-neutral JSON value in the input cannot be
/// expressed on the wire; the shapes the wire types make explicit are checked
/// here rather than trusted.
pub fn session_input_to_wire(
    input: &model::SessionInput,
) -> Result<wire::send_input_request::Event, EncodeError> {
    use wire::send_input_request::Event;
    Ok(match input {
        model::SessionInput::TerminalV1 { payload } => Event::TerminalV1(wire::TerminalV1Input {
            payload: payload.clone(),
        }),
        model::SessionInput::ClaudePtyTranscriptV1(input) => {
            Event::ClaudePtyTranscriptV1(claude_pty_input_to_wire(input))
        }
        model::SessionInput::ClaudeSdkV1(input) => {
            Event::ClaudeSdkV1(claude_sdk_input_to_wire(input)?)
        }
        model::SessionInput::CodexSdkV1(input) => Event::CodexSdkV1(codex_input_to_wire(input)),
        model::SessionInput::Control(model::SessionControl::Resize(size)) => {
            Event::Control(wire::SessionControl {
                control: Some(wire::session_control::Control::Resize(
                    terminal_size_to_wire(*size),
                )),
            })
        }
        model::SessionInput::TestEchoV1 { payload } => Event::TestEchoV1(wire::TestEchoV1Input {
            payload: payload.clone(),
        }),
    })
}

pub fn session_input_from_client_wire(
    event: wire::client_send_input_request::Event,
) -> Result<model::SessionInput, DecodeError> {
    use wire::client_send_input_request::Event;
    session_input_from_wire(match event {
        Event::TerminalV1(value) => wire::send_input_request::Event::TerminalV1(value),
        Event::ClaudePtyTranscriptV1(value) => {
            wire::send_input_request::Event::ClaudePtyTranscriptV1(value)
        }
        Event::ClaudeSdkV1(value) => wire::send_input_request::Event::ClaudeSdkV1(value),
        Event::CodexSdkV1(value) => wire::send_input_request::Event::CodexSdkV1(value),
        Event::Control(value) => wire::send_input_request::Event::Control(value),
        Event::TestEchoV1(value) => wire::send_input_request::Event::TestEchoV1(value),
    })
}

pub fn session_input_to_client_wire(
    input: &model::SessionInput,
) -> Result<wire::client_send_input_request::Event, EncodeError> {
    use wire::send_input_request::Event;
    Ok(match session_input_to_wire(input)? {
        Event::TerminalV1(value) => wire::client_send_input_request::Event::TerminalV1(value),
        Event::ClaudePtyTranscriptV1(value) => {
            wire::client_send_input_request::Event::ClaudePtyTranscriptV1(value)
        }
        Event::ClaudeSdkV1(value) => wire::client_send_input_request::Event::ClaudeSdkV1(value),
        Event::CodexSdkV1(value) => wire::client_send_input_request::Event::CodexSdkV1(value),
        Event::Control(value) => wire::client_send_input_request::Event::Control(value),
        Event::TestEchoV1(value) => wire::client_send_input_request::Event::TestEchoV1(value),
    })
}

pub fn session_output_from_wire(
    output: wire::SessionOutput,
) -> Result<model::SessionOutput, DecodeError> {
    use wire::session_output::Output;
    let output = output
        .output
        .ok_or_else(|| invalid("SessionOutput missing output"))?;
    Ok(match output {
        Output::TerminalV1(output) => model::SessionOutput::TerminalV1 {
            payload: output.payload,
        },
        Output::ClaudePtyTranscriptV1(output) => {
            model::SessionOutput::ClaudePtyTranscriptV1(structured_row_from_wire(output))
        }
        Output::ClaudeSdkV1(output) => {
            model::SessionOutput::ClaudeSdkV1(structured_row_from_wire(output))
        }
        Output::CodexSdkV1(output) => {
            model::SessionOutput::CodexSdkV1(structured_row_from_wire(output))
        }
        Output::TestEchoV1(output) => model::SessionOutput::TestEchoV1 {
            payload: output.payload,
        },
    })
}

pub fn session_output_to_wire(output: &model::SessionOutput) -> wire::SessionOutput {
    use wire::session_output::Output;
    let output = match output {
        model::SessionOutput::TerminalV1 { payload } => {
            Output::TerminalV1(wire::TerminalV1Output {
                payload: payload.clone(),
            })
        }
        model::SessionOutput::ClaudePtyTranscriptV1(row) => {
            Output::ClaudePtyTranscriptV1(structured_row_to_wire(row))
        }
        model::SessionOutput::ClaudeSdkV1(row) => Output::ClaudeSdkV1(structured_row_to_wire(row)),
        model::SessionOutput::CodexSdkV1(row) => Output::CodexSdkV1(structured_row_to_wire(row)),
        model::SessionOutput::TestEchoV1 { payload } => {
            Output::TestEchoV1(wire::TestEchoV1Output {
                payload: payload.clone(),
            })
        }
    };
    wire::SessionOutput {
        output: Some(output),
    }
}

pub fn replay_facts_from_wire(
    replay: wire::ReplayFacts,
) -> Result<model::ReplayFacts, DecodeError> {
    let outcome = match replay.outcome {
        Some(wire::replay_facts::Outcome::Continuous(_)) => model::ReplayOutcome::Continuous,
        Some(wire::replay_facts::Outcome::Truncated(value)) => model::ReplayOutcome::Truncated {
            missing_after: value.missing_after,
        },
        Some(wire::replay_facts::Outcome::Reset(value)) => model::ReplayOutcome::Reset {
            reason: value.reason,
        },
        None => return Err(invalid("ReplayFacts missing outcome")),
    };
    Ok(model::ReplayFacts {
        retained_from: replay.retained_from,
        through: replay.through,
        selected_from: replay.selected_from,
        reset_at: replay.reset_at,
        outcome,
    })
}

pub fn replay_facts_to_wire(replay: &model::ReplayFacts) -> wire::ReplayFacts {
    wire::ReplayFacts {
        retained_from: replay.retained_from,
        through: replay.through,
        selected_from: replay.selected_from,
        reset_at: replay.reset_at,
        outcome: Some(match &replay.outcome {
            model::ReplayOutcome::Continuous => {
                wire::replay_facts::Outcome::Continuous(wire::Continuous {})
            }
            model::ReplayOutcome::Truncated { missing_after } => {
                wire::replay_facts::Outcome::Truncated(wire::Truncated {
                    missing_after: *missing_after,
                })
            }
            model::ReplayOutcome::Reset { reason } => {
                wire::replay_facts::Outcome::Reset(wire::Reset {
                    reason: reason.clone(),
                })
            }
        }),
    }
}

fn structured_row_from_wire(row: wire::StructuredRow) -> model::StructuredRow {
    model::StructuredRow {
        seq: row.seq,
        published_at_unix_ms: row.published_at_unix_ms,
        activity_at_unix_ms: row.activity_at_unix_ms,
        historical: row.historical,
        payload: row.payload,
    }
}

fn structured_row_to_wire(row: &model::StructuredRow) -> wire::StructuredRow {
    wire::StructuredRow {
        seq: row.seq,
        published_at_unix_ms: row.published_at_unix_ms,
        activity_at_unix_ms: row.activity_at_unix_ms,
        historical: row.historical,
        payload: row.payload.clone(),
    }
}

fn terminal_replay_from_wire(
    query: wire::TerminalV1ReplayQuery,
) -> Result<model::TerminalV1ReplayQuery, DecodeError> {
    match query
        .query
        .ok_or_else(|| invalid("TerminalV1 replay_query missing query"))?
    {
        wire::terminal_v1_replay_query::Query::TailBytes(count) => {
            Ok(model::TerminalV1ReplayQuery::TailBytes { count })
        }
    }
}

fn terminal_replay_to_wire(query: &model::TerminalV1ReplayQuery) -> wire::TerminalV1ReplayQuery {
    wire::TerminalV1ReplayQuery {
        query: Some(match query {
            model::TerminalV1ReplayQuery::TailBytes { count } => {
                wire::terminal_v1_replay_query::Query::TailBytes(*count)
            }
        }),
    }
}

fn replay_query_from_wire(query: wire::ReplayQuery) -> Result<model::ReplayQuery, DecodeError> {
    match query
        .query
        .ok_or_else(|| invalid("ReplayQuery missing query"))?
    {
        wire::replay_query::Query::After(after) => Ok(model::ReplayQuery::After {
            after,
            tail_bound: query.tail_bound,
        }),
        wire::replay_query::Query::TailCount(count) => Ok(model::ReplayQuery::TailCount {
            count,
            tail_bound: query.tail_bound,
        }),
    }
}

fn replay_query_to_wire(query: &model::ReplayQuery) -> wire::ReplayQuery {
    wire::ReplayQuery {
        query: Some(match query {
            model::ReplayQuery::After { after, .. } => wire::replay_query::Query::After(*after),
            model::ReplayQuery::TailCount { count, .. } => {
                wire::replay_query::Query::TailCount(*count)
            }
        }),
        tail_bound: match query {
            model::ReplayQuery::After { tail_bound, .. }
            | model::ReplayQuery::TailCount { tail_bound, .. } => *tail_bound,
        },
    }
}

fn claude_pty_input_from_wire(
    input: wire::ClaudePtyTranscriptV1Input,
) -> Result<model::ClaudePtyTranscriptV1Input, DecodeError> {
    use wire::claude_pty_transcript_v1_input::Intent;
    let intent = match input
        .intent
        .ok_or_else(|| invalid("ClaudePtyTranscriptV1Input missing intent"))?
    {
        Intent::Prompt(prompt) => model::ClaudePtyIntent::Prompt { text: prompt.text },
        Intent::Interrupt(_) => model::ClaudePtyIntent::Interrupt,
        Intent::CyclePermissionMode(_) => model::ClaudePtyIntent::CyclePermissionMode,
        Intent::Answer(answer) => model::ClaudePtyIntent::Answer {
            ask_id: answer.ask_id,
            answer: answer_from_wire(
                answer
                    .answer
                    .ok_or_else(|| invalid("Claude answer missing answer"))?,
            )?,
        },
    };
    Ok(model::ClaudePtyTranscriptV1Input {
        expected_seq: input.expected_seq,
        intent,
    })
}

fn claude_pty_input_to_wire(
    input: &model::ClaudePtyTranscriptV1Input,
) -> wire::ClaudePtyTranscriptV1Input {
    use wire::claude_pty_transcript_v1_input::Intent;
    let intent = match &input.intent {
        model::ClaudePtyIntent::Prompt { text } => {
            Intent::Prompt(wire::ClaudePrompt { text: text.clone() })
        }
        model::ClaudePtyIntent::Interrupt => Intent::Interrupt(wire::ClaudeInterrupt {}),
        model::ClaudePtyIntent::CyclePermissionMode => {
            Intent::CyclePermissionMode(wire::ClaudeCyclePermissionMode {})
        }
        model::ClaudePtyIntent::Answer { ask_id, answer } => Intent::Answer(wire::ClaudeAnswer {
            ask_id: ask_id.clone(),
            answer: Some(answer_to_wire(answer)),
        }),
    };
    wire::ClaudePtyTranscriptV1Input {
        expected_seq: input.expected_seq,
        intent: Some(intent),
    }
}

fn answer_from_wire(answer: wire::claude_answer::Answer) -> Result<model::AskAnswer, DecodeError> {
    use wire::claude_answer::Answer;
    Ok(match answer {
        Answer::Permission(value) => {
            let decision = value
                .decision
                .ok_or_else(|| invalid("Claude permission decision is missing"))?;
            use wire::claude_permission_answer::Decision;
            model::AskAnswer::Permission(match decision {
                Decision::AllowOnce(_) => model::PermissionAnswer::AllowOnce,
                Decision::AllowScoped(value) => model::PermissionAnswer::AllowScoped {
                    suggestion: index_from_wire(value.suggestion, "permission suggestion")?,
                },
                Decision::Deny(value) => model::PermissionAnswer::Deny {
                    feedback: value.feedback,
                },
            })
        }
        Answer::Plan(value) => {
            let decision = value
                .decision
                .ok_or_else(|| invalid("Claude plan decision is missing"))?;
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
                            .map(|index| index_from_wire(index, "question selection"))
                            .collect::<Result<Vec<_>, _>>()?,
                        other: answer.other,
                    })
                })
                .collect::<Result<Vec<_>, DecodeError>>()?,
        }),
    })
}

fn answer_to_wire(answer: &model::AskAnswer) -> wire::claude_answer::Answer {
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
                            suggestion: index_to_wire(*suggestion),
                        },
                    )
                }
                PermissionAnswer::Deny { feedback } => {
                    wire::claude_permission_answer::Decision::Deny(wire::ClaudePermissionDeny {
                        feedback: feedback.clone(),
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
                        wire::ClaudePlanRequestChanges {
                            feedback: feedback.clone(),
                        },
                    )
                }
            }),
        }),
        AskAnswer::Question(response) => Answer::Question(wire::ClaudeQuestionResponse {
            answers: response
                .answers
                .iter()
                .map(|answer| wire::ClaudeQuestionAnswer {
                    selected: answer.selected.iter().copied().map(index_to_wire).collect(),
                    other: answer.other.clone(),
                })
                .collect(),
        }),
    }
}

fn claude_sdk_input_from_wire(
    input: wire::ClaudeSdkV1Input,
) -> Result<model::ClaudeSdkInput, DecodeError> {
    use wire::claude_sdk_v1_input::Input;
    let input = input
        .input
        .ok_or_else(|| invalid("ClaudeSdkV1Input missing input"))?;
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
        Input::PermissionDecision(value) => model::ClaudeSdkInput::PermissionDecision {
            request_id: value.request_id,
            decision: permission_decision_value(
                value
                    .decision
                    .ok_or_else(|| invalid("permission decision is missing"))?,
            )?,
        },
    })
}

fn claude_sdk_input_to_wire(
    input: &model::ClaudeSdkInput,
) -> Result<wire::ClaudeSdkV1Input, EncodeError> {
    use model::ClaudeSdkInput;
    use wire::claude_sdk_v1_input::Input;
    let input = match input {
        ClaudeSdkInput::Prompt { text } => {
            Input::Prompt(wire::ClaudeSdkPrompt { text: text.clone() })
        }
        ClaudeSdkInput::Interrupt => Input::Interrupt(wire::ClaudeSdkInterrupt {}),
        ClaudeSdkInput::SetPermissionMode { mode } => {
            Input::SetPermissionMode(wire::ClaudeSdkSetPermissionMode { mode: mode.clone() })
        }
        ClaudeSdkInput::SetModel { model } => Input::SetModel(wire::ClaudeSdkSetModel {
            model: model.clone(),
        }),
        ClaudeSdkInput::SetEffort { effort } => Input::SetEffort(wire::ClaudeSdkSetEffort {
            effort: effort.clone(),
        }),
        ClaudeSdkInput::RequestContextBreakdown => {
            Input::RequestContextBreakdown(wire::ClaudeSdkRequestContextBreakdown {})
        }
        ClaudeSdkInput::ElicitationDecision { request_id, result } => {
            Input::ElicitationDecision(wire::ClaudeSdkElicitationDecision {
                request_id: request_id.clone(),
                result_json: json_bytes(result),
            })
        }
        ClaudeSdkInput::DialogDecision { request_id, result } => {
            Input::DialogDecision(wire::ClaudeSdkDialogDecision {
                request_id: request_id.clone(),
                result_json: json_bytes(result),
            })
        }
        ClaudeSdkInput::PermissionDecision {
            request_id,
            decision,
        } => Input::PermissionDecision(permission_decision_to_wire(request_id.clone(), decision)?),
    };
    Ok(wire::ClaudeSdkV1Input { input: Some(input) })
}

/// The permission decision crosses the wire as its two typed shapes and lives
/// in `model` as the JSON object Claude's stream-JSON protocol expects.
fn permission_decision_value(
    decision: wire::claude_sdk_permission_decision::Decision,
) -> Result<Value, DecodeError> {
    use wire::claude_sdk_permission_decision::Decision;
    Ok(match decision {
        Decision::Allow(value) => {
            let mut decision = serde_json::Map::new();
            decision.insert("behavior".into(), "allow".into());
            if let Some(bytes) = value.updated_input_json.as_deref() {
                decision.insert("updatedInput".into(), json_value(bytes, "updatedInput")?);
            }
            if !value.updated_permissions_json.is_empty() {
                let permissions = value
                    .updated_permissions_json
                    .iter()
                    .enumerate()
                    .map(|(index, bytes)| {
                        json_value(bytes, &format!("updatedPermissions[{index}]"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                decision.insert("updatedPermissions".into(), Value::Array(permissions));
            }
            if let Some(tool_use_id) = value.tool_use_id {
                decision.insert("toolUseID".into(), tool_use_id.into());
            }
            Value::Object(decision)
        }
        Decision::Deny(value) => {
            let mut decision = serde_json::Map::new();
            decision.insert("behavior".into(), "deny".into());
            decision.insert("message".into(), value.message.into());
            if let Some(interrupt) = value.interrupt {
                decision.insert("interrupt".into(), interrupt.into());
            }
            if let Some(tool_use_id) = value.tool_use_id {
                decision.insert("toolUseID".into(), tool_use_id.into());
            }
            Value::Object(decision)
        }
    })
}

fn permission_decision_to_wire(
    request_id: String,
    value: &Value,
) -> Result<wire::ClaudeSdkPermissionDecision, EncodeError> {
    let tool_use_id = value
        .get("toolUseID")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let decision = match value.get("behavior").and_then(Value::as_str) {
        Some("allow") => {
            wire::claude_sdk_permission_decision::Decision::Allow(wire::ClaudeSdkPermissionAllow {
                updated_input_json: value
                    .get("updatedInput")
                    .filter(|value| !value.is_null())
                    .map(json_bytes),
                updated_permissions_json: value
                    .get("updatedPermissions")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .map(json_bytes)
                    .collect(),
                tool_use_id,
            })
        }
        Some("deny") => {
            wire::claude_sdk_permission_decision::Decision::Deny(wire::ClaudeSdkPermissionDeny {
                message: value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("User denied permission")
                    .to_owned(),
                interrupt: value.get("interrupt").and_then(Value::as_bool),
                tool_use_id,
            })
        }
        Some(other) => {
            return Err(EncodeError::Invalid(format!(
                "unsupported Claude SDK permission behavior {other:?}"
            )));
        }
        None => {
            return Err(EncodeError::Invalid(
                "Claude SDK permission decision requires behavior".into(),
            ));
        }
    };
    Ok(wire::ClaudeSdkPermissionDecision {
        request_id,
        decision: Some(decision),
    })
}

fn codex_input_from_wire(
    input: wire::CodexSdkV1Input,
) -> Result<model::CodexSdkInput, DecodeError> {
    use wire::codex_sdk_v1_input::Input;
    let input = input
        .input
        .ok_or_else(|| invalid("CodexSdkV1Input missing input"))?;
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
                .map_err(|error| invalid(format!("invalid Codex preset approval: {error}")))?,
            sandbox: serde_json::from_value(Value::String(value.sandbox))
                .map_err(|error| invalid(format!("invalid Codex preset sandbox: {error}")))?,
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

fn codex_input_to_wire(input: &model::CodexSdkInput) -> wire::CodexSdkV1Input {
    use wire::codex_sdk_v1_input::Input;
    // A preset choice's wire name is its serde name, so the two never drift.
    let policy = |value: serde_json::Result<Value>| match value {
        Ok(Value::String(name)) => name,
        _ => unreachable!("Codex preset policies serialize as strings"),
    };
    wire::CodexSdkV1Input {
        input: Some(match input {
            model::CodexSdkInput::Command { name, args } => {
                Input::Command(wire::CodexSdkV1Command {
                    name: name.clone(),
                    args: args.clone(),
                })
            }
            model::CodexSdkInput::SetModel { model } => Input::SetModel(wire::CodexSdkV1SetModel {
                model: model.clone(),
            }),
            model::CodexSdkInput::SetEffort { effort } => {
                Input::SetEffort(wire::CodexSdkV1SetEffort {
                    effort: effort.clone(),
                })
            }
            model::CodexSdkInput::SetPreset { approval, sandbox } => {
                Input::SetPreset(wire::CodexSdkV1SetPreset {
                    approval: policy(serde_json::to_value(approval)),
                    sandbox: policy(serde_json::to_value(sandbox)),
                })
            }
            model::CodexSdkInput::UserTurn { input } => Input::UserTurn(wire::CodexSdkV1UserTurn {
                input: input.clone(),
            }),
            model::CodexSdkInput::Steer { turn_id, input } => Input::Steer(wire::CodexSdkV1Steer {
                turn_id: turn_id.clone(),
                input: input.clone(),
            }),
            model::CodexSdkInput::Interrupt { turn_id } => {
                Input::Interrupt(wire::CodexSdkV1Interrupt {
                    turn_id: turn_id.clone(),
                })
            }
            model::CodexSdkInput::ApprovalDecision {
                request_id,
                decision,
            } => Input::ApprovalDecision(wire::CodexSdkV1ApprovalDecision {
                request_id: request_id.clone(),
                decision: decision.clone(),
            }),
        }),
    }
}

pub(crate) fn terminal_size_from_wire(
    size: wire::TerminalSize,
) -> Result<model::TerminalSize, DecodeError> {
    Ok(model::TerminalSize {
        rows: size
            .rows
            .try_into()
            .map_err(|_| invalid(format!("terminal rows out of range: {}", size.rows)))?,
        cols: size
            .cols
            .try_into()
            .map_err(|_| invalid(format!("terminal columns out of range: {}", size.cols)))?,
    })
}

pub(crate) fn terminal_size_to_wire(size: model::TerminalSize) -> wire::TerminalSize {
    wire::TerminalSize {
        rows: size.rows.into(),
        cols: size.cols.into(),
    }
}

fn index_from_wire(index: u64, field: &str) -> Result<usize, DecodeError> {
    index
        .try_into()
        .map_err(|_| invalid(format!("{field} is out of range: {index}")))
}

fn index_to_wire(index: usize) -> u64 {
    u64::try_from(index).unwrap_or(u64::MAX)
}

fn json_value(bytes: &[u8], field: &str) -> Result<Value, DecodeError> {
    serde_json::from_slice(bytes).map_err(|error| invalid(format!("invalid {field} JSON: {error}")))
}

fn json_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("a JSON value serializes")
}

fn invalid(message: impl Into<String>) -> DecodeError {
    DecodeError::Invalid(message.into())
}

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::*;

    #[test]
    fn session_args_roundtrip_every_protocol() {
        let cases = [
            model::SessionArgs::TerminalV1(model::TerminalV1Args {
                terminal_size: Some(model::TerminalSize {
                    rows: 30,
                    cols: 100,
                }),
                replay_query: Some(model::TerminalV1ReplayQuery::TailBytes { count: 4096 }),
            }),
            model::SessionArgs::ClaudePtyTranscriptV1(model::ClaudePtyTranscriptV1Args {
                terminal_size: None,
                replay_query: Some(model::ReplayQuery::After {
                    after: 42,
                    tail_bound: Some(20),
                }),
            }),
            model::SessionArgs::ClaudeSdkV1(model::ClaudeSdkV1Args {
                replay_query: Some(model::ReplayQuery::TailCount {
                    count: 12,
                    tail_bound: None,
                }),
            }),
            model::SessionArgs::CodexSdkV1(model::CodexSdkV1Args {
                replay_query: Some(model::ReplayQuery::After {
                    after: 7,
                    tail_bound: None,
                }),
            }),
            model::SessionArgs::TestEchoV1,
        ];
        for args in cases {
            assert_eq!(
                session_args_from_wire(session_args_to_wire(&args)).unwrap(),
                args
            );
        }
    }

    #[test]
    fn session_input_roundtrips_every_intent() {
        let inputs = vec![
            model::SessionInput::TerminalV1 {
                payload: b"ls\n".to_vec(),
            },
            model::SessionInput::Control(model::SessionControl::Resize(model::TerminalSize {
                rows: 40,
                cols: 120,
            })),
            model::SessionInput::ClaudePtyTranscriptV1(model::ClaudePtyTranscriptV1Input {
                expected_seq: 7,
                intent: model::ClaudePtyIntent::Answer {
                    ask_id: "questions".into(),
                    answer: model::AskAnswer::Question(model::QuestionResponse {
                        answers: vec![model::QuestionAnswer {
                            selected: vec![0, 2],
                            other: Some("another choice".into()),
                        }],
                    }),
                },
            }),
            model::SessionInput::ClaudeSdkV1(model::ClaudeSdkInput::PermissionDecision {
                request_id: "req".into(),
                decision: serde_json::json!({
                    "behavior": "deny",
                    "message": "no",
                    "interrupt": true,
                    "toolUseID": "tool",
                }),
            }),
            model::SessionInput::ClaudeSdkV1(model::ClaudeSdkInput::ElicitationDecision {
                request_id: "req".into(),
                result: serde_json::json!({"action": "accept"}),
            }),
            model::SessionInput::CodexSdkV1(model::CodexSdkInput::Steer {
                turn_id: "turn".into(),
                input: b"[]".to_vec(),
            }),
            model::SessionInput::TestEchoV1 {
                payload: b"echo".to_vec(),
            },
        ];
        for input in inputs {
            assert_eq!(
                session_input_from_wire(session_input_to_wire(&input).unwrap()).unwrap(),
                input
            );
        }
    }

    #[test]
    fn every_claude_pty_intent_and_codex_input_roundtrips() {
        let intents = [
            model::ClaudePtyIntent::Prompt {
                text: "hello".into(),
            },
            model::ClaudePtyIntent::Interrupt,
            model::ClaudePtyIntent::CyclePermissionMode,
            model::ClaudePtyIntent::Answer {
                ask_id: "permission".into(),
                answer: model::AskAnswer::Permission(model::PermissionAnswer::AllowScoped {
                    suggestion: 2,
                }),
            },
            model::ClaudePtyIntent::Answer {
                ask_id: "plan".into(),
                answer: model::AskAnswer::Plan(model::PlanAnswer::RequestChanges {
                    feedback: "add tests".into(),
                }),
            },
        ];
        for intent in intents {
            let input =
                model::SessionInput::ClaudePtyTranscriptV1(model::ClaudePtyTranscriptV1Input {
                    expected_seq: 3,
                    intent,
                });
            assert_eq!(
                session_input_from_wire(session_input_to_wire(&input).unwrap()).unwrap(),
                input
            );
        }
        let inputs = [
            model::CodexSdkInput::UserTurn {
                input: b"[]".to_vec(),
            },
            model::CodexSdkInput::Interrupt {
                turn_id: "turn".into(),
            },
            model::CodexSdkInput::ApprovalDecision {
                request_id: b"req".to_vec(),
                decision: "accept".into(),
            },
        ];
        for input in inputs {
            let input = model::SessionInput::CodexSdkV1(input);
            assert_eq!(
                session_input_from_wire(session_input_to_wire(&input).unwrap()).unwrap(),
                input
            );
        }
    }

    #[test]
    fn every_claude_sdk_input_roundtrips_including_an_allow_decision() {
        let inputs = [
            model::ClaudeSdkInput::Prompt {
                text: "hello".into(),
            },
            model::ClaudeSdkInput::Interrupt,
            model::ClaudeSdkInput::SetPermissionMode {
                mode: "plan".into(),
            },
            model::ClaudeSdkInput::SetModel {
                model: Some("haiku".into()),
            },
            model::ClaudeSdkInput::RequestContextBreakdown,
            model::ClaudeSdkInput::DialogDecision {
                request_id: "req".into(),
                result: serde_json::json!({"choice": 1}),
            },
            model::ClaudeSdkInput::PermissionDecision {
                request_id: "req".into(),
                decision: serde_json::json!({"behavior": "allow"}),
            },
            model::ClaudeSdkInput::PermissionDecision {
                request_id: "req".into(),
                decision: serde_json::json!({
                    "behavior": "allow",
                    "updatedInput": {"command": "ls"},
                    "updatedPermissions": [{"type": "addRules"}],
                    "toolUseID": "tool",
                }),
            },
            model::ClaudeSdkInput::PermissionDecision {
                request_id: "req".into(),
                decision: serde_json::json!({"behavior": "deny", "message": "no"}),
            },
        ];
        for input in inputs {
            let input = model::SessionInput::ClaudeSdkV1(input);
            assert_eq!(
                session_input_from_wire(session_input_to_wire(&input).unwrap()).unwrap(),
                input
            );
        }
    }

    #[test]
    fn claude_sdk_ask_inputs_preserve_provider_results_and_freeze_wire_shape() {
        for (dialog, result) in [
            (
                false,
                serde_json::json!({
                    "action": "accept",
                    "content": {"choice": "a"},
                    "future": [1, null]
                }),
            ),
            (false, serde_json::json!({"action": "accept"})),
            (false, serde_json::json!({"action": "decline"})),
            (false, serde_json::json!({"action": "cancel"})),
            (
                true,
                serde_json::json!({
                    "behavior": "completed",
                    "result": [null, {"ok": true}],
                    "future": 42
                }),
            ),
            (
                true,
                serde_json::json!({"behavior": "completed", "result": null}),
            ),
            (true, serde_json::json!({"behavior": "cancelled"})),
        ] {
            let input = if dialog {
                model::ClaudeSdkInput::DialogDecision {
                    request_id: "r".into(),
                    result: result.clone(),
                }
            } else {
                model::ClaudeSdkInput::ElicitationDecision {
                    request_id: "r".into(),
                    result: result.clone(),
                }
            };
            let encoded_input = claude_sdk_input_to_wire(&input).unwrap();
            assert_eq!(
                claude_sdk_input_from_wire(encoded_input.clone()).unwrap(),
                input
            );

            let json_bytes = serde_json::to_vec(&result).unwrap();
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
            assert_eq!(encoded_input.encode_to_vec(), expected_wire);
        }
    }

    #[test]
    fn malformed_permission_decisions_never_become_allows() {
        for decision in [
            serde_json::json!({}),
            serde_json::json!({"behavior": "ask"}),
            serde_json::json!({"behavior": "Deny"}),
            serde_json::json!("allow"),
        ] {
            let input =
                model::SessionInput::ClaudeSdkV1(model::ClaudeSdkInput::PermissionDecision {
                    request_id: "req".into(),
                    decision,
                });
            assert!(session_input_to_wire(&input).is_err());
        }
    }

    #[test]
    fn session_output_roundtrips_rows_and_bytes() {
        let outputs = [
            model::SessionOutput::TerminalV1 {
                payload: b"$ ".to_vec(),
            },
            model::SessionOutput::ClaudeSdkV1(model::StructuredRow {
                seq: 9,
                published_at_unix_ms: 123,
                activity_at_unix_ms: Some(100),
                historical: true,
                payload: br#"{"type":"assistant"}"#.to_vec(),
            }),
            model::SessionOutput::CodexSdkV1(model::StructuredRow {
                seq: 1,
                published_at_unix_ms: 456,
                activity_at_unix_ms: None,
                historical: false,
                payload: b"{}".to_vec(),
            }),
        ];
        for output in outputs {
            assert_eq!(
                session_output_from_wire(session_output_to_wire(&output)).unwrap(),
                output
            );
        }
    }

    #[test]
    fn replay_facts_roundtrip_every_outcome() {
        for outcome in [
            model::ReplayOutcome::Continuous,
            model::ReplayOutcome::Truncated { missing_after: 4 },
            model::ReplayOutcome::Reset {
                reason: "session replaced".into(),
            },
        ] {
            let facts = model::ReplayFacts {
                retained_from: 5,
                through: 11,
                selected_from: 8,
                reset_at: 3,
                outcome,
            };
            assert_eq!(
                replay_facts_from_wire(replay_facts_to_wire(&facts)).unwrap(),
                facts
            );
        }
    }

    #[test]
    fn replay_facts_require_an_outcome() {
        assert!(
            replay_facts_from_wire(wire::ReplayFacts {
                retained_from: 1,
                through: 1,
                selected_from: 1,
                reset_at: 0,
                outcome: None,
            })
            .is_err()
        );
    }
}

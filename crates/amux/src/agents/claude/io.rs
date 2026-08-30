//! Claude-owned IO protocol payloads for `AgentService/SubscribeSession` and `SendInput`.
//!
//! The core protocol treats `SubscribeSessionRequest.args`, `SessionInput.payload`,
//! `SessionOutput.payload`, and cursors as opaque bytes. This module owns the
//! first-party Claude schemas for those bytes.

use prost::Message as ProstMessage;
use serde::{Deserialize, Serialize};

use crate::agents::TerminalSize;
use crate::protocol::{ProtocolError, wire};

pub const PTY_TRANSCRIPT_V1: &str = "claude_pty_transcript_v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudePtyTranscriptV1Args {
    pub terminal_size: Option<TerminalSize>,
    pub replay_query: Option<ClaudePtyTranscriptV1ReplayQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudePtyTranscriptV1ReplayQuery {
    /// Last transcript sequence observed by the client. Replay resumes after it.
    Since {
        seq_id: u64,
    },
    Tail {
        count: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudePtyTranscriptV1Input {
    pub expected_seq: u64,
    pub intent: Intent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case", deny_unknown_fields)]
pub enum Intent {
    Prompt { text: String },
    Interrupt,
    CyclePermissionMode,
    Answer { ask_id: String, answer: AskAnswer },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case", deny_unknown_fields)]
pub enum AskAnswer {
    Permission(PermissionAnswer),
    Plan(PlanAnswer),
    Question(QuestionResponse),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "permission", rename_all = "snake_case", deny_unknown_fields)]
pub enum PermissionAnswer {
    AllowOnce,
    AllowScoped { suggestion: usize },
    Deny { feedback: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "plan", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanAnswer {
    ApproveAuto,
    ApproveManual,
    RequestChanges { feedback: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionResponse {
    pub answers: Vec<QuestionAnswer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionAnswer {
    pub selected: Vec<usize>,
    pub other: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudePtyTranscriptV1Output {
    pub seq_id: u64,
    pub payload: Vec<u8>,
}

pub(crate) fn decode_pty_transcript_v1_args(
    args: Option<&[u8]>,
) -> Result<ClaudePtyTranscriptV1Args, ProtocolError> {
    let args = match args {
        Some(args) => wire::ClaudePtyTranscriptV1Args::decode(args).map_err(|error| {
            ProtocolError::InvalidArgument {
                message: format!(
                    "`{PTY_TRANSCRIPT_V1}` SubscribeSession args must be protobuf: {error}"
                ),
            }
        })?,
        None => wire::ClaudePtyTranscriptV1Args::default(),
    };
    Ok(ClaudePtyTranscriptV1Args {
        terminal_size: args
            .terminal_size
            .map(terminal_size_from_wire)
            .transpose()?,
        replay_query: args
            .replay_query
            .map(|query| {
                let query = query.query.ok_or_else(|| ProtocolError::InvalidArgument {
                    message: format!("`{PTY_TRANSCRIPT_V1}` replay_query missing query"),
                })?;
                match query {
                    wire::claude_pty_transcript_v1_replay_query::Query::Since(seq_id) => {
                        Ok(ClaudePtyTranscriptV1ReplayQuery::Since { seq_id })
                    }
                    wire::claude_pty_transcript_v1_replay_query::Query::TailCount(count) => {
                        Ok(ClaudePtyTranscriptV1ReplayQuery::Tail { count })
                    }
                }
            })
            .transpose()?,
    })
}

pub fn encode_pty_transcript_v1_args(args: ClaudePtyTranscriptV1Args) -> Option<Vec<u8>> {
    if args.terminal_size.is_none() && args.replay_query.is_none() {
        return None;
    }
    Some(
        wire::ClaudePtyTranscriptV1Args {
            terminal_size: args.terminal_size.map(terminal_size_to_wire),
            replay_query: args
                .replay_query
                .map(|query| wire::ClaudePtyTranscriptV1ReplayQuery {
                    query: Some(match query {
                        ClaudePtyTranscriptV1ReplayQuery::Since { seq_id } => {
                            wire::claude_pty_transcript_v1_replay_query::Query::Since(seq_id)
                        }
                        ClaudePtyTranscriptV1ReplayQuery::Tail { count } => {
                            wire::claude_pty_transcript_v1_replay_query::Query::TailCount(count)
                        }
                    }),
                }),
        }
        .encode_to_vec(),
    )
}

pub fn decode_pty_transcript_v1_input(
    payload: &[u8],
) -> Result<ClaudePtyTranscriptV1Input, ProtocolError> {
    let input = wire::ClaudePtyTranscriptV1Input::decode(payload).map_err(|error| {
        ProtocolError::InvalidArgument {
            message: format!(
                "`{PTY_TRANSCRIPT_V1}` input payload must be ClaudePtyTranscriptV1Input protobuf: {error}"
            ),
        }
    })?;
    let intent = input
        .intent
        .ok_or_else(|| invalid_input("missing intent"))?;
    let intent = match intent {
        wire::claude_pty_transcript_v1_input::Intent::Prompt(prompt) => {
            Intent::Prompt { text: prompt.text }
        }
        wire::claude_pty_transcript_v1_input::Intent::Interrupt(_) => Intent::Interrupt,
        wire::claude_pty_transcript_v1_input::Intent::CyclePermissionMode(_) => {
            Intent::CyclePermissionMode
        }
        wire::claude_pty_transcript_v1_input::Intent::Answer(answer) => Intent::Answer {
            ask_id: answer.ask_id,
            answer: answer_from_wire(
                answer
                    .answer
                    .ok_or_else(|| invalid_input("answer missing answer"))?,
            )?,
        },
    };

    Ok(ClaudePtyTranscriptV1Input {
        expected_seq: input.expected_seq,
        intent,
    })
}

pub fn encode_pty_transcript_v1_input(input: ClaudePtyTranscriptV1Input) -> Vec<u8> {
    wire::ClaudePtyTranscriptV1Input {
        expected_seq: input.expected_seq,
        intent: Some(intent_to_wire(input.intent)),
    }
    .encode_to_vec()
}

fn intent_to_wire(intent: Intent) -> wire::claude_pty_transcript_v1_input::Intent {
    use wire::claude_pty_transcript_v1_input::Intent as WireIntent;
    match intent {
        Intent::Prompt { text } => WireIntent::Prompt(wire::ClaudePrompt { text }),
        Intent::Interrupt => WireIntent::Interrupt(wire::ClaudeInterrupt {}),
        Intent::CyclePermissionMode => {
            WireIntent::CyclePermissionMode(wire::ClaudeCyclePermissionMode {})
        }
        Intent::Answer { ask_id, answer } => WireIntent::Answer(wire::ClaudeAnswer {
            ask_id,
            answer: Some(answer_to_wire(answer)),
        }),
    }
}

fn answer_to_wire(answer: AskAnswer) -> wire::claude_answer::Answer {
    use wire::claude_answer::Answer as WireAnswer;
    match answer {
        AskAnswer::Permission(answer) => WireAnswer::Permission(wire::ClaudePermissionAnswer {
            decision: Some(match answer {
                PermissionAnswer::AllowOnce => wire::claude_permission_answer::Decision::AllowOnce(
                    wire::ClaudePermissionAllowOnce {},
                ),
                PermissionAnswer::AllowScoped { suggestion } => {
                    wire::claude_permission_answer::Decision::AllowScoped(
                        wire::ClaudePermissionAllowScoped {
                            suggestion: index_to_wire(suggestion),
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
        AskAnswer::Plan(answer) => WireAnswer::Plan(wire::ClaudePlanAnswer {
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
        AskAnswer::Question(response) => WireAnswer::Question(wire::ClaudeQuestionResponse {
            answers: response
                .answers
                .into_iter()
                .map(|answer| wire::ClaudeQuestionAnswer {
                    selected: answer.selected.into_iter().map(index_to_wire).collect(),
                    other: answer.other,
                })
                .collect(),
        }),
    }
}

fn answer_from_wire(answer: wire::claude_answer::Answer) -> Result<AskAnswer, ProtocolError> {
    Ok(match answer {
        wire::claude_answer::Answer::Permission(answer) => {
            let decision = answer
                .decision
                .ok_or_else(|| invalid_input("permission answer missing decision"))?;
            AskAnswer::Permission(match decision {
                wire::claude_permission_answer::Decision::AllowOnce(_) => {
                    PermissionAnswer::AllowOnce
                }
                wire::claude_permission_answer::Decision::AllowScoped(answer) => {
                    PermissionAnswer::AllowScoped {
                        suggestion: index_from_wire(answer.suggestion, "permission suggestion")?,
                    }
                }
                wire::claude_permission_answer::Decision::Deny(answer) => PermissionAnswer::Deny {
                    feedback: answer.feedback,
                },
            })
        }
        wire::claude_answer::Answer::Plan(answer) => {
            let decision = answer
                .decision
                .ok_or_else(|| invalid_input("plan answer missing decision"))?;
            AskAnswer::Plan(match decision {
                wire::claude_plan_answer::Decision::ApproveAuto(_) => PlanAnswer::ApproveAuto,
                wire::claude_plan_answer::Decision::ApproveManual(_) => PlanAnswer::ApproveManual,
                wire::claude_plan_answer::Decision::RequestChanges(answer) => {
                    PlanAnswer::RequestChanges {
                        feedback: answer.feedback,
                    }
                }
            })
        }
        wire::claude_answer::Answer::Question(response) => AskAnswer::Question(QuestionResponse {
            answers: response
                .answers
                .into_iter()
                .map(|answer| {
                    Ok(QuestionAnswer {
                        selected: answer
                            .selected
                            .into_iter()
                            .map(|index| index_from_wire(index, "question selection"))
                            .collect::<Result<Vec<_>, ProtocolError>>()?,
                        other: answer.other,
                    })
                })
                .collect::<Result<Vec<_>, ProtocolError>>()?,
        }),
    })
}

fn index_from_wire(index: u64, field: &str) -> Result<usize, ProtocolError> {
    index
        .try_into()
        .map_err(|_| invalid_input(&format!("{field} is out of range: {index}")))
}

fn index_to_wire(index: usize) -> u64 {
    u64::try_from(index).expect("usize fits the protocol's uint64 index")
}

fn invalid_input(message: &str) -> ProtocolError {
    ProtocolError::InvalidArgument {
        message: format!("`{PTY_TRANSCRIPT_V1}` input {message}"),
    }
}

pub(crate) fn encode_pty_transcript_v1_output(output: ClaudePtyTranscriptV1Output) -> Vec<u8> {
    wire::ClaudePtyTranscriptV1Output {
        seq_id: output.seq_id,
        payload: output.payload,
    }
    .encode_to_vec()
}

pub fn decode_pty_transcript_v1_output(
    payload: &[u8],
) -> Result<ClaudePtyTranscriptV1Output, ProtocolError> {
    wire::ClaudePtyTranscriptV1Output::decode(payload)
        .map(|output| ClaudePtyTranscriptV1Output {
            seq_id: output.seq_id,
            payload: output.payload,
        })
        .map_err(|error| ProtocolError::InvalidArgument {
            message: format!(
                "`{PTY_TRANSCRIPT_V1}` output payload must be ClaudePtyTranscriptV1Output protobuf: {error}"
            ),
        })
}

pub(crate) fn encode_pty_transcript_v1_cursor(seq_id: u64) -> Vec<u8> {
    wire::ClaudePtyTranscriptV1Cursor { seq_id }.encode_to_vec()
}

pub fn decode_pty_transcript_v1_cursor(cursor: &[u8]) -> Result<u64, ProtocolError> {
    wire::ClaudePtyTranscriptV1Cursor::decode(cursor)
        .map(|cursor| cursor.seq_id)
        .map_err(|error| ProtocolError::InvalidArgument {
            message: format!(
                "`{PTY_TRANSCRIPT_V1}` cursor must be ClaudePtyTranscriptV1Cursor protobuf: {error}"
            ),
        })
}

fn terminal_size_to_wire(size: TerminalSize) -> wire::TerminalSize {
    wire::TerminalSize {
        rows: u32::from(size.rows),
        cols: u32::from(size.cols),
    }
}

fn terminal_size_from_wire(size: wire::TerminalSize) -> Result<TerminalSize, ProtocolError> {
    Ok(TerminalSize {
        rows: size
            .rows
            .try_into()
            .map_err(|_| ProtocolError::InvalidArgument {
                message: format!("terminal rows out of range: {}", size.rows),
            })?,
        cols: size
            .cols
            .try_into()
            .map_err(|_| ProtocolError::InvalidArgument {
                message: format!("terminal cols out of range: {}", size.cols),
            })?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_args_decode_terminal_size_and_replay_query() {
        let cases = [
            ClaudePtyTranscriptV1Args {
                terminal_size: Some(TerminalSize {
                    rows: 30,
                    cols: 100,
                }),
                replay_query: Some(ClaudePtyTranscriptV1ReplayQuery::Since { seq_id: 42 }),
            },
            ClaudePtyTranscriptV1Args {
                terminal_size: None,
                replay_query: Some(ClaudePtyTranscriptV1ReplayQuery::Tail { count: 12 }),
            },
        ];

        for args in cases {
            let encoded = encode_pty_transcript_v1_args(args.clone()).unwrap();
            assert_eq!(decode_pty_transcript_v1_args(Some(&encoded)).unwrap(), args);
        }
    }

    #[test]
    fn transcript_input_roundtrips_every_intent() {
        let intents = vec![
            Intent::Prompt {
                text: "hello".to_string(),
            },
            Intent::Interrupt,
            Intent::CyclePermissionMode,
            Intent::Answer {
                ask_id: "permission-once".to_string(),
                answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
            },
            Intent::Answer {
                ask_id: "permission-scoped".to_string(),
                answer: AskAnswer::Permission(PermissionAnswer::AllowScoped { suggestion: 2 }),
            },
            Intent::Answer {
                ask_id: "permission-deny".to_string(),
                answer: AskAnswer::Permission(PermissionAnswer::Deny {
                    feedback: Some("not here".to_string()),
                }),
            },
            Intent::Answer {
                ask_id: "plan-auto".to_string(),
                answer: AskAnswer::Plan(PlanAnswer::ApproveAuto),
            },
            Intent::Answer {
                ask_id: "plan-manual".to_string(),
                answer: AskAnswer::Plan(PlanAnswer::ApproveManual),
            },
            Intent::Answer {
                ask_id: "plan-changes".to_string(),
                answer: AskAnswer::Plan(PlanAnswer::RequestChanges {
                    feedback: "add tests".to_string(),
                }),
            },
            Intent::Answer {
                ask_id: "questions".to_string(),
                answer: AskAnswer::Question(QuestionResponse {
                    answers: vec![
                        QuestionAnswer {
                            selected: vec![0, 2],
                            other: None,
                        },
                        QuestionAnswer {
                            selected: Vec::new(),
                            other: Some("another choice".to_string()),
                        },
                    ],
                }),
            },
        ];

        for intent in intents {
            let input = ClaudePtyTranscriptV1Input {
                expected_seq: 7,
                intent,
            };
            let payload = encode_pty_transcript_v1_input(input.clone());
            assert_eq!(decode_pty_transcript_v1_input(&payload).unwrap(), input);
        }
    }

    #[test]
    fn transcript_input_requires_an_intent() {
        let payload = wire::ClaudePtyTranscriptV1Input {
            expected_seq: 7,
            intent: None,
        }
        .encode_to_vec();
        assert!(matches!(
            decode_pty_transcript_v1_input(&payload),
            Err(ProtocolError::InvalidArgument { message }) if message.contains("missing intent")
        ));
    }

    #[test]
    fn transcript_output_and_cursor_encode_typed_payloads() {
        let output = encode_pty_transcript_v1_output(ClaudePtyTranscriptV1Output {
            seq_id: 9,
            payload: br#"{"type":"assistant"}"#.to_vec(),
        });
        let output = decode_pty_transcript_v1_output(&output).unwrap();
        assert_eq!(output.seq_id, 9);
        assert_eq!(output.payload, br#"{"type":"assistant"}"#);

        let cursor = encode_pty_transcript_v1_cursor(9);
        assert_eq!(decode_pty_transcript_v1_cursor(&cursor).unwrap(), 9);
    }
}

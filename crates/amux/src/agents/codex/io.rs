//! Codex-owned IO protocol payloads for session RPCs.
//!
//! The core protocol treats session arguments, input, control, output, and
//! cursors as opaque bytes. This module owns the first-party Codex schemas.

use prost::Message as ProstMessage;

use crate::protocol::{ProtocolError, wire};

pub const CODEX_SDK_V1: &str = "codex_sdk_v1";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodexSdkV1Args {
    pub replay_query: Option<CodexSdkV1ReplayQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexSdkV1ReplayQuery {
    /// Last structured sequence observed by the client. Replay resumes after it.
    Since {
        seq: u64,
    },
    Tail {
        count: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSdkV1Output {
    pub seq: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexSdkV1Input {
    UserTurn {
        input: Vec<u8>,
    },
    Steer {
        turn_id: String,
        input: Vec<u8>,
    },
    Interrupt {
        turn_id: String,
    },
    ApprovalDecision {
        request_id: Vec<u8>,
        decision: String,
    },
}

pub(crate) fn decode_codex_sdk_v1_args(
    args: Option<&[u8]>,
) -> Result<CodexSdkV1Args, ProtocolError> {
    let args = match args {
        Some(args) => {
            wire::CodexSdkV1Args::decode(args).map_err(|error| ProtocolError::InvalidArgument {
                message: format!(
                    "`{CODEX_SDK_V1}` SubscribeSession args must be protobuf: {error}"
                ),
            })?
        }
        None => wire::CodexSdkV1Args::default(),
    };
    let replay_query = args
        .replay_query
        .map(|query| {
            let query = query.query.ok_or_else(|| ProtocolError::InvalidArgument {
                message: format!("`{CODEX_SDK_V1}` replay_query missing query"),
            })?;
            Ok(match query {
                wire::codex_sdk_v1_replay_query::Query::Since(seq) => {
                    CodexSdkV1ReplayQuery::Since { seq }
                }
                wire::codex_sdk_v1_replay_query::Query::TailCount(count) => {
                    CodexSdkV1ReplayQuery::Tail { count }
                }
            })
        })
        .transpose()?;
    Ok(CodexSdkV1Args { replay_query })
}

pub fn encode_codex_sdk_v1_args(args: CodexSdkV1Args) -> Option<Vec<u8>> {
    args.replay_query.map(|query| {
        wire::CodexSdkV1Args {
            replay_query: Some(wire::CodexSdkV1ReplayQuery {
                query: Some(match query {
                    CodexSdkV1ReplayQuery::Since { seq } => {
                        wire::codex_sdk_v1_replay_query::Query::Since(seq)
                    }
                    CodexSdkV1ReplayQuery::Tail { count } => {
                        wire::codex_sdk_v1_replay_query::Query::TailCount(count)
                    }
                }),
            }),
        }
        .encode_to_vec()
    })
}

pub(crate) fn decode_codex_sdk_v1_input(payload: &[u8]) -> Result<CodexSdkV1Input, ProtocolError> {
    let input =
        wire::CodexSdkV1Input::decode(payload).map_err(|error| ProtocolError::InvalidArgument {
            message: format!(
                "`{CODEX_SDK_V1}` input payload must be CodexSdkV1Input protobuf: {error}"
            ),
        })?;
    match input.input.ok_or_else(|| ProtocolError::InvalidArgument {
        message: format!("`{CODEX_SDK_V1}` input payload missing input"),
    })? {
        wire::codex_sdk_v1_input::Input::UserTurn(input) => {
            Ok(CodexSdkV1Input::UserTurn { input: input.input })
        }
        wire::codex_sdk_v1_input::Input::Steer(input) => Ok(CodexSdkV1Input::Steer {
            turn_id: input.turn_id,
            input: input.input,
        }),
        wire::codex_sdk_v1_input::Input::Interrupt(input) => Ok(CodexSdkV1Input::Interrupt {
            turn_id: input.turn_id,
        }),
        wire::codex_sdk_v1_input::Input::ApprovalDecision(input) => {
            Ok(CodexSdkV1Input::ApprovalDecision {
                request_id: input.request_id,
                decision: input.decision,
            })
        }
    }
}

pub fn encode_codex_sdk_v1_input(input: CodexSdkV1Input) -> Vec<u8> {
    wire::CodexSdkV1Input {
        input: Some(match input {
            CodexSdkV1Input::UserTurn { input } => {
                wire::codex_sdk_v1_input::Input::UserTurn(wire::CodexSdkV1UserTurn { input })
            }
            CodexSdkV1Input::Steer { turn_id, input } => {
                wire::codex_sdk_v1_input::Input::Steer(wire::CodexSdkV1Steer { turn_id, input })
            }
            CodexSdkV1Input::Interrupt { turn_id } => {
                wire::codex_sdk_v1_input::Input::Interrupt(wire::CodexSdkV1Interrupt { turn_id })
            }
            CodexSdkV1Input::ApprovalDecision {
                request_id,
                decision,
            } => wire::codex_sdk_v1_input::Input::ApprovalDecision(
                wire::CodexSdkV1ApprovalDecision {
                    request_id,
                    decision,
                },
            ),
        }),
    }
    .encode_to_vec()
}

pub(crate) fn encode_codex_sdk_v1_output(output: CodexSdkV1Output) -> Vec<u8> {
    wire::CodexSdkV1Output {
        seq: output.seq,
        payload: output.payload,
    }
    .encode_to_vec()
}

pub fn decode_codex_sdk_v1_output(payload: &[u8]) -> Result<CodexSdkV1Output, ProtocolError> {
    wire::CodexSdkV1Output::decode(payload)
        .map(|output| CodexSdkV1Output {
            seq: output.seq,
            payload: output.payload,
        })
        .map_err(|error| ProtocolError::InvalidArgument {
            message: format!(
                "`{CODEX_SDK_V1}` output payload must be CodexSdkV1Output protobuf: {error}"
            ),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_and_output_roundtrip() {
        let args = [
            CodexSdkV1Args {
                replay_query: Some(CodexSdkV1ReplayQuery::Since { seq: 41 }),
            },
            CodexSdkV1Args {
                replay_query: Some(CodexSdkV1ReplayQuery::Tail { count: 8 }),
            },
        ];
        for args in args {
            let encoded = encode_codex_sdk_v1_args(args.clone()).unwrap();
            assert_eq!(decode_codex_sdk_v1_args(Some(&encoded)).unwrap(), args);
        }

        let output = CodexSdkV1Output {
            seq: 42,
            payload: br#"{"type":"turn/started"}"#.to_vec(),
        };
        let encoded = encode_codex_sdk_v1_output(output.clone());
        assert_eq!(decode_codex_sdk_v1_output(&encoded).unwrap(), output);
    }

    #[test]
    fn input_decode_preserves_json_request_id() {
        let payload = wire::CodexSdkV1Input {
            input: Some(wire::codex_sdk_v1_input::Input::ApprovalDecision(
                wire::CodexSdkV1ApprovalDecision {
                    request_id: br#""request-7""#.to_vec(),
                    decision: "accept".into(),
                },
            )),
        }
        .encode_to_vec();
        assert_eq!(
            decode_codex_sdk_v1_input(&payload).unwrap(),
            CodexSdkV1Input::ApprovalDecision {
                request_id: br#""request-7""#.to_vec(),
                decision: "accept".into(),
            }
        );
    }

    #[test]
    fn input_roundtrip_preserves_all_variants() {
        let inputs = [
            CodexSdkV1Input::UserTurn {
                input: br#"[{"type":"text","text":"PONG"}]"#.to_vec(),
            },
            CodexSdkV1Input::Steer {
                turn_id: "turn-1".into(),
                input: br#"[{"type":"text","text":"more"}]"#.to_vec(),
            },
            CodexSdkV1Input::Interrupt {
                turn_id: "turn-1".into(),
            },
            CodexSdkV1Input::ApprovalDecision {
                request_id: b"41".to_vec(),
                decision: "decline".into(),
            },
        ];
        for input in inputs {
            assert_eq!(
                decode_codex_sdk_v1_input(&encode_codex_sdk_v1_input(input.clone())).unwrap(),
                input
            );
        }
    }
}

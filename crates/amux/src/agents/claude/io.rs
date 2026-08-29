//! Claude-owned IO protocol payloads for `AgentService/SubscribeSession` and `SendInput`.
//!
//! The core protocol treats `SubscribeSessionRequest.args`, `SessionInput.payload`,
//! `SessionOutput.payload`, and cursors as opaque bytes. This module owns the
//! first-party Claude schemas for those bytes.

use prost::Message as ProstMessage;

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
    pub actions: Vec<ClaudePtyTranscriptV1Action>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudePtyTranscriptV1Action {
    Write(Vec<u8>),
    DelayMs(u32),
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

pub(crate) fn decode_pty_transcript_v1_input(
    payload: &[u8],
) -> Result<ClaudePtyTranscriptV1Input, ProtocolError> {
    let input = wire::ClaudePtyTranscriptV1Input::decode(payload).map_err(|error| {
        ProtocolError::InvalidArgument {
            message: format!(
                "`{PTY_TRANSCRIPT_V1}` input payload must be ClaudePtyTranscriptV1Input protobuf: {error}"
            ),
        }
    })?;

    let actions = input
        .actions
        .into_iter()
        .map(|action| {
            let action = action
                .action
                .ok_or_else(|| ProtocolError::InvalidArgument {
                    message: format!("`{PTY_TRANSCRIPT_V1}` input action missing action"),
                })?;
            Ok(match action {
                wire::claude_pty_transcript_v1_action::Action::Write(bytes) => {
                    ClaudePtyTranscriptV1Action::Write(bytes)
                }
                wire::claude_pty_transcript_v1_action::Action::DelayMs(delay_ms) => {
                    ClaudePtyTranscriptV1Action::DelayMs(delay_ms)
                }
            })
        })
        .collect::<Result<Vec<_>, ProtocolError>>()?;

    Ok(ClaudePtyTranscriptV1Input {
        expected_seq: input.expected_seq,
        actions,
    })
}

pub fn encode_pty_transcript_v1_input(input: ClaudePtyTranscriptV1Input) -> Vec<u8> {
    wire::ClaudePtyTranscriptV1Input {
        expected_seq: input.expected_seq,
        actions: input
            .actions
            .into_iter()
            .map(|action| wire::ClaudePtyTranscriptV1Action {
                action: Some(match action {
                    ClaudePtyTranscriptV1Action::Write(bytes) => {
                        wire::claude_pty_transcript_v1_action::Action::Write(bytes)
                    }
                    ClaudePtyTranscriptV1Action::DelayMs(delay_ms) => {
                        wire::claude_pty_transcript_v1_action::Action::DelayMs(delay_ms)
                    }
                }),
            })
            .collect(),
    }
    .encode_to_vec()
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
    fn transcript_input_payload_decodes_typed_actions() {
        let payload = encode_pty_transcript_v1_input(ClaudePtyTranscriptV1Input {
            expected_seq: 7,
            actions: vec![
                ClaudePtyTranscriptV1Action::Write(b"hello".to_vec()),
                ClaudePtyTranscriptV1Action::DelayMs(25),
            ],
        });

        assert_eq!(
            decode_pty_transcript_v1_input(&payload).unwrap(),
            ClaudePtyTranscriptV1Input {
                expected_seq: 7,
                actions: vec![
                    ClaudePtyTranscriptV1Action::Write(b"hello".to_vec()),
                    ClaudePtyTranscriptV1Action::DelayMs(25),
                ],
            }
        );
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

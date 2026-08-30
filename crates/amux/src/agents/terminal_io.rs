//! Agent-independent raw terminal IO protocol payloads for session RPCs.

use prost::Message as ProstMessage;

use crate::agents::TerminalSize;
use crate::protocol::{ProtocolError, wire};

pub const TERMINAL_V1: &str = "terminal_v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalV1Args {
    pub terminal_size: Option<TerminalSize>,
    pub replay_query: Option<TerminalV1ReplayQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalV1ReplayQuery {
    TailBytes { count: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalV1Control {
    Resize(TerminalSize),
}

pub(crate) fn decode_terminal_v1_args(
    args: Option<&[u8]>,
) -> Result<TerminalV1Args, ProtocolError> {
    let args = match args {
        Some(args) => {
            wire::TerminalV1Args::decode(args).map_err(|error| ProtocolError::InvalidArgument {
                message: format!("`{TERMINAL_V1}` SubscribeSession args must be protobuf: {error}"),
            })?
        }
        None => wire::TerminalV1Args::default(),
    };
    Ok(TerminalV1Args {
        terminal_size: args
            .terminal_size
            .map(terminal_size_from_wire)
            .transpose()?,
        replay_query: args
            .replay_query
            .map(|query| {
                let query = query.query.ok_or_else(|| ProtocolError::InvalidArgument {
                    message: format!("`{TERMINAL_V1}` replay_query missing query"),
                })?;
                match query {
                    wire::terminal_v1_replay_query::Query::TailBytes(count) => {
                        Ok(TerminalV1ReplayQuery::TailBytes { count })
                    }
                }
            })
            .transpose()?,
    })
}

pub fn encode_terminal_v1_args(args: TerminalV1Args) -> Option<Vec<u8>> {
    if args.terminal_size.is_none() && args.replay_query.is_none() {
        return None;
    }
    Some(
        wire::TerminalV1Args {
            terminal_size: args.terminal_size.map(terminal_size_to_wire),
            replay_query: args.replay_query.map(|query| wire::TerminalV1ReplayQuery {
                query: Some(match query {
                    TerminalV1ReplayQuery::TailBytes { count } => {
                        wire::terminal_v1_replay_query::Query::TailBytes(count)
                    }
                }),
            }),
        }
        .encode_to_vec(),
    )
}

pub(crate) fn decode_terminal_v1_control(
    payload: &[u8],
) -> Result<TerminalV1Control, ProtocolError> {
    let control =
        wire::SessionControl::decode(payload).map_err(|error| ProtocolError::InvalidArgument {
            message: format!(
                "`{TERMINAL_V1}` control payload must be SessionControl protobuf: {error}"
            ),
        })?;
    let control = control
        .control
        .ok_or_else(|| ProtocolError::InvalidArgument {
            message: format!("`{TERMINAL_V1}` control payload missing control"),
        })?;
    match control {
        wire::session_control::Control::Resize(size) => {
            Ok(TerminalV1Control::Resize(terminal_size_from_wire(size)?))
        }
    }
}

#[cfg(test)]
pub fn encode_terminal_v1_control(control: TerminalV1Control) -> Vec<u8> {
    wire::SessionControl {
        control: Some(match control {
            TerminalV1Control::Resize(size) => {
                wire::session_control::Control::Resize(terminal_size_to_wire(size))
            }
        }),
    }
    .encode_to_vec()
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
    fn args_roundtrip_terminal_size_and_replay_query() {
        let args = encode_terminal_v1_args(TerminalV1Args {
            terminal_size: Some(TerminalSize { rows: 24, cols: 80 }),
            replay_query: Some(TerminalV1ReplayQuery::TailBytes { count: 4096 }),
        })
        .unwrap();

        assert_eq!(
            decode_terminal_v1_args(Some(&args)).unwrap(),
            TerminalV1Args {
                terminal_size: Some(TerminalSize { rows: 24, cols: 80 }),
                replay_query: Some(TerminalV1ReplayQuery::TailBytes { count: 4096 }),
            }
        );
    }

    #[test]
    fn control_roundtrips_resize() {
        let control = TerminalV1Control::Resize(TerminalSize {
            rows: 48,
            cols: 160,
        });
        assert_eq!(
            decode_terminal_v1_control(&encode_terminal_v1_control(control.clone())).unwrap(),
            control
        );
    }
}

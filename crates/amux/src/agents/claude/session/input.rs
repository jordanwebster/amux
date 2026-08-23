use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::core::ClaudeSession;
use crate::agents::{PtyHandle, StructuredInput, StructuredLogSource};
use crate::protocol::ProtocolError;

const PASTE_BEGIN: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
const ENTER: &[u8] = b"\r";
const AFTER_PASTE_MS: u32 = 400;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) enum PtyInput {
    Bytes(Vec<u8>),
    Delay(u32),
}

/// Encode text as the same bracketed-paste submission used by the structured
/// Claude composer: paste block, a short render delay, then Enter. Tabs become
/// spaces and other controls are dropped before encoding, so an embedded
/// escape byte cannot end the paste and turn the remainder into live terminal
/// input while the rest of the message remains deliverable.
pub(super) fn paste_program(text: &str) -> Vec<PtyInput> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text: String = text
        .chars()
        .filter_map(|character| match character {
            '\n' => Some('\n'),
            '\t' => Some(' '),
            character if character.is_control() => None,
            character => Some(character),
        })
        .collect();

    let mut paste = Vec::with_capacity(PASTE_BEGIN.len() + text.len() + PASTE_END.len());
    paste.extend_from_slice(PASTE_BEGIN);
    paste.extend_from_slice(text.as_bytes());
    paste.extend_from_slice(PASTE_END);
    vec![
        PtyInput::Bytes(paste),
        PtyInput::Delay(AFTER_PASTE_MS),
        PtyInput::Bytes(ENTER.to_vec()),
    ]
}

pub(super) async fn send_pty_program(pty: &PtyHandle, actions: &[PtyInput]) -> anyhow::Result<()> {
    for action in actions {
        match action {
            PtyInput::Bytes(bytes) => pty.send_input(bytes.clone()).await?,
            PtyInput::Delay(ms) => {
                tokio::time::sleep(Duration::from_millis(u64::from((*ms).min(5000)))).await;
            }
        }
    }
    Ok(())
}

pub(super) fn sanitize_resume_args(args: Vec<String>) -> Vec<String> {
    fn skip_flag_with_optional_value(args: &[String], index: &mut usize) {
        *index += 1;
        if *index < args.len() && !args[*index].starts_with('-') {
            *index += 1;
        }
    }

    let mut sanitized = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--fork-session" | "-c" | "--continue" => index += 1,
            "-r" | "--resume" | "--from-pr" | "--session-id" | "-w" | "--worktree" | "--tmux" => {
                skip_flag_with_optional_value(&args, &mut index)
            }
            arg if arg.starts_with("--resume=")
                || arg.starts_with("--from-pr=")
                || arg.starts_with("--session-id=")
                || arg.starts_with("--worktree=")
                || arg.starts_with("--tmux=") =>
            {
                index += 1;
            }
            arg if arg.starts_with("-r") && arg.len() > 2 => index += 1,
            arg if arg.starts_with("-w") && arg.len() > 2 => index += 1,
            _ => {
                sanitized.push(args[index].clone());
                index += 1;
            }
        }
    }
    sanitized
}

pub(crate) struct ClaudeStructuredInputTarget {
    readonly: bool,
    log_source: Option<StructuredLogSource>,
    pty: Option<PtyHandle>,
}

impl ClaudeStructuredInputTarget {
    async fn current_seq(&self) -> u64 {
        match &self.log_source {
            Some(log_source) => log_source.current_seq().await,
            None => 0,
        }
    }
}

#[async_trait]
impl StructuredInput for ClaudeStructuredInputTarget {
    async fn send(
        &self,
        client_seq: u64,
        payload: Value,
    ) -> std::result::Result<(), ProtocolError> {
        if self.readonly {
            return Err(ProtocolError::ServerError {
                message: "session is readonly".to_string(),
            });
        }
        let pty = self
            .pty
            .as_ref()
            .ok_or_else(|| ProtocolError::ServerError {
                message: "structured input requires an active PTY".to_string(),
            })?;
        let current_seq = self.current_seq().await;
        if client_seq != current_seq {
            return Err(ProtocolError::SequenceNumberMismatch {
                client_seq,
                current_seq,
            });
        }

        let actions: Vec<PtyInput> =
            serde_json::from_value(payload).map_err(|e| ProtocolError::InvalidArgument {
                message: format!("invalid pty input: {e}"),
            })?;

        tracing::info!(
            client_seq,
            action_count = actions.len(),
            "structured input accepted"
        );

        send_pty_program(pty, &actions)
            .await
            .map_err(|error| ProtocolError::ServerError {
                message: error.to_string(),
            })
    }
}

impl ClaudeSession {
    pub(in crate::agents) fn structured_input_target(&self) -> ClaudeStructuredInputTarget {
        ClaudeStructuredInputTarget {
            readonly: self.readonly,
            log_source: self.log_source(),
            pty: self.pty.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a2a_pty_carrier_encodes_paste_block_delay_and_enter() {
        assert_eq!(
            paste_program("<amux from=\"human\">\r\nhello\r</amux>"),
            vec![
                PtyInput::Bytes(
                    b"\x1b[200~<amux from=\"human\">\nhello\n</amux>\x1b[201~".to_vec(),
                ),
                PtyInput::Delay(400),
                PtyInput::Bytes(b"\r".to_vec()),
            ]
        );
    }

    #[test]
    fn a2a_pty_carrier_normalizes_control_characters_without_losing_the_message() {
        assert_eq!(
            paste_program("tab\there\nescape\x1b[201~rest\0"),
            vec![
                PtyInput::Bytes(b"\x1b[200~tab here\nescape[201~rest\x1b[201~".to_vec()),
                PtyInput::Delay(400),
                PtyInput::Bytes(b"\r".to_vec()),
            ]
        );
    }
}

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::core::ClaudeSession;
use crate::agents::{PtyHandle, StructuredLogSource};
use crate::protocol::ProtocolError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) enum PtyInput {
    Bytes(Vec<u8>),
    Delay(u32),
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

#[derive(Clone)]
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

    pub(crate) async fn send_structured_input(
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

        for action in &actions {
            match action {
                PtyInput::Bytes(bytes) => {
                    pty.send_input(bytes.clone()).await.map_err(|e| {
                        ProtocolError::ServerError {
                            message: e.to_string(),
                        }
                    })?;
                }
                PtyInput::Delay(ms) => {
                    let clamped = (*ms).min(5000);
                    tokio::time::sleep(Duration::from_millis(u64::from(clamped))).await;
                }
            }
        }
        Ok(())
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

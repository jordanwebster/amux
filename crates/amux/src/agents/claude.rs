//! Claude Code agent session.
//!
//! Two-phase init: [`ClaudeSession::new`] stores metadata and event channel,
//! [`ClaudeSession::start`] spawns the PTY process. Hook handling and structured
//! input translation are encapsulated here.

use super::{PtyHandle, SessionEvent, spawn_pty_agent};
use crate::buffer::MultiplexStructuredReader;
use crate::claude::structured_log_source::StructuredLogSource;
use crate::claude::types::{
    ClaudeHook, ClaudeStructuredInput, ClaudeStructuredOutput, Hook, PermissionResponse,
    StructuredInput, StructuredOutput,
};
use crate::error::Result;
use crate::message::CreateAgentRequest;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Convert a permission response to the keystroke to send to Claude Code's TUI.
/// Claude Code's permission UI accepts:
/// - 1: Yes (accept this edit)
/// - 2: Yes (accept all edits)
/// - 3: No (deny)
fn permission_response_keystroke(response: &PermissionResponse) -> &'static [u8] {
    match response {
        PermissionResponse::Yes => b"1",
        PermissionResponse::YesAll => b"2",
        PermissionResponse::No => b"3",
    }
}

pub struct ClaudeSession {
    pub(super) agent_id: Uuid,
    pub(super) name: Option<String>,
    pub(super) command: String,
    pub(super) working_dir: PathBuf,
    pub(super) pty: Option<PtyHandle>,
    log_source: Option<StructuredLogSource>,

    // Stored for deferred start()
    event_tx: mpsc::Sender<SessionEvent>,
    user_id: Uuid,
    terminal_size: Option<crate::message::TerminalSize>,
}

impl ClaudeSession {
    /// Create a new ClaudeSession from a CreateAgentRequest.
    /// Does not spawn the process — call [`start`] afterwards.
    pub fn new(
        req: &CreateAgentRequest,
        event_tx: mpsc::Sender<SessionEvent>,
        user_id: Uuid,
    ) -> Self {
        Self {
            agent_id: req.agent_id,
            name: req.name.clone(),
            command: "claude".to_string(),
            working_dir: req.working_dir.clone(),
            pty: None,
            log_source: None,
            event_tx,
            user_id,
            terminal_size: req.terminal_size,
        }
    }

    /// Spawn the Claude Code process.
    pub fn start(&mut self) -> Result<()> {
        let env = [("AMUX_AGENT_ID", self.agent_id.to_string())];
        let (pty, log_source) = spawn_pty_agent(
            self.agent_id,
            &self.command,
            &self.working_dir,
            &env,
            self.terminal_size,
            self.event_tx.clone(),
            self.user_id,
        )?;
        self.pty = Some(pty);
        self.log_source = Some(log_source);
        Ok(())
    }

    /// Send structured input to Claude Code.
    pub async fn send_input(&self, input: StructuredInput) -> Result<()> {
        let Some(pty) = &self.pty else {
            return Ok(());
        };
        match input {
            StructuredInput::Claude(claude_input) => match claude_input {
                ClaudeStructuredInput::SubmitMessage { data } => {
                    pty.send_input(data).await?;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    pty.send_input(vec![b'\r']).await?;
                }
                ClaudeStructuredInput::PermissionResponse(response) => {
                    let keystroke = permission_response_keystroke(&response);
                    tracing::info!(agent_id = %self.agent_id, ?response, "sending permission response");
                    pty.send_input(keystroke.to_vec()).await?;
                }
            },
        }
        Ok(())
    }

    /// Handle a hook event.
    pub async fn handle_hook(&self, hook: Hook) -> Result<()> {
        let Some(log_source) = &self.log_source else {
            return Ok(());
        };
        match hook {
            Hook::Claude(ClaudeHook::SessionStart(session_start)) => {
                tracing::debug!(agent_id = %self.agent_id, "linking transcript");
                log_source
                    .link_transcript(PathBuf::from(&session_start.transcript_path))
                    .await;
            }
            Hook::Claude(ClaudeHook::PermissionRequest(perm_req)) => {
                tracing::debug!(agent_id = %self.agent_id, "permission request");
                log_source
                    .write(StructuredOutput::Claude(
                        ClaudeStructuredOutput::PermissionRequest {
                            tool: perm_req.tool,
                        },
                    ))
                    .await;
            }
            Hook::Claude(ClaudeHook::Stop(_)) => {
                tracing::debug!(agent_id = %self.agent_id, "agent stopped");
                log_source
                    .write(StructuredOutput::Claude(
                        ClaudeStructuredOutput::AgentStopped,
                    ))
                    .await;
            }
            Hook::Claude(ClaudeHook::Unknown) => {}
        }
        Ok(())
    }

    /// Subscribe to structured log output.
    pub async fn subscribe(&self) -> Option<MultiplexStructuredReader> {
        self.log_source.as_ref()?.subscribe().await
    }

    /// Shut down the session according to the given policy.
    pub async fn stop(&self, policy: super::StopPolicy) {
        tracing::info!(agent_id = %self.agent_id, "shutting down claude session");
        match policy {
            super::StopPolicy::Interrupt => {
                if let Some(pty) = &self.pty {
                    let _ = pty.send_input(vec![0x03]).await;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let _ = pty.send_input(vec![0x03]).await;
                }
            }
        }
        if let Some(pty) = &self.pty {
            pty.close().await;
        }
        if let Some(log_source) = &self.log_source {
            log_source.close().await;
        }
    }
}

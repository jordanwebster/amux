use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use uuid::Uuid;

use super::input::sanitize_resume_args;
use super::name_sniffer::spawn_name_sniffer;
use crate::agents::{
    CreateAgentRequest, LocalAgentNameSource, PtyHandle, SessionEvent, StopPolicy,
    StructuredLogSource, TerminalSize, spawn_pty_agent,
};
use crate::debug::DebugView;

pub(crate) struct ClaudeSession {
    pub(in crate::agents) agent_id: Uuid,
    pub(in crate::agents) name: Option<String>,
    pub(in crate::agents) command: String,
    pub(in crate::agents) working_dir: PathBuf,
    pub(in crate::agents) pty: Option<PtyHandle>,
    pub(super) log_source: Option<StructuredLogSource>,

    pub(in crate::agents) terminal_size: Option<TerminalSize>,
    /// Claude session ID. Set from SessionStart hook during normal operation,
    /// or pre-set before `start()` for resume (triggers `--resume <id>`).
    pub(in crate::agents) session_id: Option<Uuid>,
    /// True for externally-started sessions (no PTY, transcript-only)
    pub(in crate::agents) readonly: bool,
    /// Extra arguments passed to the claude command
    pub(in crate::agents) args: Vec<String>,
    pub(super) name_source: LocalAgentNameSource,
    pub(super) name_sniffer_abort: Option<AbortHandle>,
    pub(in crate::agents) created_at: DateTime<Utc>,
}

impl ClaudeSession {
    /// Create a new ClaudeSession from a CreateAgentRequest.
    /// Does not spawn the process — call [`start`] afterwards.
    pub(in crate::agents) fn new(req: &CreateAgentRequest) -> Self {
        Self {
            agent_id: req.agent_id,
            name: req.name.clone(),
            command: "claude".to_string(),
            working_dir: req.working_dir.clone(),
            pty: None,
            log_source: None,
            terminal_size: req.terminal_size,
            session_id: None,
            readonly: false,
            args: req.args.clone(),
            name_source: if req.name.is_some() {
                LocalAgentNameSource::Amux
            } else {
                LocalAgentNameSource::Unset
            },
            name_sniffer_abort: None,
            created_at: Utc::now(),
        }
    }

    pub(in crate::agents) fn from_suspended(
        req: &CreateAgentRequest,
        name_source: LocalAgentNameSource,
        session_id: Uuid,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            agent_id: req.agent_id,
            name: req.name.clone(),
            command: "claude".to_string(),
            working_dir: req.working_dir.clone(),
            pty: None,
            log_source: None,
            terminal_size: req.terminal_size,
            session_id: Some(session_id),
            readonly: false,
            args: sanitize_resume_args(req.args.clone()),
            name_source,
            name_sniffer_abort: None,
            created_at,
        }
    }

    /// Create a readonly session for an externally-started Claude process.
    /// Has a StructuredLogSource (for transcript tailing) but no PTY.
    pub(in crate::agents) fn new_readonly(agent_id: Uuid, working_dir: PathBuf) -> Self {
        Self {
            agent_id,
            name: None,
            command: "claude".to_string(),
            working_dir,
            pty: None,
            log_source: Some(StructuredLogSource::new()),
            terminal_size: None,
            session_id: None,
            readonly: true,
            args: vec![],
            name_source: LocalAgentNameSource::Unset,
            name_sniffer_abort: None,
            created_at: Utc::now(),
        }
    }

    pub(in crate::agents) fn name_source(&self) -> LocalAgentNameSource {
        self.name_source
    }

    pub(in crate::agents) fn set_name_and_source(
        &mut self,
        name: Option<String>,
        source: LocalAgentNameSource,
    ) {
        self.name = name;
        self.name_source = source;
        if matches!(source, LocalAgentNameSource::Amux)
            && let Some(abort) = self.name_sniffer_abort.take()
        {
            abort.abort();
        }
    }

    pub(in crate::agents) fn maybe_start_name_sniffer(
        &mut self,
        event_tx: &mpsc::Sender<SessionEvent>,
    ) {
        if self.name_sniffer_abort.is_some()
            || matches!(self.name_source, LocalAgentNameSource::Amux)
        {
            return;
        }
        let Some(log_source) = &self.log_source else {
            return;
        };

        let handle = spawn_name_sniffer(log_source.clone(), event_tx.clone(), self.agent_id);
        self.name_sniffer_abort = Some(handle.abort_handle());
    }

    /// Spawn the Claude Code process. Returns an exit handle that completes
    /// when the process exits. If `session_id` is set, passes `--resume <id>`.
    /// Extra args from creation are appended.
    pub(crate) fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        let env = [("AMUX_AGENT_ID", self.agent_id.to_string())];
        let mut args: Vec<String> = match self.session_id {
            Some(id) => vec!["--resume".to_string(), id.to_string()],
            None => vec![],
        };
        args.extend(self.args.iter().cloned());
        let (pty, log_source, exit_handle) = spawn_pty_agent(
            self.agent_id,
            &self.command,
            &args,
            &self.working_dir,
            &env,
            self.terminal_size,
        )?;
        self.pty = Some(pty);
        self.log_source = Some(log_source);
        Ok(exit_handle)
    }

    /// Return the current structured output sequence number.
    #[cfg(test)]
    pub(super) async fn current_seq(&self) -> u64 {
        match &self.log_source {
            Some(log_source) => log_source.current_seq().await,
            None => 0,
        }
    }

    pub(in crate::agents) fn log_source(&self) -> Option<StructuredLogSource> {
        self.log_source.clone()
    }

    /// Shut down the session according to the given policy.
    pub(in crate::agents) async fn stop(&self, policy: StopPolicy) {
        tracing::info!(agent_id = %self.agent_id, "shutting down claude session");
        if let Some(abort) = &self.name_sniffer_abort {
            abort.abort();
        }
        match policy {
            StopPolicy::Interrupt => {
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

impl Serialize for DebugView<'_, ClaudeSession> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let session = self.inner;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("kind", "claude")?;
        if let Some(session_id) = session.session_id {
            map.serialize_entry("session_id", &session_id)?;
        }
        map.serialize_entry("readonly", &session.readonly)?;
        map.serialize_entry("has_pty", &session.pty.is_some())?;
        if let Some(log_source) = &session.log_source {
            map.serialize_entry("transcript", &DebugView::new(log_source, self.verbose))?;
        }
        map.end()
    }
}

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
use crate::agents::claude::transcript_ingest::TranscriptIngest;
use crate::agents::{
    CreateAgentRequest, LocalAgentNameSource, PtyHandle, SessionEvent, StopPolicy,
    StructuredLogSource, TerminalSize, spawn_pty_agent,
};
use crate::debug::DebugView;

const STRUCTURED_LOG_RETENTION: usize = 1000;

/// Inherited environment variables scrubbed before spawning Claude Code.
///
/// Claude Code stamps every subprocess it spawns with a child-session marker
/// set (verified against the claude 2.1.228 binary and by `ps eww` dumps of a
/// daemon started from inside a Claude session): `CLAUDECODE=1`,
/// `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_CHILD_SESSION=1`, `CLAUDE_PID`,
/// plus `AI_AGENT`, `CLAUDE_EFFORT`, and `TRACEPARENT` when applicable, and
/// its process context (`CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_EXECPATH`,
/// `CLAUDE_CODE_MESSAGING_SOCKET`) leaks alongside. An amux daemon whose
/// ancestry includes a Claude session (the CLI auto-spawns the daemon with
/// full env inheritance — one `amux` command run from Claude's Bash tool is
/// enough) carries these vars for its whole lifetime, and a claude spawned
/// under that daemon inherits `CLAUDE_CODE_CHILD_SESSION`, sees itself as a
/// nested child session, and turns transcript persistence off ("Transcript
/// saving is off — inherited CLAUDE_CODE_CHILD_SESSION marker") — which
/// starves the structured transcript stream entirely.
///
/// The list is explicit, not a `CLAUDE_*` prefix wipe: variables like
/// `CLAUDE_CONFIG_DIR` are legitimate user configuration and must survive.
const CLAUDE_CHILD_SESSION_ENV_SCRUB: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
    "TRACEPARENT",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
];

/// Environment additions for a spawned Claude Code process.
///
/// `CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1` is belt-and-braces on top of the
/// scrub: claude's own suppression check honors it unconditionally, so
/// transcripts persist even if a future Claude Code version grows a new
/// child-session marker the scrub list does not yet know about.
fn claude_spawn_env(agent_id: Uuid) -> [(&'static str, String); 2] {
    [
        ("AMUX_AGENT_ID", agent_id.to_string()),
        ("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE", "1".to_string()),
    ]
}

pub(crate) struct ClaudeSession {
    pub(in crate::agents) agent_id: Uuid,
    pub(in crate::agents) name: Option<String>,
    pub(in crate::agents) command: String,
    pub(in crate::agents) working_dir: PathBuf,
    pub(in crate::agents) pty: Option<PtyHandle>,
    pub(super) transcript_ingest: Option<TranscriptIngest>,

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
    /// Fingerprint and arrival time of the last hook payload emitted as
    /// structured output — duplicate-delivery suppression state (see
    /// `hooks.rs`: hook delivery is at-least-once by construction).
    pub(super) last_emitted_hook: Option<(u64, tokio::time::Instant)>,
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
            transcript_ingest: None,
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
            last_emitted_hook: None,
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
            transcript_ingest: None,
            terminal_size: req.terminal_size,
            session_id: Some(session_id),
            readonly: false,
            args: sanitize_resume_args(req.args.clone()),
            name_source,
            name_sniffer_abort: None,
            created_at,
            last_emitted_hook: None,
        }
    }

    /// Create a readonly session for an externally-started Claude process.
    /// Has transcript ingest but no PTY.
    pub(in crate::agents) fn new_readonly(agent_id: Uuid, working_dir: PathBuf) -> Self {
        Self {
            agent_id,
            name: None,
            command: "claude".to_string(),
            working_dir,
            pty: None,
            transcript_ingest: Some(TranscriptIngest::new(StructuredLogSource::new(
                STRUCTURED_LOG_RETENTION,
            ))),
            terminal_size: None,
            session_id: None,
            readonly: true,
            args: vec![],
            name_source: LocalAgentNameSource::Unset,
            name_sniffer_abort: None,
            created_at: Utc::now(),
            last_emitted_hook: None,
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
        let Some(log_source) = self.log_source() else {
            return;
        };

        let handle = spawn_name_sniffer(log_source, event_tx.clone(), self.agent_id);
        self.name_sniffer_abort = Some(handle.abort_handle());
    }

    /// Spawn the Claude Code process. Returns an exit handle that completes
    /// when the process exits. If `session_id` is set, passes `--resume <id>`.
    /// Extra args from creation are appended.
    pub(crate) fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        let env = claude_spawn_env(self.agent_id);
        let mut args: Vec<String> = match self.session_id {
            Some(id) => vec!["--resume".to_string(), id.to_string()],
            None => vec![],
        };
        args.extend(self.args.iter().cloned());
        let (pty, exit_handle) = spawn_pty_agent(
            self.agent_id,
            &self.command,
            &args,
            &self.working_dir,
            &env,
            CLAUDE_CHILD_SESSION_ENV_SCRUB,
            self.terminal_size,
        )?;
        let transcript_ingest =
            TranscriptIngest::new(StructuredLogSource::new(STRUCTURED_LOG_RETENTION));
        let exit_ingest = transcript_ingest.clone();
        self.pty = Some(pty);
        self.transcript_ingest = Some(transcript_ingest);
        Ok(tokio::spawn(async move {
            let _ = exit_handle.await;
            exit_ingest.close().await;
        }))
    }

    /// Return the current structured output sequence number.
    #[cfg(test)]
    pub(super) async fn current_seq(&self) -> u64 {
        match &self.transcript_ingest {
            Some(ingest) => ingest.log_source().current_seq().await,
            None => 0,
        }
    }

    pub(in crate::agents) fn log_source(&self) -> Option<StructuredLogSource> {
        self.transcript_ingest
            .as_ref()
            .map(|ingest| ingest.log_source().clone())
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
        if let Some(ingest) = &self.transcript_ingest {
            ingest.close().await;
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
        if let Some(ingest) = &session.transcript_ingest {
            map.serialize_entry("transcript", &DebugView::new(ingest, self.verbose))?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::pty::apply_env;

    /// The environment a spawned claude actually receives: every inherited
    /// Claude Code child-session marker is scrubbed, and the force-persistence
    /// var plus the agent id are set. Guards against the "Transcript saving is
    /// off — inherited CLAUDE_CODE_CHILD_SESSION marker" failure, where a
    /// daemon whose ancestry includes a Claude session poisoned every claude
    /// it spawned and the structured transcript stream had no rows to tail.
    #[test]
    fn spawned_claude_env_scrubs_child_session_markers_and_forces_persistence() {
        let agent_id = Uuid::new_v4();
        let mut cmd = portable_pty::CommandBuilder::new("claude");
        // Simulate a daemon started from inside a Claude session: the full
        // marker set observed via `ps eww` on such a daemon.
        for key in CLAUDE_CHILD_SESSION_ENV_SCRUB {
            cmd.env(key, "poisoned");
        }
        // Deliberate user configuration must survive the scrub.
        cmd.env("CLAUDE_CONFIG_DIR", "/custom/claude");

        apply_env(
            &mut cmd,
            &claude_spawn_env(agent_id),
            CLAUDE_CHILD_SESSION_ENV_SCRUB,
        );

        for key in CLAUDE_CHILD_SESSION_ENV_SCRUB {
            assert_eq!(cmd.get_env(key), None, "{key} must be scrubbed");
        }
        assert_eq!(
            cmd.get_env("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE")
                .and_then(|v| v.to_str()),
            Some("1")
        );
        assert_eq!(
            cmd.get_env("AMUX_AGENT_ID").and_then(|v| v.to_str()),
            Some(agent_id.to_string().as_str())
        );
        assert_eq!(
            cmd.get_env("CLAUDE_CONFIG_DIR").and_then(|v| v.to_str()),
            Some("/custom/claude")
        );
    }

    /// The scrub list stays explicit: `CLAUDE_CODE_CHILD_SESSION` is the
    /// specific variable Claude Code's persistence check reads, so it must be
    /// present in the list regardless of how the list evolves.
    #[test]
    fn scrub_list_contains_the_child_session_marker() {
        assert!(CLAUDE_CHILD_SESSION_ENV_SCRUB.contains(&"CLAUDE_CODE_CHILD_SESSION"));
    }
}

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use uuid::Uuid;

use super::input::sanitize_resume_args;
use super::name_sniffer::spawn_name_sniffer;
use crate::agents::claude::ClaudeVersionCache;
use crate::agents::claude::transcript_ingest::TranscriptIngest;
use crate::agents::{
    AgentParent, CreateAgentRequest, LocalAgentNameSource, McpLaunchRoute, PtyHandle, SessionEvent,
    StopPolicy, StructuredLogSource, TerminalSize, spawn_pty_agent,
};
use crate::debug::DebugView;

const STRUCTURED_LOG_RETENTION: usize = 1000;
const CLAUDE_MESSAGING_SOCKET_MIN_VERSION: semver::Version = semver::Version::new(2, 1, 224);

#[derive(Clone)]
pub(super) struct ClaudeMessagingCredentials {
    pub(super) socket_path: PathBuf,
    pub(super) token: String,
}

/// Inherited environment variables scrubbed before spawning Claude Code.
///
/// Claude Code stamps every subprocess it spawns with a child-session marker
/// set (verified against the claude 2.1.228 binary and by `ps eww` dumps of a
/// daemon started from inside a Claude session): `CLAUDECODE=1`,
/// `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_CHILD_SESSION=1`, `CLAUDE_PID`,
/// plus `AI_AGENT`, `CLAUDE_EFFORT`, and `TRACEPARENT` when applicable, and
/// its process context (`CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_EXECPATH`,
/// `CLAUDE_CODE_MESSAGING_SOCKET`, `CLAUDE_CODE_MESSAGING_TOKEN`) leaks
/// alongside. An amux daemon whose
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
    "CLAUDE_CODE_MESSAGING_TOKEN",
];

/// Environment additions for a spawned Claude Code process.
///
/// `CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1` is belt-and-braces on top of the
/// scrub: claude's own suppression check honors it unconditionally, so
/// transcripts persist even if a future Claude Code version grows a new
/// child-session marker the scrub list does not yet know about.
fn claude_spawn_env(agent_id: Uuid, host_id: Uuid) -> [(&'static str, String); 3] {
    [
        ("AMUX_AGENT_ID", agent_id.to_string()),
        ("AMUX_HOST_ID", host_id.to_string()),
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
    pub(super) runtime_dir: PathBuf,
    pub(super) claude_version_cache: ClaudeVersionCache,
    pub(super) mcp_launch_route: Option<McpLaunchRoute>,
    pub(super) messaging_credentials: Option<ClaudeMessagingCredentials>,
    pub(super) pty_only_delivery: Arc<AtomicBool>,
    pub(super) delivery_ready: Arc<AtomicBool>,
    pub(super) parent: Option<AgentParent>,
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
    pub(in crate::agents) fn new(
        req: &CreateAgentRequest,
        runtime_dir: PathBuf,
        claude_version_cache: ClaudeVersionCache,
        mcp_launch_route: McpLaunchRoute,
    ) -> Self {
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
            runtime_dir,
            claude_version_cache,
            mcp_launch_route: Some(mcp_launch_route),
            messaging_credentials: None,
            pty_only_delivery: Arc::new(AtomicBool::new(false)),
            delivery_ready: Arc::new(AtomicBool::new(false)),
            parent: req.parent,
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
        runtime_dir: PathBuf,
        claude_version_cache: ClaudeVersionCache,
        mcp_launch_route: McpLaunchRoute,
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
            runtime_dir,
            claude_version_cache,
            mcp_launch_route: Some(mcp_launch_route),
            messaging_credentials: None,
            pty_only_delivery: Arc::new(AtomicBool::new(false)),
            delivery_ready: Arc::new(AtomicBool::new(false)),
            parent: req.parent,
            name_source,
            name_sniffer_abort: None,
            created_at,
            last_emitted_hook: None,
        }
    }

    /// Create a readonly session for an externally-started Claude process.
    /// Has transcript ingest but no PTY.
    ///
    /// The daemon's version cache describes the `claude` binary the daemon
    /// itself launches; an externally started process may be any build the
    /// user ran by hand, so its transcript must not rewrite that cache. This
    /// session therefore observes into a private cache that nothing reads.
    pub(in crate::agents) fn new_readonly(agent_id: Uuid, working_dir: PathBuf) -> Self {
        let claude_version_cache = ClaudeVersionCache::default();
        Self {
            agent_id,
            name: None,
            command: "claude".to_string(),
            working_dir,
            pty: None,
            transcript_ingest: Some(TranscriptIngest::new(
                StructuredLogSource::new(STRUCTURED_LOG_RETENTION),
                claude_version_cache.clone(),
            )),
            terminal_size: None,
            session_id: None,
            readonly: true,
            args: vec![],
            runtime_dir: std::env::temp_dir(),
            claude_version_cache,
            mcp_launch_route: None,
            messaging_credentials: None,
            pty_only_delivery: Arc::new(AtomicBool::new(false)),
            delivery_ready: Arc::new(AtomicBool::new(false)),
            parent: None,
            name_source: LocalAgentNameSource::Unset,
            name_sniffer_abort: None,
            created_at: Utc::now(),
            last_emitted_hook: None,
        }
    }

    #[cfg(feature = "testnet")]
    pub(crate) fn scripted_for_testnet(
        req: &CreateAgentRequest,
        runtime_dir: PathBuf,
        claude_version_cache: ClaudeVersionCache,
        mcp_launch_route: McpLaunchRoute,
    ) -> Self {
        let mut session = Self::new(
            req,
            runtime_dir,
            claude_version_cache.clone(),
            mcp_launch_route,
        );
        session.pty = Some(PtyHandle::test_echo());
        session.transcript_ingest = Some(TranscriptIngest::new(
            StructuredLogSource::new(STRUCTURED_LOG_RETENTION),
            claude_version_cache,
        ));
        session
            .delivery_ready
            .store(true, std::sync::atomic::Ordering::Release);
        session
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
        let claude_version = self.claude_version_cache.current();
        let args = self.spawn_args(claude_version.as_deref())?;
        let host_id = self
            .mcp_launch_route
            .as_ref()
            .context("managed Claude session is missing its MCP launch route")?
            .host_id();
        let env = claude_spawn_env(self.agent_id, host_id);
        let (pty, exit_handle) = spawn_pty_agent(
            self.agent_id,
            &self.command,
            &args,
            &self.working_dir,
            &env,
            CLAUDE_CHILD_SESSION_ENV_SCRUB,
            self.terminal_size,
        )?;
        let transcript_ingest = TranscriptIngest::with_delivery_ready(
            StructuredLogSource::new(STRUCTURED_LOG_RETENTION),
            self.delivery_ready.clone(),
            self.claude_version_cache.clone(),
        );
        // The carrier decision belongs to a process, not to the record: a
        // session demoted to pasting because one Claude never took its socket
        // messages should not keep pasting at the next one.
        self.pty_only_delivery
            .store(false, std::sync::atomic::Ordering::Release);
        let exit_ingest = transcript_ingest.clone();
        self.pty = Some(pty);
        self.transcript_ingest = Some(transcript_ingest);
        Ok(tokio::spawn(async move {
            let _ = exit_handle.await;
            exit_ingest.close().await;
        }))
    }

    fn spawn_args(&self, version: Option<&str>) -> Result<Vec<String>> {
        let mut args = match self.session_id {
            Some(id) => vec!["--resume".to_string(), id.to_string()],
            None => Vec::new(),
        };
        args.extend(without_managed_spawn_args(&self.args));
        args.push("--name".to_string());
        args.push(
            self.name
                .clone()
                .unwrap_or_else(|| self.agent_id.to_string()),
        );
        if version.is_some_and(claude_supports_messaging_socket) {
            args.push("--messaging-socket-path".to_string());
            args.push(
                self.runtime_dir
                    .join(format!("amux-{}.sock", self.agent_id))
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        let route = self
            .mcp_launch_route
            .as_ref()
            .context("managed Claude session is missing its MCP launch route")?;
        route
            .validate()
            .context("managed Claude MCP launch route is no longer valid")?;
        let amux_executable = route
            .executable()
            .to_str()
            .context("the running amux executable path is not valid UTF-8")?;
        let socket_path = route
            .socket_path()
            .to_str()
            .context("the daemon socket path is not valid UTF-8")?;
        let mut mcp_env = serde_json::Map::from_iter([
            (
                "AMUX_AGENT_ID".to_string(),
                serde_json::Value::String(self.agent_id.to_string()),
            ),
            (
                "AMUX_HOST_ID".to_string(),
                serde_json::Value::String(route.host_id().to_string()),
            ),
        ]);
        if let Some(config_path) = route.config_path() {
            let config_path = config_path
                .to_str()
                .context("the amux config path is not valid UTF-8")?;
            mcp_env.insert(
                "AMUX_CONFIG".to_string(),
                serde_json::Value::String(config_path.to_string()),
            );
        }
        args.push("--mcp-config".to_string());
        args.push(
            serde_json::json!({
                "mcpServers": {
                    "amux": {
                        "command": amux_executable,
                        "args": ["mcp", "agent", "--socket-path", socket_path],
                        "env": mcp_env
                    }
                }
            })
            .to_string(),
        );
        args.push("--allowedTools".to_string());
        args.push("mcp__amux__*".to_string());
        Ok(args)
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

fn claude_supports_messaging_socket(version: &str) -> bool {
    version
        .split_whitespace()
        .next()
        .and_then(|version| semver::Version::parse(version).ok())
        .is_some_and(|version| version >= CLAUDE_MESSAGING_SOCKET_MIN_VERSION)
}

fn without_managed_spawn_args(args: &[String]) -> Vec<String> {
    let mut retained = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--name" | "--messaging-socket-path" | "--mcp-config" | "--allowedTools" => {
                index += 1;
                if index < args.len() && !args[index].starts_with('-') {
                    index += 1;
                }
            }
            arg if arg.starts_with("--name=")
                || arg.starts_with("--messaging-socket-path=")
                || arg.starts_with("--mcp-config=")
                || arg.starts_with("--allowedTools=") =>
            {
                index += 1;
            }
            _ => {
                retained.push(args[index].clone());
                index += 1;
            }
        }
    }
    retained
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
        map.serialize_entry(
            "has_messaging_credentials",
            &session
                .messaging_credentials
                .as_ref()
                .is_some_and(|credentials| {
                    !credentials.socket_path.as_os_str().is_empty() && !credentials.token.is_empty()
                }),
        )?;
        map.serialize_entry(
            "pty_only_delivery",
            &session
                .pty_only_delivery
                .load(std::sync::atomic::Ordering::Acquire),
        )?;
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
    use crate::agents::{AgentType, CreateAgentRequest, McpLaunchRoute};

    fn test_route(host_id: Uuid) -> McpLaunchRoute {
        crate::agents::mcp_launch_route_for_tests(host_id)
    }

    fn expected_mcp_config(route: &McpLaunchRoute, agent_id: Uuid) -> String {
        let mut env = serde_json::Map::from_iter([
            (
                "AMUX_AGENT_ID".to_string(),
                serde_json::Value::String(agent_id.to_string()),
            ),
            (
                "AMUX_HOST_ID".to_string(),
                serde_json::Value::String(route.host_id().to_string()),
            ),
        ]);
        if let Some(config_path) = route.config_path() {
            env.insert(
                "AMUX_CONFIG".to_string(),
                serde_json::Value::String(config_path.to_str().unwrap().to_string()),
            );
        }
        serde_json::json!({
            "mcpServers": {
                "amux": {
                    "command": route.executable().to_str().unwrap(),
                    "args": [
                        "mcp",
                        "agent",
                        "--socket-path",
                        route.socket_path().to_str().unwrap()
                    ],
                    "env": env
                }
            }
        })
        .to_string()
    }

    fn mcp_config_arg(args: &[String]) -> serde_json::Value {
        let index = args
            .iter()
            .position(|arg| arg == "--mcp-config")
            .expect("managed MCP config argument");
        serde_json::from_str(&args[index + 1]).unwrap()
    }

    fn claude_request(agent_id: Uuid, name: Option<&str>, args: Vec<&str>) -> CreateAgentRequest {
        CreateAgentRequest {
            agent_id,
            host_id: None,
            name: name.map(str::to_string),
            agent_type: AgentType::Claude,
            working_dir: PathBuf::from("/work"),
            terminal_size: None,
            args: args.into_iter().map(str::to_string).collect(),
            parent: None,
            initial_prompt: None,
        }
    }

    #[test]
    fn a2a_claude_spawn_argv_version_gates_runtime_socket_and_owns_name() {
        let meta: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/a2a/session_registry.meta.json"
        ))
        .unwrap();
        let captured_version = meta["claude_version"].as_str().unwrap();
        let agent_id = Uuid::new_v4();
        let request = claude_request(
            agent_id,
            Some("reviewer"),
            vec![
                "--model",
                "sonnet",
                "--name",
                "spoofed",
                "--messaging-socket-path=/outside/runtime.sock",
            ],
        );
        let runtime_dir = PathBuf::from("/runtime/amux");
        let route = test_route(Uuid::from_u128(70));
        let session = ClaudeSession::new(
            &request,
            runtime_dir.clone(),
            ClaudeVersionCache::default(),
            route.clone(),
        );

        assert_eq!(
            session.spawn_args(Some(captured_version)).unwrap(),
            vec![
                "--model".to_string(),
                "sonnet".to_string(),
                "--name".to_string(),
                "reviewer".to_string(),
                "--messaging-socket-path".to_string(),
                runtime_dir
                    .join(format!("amux-{agent_id}.sock"))
                    .to_string_lossy()
                    .into_owned(),
                "--mcp-config".to_string(),
                expected_mcp_config(&route, agent_id),
                "--allowedTools".to_string(),
                "mcp__amux__*".to_string(),
            ]
        );

        let old_args = session.spawn_args(Some("2.1.223 (Claude Code)")).unwrap();
        assert_eq!(
            old_args,
            vec![
                "--model".to_string(),
                "sonnet".to_string(),
                "--name".to_string(),
                "reviewer".to_string(),
                "--mcp-config".to_string(),
                expected_mcp_config(&route, agent_id),
                "--allowedTools".to_string(),
                "mcp__amux__*".to_string(),
            ]
        );
        assert!(!claude_supports_messaging_socket("not-a-version"));

        let unnamed = ClaudeSession::new(
            &claude_request(agent_id, None, Vec::new()),
            runtime_dir,
            ClaudeVersionCache::default(),
            route.clone(),
        );
        assert_eq!(
            unnamed.spawn_args(None).unwrap(),
            vec![
                "--name".to_string(),
                agent_id.to_string(),
                "--mcp-config".to_string(),
                expected_mcp_config(&route, agent_id),
                "--allowedTools".to_string(),
                "mcp__amux__*".to_string(),
            ]
        );
    }

    #[test]
    fn a2a_claude_mcp_argv_uses_running_binary_and_owns_registration() {
        let agent_id = Uuid::from_u128(41);
        let request = claude_request(
            agent_id,
            Some("builder"),
            vec![
                "--mcp-config",
                "{\"mcpServers\":{\"spoofed\":{}}}",
                "--allowedTools=mcp__spoofed__*",
            ],
        );
        let session = ClaudeSession::new(
            &request,
            PathBuf::from("/runtime"),
            ClaudeVersionCache::default(),
            test_route(Uuid::from_u128(71)),
        );
        let route = session.mcp_launch_route.as_ref().unwrap();
        let args = session.spawn_args(None).unwrap();

        assert_eq!(
            args,
            vec![
                "--name".to_string(),
                "builder".to_string(),
                "--mcp-config".to_string(),
                expected_mcp_config(route, agent_id),
                "--allowedTools".to_string(),
                "mcp__amux__*".to_string(),
            ]
        );
    }

    #[test]
    fn managed_claude_fresh_and_suspended_sessions_keep_the_exact_mcp_route() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("amux");
        let config = dir.path().join("amux.yaml");
        let socket = dir.path().join("custom.sock");
        std::fs::write(&executable, b"test executable").unwrap();
        std::fs::write(&config, b"host_name: test\n").unwrap();
        let host_id = Uuid::from_u128(72);
        let agent_id = Uuid::from_u128(73);
        let route = McpLaunchRoute::new(
            executable.clone(),
            Some(config.clone()),
            socket.clone(),
            host_id,
        )
        .unwrap();
        let request = claude_request(agent_id, Some("builder"), Vec::new());
        let fresh = ClaudeSession::new(
            &request,
            dir.path().join("runtime"),
            ClaudeVersionCache::default(),
            route.clone(),
        );
        let suspended = ClaudeSession::from_suspended(
            &request,
            LocalAgentNameSource::Amux,
            Uuid::from_u128(74),
            Utc::now(),
            dir.path().join("runtime"),
            ClaudeVersionCache::default(),
            route,
        );

        let fresh_config = mcp_config_arg(&fresh.spawn_args(None).unwrap());
        let suspended_config = mcp_config_arg(&suspended.spawn_args(None).unwrap());
        assert_eq!(fresh_config, suspended_config);
        assert_eq!(
            fresh_config["mcpServers"]["amux"]["command"],
            executable.to_str().unwrap()
        );
        assert_eq!(
            fresh_config["mcpServers"]["amux"]["args"],
            serde_json::json!(["mcp", "agent", "--socket-path", socket])
        );
        assert_eq!(
            fresh_config["mcpServers"]["amux"]["env"],
            serde_json::json!({
                "AMUX_AGENT_ID": agent_id,
                "AMUX_CONFIG": config,
                "AMUX_HOST_ID": host_id,
            })
        );
    }

    #[test]
    fn a2a_claude_spawn_argv_scrubs_inherited_messaging_credentials() {
        let mut cmd = portable_pty::CommandBuilder::new("claude");
        cmd.env("CLAUDE_CODE_MESSAGING_SOCKET", "/parent/session.sock");
        cmd.env("CLAUDE_CODE_MESSAGING_TOKEN", "parent-token");

        apply_env(
            &mut cmd,
            &claude_spawn_env(Uuid::new_v4(), Uuid::new_v4()),
            CLAUDE_CHILD_SESSION_ENV_SCRUB,
        );

        assert_eq!(cmd.get_env("CLAUDE_CODE_MESSAGING_SOCKET"), None);
        assert_eq!(cmd.get_env("CLAUDE_CODE_MESSAGING_TOKEN"), None);
    }

    /// The environment a spawned claude actually receives: every inherited
    /// Claude Code child-session marker is scrubbed, and the force-persistence
    /// var plus the complete amux identity pair are set. Guards against the
    /// "Transcript saving is off — inherited CLAUDE_CODE_CHILD_SESSION marker"
    /// failure, where a daemon whose ancestry includes a Claude session
    /// poisoned every claude it spawned and the structured transcript stream
    /// had no rows to tail.
    #[test]
    fn spawned_claude_env_scrubs_child_session_markers_and_forces_persistence() {
        let agent_id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
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
            &claude_spawn_env(agent_id, host_id),
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
            cmd.get_env("AMUX_HOST_ID").and_then(|v| v.to_str()),
            Some(host_id.to_string().as_str())
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

    #[tokio::test]
    async fn readonly_session_transcript_never_feeds_the_daemon_version_cache() {
        let daemon_cache = ClaudeVersionCache::default();
        daemon_cache.observe_transcript_row(&serde_json::json!({"version": "2.1.224"}));

        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        tokio::fs::write(
            &transcript,
            "{\"type\":\"user\",\"version\":\"2.1.200\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .await
        .unwrap();

        let session = ClaudeSession::new_readonly(Uuid::from_u128(7), dir.path().to_path_buf());
        let ingest = session.transcript_ingest.as_ref().unwrap();
        ingest.link_transcript(transcript).await;
        let source = ingest.log_source().clone();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while source.current_seq().await < 1 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        assert_eq!(
            session.claude_version_cache.current().as_deref(),
            Some("2.1.200"),
            "the row was observed, into the session's private cache"
        );
        assert_eq!(daemon_cache.current().as_deref(), Some("2.1.224"));
    }
}

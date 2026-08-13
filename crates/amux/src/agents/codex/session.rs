use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use codex_sdk::{
    Codex, CodexConfig, DaemonMode, Thread, ThreadConfig, connect_daemon, connect_socket,
    ensure_daemon_with_fallback,
};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

use super::io;
use crate::agents::{
    AGENT_TYPE_CODEX, AgentBackend, CreateAgentRequest, LocalAgentNameSource, PtyHandle,
    StopPolicy, StructuredLogSource,
};
use crate::suspend::SuspendedAgent;

const STRUCTURED_LOG_RETENTION: usize = 1000;

/// One lazily initialized Codex app-server connection per local agent host.
pub(crate) struct CodexClient {
    private_socket: PathBuf,
    connection: Mutex<Option<Arc<CodexConnection>>>,
}

struct CodexConnection {
    client: Codex,
    mode: &'static str,
    _daemon: DaemonMode,
}

impl CodexClient {
    pub(crate) fn new(private_socket: PathBuf) -> Self {
        Self {
            private_socket,
            connection: Mutex::new(None),
        }
    }

    async fn connection(&self) -> Result<Arc<CodexConnection>> {
        let mut slot = self.connection.lock().await;
        if let Some(connection) = slot.as_ref() {
            return Ok(connection.clone());
        }

        let codex_home = codex_home()?;
        let daemon = ensure_daemon_with_fallback(&codex_home, &self.private_socket)
            .await
            .context("failed to ensure Codex app-server daemon")?;
        let mode = daemon_mode_name(&daemon);
        let config = CodexConfig {
            client_name: "amux".to_string(),
            client_title: Some("amux".to_string()),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            ..CodexConfig::default()
        };
        let client = match &daemon {
            DaemonMode::Existing | DaemonMode::Spawned(_) => {
                connect_daemon(&codex_home, config).await
            }
            DaemonMode::Private(process) => connect_socket(process.socket_path(), config).await,
            DaemonMode::PrivateExisting(socket_path) => connect_socket(socket_path, config).await,
        }
        .context("failed to connect to Codex app-server daemon")?;

        let connection = Arc::new(CodexConnection {
            client,
            mode,
            _daemon: daemon,
        });
        *slot = Some(connection.clone());
        Ok(connection)
    }
}

fn codex_home() -> Result<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or_else(|| anyhow!("CODEX_HOME or HOME is required for Codex agents"))
}

fn daemon_mode_name(mode: &DaemonMode) -> &'static str {
    match mode {
        DaemonMode::Existing => "existing",
        DaemonMode::Spawned(_) => "spawned-well-known",
        DaemonMode::Private(_) => "spawned-private",
        DaemonMode::PrivateExisting(_) => "existing-private",
    }
}

/// Naming is best-effort: a rejected `thread/name/set` leaves the thread usable,
/// and the desired-name slot lets a later rename retry.
async fn rename_thread(client: &Codex, agent_id: Uuid, thread_id: &str, name: &str) {
    if let Err(error) = client.rename_thread(thread_id, name).await {
        tracing::warn!(%agent_id, %thread_id, %error, "failed to rename Codex thread");
    }
}

#[derive(Default)]
struct CodexRuntime {
    client: Option<Codex>,
    thread: Option<Thread>,
    thread_id: Option<String>,
    daemon_mode: Option<&'static str>,
    startup_error: Option<String>,
    desired_name: Option<String>,
}

pub(crate) struct CodexSession {
    agent_id: Uuid,
    name: Option<String>,
    working_dir: PathBuf,
    model: Option<String>,
    approval_policy: Option<String>,
    sandbox_policy: Option<String>,
    resume_thread_id: Option<String>,
    created_at: DateTime<Utc>,
    log_source: StructuredLogSource,
    shared_client: Arc<CodexClient>,
    runtime: Arc<StdMutex<CodexRuntime>>,
    stop_tx: watch::Sender<bool>,
    started: bool,
}

impl CodexSession {
    pub(crate) fn new(req: &CreateAgentRequest, shared_client: Arc<CodexClient>) -> Self {
        let (model, approval_policy, sandbox_policy, resume_thread_id) = match &req.agent_type {
            crate::agents::AgentType::Codex {
                model,
                approval_policy,
                sandbox_policy,
                resume_thread_id,
            } => (
                model.clone(),
                approval_policy.clone(),
                sandbox_policy.clone(),
                resume_thread_id.clone(),
            ),
            _ => unreachable!("CodexSession requires AgentType::Codex"),
        };
        let (stop_tx, _) = watch::channel(false);
        Self {
            agent_id: req.agent_id,
            name: req.name.clone(),
            working_dir: req.working_dir.clone(),
            model,
            approval_policy,
            sandbox_policy,
            resume_thread_id,
            created_at: Utc::now(),
            log_source: StructuredLogSource::new(STRUCTURED_LOG_RETENTION),
            shared_client,
            runtime: Arc::new(StdMutex::new(CodexRuntime {
                desired_name: req.name.clone(),
                ..CodexRuntime::default()
            })),
            stop_tx,
            started: false,
        }
    }

    fn thread_config(&self) -> Result<ThreadConfig> {
        let cwd = self
            .working_dir
            .to_str()
            .ok_or_else(|| anyhow!("Codex cwd must be valid UTF-8"))?
            .to_string();
        let approval_policy = self
            .approval_policy
            .as_ref()
            .map(|value| serde_json::from_value(serde_json::Value::String(value.clone())))
            .transpose()
            .context("invalid Codex approval_policy")?;
        let sandbox = self
            .sandbox_policy
            .as_ref()
            .map(|value| serde_json::from_value(serde_json::Value::String(value.clone())))
            .transpose()
            .context("invalid Codex sandbox_policy")?;
        Ok(ThreadConfig {
            cwd: Some(cwd),
            model: self.model.clone(),
            approval_policy,
            sandbox,
            ..ThreadConfig::default()
        })
    }

    fn start_task(
        &self,
        mut stop_rx: watch::Receiver<bool>,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let thread_config = self.thread_config()?;
        let shared_client = self.shared_client.clone();
        let runtime = self.runtime.clone();
        let resume_thread_id = self.resume_thread_id.clone();
        let agent_id = self.agent_id;

        Ok(tokio::spawn(async move {
            let startup = async {
                let connection = shared_client.connection().await?;
                let thread = match resume_thread_id {
                    Some(thread_id) => {
                        connection
                            .client
                            .resume_thread(&thread_id, thread_config)
                            .await
                    }
                    None => connection.client.start_thread(thread_config).await,
                }
                .context("failed to start Codex thread")?;
                let thread_id = thread.id().to_string();
                let desired_name = {
                    let mut state = runtime.lock().unwrap_or_else(|poison| poison.into_inner());
                    state.thread_id = Some(thread_id.clone());
                    state.daemon_mode = Some(connection.mode);
                    state.client = Some(connection.client.clone());
                    state.thread = Some(thread);
                    state.desired_name.clone()
                };
                if let Some(name) = desired_name {
                    rename_thread(&connection.client, agent_id, &thread_id, &name).await;
                }
                Ok::<(), anyhow::Error>(())
            };

            tokio::select! {
                result = startup => {
                    if let Err(error) = result {
                        tracing::error!(%agent_id, %error, "Codex session startup failed");
                        runtime.lock().unwrap_or_else(|poison| poison.into_inner()).startup_error =
                            Some(error.to_string());
                        return;
                    }
                }
                _ = stop_rx.changed() => return,
            }

            // Keep the session's exit handle pending until stop (or shutdown).
            let _ = stop_rx.wait_for(|stopped| *stopped).await;
        }))
    }

    fn schedule_remote_rename(&self, name: String) {
        let target = {
            let runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            runtime.client.clone().zip(runtime.thread_id.clone())
        };
        if let Some((client, thread_id)) = target {
            let agent_id = self.agent_id;
            tokio::spawn(async move {
                rename_thread(&client, agent_id, &thread_id, &name).await;
            });
        }
    }
}

#[async_trait]
impl AgentBackend for CodexSession {
    fn agent_id(&self) -> Uuid {
        self.agent_id
    }

    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn set_local_name(&mut self, name: Option<String>, _source: LocalAgentNameSource) {
        self.name = name.clone();
        self.runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .desired_name = name.clone();
        if let Some(name) = name {
            self.schedule_remote_rename(name);
        }
    }

    fn command(&self) -> &str {
        "codex"
    }

    fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    fn readonly(&self) -> bool {
        false
    }

    fn args(&self) -> &[String] {
        &[]
    }

    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        if self.started {
            return Err(anyhow!("Codex session {} already started", self.agent_id));
        }
        self.started = true;
        self.start_task(self.stop_tx.subscribe())
    }

    async fn stop(&self, _policy: StopPolicy) {
        tracing::info!(agent_id = %self.agent_id, "stopping Codex session");
        let _ = self.stop_tx.send(true);
        {
            let mut runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            runtime.thread.take();
            runtime.client.take();
        }
        self.log_source.close().await;
    }

    fn agent_type(&self) -> &'static str {
        AGENT_TYPE_CODEX
    }

    fn io_protocols(&self) -> Vec<String> {
        // Deliberately unconditional: Codex's terminal PTY is lazy until P5c.
        vec![
            io::CODEX_SDK_V1.to_string(),
            crate::agents::terminal_io::TERMINAL_V1.to_string(),
        ]
    }

    fn log_source(&self) -> Option<StructuredLogSource> {
        Some(self.log_source.clone())
    }

    fn pty_handle(&self) -> Option<&PtyHandle> {
        None
    }

    fn suspended_state(&self) -> Result<SuspendedAgent> {
        let thread_id = self
            .runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .thread_id
            .clone();
        match thread_id {
            Some(thread_id) => Err(anyhow!(
                "cannot suspend Codex agent {} (thread {thread_id}): Codex suspend state lands in P5c",
                self.agent_id
            )),
            None => Err(anyhow!(
                "cannot suspend Codex agent {}: thread_id is not available yet",
                self.agent_id
            )),
        }
    }

    fn debug_json(&self, _verbose: bool) -> serde_json::Result<serde_json::Value> {
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        Ok(serde_json::json!({
            "kind": "codex",
            "thread_id": runtime.thread_id,
            "daemon_mode": runtime.daemon_mode,
            "startup_error": runtime.startup_error,
            "has_thread_subscription": runtime.thread.is_some(),
            "has_pty": false,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentType;

    fn session() -> CodexSession {
        let req = CreateAgentRequest {
            agent_id: Uuid::from_u128(1),
            host_id: None,
            name: Some("named".into()),
            agent_type: AgentType::Codex {
                model: None,
                approval_policy: None,
                sandbox_policy: None,
                resume_thread_id: None,
            },
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: Vec::new(),
        };
        CodexSession::new(
            &req,
            Arc::new(CodexClient::new(PathBuf::from("/tmp/amux-codex.sock"))),
        )
    }

    #[test]
    fn advertises_both_planes_before_pty_exists() {
        let session = session();
        assert!(session.pty_handle().is_none());
        assert_eq!(
            session.io_protocols(),
            [io::CODEX_SDK_V1, crate::agents::terminal_io::TERMINAL_V1]
        );
    }

    #[test]
    fn suspend_is_nonfatal_and_reports_missing_thread_id() {
        let error = session().suspended_state().unwrap_err();
        assert!(error.to_string().contains("thread_id is not available"));
    }
}

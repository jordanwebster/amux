//! Narrow adapters that let the external harness package drive private
//! provider backends without owning their orchestration or assertions here.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use chrono::{TimeZone as _, Utc};
use host_api::LocalAgentHost;
use model::{AgentKind, AgentParent, ClaudeDriver, Protocol};
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::agents::claude::sdk_io::ClaudeSdkV1Input;
use crate::agents::claude::{ClaudeSdkBackend, ClaudeSession as ClaudePtyBackend};
use crate::agents::codex::CodexBackend;
use crate::agents::{
    AgentBackend, AgentRecord, McpLaunchRoute, MultiplexStructuredReader, Plane, SessionEvent,
    StopPolicy, StructuredInput, StructuredInputEvent, StructuredLogSource,
};
use crate::host::AgentRuntime;

enum Backend {
    ClaudePty(ClaudePtyBackend),
    ClaudeSdk(ClaudeSdkBackend),
    Codex(CodexBackend),
}

impl Backend {
    fn as_backend(&self) -> &dyn AgentBackend {
        match self {
            Self::ClaudePty(backend) => backend,
            Self::ClaudeSdk(backend) => backend,
            Self::Codex(backend) => backend,
        }
    }

    fn as_backend_mut(&mut self) -> &mut dyn AgentBackend {
        match self {
            Self::ClaudePty(backend) => backend,
            Self::ClaudeSdk(backend) => backend,
            Self::Codex(backend) => backend,
        }
    }
}

/// An opaque provider backend with raw input, output and lifecycle operations.
pub struct StructuredBackendAdapter {
    backend: Backend,
    input: Box<dyn StructuredInput>,
    log: StructuredLogSource,
    reader: MultiplexStructuredReader,
    ingest: Option<tokio::task::JoinHandle<()>>,
    _events: mpsc::Receiver<SessionEvent>,
}

impl StructuredBackendAdapter {
    async fn start(mut backend: Backend, protocol: Protocol) -> Result<Self> {
        let Plane::Structured { log, input } = backend.as_backend().plane(protocol)? else {
            bail!("fixture backend did not expose the requested structured plane");
        };
        let (reader, replay) = log
            .subscribe_with_query(None)
            .await
            .context("fixture backend log was already closed")?;
        if replay.selected_from != 0 {
            bail!("fresh fixture backend unexpectedly retained replay rows");
        }
        let (event_tx, events) = mpsc::channel(8);
        let ingest = backend.as_backend_mut().start(&event_tx)?;
        Ok(Self {
            backend,
            input,
            log,
            reader,
            ingest: Some(ingest),
            _events: events,
        })
    }

    pub async fn claude_pty(session: claude::pty::Session, session_id: Uuid) -> Result<Self> {
        let record = fixture_record(
            session_id,
            "derived-claude-pty",
            AgentKind::Claude {
                driver: ClaudeDriver::Pty,
            },
        );
        Self::start(
            Backend::ClaudePty(ClaudePtyBackend::with_session(record, session)),
            Protocol::ClaudePtyTranscriptV1,
        )
        .await
    }

    pub async fn claude_sdk(session: claude::sdk::Session) -> Result<Self> {
        let session_id = session
            .control
            .session_id()
            .parse()
            .context("recorded Claude SDK session id was not a UUID")?;
        let record = fixture_record(
            session_id,
            "derived-claude-sdk",
            AgentKind::Claude {
                driver: ClaudeDriver::Sdk,
            },
        );
        Self::start(
            Backend::ClaudeSdk(ClaudeSdkBackend::with_session(record, session)),
            Protocol::ClaudeSdkV1,
        )
        .await
    }

    pub async fn codex(session: codex::Session) -> Result<Self> {
        let record = fixture_record(Uuid::from_u128(1), "derived-codex", AgentKind::Codex);
        Self::start(
            Backend::Codex(CodexBackend::with_session(record, session)),
            Protocol::CodexSdkV1,
        )
        .await
    }

    pub async fn send_claude_pty(&self, intent: model::ClaudePtyIntent) -> Result<()> {
        let Backend::ClaudePty(backend) = &self.backend else {
            bail!("Claude PTY input sent to another provider backend");
        };
        let client_seq = backend.current_seq_for_derived_rows().await;
        self.input
            .send(StructuredInputEvent::ClaudePty {
                client_seq,
                intent,
                pins: Vec::new(),
            })
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn send_claude_sdk(&self, input_id: &[u8], input: ClaudeSdkV1Input) -> Result<()> {
        self.input
            .send(StructuredInputEvent::ClaudeSdk {
                input_id: input_id.to_vec(),
                input,
            })
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn send_codex(&self, input_id: &[u8], input: model::CodexSdkInput) -> Result<()> {
        self.input
            .send(StructuredInputEvent::Codex {
                input_id: input_id.to_vec(),
                input,
            })
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn read_row(&mut self) -> Option<Value> {
        self.reader.read().await.map(|row| row.payload)
    }

    pub async fn resubscribe_after_reset(&mut self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.reader.read().await.is_some() {}
            let (reader, facts) = self
                .log
                .subscribe_with_query(None)
                .await
                .context("fixture backend log was closed after reset")?;
            if facts.reset_at == 0 {
                bail!("fixture backend subscription ended without a semantic reset");
            }
            self.reader = reader;
            Ok(())
        })
        .await
        .context("timed out waiting for the fixture backend semantic reset")?
    }

    pub fn ingest_finished(&self) -> bool {
        self.ingest
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
    }

    pub fn abort_ingest(&self) {
        if let Some(ingest) = &self.ingest {
            ingest.abort();
        }
    }

    pub async fn join_ingest(&mut self) {
        if let Some(ingest) = self.ingest.take() {
            let _ = ingest.await;
        }
    }

    pub async fn stop(&self) {
        self.backend.as_backend().stop(StopPolicy::Interrupt).await;
    }

    pub async fn claude_pty_sequence(&self) -> Result<u64> {
        let Backend::ClaudePty(backend) = &self.backend else {
            bail!("sequence requested from a non-PTY backend");
        };
        Ok(backend.current_seq_for_derived_rows().await)
    }

    pub async fn close_claude_pty_log(&self) -> Result<()> {
        let Backend::ClaudePty(backend) = &self.backend else {
            bail!("PTY log close requested from another provider backend");
        };
        backend.close_log_for_derived_rows().await;
        Ok(())
    }
}

fn fixture_record(id: Uuid, name: &str, kind: AgentKind) -> AgentRecord {
    AgentRecord {
        id,
        host_id: Uuid::from_u128(2),
        name: Some(name.to_string()),
        command: kind.provider().to_string(),
        working_dir: PathBuf::from("<MACHINE_PATH>"),
        kind,
        readonly: false,
        args: Vec::new(),
        created_at: Utc.timestamp_opt(0, 0).single().expect("Unix epoch exists"),
        parent: None,
        working_on: None,
        inventory_revision: 0,
    }
}

/// A raw row reader returned after a provider session is injected into a host.
pub struct FixtureRowReader(MultiplexStructuredReader);

impl FixtureRowReader {
    pub async fn read_row(&mut self) -> Option<Value> {
        self.0.read().await.map(|row| row.payload)
    }
}

/// Create an isolated real provider runtime whose sessions are supplied by tests.
pub fn fixture_runtime(directory: &Path, host_id: Uuid) -> Result<Arc<dyn LocalAgentHost>> {
    std::fs::create_dir_all(directory.join("data"))?;
    let route = McpLaunchRoute::new(
        std::env::current_exe()?,
        None,
        directory.join("control.sock"),
        host_id,
    )?;
    Ok(AgentRuntime::new_with_mcp_launch_route(
        route,
        directory.join("keymap"),
        directory.join("data"),
    )?)
}

/// Inject a supplied SDK session through the private provider constructor.
pub async fn register_sdk_fixture(
    host: &dyn LocalAgentHost,
    name: &str,
    parent: Option<AgentParent>,
    session: claude::sdk::Session,
) -> Result<FixtureRowReader> {
    let host = host
        .as_any()
        .downcast_ref::<AgentRuntime>()
        .context("SDK fixture requires AgentRuntime")?;
    let record = AgentRecord {
        id: session.control.session_id().parse()?,
        host_id: host.host_id(),
        name: Some(name.to_string()),
        command: "claude".to_string(),
        working_dir: "<MACHINE_PATH>".into(),
        kind: AgentKind::Claude {
            driver: ClaudeDriver::Sdk,
        },
        readonly: false,
        args: Vec::new(),
        created_at: Utc.timestamp_opt(0, 0).single().expect("Unix epoch exists"),
        parent,
        working_on: None,
        inventory_revision: 0,
    };
    Ok(FixtureRowReader(
        host.register_sdk_fixture(record, session).await?,
    ))
}

pub type ClaudeSdkFixtureInput = ClaudeSdkV1Input;
pub type CodexFixtureInput = model::CodexSdkInput;

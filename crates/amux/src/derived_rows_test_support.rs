use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use chrono::{TimeZone as _, Utc};
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::agents::claude::{ClaudeSdkBackend, ClaudeSession as ClaudePtyBackend};
use crate::agents::codex::CodexBackend;
use crate::agents::{
    AgentBackend, AgentKind, AgentRecord, Plane, Protocol, SessionEvent, StopPolicy,
    StructuredInput, StructuredInputEvent,
};
use crate::claude_sdk_io::ClaudeSdkV1Input;
use crate::codex_io::CodexSdkV1Input;

pub struct ClaudePtyBackendHarness {
    backend: ClaudePtyBackend,
    input: Box<dyn StructuredInput>,
    reader: crate::agents::MultiplexStructuredReader,
    rows: Vec<Value>,
    cursor: usize,
    ingest: tokio::task::JoinHandle<()>,
    _events: mpsc::Receiver<SessionEvent>,
}

impl ClaudePtyBackendHarness {
    pub async fn with_session(session: claude::pty::Session, session_id: Uuid) -> Result<Self> {
        let record = AgentRecord {
            id: session_id,
            host_id: Uuid::from_u128(2),
            name: Some("derived-claude-pty".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("<MACHINE_PATH>"),
            kind: AgentKind::Claude {
                driver: crate::agents::ClaudeDriver::Pty,
            },
            readonly: false,
            args: Vec::new(),
            created_at: Utc.timestamp_opt(0, 0).single().expect("Unix epoch exists"),
            parent: None,
            working_on: None,
        };
        let mut backend = ClaudePtyBackend::with_session(record, session);
        let Plane::Structured { log, input } = backend.plane(Protocol::ClaudePtyTranscriptV1)?
        else {
            bail!("Claude PTY plane was not structured");
        };
        let (reader, count) = log
            .subscribe_with_query(None)
            .await
            .context("Claude PTY backend log was already closed")?;
        if count != 0 {
            bail!("fresh Claude PTY backend unexpectedly retained {count} rows");
        }
        let (event_tx, events) = mpsc::channel(8);
        let ingest = backend.start(&event_tx)?;
        let mut harness = Self {
            backend,
            input,
            reader,
            rows: Vec::new(),
            cursor: 0,
            ingest,
            _events: events,
        };
        harness
            .wait_for(|row| row.get("type").and_then(Value::as_str) == Some("amux.claude.keymap"))
            .await?;
        Ok(harness)
    }

    pub async fn send(&self, intent: crate::claude_io::Intent) -> Result<()> {
        let client_seq = self.backend.current_seq_for_derived_rows().await;
        self.input
            .send(StructuredInputEvent::ClaudePty {
                client_seq,
                intent,
                pins: Vec::new(),
            })
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn wait_for(&mut self, matches: impl Fn(&Value) -> bool) -> Result<Value> {
        loop {
            if let Some((offset, row)) = self.rows[self.cursor..]
                .iter()
                .enumerate()
                .find(|(_, row)| matches(row))
            {
                self.cursor += offset + 1;
                return Ok(row.clone());
            }
            let row = tokio::time::timeout(Duration::from_secs(10), self.reader.read())
                .await
                .context("timed out waiting for derived Claude PTY row")?
                .context("Claude PTY backend log closed before the expected row")?;
            self.rows.push(row.payload);
        }
    }

    pub async fn finish(mut self) -> Result<Vec<Value>> {
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut last_seq = self.backend.current_seq_for_derived_rows().await;
            let mut stable = 0;
            while stable < 3 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let current = self.backend.current_seq_for_derived_rows().await;
                if current == last_seq {
                    stable += 1;
                } else {
                    last_seq = current;
                    stable = 0;
                }
            }
        })
        .await
        .context("timed out waiting for the Claude PTY backend rows to quiesce")?;
        self.ingest.abort();
        self.backend.close_log_for_derived_rows().await;
        let _ = self.ingest.await;
        while let Some(row) = self.reader.read().await {
            self.rows.push(row.payload);
        }
        Ok(self.rows)
    }
}

pub struct ClaudeSdkBackendHarness {
    backend: ClaudeSdkBackend,
    input: Box<dyn StructuredInput>,
    reader: crate::agents::MultiplexStructuredReader,
    rows: Vec<Value>,
    cursor: usize,
    ingest: tokio::task::JoinHandle<()>,
    _events: mpsc::Receiver<SessionEvent>,
}

impl ClaudeSdkBackendHarness {
    pub async fn with_session(session: claude::sdk::Session) -> Result<Self> {
        let session_id = session
            .control
            .session_id()
            .parse()
            .context("recorded Claude SDK session id was not a UUID")?;
        let record = AgentRecord {
            id: session_id,
            host_id: Uuid::from_u128(2),
            name: Some("derived-claude-sdk".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("<MACHINE_PATH>"),
            kind: AgentKind::Claude {
                driver: crate::agents::ClaudeDriver::Sdk,
            },
            readonly: false,
            args: Vec::new(),
            created_at: Utc.timestamp_opt(0, 0).single().expect("Unix epoch exists"),
            parent: None,
            working_on: None,
        };
        let mut backend = ClaudeSdkBackend::with_session(record, session);
        let Plane::Structured { log, input } = backend.plane(Protocol::ClaudeSdkV1)? else {
            bail!("Claude SDK plane was not structured");
        };
        let (reader, count) = log
            .subscribe_with_query(None)
            .await
            .context("Claude SDK backend log was already closed")?;
        if count != 0 {
            bail!("fresh Claude SDK backend unexpectedly retained {count} rows");
        }
        let (event_tx, events) = mpsc::channel(8);
        let ingest = backend.start(&event_tx)?;
        let mut harness = Self {
            backend,
            input,
            reader,
            rows: Vec::new(),
            cursor: 0,
            ingest,
            _events: events,
        };
        harness.wait_for_type("amux.claude_sdk.ready").await?;
        Ok(harness)
    }

    pub async fn send(&self, input_id: &[u8], input: ClaudeSdkV1Input) -> Result<()> {
        self.input
            .send(StructuredInputEvent::ClaudeSdk {
                input_id: input_id.to_vec(),
                input,
            })
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn wait_for_type(&mut self, expected: &str) -> Result<Value> {
        loop {
            if let Some((offset, row)) = self.rows[self.cursor..]
                .iter()
                .enumerate()
                .find(|(_, row)| row.get("type").and_then(Value::as_str) == Some(expected))
            {
                self.cursor += offset + 1;
                return Ok(row.clone());
            }
            let row = tokio::time::timeout(Duration::from_secs(10), self.reader.read())
                .await
                .with_context(|| format!("timed out waiting for derived Claude row {expected}"))?
                .with_context(|| {
                    format!("Claude SDK backend log closed while waiting for {expected}")
                })?;
            self.rows.push(row.payload);
        }
    }

    pub async fn finish(mut self) -> Result<Vec<Value>> {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self.ingest.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("timed out waiting for the Claude SDK backend ingest task to exit")?;
        self.backend.stop(StopPolicy::Interrupt).await;
        let _ = self.ingest.await;
        while let Some(row) = self.reader.read().await {
            self.rows.push(row.payload);
        }
        Ok(self.rows)
    }
}

pub struct CodexBackendHarness {
    backend: CodexBackend,
    input: Box<dyn StructuredInput>,
    reader: crate::agents::MultiplexStructuredReader,
    rows: Vec<Value>,
    cursor: usize,
    ingest: tokio::task::JoinHandle<()>,
    _events: mpsc::Receiver<SessionEvent>,
}

impl CodexBackendHarness {
    pub async fn with_session(session: codex::Session) -> Result<Self> {
        let record = AgentRecord {
            id: Uuid::from_u128(1),
            host_id: Uuid::from_u128(2),
            name: Some("derived-codex".to_string()),
            command: "codex".to_string(),
            working_dir: PathBuf::from("<MACHINE_PATH>"),
            kind: AgentKind::Codex,
            readonly: false,
            args: Vec::new(),
            created_at: Utc.timestamp_opt(0, 0).single().expect("Unix epoch exists"),
            parent: None,
            working_on: None,
        };
        let mut backend = CodexBackend::with_session(record, session);
        let Plane::Structured { log, input } = backend.plane(Protocol::CodexSdkV1)? else {
            bail!("Codex SDK plane was not structured");
        };
        let (reader, count) = log
            .subscribe_with_query(None)
            .await
            .context("Codex backend log was already closed")?;
        if count != 0 {
            bail!("fresh Codex backend unexpectedly retained {count} rows");
        }
        let (event_tx, events) = mpsc::channel(8);
        let ingest = backend.start(&event_tx)?;
        let mut harness = Self {
            backend,
            input,
            reader,
            rows: Vec::new(),
            cursor: 0,
            ingest,
            _events: events,
        };
        harness.wait_for_type("amux.codex_ready").await?;
        Ok(harness)
    }

    pub async fn send(&self, input_id: &[u8], input: CodexSdkV1Input) -> Result<()> {
        self.input
            .send(StructuredInputEvent::Codex {
                input_id: input_id.to_vec(),
                input,
            })
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn wait_for_type(&mut self, expected: &str) -> Result<Value> {
        loop {
            if let Some(row) = self.rows[self.cursor..]
                .iter()
                .find(|row| row.get("type").and_then(Value::as_str) == Some(expected))
                .cloned()
            {
                let offset = self.rows[self.cursor..]
                    .iter()
                    .position(|candidate| candidate == &row)
                    .expect("matching row remains present");
                self.cursor += offset + 1;
                return Ok(row);
            }
            let row = tokio::time::timeout(Duration::from_secs(10), self.reader.read())
                .await
                .with_context(|| format!("timed out waiting for derived row {expected}"))?
                .with_context(|| {
                    format!("Codex backend log closed while waiting for {expected}")
                })?;
            self.rows.push(row.payload);
        }
    }

    pub async fn wait_for_ingest_exit(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self.ingest.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("timed out waiting for the Codex backend ingest task to exit")
    }

    pub async fn finish(mut self) -> Result<Vec<Value>> {
        self.backend.stop(StopPolicy::Interrupt).await;
        let _ = self.ingest.await;
        while let Some(row) = self.reader.read().await {
            self.rows.push(row.payload);
        }
        Ok(self.rows)
    }
}

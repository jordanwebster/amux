// Shared by several test binaries, each of which uses a subset of it.
#![allow(dead_code)]

use std::time::Duration;

use agent_runtime::test_support::{
    ClaudeSdkFixtureInput, CodexFixtureInput, StructuredBackendAdapter,
};
use anyhow::{Context as _, Result};
use serde_json::Value;
use uuid::Uuid;

pub struct ClaudePtyBackendHarness {
    backend: StructuredBackendAdapter,
    rows: Vec<Value>,
    cursor: usize,
}

impl ClaudePtyBackendHarness {
    pub async fn with_session(session: claude::pty::Session, session_id: Uuid) -> Result<Self> {
        let backend = StructuredBackendAdapter::claude_pty(session, session_id).await?;
        let mut harness = Self {
            backend,
            rows: Vec::new(),
            cursor: 0,
        };
        harness
            .wait_for(|row| row.get("type").and_then(Value::as_str) == Some("amux.claude.keymap"))
            .await?;
        Ok(harness)
    }

    pub async fn send(&self, intent: model::ClaudePtyIntent) -> Result<()> {
        self.backend.send_claude_pty(intent).await
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
            let row = tokio::time::timeout(Duration::from_secs(10), self.backend.read_row())
                .await
                .context("timed out waiting for derived Claude PTY row")?
                .context("Claude PTY backend log closed before the expected row")?;
            self.rows.push(row);
        }
    }

    pub async fn finish(mut self) -> Result<Vec<Value>> {
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut last_seq = self.backend.claude_pty_sequence().await?;
            let mut stable = 0;
            while stable < 3 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let current = self.backend.claude_pty_sequence().await?;
                if current == last_seq {
                    stable += 1;
                } else {
                    last_seq = current;
                    stable = 0;
                }
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("timed out waiting for the Claude PTY backend rows to quiesce")??;
        self.backend.abort_ingest();
        self.backend.close_claude_pty_log().await?;
        self.backend.join_ingest().await;
        while let Some(row) = self.backend.read_row().await {
            self.rows.push(row);
        }
        Ok(self.rows)
    }
}

pub struct ClaudeSdkBackendHarness {
    backend: StructuredBackendAdapter,
    rows: Vec<Value>,
    cursor: usize,
}

impl ClaudeSdkBackendHarness {
    pub async fn with_session(session: claude::sdk::Session) -> Result<Self> {
        let backend = StructuredBackendAdapter::claude_sdk(session).await?;
        let mut harness = Self {
            backend,
            rows: Vec::new(),
            cursor: 0,
        };
        harness.wait_for_type("amux.claude_sdk.ready").await?;
        Ok(harness)
    }

    pub async fn send(&self, input_id: &[u8], input: ClaudeSdkFixtureInput) -> Result<()> {
        self.backend.send_claude_sdk(input_id, input).await
    }

    /// Exercise the same typed input value used by the client and daemon boundary.
    pub async fn send_typed(&self, input_id: &[u8], input: model::ClaudeSdkInput) -> Result<()> {
        let input = agent_runtime::test_support::claude_sdk_input_from_model(input)?;
        self.send(input_id, input).await
    }

    /// Rows observed at the daemon log boundary, including synthesized facts.
    pub fn rows(&self) -> &[Value] {
        &self.rows
    }

    pub async fn wait_for_type(&mut self, expected: &str) -> Result<Value> {
        wait_for_type(
            &mut self.backend,
            &mut self.rows,
            &mut self.cursor,
            expected,
            "Claude SDK",
        )
        .await
    }

    pub async fn resubscribe_after_reset(&mut self) -> Result<()> {
        self.backend.resubscribe_after_reset().await?;
        self.rows.clear();
        self.cursor = 0;
        Ok(())
    }

    pub async fn finish(mut self) -> Result<Vec<Value>> {
        wait_for_ingest(&self.backend, "Claude SDK").await?;
        self.backend.stop().await;
        self.backend.join_ingest().await;
        while let Some(row) = self.backend.read_row().await {
            self.rows.push(row);
        }
        Ok(self.rows)
    }
}

pub struct CodexBackendHarness {
    backend: StructuredBackendAdapter,
    rows: Vec<Value>,
    cursor: usize,
}

impl CodexBackendHarness {
    pub async fn with_session(session: codex::Session) -> Result<Self> {
        let backend = StructuredBackendAdapter::codex(session).await?;
        let mut harness = Self {
            backend,
            rows: Vec::new(),
            cursor: 0,
        };
        harness.wait_for_type("amux.codex_ready").await?;
        Ok(harness)
    }

    pub async fn send(&self, input_id: &[u8], input: CodexFixtureInput) -> Result<()> {
        self.backend.send_codex(input_id, input).await
    }

    pub async fn wait_for_type(&mut self, expected: &str) -> Result<Value> {
        wait_for_type(
            &mut self.backend,
            &mut self.rows,
            &mut self.cursor,
            expected,
            "Codex",
        )
        .await
    }

    pub async fn wait_for_ingest_exit(&self) -> Result<()> {
        wait_for_ingest(&self.backend, "Codex").await
    }

    pub async fn finish(mut self) -> Result<Vec<Value>> {
        self.backend.stop().await;
        self.backend.join_ingest().await;
        while let Some(row) = self.backend.read_row().await {
            self.rows.push(row);
        }
        Ok(self.rows)
    }
}

async fn wait_for_type(
    backend: &mut StructuredBackendAdapter,
    rows: &mut Vec<Value>,
    cursor: &mut usize,
    expected: &str,
    provider: &str,
) -> Result<Value> {
    loop {
        if let Some((offset, row)) = rows[*cursor..]
            .iter()
            .enumerate()
            .find(|(_, row)| row.get("type").and_then(Value::as_str) == Some(expected))
        {
            *cursor += offset + 1;
            return Ok(row.clone());
        }
        let row = tokio::time::timeout(Duration::from_secs(10), backend.read_row())
            .await
            .with_context(|| format!("timed out waiting for derived {provider} row {expected}"))?
            .with_context(|| {
                format!("{provider} backend log closed while waiting for {expected}")
            })?;
        rows.push(row);
    }
}

async fn wait_for_ingest(backend: &StructuredBackendAdapter, provider: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !backend.ingest_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .with_context(|| format!("timed out waiting for the {provider} backend ingest task to exit"))
}

pub type ClaudeSdkV1Input = ClaudeSdkFixtureInput;
pub type CodexSdkV1Input = CodexFixtureInput;

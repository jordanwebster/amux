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

    pub async fn resubscribe_after_reset(&mut self) -> Result<()> {
        self.backend.resubscribe_after_reset().await?;
        self.rows.clear();
        self.cursor = 0;
        Ok(())
    }

    pub async fn finish(mut self) -> Result<Vec<Value>> {
        wait_for_ingest(&self.backend, "Claude PTY").await?;
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use claude::pty::{DelaySource, HookSource, PtySource, Sources, TranscriptSource};
    use claude::transcript::TranscriptRow;
    use serde_json::json;
    use tokio::sync::{mpsc, oneshot};

    use super::*;

    #[tokio::test]
    async fn pty_finish_keeps_a_final_row_delayed_past_three_polls() {
        let (_output_tx, output) = mpsc::channel(1);
        let (hooks, hook_tx) = HookSource::channel(1);
        drop(hook_tx);
        let (transcript, row_tx, _paths) = TranscriptSource::channel(1);
        let (exit_tx, exit_rx) = oneshot::channel();
        let session = claude::pty::from_sources(
            Sources {
                pty: PtySource {
                    output,
                    writer: Box::new(tokio::io::sink()),
                    handle: None,
                    exit: Box::pin(async move {
                        exit_rx.await.unwrap_or_else(|_| {
                            pty_host::ExitStatus::with_signal("test source closed")
                        })
                    }),
                },
                hooks,
                transcript,
                version: "2.1.251".parse().expect("fixed Claude version"),
                delays: DelaySource::live(),
            },
            &claude::pty::keymap::KeymapSources::default(),
        );
        let harness = ClaudePtyBackendHarness::with_session(session, Uuid::from_u128(1))
            .await
            .expect("start PTY backend harness");

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            row_tx
                .send((
                    PathBuf::from("recording/delayed.jsonl"),
                    TranscriptRow::parse(json!({"type": "delayed-final"})),
                ))
                .await
                .expect("send delayed final row");
            tokio::time::sleep(Duration::from_millis(10)).await;
            exit_tx
                .send(pty_host::ExitStatus::with_exit_code(0))
                .expect("finish delayed session");
        });

        let rows = harness.finish().await.expect("finish PTY derivation");
        assert!(rows.iter().any(|row| row["type"] == "delayed-final"));
    }
}

//! Strict app-server recordings behind the daemon's ordinary Codex backend.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use codex::{Codex, CodexConfig, ThreadConfig};
use replay_support::{IoDirection, Recording, ReplayController, ReplayOptions, ReplayReport};
use serde_json::Value;

#[derive(Debug)]
pub struct Prepared {
    recording: Recording,
    initialize: Value,
    thread: Value,
}

impl Prepared {
    pub fn load(path: &Path) -> Result<Self> {
        let recording = replay_support::load_recording(path)?;
        ensure!(
            recording
                .io
                .iter()
                .map(|entry| &entry.transport_id)
                .collect::<std::collections::HashSet<_>>()
                .len()
                == 1,
            "Codex recording must use one app-server transport"
        );
        ensure!(
            recording.manifest.recorded.provider == "codex",
            "recording provider must be codex"
        );
        let writes = recording
            .io
            .iter()
            .filter(|entry| entry.direction == IoDirection::Write)
            .take(3)
            .map(|entry| serde_json::from_str::<Value>(&entry.line))
            .collect::<Result<Vec<_>, _>>()?;
        ensure!(
            writes.len() == 3
                && writes[0]["method"] == "initialize"
                && writes[1]["method"] == "initialized"
                && writes[2]["method"] == "thread/start",
            "Codex recording must begin with initialize, initialized and thread/start"
        );
        ensure!(
            writes[0]["params"]["clientInfo"]["name"].is_string(),
            "recorded client name missing"
        );
        ensure!(
            writes[2]["params"].is_object(),
            "recorded thread parameters missing"
        );
        Ok(Self {
            initialize: writes[0]["params"].clone(),
            thread: writes[2]["params"].clone(),
            recording,
        })
    }

    pub async fn open(&self) -> Result<(codex::Session, Recorded)> {
        let mut replay = replay_support::strict_replay(&self.recording, ReplayOptions::default());
        ensure!(
            replay.transports.len() == 1,
            "Codex recording must use one app-server transport"
        );
        let (_, transport) = replay
            .transports
            .pop_first()
            .context("recording transport missing")?;
        let controller = replay.controller;
        let driver_controller = controller.clone();
        let driver = tokio::spawn(async move { driver_controller.run().await });
        let mut recorded = Recorded {
            controller,
            driver,
            client: None,
        };
        let info = &self.initialize["clientInfo"];
        let capabilities = &self.initialize["capabilities"];
        let config = CodexConfig {
            client_name: info["name"].as_str().unwrap().into(),
            client_title: info["title"].as_str().map(str::to_owned),
            client_version: info["version"]
                .as_str()
                .context("recorded client version missing")?
                .into(),
            experimental_api: capabilities["experimentalApi"].as_bool().unwrap_or(false),
            opt_out_notification_methods: capabilities
                .get("optOutNotificationMethods")
                .map(|value| serde_json::from_value(value.clone()))
                .transpose()?
                .unwrap_or_default(),
            ..CodexConfig::default()
        };
        let client = tokio::time::timeout(
            Duration::from_secs(5),
            Codex::from_io(transport.reader, transport.writer, config),
        )
        .await
        .context("recorded Codex initialization timed out")??;
        recorded.client = Some(client.clone());
        // The recording's redacted cwd is provider data; the topology's actual
        // working directory remains the daemon agent's filesystem authority.
        let thread = tokio::time::timeout(
            Duration::from_secs(5),
            client.start_thread(ThreadConfig {
                extra: self
                    .thread
                    .as_object()
                    .unwrap()
                    .clone()
                    .into_iter()
                    .collect(),
                ..ThreadConfig::default()
            }),
        )
        .await
        .context("recorded Codex thread start timed out")??;
        Ok((codex::open(thread).await?, recorded))
    }
}

pub struct Recorded {
    pub controller: ReplayController,
    driver: tokio::task::JoinHandle<()>,
    client: Option<Codex>,
}

impl Recorded {
    pub fn verify(&self) -> Result<ReplayReport> {
        self.controller.finish().map_err(|error| {
            if !error.report.write_mismatches.is_empty() || !error.report.trailing_writes.is_empty()
            {
                anyhow::anyhow!("ReplayWriteMismatch: {:?}", error.report)
            } else {
                error.into()
            }
        })
    }

    pub async fn close(&mut self) {
        self.driver.abort();
        if let Some(client) = self.client.take() {
            client.close().await;
        }
    }
}

impl Drop for Recorded {
    fn drop(&mut self) {
        self.driver.abort();
        // Covers failed startup as well as cancellation before runner shutdown.
        if let Some(client) = self.client.take() {
            tokio::spawn(async move {
                client.clone().close().await;
            });
        }
    }
}

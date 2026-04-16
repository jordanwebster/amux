use crate::agent::{LocalAgentNameSource, SessionEvent, StructuredLogSource};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectiveNameCandidate {
    name: String,
    source: LocalAgentNameSource,
}

#[derive(Debug, Default)]
struct NameSnifferState {
    latest_slug: Option<String>,
    latest_agent_name: Option<String>,
    last_emitted: Option<EffectiveNameCandidate>,
}

impl NameSnifferState {
    /// Record a structured output Value. Returns true if internal state changed.
    fn ingest(&mut self, value: &Value) -> bool {
        let entry_type = value.get("type").and_then(Value::as_str).unwrap_or("");

        if entry_type == "agent-name"
            && let Some(name) = value.get("agentName").and_then(Value::as_str)
        {
            self.latest_agent_name = Some(name.to_string());
            return true;
        }

        if let Some(slug) = value.get("slug").and_then(Value::as_str) {
            self.latest_slug = Some(slug.to_string());
            return true;
        }

        false
    }

    /// The current best name candidate based on all ingested data.
    fn effective_candidate(&self) -> Option<EffectiveNameCandidate> {
        let (name, source) = self
            .latest_agent_name
            .as_ref()
            .map(|n| (n, LocalAgentNameSource::ProviderName))
            .or_else(|| {
                self.latest_slug
                    .as_ref()
                    .map(|n| (n, LocalAgentNameSource::ProviderSlug))
            })?;
        Some(EffectiveNameCandidate {
            name: name.clone(),
            source,
        })
    }

    /// Ingest an output event and return a candidate only if it differs from the last emission.
    fn observe(&mut self, value: &Value) -> Option<EffectiveNameCandidate> {
        if !self.ingest(value) {
            return None;
        }
        let candidate = self.effective_candidate()?;
        if self.last_emitted.as_ref() == Some(&candidate) {
            return None;
        }
        self.last_emitted = Some(candidate.clone());
        Some(candidate)
    }
}

pub(super) fn spawn_name_sniffer(
    log_source: StructuredLogSource,
    event_tx: mpsc::Sender<SessionEvent>,
    agent_id: Uuid,
    user_id: Uuid,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Some(mut reader) = log_source.subscribe().await else {
            return;
        };

        let mut state = NameSnifferState::default();

        while let Some(entry) = reader.read().await {
            let Some(candidate) = state.observe(&entry.payload) else {
                continue;
            };

            if event_tx
                .send(SessionEvent::NameCandidateChanged {
                    agent_id,
                    user_id,
                    name: candidate.name,
                    source: candidate.source,
                })
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

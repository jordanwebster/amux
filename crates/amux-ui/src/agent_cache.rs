use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::types;

/// In-memory cache of known agents keyed by `AgentId`.
#[derive(Clone)]
pub(crate) struct AgentCache {
    agents: Arc<Mutex<HashMap<types::AgentId, types::Agent>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InsertOutcome {
    /// The id was not previously known.
    Added(types::Agent),
    /// The id was known but its fields differ from the cached copy.
    Updated(types::Agent),
    /// The id was known and the fields are unchanged.
    Same,
}

impl AgentCache {
    pub(crate) fn new() -> Self {
        Self {
            agents: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Insert or replace, and report whether the consumer should emit an
    /// `AgentAdded` / `AgentUpdated` / nothing.
    pub(crate) async fn insert_with_outcome(&self, agent: types::Agent) -> InsertOutcome {
        let mut guard = self.agents.lock().await;
        let outcome = match guard.get(&agent.id) {
            None => InsertOutcome::Added(agent.clone()),
            Some(previous) if previous != &agent => InsertOutcome::Updated(agent.clone()),
            Some(_) => InsertOutcome::Same,
        };
        guard.insert(agent.id, agent);
        outcome
    }

    /// Remove an agent. Returns true if the id was present.
    pub(crate) async fn remove(&self, id: types::AgentId) -> bool {
        self.agents.lock().await.remove(&id).is_some()
    }

    pub(crate) async fn snapshot(&self) -> Vec<types::Agent> {
        let guard = self.agents.lock().await;
        let mut agents: Vec<_> = guard.values().cloned().collect();
        agents.sort_unstable_by_key(|a| a.id);
        agents
    }

    pub(crate) async fn get(&self, id: types::AgentId) -> Option<types::Agent> {
        self.agents.lock().await.get(&id).cloned()
    }

    /// Like `get`, but if the agent isn't cached yet, ask the server to
    /// resolve it and cache the result. Covers the small race between command
    /// dispatch and the inventory subscription delivering AgentUp.
    pub(crate) async fn find_or_fetch(
        &self,
        client: &amux::Client,
        id: types::AgentId,
    ) -> Option<types::Agent> {
        if let Some(agent) = self.get(id).await {
            return Some(agent);
        }
        let agent = client
            .list_agents()
            .await
            .ok()?
            .into_iter()
            .find(|agent| agent.id == id)?;
        self.agents.lock().await.insert(id, agent.clone());
        Some(agent)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn insert_distinguishes_added_updated_and_same() {
        let cache = AgentCache::new();
        let id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        let agent = sample_agent(id, host_id, Some("v1".to_string()));

        assert!(matches!(
            cache.insert_with_outcome(agent.clone()).await,
            InsertOutcome::Added(a) if a.id == id
        ));
        assert!(matches!(
            cache.insert_with_outcome(agent.clone()).await,
            InsertOutcome::Same
        ));

        let renamed = sample_agent(id, host_id, Some("v2".to_string()));
        assert!(matches!(
            cache.insert_with_outcome(renamed).await,
            InsertOutcome::Updated(a) if a.name.as_deref() == Some("v2")
        ));
    }

    fn sample_agent(
        id: types::AgentId,
        host_id: types::HostId,
        name: Option<String>,
    ) -> types::Agent {
        types::Agent {
            id,
            host_id,
            name,
            command: "test-agent".to_string(),
            working_dir: std::env::temp_dir(),
            agent_type: "test_agent".to_string(),
            io_protocols: Vec::new(),
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
        }
    }
}

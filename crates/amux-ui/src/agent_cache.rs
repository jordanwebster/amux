use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::types;

/// In-memory cache of known agents keyed by `AgentId`.
///
/// The runtime stores the full `AgentEntry` (with route) internally so it can
/// dispatch commands that need a route (SendInput, AttachSession). The public
/// notification surface only ever surfaces the route-free `Agent`.
#[derive(Clone)]
pub(crate) struct AgentCache {
    agents: Arc<Mutex<HashMap<types::AgentId, types::AgentEntry>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InsertOutcome {
    /// The id was not previously known.
    Added(types::Agent),
    /// The id was known but its `Agent` fields differ from the cached copy.
    Updated(types::Agent),
    /// The id was known and the `Agent` fields are unchanged. Caller should
    /// not emit a notification. (Routes can still change silently — see note.)
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
    ///
    /// Comparison is on the agent body only, not the route — a route change
    /// (e.g. the same agent reachable via a different hop count) is bookkept
    /// internally without surfacing an update notification, because the app
    /// addresses agents by id, not by route.
    pub(crate) async fn insert_with_outcome(&self, entry: types::AgentEntry) -> InsertOutcome {
        let mut guard = self.agents.lock().await;
        let outcome = match guard.get(&entry.agent.id) {
            None => InsertOutcome::Added(entry.agent.clone()),
            Some(previous) if previous.agent != entry.agent => {
                InsertOutcome::Updated(entry.agent.clone())
            }
            Some(_) => InsertOutcome::Same,
        };
        guard.insert(entry.agent.id, entry);
        outcome
    }

    /// Remove an agent. Returns true if the id was present.
    pub(crate) async fn remove(&self, id: types::AgentId) -> bool {
        self.agents.lock().await.remove(&id).is_some()
    }

    /// Remove every agent on a host (used when the host disappears).
    /// Returns the ids that were removed so the caller can emit
    /// `AgentRemoved` for each.
    pub(crate) async fn remove_host(&self, host_id: types::HostId) -> Vec<types::AgentId> {
        let mut guard = self.agents.lock().await;
        let ids: Vec<_> = guard
            .iter()
            .filter(|(_, entry)| entry.agent.host_id == host_id)
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            guard.remove(id);
        }
        ids
    }

    /// Snapshot the current cache as a flat `Vec<Agent>` (no routes).
    pub(crate) async fn snapshot(&self) -> Vec<types::Agent> {
        let guard = self.agents.lock().await;
        let mut agents: Vec<_> = guard.values().map(|entry| entry.agent.clone()).collect();
        agents.sort_unstable_by_key(|a| a.id);
        agents
    }

    /// Look up the cached entry for a known agent. Used by command dispatch
    /// to obtain the route for SendInput / SubscribeSession.
    pub(crate) async fn get(&self, id: types::AgentId) -> Option<types::AgentEntry> {
        self.agents.lock().await.get(&id).cloned()
    }

    /// Like `get`, but if the agent isn't cached yet, ask the server to
    /// resolve it and cache the result. Covers the small race between
    /// command dispatch and the inventory subscription delivering AgentUp.
    pub(crate) async fn find_or_fetch(
        &self,
        client: &amux::Client,
        id: types::AgentId,
    ) -> Option<types::AgentEntry> {
        if let Some(entry) = self.get(id).await {
            return Some(entry);
        }
        let entry = client
            .resolve_agent(amux::AgentIdentifier::Id(id))
            .await
            .ok()?;
        self.agents.lock().await.insert(id, entry.clone());
        Some(entry)
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
        let entry = sample_entry(id, host_id, Some("v1".to_string()));

        // First insert is Added.
        assert!(matches!(
            cache.insert_with_outcome(entry.clone()).await,
            InsertOutcome::Added(a) if a.id == id
        ));

        // Same agent body, same route → Same.
        assert!(matches!(
            cache.insert_with_outcome(entry.clone()).await,
            InsertOutcome::Same
        ));

        // Name change → Updated.
        let renamed = sample_entry(id, host_id, Some("v2".to_string()));
        assert!(matches!(
            cache.insert_with_outcome(renamed).await,
            InsertOutcome::Updated(a) if a.name.as_deref() == Some("v2")
        ));

        // Route change with identical agent body → Same (route doesn't surface).
        let rerouted = types::AgentEntry {
            agent: cache.get(id).await.unwrap().agent,
            route: types::Route::from_link(amux::protocol::Link::new("elsewhere").unwrap()),
        };
        assert!(matches!(
            cache.insert_with_outcome(rerouted).await,
            InsertOutcome::Same
        ));
    }

    #[tokio::test]
    async fn remove_host_removes_only_agents_on_that_host() {
        let cache = AgentCache::new();
        let host_a = Uuid::new_v4();
        let host_b = Uuid::new_v4();
        let agent_a1 = Uuid::new_v4();
        let agent_a2 = Uuid::new_v4();
        let agent_b1 = Uuid::new_v4();

        cache
            .insert_with_outcome(sample_entry(agent_a1, host_a, None))
            .await;
        cache
            .insert_with_outcome(sample_entry(agent_a2, host_a, None))
            .await;
        cache
            .insert_with_outcome(sample_entry(agent_b1, host_b, None))
            .await;

        let removed = cache.remove_host(host_a).await;
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&agent_a1));
        assert!(removed.contains(&agent_a2));
        assert!(cache.get(agent_b1).await.is_some());
    }

    fn sample_entry(
        id: types::AgentId,
        host_id: types::HostId,
        name: Option<String>,
    ) -> types::AgentEntry {
        types::AgentEntry {
            agent: types::Agent {
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
            },
            route: types::Route::empty(),
        }
    }
}
